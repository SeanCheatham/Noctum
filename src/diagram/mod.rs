//! GraphViz DOT diagram generation module.
//!
//! This module handles the two-phase generation of DOT diagrams:
//! 1. **Extraction Phase**: Per-file analysis to extract diagram-relevant information
//! 2. **Generation Phase**: Aggregation of extractions into final DOT diagrams
//!
//! Supported diagram types:
//! - System Architecture: High-level component relationships
//! - Data Flow: How data moves through the system
//! - Database Schema: Database tables and relationships

mod extractor;
mod generator;

pub use extractor::DiagramExtractor;
pub use generator::DiagramGenerator;

use layout::backends::svg::SVGWriter;
use layout::gv::{DotParser, GraphBuilder};
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

/// Validate DOT syntax using the layout-rs parser.
/// Returns Ok(()) if valid, or Err with a descriptive error message.
pub fn validate_dot_syntax(dot_code: &str) -> Result<(), String> {
    // Check for empty content first
    let content = dot_code
        .lines()
        .filter(|l| {
            let trimmed = l.trim();
            !trimmed.is_empty() && !trimmed.starts_with("//") && !trimmed.starts_with('#')
        })
        .count();
    if content == 0 {
        return Err("DOT diagram is empty".to_string());
    }

    // Use the layout-rs parser for real validation
    let mut parser = DotParser::new(dot_code);
    match parser.process() {
        Ok(_) => Ok(()),
        Err(err) => Err(format!("DOT syntax error: {}", err)),
    }
}

/// Render DOT code to SVG using the layout-rs library.
/// Returns the SVG string on success, or an error message on failure.
pub fn render_dot_to_svg(dot_code: &str) -> Result<String, String> {
    // Parse the DOT code
    let mut parser = DotParser::new(dot_code);
    let graph = parser
        .process()
        .map_err(|e| format!("DOT parse error: {}", e))?;

    // Build the visual graph
    let mut builder = GraphBuilder::new();
    builder.visit_graph(&graph);
    let mut vg = builder.get();

    // Render to SVG
    let mut svg_writer = SVGWriter::new();
    vg.do_it(false, false, false, &mut svg_writer);

    Ok(svg_writer.finalize())
}

/// Clean up DOT code from LLM output.
/// Removes markdown code fences and other common artifacts.
pub fn clean_dot_output(raw_output: &str) -> String {
    let mut result = raw_output.trim().to_string();

    // Remove markdown code fences (various formats)
    for prefix in ["```dot", "```graphviz", "```"] {
        if result.starts_with(prefix) {
            result = result.strip_prefix(prefix).unwrap_or(&result).to_string();
            break;
        }
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
    fn test_validate_dot_syntax_valid_digraph() {
        let valid_dot = r#"
            digraph G {
                a -> b;
                b -> c;
            }
        "#;
        assert!(validate_dot_syntax(valid_dot).is_ok());
    }

    #[test]
    fn test_validate_dot_syntax_valid_with_labels() {
        let valid_dot = r#"
            digraph Architecture {
                rankdir=LR;
                web [label="Web Server"];
                db [label="Database"];
                web -> db [label="queries"];
            }
        "#;
        assert!(validate_dot_syntax(valid_dot).is_ok());
    }

    #[test]
    fn test_validate_dot_syntax_valid_subgraph() {
        let valid_dot = r#"
            digraph G {
                subgraph cluster_0 {
                    label="Process 1";
                    a0 -> a1;
                }
                subgraph cluster_1 {
                    label="Process 2";
                    b0 -> b1;
                }
                a1 -> b0;
            }
        "#;
        assert!(validate_dot_syntax(valid_dot).is_ok());
    }

    #[test]
    fn test_validate_dot_syntax_invalid() {
        let invalid_dot = "digraph { a -> }"; // Missing target
        let result = validate_dot_syntax(invalid_dot);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_dot_syntax_empty() {
        let result = validate_dot_syntax("");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("empty"));
    }

    #[test]
    fn test_validate_dot_syntax_only_comments() {
        let result = validate_dot_syntax("// Just a comment\n// Another comment");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("empty"));
    }

    #[test]
    fn test_render_dot_to_svg_simple() {
        let dot = "digraph G { a -> b; }";
        let result = render_dot_to_svg(dot);
        assert!(result.is_ok());
        let svg = result.unwrap();
        assert!(svg.contains("<svg"));
        assert!(svg.contains("</svg>"));
    }

    #[test]
    fn test_render_dot_to_svg_invalid() {
        let invalid_dot = "digraph { -> }";
        let result = render_dot_to_svg(invalid_dot);
        assert!(result.is_err());
    }

    #[test]
    fn test_clean_dot_output_with_dot_fence() {
        let raw = "```dot\ndigraph G { a -> b; }\n```";
        assert_eq!(clean_dot_output(raw), "digraph G { a -> b; }");
    }

    #[test]
    fn test_clean_dot_output_with_graphviz_fence() {
        let raw = "```graphviz\ndigraph G { a -> b; }\n```";
        assert_eq!(clean_dot_output(raw), "digraph G { a -> b; }");
    }

    #[test]
    fn test_clean_dot_output_with_generic_fence() {
        let raw = "```\ndigraph G { a -> b; }\n```";
        assert_eq!(clean_dot_output(raw), "digraph G { a -> b; }");
    }

    #[test]
    fn test_clean_dot_output_no_fence() {
        let raw = "  digraph G { a -> b; }  ";
        assert_eq!(clean_dot_output(raw), "digraph G { a -> b; }");
    }
}
