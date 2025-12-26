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

#[derive(Subcommand, Debug, PartialEq)]
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
            let web_host = config.read().await.web.host.clone();
            let web_port = config.read().await.web.port;
            let server_handle = tokio::spawn(async move {
                start_server(state, &web_host, web_port).await
            });

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
        let cli = Cli::try_parse_from([
            "noctum",
            "--config",
            "/path/config.toml",
            "start",
        ])
        .unwrap();
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
