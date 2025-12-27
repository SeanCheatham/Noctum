mod ollama;

pub use ollama::OllamaClient;

use serde::{Deserialize, Serialize};

/// Types of analysis that can be performed
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisType {
    /// Initial code understanding pass (for File Analysis tab)
    CodeUnderstanding,
    /// Architecture-focused per-file analysis (for Architecture tab aggregation)
    ArchitectureFileAnalysis,
    /// Architectural/design summary aggregating all architecture file analyses
    ArchitectureSummary,
    /// Per-file extraction of diagram-relevant information
    DiagramExtraction,
    /// Mutation testing analysis
    MutationTesting,
    /// Security analysis
    Security,
    /// Code quality analysis
    Quality,
    /// Documentation analysis
    Documentation,
}

impl std::fmt::Display for AnalysisType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AnalysisType::CodeUnderstanding => write!(f, "code_understanding"),
            AnalysisType::ArchitectureFileAnalysis => write!(f, "architecture_file_analysis"),
            AnalysisType::ArchitectureSummary => write!(f, "architecture_summary"),
            AnalysisType::DiagramExtraction => write!(f, "diagram_extraction"),
            AnalysisType::MutationTesting => write!(f, "mutation_testing"),
            AnalysisType::Security => write!(f, "security"),
            AnalysisType::Quality => write!(f, "quality"),
            AnalysisType::Documentation => write!(f, "documentation"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_analysis_type_display() {
        assert_eq!(
            AnalysisType::CodeUnderstanding.to_string(),
            "code_understanding"
        );
        assert_eq!(
            AnalysisType::ArchitectureFileAnalysis.to_string(),
            "architecture_file_analysis"
        );
        assert_eq!(
            AnalysisType::ArchitectureSummary.to_string(),
            "architecture_summary"
        );
        assert_eq!(
            AnalysisType::DiagramExtraction.to_string(),
            "diagram_extraction"
        );
        assert_eq!(
            AnalysisType::MutationTesting.to_string(),
            "mutation_testing"
        );
    }
}
