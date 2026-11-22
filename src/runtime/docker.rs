use super::{protocol::Reply, source};
use crate::process;
use anyhow::{bail, Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct DockerSession(Arc<Session>);
#[derive(Debug)]
struct Session {
    name: String,
    user: String,
    directory: tempfile::TempDir,
    stopped: AtomicBool,
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.directory.path().join("heartbeat"));
        if !self.stopped.swap(true, Ordering::SeqCst) {
            if let Ok(mut child) = std::process::Command::new("docker")
                .args(["rm", "--force", &self.name])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
        }
    }
}

pub fn image_tag() -> String {
    format!(
        "noctum-worker:{}-{}",
        &source::identity()[..24],
        std::env::consts::ARCH
    )
}

pub async fn image_ready() -> bool {
    let output = process::run(
        Path::new("/"),
        "docker",
        &["image", "inspect", "--format", "{{.Id}}", &image_tag()],
        &[],
        Duration::from_secs(10),
    )
    .await;
    output.success
}

pub async fn prepare_image(data: &Path) -> Result<()> {
    let info = process::run(
        Path::new("/"),
        "docker",
        &["info", "--format", "{{.OSType}}"],
        &[],
        Duration::from_secs(15),
    )
    .await;
    if !info.success || info.stdout.trim() != "linux" {
        bail!(
            "Docker with a Linux engine must be running. {}",
            info.combined()
        );
    }
    if image_ready().await {
        return Ok(());
    }
    std::fs::create_dir_all(data.join("runtime"))?;
    let context = tempfile::tempdir_in(data.join("runtime"))?;
    source::write(context.path())?;
    let args = vec![
        "build".into(),
        "--platform".into(),
        platform().into(),
        "--tag".into(),
        image_tag(),
        "--file".into(),
        "runtime/Dockerfile".into(),
        ".".into(),
    ];
    let result = process::run_logged(
        context.path(),
        "docker",
        &args,
        Duration::from_secs(3600),
        &data.join("runtime/build.log"),
    )
    .await?;
    if !result.success {
        bail!(
            "Linux worker build failed. See {}",
            data.join("runtime/build.log").display()
        );
    }
    Ok(())
}

impl DockerSession {
    pub async fn start(data: &Path, repositories: &[PathBuf]) -> Result<Self> {
        let uid = id("-u").await?;
        let gid = id("-g").await?;
        std::fs::create_dir_all(data.join("runtime/sessions"))?;
        let directory = tempfile::tempdir_in(data.join("runtime/sessions"))?;
        let name = format!(
            "noctum-{}-{}",
            std::process::id(),
            directory
                .path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .trim_start_matches('.')
        );
        let session = Self(Arc::new(Session {
            name: name.clone(),
            user: format!("{uid}:{gid}"),
            directory,
            stopped: AtomicBool::new(false),
        }));
        session.heartbeat()?;
        let weak = Arc::downgrade(&session.0);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let Some(inner) = weak.upgrade() else {
                    break;
                };
                if inner.stopped.load(Ordering::SeqCst) {
                    break;
                }
                let _ = std::fs::write(inner.directory.path().join("heartbeat"), b"alive");
            }
        });
        let key = crate::hashutil::sha256_bytes(data.as_os_str().as_encoded_bytes());
        let old = process::run(
            Path::new("/"),
            "docker",
            &[
                "ps",
                "--all",
                "--quiet",
                "--filter",
                &format!("label=org.noctum.data={}", &key[..12]),
            ],
            &[],
            Duration::from_secs(15),
        )
        .await;
        if !old.success {
            bail!("cannot check previous Linux workers: {}", old.combined());
        }
        let ids: Vec<_> = old
            .stdout
            .split_whitespace()
            .filter(|id| id.bytes().all(|b| b.is_ascii_hexdigit()))
            .collect();
        if !ids.is_empty() {
            let mut remove = vec!["rm", "--force"];
            remove.extend(ids);
            let result = process::run(
                Path::new("/"),
                "docker",
                &remove,
                &[],
                Duration::from_secs(20),
            )
            .await;
            if !result.success {
                bail!(
                    "cannot close a previous Linux worker: {}",
                    result.combined()
                );
            }
        }
        let mut args = vec![
            "run".into(),
            "--detach".into(),
            "--rm".into(),
            "--init".into(),
            "--platform".into(),
            platform().into(),
            "--name".into(),
            name.clone(),
            "--label".into(),
            "org.noctum.worker=true".into(),
            "--label".into(),
            format!("org.noctum.data={}", &key[..12]),
            "--env".into(),
            format!("NOCTUM_WORKER_UID={uid}"),
            "--env".into(),
            format!("NOCTUM_WORKER_GID={gid}"),
            "--env".into(),
            format!("NOCTUM_WORKER_SESSION={name}"),
            "--env".into(),
            format!(
                "NOCTUM_WORKER_HEARTBEAT={}",
                session.0.directory.path().join("heartbeat").display()
            ),
            "--mount".into(),
            bind_mount(data, false),
            "--mount".into(),
            format!(
                "type=volume,source=noctum-{}-{}-{},target=/runtime",
                &key[..12],
                &source::identity()[..12],
                std::env::consts::ARCH
            ),
            "--mount".into(),
            format!(
                "type=volume,source=noctum-{}-cargo-{},target=/cache",
                &key[..12],
                std::env::consts::ARCH
            ),
            "--mount".into(),
            format!(
                "type=volume,source=noctum-{}-rustup-{}-{},target=/usr/local/rustup",
                &key[..12],
                std::env::consts::ARCH,
                &crate::hashutil::sha256_bytes(include_bytes!("../../runtime/Dockerfile"))[..8]
            ),
        ];
        let mut mounts = repositories.to_vec();
        mounts.sort();
        mounts.dedup();
        for path in mounts {
            if path == data {
                continue;
            }
            args.extend(["--mount".into(), bind_mount(&path, true)]);
        }
        args.extend([image_tag(), "supervise".into()]);
        let output = process::run(
            Path::new("/"),
            "docker",
            &args,
            &[],
            Duration::from_secs(120),
        )
        .await;
        if !output.success {
            bail!("cannot start the Linux worker: {}", output.combined());
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(600);
        while tokio::time::Instant::now() < deadline {
            if session
                .rpc::<_, bool>("ready", &(), Duration::from_secs(10))
                .await
                .unwrap_or(false)
            {
                return Ok(session);
            }
            let state = process::run(
                Path::new("/"),
                "docker",
                &["inspect", "--format", "{{.State.Running}}", &name],
                &[],
                Duration::from_secs(10),
            )
            .await;
            if !state.success || state.stdout.trim() != "true" {
                bail!("Linux worker exited during startup: {}", state.combined());
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        bail!("Linux worker did not become ready")
    }
    fn heartbeat(&self) -> Result<()> {
        std::fs::write(self.0.directory.path().join("heartbeat"), b"alive")?;
        Ok(())
    }
    pub async fn rpc<Q: Serialize, R: DeserializeOwned>(
        &self,
        operation: &str,
        request: &Q,
        timeout: Duration,
    ) -> Result<R> {
        if self.0.stopped.load(Ordering::SeqCst) {
            bail!("Linux worker is stopped");
        }
        let args = vec![
            "exec".into(),
            "--interactive".into(),
            "--user".into(),
            self.0.user.clone(),
            self.0.name.clone(),
            "/opt/noctum/noctum-worker".into(),
            operation.into(),
        ];
        let output = process::run_input(
            Path::new("/"),
            "docker",
            &args,
            timeout + Duration::from_secs(10),
            serde_json::to_vec(request)?,
        )
        .await;
        if !output.success {
            bail!("Linux worker operation failed: {}", output.combined());
        }
        let reply: Reply<R> =
            serde_json::from_str(&output.stdout).context("read Linux worker response")?;
        if let Some(error) = reply.error {
            bail!("{error}");
        }
        reply.value.context("Linux worker returned no result")
    }
    pub async fn stop(&self) -> Result<()> {
        if self.0.stopped.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let _ = std::fs::remove_file(self.0.directory.path().join("heartbeat"));
        let output = process::run(
            Path::new("/"),
            "docker",
            &["rm", "--force", &self.0.name],
            &[],
            Duration::from_secs(15),
        )
        .await;
        if !output.success {
            bail!(
                "cannot stop Linux worker {}: {}",
                self.0.name,
                output.combined()
            );
        }
        Ok(())
    }
}
async fn id(option: &str) -> Result<u32> {
    let output = process::run(Path::new("/"), "id", &[option], &[], Duration::from_secs(5)).await;
    if !output.success {
        bail!("cannot read the host user id");
    }
    Ok(output.stdout.trim().parse()?)
}

fn bind_mount(path: &Path, readonly: bool) -> String {
    let value = path.display().to_string().replace('"', "\"\"");
    format!(
        "type=bind,\"source={value}\",\"target={value}\"{}",
        if readonly { ",readonly" } else { "" }
    )
}

fn platform() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "linux/arm64"
    } else {
        "linux/amd64"
    }
}
