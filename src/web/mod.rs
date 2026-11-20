//! Local dashboard. It binds to loopback and checks the Host header.

mod handlers;
mod templates;

use crate::AppState;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use rust_embed::Embed;
use std::sync::Arc;

#[derive(Embed)]
#[folder = "static/"]
struct StaticAssets;

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

async fn validate_host(request: Request, next: Next) -> Result<Response, StatusCode> {
    let host = request
        .headers()
        .get("host")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let host_without_port = host.split(':').next().unwrap_or(host);
    let allowed = matches!(
        host_without_port,
        "localhost" | "127.0.0.1" | "::1" | "[::1]"
    );
    if allowed {
        Ok(next.run(request).await)
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

pub async fn start_server(state: Arc<AppState>, host: &str, port: u16) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/", get(handlers::home))
        .route("/repositories", post(handlers::add_repository))
        .route("/repositories/:id", get(handlers::repository))
        .route("/repositories/:id/enabled", post(handlers::set_enabled))
        .route(
            "/repositories/:id/delete",
            post(handlers::delete_repository),
        )
        .route("/trials/:id", get(handlers::trial))
        .route(
            "/settings",
            get(handlers::settings).post(handlers::save_settings),
        )
        .route("/run", post(handlers::run_now))
        .route("/api/status", get(handlers::api_status))
        .route("/api/repositories", get(handlers::api_repositories))
        .route("/api/config", get(handlers::api_config))
        .route("/static/*path", get(serve_static))
        .layer(middleware::from_fn(validate_host))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(format!("{host}:{port}")).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
