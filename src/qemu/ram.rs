use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use flate2::{write::GzEncoder, Compression};

use crate::ports::MeasureRequest;

const MAGIC: &[u8] = b"\nNOCTUM_RESULT_V1\n";
const OUTPUT_LIMIT: u64 = 64 * 1024 * 1024;

struct Archive<W: Write> {
    output: W,
    directories: BTreeSet<PathBuf>,
    size: u64,
    limit: u64,
    inode: u32,
}

impl<W: Write> Archive<W> {
    fn entry(&mut self, name: &str, mode: u32, body: &[u8]) -> Result<()> {
        if name.starts_with('/')
            || name.as_bytes().contains(&0)
            || Path::new(name)
                .components()
                .any(|c| c == Component::ParentDir)
        {
            bail!("invalid guest archive path: {name}");
        }
        let name_len = name.len() + 1;
        let size = 110 + name_len + body.len() + 6;
        self.size += size as u64;
        if self.size > self.limit || body.len() > u32::MAX as usize {
            bail!("guest input is too large; increase guest.memory_mb or reduce source fixtures");
        }
        self.inode += 1;
        write!(self.output, "070701")?;
        for field in [
            self.inode,
            mode,
            0,
            0,
            1,
            0,
            body.len() as u32,
            0,
            0,
            0,
            0,
            name_len as u32,
            0,
        ] {
            write!(self.output, "{field:08x}")?;
        }
        self.output.write_all(name.as_bytes())?;
        self.output.write_all(&[0])?;
        self.output
            .write_all(&[0; 3][..(4 - (110 + name_len) % 4) % 4])?;
        self.output.write_all(body)?;
        self.output.write_all(&[0; 3][..(4 - body.len() % 4) % 4])?;
        Ok(())
    }

    fn parents(&mut self, path: &Path) -> Result<()> {
        let mut parents: Vec<_> = path
            .ancestors()
            .skip(1)
            .filter(|p| !p.as_os_str().is_empty())
            .collect();
        parents.reverse();
        for parent in parents {
            if self.directories.insert(parent.to_path_buf()) {
                self.entry(
                    parent.to_str().context("guest path must be UTF-8")?,
                    0o040755,
                    &[],
                )?;
            }
        }
        Ok(())
    }

    fn file(&mut self, source: &Path, name: &Path, root: &Path) -> Result<()> {
        self.parents(name)?;
        let metadata = fs::symlink_metadata(source)?;
        let name = name.to_str().context("guest path must be UTF-8")?;
        if metadata.file_type().is_symlink() {
            let resolved = source
                .canonicalize()
                .with_context(|| format!("read guest input link {}", source.display()))?;
            if !resolved.starts_with(root) {
                bail!("guest input link {} points outside the repository; copy its input into the repository", source.display());
            }
            let target = fs::read_link(source)?;
            self.entry(
                name,
                0o120777,
                target
                    .to_str()
                    .context("guest link must be UTF-8")?
                    .as_bytes(),
            )?;
        } else if metadata.is_file() {
            if metadata.len() > self.limit.saturating_sub(self.size) {
                bail!("guest input {} is too large; increase guest.memory_mb or reduce source fixtures", source.display());
            }
            let mut body = Vec::new();
            File::open(source)?
                .take(self.limit.saturating_sub(self.size) + 1)
                .read_to_end(&mut body)?;
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode() & 0o777
            };
            #[cfg(not(unix))]
            let mode = 0o644;
            self.entry(name, 0o100000 | mode, &body)?;
        } else if metadata.is_dir() {
            if self.directories.insert(PathBuf::from(name)) {
                self.entry(name, 0o040755, &[])?;
            }
        } else {
            bail!(
                "guest input {} is not a file, link, or directory",
                source.display()
            );
        }
        Ok(())
    }
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub(super) async fn build(bundle: &Path, request: &MeasureRequest) -> Result<PathBuf> {
    let root = request.worktree.canonicalize()?;
    let scratch = request.scratch.canonicalize()?;
    let binary = request
        .harness
        .strip_prefix(&request.worktree)
        .context("test binary must be inside the worktree")?;
    let cwd = request
        .working_directory
        .as_deref()
        .unwrap_or(&request.worktree);
    let cwd = cwd
        .strip_prefix(&request.worktree)
        .context("package directory must be inside the worktree")?;
    let listing = crate::process::run(
        &root,
        "git",
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--exclude=/target/",
        ],
        &[],
        Duration::from_secs(30),
    )
    .await;
    if !listing.success {
        bail!("cannot list guest inputs: {}", listing.combined());
    }
    let mut paths = BTreeSet::new();
    for name in listing.stdout.split('\0').filter(|s| !s.is_empty()) {
        let path = PathBuf::from(name);
        if path.components().any(|c| c == Component::ParentDir) || path.is_absolute() {
            bail!("guest input is outside the repository: {name}");
        }
        if !root.join(&path).starts_with(&scratch) {
            root.join(&path)
                .symlink_metadata()
                .with_context(|| format!("cannot read guest input {}", path.display()))?;
            paths.insert(path);
        }
    }
    paths.insert(binary.to_path_buf());
    let destination = request.scratch.join("input.cpio.gz");
    let mut output = File::create(&destination)?;
    std::io::copy(
        &mut File::open(bundle.join("initramfs.cpio.gz"))?,
        &mut output,
    )?;
    let mut archive = Archive {
        output: GzEncoder::new(output, Compression::fast()),
        directories: BTreeSet::new(),
        size: 0,
        limit: u64::from(request.memory_mb) * 1024 * 1024 / 4,
        inode: 10000,
    };
    for path in paths {
        archive.file(&root.join(&path), &Path::new("work").join(&path), &root)?;
    }
    for original in BTreeSet::from([root.as_path(), request.worktree.as_path()]) {
        if original != Path::new("/work") {
            let alias = original
                .strip_prefix("/")
                .context("worktree path must be absolute")?;
            archive.parents(alias)?;
            archive.entry(
                alias.to_str().context("worktree path must be UTF-8")?,
                0o120777,
                b"/work",
            )?;
        }
    }
    archive.entry("scratch", 0o040755, &[])?;
    let executable = format!(
        "/work/{}",
        binary.to_str().context("test binary path must be UTF-8")?
    );
    let mut job = String::new();
    if request.args.len() > 63 {
        bail!("guest supports at most 63 test arguments");
    }
    for argument in std::iter::once(&executable).chain(request.args.iter()) {
        if argument.len() > 1022 {
            bail!("guest argument exceeds 1022 bytes");
        }
        if argument.contains(['\n', '\r', '\0']) {
            bail!("guest arguments must not contain line breaks or NUL bytes");
        }
        job.push_str(argument);
        job.push('\n');
    }
    archive.entry("scratch/job", 0o100600, job.as_bytes())?;
    let cwd = quote(&format!(
        "/work/{}",
        cwd.to_str().context("package path must be UTF-8")?
    ));
    let init = format!("#!/bin/sh\nexport PATH=/bin:/sbin\nmount -t proc proc /proc\nmount -t sysfs sysfs /sys\nmount -t devtmpfs devtmpfs /dev\necho 0 > /proc/sys/kernel/randomize_va_space\necho '0 0 0 0' > /proc/sys/kernel/printk\nexec </dev/console >/dev/console 2>&1\nmount -o bind /work /work || poweroff -f\nmount -o remount,bind,ro /work || poweroff -f\ncd {cwd} || poweroff -f\n/bin/runner /scratch/job /scratch /dev/console\npoweroff -f\n");
    archive.entry("init", 0o100755, init.as_bytes())?;
    archive.entry("TRAILER!!!", 0, &[])?;
    archive.output.finish()?.sync_all()?;
    Ok(destination)
}

pub(super) fn unpack(serial: &[u8], scratch: &Path) -> Result<()> {
    let start = serial
        .windows(MAGIC.len())
        .position(|p| p == MAGIC)
        .context("guest result is missing")?;
    let mut body = &serial[start + MAGIC.len()..];
    let mut hash = 0xcbf29ce484222325u64;
    let mut total = 0u64;
    for name in ["stdout", "stderr", "map", "exit"] {
        let bytes = body.get(..8).context("guest result length is missing")?;
        let length = u64::from_le_bytes(bytes.try_into()?);
        total = total
            .checked_add(length)
            .context("guest result length overflow")?;
        if total > OUTPUT_LIMIT {
            bail!("guest result exceeds 64 MiB");
        }
        let data = body
            .get(8..8 + length as usize)
            .context("guest result is incomplete")?;
        for byte in bytes.iter().chain(data) {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
        }
        fs::write(scratch.join(name), data)?;
        body = &body[8 + length as usize..];
    }
    let expected = u64::from_le_bytes(
        body.get(..8)
            .context("guest result checksum is missing")?
            .try_into()?,
    );
    if hash != expected {
        bail!("guest result checksum does not match");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn archive_has_fixtures_binary_absolute_alias_and_package_directory() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let bundle = tempfile::tempdir().unwrap();
        let encoder = GzEncoder::new(
            File::create(bundle.path().join("initramfs.cpio.gz")).unwrap(),
            Compression::fast(),
        );
        encoder.finish().unwrap();
        for path in ["member/fixtures", "target", "vendor/cmark-gfm"] {
            fs::create_dir_all(root.join(path)).unwrap();
        }
        fs::write(root.join("member/fixtures/input.bin"), b"\0\xff\n").unwrap();
        fs::write(root.join("target/harness"), b"test binary").unwrap();
        assert!(
            crate::process::run(&root, "git", &["init", "-q"], &[], Duration::from_secs(10))
                .await
                .success
        );
        assert!(
            crate::process::run(
                &root,
                "git",
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    "160000,2f13eeedfe9906c72a1843b03552550af7bee29a,vendor/cmark-gfm",
                ],
                &[],
                Duration::from_secs(10),
            )
            .await
            .success
        );
        let request = MeasureRequest {
            worktree: root.clone(),
            working_directory: Some(root.join("member")),
            harness: root.join("target/harness"),
            guest_exe: "/work/target/harness".into(),
            args: vec!["tests::fixture".into(), "--exact".into()],
            timeout_secs: 10,
            scratch: scratch.path().into(),
            memory_mb: 512,
        };
        let input = build(bundle.path(), &request).await.unwrap();
        let mut bytes = Vec::new();
        flate2::read::MultiGzDecoder::new(File::open(input).unwrap())
            .read_to_end(&mut bytes)
            .unwrap();
        let mut files = std::collections::BTreeMap::new();
        let mut modes = std::collections::BTreeMap::new();
        let mut offset = 0;
        loop {
            let header = &bytes[offset..offset + 110];
            assert_eq!(&header[..6], b"070701");
            let number = |range| {
                usize::from_str_radix(std::str::from_utf8(&header[range]).unwrap(), 16).unwrap()
            };
            let size = number(54..62);
            let mode = number(14..22);
            let name_len = number(94..102);
            let name = std::str::from_utf8(&bytes[offset + 110..offset + 110 + name_len - 1])
                .unwrap()
                .to_string();
            offset = (offset + 110 + name_len + 3) & !3;
            files.insert(name.clone(), bytes[offset..offset + size].to_vec());
            modes.insert(name.clone(), mode);
            offset = (offset + size + 3) & !3;
            if name == "TRAILER!!!" {
                break;
            }
        }
        assert_eq!(files["work/member/fixtures/input.bin"], b"\0\xff\n");
        assert_eq!(files["work/target/harness"], b"test binary");
        assert_eq!(modes["work/vendor/cmark-gfm"], 0o040755);
        assert!(files["work/vendor/cmark-gfm"].is_empty());
        assert_eq!(
            files[root.strip_prefix("/").unwrap().to_str().unwrap()],
            b"/work"
        );
        assert!(String::from_utf8_lossy(&files["init"]).contains("cd '/work/member'"));
        assert_eq!(
            files["scratch/job"],
            b"/work/target/harness\ntests::fixture\n--exact\n"
        );
        let mut small = request.clone();
        small.memory_mb = 0;
        assert!(build(bundle.path(), &small).await.is_err());
    }

    #[test]
    fn result_keeps_arbitrary_bytes_and_rejects_damage() {
        let directory = tempfile::tempdir().unwrap();
        let outputs: [&[u8]; 4] = [
            b"\0\xff\r\nNOCTUM_RESULT_V1\n",
            b"\x80\n",
            b"start 0x400000\n",
            b"0\n",
        ];
        let mut frame = MAGIC.to_vec();
        let mut hash = 0xcbf29ce484222325u64;
        for output in outputs {
            let length = (output.len() as u64).to_le_bytes();
            for byte in length.iter().chain(output) {
                hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
            }
            frame.extend(length);
            frame.extend(output);
        }
        frame.extend(hash.to_le_bytes());
        unpack(&frame, directory.path()).unwrap();
        assert_eq!(
            fs::read(directory.path().join("stdout")).unwrap(),
            outputs[0]
        );
        assert_eq!(
            fs::read(directory.path().join("stderr")).unwrap(),
            outputs[1]
        );
        for end in 0..frame.len() {
            assert!(unpack(&frame[..end], directory.path()).is_err());
        }
        frame[MAGIC.len() + 8] ^= 1;
        assert!(unpack(&frame, directory.path()).is_err());
    }
}
