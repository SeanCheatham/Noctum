use serde::{Deserialize, Serialize};
use sqlx::FromRow;

/// A repository configured for analysis
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Repository {
    pub id: i64,
    pub path: String,
    pub name: String,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// An analysis result from the daemon
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct AnalysisResult {
    pub id: i64,
    pub repository_id: i64,
    pub file_path: String,
    pub analysis_type: String,
    pub result: String,
    pub severity: Option<String>,
    pub created_at: String,
}

/// Current daemon state
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct DaemonState {
    pub id: i64,
    pub status: String,
    pub current_task: Option<String>,
    pub last_active: String,
}

/// Severity levels for analysis results
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Error,
    Critical,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Severity::Info => write!(f, "info"),
            Severity::Warning => write!(f, "warning"),
            Severity::Error => write!(f, "error"),
            Severity::Critical => write!(f, "critical"),
        }
    }
}
