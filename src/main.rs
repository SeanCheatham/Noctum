mod analyzer;
mod config;
mod daemon;
mod db;
mod diagram;
mod language;
mod mutation;
mod project;
mod web;

use clap::{Parser, Subcommand};
use std::sync::Arc;
use tokio::signal;
use tokio::sync::RwLock;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

use crate::config::Config;
use crate::daemon::{Daemon, DaemonHandle};
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

#[derive(Subcommand, Debug, PartialEq)]
enum Commands {
    /// Start the daemon and web server
    Start,
}

/// Shared application state
pub struct AppState {
    pub db: Database,
    pub config: Arc<RwLock<Config>>,
    pub daemon: DaemonHandle,
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

            // Initialize daemon with shared config
            let config = Arc::new(RwLock::new(config));
            let mut daemon = Daemon::new(config.clone(), db.clone());
            let daemon_handle = daemon.handle();

            // Create shared state
            let state = Arc::new(AppState {
                db,
                config: config.clone(),
                daemon: daemon_handle.clone(),
            });

            // Start the daemon in a background task
            let mut daemon_task = tokio::spawn(async move { daemon.run().await });

            // Start the web server
            let web_host = config.read().await.web.host.clone();
            let web_port = config.read().await.web.port;
            let mut server_handle =
                tokio::spawn(async move { start_server(state, &web_host, web_port).await });

            tracing::info!(
                "Noctum is running. Dashboard available at http://localhost:{}",
                web_port
            );
            tracing::info!("Press Ctrl+C to stop");

            // Wait for shutdown signal or task failure
            tokio::select! {
                _ = shutdown_signal() => {
                    tracing::info!("Shutdown signal received");
                }
                result = &mut daemon_task => {
                    match result {
                        Ok(Ok(())) => tracing::info!("Daemon stopped unexpectedly"),
                        Ok(Err(e)) => tracing::error!("Daemon error: {}", e),
                        Err(e) => tracing::error!("Daemon task panicked: {}", e),
                    }
                }
                result = &mut server_handle => {
                    match result {
                        Ok(Ok(())) => tracing::info!("Server stopped unexpectedly"),
                        Ok(Err(e)) => tracing::error!("Server error: {}", e),
                        Err(e) => tracing::error!("Server task panicked: {}", e),
                    }
                }
            }

            // Signal daemon to stop
            tracing::info!("Stopping daemon...");
            daemon_handle.stop();

            // Give the daemon a moment to finish current work, then exit
            // The web server will be terminated when we exit
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {
                    tracing::debug!("Shutdown timeout reached");
                }
                _ = &mut daemon_task => {
                    tracing::debug!("Daemon task completed");
                }
            }

            tracing::info!("Noctum stopped");
        }
    }

    Ok(())
}

/// Wait for shutdown signal (Ctrl+C or SIGTERM)
async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("Failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn test_cli_parse_default() {
        let cli = Cli::try_parse_from(["noctum"]).unwrap();
        assert!(cli.command.is_none());
        assert!(cli.config.is_none());
    }

    #[test]
    fn test_cli_parse_start() {
        let cli = Cli::try_parse_from(["noctum", "start"]).unwrap();
        assert_eq!(cli.command, Some(Commands::Start));
    }

    #[test]
    fn test_cli_parse_config_flag() {
        let cli = Cli::try_parse_from(["noctum", "--config", "/path/to/config.toml"]).unwrap();
        assert_eq!(
            cli.config,
            Some(std::path::PathBuf::from("/path/to/config.toml"))
        );
    }

    #[test]
    fn test_cli_parse_config_short_flag() {
        let cli = Cli::try_parse_from(["noctum", "-c", "/path/to/config.toml"]).unwrap();
        assert_eq!(
            cli.config,
            Some(std::path::PathBuf::from("/path/to/config.toml"))
        );
    }

    #[test]
    fn test_cli_parse_start_with_config() {
        let cli =
            Cli::try_parse_from(["noctum", "--config", "/path/config.toml", "start"]).unwrap();
        assert_eq!(cli.command, Some(Commands::Start));
        assert_eq!(
            cli.config,
            Some(std::path::PathBuf::from("/path/config.toml"))
        );
    }

    #[test]
    fn test_cli_validate() {
        let cmd = Cli::command();
        cmd.debug_assert();
    }

    #[test]
    fn test_cli_long_about() {
        let cmd = Cli::command();
        assert!(cmd.get_about().is_some());
    }

    #[test]
    fn test_cli_version() {
        let cmd = Cli::command();
        assert!(cmd.get_version().is_some());
    }
}
