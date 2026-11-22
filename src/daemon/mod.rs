//! Window, manual trigger, and stop. One pass per repository per window.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use chrono::Local;
use tokio::sync::{mpsc, RwLock};

use crate::config::Config;
use crate::db::Database;
use crate::orchestrator::RunOptions;

#[derive(Clone)]
pub struct DaemonHandle {
    stop: Arc<AtomicBool>,
    trigger: mpsc::Sender<Option<PathBuf>>,
}

impl DaemonHandle {
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    pub fn trigger(&self, repo: Option<PathBuf>) -> Result<()> {
        self.trigger
            .try_send(repo)
            .map_err(|_| anyhow::anyhow!("a run is already queued"))
    }
}

pub struct Daemon {
    db: Database,
    config: Arc<RwLock<Config>>,
    stop: Arc<AtomicBool>,
    trigger: mpsc::Receiver<Option<PathBuf>>,
}

impl Daemon {
    pub fn new(db: Database, config: Arc<RwLock<Config>>) -> (Self, DaemonHandle) {
        let (tx, rx) = mpsc::channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let handle = DaemonHandle {
            stop: stop.clone(),
            trigger: tx,
        };
        (
            Self {
                db,
                config,
                stop,
                trigger: rx,
            },
            handle,
        )
    }

    pub async fn run(mut self) {
        let mut ran_window = None;
        loop {
            if self.stop.load(Ordering::SeqCst) {
                break;
            }
            let config = self.config.read().await.clone();
            if config.schedule.is_in_window() {
                let key = config.schedule.window_key_at(Local::now());
                if ran_window.as_deref() != Some(key.as_str()) {
                    self.pass(false, None).await;
                    ran_window = Some(key);
                }
            }
            let interval = Duration::from_secs(config.schedule.check_interval_seconds.max(1));
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                Some(repo) = self.trigger.recv() => {
                    self.pass(true, repo).await;
                }
                _ = wait_stop(&self.stop) => break,
            }
        }
        let _ = self.db.set_daemon("idle", "idle", None).await;
    }

    async fn pass(&self, ignore_window: bool, only_repo: Option<PathBuf>) {
        let config = self.config.read().await.clone();
        let opts = RunOptions {
            ignore_window,
            stop: self.stop.clone(),
            asset_error: None,
            only_repo,
        };
        if let Err(err) = crate::runtime::run_pass(self.db.clone(), config, opts).await {
            tracing::error!("night pass failed: {err}");
        }
    }
}

async fn wait_stop(stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
