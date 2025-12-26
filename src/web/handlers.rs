//! HTTP request handlers for the web dashboard and API.
//!
//! HTML handlers render Askama templates for the browser UI.
//! API handlers return JSON for programmatic access and AJAX requests.

use crate::analyzer::OllamaClient;
use crate::config::{Config, OllamaEndpoint};
use crate::db::{AnalysisResult, Database, DaemonState, Repository};
use crate::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use std::path::Path as FilePath;
use std::sync::Arc;

use super::templates::{
    render_markdown, AnalysisResultView, MutationResultView, MutationResultsTemplate,
    RepositoriesTemplate, RepositoryResultsTemplate, SettingsTemplate,
};
use askama::Template;

fn render_template<T: Template>(template: T) -> Response {
    match template.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Template error: {}", e),
        )
            .into_response(),
    }
}

async fn get_repo_or_error(db: &Database, id: i64) -> Result<Repository, Response> {
    match db.get_repository(id).await {
        Ok(Some(repo)) => Ok(repo),
        Ok(None) => Err((StatusCode::NOT_FOUND, "Repository not found").into_response()),
        Err(e) => {
            tracing::error!("Database error fetching repository {}: {}", id, e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, "Database error").into_response())
        }
    }
}

pub async fn list_repositories(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let repositories = state.db.get_repositories().await.unwrap_or_default();
    render_template(RepositoriesTemplate { repositories })
}

#[derive(Deserialize, Serialize)]
pub struct AddRepositoryRequest {
    path: String,
    name: String,
}

pub async fn add_repository(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AddRepositoryRequest>,
) -> impl IntoResponse {
    // Validate path exists and is a directory
    let path = FilePath::new(&req.path);
    if !path.exists() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "Path does not exist" })),
        )
            .into_response();
    }
    if !path.is_dir() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "Path must be a directory" })),
        )
            .into_response();
    }

    match state.db.add_repository(&req.path, &req.name).await {
        Ok(id) => (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response(),
        Err(e) => {
            tracing::warn!("Failed to add repository: {}", e);
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "Failed to add repository" })),
            )
                .into_response()
        }
    }
}

pub async fn repository_results(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let repository = match get_repo_or_error(&state.db, id).await {
        Ok(repo) => repo,
        Err(response) => return response,
    };

    let all_results = state
        .db
        .get_all_repository_results(id)
        .await
        .unwrap_or_default();

    let mut architecture_summary = None;
    let mut file_results = Vec::new();

    for result in all_results {
        if result.analysis_type == "architecture_summary" {
            architecture_summary = Some(result);
        } else {
            file_results.push(result);
        }
    }

    let file_results: Vec<AnalysisResultView> = file_results
        .into_iter()
        .map(|r| AnalysisResultView::from_result(r, &repository.path))
        .collect();

    let architecture_summary_html = architecture_summary
        .as_ref()
        .map(|s| render_markdown(&s.result))
        .unwrap_or_default();

    render_template(RepositoryResultsTemplate {
        repository,
        architecture_summary,
        file_results,
        architecture_summary_html,
    })
}

pub async fn mutation_results(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let repository = match get_repo_or_error(&state.db, id).await {
        Ok(repo) => repo,
        Err(response) => return response,
    };

    let raw_results = state.db.get_mutation_results(id).await.unwrap_or_default();
    let summary = state.db.get_mutation_summary(id).await.unwrap_or_default();

    let results: Vec<MutationResultView> = raw_results
        .into_iter()
        .map(|r| MutationResultView::from_result(r, &repository.path))
        .collect();

    let mutation_score_percent = format!("{:.1}", summary.mutation_score() * 100.0);

    render_template(MutationResultsTemplate {
        repository,
        results,
        summary,
        mutation_score_percent,
    })
}

pub async fn settings(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let config = state.config.read().await;
    let endpoints = config.endpoints.clone();
    let start_hour = config.schedule.start_hour;
    let end_hour = config.schedule.end_hour;
    let config_path = Config::default_config_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(unknown)".to_string());

    render_template(SettingsTemplate {
        endpoints,
        start_hour,
        end_hour,
        config_path,
    })
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

    tracing::info!("Added new Ollama endpoint");
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

    tracing::info!("Updated Ollama endpoint at index {}", index);
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

    tracing::info!("Deleted Ollama endpoint at index {}", index);
    (StatusCode::OK, Json(serde_json::json!({ "success": true }))).into_response()
}

/// API: Get all Ollama endpoints
pub async fn api_endpoints(State(state): State<Arc<AppState>>) -> Json<Vec<OllamaEndpoint>> {
    let config = state.config.read().await;
    Json(config.endpoints.clone())
}

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

    tracing::info!(
        "Config updated: schedule = {:02}:00 - {:02}:00",
        start_hour,
        end_hour
    );

    (StatusCode::OK, Json(serde_json::json!({ "success": true })))
}

/// Save config to disk
pub async fn api_save_config(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let config = state.config.read().await;

    match config.save(None) {
        Ok(()) => {
            tracing::info!("Config saved to disk");
            (StatusCode::OK, Json(serde_json::json!({ "success": true }))).into_response()
        }
        Err(e) => {
            tracing::error!("Failed to save config: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "Failed to save configuration" })),
            )
                .into_response()
        }
    }
}

/// Reload config from disk
pub async fn api_reload_config(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match Config::load(None) {
        Ok(new_config) => {
            let schedule = new_config.schedule.clone();

            {
                let mut config = state.config.write().await;
                *config = new_config;
            }

            {
                let daemon = state.daemon.read().await;
                daemon.set_schedule(schedule).await;
            }

            tracing::info!("Config reloaded from disk");
            (StatusCode::OK, Json(serde_json::json!({ "success": true }))).into_response()
        }
        Err(e) => {
            tracing::error!("Failed to reload config: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "Failed to reload configuration" })),
            )
                .into_response()
        }
    }
}

/// API: Trigger an immediate scan
pub async fn api_trigger_scan(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let daemon = state.daemon.read().await;
    daemon.trigger_scan();
    tracing::info!("Scan triggered via API");
    (
        StatusCode::OK,
        Json(serde_json::json!({ "success": true, "message": "Scan triggered" })),
    )
}
