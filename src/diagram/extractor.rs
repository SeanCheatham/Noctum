//! Diagram extraction prompts for per-file analysis.
//!
//! This module provides prompts for the first phase of diagram generation:
//! extracting diagram-relevant information from individual source files.
//!
//! Language-specific prompts are delegated to the `language` module.

use super::DiagramType;
use crate::language::Language;

/// Provides prompts for extracting diagram-relevant information from source files.
///
/// This struct delegates to language-specific implementations for prompt generation.
/// Currently only Rust is supported, but the architecture allows for future language additions.
pub struct DiagramExtractor;

impl DiagramExtractor {
    /// Get the extraction prompt for a specific diagram type.
    ///
    /// Delegates to language-specific prompt generation.
    pub fn prompt_for_type(
        diagram_type: DiagramType,
        file_path: &str,
        code: &str,
        language: Language,
    ) -> String {
        match diagram_type {
            DiagramType::SystemArchitecture => {
                language.diagram_architecture_prompt(file_path, code)
            }
            DiagramType::DataFlow => language.diagram_data_flow_prompt(file_path, code),
            DiagramType::DatabaseSchema => language.diagram_database_schema_prompt(file_path, code),
        }
    }

    /// Prompt for architecture-focused file analysis (used for Architecture tab).
    ///
    /// Delegates to language-specific prompt generation.
    pub fn architecture_file_analysis_prompt(
        file_path: &str,
        code: &str,
        language: Language,
    ) -> String {
        language.architecture_file_analysis_prompt(file_path, code)
    }

    /// Prompt for analyzing documentation and context files (READMEs, Cargo.toml, etc.).
    ///
    /// Delegates to language-specific prompt generation.
    pub fn documentation_analysis_prompt(
        file_path: &str,
        content: &str,
        language: Language,
    ) -> String {
        language.documentation_prompt(file_path, content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prompt_for_type_dispatches_correctly() {
        let arch_prompt = DiagramExtractor::prompt_for_type(
            DiagramType::SystemArchitecture,
            "test.rs",
            "code",
            Language::Rust,
        );
        assert!(arch_prompt.contains("ARCHITECTURAL"));

        let flow_prompt = DiagramExtractor::prompt_for_type(
            DiagramType::DataFlow,
            "test.rs",
            "code",
            Language::Rust,
        );
        assert!(flow_prompt.contains("DATA FLOW"));

        let db_prompt = DiagramExtractor::prompt_for_type(
            DiagramType::DatabaseSchema,
            "test.rs",
            "code",
            Language::Rust,
        );
        assert!(db_prompt.contains("DATABASE"));
    }

    #[test]
    fn test_prompt_for_type_typescript() {
        let arch_prompt = DiagramExtractor::prompt_for_type(
            DiagramType::SystemArchitecture,
            "test.ts",
            "code",
            Language::TypeScript,
        );
        assert!(arch_prompt.contains("ARCHITECTURAL"));

        let flow_prompt = DiagramExtractor::prompt_for_type(
            DiagramType::DataFlow,
            "test.ts",
            "code",
            Language::TypeScript,
        );
        assert!(flow_prompt.contains("DATA FLOW"));
    }

    #[test]
    fn test_architecture_file_analysis_prompt() {
        let prompt = DiagramExtractor::architecture_file_analysis_prompt(
            "src/web/mod.rs",
            "pub mod handlers;",
            Language::Rust,
        );
        assert!(prompt.contains("ARCHITECTURAL"));
        assert!(prompt.contains("Layer"));
        assert!(prompt.contains("Key Abstractions"));
    }

    #[test]
    fn test_documentation_analysis_prompt() {
        let prompt = DiagramExtractor::documentation_analysis_prompt(
            "README.md",
            "# My Project",
            Language::Rust,
        );
        assert!(prompt.contains("README.md"));
    }
}
