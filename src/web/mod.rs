//! Web server and dashboard for Noctum.
//!
//! Provides an Axum-based HTTP server with HTML pages for viewing results
//! and a JSON API for configuration and triggering scans.

mod handlers;
mod templates;

use crate::AppState;
use axum::{
    body::Body,
    extract::Request,
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Router,
};
use rust_embed::Embed;
use std::sync::Arc;

#[derive(Embed)]
#[folder = "static/"]
struct StaticAssets;

/// Serve embedded static files.
async fn serve_static(axum::extract::Path(path): axum::extract::Path<String>) -> impl IntoResponse {
    let mime_type = mime_guess::from_path(&path)
        .first_raw()
        .unwrap_or("application/octet-stream");

    match StaticAssets::get(&path) {
        Some(content) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime_type)
            .header(header::CACHE_CONTROL, "public, max-age=31536000")
            .body(Body::from(content.data.into_owned()))
            .unwrap(),
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("Not found"))
            .unwrap(),
    }
}

/// Middleware to validate Host header against DNS rebinding attacks.
///
/// Only allows requests where the Host header matches localhost variants
/// or the configured bind address.
async fn validate_host(request: Request, next: Next) -> Result<Response, StatusCode> {
    let host = request
        .headers()
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");

    // Strip port if present
    let host_without_port = host.split(':').next().unwrap_or(host);

    // Allow localhost variants and 127.0.0.1
    let allowed = matches!(
        host_without_port,
        "localhost" | "127.0.0.1" | "::1" | "[::1]"
    );

    if allowed {
        Ok(next.run(request).await)
    } else {
        tracing::warn!("Rejected request with invalid Host header: {}", host);
        Err(StatusCode::FORBIDDEN)
    }
}

/// Start the web server
pub async fn start_server(state: Arc<AppState>, host: &str, port: u16) -> anyhow::Result<()> {
    // Only enforce host validation when binding to localhost
    let is_localhost = matches!(host, "127.0.0.1" | "localhost" | "::1");

    let app = Router::new()
        // Repositories (default page)
        .route("/", get(handlers::list_repositories))
        .route("/repositories", get(handlers::list_repositories))
        .route("/repositories", post(handlers::add_repository))
        .route("/repositories/:id", delete(handlers::delete_repository))
        .route(
            "/repositories/:id/results",
            get(handlers::repository_results),
        )
        .route(
            "/repositories/:id/architecture",
            get(handlers::repository_architecture),
        )
        .route("/repositories/:id/files", get(handlers::repository_files))
        .route(
            "/repositories/:id/mutations",
            get(handlers::mutation_results),
        )
        .route(
            "/repositories/:id/diagrams",
            get(handlers::repository_diagrams),
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
        // Mutations API
        .route(
            "/api/repositories/:id/mutations/survived",
            get(handlers::api_survived_mutations),
        )
        // Static files (embedded in binary)
        .route("/static/*path", get(serve_static))
        // State
        .with_state(state);

    // Apply host validation middleware only for localhost bindings
    let app = if is_localhost {
        app.layer(middleware::from_fn(validate_host))
    } else {
        app
    };

    let addr = format!("{}:{}", host, port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    tracing::info!("Web server listening on http://{}", addr);

    axum::serve(listener, app).await?;

    Ok(())
}
