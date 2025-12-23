mod handlers;
mod templates;

use crate::AppState;
use axum::{
    routing::{get, post},
    Router,
};
use std::sync::Arc;
use tower_http::services::ServeDir;
use tracing::info;

/// Start the web server
pub async fn start_server(state: Arc<AppState>, port: u16) -> anyhow::Result<()> {
    let app = Router::new()
        // Dashboard
        .route("/", get(handlers::dashboard))
        // Repositories
        .route("/repositories", get(handlers::list_repositories))
        .route("/repositories", post(handlers::add_repository))
        // Results
        .route("/results", get(handlers::list_results))
        // API endpoints
        .route("/api/status", get(handlers::api_status))
        .route("/api/repositories", get(handlers::api_repositories))
        .route("/api/results", get(handlers::api_results))
        // Static files
        .nest_service("/static", ServeDir::new("static"))
        // State
        .with_state(state);

    let addr = format!("127.0.0.1:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    info!("Web server listening on http://{}", addr);

    axum::serve(listener, app).await?;

    Ok(())
}
