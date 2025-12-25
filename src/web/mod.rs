mod handlers;
mod templates;

use crate::AppState;
use axum::{
    routing::{delete, get, post},
    Router,
};
use std::sync::Arc;
use tower_http::services::ServeDir;

/// Start the web server
pub async fn start_server(state: Arc<AppState>, port: u16) -> anyhow::Result<()> {
    let app = Router::new()
        // Repositories (default page)
        .route("/", get(handlers::list_repositories))
        .route("/repositories", get(handlers::list_repositories))
        .route("/repositories", post(handlers::add_repository))
        .route(
            "/repositories/:id/results",
            get(handlers::repository_results),
        )
        .route(
            "/repositories/:id/mutations",
            get(handlers::mutation_results),
        )
        // Settings / Endpoints
        .route("/settings", get(handlers::settings))
        .route("/endpoints", post(handlers::add_endpoint))
        .route("/endpoints/:id", post(handlers::update_endpoint))
        .route("/endpoints/:id", delete(handlers::delete_endpoint))
        // API endpoints
        .route("/api/status", get(handlers::api_status))
        .route("/api/repositories", get(handlers::api_repositories))
        .route("/api/results", get(handlers::api_results))
        .route("/api/endpoints", get(handlers::api_endpoints))
        .route("/api/test-ollama", post(handlers::api_test_ollama))
        // Config API
        .route("/api/config", get(handlers::api_get_config))
        .route("/api/config", post(handlers::api_update_config))
        .route("/api/config/save", post(handlers::api_save_config))
        .route("/api/config/reload", post(handlers::api_reload_config))
        // Scan API
        .route("/api/scan/trigger", post(handlers::api_trigger_scan))
        // Static files
        .nest_service("/static", ServeDir::new("static"))
        // State
        .with_state(state);

    let addr = format!("127.0.0.1:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    tracing::info!("Web server listening on http://{}", addr);

    axum::serve(listener, app).await?;

    Ok(())
}
