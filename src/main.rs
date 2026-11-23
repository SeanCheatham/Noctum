//! Overnight daemon. `start` is the default.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tokio::sync::RwLock;

use noctum::config::Config;
use noctum::daemon::Daemon;
use noctum::db::Database;
use noctum::lockfile::{self, AssetState};
use noctum::model;
use noctum::runtime;
use noctum::web::start_server;
use noctum::AppState;

mod devex;
mod run_cli;

#[derive(Parser)]
#[command(name = "noctum")]
#[command(version)]
#[command(about = "A local overnight daemon that keeps a Rust change when a test gets cheaper")]
struct Cli {
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Create repository configuration and register the repository
    Init {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(short, long)]
        package: Option<String>,
    },
    /// Check setup and show instructions to correct each failure
    Doctor {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
    /// Show the latest run, accepted changes, and Git commands
    Review {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
    Runtime {
        #[command(subcommand)]
        command: RuntimeCommand,
    },
    /// Daemon plus the localhost dashboard
    Start,
    /// One pass, ignoring the window
    Run {
        #[arg(long, conflicts_with = "all")]
        repo: Option<PathBuf>,
        /// Run all enabled repositories
        #[arg(long, conflicts_with = "test")]
        all: bool,
        /// Select one test by its full name or harness and test name
        #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
        test: Option<String>,
        /// Limit model requests, including repair requests
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
        max_attempts: Option<u32>,
        /// Try functions again when their source is unchanged
        #[arg(long)]
        retry: bool,
    },
    /// Measure working source without model requests
    Measure {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
        test: Option<String>,
    },
    Model {
        #[command(subcommand)]
        command: ModelCommand,
    },
    /// Daemon phase, lockfile state, and the latest run of each repository
    Status,
}

#[derive(Subcommand)]
enum RuntimeCommand {
    Prepare,
    Status,
}

#[derive(Subcommand)]
enum ModelCommand {
    Pull,
    Status,
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
compile_error!("The Noctum host requires macOS on Apple silicon.");

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config_path = cli.config.clone().or_else(Config::default_config_path);
    let config = Config::load(cli.config.as_deref())?;
    let log = if matches!(
        &cli.command,
        Some(Command::Run { .. } | Command::Measure { .. })
    ) {
        Some(run_cli::CommandLog::new(&config)?)
    } else {
        None
    };
    let trace_log = log.clone();
    tracing_subscriber::fmt()
        .with_env_filter(config.general.log_level.clone())
        .with_target(false)
        .with_ansi(false)
        .with_writer(move || run_cli::LogWriter(trace_log.clone()))
        .init();

    match cli.command.unwrap_or(Command::Start) {
        Command::Init { repo, package } => {
            devex::init(&config, &repo, package.as_deref(), cli.config.as_deref()).await
        }
        Command::Doctor { repo } => devex::doctor(&config, &repo, cli.config.as_deref()).await,
        Command::Review { repo } => devex::review(&config, &repo, cli.config.as_deref()).await,
        Command::Start => start(config, config_path).await,
        Command::Run {
            repo,
            all,
            test,
            max_attempts,
            retry,
        } => {
            let repo = if all {
                None
            } else {
                Some(repo.unwrap_or_else(|| PathBuf::from(".")))
            };
            run_cli::run(config, repo, test, max_attempts, false, retry, log.unwrap()).await
        }
        Command::Measure { repo, test } => {
            run_cli::run(config, Some(repo), test, None, true, false, log.unwrap()).await
        }
        Command::Model { command } => match command {
            ModelCommand::Pull => model::pull(&config.data_dir(), config.model.quant).await,
            ModelCommand::Status => model_status(&config).await,
        },
        Command::Runtime { command } => match command {
            RuntimeCommand::Prepare => {
                let data = config.data_dir();
                tokio::select! {
                    result = runtime::prepare_image(&data) => result,
                    signal = tokio::signal::ctrl_c() => { signal?; anyhow::bail!("runtime preparation was cancelled"); },
                }
            }
            RuntimeCommand::Status => {
                if !runtime::image_ready(&config.data_dir()).await {
                    anyhow::bail!("Worker is not ready. Run noctum runtime prepare.");
                }
                println!("Worker is ready");
                Ok(())
            }
        },
        Command::Status => status(&config).await,
    }
}

async fn start(config: Config, config_path: Option<PathBuf>) -> Result<()> {
    let db = open_db(&config).await?;
    let config = Arc::new(RwLock::new(config));
    let (daemon, handle) = Daemon::new(db.clone(), config.clone());
    let state = Arc::new(AppState {
        db,
        config: config.clone(),
        config_path,
        daemon: handle,
    });
    let web = {
        let config = config.read().await.clone();
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(err) = start_server(state, &config.web.host, config.web.port).await {
                tracing::error!("dashboard stopped: {err}");
            }
        })
    };
    let mut task = tokio::spawn(daemon.run());
    tokio::select! {
        result = &mut task => result?,
        signal = tokio::signal::ctrl_c() => {
            signal?;
            state.daemon.request_stop();
            task.await?;
        }
    }
    web.abort();
    Ok(())
}

async fn model_status(config: &Config) -> Result<()> {
    match lockfile::verify_model(&config.data_dir(), config.model.quant) {
        Ok(()) => println!("model ok"),
        Err(err) => {
            println!("model: {err}");
            std::process::exit(1);
        }
    }
    Ok(())
}

async fn status(config: &Config) -> Result<()> {
    let data = config.data_dir();
    println!(
        "model {}",
        stamp(lockfile::model_stamp(&data, config.model.quant))
    );
    println!("llama {}", stamp(lockfile::llama_stamp(&data)));
    println!(
        "worker {}",
        if runtime::image_ready(&data).await {
            "ready"
        } else {
            "missing"
        }
    );
    if !config.database_path().is_file() {
        println!("phase idle");
        return Ok(());
    }
    let db = open_db(config).await?;
    let daemon = db.get_daemon().await?;
    println!(
        "phase {} {}",
        daemon.phase,
        daemon.detail.unwrap_or_default()
    );
    for repo in db.get_repositories().await? {
        let latest = db.latest_run(repo.id).await?;
        let text = latest
            .map(|run| format!("{} {}", run.status, run.git_sha))
            .unwrap_or_else(|| "no runs".into());
        println!("{} {}", repo.name, text);
    }
    Ok(())
}

async fn open_db(config: &Config) -> Result<Database> {
    let db = Database::new(&config.database_path()).await?;
    db.run_migrations().await?;
    Ok(db)
}

fn stamp(state: AssetState) -> &'static str {
    match state {
        AssetState::Missing => "missing",
        AssetState::Present => "present",
        AssetState::Mismatch => "mismatch",
        AssetState::Unverified => "unverified",
    }
}
