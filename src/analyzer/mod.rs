mod ollama;

pub use ollama::OllamaClient;

use anyhow::Result;
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
}

impl std::fmt::Display for AnalysisType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AnalysisType::CodeUnderstanding => write!(f, "code_understanding"),
            AnalysisType::MutationTesting => write!(f, "mutation_testing"),
            AnalysisType::Security => write!(f, "security"),
            AnalysisType::Quality => write!(f, "quality"),
            AnalysisType::Documentation => write!(f, "documentation"),
        }
    }
}

/// Analyzer for running code analysis tasks
pub struct Analyzer {
    ollama: OllamaClient,
}

impl Analyzer {
    /// Create a new analyzer
    pub fn new(ollama_url: &str, model: &str) -> Self {
        Self {
            ollama: OllamaClient::new(ollama_url, model),
        }
    }

    /// Analyze a file for code understanding
    pub async fn analyze_file(&self, file_path: &str, content: &str) -> Result<String> {
        let prompt = format!(
            "Analyze the following Rust code and provide a brief summary of what it does:\n\n\
             File: {}\n\n\
             ```rust\n{}\n```\n\n\
             Provide a concise analysis including:\n\
             1. Purpose of the code\n\
             2. Key functions/structs\n\
             3. Any potential issues or improvements",
            file_path, content
        );

        self.ollama.generate(&prompt).await
    }

    /// Analyze mutation testing results
    pub async fn analyze_mutation_results(
        &self,
        file_path: &str,
        mutation_output: &str,
    ) -> Result<String> {
        let prompt = format!(
            "Analyze the following mutation testing results for a Rust file:\n\n\
             File: {}\n\n\
             Mutation Testing Output:\n{}\n\n\
             Provide insights on:\n\
             1. Areas with poor test coverage\n\
             2. Recommendations for additional tests\n\
             3. Priority of issues (high/medium/low)",
            file_path, mutation_output
        );

        self.ollama.generate(&prompt).await
    }
}
