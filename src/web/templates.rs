//! Askama templates and view models for HTML rendering.
//!
//! Contains template structs for each page and view wrappers that transform
//! database models for display (e.g., converting absolute paths to relative).

use crate::config::OllamaEndpoint;
use crate::db::{AnalysisResult, Diagram, MutationResult, MutationSummary, Repository};
use askama::Template;
use pulldown_cmark::{html, Options, Parser};
use serde::Serialize;

/// Render markdown to HTML
pub fn render_markdown(s: &str) -> String {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_HEADING_ATTRIBUTES
        | Options::ENABLE_SMART_PUNCTUATION;
    let parser = Parser::new_ext(s, options);
    let mut html_output = String::new();
    html::push_html(&mut html_output, parser);
    html_output
}

#[derive(Template)]
#[template(path = "repositories.html")]
pub struct RepositoriesTemplate {
    pub repositories: Vec<Repository>,
}

#[derive(Template)]
#[template(path = "settings.html")]
pub struct SettingsTemplate {
    pub endpoints: Vec<OllamaEndpoint>,
    pub start_hour: u8,
    pub end_hour: u8,
    pub config_path: String,
}

/// An analysis result with a relative file path for display
#[derive(Clone, Serialize)]
pub struct AnalysisResultView {
    pub id: i64,
    pub repository_id: i64,
    pub file_path: String,
    pub analysis_type: String,
    pub result: String,
    pub severity: Option<String>,
    pub content_hash: Option<String>,
    pub created_at: String,
}

impl AnalysisResultView {
    /// Create a view from an AnalysisResult, stripping the repo path from file_path
    pub fn from_result(result: AnalysisResult, repo_path: &str) -> Self {
        let relative_path = result
            .file_path
            .strip_prefix(repo_path)
            .map(|p| p.trim_start_matches('/'))
            .unwrap_or(&result.file_path)
            .to_string();

        Self {
            id: result.id,
            repository_id: result.repository_id,
            file_path: relative_path,
            analysis_type: result.analysis_type,
            result: result.result,
            severity: result.severity,
            content_hash: result.content_hash,
            created_at: result.created_at,
        }
    }
}

#[derive(Template)]
#[template(path = "repository_architecture.html")]
pub struct RepositoryArchitectureTemplate {
    pub repository: Repository,
    pub architecture_summary: Option<AnalysisResult>,
    pub architecture_summary_html: String,
}

#[derive(Template)]
#[template(path = "repository_files.html")]
pub struct RepositoryFilesTemplate {
    pub repository: Repository,
    pub file_results: Vec<AnalysisResultView>,
}

/// A mutation result with a relative file path for display
#[derive(Clone, Serialize)]
pub struct MutationResultView {
    pub id: i64,
    pub repository_id: i64,
    pub file_path: String,
    pub description: String,
    pub reasoning: String,
    pub replacements_json: String,
    pub test_outcome: String,
    pub killing_test: Option<String>,
    pub test_output: Option<String>,
    pub execution_time_ms: Option<i32>,
    pub content_hash: Option<String>,
    pub created_at: String,
}

impl MutationResultView {
    /// Create a view from a MutationResult, stripping the repo path from file_path
    pub fn from_result(result: MutationResult, repo_path: &str) -> Self {
        let relative_path = result
            .file_path
            .strip_prefix(repo_path)
            .map(|p| p.trim_start_matches('/'))
            .unwrap_or(&result.file_path)
            .to_string();

        Self {
            id: result.id,
            repository_id: result.repository_id,
            file_path: relative_path,
            description: result.description,
            reasoning: result.reasoning,
            replacements_json: result.replacements_json,
            test_outcome: result.test_outcome,
            killing_test: result.killing_test,
            test_output: result.test_output,
            execution_time_ms: result.execution_time_ms,
            content_hash: result.content_hash,
            created_at: result.created_at,
        }
    }
}

#[derive(Template)]
#[template(path = "mutation_results.html")]
pub struct MutationResultsTemplate {
    pub repository: Repository,
    pub results: Vec<MutationResultView>,
    pub summary: MutationSummary,
    pub mutation_score_percent: String,
}

#[derive(Template)]
#[template(path = "repository_diagrams.html")]
pub struct RepositoryDiagramsTemplate {
    pub repository: Repository,
    pub diagrams: Vec<Diagram>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_markdown_basic() {
        let md = "# Heading\n\nSome **bold** text.";
        let html = render_markdown(md);

        assert!(html.contains("<h1>"));
        assert!(html.contains("Heading"));
        assert!(html.contains("<strong>"));
        assert!(html.contains("bold"));
    }

    #[test]
    fn test_render_markdown_code() {
        let md = "```rust\nfn main() {}\n```";
        let html = render_markdown(md);

        assert!(html.contains("<code>") || html.contains("<pre>"));
    }

    #[test]
    fn test_render_markdown_links() {
        let md = "[link](http://example.com)";
        let html = render_markdown(md);

        assert!(html.contains("<a href"));
        assert!(html.contains("http://example.com"));
    }

    #[test]
    fn test_render_markdown_lists() {
        let md = "- item 1\n- item 2";
        let html = render_markdown(md);

        assert!(html.contains("<ul>") || html.contains("<li>"));
    }

    #[test]
    fn test_render_markdown_tables() {
        let md = "| col1 | col2 |\n|------|------|\n| val1 | val2 |";
        let html = render_markdown(md);

        assert!(html.contains("<table>") || html.contains("<td>"));
    }

    #[test]
    fn test_render_markdown_empty() {
        let html = render_markdown("");
        assert!(html.is_empty());
    }

    #[test]
    fn test_analysis_result_view_from_result_full_path() {
        let result = AnalysisResult {
            id: 1,
            repository_id: 1,
            file_path: "/repo/path/src/main.rs".to_string(),
            analysis_type: "type1".to_string(),
            result: "test".to_string(),
            severity: Some("info".to_string()),
            content_hash: Some("hash".to_string()),
            created_at: "2025-01-01".to_string(),
        };

        let view = AnalysisResultView::from_result(result, "/repo/path");
        assert_eq!(view.file_path, "src/main.rs");
    }

    #[test]
    fn test_analysis_result_view_from_result_with_leading_slash() {
        let result = AnalysisResult {
            id: 1,
            repository_id: 1,
            file_path: "/repo/path//src/main.rs".to_string(),
            analysis_type: "type1".to_string(),
            result: "test".to_string(),
            severity: None,
            content_hash: None,
            created_at: "2025-01-01".to_string(),
        };

        let view = AnalysisResultView::from_result(result, "/repo/path");
        assert_eq!(view.file_path, "src/main.rs");
    }

    #[test]
    fn test_analysis_result_view_from_result_no_match() {
        let result = AnalysisResult {
            id: 1,
            repository_id: 1,
            file_path: "/other/path/src/main.rs".to_string(),
            analysis_type: "type1".to_string(),
            result: "test".to_string(),
            severity: None,
            content_hash: None,
            created_at: "2025-01-01".to_string(),
        };

        let view = AnalysisResultView::from_result(result, "/repo/path");
        assert_eq!(view.file_path, "/other/path/src/main.rs");
    }

    #[test]
    fn test_mutation_result_view_from_result_full_path() {
        let result = MutationResult {
            id: 1,
            repository_id: 1,
            file_path: "/repo/path/src/main.rs".to_string(),
            description: "test".to_string(),
            reasoning: "reason".to_string(),
            replacements_json: "{}".to_string(),
            test_outcome: "killed".to_string(),
            killing_test: Some("test_foo".to_string()),
            test_output: Some("output".to_string()),
            execution_time_ms: Some(100),
            content_hash: Some("hash".to_string()),
            created_at: "2025-01-01".to_string(),
        };

        let view = MutationResultView::from_result(result, "/repo/path");
        assert_eq!(view.file_path, "src/main.rs");
    }

    #[test]
    fn test_mutation_result_view_from_result_no_match() {
        let result = MutationResult {
            id: 1,
            repository_id: 1,
            file_path: "/other/path/src/main.rs".to_string(),
            description: "test".to_string(),
            reasoning: "reason".to_string(),
            replacements_json: "{}".to_string(),
            test_outcome: "killed".to_string(),
            killing_test: None,
            test_output: None,
            execution_time_ms: None,
            content_hash: None,
            created_at: "2025-01-01".to_string(),
        };

        let view = MutationResultView::from_result(result, "/repo/path");
        assert_eq!(view.file_path, "/other/path/src/main.rs");
    }
}
