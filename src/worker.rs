//! Linux operations for the local container worker.
use crate::ports::{MeasureRequest, Measurer};
use crate::runtime::protocol::{ExecRequest, Reply};
use anyhow::{bail, Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn request<T: DeserializeOwned>() -> Result<T> {
    serde_json::from_reader(std::io::stdin().lock()).context("read worker request")
}
fn reply<T: Serialize>(result: Result<T>) -> Result<()> {
    let response = match result {
        Ok(value) => Reply {
            value: Some(value),
            error: None,
        },
        Err(error) => Reply {
            value: None,
            error: Some(format!("{error:#}")),
        },
    };
    serde_json::to_writer(std::io::stdout().lock(), &response)?;
    std::io::stdout().flush()?;
    Ok(())
}

pub async fn serve() -> Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("supervise") => supervise().await,
        Some("ready") => {
            let expected =
                std::env::var("NOCTUM_WORKER_SESSION").context("worker session is missing")?;
            reply(Ok(std::fs::read_to_string("/runtime/ready")
                .ok()
                .as_deref()
                == Some(&expected)))
        }
        Some("exec") => {
            let result = async {
                let r: ExecRequest = request()?;
                let env: Vec<_> = r
                    .env
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str()))
                    .collect();
                Ok(crate::process::run(
                    &r.cwd,
                    &r.program,
                    &r.args,
                    &env,
                    Duration::from_secs(r.timeout_secs),
                )
                .await)
            }
            .await;
            reply(result)
        }
        Some("profile") => reply((|| {
            Ok((
                std::fs::read_to_string("/opt/noctum/guest/MEASUREMENT_VERSION")?
                    .trim()
                    .to_string(),
                crate::hashutil::sha256_file(Path::new("/opt/noctum/guest/MANIFEST"))?,
            ))
        })()),
        Some("measure") => {
            let result = async {
                let mut r: MeasureRequest = request()?;
                let destination = r.scratch.clone();
                let scratch = tempfile::tempdir_in("/runtime/scratch")?;
                r.scratch = scratch.path().to_path_buf();
                let measurement = crate::qemu::QemuRunner::at_bundle(
                    PathBuf::from("/opt/noctum/guest"),
                    PathBuf::from("/runtime/snapshots"),
                    r.memory_mb,
                )
                .measure(&r)
                .await;
                std::fs::create_dir_all(&destination)?;
                for name in [
                    "dump.txt",
                    "qemu.stdout",
                    "qemu.stderr",
                    "serial.log",
                    "stdout",
                    "stderr",
                    "exit",
                    "map",
                    "job",
                ] {
                    let path = scratch.path().join(name);
                    if path.is_file() {
                        std::fs::copy(path, destination.join(name))?;
                    }
                }
                measurement
            }
            .await;
            reply(result)
        }
        _ => bail!("unknown worker operation"),
    }
}

async fn checked(cwd: &Path, program: &str, args: &[String]) -> Result<()> {
    let output = crate::process::run(cwd, program, args, &[], Duration::from_secs(600)).await;
    if !output.success {
        bail!("{program} failed: {}", output.combined());
    }
    Ok(())
}

async fn supervise() -> Result<()> {
    let uid: u32 = std::env::var("NOCTUM_WORKER_UID")?.parse()?;
    let gid: u32 = std::env::var("NOCTUM_WORKER_GID")?.parse()?;
    let session = std::env::var("NOCTUM_WORKER_SESSION")?;
    let _ = std::fs::remove_file("/runtime/ready");
    for directory in [
        "/runtime/snapshots",
        "/runtime/scratch",
        "/cache/cargo",
        "/cache/home",
    ] {
        std::fs::create_dir_all(directory)?;
    }
    checked(
        Path::new("/opt/noctum/guest"),
        "sha256sum",
        &["-c".into(), "MANIFEST".into()],
    )
    .await?;
    let architecture = std::fs::read_to_string("/opt/noctum/guest/ARCH")?;
    if architecture.trim() != crate::target::GuestTarget::native().name() {
        bail!("guest architecture does not match the worker");
    }
    let owner = format!("{uid}:{gid}");
    for (directory, marker) in [
        ("/cache", "/cache/owner"),
        ("/usr/local/rustup", "/usr/local/rustup/.noctum-owner"),
    ] {
        if std::fs::read_to_string(marker).ok().as_deref() != Some(&owner) {
            checked(
                Path::new("/"),
                "chown",
                &["-R".into(), owner.clone(), directory.into()],
            )
            .await?;
            std::fs::write(marker, &owner)?;
        }
    }
    checked(
        Path::new("/"),
        "chown",
        &[
            "-R".into(),
            owner,
            "/runtime/snapshots".into(),
            "/runtime/scratch".into(),
        ],
    )
    .await?;
    std::fs::write("/runtime/ready", session)?;
    std::future::pending::<()>().await;
    Ok(())
}
