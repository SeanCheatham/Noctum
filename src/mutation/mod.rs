//! LLM-driven mutation testing module.
//!
//! This module provides functionality for:
//! - Analyzing Rust code to find and generate mutations in a single LLM call
//! - Executing tests against mutations and recording results

pub mod analyzer;
pub mod executor;

// Re-export main function for convenience
pub use analyzer::analyze_and_generate_mutations;

use serde::{Deserialize, Serialize};

/// A generated mutation ready for testing (search/replace based).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedMutation {
    /// Path to the file containing this mutation
    pub file_path: String,
    /// Approximate line number (1-indexed) where the mutation is located
    pub line_number: usize,
    /// The exact text to find
    pub find: String,
    /// The replacement text
    pub replace: String,
    /// Why this is a high-value mutation point
    pub reasoning: String,
    /// Human-readable description of the mutation (e.g., "Changed > to >=")
    pub description: String,
}

/// Result of running tests against a mutation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestOutcome {
    /// A test failed - mutation was caught
    Killed,
    /// All tests passed - mutation was NOT caught
    Survived,
    /// Tests took too long
    Timeout,
    /// Mutation caused compilation failure
    CompileError,
}

impl std::fmt::Display for TestOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Killed => write!(f, "killed"),
            Self::Survived => write!(f, "survived"),
            Self::Timeout => write!(f, "timeout"),
            Self::CompileError => write!(f, "compile_error"),
        }
    }
}

/// Complete result of a mutation test
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationTestResult {
    pub mutation: GeneratedMutation,
    pub outcome: TestOutcome,
    pub killing_test: Option<String>,
    pub test_output: Option<String>,
    pub execution_time_ms: u64,
}

/// Configuration for mutation testing
#[derive(Debug, Clone)]
pub struct MutationConfig {
    /// Maximum mutations to test per file
    pub max_mutations_per_file: usize,
    /// Test timeout in seconds
    pub test_timeout_seconds: u64,
    /// Maximum test output to store (bytes)
    pub max_test_output_bytes: usize,
}

impl Default for MutationConfig {
    fn default() -> Self {
        Self {
            max_mutations_per_file: 10,
            test_timeout_seconds: 300, // 5 minutes
            max_test_output_bytes: 10000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_test_outcome_display() {
        assert_eq!(TestOutcome::Killed.to_string(), "killed");
        assert_eq!(TestOutcome::Survived.to_string(), "survived");
        assert_eq!(TestOutcome::Timeout.to_string(), "timeout");
        assert_eq!(TestOutcome::CompileError.to_string(), "compile_error");
    }

    #[test]
    fn test_mutation_config_default() {
        let config = MutationConfig::default();
        assert_eq!(config.max_mutations_per_file, 10);
        assert_eq!(config.test_timeout_seconds, 300);
        assert_eq!(config.max_test_output_bytes, 10000);
    }
}
