//! Native control with Linux build and measurement operations on macOS.
mod assets;
mod container;
mod host;
pub mod protocol;
mod source;
use crate::ports::{CheckOutput, HostCheck, MeasureRequest, Measurement, Measurer};
use crate::process::{self, CmdOutput};
use anyhow::Result;
pub use container::{engine_ready, image_ready, prepare_image, ContainerSession};
use std::ffi::OsStr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Clone, Debug, Default)]
pub enum Execution {
    #[default]
    Unavailable,
    #[cfg(test)]
    Local,
    Container(ContainerSession),
}
impl Execution {
    pub async fn suspend_for_inference(&self) -> Result<()> {
        match self {
            Self::Unavailable => anyhow::bail!("worker is not connected"),
            #[cfg(test)]
            Self::Local => Ok(()),
            Self::Container(session) => session.suspend().await,
        }
    }

    pub async fn measurement_profile(&self, _data: &Path, memory_mb: u32) -> Result<String> {
        let (version, bundle): (String, String) = match self {
            Self::Unavailable => anyhow::bail!("worker is not connected"),
            #[cfg(test)]
            Self::Local => return Ok("test".into()),
            Self::Container(session) => {
                session.rpc("profile", &(), Duration::from_secs(10)).await?
            }
        };
        Ok(format!(
            "{version};cpu={};memory={memory_mb};bundle={bundle}",
            self.target().cpu()
        ))
    }

    pub fn target(&self) -> crate::target::GuestTarget {
        crate::target::GuestTarget::Aarch64
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
            Self::Unavailable => operation_error(anyhow::anyhow!("worker is not connected")),
            #[cfg(test)]
            Self::Local => process::watch_disk(
                async { Ok(process::run(cwd, program, args, env, timeout).await) },
                Duration::from_secs(2),
                || process::require_free_space(cwd, 2 * 1024 * 1024 * 1024),
            )
            .await
            .unwrap_or_else(operation_error),
            Self::Container(session) => {
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
                session
                    .rpc("exec", &request, timeout)
                    .await
                    .unwrap_or_else(operation_error)
            }
        }
    }
    pub async fn ensure_target(&self, cwd: &Path) -> Result<()> {
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
        Ok(())
    }
}

fn operation_error(error: anyhow::Error) -> CmdOutput {
    CmdOutput {
        success: false,
        timed_out: false,
        code: None,
        stdout: String::new(),
        stderr: format!("{error:#}"),
        duration_ms: 0,
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
    Container(ContainerSession),
}
impl Measurer for RuntimeMeasurer {
    async fn measure(&self, req: &MeasureRequest) -> Result<Measurement> {
        match self {
            Self::Container(session) => {
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
        Some(if opts.measurement_only {
            "Prepare measurement tools"
        } else {
            "Prepare the model and measurement worker"
        }),
    )
    .await?;
    let prepare = async {
        if !opts.measurement_only
            && crate::lockfile::verify_model(&data, config.model.quant).is_err()
        {
            crate::model::pull(&data, config.model.quant).await?;
        }
        prepare_image(&data).await
    };
    tokio::select! {
        result = prepare => result?,
        _ = wait_cancel(&opts.stop) => { db.set_daemon("idle", "idle", None).await?; return Ok(()); },
    }
    let session = {
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
        let future = ContainerSession::start(&data, &paths);
        tokio::select! {
            result = future => result?,
            _ = wait_cancel(&opts.stop) => { db.set_daemon("idle", "idle", None).await?; return Ok(()); },
        }
    };
    let execution = Execution::Container(session.clone());
    let measurer = RuntimeMeasurer::Container(session.clone());
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
            let _ = session.stop().await;
            future.await
        }
    };
    session.stop().await?;
    if opts.stop.load(Ordering::SeqCst) {
        db.set_daemon("idle", "idle", None).await?;
        return Ok(());
    }
    result
}

pub async fn measurement_state(data: &Path) -> crate::lockfile::AssetState {
    if image_ready(data).await {
        crate::lockfile::AssetState::Present
    } else {
        crate::lockfile::AssetState::Missing
    }
}
