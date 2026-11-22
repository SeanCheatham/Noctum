//! Native control with Linux build and measurement operations on macOS.
mod docker;
pub mod protocol;
mod source;
use crate::ports::{CheckOutput, HostCheck, MeasureRequest, Measurement, Measurer};
use crate::process::{self, CmdOutput};
use anyhow::Result;
pub use docker::{image_ready, image_tag, prepare_image, DockerSession};
use std::ffi::OsStr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Clone, Debug, Default)]
pub enum Execution {
    #[default]
    Native,
    Docker(DockerSession),
}
impl Execution {
    pub fn target(&self) -> crate::target::GuestTarget {
        match self {
            Self::Native => crate::target::GuestTarget::X86_64,
            Self::Docker(_) => crate::target::GuestTarget::native(),
        }
    }

    pub async fn run(
        &self,
        cwd: &Path,
        program: impl AsRef<OsStr>,
        args: &[impl AsRef<OsStr>],
        env: &[(&str, &str)],
        timeout: Duration,
    ) -> CmdOutput {
        match self {
            Self::Native => process::run(cwd, program, args, env, timeout).await,
            Self::Docker(session) => {
                let request = protocol::ExecRequest {
                    cwd: cwd.into(),
                    program: program.as_ref().to_string_lossy().into_owned(),
                    args: args
                        .iter()
                        .map(|a| a.as_ref().to_string_lossy().into_owned())
                        .collect(),
                    env: env
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .collect(),
                    timeout_secs: timeout.as_secs(),
                };
                match session.rpc("exec", &request, timeout).await {
                    Ok(output) => output,
                    Err(error) => CmdOutput {
                        success: false,
                        timed_out: false,
                        code: None,
                        stdout: String::new(),
                        stderr: format!("{error:#}"),
                        duration_ms: 0,
                    },
                }
            }
        }
    }
    pub async fn ensure_target(&self, cwd: &Path) -> Result<bool> {
        if matches!(self, Self::Native) {
            return crate::builder::musl_target_installed();
        }
        let args = ["target", "add", self.target().triple()];
        let result = self
            .run(cwd, "rustup", &args, &[], Duration::from_secs(300))
            .await;
        if !result.success {
            anyhow::bail!(
                "cannot prepare the Linux test target: {}",
                result.combined()
            );
        }
        Ok(true)
    }
}

pub struct RuntimeHost(pub Execution);
impl HostCheck for RuntimeHost {
    async fn run(
        &self,
        worktree: &Path,
        repo: &crate::repo_config::RepoConfig,
        package: &str,
    ) -> Result<CheckOutput> {
        crate::builder::host_check_with(&self.0, worktree, repo, package).await
    }
}
pub enum RuntimeMeasurer {
    Native(crate::qemu::QemuRunner),
    Docker(DockerSession),
}
impl Measurer for RuntimeMeasurer {
    async fn measure(&self, req: &MeasureRequest) -> Result<Measurement> {
        match self {
            Self::Native(runner) => runner.measure(req).await,
            Self::Docker(session) => {
                session
                    .rpc("measure", req, Duration::from_secs(req.timeout_secs + 150))
                    .await
            }
        }
    }
}

pub async fn wait_cancel(cancel: &AtomicBool) {
    while !cancel.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub async fn run_pass(
    db: crate::db::Database,
    mut config: crate::config::Config,
    opts: crate::orchestrator::RunOptions,
) -> Result<()> {
    let data = config.data_dir().join("runtime");
    std::fs::create_dir_all(&data)?;
    let lease = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(data.join("pass.lock"))?;
    lease
        .try_lock()
        .map_err(|_| anyhow::anyhow!("an overnight pass is already running"))?;
    let enabled = db.enabled_repositories().await?;
    let selected = enabled.iter().any(|repo| {
        opts.only_repo
            .as_ref()
            .is_none_or(|path| crate::worktree::same_path(path, Path::new(&repo.path)))
    });
    if !selected {
        if opts.only_repo.is_some() {
            anyhow::bail!("the repository is not enabled in Noctum");
        }
        db.set_daemon("idle", "idle", None).await?;
        return Ok(());
    }
    let result = run_inner(db.clone(), &mut config, &opts).await;
    if let Err(error) = &result {
        db.set_daemon("error", "idle", Some(&format!("{error:#}")))
            .await?;
    }
    result
}

async fn run_inner(
    db: crate::db::Database,
    config: &mut crate::config::Config,
    opts: &crate::orchestrator::RunOptions,
) -> Result<()> {
    let data = config.data_dir();
    std::fs::create_dir_all(&data)?;
    let data = data.canonicalize()?;
    config.data_dir = Some(data.clone());
    db.set_daemon(
        "running",
        "preparing",
        Some("Prepare the model and measurement worker"),
    )
    .await?;
    let prepare = async {
        if cfg!(target_os = "macos") {
            if crate::lockfile::verify_model(&data, config.model.quant).is_err() {
                crate::model::pull(&data, config.model.quant).await?;
            }
            prepare_image(&data).await
        } else {
            crate::orchestrator::prepare_assets(&data, config.model.quant).await
        }
    };
    tokio::select! {
        result = prepare => result?,
        _ = wait_cancel(&opts.stop) => { db.set_daemon("idle", "idle", None).await?; return Ok(()); },
    }
    let session = if cfg!(target_os = "macos") {
        let mut paths = Vec::new();
        for repo in db.enabled_repositories().await? {
            if opts
                .only_repo
                .as_ref()
                .is_some_and(|only| !crate::worktree::same_path(only, Path::new(&repo.path)))
            {
                continue;
            }
            let Ok(path) = Path::new(&repo.path).canonicalize() else {
                continue;
            };
            let common = process::run(
                &path,
                "git",
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
                &[],
                Duration::from_secs(10),
            )
            .await;
            if common.success {
                if let Ok(p) = Path::new(common.stdout.trim()).canonicalize() {
                    paths.push(p);
                }
            }
            paths.push(path);
        }
        let future = DockerSession::start(&data, &paths);
        Some(tokio::select! {
            result = future => result?,
            _ = wait_cancel(&opts.stop) => { db.set_daemon("idle", "idle", None).await?; return Ok(()); },
        })
    } else {
        None
    };
    let execution = session.clone().map(Execution::Docker).unwrap_or_default();
    let measurer = match &session {
        Some(s) => RuntimeMeasurer::Docker(s.clone()),
        None => {
            RuntimeMeasurer::Native(crate::qemu::QemuRunner::new(&data, config.guest.memory_mb))
        }
    };
    let night = crate::orchestrator::Night::new(
        db.clone(),
        config.clone(),
        measurer,
        crate::model::LlamaManager::new(data, config.model.quant),
        RuntimeHost(execution.clone()),
    )
    .with_execution(execution);
    let future = night.run_pass(opts);
    tokio::pin!(future);
    let result = tokio::select! {
        result = &mut future => result,
        _ = wait_cancel(&opts.stop) => {
            if let Some(session) = &session { let _ = session.stop().await; }
            future.await
        }
    };
    if let Some(session) = &session {
        session.stop().await?;
    }
    if opts.stop.load(Ordering::SeqCst) {
        db.set_daemon("idle", "idle", None).await?;
        return Ok(());
    }
    result
}

pub async fn measurement_state(data: &Path) -> crate::lockfile::AssetState {
    if cfg!(target_os = "macos") {
        if image_ready().await {
            crate::lockfile::AssetState::Present
        } else {
            crate::lockfile::AssetState::Missing
        }
    } else {
        crate::lockfile::qemu_stamp(data)
    }
}
