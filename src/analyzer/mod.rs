mod ollama;

pub use ollama::OllamaClient;

use serde::{Deserialize, Serialize};

/// Types of analysis that can be performed
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisType {
    /// Initial code understanding pass
    CodeUnderstanding,
    /// Mutation testing analysis
    MutationTesting,
    /// Security analysis
    Security,
    /// Code quality analysis
    Quality,
    /// Documentation analysis
    Documentation,
    /// Architectural/design summary aggregating all file analyses
    ArchitectureSummary,
}

impl std::fmt::Display for AnalysisType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AnalysisType::CodeUnderstanding => write!(f, "code_understanding"),
            AnalysisType::MutationTesting => write!(f, "mutation_testing"),
            AnalysisType::Security => write!(f, "security"),
            AnalysisType::Quality => write!(f, "quality"),
            AnalysisType::Documentation => write!(f, "documentation"),
            AnalysisType::ArchitectureSummary => write!(f, "architecture_summary"),
        }
    }
}
