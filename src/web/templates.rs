use crate::config::OllamaEndpoint;
use crate::db::{AnalysisResult, MutationResult, MutationSummary, Repository};
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
#[template(path = "repository_results.html")]
pub struct RepositoryResultsTemplate {
    pub repository: Repository,
    pub architecture_summary: Option<AnalysisResult>,
    pub file_results: Vec<AnalysisResultView>,
    pub architecture_summary_html: String,
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
