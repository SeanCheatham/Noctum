//! Manual runs and measurement reports.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use noctum::config::Config;
use noctum::db::Database;
use noctum::orchestrator::RunOptions;
use noctum::{repo_config::RepoConfig, runtime};

#[derive(Clone)]
pub struct CommandLog {
    file: Arc<Mutex<File>>,
    path: PathBuf,
    started: Instant,
}

impl CommandLog {
    pub fn new(config: &Config) -> Result<Self> {
        let directory = config.data_dir().join("logs");
        fs::create_dir_all(&directory)?;
        let name = format!(
            "run-{}-{}.log",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
            std::process::id()
        );
        let path = directory.join(name);
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&path)?;
        Ok(Self {
            file: Arc::new(Mutex::new(file)),
            path: path.canonicalize()?,
            started: Instant::now(),
        })
    }

    fn event(&self, message: &str) -> Result<()> {
        let line = format!("[{}s] {message}", self.started.elapsed().as_secs());
        eprintln!("{line}");
        writeln!(
            self.file
                .lock()
                .map_err(|_| anyhow::anyhow!("cannot open command log"))?,
            "{line}"
        )?;
        Ok(())
    }

    fn save(&self, message: &str) -> Result<()> {
        writeln!(
            self.file
                .lock()
                .map_err(|_| anyhow::anyhow!("cannot open command log"))?,
            "{message}"
        )?;
        Ok(())
    }
}

pub struct LogWriter(pub Option<CommandLog>);

impl Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        io::stderr().write_all(bytes)?;
        if let Some(log) = &self.0 {
            log.file
                .lock()
                .map_err(|_| io::Error::other("cannot open command log"))?
                .write_all(bytes)?;
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stderr().flush()
    }
}

pub async fn run(
    config: Config,
    repo: Option<PathBuf>,
    test: Option<String>,
    max_attempts: Option<u32>,
    measurement_only: bool,
    retry: bool,
    log: CommandLog,
) -> Result<()> {
    eprintln!("Log: {}", log.path.display());
    let result = run_inner(
        config,
        repo,
        test,
        max_attempts,
        measurement_only,
        retry,
        &log,
    )
    .await;
    match &result {
        Ok(()) => log.event("Command completed")?,
        Err(err) => log.event(&format!("Command failed: {err:#}"))?,
    }
    result
}

async fn run_inner(
    config: Config,
    repo: Option<PathBuf>,
    test: Option<String>,
    max_attempts: Option<u32>,
    measurement_only: bool,
    retry: bool,
    log: &CommandLog,
) -> Result<()> {
    let root = match repo {
        Some(path) => Some(super::devex::repository_root(&path).await?),
        None => None,
    };
    if let Some(root) = &root {
        RepoConfig::load(root)?;
    }
    let report_dir = if measurement_only {
        let root = root.as_ref().context("measurement needs a repository")?;
        let data = config.data_dir().canonicalize()?;
        if data.starts_with(root) {
            bail!("measurement data directory must be outside the source repository")
        }
        let directory = data
            .join("measurements")
            .join(log.path.file_stem().context("log name is missing")?);
        fs::create_dir_all(&directory)?;
        Some(directory)
    } else {
        None
    };
    let db = if let Some(directory) = &report_dir {
        let db = Database::new(&directory.join("results.db")).await?;
        db.run_migrations().await?;
        let root = root.as_ref().unwrap();
        db.add_repository(
            root.to_str().context("repository path must be UTF-8")?,
            "measurement",
        )
        .await?;
        db
    } else {
        super::open_db(&config).await?
    };
    let mut selected = Vec::new();
    for repo in db.enabled_repositories().await? {
        if root
            .as_ref()
            .is_some_and(|p| !noctum::worktree::same_path(p, Path::new(&repo.path)))
        {
            continue;
        }
        let previous = db.latest_run(repo.id).await?.map(|r| r.id);
        selected.push((repo, previous));
    }
    if selected.is_empty() {
        bail!("no enabled repository was selected; run noctum init with the same application configuration")
    }
    let stop = Arc::new(AtomicBool::new(false));
    let mut opts = RunOptions::manual(stop.clone());
    opts.only_repo = root;
    opts.test = test;
    opts.max_attempts = max_attempts;
    opts.measurement_only = measurement_only;
    opts.retry = retry;
    if retry {
        log.event("Retry functions with unchanged source")?;
    }
    log.event(if measurement_only {
        "Prepare measurement of working source"
    } else {
        "Prepare run of committed source"
    })?;
    let future = runtime::run_pass(db.clone(), config, opts);
    tokio::pin!(future);
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    let mut last = None;
    let mut last_update = Instant::now();
    let mut cancelled = false;
    let result = loop {
        tokio::select! {
            result = &mut future => break result,
            signal = tokio::signal::ctrl_c(), if !cancelled => {
                signal?;
                cancelled = true;
                stop.store(true, Ordering::SeqCst);
                log.event("Stop requested. Wait for cleanup.")?;
            }
            _ = interval.tick() => {
                let state = db.get_daemon().await?;
                let phase = (state.phase, state.detail.unwrap_or_default());
                if state.status == "running" && (last.as_ref() != Some(&phase) || last_update.elapsed() >= Duration::from_secs(30)) {
                    log.event(&format!("{}: {}", phase.0, phase.1))?;
                    last = Some(phase);
                    last_update = Instant::now();
                }
            }
        }
    };
    let mut report = String::new();
    let mut failed = false;
    for (repo, previous) in selected {
        let Some(run) = db.latest_run(repo.id).await? else {
            continue;
        };
        if Some(run.id) == previous {
            continue;
        }
        report.push_str(&format!(
            "Repository: {}\nRun: {} ({})\nTarget: {}\nSource: {}\nSource commit: {}\n",
            repo.path,
            run.id,
            run.status,
            run.target,
            if measurement_only {
                "working files, including non-ignored new files"
            } else {
                "committed source"
            },
            run.git_sha
        ));
        if let Some(error) = &run.error {
            report.push_str(&format!("Run error: {error}\n"));
        }
        failed |= run.status != "ok";
        let baselines = db.baselines_for_run(run.id).await?;
        let valid = baselines
            .iter()
            .any(|b| matches!(b.status.as_str(), "measured" | "skipped_small"));
        if !valid {
            report.push_str("No tests produced stable instruction counts.\n");
        } else if !baselines.iter().any(|b| b.status == "measured") {
            report.push_str("No test meets the optimization floor. Use a fixed workload with runtime input, then measure it again.\n");
        }
        failed |= !valid;
        for baseline in baselines {
            report.push_str(&format!(
                "{} :: {}: {}",
                baseline.harness, baseline.test_name, baseline.status
            ));
            if let Some(count) = baseline.insn_count {
                report.push_str(&format!(", {count} instructions"));
            }
            if baseline.memory_json.is_some() {
                report.push_str(&format!(
                    ", {}",
                    noctum::optimization::memory_text(baseline.memory_json.as_deref())
                ));
            }
            if let Some(reason) = baseline.reason {
                report.push_str(&format!(" ({reason})"));
            }
            report.push('\n');
        }
        let trials = db.trials_for_run(run.id).await?;
        report.push_str(&format!("Counter profile: {}\n", run.measurement_profile));
        report.push_str(&format!(
            "Accepted changes: {}\n",
            trials.iter().filter(|t| t.status == "accepted").count()
        ));
        report.push_str(&format!("Model trials: {}\n", trials.len()));
        for trial in trials {
            failed |= trial.status == "error";
            report.push_str(&format!(
                "Trial {} {}: {}{}\n",
                trial.id,
                trial.symbol,
                trial.status,
                trial
                    .reason
                    .as_ref()
                    .map(|reason| format!(": {reason}"))
                    .unwrap_or_default()
            ));
            if trial.memory_before_json.is_some() || trial.memory_after_json.is_some() {
                report.push_str(&format!(
                    "  Memory: {} -> {}\n",
                    noctum::optimization::memory_text(trial.memory_before_json.as_deref()),
                    noctum::optimization::memory_text(trial.memory_after_json.as_deref())
                ));
            }
            if let Some(analysis) = trial.analysis_json {
                log.save(&format!("Trial {} source analysis:\n{analysis}", trial.id))?;
            }
            if let Some(output) = trial.tool_output {
                log.save(&format!("Trial {} tool output:\n{output}", trial.id))?;
            }
        }
    }
    log.save(&report)?;
    print!("{report}");
    if let Some(directory) = report_dir {
        fs::write(directory.join("report.txt"), &report)?;
        eprintln!("Results: {}", directory.join("report.txt").display());
    }
    result?;
    if cancelled {
        bail!("command was cancelled")
    }
    if failed {
        bail!("run checks failed; see the report and command log")
    }
    Ok(())
}
