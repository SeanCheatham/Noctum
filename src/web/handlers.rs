use crate::analyzer::OllamaClient;
use crate::config::{Config, OllamaEndpoint};
use crate::db::{AnalysisResult, DaemonState, Repository};
use crate::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse},
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::info;

use super::templates::{
    render_markdown, AnalysisResultView, MutationResultView, MutationResultsTemplate,
    RepositoriesTemplate, RepositoryResultsTemplate, SettingsTemplate,
};
use askama::Template;

// ============================================================================
// HTML Handlers
// ============================================================================

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

/// Repository results page
pub async fn repository_results(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    // Get the repository
    let repository = match state.db.get_repository(id).await {
        Ok(Some(repo)) => repo,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, "Repository not found").into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            )
                .into_response();
        }
    };

    // Get all results for this repository
    let all_results = state
        .db
        .get_all_repository_results(id)
        .await
        .unwrap_or_default();

    // Separate architecture summary from file results
    let mut architecture_summary = None;
    let mut file_results = Vec::new();

    for result in all_results {
        if result.analysis_type == "architecture_summary" {
            architecture_summary = Some(result);
        } else {
            file_results.push(result);
        }
    }

    // Convert file results to view models with relative paths
    let file_results: Vec<AnalysisResultView> = file_results
        .into_iter()
        .map(|r| AnalysisResultView::from_result(r, &repository.path))
        .collect();

    // Pre-render architecture summary markdown
    let architecture_summary_html = architecture_summary
        .as_ref()
        .map(|s| render_markdown(&s.result))
        .unwrap_or_default();

    let template = RepositoryResultsTemplate {
        repository,
        architecture_summary,
        file_results,
        architecture_summary_html,
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

/// Mutation testing results page
pub async fn mutation_results(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    // Get the repository
    let repository = match state.db.get_repository(id).await {
        Ok(Some(repo)) => repo,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, "Repository not found").into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error: {}", e),
            )
                .into_response();
        }
    };

    // Get mutation results and summary
    let raw_results = state.db.get_mutation_results(id).await.unwrap_or_default();
    let summary = state.db.get_mutation_summary(id).await.unwrap_or_default();

    // Convert to view models with relative paths
    let results: Vec<MutationResultView> = raw_results
        .into_iter()
        .map(|r| MutationResultView::from_result(r, &repository.path))
        .collect();

    // Pre-compute the mutation score as a formatted string
    let mutation_score_percent = format!("{:.1}", summary.mutation_score() * 100.0);

    let template = MutationResultsTemplate {
        repository,
        results,
        summary,
        mutation_score_percent,
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

/// Settings page - shows all Ollama endpoints and config
pub async fn settings(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let config = state.config.read().await;
    let endpoints = config.endpoints.clone();
    let start_hour = config.schedule.start_hour;
    let end_hour = config.schedule.end_hour;
    let config_path = Config::default_config_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(unknown)".to_string());

    let template = SettingsTemplate {
        endpoints,
        start_hour,
        end_hour,
        config_path,
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

/// Add a new Ollama endpoint
#[derive(Deserialize)]
pub struct AddEndpointRequest {
    name: String,
    url: String,
    model: String,
}

pub async fn add_endpoint(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AddEndpointRequest>,
) -> impl IntoResponse {
    let new_endpoint = OllamaEndpoint {
        name: req.name,
        url: req.url,
        model: req.model,
        enabled: true,
    };

    {
        let mut config = state.config.write().await;
        config.endpoints.push(new_endpoint);
    }

    info!("Added new Ollama endpoint");
    (
        StatusCode::CREATED,
        Json(serde_json::json!({ "success": true })),
    )
}

/// Update an Ollama endpoint (by index)
#[derive(Deserialize)]
pub struct UpdateEndpointRequest {
    name: String,
    url: String,
    model: String,
    enabled: bool,
}

pub async fn update_endpoint(
    State(state): State<Arc<AppState>>,
    Path(index): Path<usize>,
    Json(req): Json<UpdateEndpointRequest>,
) -> impl IntoResponse {
    let mut config = state.config.write().await;

    if index >= config.endpoints.len() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "Endpoint not found" })),
        )
            .into_response();
    }

    config.endpoints[index] = OllamaEndpoint {
        name: req.name,
        url: req.url,
        model: req.model,
        enabled: req.enabled,
    };

    info!("Updated Ollama endpoint at index {}", index);
    (StatusCode::OK, Json(serde_json::json!({ "success": true }))).into_response()
}

/// Delete an Ollama endpoint (by index)
pub async fn delete_endpoint(
    State(state): State<Arc<AppState>>,
    Path(index): Path<usize>,
) -> impl IntoResponse {
    let mut config = state.config.write().await;

    if index >= config.endpoints.len() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "Endpoint not found" })),
        )
            .into_response();
    }

    config.endpoints.remove(index);

    info!("Deleted Ollama endpoint at index {}", index);
    (StatusCode::OK, Json(serde_json::json!({ "success": true }))).into_response()
}

/// API: Get all Ollama endpoints
pub async fn api_endpoints(State(state): State<Arc<AppState>>) -> Json<Vec<OllamaEndpoint>> {
    let config = state.config.read().await;
    Json(config.endpoints.clone())
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

/// API: Test Ollama connection
#[derive(Deserialize)]
pub struct TestOllamaRequest {
    url: String,
}

#[derive(Serialize)]
pub struct TestOllamaResponse {
    success: bool,
    model_count: Option<usize>,
    error: Option<String>,
}

pub async fn api_test_ollama(Json(req): Json<TestOllamaRequest>) -> Json<TestOllamaResponse> {
    let client = OllamaClient::new(&req.url, "");

    if !client.is_available().await {
        return Json(TestOllamaResponse {
            success: false,
            model_count: None,
            error: Some("Cannot connect to Ollama at the specified URL".to_string()),
        });
    }

    match client.list_models().await {
        Ok(models) => Json(TestOllamaResponse {
            success: true,
            model_count: Some(models.len()),
            error: None,
        }),
        Err(e) => Json(TestOllamaResponse {
            success: false,
            model_count: None,
            error: Some(e.to_string()),
        }),
    }
}

// ============================================================================
// Config Handlers
// ============================================================================

/// Get current config as JSON
#[derive(Serialize)]
pub struct ConfigResponse {
    pub start_hour: u8,
    pub end_hour: u8,
    pub check_interval_seconds: u64,
}

pub async fn api_get_config(State(state): State<Arc<AppState>>) -> Json<ConfigResponse> {
    let config = state.config.read().await;
    Json(ConfigResponse {
        start_hour: config.schedule.start_hour,
        end_hour: config.schedule.end_hour,
        check_interval_seconds: config.schedule.check_interval_seconds,
    })
}

/// Update config
#[derive(Deserialize)]
pub struct UpdateConfigRequest {
    pub start_hour: u8,
    pub end_hour: u8,
}

pub async fn api_update_config(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UpdateConfigRequest>,
) -> impl IntoResponse {
    let start_hour = req.start_hour.min(23);
    let end_hour = req.end_hour.min(23);

    // Update in-memory config
    {
        let mut config = state.config.write().await;
        config.schedule.start_hour = start_hour;
        config.schedule.end_hour = end_hour;
    }

    // Update daemon's schedule
    {
        let daemon = state.daemon.read().await;
        let config = state.config.read().await;
        daemon.set_schedule(config.schedule.clone()).await;
    }

    info!(
        "Config updated: schedule = {:02}:00 - {:02}:00",
        start_hour, end_hour
    );

    (StatusCode::OK, Json(serde_json::json!({ "success": true })))
}

/// Save config to disk
pub async fn api_save_config(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let config = state.config.read().await;

    match config.save(None) {
        Ok(()) => {
            info!("Config saved to disk");
            (StatusCode::OK, Json(serde_json::json!({ "success": true }))).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// Reload config from disk
pub async fn api_reload_config(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match Config::load(None) {
        Ok(new_config) => {
            let schedule = new_config.schedule.clone();

            // Update in-memory config
            {
                let mut config = state.config.write().await;
                *config = new_config;
            }

            // Update daemon's schedule
            {
                let daemon = state.daemon.read().await;
                daemon.set_schedule(schedule).await;
            }

            info!("Config reloaded from disk");
            (StatusCode::OK, Json(serde_json::json!({ "success": true }))).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

// ============================================================================
// Scan Trigger Handler
// ============================================================================

/// API: Trigger an immediate scan
pub async fn api_trigger_scan(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let daemon = state.daemon.read().await;
    daemon.trigger_scan();
    info!("Scan triggered via API");
    (
        StatusCode::OK,
        Json(serde_json::json!({ "success": true, "message": "Scan triggered" })),
    )
}
