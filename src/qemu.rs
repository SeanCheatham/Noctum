//! Pinned system-mode QEMU. The guest is the ruler: one vCPU, TCG, icount,
//! no NIC, no virtio-rng, a fixed CPU model, and a fresh disk overlay.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::lockfile::qemu_dir;
use crate::ports::{MeasureRequest, Measurement, Measurer};
use crate::process::{self, signal_group};
use crate::profile::{self, ProfileDump};
use crate::target::GuestTarget;

pub struct QemuRunner {
    pub data_dir: PathBuf,
    pub memory_mb: u32,
    bundle_override: Option<PathBuf>,
    snapshot_override: Option<PathBuf>,
    target: GuestTarget,
}

impl QemuRunner {
    pub fn new(data_dir: impl Into<PathBuf>, memory_mb: u32) -> Self {
        Self {
            data_dir: data_dir.into(),
            memory_mb,
            bundle_override: None,
            snapshot_override: None,
            target: GuestTarget::X86_64,
        }
    }

    pub fn at_bundle(bundle: PathBuf, snapshots: PathBuf, memory_mb: u32) -> Self {
        Self {
            data_dir: PathBuf::new(),
            memory_mb,
            bundle_override: Some(bundle),
            snapshot_override: Some(snapshots),
            target: GuestTarget::native(),
        }
    }

    fn bundle(&self) -> PathBuf {
        self.bundle_override
            .clone()
            .unwrap_or_else(|| qemu_dir(&self.data_dir))
    }
}

impl Measurer for QemuRunner {
    async fn measure(&self, req: &MeasureRequest) -> Result<Measurement> {
        let bundle = self.bundle();
        ensure_tools(&bundle, self.target)?;
        std::fs::create_dir_all(&req.scratch)?;
        let snapshots = self.snapshot_override.as_deref().unwrap_or(&bundle);
        std::fs::create_dir_all(snapshots)?;
        ensure_snapshot(&bundle, snapshots, self.memory_mb, self.target).await?;
        let overlay = req.scratch.join("overlay.qcow2");
        std::fs::copy(snapshots.join("ready.qcow2"), &overlay)?;
        write_job(&req.scratch.join("job"), &req.guest_exe, &req.args)?;
        let dump_path = req.scratch.join("dump.txt");
        let serial_sock = req.scratch.join("serial.sock");
        let qmp_sock = req.scratch.join("qmp.sock");
        let args = measurement_args(&QemuLaunch {
            bundle: bundle.clone(),
            target: self.target,
            memory_mb: self.memory_mb,
            disk: overlay.clone(),
            loadvm: true,
            worktree: req.worktree.clone(),
            scratch: req.scratch.clone(),
            dump: dump_path.clone(),
            serial: serial_sock.clone(),
            qmp: qmp_sock.clone(),
        });
        let started = Instant::now();
        let mut child = spawn_qemu(&bundle, &args, &req.scratch, self.target)?;
        let pid = child.id().unwrap_or(0);
        let launch = async {
            wait_until_running(&qmp_sock).await?;
            let mut serial = connect_unix(&serial_sock).await?;
            serial.write_all(b"go\n").await?;
            serial.flush().await?;
            Ok::<(), anyhow::Error>(())
        };
        let launched = match tokio::time::timeout(Duration::from_secs(90), launch).await {
            Ok(result) => result,
            Err(_) => Err(anyhow!("timed out waiting for the guest to accept work")),
        };
        if let Err(err) = launched {
            signal_group(pid, "-KILL");
            let _ = child.kill().await;
            bail!("{err}\n{}", qemu_logs(&req.scratch));
        }
        let timeout = Duration::from_secs(req.timeout_secs);
        let status = tokio::time::timeout(timeout, child.wait()).await;
        let serial_log = serial_text(&req.scratch);
        match status {
            Ok(Ok(status)) if status.code().is_some() => {
                let mut measured =
                    read_measurement(&req.scratch, &dump_path, started.elapsed(), &serial_log)?;
                let elf = std::fs::read(&req.harness)?;
                measured.load_bias = profile::elf_load_bias(&elf, measured.load_bias)?;
                Ok(measured)
            }
            Ok(Ok(_)) | Ok(Err(_)) => {
                signal_group(pid, "-KILL");
                bail!(
                    "QEMU exited before the guest finished\n{}",
                    qemu_logs(&req.scratch)
                );
            }
            Err(_) => {
                signal_group(pid, "-KILL");
                let _ = child.kill().await;
                bail!(
                    "guest timed out after {} seconds\n{}",
                    req.timeout_secs,
                    qemu_logs(&req.scratch)
                );
            }
        }
    }
}

pub(crate) struct QemuLaunch {
    target: GuestTarget,
    bundle: PathBuf,
    memory_mb: u32,
    disk: PathBuf,
    loadvm: bool,
    worktree: PathBuf,
    scratch: PathBuf,
    dump: PathBuf,
    serial: PathBuf,
    qmp: PathBuf,
}

pub(crate) fn measurement_args(launch: &QemuLaunch) -> Vec<String> {
    let mut args = vec![
        "-accel".into(),
        "tcg".into(),
        "-machine".into(),
        launch.target.machine().into(),
        "-cpu".into(),
        launch.target.cpu().into(),
        "-smp".into(),
        "1".into(),
        "-m".into(),
        launch.memory_mb.to_string(),
        "-display".into(),
        "none".into(),
        "-L".into(),
        launch.bundle.join("pc-bios").display().to_string(),
        "-nic".into(),
        "none".into(),
        "-icount".into(),
        launch.target.icount().into(),
        "-no-reboot".into(),
        "-kernel".into(),
        launch.bundle.join("vmlinuz").display().to_string(),
        "-initrd".into(),
        launch
            .bundle
            .join("initramfs.cpio.gz")
            .display()
            .to_string(),
        "-append".into(),
        launch.target.console().into(),
        "-drive".into(),
        format!(
            "if=none,id=disk,file={},format=qcow2",
            qemu_option(&launch.disk)
        ),
        "-device".into(),
        "virtio-blk-pci,drive=disk".into(),
        "-fsdev".into(),
        format!(
            "local,id=work,path={},security_model=none,readonly=on",
            qemu_option(&launch.worktree)
        ),
        "-device".into(),
        "virtio-9p-pci,fsdev=work,mount_tag=work".into(),
        "-fsdev".into(),
        format!(
            "local,id=scratch,path={},security_model=none",
            qemu_option(&launch.scratch)
        ),
        "-device".into(),
        "virtio-9p-pci,fsdev=scratch,mount_tag=scratch".into(),
        "-chardev".into(),
        format!(
            "socket,id=ser,path={},server=on,wait=off,logfile={}",
            qemu_option(&launch.serial),
            qemu_option(&launch.scratch.join("serial.log"))
        ),
        "-serial".into(),
        "chardev:ser".into(),
        "-qmp".into(),
        format!("unix:{},server=on,wait=off", qemu_option(&launch.qmp)),
        "-plugin".into(),
        format!(
            "{},output={}",
            qemu_option(&launch.bundle.join("noctum_profile.so")),
            qemu_option(&launch.dump)
        ),
    ];
    if launch.loadvm {
        args.push("-loadvm".into());
        args.push("ready".into());
    }
    args
}

fn qemu_option(path: &Path) -> String {
    path.to_string_lossy().replace(',', ",,")
}

fn ensure_tools(bundle: &Path, target: GuestTarget) -> Result<()> {
    for name in [
        target.binary(),
        "qemu-img",
        "noctum_profile.so",
        "vmlinuz",
        "initramfs.cpio.gz",
    ] {
        if !bundle.join(name).is_file() {
            bail!("QEMU bundle is missing {name} in {}", bundle.display());
        }
    }
    if !bundle.join("pc-bios").join("bios-256k.bin").is_file() {
        bail!(
            "QEMU bundle is missing pc-bios/bios-256k.bin in {}",
            bundle.display()
        );
    }
    Ok(())
}

async fn ensure_snapshot(
    bundle: &Path,
    snapshots: &Path,
    memory_mb: u32,
    target: GuestTarget,
) -> Result<()> {
    let ready = snapshots.join("ready.qcow2");
    let stamp = snapshots.join("ready.stamp");
    let current = snapshot_stamp(bundle, memory_mb)?;
    if ready.is_file() && std::fs::read_to_string(&stamp).ok().as_deref() == Some(current.as_str())
    {
        return Ok(());
    }
    let img = bundle.join("qemu-img");
    let created = process::run(
        snapshots,
        &img,
        &["create", "-f", "qcow2", "ready.qcow2", "2G"],
        &[],
        Duration::from_secs(30),
    )
    .await;
    if !created.success {
        bail!("qemu-img create failed: {}", created.combined());
    }
    let scratch = snapshots.join("snapshot-scratch");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch)?;
    std::fs::write(scratch.join("job"), b"/bin/true\n")?;
    let serial = scratch.join("serial.sock");
    let qmp = scratch.join("qmp.sock");
    let args = measurement_args(&QemuLaunch {
        bundle: bundle.to_path_buf(),
        target,
        memory_mb,
        disk: ready.clone(),
        loadvm: false,
        worktree: scratch.clone(),
        scratch: scratch.clone(),
        dump: scratch.join("dump.txt"),
        serial: serial.clone(),
        qmp: qmp.clone(),
    });
    let mut child = spawn_qemu(bundle, &args, &scratch, target)?;
    let pid = child.id().unwrap_or(0);
    let held = hold_until(&serial, "NOCTUM_READY", Duration::from_secs(180)).await;
    let held = match held {
        Ok(stream) => stream,
        Err(err) => {
            signal_group(pid, "-KILL");
            let _ = child.kill().await;
            bail!("{err}\n{}", qemu_logs(&scratch));
        }
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut qmp = match Qmp::connect(&qmp).await {
        Ok(qmp) => qmp,
        Err(err) => {
            signal_group(pid, "-KILL");
            let _ = child.kill().await;
            bail!("{err}\n{}", qemu_logs(&scratch));
        }
    };
    let saved = tokio::time::timeout(
        Duration::from_secs(180),
        qmp.execute(
            &json!({"execute":"human-monitor-command","arguments":{"command-line":"savevm ready"}}),
        ),
    )
    .await;
    if let Err(err) = match saved {
        Ok(result) => result,
        Err(_) => Err(anyhow!("savevm timed out")),
    } {
        signal_group(pid, "-KILL");
        let _ = child.kill().await;
        bail!("savevm failed: {err}\n{}", qemu_logs(&scratch));
    }
    let _ = qmp.execute(&json!({"execute":"quit"})).await;
    let _ = tokio::time::timeout(Duration::from_secs(30), child.wait()).await;
    drop(held);
    std::fs::write(stamp, current)?;
    Ok(())
}

pub async fn fetch(data_dir: &Path) -> Result<()> {
    let lock = crate::lockfile::QemuLock::load()?;
    if !lock.published() {
        if lock.sha256.is_empty() {
            bail!(
                "QEMU bundle {} has no published url and sha256. Build it with qemu/build-bundle.sh and record the tarball hash in qemu.lock.toml",
                lock.id
            );
        }
        bail!(
            "QEMU bundle {} is pinned but has no download URL. Build it with qemu/build-bundle.sh and run `noctum qemu install` on the tarball.",
            lock.id
        );
    }
    let dest = crate::lockfile::bundle_archive(data_dir);
    crate::model::download_verified(&lock.url, &dest, &lock.sha256).await?;
    extract_bundle(&dest, &crate::lockfile::qemu_dir(data_dir))?;
    crate::lockfile::verify_qemu(data_dir)?;
    Ok(())
}

fn extract_bundle(archive: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    let file = std::fs::File::open(archive)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(decoder);
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            continue;
        }
        let out = dest.join(&path);
        if entry.header().entry_type().is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        entry.unpack(&out)?;
    }
    Ok(())
}

fn snapshot_stamp(bundle: &Path, memory_mb: u32) -> Result<String> {
    let kernel = crate::hashutil::sha256_file(&bundle.join("vmlinuz"))?;
    let initrd = crate::hashutil::sha256_file(&bundle.join("initramfs.cpio.gz"))?;
    Ok(format!("{kernel} {initrd} {memory_mb}\n"))
}

fn spawn_qemu(
    bundle: &Path,
    args: &[String],
    log_dir: &Path,
    target: GuestTarget,
) -> Result<tokio::process::Child> {
    let stdout = std::fs::File::create(log_dir.join("qemu.stdout"))?;
    let stderr = std::fs::File::create(log_dir.join("qemu.stderr"))?;
    let mut command = tokio::process::Command::new(bundle.join(target.binary()));
    command
        .args(args)
        .current_dir(bundle)
        .stdin(std::process::Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        command.process_group(0);
    }
    command.spawn().context("spawn qemu-system-x86_64")
}

fn write_job(path: &Path, exe: &str, args: &[String]) -> Result<()> {
    let mut body = format!("{exe}\n");
    for arg in args {
        body.push_str(arg);
        body.push('\n');
    }
    std::fs::write(path, body)?;
    Ok(())
}

fn read_measurement(
    scratch: &Path,
    dump_path: &Path,
    elapsed: Duration,
    serial_log: &str,
) -> Result<Measurement> {
    let dump_text = std::fs::read_to_string(dump_path).unwrap_or_default();
    let dump = if dump_text.is_empty() {
        ProfileDump {
            total: 0,
            blocks: Vec::new(),
            error: Some("missing profile dump".into()),
        }
    } else {
        profile::parse_dump(&dump_text)?
    };
    let stdout = std::fs::read(scratch.join("stdout")).unwrap_or_default();
    let stderr = std::fs::read(scratch.join("stderr")).unwrap_or_default();
    let exit_code = std::fs::read_to_string(scratch.join("exit"))
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(128);
    let (load_bias, map_start, map_end) =
        parse_map(&std::fs::read_to_string(scratch.join("map")).unwrap_or_default());
    Ok(Measurement {
        total: dump.total,
        stdout,
        stderr,
        exit_code,
        wall_ms: elapsed.as_millis() as u64,
        dump,
        map_start,
        map_end,
        load_bias,
        serial_log: serial_log.to_string(),
    })
}

fn parse_map(text: &str) -> (u64, u64, u64) {
    let mut bias = 0;
    let mut start = 0;
    let mut end = 0;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        match parts.next() {
            Some("bias") => bias = parse_hex(parts.next().unwrap_or("0")),
            Some("start") => start = parse_hex(parts.next().unwrap_or("0")),
            Some("end") => end = parse_hex(parts.next().unwrap_or("0")),
            _ => {}
        }
    }
    (bias, start, end)
}

fn parse_hex(text: &str) -> u64 {
    let text = text.strip_prefix("0x").unwrap_or(text);
    u64::from_str_radix(text, 16).unwrap_or(0)
}

fn serial_text(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("serial.log")).unwrap_or_default()
}

fn qemu_logs(dir: &Path) -> String {
    let mut out = String::new();
    for name in ["serial.log", "qemu.stderr", "qemu.stdout"] {
        let text = std::fs::read_to_string(dir.join(name)).unwrap_or_default();
        if text.is_empty() {
            continue;
        }
        out.push_str(name);
        out.push('\n');
        out.push_str(&crate::hashutil::tail_str(&text, 4096));
        out.push('\n');
    }
    out
}

async fn wait_until_running(path: &Path) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let mut qmp = Qmp::connect(path).await?;
    loop {
        if tokio::time::Instant::now() > deadline {
            bail!("timed out waiting for the guest to run");
        }
        let value = qmp.execute(&json!({"execute":"query-status"})).await?;
        let running = value
            .pointer("/return/running")
            .and_then(|item| item.as_bool())
            .unwrap_or(false);
        if running {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn connect_unix(path: &Path) -> Result<UnixStream> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(stream) = UnixStream::connect(path).await {
            return Ok(stream);
        }
        if tokio::time::Instant::now() > deadline {
            bail!("timed out connecting to {}", path.display());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn hold_until(path: &Path, needle: &str, timeout: Duration) -> Result<UnixStream> {
    let stream = connect_unix(path).await?;
    read_until_on(stream, needle, timeout).await
}

async fn read_until_on(stream: UnixStream, needle: &str, timeout: Duration) -> Result<UnixStream> {
    let mut reader = BufReader::new(stream);
    let mut collected = String::new();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if tokio::time::Instant::now() > deadline {
            bail!("timed out waiting for {needle}");
        }
        let mut line = String::new();
        let n = tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line)).await;
        match n {
            Ok(Ok(0)) => bail!("serial closed before {needle}"),
            Ok(Ok(_)) => {
                collected.push_str(&line);
                if collected.contains(needle) {
                    return Ok(reader.into_inner());
                }
            }
            _ => {}
        }
    }
}

struct Qmp {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl Qmp {
    async fn connect(path: &Path) -> Result<Self> {
        let stream = connect_unix(path).await?;
        let (read, write) = stream.into_split();
        let mut reader = BufReader::new(read);
        let mut greeting = String::new();
        reader.read_line(&mut greeting).await?;
        let mut qmp = Self {
            reader,
            writer: write,
        };
        qmp.execute(&json!({"execute":"qmp_capabilities"})).await?;
        Ok(qmp)
    }

    async fn execute(&mut self, command: &Value) -> Result<Value> {
        let mut line = serde_json::to_string(command)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await?;
        self.writer.flush().await?;
        loop {
            let mut buf = String::new();
            let n = self.reader.read_line(&mut buf).await?;
            if n == 0 {
                bail!("QMP closed");
            }
            let value: Value = serde_json::from_str(&buf).context("parse QMP")?;
            if value.get("error").is_some() {
                bail!("QMP error: {value}");
            }
            if value.get("return").is_some() {
                return Ok(value);
            }
        }
    }
}

pub fn install(data_dir: &Path, archive: &Path) -> Result<()> {
    let lock = crate::lockfile::QemuLock::load()?;
    if lock.sha256.is_empty() {
        bail!(
            "QEMU bundle {} has no sha256. Build it with qemu/build-bundle.sh and record the tarball hash in qemu.lock.toml",
            lock.id
        );
    }
    if !archive.is_file() {
        bail!("bundle archive {} is missing", archive.display());
    }
    let got = crate::hashutil::sha256_file(archive)?;
    if got != lock.sha256 {
        bail!("tarball sha256 is {got}, lockfile says {}", lock.sha256);
    }
    let dest = crate::lockfile::bundle_archive(data_dir);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if archive.canonicalize().ok() != dest.canonicalize().ok() {
        std::fs::copy(archive, &dest)?;
    }
    extract_bundle(&dest, &crate::lockfile::qemu_dir(data_dir))?;
    crate::lockfile::verify_qemu(data_dir)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measurement_command_is_the_pinned_guest() {
        let mut launch = QemuLaunch {
            bundle: PathBuf::from("/bundle"),
            target: GuestTarget::X86_64,
            memory_mb: 1024,
            disk: PathBuf::from("/tmp/overlay.qcow2"),
            loadvm: true,
            worktree: PathBuf::from("/work"),
            scratch: PathBuf::from("/scratch"),
            dump: PathBuf::from("/scratch/dump.txt"),
            serial: PathBuf::from("/scratch/serial.sock"),
            qmp: PathBuf::from("/scratch/qmp.sock"),
        };
        let args = measurement_args(&launch);
        let text = args.join(" ");
        assert!(text.contains("-accel tcg"));
        assert!(text.contains("-cpu qemu64"));
        assert!(text.contains("-smp 1"));
        assert!(text.contains("-icount shift=0,sleep=off"));
        assert!(text.contains("-nic none"));
        assert!(text.contains("-loadvm ready"));
        assert!(text.contains("readonly=on"));
        assert!(!text.contains("kvm"));
        assert!(!text.contains("-cpu host"));
        assert!(!text.contains("virtio-rng"));
        assert!(text.contains("pc-bios"));
        assert!(text.contains("logfile="));
        launch.target = GuestTarget::Aarch64;
        launch.worktree = PathBuf::from("/work, tests");
        let text = measurement_args(&launch).join(" ");
        assert!(text.contains("path=/work,, tests,security_model=none"));
        assert!(text.contains("-machine virt,gic-version=2"));
        assert!(text.contains("-cpu cortex-a72"));
        assert!(text.contains("console=ttyAMA0"));
        assert!(text.contains("-accel tcg"));
        assert!(text.contains("-icount shift=0,sleep=on"));
    }

    #[test]
    fn map_line_is_the_load_bias() {
        let (bias, start, end) = parse_map("bias 0x400000\nstart 0x401000\nend 0x402000\n");
        assert_eq!(bias, 0x400000);
        assert_eq!(start, 0x401000);
        assert_eq!(end, 0x402000);
    }
}
