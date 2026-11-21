//! Askama pages for the dashboard.

use askama::Template;

#[derive(Template)]
#[template(path = "home.html")]
pub struct HomeTemplate {
    pub phase: String,
    pub detail: String,
    pub window: String,
    pub model: String,
    pub qemu: String,
    pub repositories: Vec<RepoRow>,
}

#[derive(Template)]
#[template(path = "repository.html")]
pub struct RepositoryTemplate {
    pub name: String,
    pub path: String,
    pub git_sha: String,
    pub perf_sha: String,
    pub run_status: String,
    pub run_error: String,
    pub delta: String,
    pub baselines: Vec<BaselineRow>,
    pub trials: Vec<TrialRow>,
}

#[derive(Template)]
#[template(path = "trial.html")]
pub struct TrialTemplate {
    pub test_name: String,
    pub symbol: String,
    pub outcome: String,
    pub why: String,
    pub reason: String,
    pub summary: String,
    pub baseline: String,
    pub candidate: String,
    pub ratio: String,
    pub diff: String,
    pub tool_output: String,
}

#[derive(Template)]
#[template(path = "settings.html")]
pub struct SettingsTemplate {
    pub start_hour: u8,
    pub end_hour: u8,
    pub check_interval_seconds: u64,
    pub min_improvement_ratio: f64,
    pub min_insn_delta: u64,
    pub max_candidates: u32,
    pub max_tests: u32,
    pub max_scan: u32,
    pub host: String,
    pub port: u16,
    pub model_lock: String,
    pub qemu_lock: String,
    pub saved: bool,
}

pub struct RepoRow {
    pub id: i64,
    pub name: String,
    pub path: String,
    pub enabled: bool,
}

pub struct BaselineRow {
    pub test_name: String,
    pub status: String,
    pub insn_count: String,
    pub reason: String,
}

pub struct TrialRow {
    pub id: i64,
    pub symbol: String,
    pub test_name: String,
    pub before: String,
    pub after: String,
    pub result: String,
    pub why: String,
    pub summary: String,
    pub ratio: String,
}
