//! HTTP server: routing, middleware and graceful shutdown.
//!
//! Layers: `http` (transport concerns, this module) -> `api` (route table,
//! validation, access control, OpenAPI) -> `modules` (routes and services) ->
//! `data` (SQL). Sessions and permissions are resolved per route in
//! `api::route` (see [`crate::auth`]); future modules add routes in
//! `api::routes` and middleware here.

pub mod error;
pub mod request_id;
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
use crate::config::{AuthConfig, Config};
use crate::db;
use error::AppError;

/// Shared by every handler.
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    /// Session settings and the login backoff.
    pub auth: Arc<AuthState>,
}

impl AppState {
    pub fn new(pool: PgPool, auth: AuthConfig) -> Self {
        AppState { pool, auth: Arc::new(AuthState::new(auth)) }
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
async fn security_headers(
    axum::extract::State(csp): axum::extract::State<Arc<Csp>>,
    req: Request,
    next: axum::middleware::Next,
) -> Response {
    let https = request_is_https(req.headers());
    let mut res = next.run(req).await;
    let headers = res.headers_mut();
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
fn with_security_headers(app: Router, csp: Csp) -> Router {
    app.layer(axum::middleware::from_fn_with_state(Arc::new(csp), security_headers))
        .layer(SetResponseHeaderLayer::overriding(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")))
        .layer(SetResponseHeaderLayer::overriding(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer")))
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
    let pool = db::lazy_pool(&cfg.database)?;
    let app = router(AppState::new(pool.clone(), cfg.auth.clone()), &cfg);

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

    /// The real router, with a pool that is never connected: the paths used here do not touch the database.
    fn app() -> Router {
        app_reporting_to(None)
    }

    fn app_reporting_to(csp_report_uri: Option<&str>) -> Router {
        let pool = PgPool::connect_lazy("postgres://nobody@127.0.0.1:1/none").unwrap();
        let auth = AuthConfig {
            session_idle: Duration::from_secs(60),
            session_max_age: Duration::from_secs(3600),
            cookie_secure: CookieSecure::Auto,
        };
        let cfg = Config {
            api_host: "127.0.0.1".into(),
            api_port: 3000,
            cors_origins: Vec::new(),
            csp_report_uri: csp_report_uri.map(str::to_owned),
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
            },
            auth: auth.clone(),
        };
        router(AppState::new(pool, auth), &cfg)
    }

    #[tokio::test]
    async fn json_responses_get_the_baseline_headers_and_no_csp() {
        let res = get(app(), "/api/v1/no-such-route", &[]).await;
        assert_eq!(res.status(), 404);
        assert!(header(&res, header::CONTENT_TYPE).unwrap().starts_with("application/json"));
        assert_eq!(header(&res, header::X_CONTENT_TYPE_OPTIONS), Some("nosniff"));
        assert_eq!(header(&res, header::REFERRER_POLICY), Some("no-referrer"));
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
        assert_eq!(header(&res, header::X_CONTENT_TYPE_OPTIONS), Some("nosniff"));
        assert_eq!(header(&res, header::REFERRER_POLICY), Some("no-referrer"));
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
}
