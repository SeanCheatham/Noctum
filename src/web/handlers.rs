//! Dashboard pages and the small JSON API the pages call.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use axum::Json;
use serde::Deserialize;
use serde::Serialize;

use crate::config::Config;
use crate::lockfile::{self, AssetState};
use crate::web::templates::{
    BaselineRow, HomeTemplate, RepoRow, RepositoryTemplate, SettingsTemplate, TrialRow,
    TrialTemplate,
};
use crate::AppState;

pub async fn home(State(state): State<Arc<AppState>>) -> Response {
    let daemon = state.db.get_daemon().await.ok();
    let config = state.config.read().await.clone();
    let repos = state.db.get_repositories().await.unwrap_or_default();
    let page = HomeTemplate {
        phase: daemon
            .as_ref()
            .map(|d| d.phase.clone())
            .unwrap_or_else(|| "idle".into()),
        detail: daemon.and_then(|d| d.detail).unwrap_or_default(),
        window: format!(
            "{}:00–{}:00",
            config.schedule.start_hour, config.schedule.end_hour
        ),
        model: format!("{:?}", lockfile::model_stamp(&config.data_dir())),
        qemu: format!("{:?}", lockfile::qemu_stamp(&config.data_dir())),
        repositories: repos
            .into_iter()
            .map(|repo| RepoRow {
                id: repo.id,
                name: repo.name,
                path: repo.path,
                enabled: repo.enabled,
            })
            .collect(),
    };
    render(page)
}

pub async fn repository(State(state): State<Arc<AppState>>, Path(id): Path<i64>) -> Response {
    let Some(repo) = state.db.get_repository(id).await.ok().flatten() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let run = state.db.latest_run(id).await.ok().flatten();
    let baselines = match &run {
        Some(run) => state.db.baselines_for_run(run.id).await.unwrap_or_default(),
        None => Vec::new(),
    };
    let trials = match &run {
        Some(run) => state.db.trials_for_run(run.id).await.unwrap_or_default(),
        None => Vec::new(),
    };
    let delta = match &run {
        Some(run) => state.db.accepted_delta(run.id).await.unwrap_or(0),
        None => 0,
    };
    let page = RepositoryTemplate {
        name: repo.name,
        path: repo.path,
        git_sha: run
            .as_ref()
            .map(|run| run.git_sha.clone())
            .unwrap_or_default(),
        perf_sha: run
            .as_ref()
            .and_then(|run| run.perf_sha.clone())
            .unwrap_or_default(),
        run_status: run
            .as_ref()
            .map(|run| run.status.clone())
            .unwrap_or_else(|| "none".into()),
        run_error: run.and_then(|run| run.error).unwrap_or_default(),
        delta: delta.to_string(),
        baselines: baselines
            .into_iter()
            .map(|row| BaselineRow {
                test_name: row.test_name,
                status: row.status,
                insn_count: row.insn_count.map(|n| n.to_string()).unwrap_or_default(),
                reason: row.reason.unwrap_or_default(),
            })
            .collect(),
        trials: trials
            .into_iter()
            .map(|row| TrialRow {
                id: row.id,
                test_name: row.test_name,
                status: row.status,
                summary: row.model_summary.unwrap_or_default(),
                ratio: row
                    .improvement_ratio
                    .map(|ratio| format!("{ratio:.4}"))
                    .unwrap_or_default(),
            })
            .collect(),
    };
    render(page)
}

pub async fn trial(State(state): State<Arc<AppState>>, Path(id): Path<i64>) -> Response {
    let Some(trial) = state.db.trial(id).await.ok().flatten() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let page = TrialTemplate {
        test_name: trial.test_name,
        symbol: trial.symbol,
        status: trial.status,
        reason: trial.reason.unwrap_or_default(),
        summary: trial.model_summary.unwrap_or_default(),
        baseline: trial
            .baseline_count
            .map(|n| n.to_string())
            .unwrap_or_default(),
        candidate: trial.insn_count.map(|n| n.to_string()).unwrap_or_default(),
        ratio: trial
            .improvement_ratio
            .map(|ratio| format!("{ratio:.4}"))
            .unwrap_or_default(),
        diff: trial.unified_diff.unwrap_or_default(),
        tool_output: trial.tool_output.unwrap_or_default(),
    };
    render(page)
}

pub async fn settings(State(state): State<Arc<AppState>>) -> Response {
    let config = state.config.read().await.clone();
    render(settings_page(&config, false))
}

pub async fn save_settings(
    State(state): State<Arc<AppState>>,
    Form(form): Form<SettingsForm>,
) -> Response {
    let mut config = state.config.read().await.clone();
    config.schedule.start_hour = form.start_hour;
    config.schedule.end_hour = form.end_hour;
    config.schedule.check_interval_seconds = form.check_interval_seconds;
    config.gate.min_improvement_ratio = form.min_improvement_ratio;
    config.gate.min_insn_delta = form.min_insn_delta;
    config.gate.max_candidates = form.max_candidates;
    config.gate.max_tests = form.max_tests;
    config.gate.max_scan = form.max_scan;
    config.web.host = form.host;
    config.web.port = form.port;
    if config.validate().is_err() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if let Err(err) = config.save(state.config_path.as_deref()) {
        tracing::warn!("could not save config: {err}");
    }
    *state.config.write().await = config.clone();
    render(settings_page(&config, true))
}

fn settings_page(config: &Config, saved: bool) -> SettingsTemplate {
    let model = lockfile::ModelLock::load().ok();
    let qemu = lockfile::QemuLock::load().ok();
    SettingsTemplate {
        start_hour: config.schedule.start_hour,
        end_hour: config.schedule.end_hour,
        check_interval_seconds: config.schedule.check_interval_seconds,
        min_improvement_ratio: config.gate.min_improvement_ratio,
        min_insn_delta: config.gate.min_insn_delta,
        max_candidates: config.gate.max_candidates,
        max_tests: config.gate.max_tests,
        max_scan: config.gate.max_scan,
        host: config.web.host.clone(),
        port: config.web.port,
        model_lock: model
            .map(|lock| format!("{} {}", lock.llama_cpp_tag, lock.gguf_filename))
            .unwrap_or_default(),
        qemu_lock: qemu
            .map(|lock| {
                if lock.published() {
                    lock.id
                } else if !lock.sha256.is_empty() {
                    format!("{} (pinned, no download URL)", lock.id)
                } else {
                    format!("{} (not published; run qemu/build-bundle.sh)", lock.id)
                }
            })
            .unwrap_or_default(),
        saved,
    }
}

#[derive(Deserialize)]
pub struct SettingsForm {
    pub start_hour: u8,
    pub end_hour: u8,
    pub check_interval_seconds: u64,
    pub min_improvement_ratio: f64,
    pub min_insn_delta: u64,
    pub max_candidates: u32,
    pub max_tests: u32,
    pub max_scan: u32,
    pub host: String,
    pub port: u16,
}

#[derive(Deserialize)]
pub struct AddRepoForm {
    pub name: String,
    pub path: String,
}

pub async fn add_repository(
    State(state): State<Arc<AppState>>,
    Form(form): Form<AddRepoForm>,
) -> impl IntoResponse {
    if form.name.trim().is_empty() || form.path.trim().is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if state
        .db
        .add_repository(form.path.trim(), form.name.trim())
        .await
        .is_err()
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    Redirect::to("/").into_response()
}

#[derive(Deserialize)]
pub struct EnabledForm {
    pub enabled: Option<String>,
}

pub async fn set_enabled(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Form(form): Form<EnabledForm>,
) -> impl IntoResponse {
    let enabled = matches!(
        form.enabled.as_deref(),
        Some("on") | Some("true") | Some("1")
    );
    let _ = state.db.set_enabled(id, enabled).await;
    Redirect::to("/")
}

pub async fn delete_repository(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let _ = state.db.delete_repository(id).await;
    Redirect::to("/")
}

pub async fn run_now(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    if state.daemon.trigger(None).is_err() {
        return StatusCode::CONFLICT.into_response();
    }
    Redirect::to("/").into_response()
}

#[derive(Serialize)]
pub struct StatusBody {
    pub phase: String,
    pub detail: Option<String>,
    pub model: String,
    pub qemu: String,
}

pub async fn api_status(State(state): State<Arc<AppState>>) -> Json<StatusBody> {
    let daemon = state.db.get_daemon().await.ok();
    let config = state.config.read().await.clone();
    Json(StatusBody {
        phase: daemon
            .as_ref()
            .map(|d| d.phase.clone())
            .unwrap_or_else(|| "idle".into()),
        detail: daemon.and_then(|d| d.detail),
        model: stamp_name(lockfile::model_stamp(&config.data_dir())),
        qemu: stamp_name(lockfile::qemu_stamp(&config.data_dir())),
    })
}

pub async fn api_repositories(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match state.db.get_repositories().await {
        Ok(repos) => Json(repos).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub async fn api_config(State(state): State<Arc<AppState>>) -> Json<Config> {
    Json(state.config.read().await.clone())
}

fn stamp_name(state: AssetState) -> String {
    match state {
        AssetState::Missing => "missing",
        AssetState::Present => "present",
        AssetState::Mismatch => "mismatch",
        AssetState::Unverified => "unverified",
    }
    .into()
}

fn render<T: askama::Template>(page: T) -> Response {
    match page.render() {
        Ok(html) => (
            [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
            html,
        )
            .into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}
