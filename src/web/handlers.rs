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
            .map(|row| {
                let (result, why) = trial_words(&row.status, row.reason.as_deref());
                TrialRow {
                    id: row.id,
                    symbol: row.symbol,
                    test_name: row.test_name,
                    before: count_text(row.baseline_count),
                    after: count_text(row.insn_count),
                    result,
                    why,
                    summary: row.model_summary.unwrap_or_default(),
                    ratio: row
                        .improvement_ratio
                        .map(|ratio| format!("{ratio:.4}"))
                        .unwrap_or_default(),
                }
            })
            .collect(),
    };
    render(page)
}

pub async fn trial(State(state): State<Arc<AppState>>, Path(id): Path<i64>) -> Response {
    let Some(trial) = state.db.trial(id).await.ok().flatten() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let (outcome, why) = trial_words(&trial.status, trial.reason.as_deref());
    let page = TrialTemplate {
        test_name: trial.test_name,
        symbol: trial.symbol,
        outcome,
        why,
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

/// Morning words for one trial. Counts stay numeric; this is only the label.
fn trial_words(status: &str, reason: Option<&str>) -> (String, String) {
    if status == "accepted" {
        return ("kept".to_string(), "kept".to_string());
    }
    let result = if status == "rejected" {
        "rejected".to_string()
    } else {
        status.to_string()
    };
    (result, morning_why(reason.unwrap_or("")))
}

fn morning_why(reason: &str) -> String {
    let reason = reason.trim();
    if reason.is_empty() {
        return String::new();
    }
    if reason == "no change" {
        return "no change".to_string();
    }
    if reason.contains("loop bound") {
        return "bound cheat".to_string();
    }
    if reason == "host check failed" {
        return "host failure".to_string();
    }
    if matches!(reason, "slower" | "below ratio" | "below absolute delta") {
        return "gate miss".to_string();
    }
    reason.to_string()
}

fn count_text(count: Option<i64>) -> String {
    count.map(|n| n.to_string()).unwrap_or_default()
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::to_bytes;
    use axum::extract::{Path, State};
    use axum::http::StatusCode;
    use axum::response::Response;
    use tokio::sync::RwLock;

    use super::{repository, trial, trial_words};
    use crate::config::Config;
    use crate::daemon::Daemon;
    use crate::db::Database;
    use crate::AppState;

    #[test]
    fn morning_words_name_the_night() {
        assert_eq!(
            trial_words("accepted", None),
            ("kept".to_string(), "kept".to_string())
        );
        assert_eq!(
            trial_words("rejected", Some("no change")),
            ("rejected".to_string(), "no change".to_string())
        );
        assert_eq!(
            trial_words(
                "rejected",
                Some("diff lowers a fixed loop bound in the measured test")
            ),
            ("rejected".to_string(), "bound cheat".to_string())
        );
        assert_eq!(
            trial_words("rejected", Some("host check failed")),
            ("rejected".to_string(), "host failure".to_string())
        );
        for reason in ["slower", "below ratio", "below absolute delta"] {
            assert_eq!(
                trial_words("rejected", Some(reason)),
                ("rejected".to_string(), "gate miss".to_string()),
                "{reason}"
            );
        }
        let assertion = "diff removes an assertion from the measured test";
        assert_eq!(
            trial_words("rejected", Some(assertion)),
            ("rejected".to_string(), assertion.to_string())
        );
        assert_eq!(
            trial_words("error", Some("llama-server died")),
            ("error".to_string(), "llama-server died".to_string())
        );
    }

    #[tokio::test]
    async fn repository_page_reads_candidate_counts_from_perf_db() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            data_dir: Some(dir.path().to_path_buf()),
            ..Config::default()
        };
        let db = Database::new(&config.database_path()).await.unwrap();
        db.run_migrations().await.unwrap();
        let repo_id = db
            .add_repository(dir.path().to_str().unwrap(), "demo")
            .await
            .unwrap();
        let run_id = db
            .insert_run(repo_id, "abc123", Some("demo"), "running", None)
            .await
            .unwrap();
        db.finish_run(run_id, "ok", None, Some("def456"))
            .await
            .unwrap();
        db.insert_baseline(
            run_id,
            "unittests src/lib.rs",
            "encode_batch",
            "measured",
            Some(120_034),
            None,
            None,
            Some(40),
            None,
        )
        .await
        .unwrap();

        let kept = insert_trial(
            &db,
            run_id,
            "encode",
            "encode_batch",
            "accepted",
            None,
            Some(120_034),
            Some(98_012),
            Some(0.1835),
            "shorter encode",
        )
        .await;
        insert_trial(
            &db,
            run_id,
            "parse",
            "parse_batch",
            "rejected",
            Some("no change"),
            Some(50_000),
            Some(50_000),
            Some(0.0),
            "same parse",
        )
        .await;
        let cheat = insert_trial(
            &db,
            run_id,
            "checksum",
            "checksum_batch",
            "rejected",
            Some("diff lowers a fixed loop bound in the measured test"),
            Some(80_000),
            None,
            None,
            "smaller loop",
        )
        .await;
        insert_trial(
            &db,
            run_id,
            "write_out",
            "write_batch",
            "rejected",
            Some("host check failed"),
            Some(70_000),
            None,
            None,
            "broke the suite",
        )
        .await;
        let missed = insert_trial(
            &db,
            run_id,
            "fold",
            "fold_batch",
            "rejected",
            Some("below ratio"),
            Some(2_000_000),
            Some(1_990_000),
            Some(0.005),
            "tiny fold",
        )
        .await;

        let state = app_state(db, config);
        let page = repository(State(state.clone()), Path(repo_id)).await;
        assert_eq!(page.status(), StatusCode::OK);
        let html = body(page).await;
        assert!(html.contains("encode</a>"), "{html}");
        assert!(html.contains(">120034<"), "{html}");
        assert!(html.contains(">98012<"), "{html}");
        assert!(html.contains(">kept<"), "{html}");
        assert!(html.contains(">no change<"), "{html}");
        assert!(html.contains(">bound cheat<"), "{html}");
        assert!(html.contains(">host failure<"), "{html}");
        assert!(html.contains(">gate miss<"), "{html}");
        assert!(html.contains(">50000<"), "{html}");
        assert!(html.contains(">70000<"), "{html}");
        assert!(html.contains(">2000000<"), "{html}");
        assert!(html.contains(">1990000<"), "{html}");
        assert!(
            html.contains(
                "<td>80000</td>\n            <td></td>\n            <td>rejected</td>\n            <td>bound cheat</td>"
            ),
            "{html}"
        );

        let kept_page = trial(State(state.clone()), Path(kept)).await;
        let kept_html = body(kept_page).await;
        assert!(kept_html.contains("Hot function encode"), "{kept_html}");
        assert!(kept_html.contains("<p>kept</p>"), "{kept_html}");
        assert!(kept_html.contains("Why kept"), "{kept_html}");
        assert!(
            kept_html.contains("Before 120034 instructions"),
            "{kept_html}"
        );
        assert!(
            kept_html.contains("After 98012 instructions"),
            "{kept_html}"
        );
        assert!(!kept_html.contains("Gate"), "{kept_html}");

        let cheat_page = trial(State(state.clone()), Path(cheat)).await;
        let cheat_html = body(cheat_page).await;
        assert!(cheat_html.contains("Why bound cheat"), "{cheat_html}");
        assert!(
            cheat_html.contains("Gate diff lowers a fixed loop bound in the measured test"),
            "{cheat_html}"
        );
        assert!(
            cheat_html.contains("Before 80000 instructions"),
            "{cheat_html}"
        );
        assert!(!cheat_html.contains("After"), "{cheat_html}");

        let miss_page = trial(State(state), Path(missed)).await;
        let miss_html = body(miss_page).await;
        assert!(miss_html.contains("Hot function fold"), "{miss_html}");
        assert!(miss_html.contains("Why gate miss"), "{miss_html}");
        assert!(miss_html.contains("Gate below ratio"), "{miss_html}");
        assert!(
            miss_html.contains("Before 2000000 instructions"),
            "{miss_html}"
        );
        assert!(
            miss_html.contains("After 1990000 instructions"),
            "{miss_html}"
        );
    }

    fn app_state(db: Database, config: Config) -> Arc<AppState> {
        let config = Arc::new(RwLock::new(config));
        let (_daemon, handle) = Daemon::new(db.clone(), config.clone());
        Arc::new(AppState {
            db,
            config,
            config_path: None,
            daemon: handle,
        })
    }

    async fn insert_trial(
        db: &Database,
        run_id: i64,
        symbol: &str,
        test_name: &str,
        status: &str,
        reason: Option<&str>,
        before: Option<i64>,
        after: Option<i64>,
        ratio: Option<f64>,
        summary: &str,
    ) -> i64 {
        let candidate = db
            .insert_candidate(
                run_id,
                test_name,
                "unittests src/lib.rs",
                0,
                symbol,
                "src/lib.rs",
                1,
                10,
                100,
                0.5,
                "blob",
            )
            .await
            .unwrap();
        db.insert_trial(
            candidate,
            status,
            reason,
            Some(summary),
            Some("diff --git a/src/lib.rs b/src/lib.rs\n"),
            before,
            after,
            ratio,
            status == "accepted",
            status == "accepted",
            None,
        )
        .await
        .unwrap()
    }

    async fn body(response: Response) -> String {
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }
}
