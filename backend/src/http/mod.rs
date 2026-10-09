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
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicU64, AtomicUsize};

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
    /// Impact analysis limits and the analyses in progress (`IMPACT_*`).
    pub impact: Arc<crate::modules::impact::ImpactState>,
    /// Bulk import limits (`IMPORT_*`).
    pub imports: Arc<crate::config::ImportConfig>,
    /// Business service limits (`BUSINESS_SERVICE_*`).
    pub business_services: crate::config::BusinessServiceConfig,
    /// Workflow action limits (`WORKFLOW_ACTIONS_*`).
    pub workflow_actions: crate::config::WorkflowActionsConfig,
    /// Saved-view count requests running at once (GH#780).
    pub view_counts: Arc<tokio::sync::Semaphore>,
    /// Inventory exports in progress (`EXPORT_MAX_CONCURRENT`, GH#801).
    pub exports: Arc<crate::modules::items::export::Exports>,
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
            auth: Arc::new(AuthState::new(auth, keyring)),
            capture: ClientCapture { ip: true, user_agent: true },
            schema: Arc::default(),
            sealed: Arc::default(),
            capacity: Capacity::new(512, Duration::from_secs(10)),
            readiness: Arc::default(),
            impact: Arc::default(),
            imports: Arc::default(),
            business_services: Default::default(),
            workflow_actions: Default::default(),
            view_counts: Arc::new(tokio::sync::Semaphore::new(crate::modules::saved_views::service::count_slots(
                pool.options().get_max_connections(),
            ))),
            exports: Arc::new(crate::modules::items::export::Exports::new(crate::config::ExportConfig::for_pool(
                pool.options().get_max_connections(),
            ))),
            pool,
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

    pub fn importing(mut self, imports: &crate::config::ImportConfig) -> Self {
        self.imports = Arc::new(imports.clone());
        self
    }

    pub fn limited(mut self, http: &HttpConfig) -> Self {
        self.capacity =
            Capacity::new(http.max_concurrent_requests, http.header_read_timeout).with_body_timeout(http.body_timeout);
        self
    }

    pub fn with_business_services(mut self, limits: crate::config::BusinessServiceConfig) -> Self {
        self.business_services = limits;
        self
    }

    pub fn with_workflow_actions(mut self, limits: crate::config::WorkflowActionsConfig) -> Self {
        self.workflow_actions = limits;
        self
    }

    pub fn with_exports(mut self, exports: crate::config::ExportConfig) -> Self {
        self.exports = Arc::new(crate::modules::items::export::Exports::new(exports));
        self
    }

    pub fn with_impact(mut self, impact: crate::config::ImpactConfig) -> Self {
        self.impact = Arc::new(crate::modules::impact::ImpactState::new(impact));
        self
    }
}

/// Bounds the API requests in progress; a request that finds its pool empty
/// is answered 503 SERVER_BUSY instead of queueing, and a rejected request
/// never holds capacity.
///
/// Authenticated routes take a permit from the global pool in `api::route`
/// after they authorise the caller and before they read the body. One user
/// (with all their sessions and API tokens) holds at most a quarter of that
/// pool (GH#502), and the body must arrive within `HTTP_BODY_TIMEOUT_SECS`
/// (GH#556), so users who send bodies slowly cannot hold every permit and
/// refuse everyone else for `HTTP_REQUEST_TIMEOUT_SECS`. Public
/// routes (setup, sign-in, OIDC, branding) draw from their own, smaller pool,
/// and only once their body is in (GH#283): anyone can send one slowly, so a
/// body in transit holds no permit. It must arrive within
/// `HTTP_HEADER_READ_TIMEOUT_SECS`, and once the read has to wait for more, it
/// counts against a shared budget of `HTTP_MAX_CONCURRENT_REQUESTS` × 64 KiB
/// (16 MiB to 256 MiB): what has arrived, plus [`WAITING_BODY_COST`] for the
/// connection that waits. So slow senders cost a connection each but never the
/// permits of real sign-ins, and both the memory and the number of bodies in
/// transit stay bounded, even for bodies that send nothing (GH#343). One client
/// network (`auth::throttle::Net`) holds at most a sixteenth of the budget
/// (GH#342), and one wider network (`Net::wide`, an IPv4 /16 or IPv6 /48) at
/// most a quarter (GH#504), so one host cannot spend it for everyone else, even
/// with many /64s. A body that arrives
/// without a wait is never held and never counts, so a spent budget cannot
/// refuse it. Anonymous
/// callers can then only saturate the public routes, never the capacity
/// signed-in users and API tokens need. Only the health routes (liveness,
/// readiness, version; `RouteBuilder::unlimited`) take no permit, so a busy
/// server is not mistaken for a dead one.
#[derive(Clone)]
pub struct Capacity {
    global: Arc<tokio::sync::Semaphore>,
    public: Arc<tokio::sync::Semaphore>,
    /// Global permits each user holds.
    by_user: Arc<Shares<uuid::Uuid>>,
    /// Bytes held by public request bodies waiting for the rest, one permit per byte.
    public_body_bytes: Arc<tokio::sync::Semaphore>,
    /// The share of those bytes each client network holds.
    public_body_by_net: Arc<Shares<crate::auth::throttle::Net>>,
    /// The share of those bytes each wider network (`Net::wide`) holds.
    public_body_by_wide: Arc<Shares<crate::auth::throttle::Net>>,
    /// Time a public route may take to receive its body.
    pub public_body_timeout: Duration,
    /// Time an authenticated route may take to receive its body
    /// (`HTTP_BODY_TIMEOUT_SECS`), while it holds a global permit (GH#556).
    pub body_timeout: Duration,
}

/// What each key holds of a shared resource, and the most one key may hold.
/// Keys holding nothing are forgotten.
struct Shares<K> {
    held: std::sync::Mutex<std::collections::HashMap<K, usize>>,
    max: usize,
}

impl<K: std::hash::Hash + Eq + Copy> Shares<K> {
    fn new(max: usize) -> Self {
        Shares { held: std::sync::Mutex::default(), max }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, std::collections::HashMap<K, usize>> {
        self.held.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn room(held: &std::collections::HashMap<K, usize>, key: K, n: usize, max: usize) -> bool {
        held.get(&key).copied().unwrap_or(0).saturating_add(n) <= max
    }

    fn give_back(&self, key: K, n: usize) {
        let mut held = self.lock();
        if let Some(h) = held.get_mut(&key) {
            *h -= n.min(*h);
            if *h == 0 {
                held.remove(&key);
            }
        }
    }

    #[cfg(test)]
    fn of(&self, key: K) -> usize {
        self.lock().get(&key).copied().unwrap_or(0)
    }
}

/// A permit from one of the request pools, given back when dropped.
pub struct Slot {
    _permit: tokio::sync::OwnedSemaphorePermit,
    user: Option<(Arc<Shares<uuid::Uuid>>, uuid::Uuid)>,
}

impl Drop for Slot {
    fn drop(&mut self) {
        if let Some((by_user, user)) = &self.user {
            by_user.give_back(*user, 1);
        }
    }
}

/// Floor of the public body budget, so a small HTTP_MAX_CONCURRENT_REQUESTS
/// does not leave room for only a handful of bodies.
const MIN_PUBLIC_BODY_BUDGET: usize = 16 * 1024 * 1024;
/// Ceiling of the public body budget: HTTP_MAX_CONCURRENT_REQUESTS goes up to
/// 1,000,000, which would otherwise let slow senders hold about 64 GiB (GH#342).
const MAX_PUBLIC_BODY_BUDGET: usize = 256 * 1024 * 1024;
/// The part of the public body budget one client network may hold.
const PUBLIC_BODY_SHARES: usize = 16;
/// The part of the public body budget one wider network (`Net::wide`) may hold.
const PUBLIC_BODY_WIDE_SHARES: usize = 4;
/// The part of the global pool one user may hold.
const USER_SHARES: usize = 4;
/// `HTTP_BODY_TIMEOUT_SECS` when not set.
pub const DEFAULT_BODY_TIMEOUT: Duration = Duration::from_secs(30);
/// What a public body that waits for more costs beyond its bytes: the
/// connection, its task, timer and buffers. Charged even when nothing has
/// arrived yet, so the budget also bounds how many bodies are in transit
/// (GH#343). Measured on a release build at about 66 kB of resident memory per
/// waiting body; 4 KiB let the budget admit 16 times as many (GH#501).
pub const WAITING_BODY_COST: usize = 64 * 1024;

impl Capacity {
    /// `max` requests for authenticated routes, and `max / 8` (at least 16) for public ones.
    pub fn new(max: usize, public_body_timeout: Duration) -> Self {
        Capacity::with_sizes(max, (max / 8).max(16), public_body_timeout)
    }

    pub fn with_sizes(global: usize, public: usize, public_body_timeout: Duration) -> Self {
        let budget = global
            .saturating_mul(crate::api::route::PUBLIC_BODY_LIMIT)
            .clamp(MIN_PUBLIC_BODY_BUDGET, MAX_PUBLIC_BODY_BUDGET);
        let per_net = (budget / PUBLIC_BODY_SHARES).max(crate::api::route::PUBLIC_BODY_LIMIT + WAITING_BODY_COST);
        Capacity::with_body_budget(global, public, budget, per_net, public_body_timeout)
    }

    /// A wider network may hold a quarter of `body_bytes`, and never less than `per_net`.
    pub fn with_body_budget(
        global: usize,
        public: usize,
        body_bytes: usize,
        per_net: usize,
        public_body_timeout: Duration,
    ) -> Self {
        Capacity {
            global: Arc::new(tokio::sync::Semaphore::new(global)),
            public: Arc::new(tokio::sync::Semaphore::new(public)),
            by_user: Arc::new(Shares::new((global / USER_SHARES).max(1))),
            public_body_bytes: Arc::new(tokio::sync::Semaphore::new(
                body_bytes.min(tokio::sync::Semaphore::MAX_PERMITS),
            )),
            public_body_by_net: Arc::new(Shares::new(per_net)),
            public_body_by_wide: Arc::new(Shares::new((body_bytes / PUBLIC_BODY_WIDE_SHARES).max(per_net))),
            public_body_timeout,
            body_timeout: DEFAULT_BODY_TIMEOUT,
        }
    }

    pub fn with_body_timeout(mut self, body_timeout: Duration) -> Self {
        self.body_timeout = body_timeout;
        self
    }

    /// A permit from the public or the global pool, or 503 SERVER_BUSY.
    pub fn acquire(&self, public: bool) -> Result<Slot, AppError> {
        let pool = if public { &self.public } else { &self.global };
        let permit = pool.clone().try_acquire_owned().map_err(|_| {
            tracing::warn!(public, "request refused: HTTP_MAX_CONCURRENT_REQUESTS reached");
            server_busy()
        })?;
        Ok(Slot { _permit: permit, user: None })
    }

    /// A permit from the global pool for `user`, or 503 SERVER_BUSY when the
    /// pool, or the user's share of it, is spent. The refusal is logged with the
    /// request span's `user_id` (and `token_id`), recorded by `api::route` (GH#571).
    pub fn acquire_for_user(&self, user: uuid::Uuid) -> Result<Slot, AppError> {
        let mut held = self.by_user.lock();
        if !Shares::room(&held, user, 1, self.by_user.max) {
            tracing::warn!("request refused: one user's share of HTTP_MAX_CONCURRENT_REQUESTS reached");
            return Err(server_busy());
        }
        let mut slot = self.acquire(false)?;
        *held.entry(user).or_default() += 1;
        slot.user = Some((self.by_user.clone(), user));
        Ok(slot)
    }

    /// 503 SERVER_BUSY when the public pool has no permit left: a public route
    /// checks before it reads its body, so a full pool refuses without reading it.
    pub fn check_public(&self) -> Result<(), AppError> {
        if self.public.available_permits() > 0 {
            return Ok(());
        }
        tracing::warn!(public = true, "request refused: HTTP_MAX_CONCURRENT_REQUESTS reached");
        Err(server_busy())
    }

    /// What a public request body from `net` holds of the budget: nothing until [`BodyHold::add`].
    pub fn hold_public_body(&self, net: crate::auth::throttle::Net) -> BodyHold {
        BodyHold { capacity: self.clone(), net, logs_net: false, permit: None, bytes: 0 }
    }

    #[cfg(test)]
    pub fn available(&self, public: bool) -> usize {
        if public { self.public.available_permits() } else { self.global.available_permits() }
    }

    #[cfg(test)]
    pub fn available_body_bytes(&self) -> usize {
        self.public_body_bytes.available_permits()
    }

    #[cfg(test)]
    pub fn held_body_bytes(&self, net: crate::auth::throttle::Net) -> usize {
        self.public_body_by_net.of(net)
    }

    #[cfg(test)]
    pub fn held_by_user(&self, user: uuid::Uuid) -> usize {
        self.by_user.of(user)
    }
}

/// Budget held by one public request body, given back when dropped.
pub struct BodyHold {
    capacity: Capacity,
    net: crate::auth::throttle::Net,
    /// A refusal records `net` on the request span (AUDIT_CAPTURE_IP).
    logs_net: bool,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
    bytes: usize,
}

impl BodyHold {
    /// A refusal logs the client network (GH#571).
    pub fn logging_net(mut self, yes: bool) -> Self {
        self.logs_net = yes;
        self
    }

    /// Holds `bytes` more, or 503 SERVER_BUSY when the budget, or this client
    /// network's or its wider network's share of it, is spent.
    pub fn add(&mut self, bytes: usize) -> Result<(), AppError> {
        let n = u32::try_from(bytes).map_err(|_| server_busy())?;
        let capacity = &self.capacity;
        let wide = self.net.wide();
        let refuse = |why: &str| {
            if self.logs_net {
                record_net(self.net);
            }
            tracing::warn!("request refused: {why}");
            server_busy()
        };
        // Always in this order (net, then wide), so two holds cannot deadlock.
        let mut by_net = capacity.public_body_by_net.lock();
        let mut by_wide = capacity.public_body_by_wide.lock();
        if !Shares::room(&by_net, self.net, bytes, capacity.public_body_by_net.max) {
            return Err(refuse("one client network's share of the public request body budget reached"));
        }
        if !Shares::room(&by_wide, wide, bytes, capacity.public_body_by_wide.max) {
            return Err(refuse("one wider network's share of the public request body budget reached"));
        }
        let more = capacity
            .public_body_bytes
            .clone()
            .try_acquire_many_owned(n)
            .map_err(|_| refuse("public request body budget reached"))?;
        *by_net.entry(self.net).or_default() += bytes;
        *by_wide.entry(wide).or_default() += bytes;
        drop((by_net, by_wide));
        match &mut self.permit {
            Some(p) => p.merge(more),
            None => self.permit = Some(more),
        }
        self.bytes += bytes;
        Ok(())
    }
}

impl Drop for BodyHold {
    fn drop(&mut self) {
        if self.bytes == 0 {
            return;
        }
        self.capacity.public_body_by_net.give_back(self.net, self.bytes);
        self.capacity.public_body_by_wide.give_back(self.net.wide(), self.bytes);
    }
}

/// Records the client's network and wider network, never its address, on the
/// request span, so the refusal or timeout of a public body can be attributed (GH#571).
pub fn record_net(net: crate::auth::throttle::Net) {
    let span = tracing::Span::current();
    span.record("net", net.cidr());
    span.record("wide", net.wide_cidr());
}

fn server_busy() -> AppError {
    let mut err = AppError::new(ErrorCode::ServerBusy, "The server is handling too many requests; retry shortly");
    err.retry_after = Some(1);
    err
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
/// proxy in front should not add its own copies.
///
/// CSP `frame-ancestors` only reaches HTML documents; `X-Frame-Options: DENY`
/// keeps every other response (API JSON, assets, uploaded logos) out of frames
/// too. `Cross-Origin-Opener-Policy: same-origin` cuts the `window.opener` link
/// to cross-origin pages. The layout editor's popup is same-origin with the
/// same policy, so it keeps its opener; sign-in with OIDC is a top-level
/// redirect, not a popup. `Cross-Origin-Resource-Policy: same-origin` stops
/// another site from pulling a response into its process with a no-cors
/// `<img>` or `<script>` (Spectre-style reads); CORS requests from
/// `CORS_ORIGINS` are not no-cors and are unaffected. A handler may set its own
/// value: the public logo and favicon (`ui_settings::serve_asset`) do.
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
        .layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("cross-origin-resource-policy"),
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
            // `{*rest}` does not match an empty tail, so `/docs/` needs its own
            // route or it falls through to the embedded UI (GH#602).
            .route("/docs/", axum::routing::any(docs_disabled))
            .route("/docs/{*rest}", axum::routing::any(docs_disabled)),
    }
}

/// `HTTP_REQUEST_TIMEOUT_SECS`, and the routes that bound their own duration
/// instead (the bulk import upload, `IMPORT_UPLOAD_TIMEOUT_SECS`).
#[derive(Clone)]
struct TimeoutRules {
    limit: Duration,
    own: Arc<Vec<(Method, String)>>,
}

/// Bounds the whole request (body upload included) until the response
/// starts. Dropping the handler rolls back its open transaction.
async fn request_timeout(
    axum::extract::State(rules): axum::extract::State<TimeoutRules>,
    req: Request,
    next: axum::middleware::Next,
) -> Response {
    if rules.own.iter().any(|(m, p)| m == req.method() && p == req.uri().path()) {
        return next.run(req).await;
    }
    let limit = rules.limit;
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
        .layer(axum::middleware::from_fn_with_state(
            TimeoutRules { limit: cfg.http.request_timeout, own: Arc::new(api::own_timeout_routes()) },
            request_timeout,
        ))
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
    if cfg.auth.trusted_proxies.is_empty() {
        tracing::warn!(
            "TRUSTED_PROXIES is empty: the sign-in throttle keys on the TCP peer address. Behind a reverse proxy, \
             list it there, or every client shares the proxy's network for throttling"
        );
    }
    let state = AppState::new(pool.clone(), cfg.auth.clone(), keyring)
        .capturing(&cfg.audit)
        .limited(&cfg.http)
        .with_impact(cfg.impact)
        .with_business_services(cfg.business_services)
        .with_workflow_actions(cfg.workflow_actions)
        .with_exports(cfg.exports)
        .importing(&cfg.imports);
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
    let exporter =
        cfg.audit.export.clone().map(|export| crate::audit_export::spawn(pool.clone(), export)).transpose()?;
    let import_workers = crate::modules::imports::worker::spawn(pool.clone(), state.imports.clone());
    let refusals = crate::auth::token::RefusalFlush::spawn(pool.clone());
    let note_retention = crate::modules::ci_notes::service::spawn_retention(pool.clone());
    let notifications = crate::modules::notifications::service::Retention::spawn(pool.clone(), cfg.notifications);
    let approval_sweep = crate::modules::workflows::runtime::sweep::Sweep::spawn(pool.clone(), cfg.approval_sweep);
    let action_outbox = crate::modules::workflows::actions::outbox::Outbox::spawn(
        pool.clone(),
        cfg.workflow_actions,
        crate::modules::workflows::actions::outbox::Channels::default(),
    );

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
    raise_open_files_limit();
    let open_files = open_files_limit();
    let max_connections = connection_limit(cfg.http.max_concurrent_requests, open_files, cfg.database.pool_max);
    let peers = PeerConnections::new(max_connections, cfg.auth.trusted_proxies.clone());
    tracing::info!(
        max_connections,
        per_network = peers.by_net.max,
        per_wide_network = peers.by_wide.max,
        open_files = open_files.map_or_else(|| "unlimited".to_owned(), |n| n.to_string()),
        "connection limit"
    );
    let needed = open_files_needed(cfg.http.max_concurrent_requests, cfg.database.pool_max);
    if let Some(open_files) = open_files.filter(|&n| n < needed) {
        // A small limit next to the pool leaves few connections, down to one,
        // which a single keep-alive or slow client then holds (GH#628).
        tracing::warn!(
            open_files,
            needed,
            max_connections,
            per_network = peers.by_net.max,
            wanted = cfg.http.max_concurrent_requests.saturating_mul(CONNECTIONS_PER_REQUEST_SLOT),
            pool_max = cfg.database.pool_max,
            "the open-files limit caps the connection limit: raise the hard open-files limit (LimitNOFILE= in \
             the systemd unit, ulimit -Hn) to at least `needed`, or lower DATABASE_POOL_MAX or \
             HTTP_MAX_CONCURRENT_REQUESTS"
        );
    }
    accept_loop(listener, app, &cfg.http, max_connections, peers, stop).await;
    // Before the exporter's last pass, so the summary rows leave too.
    refusals.stop().await;
    notifications.stop().await;
    if let Some(sweep) = approval_sweep {
        sweep.stop().await;
    }
    if let Some(outbox) = action_outbox {
        outbox.stop().await;
    }
    if let Some(exporter) = exporter {
        exporter.stop().await;
    }
    import_workers.stop().await;
    note_retention.stop().await;
    tracing::info!("draining complete, closing database pool");
    // Do not let a wedged connection hold up process exit.
    let _ = tokio::time::timeout(Duration::from_secs(5), pool.close()).await;
    if let Some(message) = sealed.refused.lock().expect("not poisoned").take() {
        anyhow::bail!(message);
    }
    tracing::info!("server stopped");
    Ok(())
}

/// Requests one HTTP/2 connection may have open at once (RFC 9113 recommends no fewer than 100).
const HTTP2_MAX_CONCURRENT_STREAMS: u32 = 100;

/// Connections open at once per request slot (GH#557): room for keep-alive
/// browsers and proxy pools, while idle or half-sent connections cannot grow
/// without bound.
const CONNECTIONS_PER_REQUEST_SLOT: usize = 4;
/// Share of the open-files limit kept back for the database pool, import
/// files and logs, and its floor on top of `DATABASE_POOL_MAX` (GH#562).
const FD_RESERVE_DIVISOR: u64 = 4;
const FD_RESERVE_MIN: u64 = 64;
/// The part of the open connections one client network (`auth::throttle::Net`,
/// an IPv4 /24 or IPv6 /64) may hold, and one wider network (`Net::wide`, an
/// IPv4 /16 or IPv6 /48), unless the peer is in `TRUSTED_PROXIES` (GH#561).
const CONNECTION_NET_SHARES: usize = 4;
const CONNECTION_WIDE_SHARES: usize = 2;
/// At most one "connection refused" warning per interval.
const REFUSED_WARN_INTERVAL: Duration = Duration::from_secs(10);

/// Connections the server keeps open at once: `CONNECTIONS_PER_REQUEST_SLOT`
/// per request slot, and below the open-files limit, so that the listener
/// never reaches EMFILE and the process keeps descriptors for its own work:
/// a quarter of the limit, and never fewer than the database pool's
/// `pool_max` connections plus `FD_RESERVE_MIN`.
fn connection_limit(max_concurrent_requests: usize, open_files: Option<u64>, pool_max: u32) -> usize {
    let by_slots = max_concurrent_requests.saturating_mul(CONNECTIONS_PER_REQUEST_SLOT);
    let by_files = open_files.map_or(usize::MAX, |n| {
        let reserve = (n / FD_RESERVE_DIVISOR).max(FD_RESERVE_MIN + u64::from(pool_max));
        usize::try_from(n.saturating_sub(reserve)).unwrap_or(usize::MAX)
    });
    by_slots.min(by_files).clamp(1, tokio::sync::Semaphore::MAX_PERMITS)
}

/// The smallest open-files limit at which `connection_limit` is not capped by
/// it: `n - n / 4` and `n - pool_max - FD_RESERVE_MIN` both reach the
/// connections the request slots allow (GH#628).
fn open_files_needed(max_concurrent_requests: usize, pool_max: u32) -> u64 {
    let by_slots = u64::try_from(
        max_concurrent_requests.saturating_mul(CONNECTIONS_PER_REQUEST_SLOT).min(tokio::sync::Semaphore::MAX_PERMITS),
    )
    .unwrap_or(u64::MAX);
    // n - floor(n / 4) = ceil(3n / 4) >= s holds from n = floor((4s - 1) / 3).
    let by_quarter = by_slots.saturating_mul(FD_RESERVE_DIVISOR).saturating_sub(1) / (FD_RESERVE_DIVISOR - 1);
    by_quarter.max(by_slots.saturating_add(FD_RESERVE_MIN + u64::from(pool_max)))
}

/// Soft `RLIMIT_NOFILE` of this process, where the platform shows it.
fn open_files_limit() -> Option<u64> {
    let limits = std::fs::read_to_string("/proc/self/limits").ok()?;
    let line = limits.lines().find(|l| l.starts_with("Max open files"))?;
    // "Max open files  <soft>  <hard>  files"; "unlimited" means no cap here.
    line["Max open files".len()..].split_whitespace().next()?.parse().ok()
}

/// Raises the soft `RLIMIT_NOFILE` to the hard limit, as nginx and Go do:
/// systemd and most shells start services with a soft limit of 1024, which
/// would cap the connection limit at 768 (GH#562). Raising the soft limit up
/// to the hard one needs no privilege; a failure is logged and the soft
/// limit stays as it was.
#[cfg(target_os = "linux")]
fn raise_open_files_limit() {
    let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: getrlimit only writes the struct it is given.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 || limit.rlim_cur >= limit.rlim_max {
        return;
    }
    let soft = limit.rlim_cur;
    limit.rlim_cur = limit.rlim_max;
    // SAFETY: setrlimit only reads the struct it is given.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) } == 0 {
        tracing::debug!(from = soft, to = limit.rlim_max, "open-files limit raised to the hard limit");
    } else {
        tracing::warn!(
            error = %std::io::Error::last_os_error(),
            soft,
            hard = limit.rlim_max,
            "cannot raise the open-files limit to the hard limit"
        );
    }
}

#[cfg(not(target_os = "linux"))]
fn raise_open_files_limit() {}

/// The open connections each client network and wider network holds, so one
/// source cannot take every place under the connection limit (GH#561).
/// Behind a reverse proxy every connection comes from the proxy, so peers in
/// `TRUSTED_PROXIES` are not counted; the global limit still bounds them.
struct PeerConnections {
    by_net: Shares<crate::auth::throttle::Net>,
    by_wide: Shares<crate::auth::throttle::Net>,
    trusted: crate::auth::session::TrustedProxies,
}

impl PeerConnections {
    fn new(max_connections: usize, trusted: crate::auth::session::TrustedProxies) -> Arc<Self> {
        Arc::new(PeerConnections {
            by_net: Shares::new((max_connections / CONNECTION_NET_SHARES).max(1)),
            by_wide: Shares::new((max_connections / CONNECTION_WIDE_SHARES).max(1)),
            trusted,
        })
    }

    /// A place for a connection from `peer`, or None when its network or
    /// wider network already holds its share.
    fn hold(self: &Arc<Self>, peer: std::net::IpAddr) -> Option<PeerHold> {
        if self.trusted.contains(peer) {
            return Some(PeerHold { peers: self.clone(), net: None });
        }
        let net = crate::auth::throttle::Net::of(Some(peer));
        let wide = net.wide();
        // Always in this order (net, then wide), like BodyHold::add.
        let mut by_net = self.by_net.lock();
        let mut by_wide = self.by_wide.lock();
        if !Shares::room(&by_net, net, 1, self.by_net.max) || !Shares::room(&by_wide, wide, 1, self.by_wide.max) {
            return None;
        }
        *by_net.entry(net).or_default() += 1;
        *by_wide.entry(wide).or_default() += 1;
        drop((by_net, by_wide));
        Some(PeerHold { peers: self.clone(), net: Some(net) })
    }
}

/// One connection's place in its network's share, given back when dropped.
struct PeerHold {
    peers: Arc<PeerConnections>,
    /// None for a trusted proxy, which is not counted.
    net: Option<crate::auth::throttle::Net>,
}

impl Drop for PeerHold {
    fn drop(&mut self) {
        if let Some(net) = self.net {
            self.peers.by_net.give_back(net, 1);
            self.peers.by_wide.give_back(net.wide(), 1);
        }
    }
}

/// Largest response frame handed to hyper at once. hyper asks a body for its
/// next frame only once the last one is sent, so a larger frame (a JSON
/// response is one) would show no progress until the client had taken all of
/// it, however fast it reads (GH#682).
const MAX_FRAME: usize = 16 * 1024;

/// What one connection is doing, for the limits `accept_loop` applies to it
/// (GH#665, GH#682). Times are milliseconds since `start`.
struct Activity {
    start: tokio::time::Instant,
    /// Requests from the request head until the response body is dropped.
    requests: AtomicUsize,
    /// When each response body that handed hyper a frame, or none yet,
    /// began to wait for hyper to take the next one: hyper only does once the
    /// client has taken enough of the response (HTTP/2 flow control, a full
    /// socket buffer). Kept per body, so one stream taking data does not
    /// hide another the client keeps shut.
    waiting: std::sync::Mutex<std::collections::HashMap<u64, u64>>,
    next_body: AtomicU64,
    /// When a request last started or ended, or a refused write went through.
    last_activity: AtomicU64,
    /// One more than when a write to the socket was refused with none going
    /// through since; 0 if writes go through.
    blocked_since: AtomicU64,
}

impl Activity {
    fn new() -> Self {
        Activity {
            start: tokio::time::Instant::now(),
            requests: AtomicUsize::new(0),
            waiting: Default::default(),
            next_body: AtomicU64::new(0),
            last_activity: AtomicU64::new(0),
            blocked_since: AtomicU64::new(0),
        }
    }

    fn now(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn waiting(&self) -> std::sync::MutexGuard<'_, std::collections::HashMap<u64, u64>> {
        self.waiting.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Since when the body that has waited longest for the client has waited.
    fn stalled_since(&self) -> Option<u64> {
        self.waiting().values().min().copied()
    }

    fn written(&self, refused: bool) {
        if refused {
            let _ = self.blocked_since.compare_exchange(0, self.now() + 1, Relaxed, Relaxed);
        } else if self.blocked_since.swap(0, Relaxed) != 0 {
            self.last_activity.store(self.now(), Relaxed);
        }
    }
}

/// A request in progress, from the request head until its response body is dropped.
struct InFlight(Arc<Activity>);

impl InFlight {
    fn new(activity: &Arc<Activity>) -> Self {
        activity.requests.fetch_add(1, Relaxed);
        activity.last_activity.store(activity.now(), Relaxed);
        InFlight(activity.clone())
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.last_activity.store(self.0.now(), Relaxed);
        self.0.requests.fetch_sub(1, Relaxed);
    }
}

/// A response body that tells `Activity` whether it waits for the client,
/// and hands hyper its data at most `MAX_FRAME` at a time.
struct TrackedBody {
    inner: axum::body::Body,
    /// What is left of a frame larger than `MAX_FRAME`.
    rest: axum::body::Bytes,
    id: u64,
    in_flight: InFlight,
}

impl TrackedBody {
    fn new(inner: axum::body::Body, in_flight: InFlight) -> Self {
        let activity = &in_flight.0;
        let id = activity.next_body.fetch_add(1, Relaxed);
        activity.waiting().insert(id, activity.now());
        TrackedBody { inner, rest: axum::body::Bytes::new(), id, in_flight }
    }
}

impl hyper::body::Body for TrackedBody {
    type Data = axum::body::Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        let activity = this.in_flight.0.clone();
        activity.waiting().remove(&this.id);
        let data = if this.rest.is_empty() {
            match std::pin::Pin::new(&mut this.inner).poll_frame(cx) {
                std::task::Poll::Ready(Some(Ok(frame))) => match frame.into_data() {
                    Ok(data) => data,
                    Err(frame) => {
                        activity.waiting().insert(this.id, activity.now());
                        return std::task::Poll::Ready(Some(Ok(frame)));
                    }
                },
                // Pending: the response itself is slow to come, which is not the client's doing.
                polled => return polled,
            }
        } else {
            std::mem::take(&mut this.rest)
        };
        let mut data = data;
        if data.len() > MAX_FRAME {
            this.rest = data.split_off(MAX_FRAME);
        }
        activity.waiting().insert(this.id, activity.now());
        std::task::Poll::Ready(Some(Ok(hyper::body::Frame::data(data))))
    }

    fn is_end_stream(&self) -> bool {
        self.rest.is_empty() && self.inner.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        let mut hint = self.inner.size_hint();
        let rest = self.rest.len() as u64;
        if let Some(upper) = hint.upper() {
            hint.set_upper(upper + rest);
        }
        hint.set_lower(hint.lower() + rest);
        hint
    }
}

impl Drop for TrackedBody {
    fn drop(&mut self) {
        self.in_flight.0.waiting().remove(&self.id);
    }
}

/// The client socket, telling `Activity` when the client stops taking what is
/// written to it.
struct TrackedIo {
    inner: tokio::net::TcpStream,
    activity: Arc<Activity>,
}

impl TrackedIo {
    fn track(&self, polled: &std::task::Poll<std::io::Result<usize>>) {
        match polled {
            std::task::Poll::Pending => self.activity.written(true),
            std::task::Poll::Ready(Ok(n)) if *n > 0 => self.activity.written(false),
            std::task::Poll::Ready(_) => {}
        }
    }
}

impl tokio::io::AsyncRead for TrackedIo {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl tokio::io::AsyncWrite for TrackedIo {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let polled = std::pin::Pin::new(&mut self.inner).poll_write(cx, buf);
        self.track(&polled);
        polled
    }

    fn poll_write_vectored(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let polled = std::pin::Pin::new(&mut self.inner).poll_write_vectored(cx, bufs);
        self.track(&polled);
        polled
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// What a connection's limits call for (see `accept_loop`).
#[derive(Debug, PartialEq)]
enum Verdict {
    Keep,
    /// Served nothing for the limit: ask it to close.
    Idle,
    /// Served nothing for the limit since it was asked to close, or the
    /// client took none of its response for the limit: drop it.
    Drop(&'static str),
}

/// `idle` and `send` are `accept_loop`'s two limits, in milliseconds.
fn verdict(activity: &Activity, idle: u64, send: u64, shut_down_at: Option<u64>) -> Verdict {
    let now = activity.now();
    let blocked_since = activity.blocked_since.load(Relaxed);
    if blocked_since != 0 && now.saturating_sub(blocked_since - 1) >= send {
        return Verdict::Drop("client stopped reading its response");
    }
    if activity.stalled_since().is_some_and(|since| now.saturating_sub(since) >= send) {
        return Verdict::Drop("client took none of its response");
    }
    if blocked_since != 0 || activity.requests.load(Relaxed) > 0 {
        return Verdict::Keep;
    }
    let since = activity.last_activity.load(Relaxed).max(shut_down_at.unwrap_or(0));
    match (now.saturating_sub(since) >= idle, shut_down_at) {
        (false, _) => Verdict::Keep,
        (true, None) => Verdict::Idle,
        (true, Some(_)) => Verdict::Drop("idle connection did not close after its shutdown"),
    }
}

/// Accepts connections until `shutdown`, then waits for open ones to finish
/// their current request. axum::serve sets no timer on hyper, which leaves
/// HTTP/1 header reads unbounded; this loop sets one. Past `max_connections`
/// open connections, or past its network's share of them (`PeerConnections`),
/// new ones are closed at once (GH#557, GH#561).
///
/// hyper's header read timeout covers HTTP/1 only, not the protocol preface
/// read before it nor HTTP/2, so a connection that serves no request for
/// `header_read_timeout` (from accept, or since its last request started or
/// ended) is shut down here whatever its protocol (GH#665): HTTP/2 gets a
/// GOAWAY. One that has not closed another `header_read_timeout` later is
/// dropped. So is one whose client takes none of a response for
/// `send_timeout`, by not reading the socket or by holding the HTTP/2
/// flow-control window shut, as the response would otherwise never end
/// (GH#682). Taking `MAX_FRAME` of it is progress, so a client that reads
/// slowly but steadily is not cut. A response that is slow to come is not
/// limited here.
async fn accept_loop(
    listener: TcpListener,
    app: Router,
    http: &crate::config::HttpConfig,
    max_connections: usize,
    peers: Arc<PeerConnections>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) {
    use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
    use hyper_util::server::conn::auto::Builder;
    use hyper_util::service::TowerToHyperService;
    use tower::ServiceExt;

    let mut builder = Builder::new(TokioExecutor::new());
    builder.http1().timer(TokioTimer::new()).header_read_timeout(http.header_read_timeout);
    // Explicit rather than hyper's default: each stream of a public route may
    // wait for its body, and that is what Capacity's body budget bounds (GH#343).
    builder
        .http2()
        .timer(TokioTimer::new())
        .max_concurrent_streams(HTTP2_MAX_CONCURRENT_STREAMS)
        .enable_connect_protocol()
        .keep_alive_interval(Duration::from_secs(30))
        .keep_alive_timeout(Duration::from_secs(20));

    let (stopping, stop) = tokio::sync::watch::channel(false);
    let idle_limit = http.header_read_timeout;
    let send_limit = http.send_timeout;
    let connections = Arc::new(tokio::sync::Semaphore::new(max_connections));
    let mut refused: u64 = 0;
    let mut last_warned: Option<tokio::time::Instant> = None;
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
        // Closed before any byte is read; the client sees a reset or EOF.
        let held = match peers.hold(peer.ip()) {
            None => Err("one client network's share of the open connection limit reached"),
            Some(hold) => connections.clone().try_acquire_owned().map(|permit| (permit, hold)).map_err(
                |_| "open connection limit reached (4 x HTTP_MAX_CONCURRENT_REQUESTS, or the open-files limit)",
            ),
        };
        let held = match held {
            Ok(held) => held,
            Err(why) => {
                drop(stream);
                refused += 1;
                if last_warned.is_none_or(|at| at.elapsed() >= REFUSED_WARN_INTERVAL) {
                    tracing::warn!(max_connections, refused, "connections refused: {why}");
                    last_warned = Some(tokio::time::Instant::now());
                    refused = 0;
                }
                continue;
            }
        };
        let _ = stream.set_nodelay(true);
        // The socket then takes more only once what it holds unsent is below
        // MAX_FRAME, so a write going through means the client took data.
        // Otherwise Linux waits for a third of a send buffer of up to 4 MiB
        // to drain, which a client reading steadily below about 1 Mbit/s
        // would not do within the send limit (GH#682).
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let _ = socket2::SockRef::from(&stream).set_tcp_notsent_lowat(MAX_FRAME as u32);
        // The peer address is the client IP of last resort for the audit trail (auth::session::client_ip).
        let activity = Arc::new(Activity::new());
        let service = app
            .clone()
            .map_request(move |req: axum::http::Request<hyper::body::Incoming>| {
                let mut req = req.map(axum::body::Body::new);
                req.extensions_mut().insert(axum::extract::ConnectInfo(peer));
                req
            })
            .map_future({
                let activity = activity.clone();
                move |response| {
                    let in_flight = InFlight::new(&activity);
                    async move {
                        let response: Response = response.await?;
                        Ok::<_, std::convert::Infallible>(response.map(|body| TrackedBody::new(body, in_flight)))
                    }
                }
            });
        let io = TrackedIo { inner: stream, activity: activity.clone() };
        let conn =
            builder.serve_connection_with_upgrades(TokioIo::new(io), TowerToHyperService::new(service)).into_owned();
        let mut stop = stop.clone();
        tokio::spawn(async move {
            let mut conn = std::pin::pin!(conn);
            let millis = |limit: Duration| u64::try_from(limit.as_millis()).unwrap_or(u64::MAX);
            let (idle, send) = (millis(idle_limit), millis(send_limit));
            let mut check = tokio::time::interval((idle_limit.min(send_limit) / 4).max(Duration::from_millis(10)));
            check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut shut_down_at: Option<u64> = None;
            let result = loop {
                tokio::select! {
                    result = conn.as_mut() => break Ok(result),
                    _ = stop.wait_for(|stop| *stop), if shut_down_at.is_none() => {
                        conn.as_mut().graceful_shutdown();
                        shut_down_at = Some(activity.now());
                    }
                    _ = check.tick() => match verdict(&activity, idle, send, shut_down_at) {
                        Verdict::Keep => {}
                        Verdict::Idle => {
                            conn.as_mut().graceful_shutdown();
                            shut_down_at = Some(activity.now());
                        }
                        Verdict::Drop(why) => break Err(why),
                    },
                }
            };
            match result {
                Ok(Err(e)) => tracing::debug!(error = %e, "connection closed with an error"),
                Err(why) => tracing::debug!("connection dropped: {why}"),
                Ok(Ok(())) => {}
            }
            drop(held);
        });
    }
    drop(listener);
    // Every connection task holds a permit until it ends.
    let _ = stopping.send(true);
    let _ = connections.acquire_many(u32::try_from(max_connections).unwrap_or(u32::MAX)).await;
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
            trusted_proxies: Default::default(),
            sign_in_failure_floor: Duration::ZERO,
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
                body_timeout: Duration::from_secs(30),
                send_timeout: Duration::from_secs(60),
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
            impact: Default::default(),
            imports: Default::default(),
            business_services: Default::default(),
            exports: Default::default(),
            notifications: Default::default(),
            approval_sweep: Default::default(),
            workflow_actions: Default::default(),
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
        assert_eq!(header(res, header::HeaderName::from_static("cross-origin-resource-policy")), Some("same-origin"));
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

    /// `ui_settings::serve_asset` opts its public logo and favicon out of `same-origin`.
    #[tokio::test]
    async fn handler_may_widen_resource_policy() {
        let corp = header::HeaderName::from_static("cross-origin-resource-policy");
        let app = Router::new().fallback({
            let corp = corp.clone();
            move || async move { ([(corp, "cross-origin")], "logo") }
        });
        let res = get(with_security_headers(app, Csp::new(None)), "/api/v1/ui-settings/assets/logo", &[]).await;
        assert_eq!(header(&res, corp), Some("cross-origin"));
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
        let app = slow.layer(axum::middleware::from_fn_with_state(
            TimeoutRules { limit: Duration::from_millis(50), own: Arc::default() },
            request_timeout,
        ));
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
            body_timeout: Duration::from_secs(5),
            send_timeout: Duration::from_secs(60),
            max_concurrent_requests: 512,
        };
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            accept_loop(listener, app(), &http, 16, PeerConnections::new(16, Default::default()), async {
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

    /// GH#665: hyper's header read timeout does not cover the protocol preface
    /// nor HTTP/2, so a silent client, one that sends part of the HTTP/2
    /// preface, or an HTTP/2 client that opens no stream held its connection
    /// (and its place under the connection limit) for ever.
    #[tokio::test]
    async fn connections_that_send_no_request_are_closed_whatever_the_protocol() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let http = crate::config::HttpConfig {
            header_read_timeout: Duration::from_millis(200),
            request_timeout: Duration::from_secs(5),
            body_timeout: Duration::from_secs(5),
            send_timeout: Duration::from_secs(60),
            max_concurrent_requests: 512,
        };
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            accept_loop(listener, app(), &http, 16, PeerConnections::new(16, Default::default()), async {
                let _ = rx.await;
            })
            .await
        });
        // An HTTP/2 SETTINGS frame with no settings.
        const H2_SETTINGS: &[u8] = &[0, 0, 0, 4, 0, 0, 0, 0, 0];
        let preface = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
        let cases: [(&str, Vec<u8>); 3] = [
            ("silent", Vec::new()),
            ("partial HTTP/2 preface", b"PRI * HTTP/2.0\r\n".to_vec()),
            ("HTTP/2 with no stream", [&preface[..], H2_SETTINGS].concat()),
        ];
        for (case, sent) in cases {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            stream.write_all(&sent).await.unwrap();
            let mut buf = Vec::new();
            let read = tokio::time::timeout(Duration::from_secs(3), stream.read_to_end(&mut buf)).await;
            assert!(read.is_ok(), "{case}: connection still open after the header read timeout");
        }
        let _ = tx.send(());
        server.await.unwrap();
    }

    /// GH#665: places held by silent connections are freed once they are
    /// closed, so a later client is served.
    #[tokio::test]
    async fn silent_connections_give_back_their_place() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let http = crate::config::HttpConfig {
            header_read_timeout: Duration::from_millis(200),
            request_timeout: Duration::from_secs(5),
            body_timeout: Duration::from_secs(5),
            send_timeout: Duration::from_secs(60),
            max_concurrent_requests: 512,
        };
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let peers = PeerConnections::new(2, crate::auth::session::TrustedProxies::parse("127.0.0.0/8").unwrap());
            accept_loop(listener, app(), &http, 2, peers, async {
                let _ = rx.await;
            })
            .await
        });
        let mut silent = Vec::new();
        for _ in 0..2 {
            silent.push(tokio::net::TcpStream::connect(addr).await.unwrap());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").await.unwrap();
        let mut buf = Vec::new();
        tokio::time::timeout(Duration::from_secs(3), stream.read_to_end(&mut buf)).await.unwrap().unwrap();
        let answer = String::from_utf8_lossy(&buf);
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
        drop(silent);
        let _ = tx.send(());
        server.await.unwrap();
    }

    /// GH#665: the idle limit applies only while a connection serves nothing;
    /// a response that takes longer than it is sent in full, and the
    /// connection stays open for the next request.
    #[tokio::test]
    async fn the_idle_limit_does_not_cut_requests_in_flight() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let http = crate::config::HttpConfig {
            header_read_timeout: Duration::from_millis(200),
            request_timeout: Duration::from_secs(5),
            body_timeout: Duration::from_secs(5),
            send_timeout: Duration::from_secs(60),
            max_concurrent_requests: 512,
        };
        // Answers after 600 ms, then streams its body over another 600 ms.
        let slow = Router::new().route(
            "/slow",
            axum::routing::get(|| async {
                tokio::time::sleep(Duration::from_millis(600)).await;
                let chunks = futures_util::stream::unfold(0, |i| async move {
                    (i < 3).then_some(())?;
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    Some((Ok::<_, std::convert::Infallible>(axum::body::Bytes::from_static(b"chunk")), i + 1))
                });
                Body::from_stream(chunks)
            }),
        );
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            accept_loop(listener, slow, &http, 16, PeerConnections::new(16, Default::default()), async {
                let _ = rx.await;
            })
            .await
        });
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        for last in [false, true] {
            let close = if last { "Connection: close\r\n" } else { "" };
            let req = format!("GET /slow HTTP/1.1\r\nHost: x\r\n{close}\r\n");
            stream.write_all(req.as_bytes()).await.unwrap();
            // Read until the end of the chunked body.
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            while !buf.ends_with(b"0\r\n\r\n") {
                let n = tokio::time::timeout(Duration::from_secs(3), stream.read(&mut chunk)).await.unwrap().unwrap();
                assert!(n > 0, "connection closed mid-response: {}", String::from_utf8_lossy(&buf));
                buf.extend_from_slice(&chunk[..n]);
            }
            let answer = String::from_utf8_lossy(&buf);
            assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
            assert_eq!(answer.matches("\r\nchunk\r\n").count(), 3, "{answer}");
        }
        let _ = tx.send(());
        server.await.unwrap();
    }

    /// GH#665 review: a keep-alive connection whose requests each start and
    /// end between two checks is busy, not idle, and is never asked to close.
    #[tokio::test]
    async fn busy_keep_alive_connections_are_not_treated_as_idle() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let http = crate::config::HttpConfig {
            header_read_timeout: Duration::from_millis(200),
            request_timeout: Duration::from_secs(5),
            body_timeout: Duration::from_secs(5),
            send_timeout: Duration::from_secs(60),
            max_concurrent_requests: 512,
        };
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            accept_loop(listener, app(), &http, 16, PeerConnections::new(16, Default::default()), async {
                let _ = rx.await;
            })
            .await
        });
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        // A request every 50 ms for 1 s: five times the 200 ms limit.
        for i in 0..20 {
            stream.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n\r\n").await.unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            while !String::from_utf8_lossy(&buf).contains("\r\n\r\n") {
                let n = tokio::time::timeout(Duration::from_secs(3), stream.read(&mut chunk)).await.unwrap().unwrap();
                assert!(n > 0, "request {i}: connection closed");
                buf.extend_from_slice(&chunk[..n]);
            }
            let head = String::from_utf8_lossy(&buf).to_ascii_lowercase();
            assert!(head.starts_with("http/1.1 200"), "request {i}: {head}");
            assert!(!head.contains("connection: close"), "request {i}: asked to close: {head}");
            // /healthz answers with a Content-Length body; read the rest of it.
            let length: usize = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .map(|n| n.trim().parse().unwrap())
                .unwrap_or(0);
            let body_start = head.find("\r\n\r\n").unwrap() + 4;
            while buf.len() < body_start + length {
                let n = tokio::time::timeout(Duration::from_secs(3), stream.read(&mut chunk)).await.unwrap().unwrap();
                assert!(n > 0, "request {i}: connection closed mid-body");
                buf.extend_from_slice(&chunk[..n]);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let _ = tx.send(());
        server.await.unwrap();
    }

    /// A server that allows one open connection and has large responses.
    async fn one_connection_server(
        tx_rx: tokio::sync::oneshot::Receiver<()>,
        send_timeout: Duration,
    ) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let http = crate::config::HttpConfig {
            header_read_timeout: Duration::from_millis(200),
            request_timeout: Duration::from_secs(30),
            body_timeout: Duration::from_secs(30),
            send_timeout,
            max_concurrent_requests: 512,
        };
        // 256 MiB in 64 KiB chunks: more than any socket buffer holds.
        let routes = Router::new()
            .route(
                "/big",
                axum::routing::get(|| async {
                    let chunks = futures_util::StreamExt::map(futures_util::stream::iter(0..4096), |_| {
                        Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(vec![b'x'; 64 * 1024]))
                    });
                    Body::from_stream(chunks)
                }),
            )
            // 8 MiB in one frame, as a JSON response is.
            .route("/single", axum::routing::get(|| async { Body::from(vec![b'x'; 8 << 20]) }))
            .route("/small", axum::routing::get(|| async { "ok" }));
        let server = tokio::spawn(async move {
            let peers = PeerConnections::new(1, crate::auth::session::TrustedProxies::parse("127.0.0.0/8").unwrap());
            accept_loop(listener, routes, &http, 1, peers, async {
                let _ = tx_rx.await;
            })
            .await
        });
        (addr, server)
    }

    /// Asserts that a new client is served, so the one open connection was freed.
    async fn assert_served(addr: std::net::SocketAddr) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream.write_all(b"GET /small HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").await.unwrap();
        let mut buf = Vec::new();
        tokio::time::timeout(Duration::from_secs(3), stream.read_to_end(&mut buf)).await.unwrap().unwrap();
        let answer = String::from_utf8_lossy(&buf);
        assert!(answer.starts_with("HTTP/1.1 200"), "the stalled connection kept the only place: {answer}");
    }

    /// GH#682: an HTTP/1 client that asks for a large response and stops
    /// reading it is dropped, and its place is freed.
    #[tokio::test]
    async fn clients_that_stop_reading_their_response_are_dropped() {
        use tokio::io::AsyncWriteExt;
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let (addr, server) = one_connection_server(rx, Duration::from_millis(200)).await;
        let mut stalled = tokio::net::TcpStream::connect(addr).await.unwrap();
        stalled.write_all(b"GET /big HTTP/1.1\r\nHost: x\r\n\r\n").await.unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_served(addr).await;
        drop(stalled);
        let _ = tx.send(());
        server.await.unwrap();
    }

    /// GH#682: an HTTP/2 client that shuts its flow-control window and asks
    /// for a response is dropped, and its place is freed.
    #[tokio::test]
    async fn http2_clients_with_a_shut_window_are_dropped() {
        use tokio::io::AsyncWriteExt;
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let (addr, server) = one_connection_server(rx, Duration::from_millis(200)).await;
        let mut stalled = tokio::net::TcpStream::connect(addr).await.unwrap();
        // SETTINGS_INITIAL_WINDOW_SIZE = 0.
        const SETTINGS: &[u8] = &[0, 0, 6, 4, 0, 0, 0, 0, 0, 0, 4, 0, 0, 0, 0];
        // HPACK: :method GET, :scheme http, :path /big, :authority x.
        const BLOCK: &[u8] = &[0x82, 0x86, 0x44, 4, b'/', b'b', b'i', b'g', 0x41, 1, b'x'];
        // HEADERS, END_STREAM | END_HEADERS, stream 1.
        let headers = [&[0, 0, BLOCK.len() as u8, 1, 5, 0, 0, 0, 1][..], BLOCK].concat();
        let sent = [&b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n"[..], SETTINGS, &headers].concat();
        stalled.write_all(&sent).await.unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_served(addr).await;
        drop(stalled);
        let _ = tx.send(());
        server.await.unwrap();
    }

    /// GH#682 review: an HTTP/1 client that reads its response slowly but
    /// steadily is not dropped, though the socket's send buffer drains much
    /// more slowly than the send limit. Without `TCP_NOTSENT_LOWAT` the
    /// server sees no progress until a third of that buffer has drained, and
    /// drops this client, so the test runs only where the option is set.
    ///
    /// The send limit is wall-clock time, and the client shares the test's
    /// one thread with the server: on a loaded host that thread can stall
    /// for the limit, and the drop is then deserved (GH#751). A run in which
    /// the client went half the limit without reading is retried; one in
    /// which it read steadily must pass.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[tokio::test]
    async fn steady_slow_http1_readers_are_not_dropped() {
        const ATTEMPTS: usize = 3;
        for attempt in 1..=ATTEMPTS {
            match slow_http1_read(Duration::from_secs(1)).await {
                Ok(()) => return,
                Err((why, Some(gap))) if attempt < ATTEMPTS => {
                    eprintln!("attempt {attempt}: {why}, but the client stalled {gap:?}: retrying");
                }
                Err((why, gap)) => panic!("{why} (longest gap between reads: {gap:?})"),
            }
        }
    }

    /// One run of `steady_slow_http1_readers_are_not_dropped`. On failure,
    /// also returns the longest gap between the client's reads if it reached
    /// half of `send_limit`.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    async fn slow_http1_read(send_limit: Duration) -> Result<(), (String, Option<Duration>)> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let (addr, server) = one_connection_server(rx, send_limit).await;
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream.write_all(b"GET /big HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").await.unwrap();
        let pace = Duration::from_millis(100);
        let mut last_read = tokio::time::Instant::now();
        let mut longest_gap = Duration::ZERO;
        let mut gap = |last_read: &mut tokio::time::Instant, expected: Duration| {
            let now = tokio::time::Instant::now();
            longest_gap = longest_gap.max(now.duration_since(*last_read).saturating_sub(expected));
            *last_read = now;
        };
        // 16 KiB every 100 ms (about 160 KiB/s) for 4 s, four times the send limit.
        let mut chunk = vec![0u8; 16 * 1024];
        let mut taken = 0;
        let mut outcome = Ok(());
        for _ in 0..40 {
            let read = tokio::time::timeout(Duration::from_secs(3), stream.read_exact(&mut chunk)).await.unwrap();
            gap(&mut last_read, pace);
            if let Err(e) = read {
                outcome = Err(format!("dropped after the client took {taken} bytes: {e}"));
                break;
            }
            taken += chunk.len();
            tokio::time::sleep(pace).await;
        }
        // The rest at full speed: the whole chunked response arrives, last chunk included.
        if outcome.is_ok() {
            // Only the end is checked; keep its last bytes, not all 256 MiB.
            let mut tail = Vec::new();
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                let n = tokio::time::timeout(Duration::from_secs(30), stream.read(&mut buf)).await.unwrap().unwrap();
                gap(&mut last_read, Duration::ZERO);
                if n == 0 {
                    break;
                }
                taken += n;
                tail.extend_from_slice(&buf[..n]);
                tail.drain(..tail.len().saturating_sub(16));
            }
            if taken <= 256 << 20 || !tail.ends_with(b"\r\n0\r\n\r\n") {
                outcome = Err(format!("dropped after the client took {taken} bytes"));
            }
        }
        drop(stream);
        let _ = tx.send(());
        server.await.unwrap();
        outcome.map_err(|why| (why, (longest_gap >= send_limit / 2).then_some(longest_gap)))
    }

    /// GH#682 review: an HTTP/2 client that takes a large single-frame
    /// response as fast as its 64 KiB window allows is not dropped.
    #[tokio::test]
    async fn http2_clients_taking_a_large_frame_steadily_are_not_dropped() {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let (addr, server) = one_connection_server(rx, Duration::from_secs(1)).await;
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (client, connection) = h2::client::Builder::new()
            .initial_window_size(64 * 1024)
            .initial_connection_window_size(64 * 1024)
            .handshake::<_, axum::body::Bytes>(stream)
            .await
            .unwrap();
        tokio::spawn(connection);
        let mut client = client.ready().await.unwrap();
        let request = axum::http::Request::get("http://x/single").body(()).unwrap();
        let (response, _) = client.send_request(request, true).unwrap();
        let mut body = response.await.unwrap().into_body();
        // A window update after every 32 KiB, 20 ms apart: about 1.6 MiB/s,
        // so the 8 MiB take five times the send limit.
        let mut taken = 0;
        let mut unreleased = 0;
        while let Some(data) = tokio::time::timeout(Duration::from_secs(3), body.data()).await.unwrap() {
            let n = data.unwrap_or_else(|e| panic!("dropped after the client took {taken} bytes: {e}")).len();
            taken += n;
            unreleased += n;
            if unreleased >= 32 * 1024 {
                tokio::time::sleep(Duration::from_millis(20)).await;
                body.flow_control().release_capacity(unreleased).unwrap();
                unreleased = 0;
            }
        }
        assert_eq!(taken, 8 << 20);
        drop(client);
        let _ = tx.send(());
        server.await.unwrap();
    }

    /// GH#557: connections that never finish their headers could pile up
    /// until the process ran out of file descriptors. Past the limit, new
    /// connections are closed at once, and closed ones free their place.
    #[tokio::test]
    async fn connections_past_the_limit_are_closed_until_one_closes() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let http = crate::config::HttpConfig {
            header_read_timeout: Duration::from_secs(30),
            request_timeout: Duration::from_secs(5),
            body_timeout: Duration::from_secs(5),
            send_timeout: Duration::from_secs(60),
            max_concurrent_requests: 512,
        };
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            // Loopback is a trusted proxy here, so only the global limit applies.
            let peers = PeerConnections::new(2, crate::auth::session::TrustedProxies::parse("127.0.0.0/8").unwrap());
            accept_loop(listener, app(), &http, 2, peers, async {
                let _ = rx.await;
            })
            .await
        });
        // Answers the request in full, or None when the server closed the connection unanswered.
        async fn healthz(addr: std::net::SocketAddr) -> Option<String> {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            let _ = stream.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").await;
            let mut buf = Vec::new();
            let read = tokio::time::timeout(Duration::from_secs(3), stream.read_to_end(&mut buf)).await;
            let read = read.expect("the server neither answered nor closed the connection");
            (read.is_ok() && !buf.is_empty()).then(|| String::from_utf8_lossy(&buf).into_owned())
        }
        let mut slow = Vec::new();
        for _ in 0..2 {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            stream.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n").await.unwrap();
            slow.push(stream);
        }
        // Both places taken by connections that are still sending headers.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(healthz(addr).await, None, "a connection past the limit was served");

        // One slow client leaves; its place is freed for the next client.
        drop(slow.pop());
        let mut answer = None;
        for _ in 0..50 {
            answer = healthz(addr).await;
            if answer.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let answer = answer.expect("the freed place was never given to a new connection");
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");

        drop(slow);
        let _ = tx.send(());
        server.await.unwrap();
    }

    /// GH#561: one client network (IPv4 /24) holds at most a quarter of the
    /// open connections; another network is still served, and peers in
    /// TRUSTED_PROXIES are not counted.
    #[tokio::test]
    async fn one_network_holds_at_most_its_share_of_the_connections() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // 127.0.0.1 and 127.0.1.1 are different /24s; Linux routes all of 127/8 to loopback.
        if !cfg!(target_os = "linux") {
            return;
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let http = crate::config::HttpConfig {
            header_read_timeout: Duration::from_secs(30),
            request_timeout: Duration::from_secs(5),
            body_timeout: Duration::from_secs(5),
            send_timeout: Duration::from_secs(60),
            max_concurrent_requests: 512,
        };
        let peers = PeerConnections::new(8, crate::auth::session::TrustedProxies::parse("127.0.2.1").unwrap());
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn({
            let peers = peers.clone();
            async move {
                accept_loop(listener, app(), &http, 8, peers, async {
                    let _ = rx.await;
                })
                .await
            }
        });
        async fn connect_from(from: &str, addr: std::net::SocketAddr) -> tokio::net::TcpStream {
            let socket = tokio::net::TcpSocket::new_v4().unwrap();
            socket.bind(format!("{from}:0").parse().unwrap()).unwrap();
            socket.connect(addr).await.unwrap()
        }
        // Answers the request in full, or None when the server closed the connection unanswered.
        async fn healthz(from: &str, addr: std::net::SocketAddr) -> Option<String> {
            let mut stream = connect_from(from, addr).await;
            let _ = stream.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").await;
            let mut buf = Vec::new();
            let read = tokio::time::timeout(Duration::from_secs(3), stream.read_to_end(&mut buf)).await;
            let read = read.expect("the server neither answered nor closed the connection");
            (read.is_ok() && !buf.is_empty()).then(|| String::from_utf8_lossy(&buf).into_owned())
        }
        async fn slow(from: &str, addr: std::net::SocketAddr, n: usize) -> Vec<tokio::net::TcpStream> {
            let mut held = Vec::new();
            for _ in 0..n {
                let mut stream = connect_from(from, addr).await;
                stream.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n").await.unwrap();
                held.push(stream);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            held
        }
        let net = crate::auth::throttle::Net::of(Some("127.0.0.1".parse().unwrap()));

        // One network fills its share (8 / 4 = 2) with connections still sending headers.
        let mut first = slow("127.0.0.1", addr, 2).await;
        assert_eq!(peers.by_net.of(net), 2);
        assert_eq!(healthz("127.0.0.1", addr).await, None, "a connection past the network's share was served");
        // Another network is still served.
        let answer = healthz("127.0.1.1", addr).await.expect("another network was refused");
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
        // A trusted proxy is not held to a share: 3 slow connections, and a fourth is served.
        let proxied = slow("127.0.2.1", addr, 3).await;
        let answer = healthz("127.0.2.1", addr).await.expect("a trusted proxy was held to a share");
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");

        // A closed connection gives its place back to its network.
        drop(first.pop());
        let mut answer = None;
        for _ in 0..50 {
            answer = healthz("127.0.0.1", addr).await;
            if answer.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(answer.expect("the freed place was never given back").starts_with("HTTP/1.1 200"));

        drop((first, proxied));
        let _ = tx.send(());
        server.await.unwrap();
        assert_eq!(peers.by_net.of(net), 0, "every place is given back");
    }

    #[test]
    fn a_wider_network_holds_at_most_half_of_the_connections() {
        let peers = PeerConnections::new(8, Default::default());
        // Four /24s of one /16, two connections each (their share): the /16 stops at 4.
        let holds: Vec<_> =
            ["10.1.1.1", "10.1.1.2", "10.1.2.1", "10.1.2.2"].iter().map(|ip| peers.hold(ip.parse().unwrap())).collect();
        assert!(holds.iter().all(Option::is_some));
        assert!(peers.hold("10.1.3.1".parse().unwrap()).is_none(), "the wider network is past its half");
        let other = peers.hold("10.2.3.1".parse().unwrap());
        assert!(other.is_some(), "another wider network is served");
        // IPv6: one /64 holds its quarter.
        let v6: Vec<_> =
            ["2001:db8:1:2::1", "2001:db8:1:2::2"].iter().map(|ip| peers.hold(ip.parse().unwrap())).collect();
        assert!(v6.iter().all(Option::is_some));
        assert!(peers.hold("2001:db8:1:2::3".parse().unwrap()).is_none(), "the /64 is past its quarter");
        assert!(peers.hold("2001:db8:1:3::1".parse().unwrap()).is_some(), "another /64 is served");
        drop(holds);
        assert!(peers.hold("10.1.3.1".parse().unwrap()).is_some(), "places are given back");
    }

    #[test]
    fn the_connection_limit_follows_request_slots_and_open_files() {
        // Four per request slot when descriptors are plentiful.
        assert_eq!(connection_limit(512, Some(1_048_576), 10), 2048);
        assert_eq!(connection_limit(512, None, 10), 2048);
        // A quarter of the open-files limit stays free...
        assert_eq!(connection_limit(512, Some(1024), 10), 768);
        assert_eq!(connection_limit(512, Some(65_536), 200), 2048);
        // ...and never less than the database pool plus 64 (GH#562).
        assert_eq!(connection_limit(512, Some(200), 10), 126);
        assert_eq!(connection_limit(512, Some(256), 200), 1);
        assert_eq!(connection_limit(512, Some(1024), 200), 760);
        // Never zero, and never more than a semaphore holds.
        assert_eq!(connection_limit(512, Some(10), 10), 1);
        assert_eq!(connection_limit(usize::MAX, None, 10), tokio::sync::Semaphore::MAX_PERMITS);
    }

    /// GH#628: start-up warns below `open_files_needed`, which is exactly the
    /// smallest open-files limit that leaves the full connection limit.
    #[test]
    fn open_files_needed_is_where_the_open_files_limit_stops_capping() {
        assert_eq!(open_files_needed(512, 10), 2730);
        assert_eq!(open_files_needed(512, 200), 2730);
        assert_eq!(open_files_needed(1, 200), 268);
        assert_eq!(open_files_needed(16, 1), 129);
        // The GH#628 repro: 256 open files with a pool of 200 is far below it.
        assert!(256 < open_files_needed(512, 200));
        for slots in [1, 2, 3, 7, 16, 100, 512, 1000, 4096] {
            for pool in [1, 10, 50, 200] {
                let full = connection_limit(slots, None, pool);
                let needed = open_files_needed(slots, pool);
                assert_eq!(connection_limit(slots, Some(needed), pool), full, "{slots} slots, pool {pool}");
                assert!(connection_limit(slots, Some(needed - 1), pool) < full, "{slots} slots, pool {pool}");
            }
        }
        let needed = open_files_needed(usize::MAX, 10);
        assert_eq!(connection_limit(usize::MAX, Some(needed), 10), tokio::sync::Semaphore::MAX_PERMITS);
    }

    /// GH#562: the soft open-files limit is raised to the hard one, so a
    /// systemd default of 1024 does not cap the connection limit at 768.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_soft_open_files_limit_is_raised_to_the_hard_limit() {
        raise_open_files_limit();
        let limits = std::fs::read_to_string("/proc/self/limits").unwrap();
        let line = limits.lines().find(|l| l.starts_with("Max open files")).unwrap();
        let mut fields = line["Max open files".len()..].split_whitespace();
        assert_eq!(fields.next(), fields.next(), "{line}");
    }

    #[test]
    fn the_open_files_limit_is_read_where_the_platform_shows_it() {
        if cfg!(target_os = "linux") {
            assert!(open_files_limit().is_some_and(|n| n > 0));
        }
    }

    /// GH#342: the public body budget grew with HTTP_MAX_CONCURRENT_REQUESTS
    /// without a ceiling (about 64 GiB at the 1,000,000 the setting accepts).
    #[test]
    fn the_public_body_budget_has_a_floor_and_a_ceiling() {
        const MIB: usize = 1024 * 1024;
        let timeout = Duration::from_secs(10);
        assert_eq!(Capacity::new(1, timeout).available_body_bytes(), 16 * MIB);
        assert_eq!(Capacity::new(512, timeout).available_body_bytes(), 32 * MIB);
        assert_eq!(Capacity::new(1_000_000, timeout).available_body_bytes(), 256 * MIB);
    }

    /// GH#342: one client network could hold the whole public body budget, so
    /// sign-ins from everywhere else that had to wait for their body got 503.
    #[test]
    fn one_client_network_holds_at_most_its_share_of_the_public_body_budget() {
        use crate::auth::throttle::Net;
        let net = |ip: &str| Net::of(Some(ip.parse().unwrap()));
        let capacity = Capacity::new(512, Duration::from_secs(10));
        let (budget, share) = (32 * 1024 * 1024, 2 * 1024 * 1024);
        let full = crate::api::route::PUBLIC_BODY_LIMIT;

        // A /24 fills its share, whichever of its addresses the bodies come from.
        let mut held = Vec::new();
        for i in 0..share / full {
            let mut hold = capacity.hold_public_body(net(&format!("203.0.113.{}", i % 250)));
            hold.add(full).unwrap();
            held.push(hold);
        }
        assert_eq!(capacity.held_body_bytes(net("203.0.113.1")), share);
        let mut more = capacity.hold_public_body(net("203.0.113.7"));
        assert_eq!(more.add(1).unwrap_err().code, ErrorCode::ServerBusy);
        // A refusal holds nothing, and other networks still get their share.
        assert_eq!(capacity.held_body_bytes(net("203.0.113.1")), share);
        assert_eq!(capacity.available_body_bytes(), budget - share);
        let mut other = capacity.hold_public_body(net("198.51.100.20"));
        other.add(full).unwrap();
        let mut v6 = capacity.hold_public_body(net("2001:db8::1"));
        v6.add(full).unwrap();
        assert_eq!(capacity.held_body_bytes(net("2001:db8::ffff")), full);

        // Dropped holds give back both the budget and the network's share.
        drop((held, more, other, v6));
        assert_eq!(capacity.available_body_bytes(), budget);
        assert_eq!(capacity.held_body_bytes(net("203.0.113.1")), 0);
        assert!(
            capacity.public_body_by_net.lock().is_empty() && capacity.public_body_by_wide.lock().is_empty(),
            "networks are forgotten once they hold nothing"
        );

        // The budget itself still bounds every network together.
        let mut held = Vec::new();
        for n in 0..PUBLIC_BODY_SHARES {
            let mut hold = capacity.hold_public_body(net(&format!("10.{n}.0.1")));
            hold.add(share).unwrap();
            held.push(hold);
        }
        let mut late = capacity.hold_public_body(net("10.200.0.1"));
        assert_eq!(late.add(1).unwrap_err().code, ErrorCode::ServerBusy);
        assert_eq!(capacity.held_body_bytes(net("10.200.0.1")), 0, "a refusal for the budget holds no share");
    }

    /// GH#504: one client with an IPv6 /56 holds 256 /64s, and so could fill
    /// all sixteen shares of the public body budget. A wider network (IPv6 /48,
    /// IPv4 /16) now holds at most a quarter of it.
    #[test]
    fn one_wider_network_holds_at_most_a_quarter_of_the_public_body_budget() {
        use crate::auth::throttle::Net;
        let net = |ip: &str| Net::of(Some(ip.parse().unwrap()));
        let capacity = Capacity::new(512, Duration::from_secs(10));
        let (budget, share) = (32 * 1024 * 1024, 2 * 1024 * 1024);

        for (prefix, other) in [("2001:db8:0:{n}00::1", "2001:db8:1::1"), ("198.51.{n}.1", "198.52.0.1")] {
            let mut held = Vec::new();
            for n in 0..PUBLIC_BODY_SHARES {
                let mut hold = capacity.hold_public_body(net(&prefix.replace("{n}", &n.to_string())));
                if hold.add(share).is_err() {
                    break;
                }
                held.push(hold);
            }
            assert_eq!(held.len() * share, budget / 4, "{prefix}");
            // Another network still gets its whole share.
            let mut hold = capacity.hold_public_body(net(other));
            hold.add(share).unwrap();
            drop((held, hold));
            assert_eq!(capacity.available_body_bytes(), budget);
        }
    }

    /// GH#501: each waiting public body was charged 4 KiB but costs about
    /// 66 kB of memory, so the budget admitted 16 times as many as it meant to.
    #[test]
    fn a_waiting_public_body_is_charged_what_it_costs() {
        use crate::auth::throttle::Net;
        assert_eq!(WAITING_BODY_COST, 64 * 1024);
        // The largest budget, from as many networks as it takes.
        let capacity = Capacity::new(1_000_000, Duration::from_secs(10));
        let mut held = Vec::new();
        'fill: for wide in 0..=u16::MAX {
            for n in 0..=u16::MAX {
                let ip = std::net::Ipv6Addr::new(0x2001, 0xdb8, wide, n, 0, 0, 0, 1);
                let mut hold = capacity.hold_public_body(Net::of(Some(ip.into())));
                match hold.add(WAITING_BODY_COST) {
                    Ok(()) => held.push(hold),
                    Err(_) if n == 0 => break 'fill,
                    Err(_) => break,
                }
            }
        }
        assert_eq!(held.len(), MAX_PUBLIC_BODY_BUDGET / (64 * 1024), "at most 4,096 bodies wait at once");
    }

    /// GH#502: one user's requests could hold every global permit while
    /// sending their bodies slowly, so everyone else got 503 SERVER_BUSY.
    #[test]
    fn one_user_holds_at_most_a_quarter_of_the_request_pool() {
        let capacity = Capacity::new(512, Duration::from_secs(10));
        let (alice, bob) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let held: Vec<_> = (0..128).map(|_| capacity.acquire_for_user(alice).unwrap()).collect();
        assert_eq!(capacity.acquire_for_user(alice).err().unwrap().code, ErrorCode::ServerBusy);
        assert_eq!(capacity.held_by_user(alice), 128, "a refusal holds nothing");
        assert_eq!(capacity.available(false), 512 - 128);
        let _bob = capacity.acquire_for_user(bob).unwrap();
        drop(held);
        assert_eq!(capacity.held_by_user(alice), 0);
        assert!(capacity.acquire_for_user(alice).is_ok(), "the permits were given back");

        // The pool still bounds every user together, and a tiny pool still admits one each.
        let small = Capacity::new(2, Duration::from_secs(10));
        let _a = small.acquire_for_user(alice).unwrap();
        assert!(small.acquire_for_user(alice).is_err());
        let _b = small.acquire_for_user(bob).unwrap();
        let carol = uuid::Uuid::new_v4();
        assert!(small.acquire_for_user(carol).is_err());
        assert_eq!(small.held_by_user(carol), 0, "a refusal for the pool holds no share");
    }
}
