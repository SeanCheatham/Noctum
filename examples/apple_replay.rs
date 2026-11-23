//! Replay a saved model response through the real test and count checks.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use noctum::config::Config;
use noctum::db::Database;
use noctum::orchestrator::{Night, RunOptions};
use noctum::ports::{ChatMessage, Completion, Inferencer};
use noctum::runtime::{ContainerSession, Execution, RuntimeHost, RuntimeMeasurer};

struct Saved {
    completion: Completion,
    calls: Arc<AtomicUsize>,
}

impl Inferencer for Saved {
    async fn complete(&self, _: &[ChatMessage], _: &AtomicBool) -> Result<Completion> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.completion.clone())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args: Vec<_> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let check_retry = args.last().is_some_and(|arg| arg == "--check-retry");
    if check_retry {
        args.pop();
    }
    if args.len() != 3 {
        bail!("pass REPOSITORY DATA_DIRECTORY RESPONSE_JSON [--check-retry]");
    }
    let repo = args[0].canonicalize()?;
    std::fs::create_dir_all(&args[1])?;
    let data = args[1].canonicalize()?;
    let config = Config {
        data_dir: Some(data.clone()),
        ..Config::default()
    };
    noctum::runtime::prepare_image(&data).await?;
    let history = tempfile::tempdir_in(data.join("runtime"))?;
    let db = Database::new(&history.path().join("replay.db")).await?;
    db.run_migrations().await?;
    let id = db
        .add_repository(
            repo.to_str().context("repository path must be UTF-8")?,
            "saved response",
        )
        .await?;
    let row = db
        .get_repository(id)
        .await?
        .context("repository is missing")?;
    let completion = noctum::model::parse_completion(&std::fs::read_to_string(&args[2])?)?;
    let calls = Arc::new(AtomicUsize::new(0));
    let session = ContainerSession::start(&data, &[repo.clone(), repo.join(".git")]).await?;
    let execution = Execution::Container(session.clone());
    let make_night = || {
        Night::new(
            db.clone(),
            config.clone(),
            RuntimeMeasurer::Container(session.clone()),
            Saved {
                completion: completion.clone(),
                calls: calls.clone(),
            },
            RuntimeHost(execution.clone()),
        )
        .with_execution(execution.clone())
    };
    let mut opts = RunOptions::manual(Arc::new(AtomicBool::new(false)));
    opts.test = Some("decode::tests::decode_engine_estimation_works_for_various_lengths".into());
    opts.max_attempts = Some(1);
    eprintln!("Replay the saved response. No new model request will run.");
    let result: Result<()> = async {
        make_night().run_repository(&row, &opts).await?;
        if check_retry {
            let run = db.latest_run(id).await?.context("run is missing")?;
            let first = db.trials_for_run(run.id).await?;
            if first.len() != 1 || first[0].reason.as_deref() != Some("slower") {
                bail!("the retry check needs one rejected slower response");
            }
            make_night().run_repository(&row, &opts).await?;
            let run = db.latest_run(id).await?.context("run is missing")?;
            if calls.load(Ordering::SeqCst) != 1 || !db.trials_for_run(run.id).await?.is_empty() {
                bail!("an unchanged function was tried without the retry option");
            }
            opts.retry = true;
            make_night().run_repository(&row, &opts).await?;
            let run = db.latest_run(id).await?.context("run is missing")?;
            let repeated = db.trials_for_run(run.id).await?;
            if calls.load(Ordering::SeqCst) != 2
                || repeated.len() != 1
                || repeated[0].reason.as_deref() != Some("slower")
                || !repeated[0].host_check_passed
                || repeated[0].insn_count.is_none()
            {
                bail!("the retry did not repeat the test and count checks");
            }
        }
        Ok(())
    }
    .await;
    let stop = session.stop().await;
    let cleanup =
        noctum::worktree::remove(&repo, &data.join("worktrees").join(id.to_string())).await;
    result?;
    stop?;
    cleanup?;
    let run = db.latest_run(id).await?.context("run is missing")?;
    let trials = db.trials_for_run(run.id).await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "mode": "saved model response",
            "responses_used": calls.load(Ordering::SeqCst),
            "retry_checked": check_retry,
            "run": run,
            "tests": db.baselines_for_run(run.id).await?,
            "trials": trials,
        }))?
    );
    if calls.load(Ordering::SeqCst) != if check_retry { 2 } else { 1 }
        || !trials
            .iter()
            .any(|trial| trial.host_check_passed && trial.insn_count.is_some())
    {
        bail!("the saved response did not pass the test suite and reach measurement");
    }
    Ok(())
}
