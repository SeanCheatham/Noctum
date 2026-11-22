//! Window, manual trigger, and stop. One pass per repository per window.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use chrono::Local;
use tokio::sync::{mpsc, RwLock};

use crate::builder::CargoHost;
use crate::config::Config;
use crate::db::Database;
use crate::model::LlamaManager;
use crate::orchestrator::{self, Night, RunOptions};
use crate::qemu::QemuRunner;

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
        let data = config.data_dir();
        let asset_error = orchestrator::prepare_assets(&data, config.model.quant)
            .await
            .err()
            .map(|err| err.to_string());
        let night = Night::new(
            self.db.clone(),
            config.clone(),
            QemuRunner::new(data.clone(), self.config.read().await.guest.memory_mb),
            LlamaManager::new(data, config.model.quant),
            CargoHost,
        );
        let opts = RunOptions {
            ignore_window,
            stop: self.stop.clone(),
            asset_error,
            only_repo,
        };
        if let Err(err) = night.run_pass(&opts).await {
            tracing::error!("night pass failed: {err}");
            let _ = self
                .db
                .set_daemon("error", "idle", Some(&err.to_string()))
                .await;
        }
    }
}

async fn wait_stop(stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
