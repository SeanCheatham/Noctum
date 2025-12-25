mod analyzer;
mod config;
mod daemon;
mod db;
mod mutation;
mod web;

use clap::{Parser, Subcommand};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

use crate::config::Config;
use crate::daemon::Daemon;
use crate::db::Database;
use crate::web::start_server;

#[derive(Parser)]
#[command(name = "noctum")]
#[command(version)]
#[command(about = "A local-first, AI-powered code analyzer")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Path to configuration file
    #[arg(short, long)]
    config: Option<std::path::PathBuf>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the daemon and web server
    Start,
}

/// Shared application state
pub struct AppState {
    pub db: Database,
    pub config: Arc<RwLock<Config>>,
    pub daemon: Arc<RwLock<Daemon>>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .with_target(false)
        .init();

    let cli = Cli::parse();

    // Load configuration
    let config_path = cli.config.clone().or_else(Config::default_config_path);
    let config = Config::load(cli.config.as_deref())?;

    tracing::info!(
        "Config path: {}",
        config_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(none, using defaults)".to_string())
    );
    tracing::info!("Data directory: {}", config.data_dir().display());

    match cli.command.unwrap_or(Commands::Start) {
        Commands::Start => {
            tracing::info!("Starting Noctum daemon...");

            // Initialize database
            let db = Database::new(&config.database_path()).await?;
            db.run_migrations().await?;
            tracing::info!("Database initialized");

            // Initialize daemon
            let config = Arc::new(RwLock::new(config));
            let daemon = Arc::new(RwLock::new(Daemon::new(
                config.read().await.clone(),
                db.clone(),
            )));

            // Create shared state
            let state = Arc::new(AppState {
                db,
                config: config.clone(),
                daemon: daemon.clone(),
            });

            // Start the daemon in a background task
            let daemon_handle = {
                let daemon = daemon.clone();
                tokio::spawn(async move {
                    let mut daemon = daemon.write().await;
                    daemon.run().await
                })
            };

            // Start the web server
            let web_port = config.read().await.web.port;
            let server_handle = tokio::spawn(start_server(state, web_port));

            tracing::info!(
                "Noctum is running. Dashboard available at http://localhost:{}",
                web_port
            );

            // Wait for either to complete (or fail)
            tokio::select! {
                result = daemon_handle => {
                    if let Err(e) = result {
                        tracing::error!("Daemon task failed: {}", e);
                    }
                }
                result = server_handle => {
                    if let Err(e) = result {
                        tracing::error!("Server task failed: {}", e);
                    }
                }
            }
        }
    }

    Ok(())
}
