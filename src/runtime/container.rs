use super::{assets::Assets, host::Host, protocol::Reply};
use crate::process;
use anyhow::{bail, Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ContainerSession(Arc<Session>);

#[derive(Debug)]
struct Session {
    host: Host,
    _lease: std::fs::File,
    assets: Assets,
    mounts: Vec<serde_json::Value>,
    directory: tempfile::TempDir,
    uid: u32,
    gid: u32,
    running: AtomicBool,
    stopped: AtomicBool,
    gate: tokio::sync::Mutex<()>,
}

#[derive(Deserialize)]
struct Output {
    code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

pub async fn engine_ready() -> Result<()> {
    let host = Host::start(None).await?;
    host.close().await
}

pub async fn image_ready(data: &Path) -> bool {
    Assets::load(data).is_ok()
}

pub async fn prepare_image(data: &Path) -> Result<()> {
    engine_ready().await?;
    super::assets::prepare(data).await
}

impl ContainerSession {
    pub async fn start(data: &Path, repositories: &[PathBuf]) -> Result<Self> {
        let lease = super::assets::lease(data)?;
        let assets = Assets::load(data)?;
        check_disk(data)?;
        std::fs::create_dir_all(data.join("runtime/sessions"))?;
        let directory = tempfile::tempdir_in(data.join("runtime/sessions"))?;
        let uid = id("-u").await?;
        let gid = id("-g").await?;
        let mut mounts = Vec::new();
        for (path, readonly) in [
            (data.join("worktrees"), false),
            (data.join("measurements"), false),
            (data.join("scratch"), false),
            (data.join("fence"), true),
        ] {
            std::fs::create_dir_all(&path)?;
            mounts.push(json!({"source":path,"destination":path,"readOnly":readonly}));
        }
        for path in repositories {
            mounts.push(json!({"source":path,"destination":path,"readOnly":true}));
        }
        let host = Host::start(Some(&data.join("runtime/helper.log"))).await?;
        let session = Self(Arc::new(Session {
            host,
            _lease: lease,
            assets,
            mounts,
            directory,
            uid,
            gid,
            running: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            gate: tokio::sync::Mutex::new(()),
        }));
        session.resume().await?;
        Ok(session)
    }

    async fn resume(&self) -> Result<()> {
        if self.0.stopped.load(Ordering::SeqCst) {
            bail!("worker is stopped");
        }
        check_disk(self.0.directory.path())?;
        let assets = &self.0.assets;
        self.0.host.call::<serde_json::Value>(json!({
            "operation":"start","store":assets.store,"kernel":assets.kernel,"image":assets.image,"rootfs":assets.rootfs,"initReference":assets.init,
            "network":true,"cpus":2,"memoryMb":2048,"mounts":self.0.mounts,
            "arguments":["/opt/noctum/noctum-worker","supervise"],
            "environment":[format!("NOCTUM_WORKER_UID={}",self.0.uid),format!("NOCTUM_WORKER_GID={}",self.0.gid),"CARGO_HOME=/cache/cargo","HOME=/cache/home","CARGO_BUILD_JOBS=2","CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld","CC_aarch64_unknown_linux_musl=musl-gcc"],"cwd":"/runtime"
        }),Duration::from_secs(120)).await?;
        self.0.running.store(true, Ordering::SeqCst);
        for _ in 0..120 {
            if self
                .rpc_inner::<_, bool>("ready", &(), Duration::from_secs(10))
                .await
                .unwrap_or(false)
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        self.stop().await?;
        bail!("worker startup timed out")
    }

    pub async fn rpc<Q: Serialize, R: DeserializeOwned>(
        &self,
        operation: &str,
        request: &Q,
        timeout: Duration,
    ) -> Result<R> {
        let _guard = self.0.gate.lock().await;
        if self.0.stopped.load(Ordering::SeqCst) {
            bail!("worker is stopped");
        }
        if matches!(operation, "exec" | "measure") {
            self.suspend_inner().await?;
        }
        if !self.0.running.load(Ordering::SeqCst) {
            self.resume().await?;
        }
        let result = process::watch_disk(
            self.rpc_inner(operation, request, timeout),
            Duration::from_secs(2),
            || check_disk(self.0.directory.path()),
        )
        .await;
        if result.is_err() {
            self.stop().await?;
        }
        result
    }

    async fn rpc_inner<Q: Serialize, R: DeserializeOwned>(
        &self,
        operation: &str,
        request: &Q,
        timeout: Duration,
    ) -> Result<R> {
        let output:Output=self.0.host.call(json!({"operation":"exec","arguments":["/opt/noctum/noctum-worker",operation],"input":serde_json::to_vec(request)?,"uid":self.0.uid,"gid":self.0.gid,"timeoutSeconds":timeout.as_secs()}),timeout+Duration::from_secs(15)).await?;
        if output.code != 0 {
            bail!(
                "worker operation failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let reply: Reply<R> =
            serde_json::from_slice(&output.stdout).context("read worker response")?;
        if let Some(error) = reply.error {
            bail!("{error}");
        }
        reply.value.context("worker returned no result")
    }

    async fn suspend_inner(&self) -> Result<()> {
        if self.0.running.load(Ordering::SeqCst) {
            self.0
                .host
                .call::<serde_json::Value>(json!({"operation":"stop"}), Duration::from_secs(30))
                .await?;
            self.0.running.store(false, Ordering::SeqCst);
        }
        Ok(())
    }
    pub async fn suspend(&self) -> Result<()> {
        let _guard = self.0.gate.lock().await;
        self.suspend_inner().await
    }
    pub async fn stop(&self) -> Result<()> {
        self.0.stopped.store(true, Ordering::SeqCst);
        self.0.running.store(false, Ordering::SeqCst);
        self.0.host.close().await
    }
}

fn check_disk(path: &Path) -> Result<()> {
    process::require_free_space(path, 2 * 1024 * 1024 * 1024)?;
    process::require_free_space(Path::new("/"), 2 * 1024 * 1024 * 1024)
}

async fn id(option: &str) -> Result<u32> {
    let result = process::run(Path::new("/"), "id", &[option], &[], Duration::from_secs(5)).await;
    if !result.success {
        bail!("cannot read the current user ID");
    }
    Ok(result.stdout.trim().parse()?)
}
