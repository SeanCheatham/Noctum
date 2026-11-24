//! Noctum runs overnight on one machine. It measures a Rust test inside a
//! pinned QEMU guest, asks a local model for a change at a hot function, and
//! keeps the change only when that same test uses fewer guest instruction callbacks.

pub mod analysis;
pub mod builder;
pub mod config;
pub mod daemon;
pub mod db;
pub mod diffcheck;
pub mod fence;
pub mod gate;
pub mod hashutil;
pub mod lockfile;
pub mod model;
pub mod optimization;
pub mod orchestrator;
pub mod ports;
pub mod process;
pub mod profile;
pub mod qemu;
pub mod repo_config;
pub mod runtime;
pub mod selector;
pub mod skip;
pub mod target;
pub mod web;
#[cfg(feature = "worker")]
pub mod worker;
pub mod worktree;

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::config::Config;
use crate::daemon::DaemonHandle;
use crate::db::Database;

/// Shared state for the dashboard and the daemon.
pub struct AppState {
    pub db: Database,
    pub config: Arc<RwLock<Config>>,
    pub config_path: Option<PathBuf>,
    pub daemon: DaemonHandle,
}

pub const MUSL_TARGET: &str = "aarch64-unknown-linux-musl";
pub const PERF_BRANCH: &str = "noctum/perf";
pub const LOG_LIMIT: usize = 16 * 1024;

#[cfg(test)]
mod source_tests {
    use std::fs;
    use std::path::Path;

    #[test]
    fn rust_sources_do_not_mention_the_old_model_client() {
        let needle: String = ['o', 'l', 'l', 'a', 'm', 'a'].into_iter().collect();
        let mut offenders = Vec::new();
        walk(Path::new("src"), &needle, &mut offenders);
        assert!(
            offenders.is_empty(),
            "unexpected reference in {offenders:?}"
        );
    }

    fn walk(dir: &Path, needle: &str, out: &mut Vec<String>) {
        let entries = fs::read_dir(dir).unwrap();
        for entry in entries {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                walk(&path, needle, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                let text = fs::read_to_string(&path).unwrap().to_ascii_lowercase();
                if text.contains(needle) {
                    out.push(path.display().to_string());
                }
            }
        }
    }
}
