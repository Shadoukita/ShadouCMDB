//! HTTP server: routing, middleware and graceful shutdown.
//!
//! Layers: `http` (transport concerns, this module) -> `api` (route table,
//! validation, OpenAPI) -> `modules` (routes and services) -> `data` (SQL).
//! Auth, RBAC and other future modules plug in as a different
//! [`ActorResolver`], extra routes in `api::routes` and middleware here.

pub mod error;
pub mod request_id;
mod ui;

use std::any::Any;
use std::future::Future;
use std::time::Duration;

use axum::Router;
use axum::extract::{DefaultBodyLimit, Request};
use axum::http::{HeaderName, HeaderValue, Method};
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

use sqlx::PgPool;
use tokio::net::TcpListener;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::api;
use crate::api::context::{ActorResolver, AnonymousActorResolver};
use crate::config::Config;
use crate::db;
use error::AppError;

/// Shared by every handler.
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    /// Who is calling; the authentication seam.
    pub actors: Arc<dyn ActorResolver>,
}

impl AppState {
    /// Milestone 1: no authentication.
    pub fn new(pool: PgPool) -> Self {
        AppState { pool, actors: Arc::new(AnonymousActorResolver) }
    }
}

/// Unknown routes: the embedded UI for browser paths, the error envelope for
/// everything else (and always for /api/*, so API clients never get HTML).
async fn fallback(req: Request) -> Response {
    let path = req.uri().path();
    let is_read = req.method() == Method::GET || req.method() == Method::HEAD;
    if is_read
        && !path.starts_with("/api/")
        && path != "/api"
        && let Some(res) = ui::serve(path)
    {
        return res;
    }
    AppError::not_found(format!("Route {} {} does not exist", req.method(), path)).into_response()
}

fn panic_response(_: Box<dyn Any + Send + 'static>) -> Response {
    tracing::error!("handler panicked");
    AppError::internal().into_response()
}

pub fn router(state: AppState, cfg: &Config) -> Router {
    let mut app = api::router()
        .fallback(fallback)
        .method_not_allowed_fallback(fallback)
        .with_state(state)
        .layer(CatchPanicLayer::custom(panic_response))
        .layer(DefaultBodyLimit::max(1024 * 1024));

    if !cfg.cors_origins.is_empty() {
        let origins: Vec<HeaderValue> = cfg.cors_origins.iter().filter_map(|o| HeaderValue::from_str(o).ok()).collect();
        app = app.layer(
            CorsLayer::new()
                .allow_origin(AllowOrigin::list(origins))
                .allow_methods([Method::GET, Method::POST, Method::PATCH, Method::DELETE])
                .allow_headers([
                    axum::http::header::CONTENT_TYPE,
                    HeaderName::from_static(request_id::HEADER),
                    HeaderName::from_static("x-actor-name"),
                ])
                .expose_headers([HeaderName::from_static(request_id::HEADER)]),
        );
    }

    // Outermost, so every response (including 404s, panics and CORS preflights) carries the id.
    app.layer(axum::middleware::from_fn(request_id::middleware))
}

/// Runs the server until `shutdown` resolves, then drains in-flight requests
/// and closes the pool.
pub async fn serve(cfg: Config, shutdown: impl Future<Output = ()> + Send + 'static) -> anyhow::Result<()> {
    let pool = db::lazy_pool(&cfg.database)?;
    let app = router(AppState::new(pool.clone()), &cfg);

    let listener = TcpListener::bind((cfg.api_host.as_str(), cfg.api_port))
        .await
        .map_err(|e| anyhow::anyhow!("cannot listen on {}:{}: {e}", cfg.api_host, cfg.api_port))?;
    tracing::info!(
        address = %listener.local_addr()?,
        version = env!("CARGO_PKG_VERSION"),
        ui = ui::available(),
        migrations = db::expected_count(),
        ssl = cfg.database.ssl.as_str(),
        "server listening"
    );

    axum::serve(listener, app).with_graceful_shutdown(shutdown).await?;
    tracing::info!("draining complete, closing database pool");
    // Do not let a wedged connection hold up process exit.
    let _ = tokio::time::timeout(Duration::from_secs(5), pool.close()).await;
    tracing::info!("server stopped");
    Ok(())
}

/// Resolves on Ctrl+C, or SIGTERM on Unix (systemd, Docker, Kubernetes).
pub async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received");
}
