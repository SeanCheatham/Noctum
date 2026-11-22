//! Overnight daemon. `start` is the default.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tokio::sync::RwLock;

use noctum::builder::CargoHost;
use noctum::config::Config;
use noctum::daemon::Daemon;
use noctum::db::Database;
use noctum::lockfile::{self, AssetState};
use noctum::model::{self, LlamaManager};
use noctum::orchestrator::{self, Night, RunOptions};
use noctum::qemu::{self, QemuRunner};
use noctum::web::start_server;
use noctum::AppState;

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
    /// Daemon plus the localhost dashboard
    Start,
    /// One pass, ignoring the window
    Run {
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    Model {
        #[command(subcommand)]
        command: ModelCommand,
    },
    Qemu {
        #[command(subcommand)]
        command: QemuCommand,
    },
    /// Daemon phase, lockfile state, and the latest run of each repository
    Status,
}

#[derive(Subcommand)]
enum ModelCommand {
    Pull,
    Status,
}

#[derive(Subcommand)]
enum QemuCommand {
    /// Download the bundle named by qemu.lock.toml
    Fetch,
    /// Install a tarball built by qemu/build-bundle.sh after checking its sha256
    Install {
        /// Path to noctum-qemu-*.tar.gz
        archive: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config_path = cli.config.clone().or_else(Config::default_config_path);
    let config = Config::load(cli.config.as_deref())?;
    tracing_subscriber::fmt()
        .with_env_filter(config.general.log_level.clone())
        .with_target(false)
        .init();

    match cli.command.unwrap_or(Command::Start) {
        Command::Start => start(config, config_path).await,
        Command::Run { repo } => run_once(config, repo).await,
        Command::Model { command } => match command {
            ModelCommand::Pull => model::pull(&config.data_dir(), config.model.quant).await,
            ModelCommand::Status => model_status(&config).await,
        },
        Command::Qemu { command } => match command {
            QemuCommand::Fetch => qemu::fetch(&config.data_dir()).await,
            QemuCommand::Install { archive } => {
                qemu::install(&config.data_dir(), &archive)?;
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
    tokio::select! {
        _ = daemon.run() => {}
        _ = tokio::signal::ctrl_c() => {
            state.daemon.request_stop();
        }
    }
    web.abort();
    Ok(())
}

async fn run_once(config: Config, repo: Option<PathBuf>) -> Result<()> {
    let db = open_db(&config).await?;
    let data = config.data_dir();
    let asset_error = orchestrator::prepare_assets(&data, config.model.quant)
        .await
        .err()
        .map(|err| err.to_string());
    let night = Night::new(
        db,
        config.clone(),
        QemuRunner::new(data.clone(), config.guest.memory_mb),
        LlamaManager::new(data, config.model.quant),
        CargoHost,
    );
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    night
        .run_pass(&RunOptions {
            ignore_window: true,
            stop,
            asset_error,
            only_repo: repo,
        })
        .await
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
    println!("qemu {}", stamp(lockfile::qemu_stamp(&data)));
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
