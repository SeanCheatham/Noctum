mod analyzer;
mod config;
mod daemon;
mod db;
mod web;

use clap::{Parser, Subcommand};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

use crate::config::Config;
use crate::daemon::Daemon;
use crate::db::Database;
use crate::web::start_server;

#[derive(Parser)]
#[command(name = "noctum")]
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
    /// Stop a running daemon
    Stop,
    /// Check daemon status
    Status,
}

/// Shared application state
pub struct AppState {
    pub db: Database,
    pub config: Config,
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
    let config = Config::load(cli.config.as_deref())?;
    info!("Configuration loaded");

    match cli.command.unwrap_or(Commands::Start) {
        Commands::Start => {
            info!("Starting Noctum daemon...");

            // Initialize database
            let db = Database::new(&config.database_path()).await?;
            db.run_migrations().await?;
            info!("Database initialized");

            // Initialize daemon
            let daemon = Arc::new(RwLock::new(Daemon::new(config.clone())));

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
            let server_handle = tokio::spawn(start_server(state, config.web.port));

            info!("Noctum is running. Dashboard available at http://localhost:{}", config.web.port);

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
        Commands::Stop => {
            info!("Stopping Noctum daemon...");
            // TODO: Implement stop logic (send signal to running daemon)
            println!("Stop command not yet implemented");
        }
        Commands::Status => {
            // TODO: Implement status check
            println!("Status command not yet implemented");
        }
    }

    Ok(())
}
