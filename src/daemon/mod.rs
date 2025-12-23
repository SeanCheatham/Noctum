mod idle;

use crate::config::Config;
use idle::IdleMonitor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::time::{interval, Duration};
use tracing::{debug, info};

/// Daemon status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonStatus {
    /// Daemon is idle, waiting for user to become inactive
    Idle,
    /// Daemon is actively processing
    Processing,
    /// Daemon is paused (user became active)
    Paused,
    /// Daemon is stopping
    Stopping,
}

/// The background daemon that manages analysis tasks
pub struct Daemon {
    config: Config,
    status: DaemonStatus,
    idle_monitor: IdleMonitor,
    should_stop: Arc<AtomicBool>,
}

impl Daemon {
    /// Create a new daemon instance
    pub fn new(config: Config) -> Self {
        Self {
            idle_monitor: IdleMonitor::new(config.idle.threshold_seconds),
            config,
            status: DaemonStatus::Idle,
            should_stop: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Get current daemon status
    pub fn status(&self) -> DaemonStatus {
        self.status
    }

    /// Signal the daemon to stop
    pub fn stop(&self) {
        self.should_stop.store(true, Ordering::SeqCst);
    }

    /// Run the daemon loop
    pub async fn run(&mut self) -> anyhow::Result<()> {
        info!("Daemon started");

        let check_interval = Duration::from_secs(self.config.idle.check_interval_seconds);
        let mut ticker = interval(check_interval);

        while !self.should_stop.load(Ordering::SeqCst) {
            ticker.tick().await;

            let is_idle = self.idle_monitor.is_idle();
            debug!("Idle check: {}", is_idle);

            match (self.status, is_idle) {
                (DaemonStatus::Idle, true) => {
                    info!("User is idle, starting background processing");
                    self.status = DaemonStatus::Processing;
                    self.process_tasks().await?;
                }
                (DaemonStatus::Processing, false) => {
                    info!("User became active, pausing background processing");
                    self.status = DaemonStatus::Paused;
                }
                (DaemonStatus::Paused, true) => {
                    info!("User is idle again, resuming background processing");
                    self.status = DaemonStatus::Processing;
                }
                (DaemonStatus::Paused, false) => {
                    // Still waiting for user to be idle
                    debug!("Waiting for user to become idle");
                }
                (DaemonStatus::Processing, true) => {
                    // Continue processing
                    self.process_tasks().await?;
                }
                (DaemonStatus::Idle, false) => {
                    // Normal state, user is active
                    debug!("User is active, daemon idle");
                }
                (DaemonStatus::Stopping, _) => {
                    break;
                }
            }
        }

        self.status = DaemonStatus::Stopping;
        info!("Daemon stopped");
        Ok(())
    }

    /// Process background analysis tasks
    async fn process_tasks(&mut self) -> anyhow::Result<()> {
        // TODO: Implement actual task processing
        // This will:
        // 1. Check for repositories that need analysis
        // 2. Run analysis on pending files
        // 3. Execute mutation testing
        // 4. Store results in database

        debug!("Processing tasks (placeholder)");

        // For now, just yield to avoid busy-looping
        tokio::time::sleep(Duration::from_secs(1)).await;

        Ok(())
    }
}
