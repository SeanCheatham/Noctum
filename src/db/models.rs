//! Rows in `perf.db`. The old mutation tables are not here.

use serde::Serialize;
use sqlx::FromRow;

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Repository {
    pub id: i64,
    pub path: String,
    pub name: String,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct DaemonState {
    pub status: String,
    pub phase: String,
    pub detail: Option<String>,
    pub last_active: String,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Run {
    pub target: String,
    #[sqlx(default)]
    pub measurement_profile: String,
    pub id: i64,
    pub repository_id: i64,
    pub git_sha: String,
    pub package: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: String,
    pub error: Option<String>,
    pub window_key: Option<String>,
    pub perf_sha: Option<String>,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct TestBaseline {
    pub id: i64,
    pub run_id: i64,
    pub harness: String,
    pub test_name: String,
    pub status: String,
    pub insn_count: Option<i64>,
    pub stdout_sha256: Option<String>,
    pub binary_sha256: Option<String>,
    pub wall_ms: Option<i64>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Candidate {
    pub id: i64,
    pub run_id: i64,
    pub test_name: String,
    pub harness: String,
    pub rank: i64,
    pub symbol: String,
    pub file: String,
    pub line_start: i64,
    pub line_end: i64,
    pub exclusive_count: i64,
    pub share: f64,
    pub file_blob: String,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Trial {
    pub target: String,
    pub id: i64,
    pub candidate_id: i64,
    pub status: String,
    pub reason: Option<String>,
    pub model_summary: Option<String>,
    pub unified_diff: Option<String>,
    pub baseline_count: Option<i64>,
    pub insn_count: Option<i64>,
    pub improvement_ratio: Option<f64>,
    pub host_check_passed: bool,
    pub stdout_matched: bool,
    pub tool_output: Option<String>,
    pub test_name: String,
    pub harness: String,
    pub symbol: String,
    pub file: String,
}

#[derive(Debug, Clone, FromRow)]
pub struct PendingCommit {
    pub target: String,
    pub trial_id: i64,
    pub unified_diff: String,
    pub model_summary: Option<String>,
    pub test_name: String,
    pub baseline_count: Option<i64>,
    pub insn_count: Option<i64>,
    pub improvement_ratio: Option<f64>,
}
