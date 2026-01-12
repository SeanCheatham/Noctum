//! Diagram extraction prompts for per-file analysis.
//!
//! This module provides prompts for the first phase of diagram generation:
//! extracting diagram-relevant information from individual source files.
//!
//! Uses language-agnostic prompts that let the LLM infer the language from code.

use super::DiagramType;
use crate::language;

/// Provides prompts for extracting diagram-relevant information from source files.
pub struct DiagramExtractor;

impl DiagramExtractor {
    /// Get the extraction prompt for a specific diagram type.
    pub fn prompt_for_type(diagram_type: DiagramType, file_path: &str, code: &str) -> String {
        match diagram_type {
            DiagramType::SystemArchitecture => {
                language::diagram_architecture_prompt(file_path, code)
            }
            DiagramType::DataFlow => language::diagram_data_flow_prompt(file_path, code),
            DiagramType::DatabaseSchema => {
                language::diagram_database_schema_prompt(file_path, code)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prompt_for_type_dispatches_correctly() {
        let arch_prompt =
            DiagramExtractor::prompt_for_type(DiagramType::SystemArchitecture, "test.rs", "code");
        assert!(arch_prompt.contains("ARCHITECTURAL"));

        let flow_prompt =
            DiagramExtractor::prompt_for_type(DiagramType::DataFlow, "test.rs", "code");
        assert!(flow_prompt.contains("DATA FLOW"));

        let db_prompt =
            DiagramExtractor::prompt_for_type(DiagramType::DatabaseSchema, "test.rs", "code");
        assert!(db_prompt.contains("DATABASE"));
    }

    #[test]
    fn test_prompt_for_type_typescript() {
        let arch_prompt =
            DiagramExtractor::prompt_for_type(DiagramType::SystemArchitecture, "test.ts", "code");
        assert!(arch_prompt.contains("ARCHITECTURAL"));

        let flow_prompt =
            DiagramExtractor::prompt_for_type(DiagramType::DataFlow, "test.ts", "code");
        assert!(flow_prompt.contains("DATA FLOW"));
    }
}
