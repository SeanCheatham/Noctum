use crate::db::{AnalysisResult, DaemonState, Repository};
use crate::AppState;
use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse},
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::templates::{DashboardTemplate, RepositoriesTemplate, ResultsTemplate};
use askama::Template;

// ============================================================================
// HTML Handlers
// ============================================================================

/// Dashboard page
pub async fn dashboard(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let daemon_status = state.db.get_daemon_status().await.ok();
    let repositories = state.db.get_repositories().await.unwrap_or_default();
    let recent_results = state.db.get_recent_results(10).await.unwrap_or_default();

    let template = DashboardTemplate {
        daemon_status,
        repository_count: repositories.len(),
        result_count: recent_results.len(),
    };

    match template.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Template error: {}", e),
        )
            .into_response(),
    }
}

/// Repositories page
pub async fn list_repositories(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let repositories = state.db.get_repositories().await.unwrap_or_default();

    let template = RepositoriesTemplate { repositories };

    match template.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Template error: {}", e),
        )
            .into_response(),
    }
}

/// Add a repository
#[derive(Deserialize)]
pub struct AddRepositoryRequest {
    path: String,
    name: String,
}

pub async fn add_repository(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AddRepositoryRequest>,
) -> impl IntoResponse {
    match state.db.add_repository(&req.path, &req.name).await {
        Ok(id) => (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// Results page
pub async fn list_results(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let results = state.db.get_recent_results(50).await.unwrap_or_default();

    let template = ResultsTemplate { results };

    match template.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Template error: {}", e),
        )
            .into_response(),
    }
}

// ============================================================================
// API Handlers
// ============================================================================

#[derive(Serialize)]
pub struct StatusResponse {
    pub daemon_status: Option<DaemonState>,
    pub version: &'static str,
}

/// API: Get daemon status
pub async fn api_status(State(state): State<Arc<AppState>>) -> Json<StatusResponse> {
    let daemon_status = state.db.get_daemon_status().await.ok();

    Json(StatusResponse {
        daemon_status,
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// API: Get repositories
pub async fn api_repositories(State(state): State<Arc<AppState>>) -> Json<Vec<Repository>> {
    let repositories = state.db.get_repositories().await.unwrap_or_default();
    Json(repositories)
}

/// API: Get analysis results
pub async fn api_results(State(state): State<Arc<AppState>>) -> Json<Vec<AnalysisResult>> {
    let results = state.db.get_recent_results(100).await.unwrap_or_default();
    Json(results)
}
