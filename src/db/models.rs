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
    pub content_hash: Option<String>,
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

/// A mutation testing result
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct MutationResult {
    pub id: i64,
    pub repository_id: i64,
    pub file_path: String,
    /// Human-readable description of the mutation
    pub description: String,
    /// Why this mutation was chosen
    pub reasoning: String,
    /// JSON map of line numbers to replacement content
    pub replacements_json: String,
    pub test_outcome: String,
    pub killing_test: Option<String>,
    pub test_output: Option<String>,
    pub execution_time_ms: Option<i32>,
    pub content_hash: Option<String>,
    pub created_at: String,
}

/// Summary statistics for mutation testing
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MutationSummary {
    pub total: usize,
    pub killed: usize,
    pub survived: usize,
    pub timeout: usize,
    pub compile_error: usize,
}

/// A generated D2 diagram for a repository
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Diagram {
    pub id: i64,
    pub repository_id: i64,
    /// Type of diagram: 'system_architecture', 'data_flow', 'database_schema'
    pub diagram_type: String,
    /// Human-readable title for the diagram
    pub title: String,
    /// Description of what the diagram shows
    pub description: String,
    /// The D2 diagram source code
    pub d2_content: String,
    /// Combined hash of source files used to generate this diagram
    pub content_hash: Option<String>,
    pub created_at: String,
}

impl MutationSummary {
    /// Calculate the mutation score (killed / (killed + survived))
    pub fn mutation_score(&self) -> f64 {
        let testable = self.killed + self.survived;
        if testable == 0 {
            0.0
        } else {
            self.killed as f64 / testable as f64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mutation_score_all_killed() {
        let summary = MutationSummary {
            total: 10,
            killed: 10,
            survived: 0,
            timeout: 0,
            compile_error: 0,
        };
        assert!((summary.mutation_score() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_mutation_score_none_killed() {
        let summary = MutationSummary {
            total: 10,
            killed: 0,
            survived: 10,
            timeout: 0,
            compile_error: 0,
        };
        assert!((summary.mutation_score() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_mutation_score_half_killed() {
        let summary = MutationSummary {
            total: 10,
            killed: 5,
            survived: 5,
            timeout: 0,
            compile_error: 0,
        };
        assert!((summary.mutation_score() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_mutation_score_no_testable() {
        // When there are only timeouts and compile errors, score should be 0
        let summary = MutationSummary {
            total: 10,
            killed: 0,
            survived: 0,
            timeout: 5,
            compile_error: 5,
        };
        assert!((summary.mutation_score() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_mutation_score_empty() {
        let summary = MutationSummary::default();
        assert!((summary.mutation_score() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_mutation_score_ignores_timeout_and_compile_error() {
        // Score should only consider killed and survived
        let summary = MutationSummary {
            total: 20,
            killed: 6,
            survived: 4,
            timeout: 5,
            compile_error: 5,
        };
        // 6 / (6 + 4) = 0.6
        assert!((summary.mutation_score() - 0.6).abs() < f64::EPSILON);
    }

    #[test]
    fn test_mutation_summary_default() {
        let summary = MutationSummary::default();
        assert_eq!(summary.total, 0);
        assert_eq!(summary.killed, 0);
        assert_eq!(summary.survived, 0);
        assert_eq!(summary.timeout, 0);
        assert_eq!(summary.compile_error, 0);
    }
}
