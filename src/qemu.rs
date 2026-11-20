//! Pinned system-mode QEMU. The guest is the ruler: one vCPU, TCG, icount,
//! no NIC, no virtio-rng, CPU model `qemu64`, and a fresh disk overlay.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::lockfile::qemu_dir;
use crate::ports::{MeasureRequest, Measurement, Measurer};
use crate::process::{self, signal_group};
use crate::profile::{self, ProfileDump};

pub struct QemuRunner {
    pub data_dir: PathBuf,
    pub memory_mb: u32,
}

impl QemuRunner {
    pub fn new(data_dir: impl Into<PathBuf>, memory_mb: u32) -> Self {
        Self {
            data_dir: data_dir.into(),
            memory_mb,
        }
    }

    fn bundle(&self) -> PathBuf {
        qemu_dir(&self.data_dir)
    }
}

impl Measurer for QemuRunner {
    async fn measure(&self, req: &MeasureRequest) -> Result<Measurement> {
        let bundle = self.bundle();
        ensure_tools(&bundle)?;
        std::fs::create_dir_all(&req.scratch)?;
        ensure_snapshot(&bundle, self.memory_mb).await?;
        let overlay = req.scratch.join("overlay.qcow2");
        std::fs::copy(bundle.join("ready.qcow2"), &overlay)?;
        write_job(&req.scratch.join("job"), &req.guest_exe, &req.args)?;
        let dump_path = req.scratch.join("dump.txt");
        let serial_sock = req.scratch.join("serial.sock");
        let qmp_sock = req.scratch.join("qmp.sock");
        let args = measurement_args(&QemuLaunch {
            bundle: bundle.clone(),
            memory_mb: self.memory_mb,
            disk: overlay.clone(),
            loadvm: true,
            worktree: req.worktree.clone(),
            scratch: req.scratch.clone(),
            dump: dump_path.clone(),
            serial: serial_sock.clone(),
            qmp: qmp_sock,
        });
        let started = Instant::now();
        let mut child = spawn_qemu(&bundle, &args)?;
        let pid = child.id().unwrap_or(0);
        let go = async {
            let mut serial = connect_unix(&serial_sock).await?;
            serial.write_all(b"go\n").await?;
            serial.flush().await?;
            Ok::<(), anyhow::Error>(())
        };
        let _ = go.await;
        let timeout = Duration::from_secs(req.timeout_secs);
        let status = tokio::time::timeout(timeout, child.wait()).await;
        let serial_log =
            std::fs::read_to_string(req.scratch.join("serial.log")).unwrap_or_default();
        match status {
            Ok(Ok(status)) if status.success() || status.code().is_some() => {
                read_measurement(&req.scratch, &dump_path, started.elapsed(), &serial_log)
            }
            Ok(Ok(_)) | Ok(Err(_)) => {
                signal_group(pid, "-KILL");
                bail!("QEMU exited before the guest finished");
            }
            Err(_) => {
                signal_group(pid, "-KILL");
                let _ = child.kill().await;
                bail!("guest timed out after {} seconds", req.timeout_secs);
            }
        }
    }
}

pub(crate) struct QemuLaunch {
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
        "pc".into(),
        "-cpu".into(),
        "qemu64".into(),
        "-smp".into(),
        "1".into(),
        "-m".into(),
        launch.memory_mb.to_string(),
        "-display".into(),
        "none".into(),
        "-nic".into(),
        "none".into(),
        "-icount".into(),
        "shift=0,sleep=off".into(),
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
        "console=ttyS0 nokaslr panic=1 loglevel=3".into(),
        "-drive".into(),
        format!(
            "if=none,id=disk,file={},format=qcow2",
            launch.disk.display()
        ),
        "-device".into(),
        "virtio-blk-pci,drive=disk".into(),
        "-fsdev".into(),
        format!(
            "local,id=work,path={},security_model=none,readonly=on",
            launch.worktree.display()
        ),
        "-device".into(),
        "virtio-9p-pci,fsdev=work,mount_tag=work".into(),
        "-fsdev".into(),
        format!(
            "local,id=scratch,path={},security_model=none",
            launch.scratch.display()
        ),
        "-device".into(),
        "virtio-9p-pci,fsdev=scratch,mount_tag=scratch".into(),
        "-chardev".into(),
        format!(
            "socket,id=ser,path={},server=on,wait=off",
            launch.serial.display()
        ),
        "-serial".into(),
        "chardev:ser".into(),
        "-qmp".into(),
        format!("unix:{},server=on,wait=off", launch.qmp.display()),
        "-plugin".into(),
        format!(
            "{},output={}",
            launch.bundle.join("noctum_profile.so").display(),
            launch.dump.display()
        ),
    ];
    if launch.loadvm {
        args.push("-loadvm".into());
        args.push("ready".into());
    }
    args
}

fn ensure_tools(bundle: &Path) -> Result<()> {
    for name in [
        "qemu-system-x86_64",
        "qemu-img",
        "noctum_profile.so",
        "vmlinuz",
        "initramfs.cpio.gz",
    ] {
        if !bundle.join(name).is_file() {
            bail!("QEMU bundle is missing {name} in {}", bundle.display());
        }
    }
    Ok(())
}

async fn ensure_snapshot(bundle: &Path, memory_mb: u32) -> Result<()> {
    let ready = bundle.join("ready.qcow2");
    let stamp = bundle.join("ready.stamp");
    let current = snapshot_stamp(bundle)?;
    if ready.is_file() && std::fs::read_to_string(&stamp).ok().as_deref() == Some(current.as_str())
    {
        return Ok(());
    }
    let img = bundle.join("qemu-img");
    let created = process::run(
        bundle,
        &img,
        &["create", "-f", "qcow2", "ready.qcow2", "64M"],
        &[],
        Duration::from_secs(30),
    )
    .await;
    if !created.success {
        bail!("qemu-img create failed: {}", created.combined());
    }
    let scratch = bundle.join("snapshot-scratch");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch)?;
    std::fs::write(scratch.join("job"), b"/bin/true\n")?;
    let serial = scratch.join("serial.sock");
    let qmp = scratch.join("qmp.sock");
    let args = measurement_args(&QemuLaunch {
        bundle: bundle.to_path_buf(),
        memory_mb,
        disk: ready.clone(),
        loadvm: false,
        worktree: scratch.clone(),
        scratch: scratch.clone(),
        dump: scratch.join("dump.txt"),
        serial: serial.clone(),
        qmp: qmp.clone(),
    });
    let mut child = spawn_qemu(bundle, &args)?;
    let pid = child.id().unwrap_or(0);
    let held = hold_until(&serial, "NOCTUM_READY", Duration::from_secs(120)).await;
    let held = match held {
        Ok(stream) => stream,
        Err(err) => {
            signal_group(pid, "-KILL");
            let _ = child.kill().await;
            return Err(err);
        }
    };
    let mut qmp = Qmp::connect(&qmp).await?;
    qmp.execute(
        &json!({"execute":"human-monitor-command","arguments":{"command-line":"savevm ready"}}),
    )
    .await?;
    let _ = qmp.execute(&json!({"execute":"quit"})).await;
    let _ = tokio::time::timeout(Duration::from_secs(30), child.wait()).await;
    drop(held);
    std::fs::write(stamp, current)?;
    Ok(())
}

pub async fn fetch(data_dir: &Path) -> Result<()> {
    let lock = crate::lockfile::QemuLock::load()?;
    if !lock.published() {
        bail!(
            "QEMU bundle {} has no published url and sha256. Build it with qemu/build-bundle.sh and record the tarball hash in qemu.lock.toml",
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

fn snapshot_stamp(bundle: &Path) -> Result<String> {
    let kernel = crate::hashutil::sha256_file(&bundle.join("vmlinuz"))?;
    let initrd = crate::hashutil::sha256_file(&bundle.join("initramfs.cpio.gz"))?;
    Ok(format!("{kernel} {initrd}\n"))
}

fn spawn_qemu(bundle: &Path, args: &[String]) -> Result<tokio::process::Child> {
    let mut command = tokio::process::Command::new(bundle.join("qemu-system-x86_64"));
    command
        .args(args)
        .current_dir(bundle)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measurement_command_is_the_pinned_guest() {
        let launch = QemuLaunch {
            bundle: PathBuf::from("/bundle"),
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
    }

    #[test]
    fn map_line_is_the_load_bias() {
        let (bias, start, end) = parse_map("bias 0x400000\nstart 0x401000\nend 0x402000\n");
        assert_eq!(bias, 0x400000);
        assert_eq!(start, 0x401000);
        assert_eq!(end, 0x402000);
    }
}
