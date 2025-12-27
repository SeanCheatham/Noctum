//! D2 diagram generation module.
//!
//! This module handles the two-phase generation of D2 diagrams:
//! 1. **Extraction Phase**: Per-file analysis to extract diagram-relevant information
//! 2. **Generation Phase**: Aggregation of extractions into final D2 diagrams
//!
//! Supported diagram types:
//! - System Architecture: High-level component relationships
//! - Data Flow: How data moves through the system
//! - Database Schema: Database tables and relationships

mod extractor;
mod generator;

pub use extractor::DiagramExtractor;
pub use generator::DiagramGenerator;

use serde::{Deserialize, Serialize};

/// Types of diagrams that can be generated
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagramType {
    /// High-level view of system components and their relationships
    SystemArchitecture,
    /// How data moves through the system
    DataFlow,
    /// Database tables and their relationships
    DatabaseSchema,
}

impl DiagramType {
    /// Returns all available diagram types
    pub fn all() -> &'static [DiagramType] {
        &[
            DiagramType::SystemArchitecture,
            DiagramType::DataFlow,
            DiagramType::DatabaseSchema,
        ]
    }

    /// Returns the string identifier for this diagram type (used in database)
    pub fn as_str(&self) -> &'static str {
        match self {
            DiagramType::SystemArchitecture => "system_architecture",
            DiagramType::DataFlow => "data_flow",
            DiagramType::DatabaseSchema => "database_schema",
        }
    }

    /// Returns a human-readable title for this diagram type
    pub fn title(&self) -> &'static str {
        match self {
            DiagramType::SystemArchitecture => "System Architecture",
            DiagramType::DataFlow => "Data Flow",
            DiagramType::DatabaseSchema => "Database Schema",
        }
    }

    /// Returns a description of what this diagram shows
    pub fn description(&self) -> &'static str {
        match self {
            DiagramType::SystemArchitecture => {
                "High-level view of system components and their relationships"
            }
            DiagramType::DataFlow => "How data moves through the system",
            DiagramType::DatabaseSchema => "Database tables and their relationships",
        }
    }
}

impl std::fmt::Display for DiagramType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Basic D2 syntax validation
/// Returns Ok(()) if the D2 code appears valid, or Err with an error message
pub fn validate_d2_syntax(d2_code: &str) -> Result<(), String> {
    // Check for balanced braces
    let mut brace_count = 0i32;
    for ch in d2_code.chars() {
        match ch {
            '{' => brace_count += 1,
            '}' => {
                brace_count -= 1;
                if brace_count < 0 {
                    return Err("Unbalanced braces: extra closing brace".to_string());
                }
            }
            _ => {}
        }
    }
    if brace_count != 0 {
        return Err(format!(
            "Unbalanced braces: {} unclosed opening brace(s)",
            brace_count
        ));
    }

    // Check that arrows have valid format (basic check)
    // Valid: ->, <-, <->, --
    for line in d2_code.lines() {
        let trimmed = line.trim();
        // Skip empty lines and comments
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Check for malformed arrows (single dash not part of valid arrow)
        // This is a heuristic - D2 is complex, we just catch obvious errors
        if trimmed.contains(" - ") && !trimmed.contains(" -- ") {
            // Single dash between spaces might be an error
            // But could also be in a string, so just warn
        }
    }

    // Check for empty content
    let content = d2_code
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim().starts_with('#'))
        .count();
    if content == 0 {
        return Err("D2 diagram is empty".to_string());
    }

    Ok(())
}

/// Clean up D2 code from LLM output
/// Removes markdown code fences and other common artifacts
pub fn clean_d2_output(raw_output: &str) -> String {
    let mut result = raw_output.trim().to_string();

    // Remove markdown code fences
    if result.starts_with("```d2") {
        result = result.strip_prefix("```d2").unwrap_or(&result).to_string();
    } else if result.starts_with("```") {
        result = result.strip_prefix("```").unwrap_or(&result).to_string();
    }

    if result.ends_with("```") {
        result = result.strip_suffix("```").unwrap_or(&result).to_string();
    }

    result.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diagram_type_all() {
        let types = DiagramType::all();
        assert_eq!(types.len(), 3);
        assert!(types.contains(&DiagramType::SystemArchitecture));
        assert!(types.contains(&DiagramType::DataFlow));
        assert!(types.contains(&DiagramType::DatabaseSchema));
    }

    #[test]
    fn test_diagram_type_as_str() {
        assert_eq!(
            DiagramType::SystemArchitecture.as_str(),
            "system_architecture"
        );
        assert_eq!(DiagramType::DataFlow.as_str(), "data_flow");
        assert_eq!(DiagramType::DatabaseSchema.as_str(), "database_schema");
    }

    #[test]
    fn test_diagram_type_title() {
        assert_eq!(
            DiagramType::SystemArchitecture.title(),
            "System Architecture"
        );
        assert_eq!(DiagramType::DataFlow.title(), "Data Flow");
        assert_eq!(DiagramType::DatabaseSchema.title(), "Database Schema");
    }

    #[test]
    fn test_diagram_type_display() {
        assert_eq!(
            format!("{}", DiagramType::SystemArchitecture),
            "system_architecture"
        );
    }

    #[test]
    fn test_validate_d2_syntax_valid() {
        let valid_d2 = r#"
            web: Web Layer {
                handlers: HTTP Handlers
            }
            db: Database
            web -> db: queries
        "#;
        assert!(validate_d2_syntax(valid_d2).is_ok());
    }

    #[test]
    fn test_validate_d2_syntax_unbalanced_braces() {
        let invalid_d2 = "web { handlers }}}";
        let result = validate_d2_syntax(invalid_d2);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unbalanced"));
    }

    #[test]
    fn test_validate_d2_syntax_unclosed_brace() {
        let invalid_d2 = "web { handlers";
        let result = validate_d2_syntax(invalid_d2);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unclosed"));
    }

    #[test]
    fn test_validate_d2_syntax_empty() {
        let result = validate_d2_syntax("");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("empty"));
    }

    #[test]
    fn test_validate_d2_syntax_only_comments() {
        let result = validate_d2_syntax("# Just a comment\n# Another comment");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("empty"));
    }

    #[test]
    fn test_clean_d2_output_with_code_fence() {
        let raw = "```d2\nweb -> db\n```";
        assert_eq!(clean_d2_output(raw), "web -> db");
    }

    #[test]
    fn test_clean_d2_output_with_generic_fence() {
        let raw = "```\nweb -> db\n```";
        assert_eq!(clean_d2_output(raw), "web -> db");
    }

    #[test]
    fn test_clean_d2_output_no_fence() {
        let raw = "  web -> db  ";
        assert_eq!(clean_d2_output(raw), "web -> db");
    }

    #[test]
    fn test_clean_d2_output_preserves_internal_content() {
        let raw = "```d2\nweb: Web {\n  api: API\n}\n```";
        let cleaned = clean_d2_output(raw);
        assert!(cleaned.contains("web: Web"));
        assert!(cleaned.contains("api: API"));
    }
}
