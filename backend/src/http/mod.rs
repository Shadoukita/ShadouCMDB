//! HTTP server: routing, middleware and graceful shutdown.
//!
//! Layers: `http` (transport concerns, this module) -> `api` (route table,
//! validation, access control, OpenAPI) -> `modules` (routes and services) ->
//! `data` (SQL). Sessions and permissions are resolved per route in
//! `api::route` (see [`crate::auth`]); future modules add routes in
//! `api::routes` and middleware here.

pub mod error;
pub mod request_id;
mod security_txt;
mod ui;

use std::any::Any;
use std::future::Future;
use std::time::Duration;

use axum::Router;
use axum::extract::{DefaultBodyLimit, Request};
use axum::http::{HeaderName, HeaderValue, Method, header};
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

use sqlx::PgPool;
use tokio::net::TcpListener;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::set_header::SetResponseHeaderLayer;

use crate::api;
use crate::auth::AuthState;
use crate::auth::session::{CSRF_HEADER, request_is_https};
use crate::config::{ApiDocs, AuditConfig, AuthConfig, Config, HttpConfig};
use crate::db;
use error::{AppError, ErrorCode};

/// Which client details the API records (sessions and sign-in events);
/// `AUDIT_CAPTURE_CLIENT_IP` and `AUDIT_CAPTURE_USER_AGENT`.
#[derive(Debug, Clone, Copy)]
pub struct ClientCapture {
    pub ip: bool,
    pub user_agent: bool,
}

/// Shared by every handler.
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    /// Session settings and the login backoff.
    pub auth: Arc<AuthState>,
    pub capture: ClientCapture,
    /// Whether every migration of this build is applied (see `schema_gate`).
    pub schema: Arc<db::SchemaState>,
    /// Whether the start-up step for encrypted secrets ran (see `schema_gate`).
    pub sealed: Arc<SealedState>,
    /// Requests the API handles at once (`HTTP_MAX_CONCURRENT_REQUESTS`).
    pub capacity: Capacity,
    /// The last `/readyz` check, reused briefly (GH#242).
    pub readiness: Arc<crate::modules::health::ReadinessCache>,
}

/// The start-up step for encrypted secrets ([`crate::secrets::sealed::prepare`]):
/// once the schema is current and before any API request is handled. `serve`
/// runs it before it listens; when the database was unreachable or not
/// migrated then, the first API request after that runs it.
#[derive(Default)]
pub struct SealedState {
    done: tokio::sync::OnceCell<()>,
    /// Rows under a key that is not configured: `serve` stops with this message.
    refused: std::sync::Mutex<Option<String>>,
    stop: tokio::sync::Notify,
}

impl AppState {
    pub fn new(pool: PgPool, auth: AuthConfig, keyring: Arc<crate::secrets::Keyring>) -> Self {
        AppState {
            pool,
            auth: Arc::new(AuthState::new(auth, keyring)),
            capture: ClientCapture { ip: true, user_agent: true },
            schema: Arc::default(),
            sealed: Arc::default(),
            capacity: Capacity::new(512, Duration::from_secs(10)),
            readiness: Arc::default(),
        }
    }

    /// Runs [`crate::secrets::sealed::prepare`] once; later calls return at once.
    async fn prepare_sealed(&self) -> Result<(), crate::secrets::sealed::PrepareError> {
        self.sealed
            .done
            .get_or_try_init(|| async {
                crate::secrets::sealed::prepare(&self.pool, &self.auth.keyring).await.map(|_| ())
            })
            .await
            .map(|_| ())
    }

    pub fn capturing(mut self, audit: &AuditConfig) -> Self {
        self.capture = ClientCapture { ip: audit.capture_client_ip, user_agent: audit.capture_user_agent };
        self
    }

    pub fn limited(mut self, http: &HttpConfig) -> Self {
        self.capacity = Capacity::new(http.max_concurrent_requests, http.header_read_timeout);
        self
    }
}

/// Bounds the API requests in progress; each route takes a permit in
/// `api::route` after it authorises the caller and before it reads the body,
/// so a rejected request never holds capacity, and a request that finds its
/// pool empty is answered 503 SERVER_BUSY instead of
/// queueing. Public routes (setup, sign-in, OIDC, branding) draw from their
/// own, smaller pool and must deliver any body within
/// `HTTP_HEADER_READ_TIMEOUT_SECS`: anonymous callers can then only saturate
/// the public routes, never the capacity signed-in users and API tokens need.
/// Only the health routes (liveness, readiness, version; `RouteBuilder::unlimited`)
/// take no permit, so a busy server is not mistaken for a dead one.
#[derive(Clone)]
pub struct Capacity {
    global: Arc<tokio::sync::Semaphore>,
    public: Arc<tokio::sync::Semaphore>,
    /// Time a public route may take to receive its body.
    pub public_body_timeout: Duration,
}

impl Capacity {
    /// `max` requests for authenticated routes, and `max / 8` (at least 16) for public ones.
    pub fn new(max: usize, public_body_timeout: Duration) -> Self {
        Capacity::with_sizes(max, (max / 8).max(16), public_body_timeout)
    }

    pub fn with_sizes(global: usize, public: usize, public_body_timeout: Duration) -> Self {
        Capacity {
            global: Arc::new(tokio::sync::Semaphore::new(global)),
            public: Arc::new(tokio::sync::Semaphore::new(public)),
            public_body_timeout,
        }
    }

    /// A permit from the public or the global pool, or 503 SERVER_BUSY.
    pub fn acquire(&self, public: bool) -> Result<tokio::sync::OwnedSemaphorePermit, AppError> {
        let pool = if public { &self.public } else { &self.global };
        pool.clone().try_acquire_owned().map_err(|_| {
            tracing::warn!(public, "request refused: HTTP_MAX_CONCURRENT_REQUESTS reached");
            let mut err =
                AppError::new(ErrorCode::ServerBusy, "The server is handling too many requests; retry shortly");
            err.retry_after = Some(1);
            err
        })
    }

    #[cfg(test)]
    pub fn available(&self, public: bool) -> usize {
        if public { self.public.available_permits() } else { self.global.available_permits() }
    }
}

fn not_migrated(applied: usize, expected: usize) -> String {
    format!(
        "The database schema is not migrated ({applied} of {expected} migrations applied). Run `shadoucmdb migrate`, \
         then retry."
    )
}

/// API requests against a database with pending migrations would each fail on
/// a missing table or column; answer 503 SCHEMA_NOT_MIGRATED instead, before
/// any handler runs. An unreachable database is answered here too, so the
/// request does not wait for a connection twice. Any other failure of the
/// check (e.g. no privileges) lets the request go on to the handler.
/// `/api/v1/version` needs no database and stays answerable, so an operator
/// can see which build (and how many migrations) is running before migrating.
async fn schema_gate(
    axum::extract::State(state): axum::extract::State<AppState>,
    req: Request,
    next: axum::middleware::Next,
) -> Response {
    let path = req.uri().path();
    if is_api_path(path) && path != "/api/v1/version" {
        match state.schema.check(&state.pool).await {
            Ok(db::SchemaCheck::Pending { applied, expected }) => {
                let message = not_migrated(applied, expected);
                return AppError::new(error::ErrorCode::SchemaNotMigrated, message).into_response();
            }
            Ok(db::SchemaCheck::Current) => {
                use crate::secrets::sealed::PrepareError;
                match state.prepare_sealed().await {
                    Ok(()) => {}
                    Err(PrepareError::Refused(message)) => {
                        // As at start-up: the server does not run with secrets it cannot read.
                        tracing::error!("{message}");
                        state.sealed.refused.lock().expect("not poisoned").get_or_insert(message);
                        state.sealed.stop.notify_one();
                        return AppError::internal().into_response();
                    }
                    Err(PrepareError::Database(err)) if api::pg_error::is_connection_error(&err) => {
                        return AppError::from(err).into_response();
                    }
                    // Retried on the next request; rows not encrypted yet still open.
                    Err(PrepareError::Database(err)) => {
                        tracing::warn!(error = %err, "cannot encrypt the stored secrets yet")
                    }
                }
            }
            Err(err) if api::pg_error::is_connection_error(&err) => return AppError::from(err).into_response(),
            _ => {}
        }
    }
    next.run(req).await
}

/// Logged once at startup, without holding it up: the server also starts
/// (and reports not-ready) while the database is unreachable.
async fn log_schema_state(state: AppState) {
    match state.schema.check(&state.pool).await {
        // A fresh install logs its setup token now; an installed one deletes a leftover token file.
        Ok(db::SchemaCheck::Current) => match crate::modules::auth::setup_required(&state.pool).await {
            Ok(true) => state.auth.setup.arm(),
            Ok(false) => state.auth.setup.disarm(),
            Err(err) => tracing::warn!(error = %err.message, "cannot check whether first-run setup is needed"),
        },
        Ok(db::SchemaCheck::Pending { applied, expected }) => tracing::warn!(
            applied,
            expected,
            "database schema is not migrated: run `shadoucmdb migrate`; until then API requests answer 503 \
             SCHEMA_NOT_MIGRATED"
        ),
        Err(err) => tracing::warn!(error = %err, "cannot check the migration state; see /readyz"),
    }
}

/// Paths the API owns: they get the error envelope from `fallback` and the
/// no-store cache policy from `security_headers`. Every authenticated route is
/// under `/api/`; `/healthz`, `/readyz`, `/openapi.json`, `/docs`,
/// `/.well-known/security.txt` and the UI are not. A module that declares a route outside `/api/` is outside this
/// gate, and its responses may be stored by a shared cache.
fn is_api_path(path: &str) -> bool {
    path.starts_with("/api/") || path == "/api"
}

/// Unknown routes: the embedded UI for browser paths, the error envelope for
/// everything else (and always for /api/*, so API clients never get HTML).
async fn fallback(req: Request) -> Response {
    let path = req.uri().path();
    let is_read = req.method() == Method::GET || req.method() == Method::HEAD;
    if is_read
        && !is_api_path(path)
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

/// Policy for HTML documents: the embedded web UI and Swagger UI at /docs.
/// Both load only same-origin files (the UI: `/config.js` and `/assets/*`;
/// Swagger UI: its vendored bundle and `swagger-initializer.js`) and have no
/// inline `<script>` or `<style>`, so neither `'unsafe-inline'` nor a nonce is
/// needed. Swagger UI runs with `BaseLayout` because the standalone top bar's
/// logo injects an inline `<style>`. `img-src data:` covers the UI's favicon
/// and Swagger UI's icons.
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
    connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; object-src 'none'; form-action 'self'";

const HSTS: &str = "max-age=31536000; includeSubDomains";

/// Powerful browser features nothing here uses, switched off for this origin
/// and anything it might embed. Clipboard stays at the browser default
/// (`self`): the UI copies API token secrets, recovery codes and the OIDC
/// redirect URI with `navigator.clipboard.writeText`.
const PERMISSIONS_POLICY: &str = "accelerometer=(), bluetooth=(), camera=(), display-capture=(), geolocation=(), \
    gyroscope=(), hid=(), magnetometer=(), microphone=(), midi=(), payment=(), serial=(), usb=()";

/// Responses under `/api/` carry one principal's data (`GET /api/v1/auth/me`
/// returns the username, permissions and the CSRF token) and are
/// authenticated by the session cookie. RFC 9111 §3.5 only keeps a shared
/// cache from storing responses to requests with `Authorization`, not with a
/// cookie, and a 200 without freshness information is heuristically cacheable
/// (§4.2.2). `no-store` stops a compliant intermediary from storing them;
/// `Vary: Cookie` splits the cache key for one that stores anyway.
const API_CACHE_CONTROL: &str = "no-store";

/// The `Content-Security-Policy` and `Reporting-Endpoints` values, built
/// once at startup from `CSP_REPORT_URI`; requests only clone them.
///
/// A cross-origin report URI needs no `connect-src` entry: violation reports
/// are sent by the browser, not fetched by the page, and the page's CSP does
/// not apply to them. Do not widen `connect-src` for a report collector.
///
/// Browsers that implement `report-to` (current Chromium and Firefox) ignore
/// `report-uri` whenever `report-to` is present, and their Reporting API drops
/// endpoints that are not HTTPS. On a plain-HTTP install both directives would
/// therefore report nothing, so `report-to` is added only where it can deliver:
/// the request arrived over HTTPS and the endpoint is `https:` or a path on
/// this server. Everywhere else the policy carries `report-uri` alone.
#[derive(Clone)]
pub(crate) struct Csp {
    /// `report-uri` only (or no reporting at all when unset).
    plain: HeaderValue,
    /// `report-uri` + `report-to csp`, and the matching `Reporting-Endpoints`.
    reporting_api: Option<(HeaderValue, HeaderValue)>,
}

impl Csp {
    pub(crate) fn new(report_uri: Option<&str>) -> Self {
        let Some(uri) = report_uri else {
            return Csp { plain: HeaderValue::from_static(CSP), reporting_api: None };
        };
        // `config::parse_csp_report_uri` admits visible ASCII only, without `;`, `,`, `"` or `\`.
        let value = |s: String| HeaderValue::from_str(&s).expect("CSP_REPORT_URI is validated at startup");
        let plain = value(format!("{CSP}; report-uri {uri}"));
        let reporting_api = (uri.starts_with('/') || uri.starts_with("https:"))
            .then(|| (value(format!("{CSP}; report-uri {uri}; report-to csp")), value(format!("csp=\"{uri}\""))));
        Csp { plain, reporting_api }
    }
}

/// Headers that depend on the request or on the response type:
/// - `Strict-Transport-Security` only when the request reached us (or the
///   proxy in front of us) over HTTPS; on plain HTTP it would strand lab and
///   LAN deployments on a scheme they cannot serve.
/// - `Content-Security-Policy` (and `Reporting-Endpoints`) only on HTML
///   documents, never on JSON.
/// - `Cache-Control: no-store` and `Vary: Cookie` on API responses, see
///   [`API_CACHE_CONTROL`].
async fn security_headers(
    axum::extract::State(csp): axum::extract::State<Arc<Csp>>,
    req: Request,
    next: axum::middleware::Next,
) -> Response {
    let https = request_is_https(req.headers());
    let is_api = is_api_path(req.uri().path());
    let mut res = next.run(req).await;
    let headers = res.headers_mut();
    if is_api {
        // Overrides any handler value on purpose: an API path that should be
        // cacheable needs an explicit exception here. Safe only because it is
        // gated on API paths; `ui::file_response` owns the UI's caching
        // (immutable `assets/*`), so widening the gate would break it.
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(API_CACHE_CONTROL));
        // append, not insert: CorsLayer runs inside this layer and has already
        // set its own `Vary: origin, ...`, which insert would drop.
        headers.append(header::VARY, HeaderValue::from_static("Cookie"));
    }
    if https {
        headers.insert(header::STRICT_TRANSPORT_SECURITY, HeaderValue::from_static(HSTS));
    }
    let is_html = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.trim_start().to_ascii_lowercase().starts_with("text/html"));
    if is_html {
        match &csp.reporting_api {
            Some((policy, endpoints)) if https => {
                headers.insert(header::CONTENT_SECURITY_POLICY, policy.clone());
                headers.insert(HeaderName::from_static("reporting-endpoints"), endpoints.clone());
            }
            _ => {
                headers.insert(header::CONTENT_SECURITY_POLICY, csp.plain.clone());
            }
        }
    }
    res
}

/// Security headers for every response. The UI is served same-origin with the
/// API by this router, so this is the only place they can be set; a reverse
/// proxy in front should not add its own copies (see docs/deployment.md).
///
/// CSP `frame-ancestors` only reaches HTML documents; `X-Frame-Options: DENY`
/// keeps every other response (API JSON, assets, uploaded logos) out of frames
/// too. `Cross-Origin-Opener-Policy: same-origin` cuts the `window.opener` link
/// to cross-origin pages. The layout editor's popup is same-origin with the
/// same policy, so it keeps its opener; sign-in with OIDC is a top-level
/// redirect, not a popup.
fn with_security_headers(app: Router, csp: Csp) -> Router {
    app.layer(axum::middleware::from_fn_with_state(Arc::new(csp), security_headers))
        .layer(SetResponseHeaderLayer::overriding(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")))
        .layer(SetResponseHeaderLayer::overriding(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer")))
        .layer(SetResponseHeaderLayer::overriding(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY")))
        .layer(SetResponseHeaderLayer::overriding(
            HeaderName::from_static("permissions-policy"),
            HeaderValue::from_static(PERMISSIONS_POLICY),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            HeaderName::from_static("cross-origin-opener-policy"),
            HeaderValue::from_static("same-origin"),
        ))
}

/// `API_DOCS=authenticated`: the contract and Swagger UI need a session, like `/api/v1/auth/me`.
async fn require_session(
    axum::extract::State(state): axum::extract::State<AppState>,
    req: Request,
    next: axum::middleware::Next,
) -> Response {
    let client = crate::api::context::ClientInfo::default();
    match crate::auth::authenticate(&state.pool, &state.auth.config, req.headers(), &client).await {
        Ok(Some(_)) => next.run(req).await,
        Ok(None) => AppError::new(ErrorCode::Unauthenticated, "Sign in to read the API documentation").into_response(),
        Err(e) => e.into_response(),
    }
}

async fn docs_disabled(req: Request) -> Response {
    AppError::not_found(format!(
        "Route {} {} does not exist (API documentation is off: API_DOCS)",
        req.method(),
        req.uri().path()
    ))
    .into_response()
}

/// `/openapi.json` and `/docs` as `API_DOCS` says. Off by default: the
/// contract lists every route and parameter, which helps an attacker map the
/// API; it is also committed as backend/openapi.json for developers.
fn docs(state: &AppState, mode: ApiDocs) -> Router<AppState> {
    match mode {
        ApiDocs::Public => api::docs_router(),
        ApiDocs::Authenticated => {
            api::docs_router().route_layer(axum::middleware::from_fn_with_state(state.clone(), require_session))
        }
        ApiDocs::Off => Router::new()
            .route("/openapi.json", axum::routing::any(docs_disabled))
            .route("/docs", axum::routing::any(docs_disabled))
            .route("/docs/{*rest}", axum::routing::any(docs_disabled)),
    }
}

/// Bounds the whole request (body upload included) until the response
/// starts. Dropping the handler rolls back its open transaction.
async fn request_timeout(
    axum::extract::State(limit): axum::extract::State<Duration>,
    req: Request,
    next: axum::middleware::Next,
) -> Response {
    match tokio::time::timeout(limit, next.run(req)).await {
        Ok(res) => res,
        Err(_) => {
            tracing::warn!(limit_secs = limit.as_secs(), "request timed out");
            AppError::new(
                ErrorCode::RequestTimeout,
                format!("The request was not completed within {} s", limit.as_secs()),
            )
            .into_response()
        }
    }
}

pub fn router(state: AppState, cfg: &Config) -> Router {
    let mut app = api::router()
        .route(security_txt::PATH, axum::routing::get(security_txt::handler))
        .merge(docs(&state, cfg.api_docs))
        .fallback(fallback)
        .method_not_allowed_fallback(fallback)
        // Matched routes only: an unknown path is a 404 whatever the database state.
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), schema_gate))
        .with_state(state)
        .layer(axum::middleware::from_fn_with_state(cfg.http.request_timeout, request_timeout))
        .layer(CatchPanicLayer::custom(panic_response))
        .layer(DefaultBodyLimit::max(api::route::BODY_LIMIT));

    if !cfg.cors_origins.is_empty() {
        let origins: Vec<HeaderValue> = cfg.cors_origins.iter().filter_map(|o| HeaderValue::from_str(o).ok()).collect();
        // Credentials: the session cookie travels with cross-origin requests from these origins only.
        app = app.layer(
            CorsLayer::new()
                .allow_origin(AllowOrigin::list(origins))
                .allow_credentials(true)
                .allow_methods([Method::GET, Method::POST, Method::PUT, Method::PATCH, Method::DELETE])
                .allow_headers([
                    axum::http::header::CONTENT_TYPE,
                    HeaderName::from_static(request_id::HEADER),
                    HeaderName::from_static(CSRF_HEADER),
                ])
                .expose_headers([HeaderName::from_static(request_id::HEADER)]),
        );
    }

    // Outermost, so every response (including 404s, panics and CORS preflights)
    // carries the id and the security headers.
    with_security_headers(app, Csp::new(cfg.csp_report_uri.as_deref()))
        .layer(axum::middleware::from_fn(request_id::middleware))
}

/// Runs the server until `shutdown` resolves, then drains in-flight requests
/// and closes the pool.
pub async fn serve(cfg: Config, shutdown: impl Future<Output = ()> + Send + 'static) -> anyhow::Result<()> {
    // No key, no server: checked before anything else, with no database needed.
    let keyring = Arc::new(crate::secrets::Keyring::load(&cfg.encryption)?);
    let pool = db::lazy_pool(&cfg.database)?;
    let state = AppState::new(pool.clone(), cfg.auth.clone(), keyring).capturing(&cfg.audit).limited(&cfg.http);
    // Before listening: rows under a key that is not configured stop the server
    // here, and rows not encrypted yet are encrypted. An unreachable or
    // unmigrated database defers this to the first API request (`schema_gate`).
    if let Ok(db::SchemaCheck::Current) = state.schema.check(&pool).await {
        use crate::secrets::sealed::PrepareError;
        match state.prepare_sealed().await {
            Ok(()) => {}
            Err(PrepareError::Refused(message)) => anyhow::bail!(message),
            Err(PrepareError::Database(err)) => tracing::warn!(
                error = %err,
                "cannot encrypt the stored secrets yet; retried on the first API request"
            ),
        }
    }
    let app = router(state.clone(), &cfg);
    let exporter = cfg.audit.export.clone().map(|export| crate::audit_export::spawn(pool.clone(), export));

    let listener = TcpListener::bind((cfg.api_host.as_str(), cfg.api_port))
        .await
        .map_err(|e| anyhow::anyhow!("cannot listen on {}:{}: {e}", cfg.api_host, cfg.api_port))?;
    tracing::info!(
        address = %listener.local_addr()?,
        version = env!("CARGO_PKG_VERSION"),
        ui = ui::available(),
        migrations = db::expected_count(),
        ssl = cfg.database.ssl.as_str(),
        api_docs = cfg.api_docs.as_str(),
        "server listening"
    );
    tokio::spawn(log_schema_state(state.clone()));

    let sealed = state.sealed.clone();
    let stop = {
        let sealed = sealed.clone();
        async move {
            tokio::select! {
                () = shutdown => {}
                () = sealed.stop.notified() => {}
            }
        }
    };
    accept_loop(listener, app, &cfg.http, stop).await;
    if let Some(exporter) = exporter {
        exporter.stop().await;
    }
    tracing::info!("draining complete, closing database pool");
    // Do not let a wedged connection hold up process exit.
    let _ = tokio::time::timeout(Duration::from_secs(5), pool.close()).await;
    if let Some(message) = sealed.refused.lock().expect("not poisoned").take() {
        anyhow::bail!(message);
    }
    tracing::info!("server stopped");
    Ok(())
}

/// Accepts connections until `shutdown`, then waits for open ones to finish
/// their current request. axum::serve sets no timer on hyper, which leaves
/// HTTP/1 header reads unbounded; this loop sets one.
async fn accept_loop(
    listener: TcpListener,
    app: Router,
    http: &crate::config::HttpConfig,
    shutdown: impl Future<Output = ()> + Send + 'static,
) {
    use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
    use hyper_util::server::conn::auto::Builder;
    use hyper_util::server::graceful::GracefulShutdown;
    use hyper_util::service::TowerToHyperService;
    use tower::ServiceExt;

    let mut builder = Builder::new(TokioExecutor::new());
    builder.http1().timer(TokioTimer::new()).header_read_timeout(http.header_read_timeout);
    builder
        .http2()
        .timer(TokioTimer::new())
        .enable_connect_protocol()
        .keep_alive_interval(Duration::from_secs(30))
        .keep_alive_timeout(Duration::from_secs(20));

    let graceful = GracefulShutdown::new();
    let mut shutdown = std::pin::pin!(shutdown);
    loop {
        let (stream, peer) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(conn) => conn,
                Err(e) => {
                    // EMFILE and friends: back off instead of spinning.
                    tracing::warn!(error = %e, "accept failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
            () = &mut shutdown => break,
        };
        let _ = stream.set_nodelay(true);
        // The peer address is the client IP of last resort for the audit trail (auth::session::client_ip).
        let service = app.clone().map_request(move |req: axum::http::Request<hyper::body::Incoming>| {
            let mut req = req.map(axum::body::Body::new);
            req.extensions_mut().insert(axum::extract::ConnectInfo(peer));
            req
        });
        let conn = builder.serve_connection_with_upgrades(TokioIo::new(stream), TowerToHyperService::new(service));
        let conn = graceful.watch(conn.into_owned());
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                tracing::debug!(error = %e, "connection closed with an error");
            }
        });
    }
    drop(listener);
    graceful.shutdown().await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::response::Html;
    use std::time::Duration;
    use tower::ServiceExt;

    use crate::config::CookieSecure;

    async fn get(app: Router, path: &str, extra: &[(&'static str, &'static str)]) -> Response {
        let mut req = Request::builder().uri(path);
        for (k, v) in extra {
            req = req.header(*k, *v);
        }
        app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap()
    }

    fn header(res: &Response, name: header::HeaderName) -> Option<&str> {
        res.headers().get(name).map(|v| v.to_str().unwrap())
    }

    /// The real router, with a pool that never connects (API routes answer 503 DATABASE_UNAVAILABLE).
    fn app() -> Router {
        app_reporting_to(None)
    }

    fn app_reporting_to(csp_report_uri: Option<&str>) -> Router {
        app_with(|cfg| cfg.csp_report_uri = csp_report_uri.map(str::to_owned))
    }

    fn app_with(configure: impl FnOnce(&mut Config)) -> Router {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(200))
            .connect_lazy("postgres://nobody@127.0.0.1:1/none")
            .unwrap();
        app_on(pool, configure)
    }

    fn app_on(pool: PgPool, configure: impl FnOnce(&mut Config)) -> Router {
        let auth = AuthConfig {
            session_idle: Duration::from_secs(60),
            session_max_age: Duration::from_secs(3600),
            cookie_secure: CookieSecure::Auto,
            public_url: None,
            oidc_allowed_hosts: None,
            setup_token: Some(crate::auth::setup_token::TEST_TOKEN.into()),
            setup_token_file: None,
        };
        let mut cfg = Config {
            api_host: "127.0.0.1".into(),
            api_port: 3000,
            cors_origins: Vec::new(),
            csp_report_uri: None,
            api_docs: ApiDocs::Public,
            http: crate::config::HttpConfig {
                header_read_timeout: Duration::from_secs(10),
                request_timeout: Duration::from_secs(120),
                max_concurrent_requests: 512,
            },
            database: crate::config::DatabaseConfig {
                url: Some("postgres://nobody@127.0.0.1:1/none".into()),
                host: None,
                port: 5432,
                database: None,
                user: None,
                password: None,
                ssl: crate::config::SslMode::Disable,
                ssl_ca_file: None,
                pool_max: 1,
                statement_timeout: Duration::ZERO,
                connect_timeout: Duration::from_secs(1),
                roles: Default::default(),
            },
            migration_url: None,
            maintenance_url: None,
            auth: auth.clone(),
            audit: AuditConfig::default(),
            encryption: Default::default(),
        };
        configure(&mut cfg);
        router(AppState::new(pool, auth, crate::secrets::Keyring::for_tests()), &cfg)
    }

    /// GH#43: `serve` before `migrate` answered every API call with 500 INTERNAL_ERROR.
    #[tokio::test]
    async fn api_answers_schema_not_migrated_until_migrate_runs() {
        let Some(db) = db::scratch::empty("api_answers_schema_not_migrated_until_migrate_runs").await else {
            return;
        };
        let app = app_on(db.pool.clone(), |_| {});

        let res = get(app.clone(), "/api/v1/setup", &[]).await;
        assert_eq!(res.status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(res.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["code"], "SCHEMA_NOT_MIGRATED");
        let expected = format!("(0 of {} migrations applied)", db::expected_count());
        assert!(body["error"]["message"].as_str().unwrap().contains(&expected), "{body}");
        // Probes and unknown routes stay outside the gate.
        assert_eq!(get(app.clone(), "/healthz", &[]).await.status(), axum::http::StatusCode::OK);
        assert_eq!(get(app.clone(), "/api/v1/nope", &[]).await.status(), axum::http::StatusCode::NOT_FOUND);

        // Migrating under a running server takes effect without a restart.
        db::MIGRATOR.run(&db.pool).await.unwrap();
        assert_eq!(get(app, "/api/v1/setup", &[]).await.status(), axum::http::StatusCode::OK);
        db.drop().await;
    }

    /// Every `Vary` entry, across however many `Vary` field lines there are.
    fn vary(res: &Response) -> Vec<String> {
        res.headers()
            .get_all(header::VARY)
            .iter()
            .flat_map(|v| v.to_str().unwrap().split(','))
            .map(|v| v.trim().to_ascii_lowercase())
            .collect()
    }

    fn varies_on(res: &Response, name: &str) -> bool {
        vary(res).iter().any(|v| v == name)
    }

    fn assert_not_stored(res: &Response) {
        assert_eq!(header(res, header::CACHE_CONTROL), Some("no-store"));
        assert!(varies_on(res, "cookie"), "{:?}", vary(res));
    }

    #[tokio::test]
    async fn api_responses_are_not_stored_by_shared_caches() {
        // Carries the user, their permissions and the CSRF token. The pool is dead, so this is an
        // error rather than a 200; the headers are path-based and do not depend on the status.
        assert_not_stored(&get(app(), "/api/v1/auth/me", &[]).await);
        // The bare prefix gets the JSON envelope from `fallback`, so it gets the same headers.
        assert_not_stored(&get(app(), "/api", &[]).await);
    }

    #[tokio::test]
    async fn api_error_responses_are_not_stored() {
        let res = get(app(), "/api/v1/nope", &[]).await;
        assert_eq!(res.status(), 404);
        assert_not_stored(&res);
    }

    #[tokio::test]
    async fn cors_vary_survives() {
        let app = app_with(|cfg| cfg.cors_origins = vec!["https://ui.example.com".into()]);
        let res = get(app, "/api/v1/nope", &[("origin", "https://ui.example.com")]).await;
        assert_eq!(header(&res, header::ACCESS_CONTROL_ALLOW_ORIGIN), Some("https://ui.example.com"));
        assert_not_stored(&res);
        assert!(varies_on(&res, "origin"), "CORS Vary dropped: {:?}", vary(&res));
    }

    /// Stands in for `ui::file_response`, which only exists in builds with frontend/dist.
    fn ui_file(cache: &'static str) -> Router {
        Router::new().fallback(move || async move { ([(header::CACHE_CONTROL, cache)], "ui file") })
    }

    /// Sent on every response, whatever its path or type (GH#192): framing protection cannot rely on
    /// CSP `frame-ancestors`, which only HTML documents carry.
    fn assert_baseline_headers(res: &Response) {
        assert_eq!(header(res, header::X_CONTENT_TYPE_OPTIONS), Some("nosniff"));
        assert_eq!(header(res, header::REFERRER_POLICY), Some("no-referrer"));
        assert_eq!(header(res, header::X_FRAME_OPTIONS), Some("DENY"));
        assert_eq!(header(res, header::HeaderName::from_static("cross-origin-opener-policy")), Some("same-origin"));
        let policy = header(res, header::HeaderName::from_static("permissions-policy")).unwrap();
        assert_eq!(policy, PERMISSIONS_POLICY);
        for feature in ["camera", "microphone", "geolocation", "payment", "usb"] {
            assert!(policy.split(", ").any(|d| d == format!("{feature}=()")), "{feature} not disabled: {policy}");
        }
        assert!(!policy.contains("clipboard"), "the UI copies secrets with navigator.clipboard: {policy}");
    }

    #[tokio::test]
    async fn immutable_ui_assets_keep_their_long_cache() {
        let immutable = "public, max-age=31536000, immutable";
        let res = get(with_security_headers(ui_file(immutable), Csp::new(None)), "/assets/index-abc123.js", &[]).await;
        assert_eq!(header(&res, header::CACHE_CONTROL), Some(immutable));
        assert_baseline_headers(&res);
        assert!(!varies_on(&res, "cookie"), "{:?}", vary(&res));
    }

    #[tokio::test]
    async fn spa_index_still_revalidates() {
        let res = get(with_security_headers(ui_file("no-cache"), Csp::new(None)), "/", &[]).await;
        assert_eq!(header(&res, header::CACHE_CONTROL), Some("no-cache"));
        assert!(!varies_on(&res, "cookie"), "{:?}", vary(&res));
    }

    #[tokio::test]
    async fn public_endpoints_are_left_alone() {
        for path in ["/healthz", "/openapi.json"] {
            let res = get(app(), path, &[]).await;
            assert_eq!(header(&res, header::CACHE_CONTROL), None, "{path}");
            assert!(vary(&res).is_empty(), "{path}: {:?}", vary(&res));
            assert_baseline_headers(&res);
        }
    }

    #[tokio::test]
    async fn security_txt_is_served_without_a_session() {
        let res = get(app(), "/.well-known/security.txt", &[]).await;
        assert_eq!(res.status(), 200);
        assert_eq!(header(&res, header::CONTENT_TYPE), Some("text/plain; charset=utf-8"));
        assert_baseline_headers(&res);
        let body = axum::body::to_bytes(res.into_body(), 64 * 1024).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(body.lines().any(|l| l.starts_with("Contact: ")), "{body}");
        assert!(body.lines().any(|l| l.starts_with("Expires: ")), "{body}");
    }

    #[tokio::test]
    async fn json_responses_get_the_baseline_headers_and_no_csp() {
        let res = get(app(), "/api/v1/no-such-route", &[]).await;
        assert_eq!(res.status(), 404);
        assert!(header(&res, header::CONTENT_TYPE).unwrap().starts_with("application/json"));
        assert_baseline_headers(&res);
        assert_eq!(header(&res, header::CONTENT_SECURITY_POLICY), None);
        assert_eq!(header(&res, header::STRICT_TRANSPORT_SECURITY), None, "no HSTS over plain HTTP");
    }

    #[tokio::test]
    async fn hsts_only_when_the_request_arrived_over_https() {
        let res = get(app(), "/api/v1/no-such-route", &[("x-forwarded-proto", "https")]).await;
        assert_eq!(header(&res, header::STRICT_TRANSPORT_SECURITY), Some(HSTS));
        let res = get(app(), "/api/v1/no-such-route", &[("forwarded", "for=10.0.0.1;proto=https")]).await;
        assert_eq!(header(&res, header::STRICT_TRANSPORT_SECURITY), Some(HSTS));
        let res = get(app(), "/api/v1/no-such-route", &[("x-forwarded-proto", "http")]).await;
        assert_eq!(header(&res, header::STRICT_TRANSPORT_SECURITY), None);
    }

    #[tokio::test]
    async fn ui_documents_get_the_strict_csp() {
        // The embedded UI only exists in builds with frontend/dist, so stand in for `ui::serve` here.
        let ui = with_security_headers(ui_document(), Csp::new(None));
        let res = get(ui, "/inventory", &[("x-forwarded-proto", "https")]).await;
        assert_eq!(header(&res, header::CONTENT_SECURITY_POLICY), Some(CSP));
        assert!(!CSP.contains("unsafe"));
        assert_baseline_headers(&res);
        assert_eq!(header(&res, header::STRICT_TRANSPORT_SECURITY), Some(HSTS));
        // Reporting is opt-in: without CSP_REPORT_URI the policy is exactly CSP and reports go nowhere.
        assert_eq!(header(&res, REPORTING_ENDPOINTS), None);
    }

    const REPORTING_ENDPOINTS: header::HeaderName = header::HeaderName::from_static("reporting-endpoints");

    fn ui_document() -> Router {
        Router::new().fallback(|| async { Html("<!doctype html><div id=app></div>") })
    }

    fn assert_base_policy_intact(csp: &str) {
        assert!(csp.starts_with(&format!("{CSP}; ")), "{csp}");
        assert!(!csp.contains("unsafe"), "{csp}");
    }

    #[tokio::test]
    async fn csp_report_uri_adds_both_reporting_mechanisms_over_https() {
        for uri in ["/csp-reports", "https://reports.example.com/csp"] {
            let ui = with_security_headers(ui_document(), Csp::new(Some(uri)));
            let res = get(ui, "/inventory", &[("x-forwarded-proto", "https")]).await;
            let csp = header(&res, header::CONTENT_SECURITY_POLICY).unwrap();
            assert_base_policy_intact(csp);
            assert!(csp.ends_with(&format!("; report-uri {uri}; report-to csp")), "{csp}");
            assert_eq!(header(&res, REPORTING_ENDPOINTS), Some(format!("csp=\"{uri}\"").as_str()));
        }
    }

    #[tokio::test]
    async fn csp_report_uri_alone_where_the_reporting_api_would_drop_reports() {
        // Plain-HTTP page, or an http: collector: browsers with report-to would then ignore report-uri
        // and deliver nothing, so only report-uri is sent.
        for (uri, extra) in [
            ("/csp-reports", &[][..]),
            ("https://reports.example.com/csp", &[][..]),
            ("http://10.0.0.5:8080/csp", &[("x-forwarded-proto", "https")][..]),
        ] {
            let ui = with_security_headers(ui_document(), Csp::new(Some(uri)));
            let res = get(ui, "/inventory", extra).await;
            let csp = header(&res, header::CONTENT_SECURITY_POLICY).unwrap();
            assert_base_policy_intact(csp);
            assert!(csp.ends_with(&format!("; report-uri {uri}")), "{csp}");
            assert!(!csp.contains("report-to"), "{csp}");
            assert_eq!(header(&res, REPORTING_ENDPOINTS), None);
        }
    }

    #[tokio::test]
    async fn csp_report_uri_leaves_json_responses_alone() {
        for proto in ["http", "https"] {
            let res =
                get(app_reporting_to(Some("/csp-reports")), "/api/v1/no-such-route", &[("x-forwarded-proto", proto)])
                    .await;
            assert!(header(&res, header::CONTENT_TYPE).unwrap().starts_with("application/json"));
            assert_eq!(header(&res, header::CONTENT_SECURITY_POLICY), None);
            assert_eq!(header(&res, REPORTING_ENDPOINTS), None);
        }
    }

    #[tokio::test]
    async fn swagger_ui_gets_the_same_csp() {
        let res = get(app(), "/docs/", &[]).await;
        assert_eq!(res.status(), 200);
        assert!(header(&res, header::CONTENT_TYPE).unwrap().starts_with("text/html"));
        assert_eq!(header(&res, header::CONTENT_SECURITY_POLICY), Some(CSP));
        assert_eq!(header(&res, header::STRICT_TRANSPORT_SECURITY), None);

        // StandaloneLayout's top-bar logo injects an inline <style> that this CSP blocks.
        let res = get(app(), "/docs/swagger-initializer.js", &[]).await;
        assert_eq!(res.status(), 200);
        let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let js = String::from_utf8(body.to_vec()).unwrap();
        assert!(js.contains(r#""layout": "BaseLayout""#), "{js}");
    }

    #[tokio::test]
    async fn api_docs_are_off_unless_enabled() {
        let off = || app_with(|cfg| cfg.api_docs = ApiDocs::Off);
        for path in ["/openapi.json", "/docs", "/docs/", "/docs/swagger-initializer.js"] {
            let res = get(off(), path, &[]).await;
            assert_eq!(res.status(), 404, "{path}");
            assert!(header(&res, header::CONTENT_TYPE).unwrap().starts_with("application/json"), "{path}");
        }
        assert_eq!(get(app(), "/openapi.json", &[]).await.status(), 200, "API_DOCS=public");
    }

    #[tokio::test]
    async fn api_docs_can_require_a_session() {
        let app = || app_with(|cfg| cfg.api_docs = ApiDocs::Authenticated);
        // No cookie: refused before any database lookup.
        for path in ["/openapi.json", "/docs/"] {
            let res = get(app(), path, &[]).await;
            assert_eq!(res.status(), 401, "{path}");
        }
    }

    #[tokio::test]
    async fn slow_requests_get_the_timeout_envelope() {
        let slow = Router::new().route(
            "/api/slow",
            axum::routing::get(|| async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                "late"
            }),
        );
        let app = slow.layer(axum::middleware::from_fn_with_state(Duration::from_millis(50), request_timeout));
        let res = get(app, "/api/slow", &[]).await;
        assert_eq!(res.status(), 408);
        let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"]["code"], "REQUEST_TIMEOUT");
    }

    #[tokio::test]
    async fn healthz_and_version_report_the_build() {
        for path in ["/healthz", "/api/v1/version"] {
            let res = get(app(), path, &[]).await;
            assert_eq!(res.status(), 200, "{path}");
            let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(v["version"], env!("CARGO_PKG_VERSION"), "{path}");
        }
    }

    /// A client that opens a connection and never finishes its headers is cut off.
    #[tokio::test]
    async fn header_read_timeout_closes_slow_connections() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let http = crate::config::HttpConfig {
            header_read_timeout: Duration::from_millis(200),
            request_timeout: Duration::from_secs(5),
            max_concurrent_requests: 512,
        };
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            accept_loop(listener, app(), &http, async {
                let _ = rx.await;
            })
            .await
        });
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n").await.unwrap();
        let mut buf = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(3), stream.read_to_end(&mut buf)).await;
        assert!(read.is_ok(), "connection still open after the header read timeout");
        let _ = tx.send(());
        server.await.unwrap();
    }
}
