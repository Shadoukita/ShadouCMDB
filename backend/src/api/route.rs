//! A route is declared once, with Rust types for its path, query, body and
//! response. The same declaration drives request validation (api/validate.rs),
//! the axum handler, and the OpenAPI document (api/openapi.rs), so the spec
//! cannot drift from the code and a route cannot exist without a spec entry.
//!
//! Access control is part of the declaration too: every route needs a signed-in
//! user unless it is marked [`RouteBuilder::public`], and may require a global
//! permission ([`RouteBuilder::requires`]). The session (or the API token of an
//! `Authorization: Bearer` header) is resolved, CSRF is checked for
//! state-changing methods on a session and the permission is checked before
//! the request is validated, so an unauthenticated caller learns nothing about
//! a route beyond 401. Routes marked [`RouteBuilder::session_only`] refuse
//! API tokens. A session whose user must set up MFA first reaches only the
//! routes marked [`RouteBuilder::before_mfa_enrolment`].

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Extension;
use axum::body::Body as RequestBody;
use axum::extract::{ConnectInfo, RawPathParams, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodFilter, MethodRouter, on};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use sqlx::PgPool;
use utoipa::openapi::path::{Parameter, ParameterBuilder, ParameterIn};
use utoipa::openapi::{RefOr, Required, schema::Schema};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::context::{ClientInfo, RequestContext, forbidden, unauthenticated};
use super::schemas;
use super::validate::{self, QueryParam};
use crate::auth::permissions::GlobalPermission;
use crate::auth::{self, AuthState};
use crate::http::AppState;
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::http::request_id;

/// What a handler gets besides its validated inputs.
pub struct Api {
    pub pool: PgPool,
    pub ctx: RequestContext,
    pub auth: Arc<AuthState>,
    /// Request headers (the auth routes read cookies and the forwarded protocol).
    pub headers: HeaderMap,
}

/// Who may call a route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// No session needed (health, login, first-run setup).
    Public,
    /// Any signed-in user; the service may still check class permissions.
    Authenticated,
    /// A signed-in user holding this global permission.
    Permission(GlobalPermission),
}

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

pub trait PathInput: Sized + Send + 'static {
    fn params() -> Vec<Parameter>;
    fn parse(raw: &RawPathParams) -> Result<Self, AppError>;
}

/// No path parameters.
pub struct NoPath;

impl PathInput for NoPath {
    fn params() -> Vec<Parameter> {
        Vec::new()
    }
    fn parse(_: &RawPathParams) -> Result<Self, AppError> {
        Ok(NoPath)
    }
}

/// `{id}`: a uuid.
pub struct IdPath(pub Uuid);

impl PathInput for IdPath {
    fn params() -> Vec<Parameter> {
        vec![
            ParameterBuilder::new()
                .name("id")
                .parameter_in(ParameterIn::Path)
                .required(Required::True)
                .schema(Some(schemas::uuid_builder()))
                .build(),
        ]
    }
    fn parse(raw: &RawPathParams) -> Result<Self, AppError> {
        let value = raw.iter().find(|(k, _)| *k == "id").map(|(_, v)| v).unwrap_or_default();
        match validate::is_uuid(value).then(|| Uuid::parse_str(value).ok()).flatten() {
            Some(id) => Ok(IdPath(id)),
            None => Err(AppError::validation(vec![FieldError {
                location: FieldLocation::Params,
                field: "id".into(),
                message: "Invalid UUID".into(),
                code: "invalid_format".into(),
            }])),
        }
    }
}

/// `{key}`: a stable machine key (lower_snake_case).
pub struct KeyPath(pub String);

impl PathInput for KeyPath {
    fn params() -> Vec<Parameter> {
        vec![
            ParameterBuilder::new()
                .name("key")
                .parameter_in(ParameterIn::Path)
                .required(Required::True)
                .schema(Some(schemas::key_schema()))
                .build(),
        ]
    }
    fn parse(raw: &RawPathParams) -> Result<Self, AppError> {
        let value = raw.iter().find(|(k, _)| *k == "key").map(|(_, v)| v).unwrap_or_default();
        match validate::cached_regex(schemas::KEY_PATTERN) {
            Some(re) if re.is_match(value) => Ok(KeyPath(value.to_owned())),
            _ => Err(AppError::validation(vec![FieldError {
                location: FieldLocation::Params,
                field: "key".into(),
                message: "Invalid key".into(),
                code: "invalid_format".into(),
            }])),
        }
    }
}

pub trait QueryInput: Sized + Send + 'static {
    fn params() -> Vec<Parameter>;
    fn parse(raw: Option<&str>) -> Result<Self, AppError>;
}

/// The route takes no query parameters (any query string is ignored).
pub struct NoQuery;

impl QueryInput for NoQuery {
    fn params() -> Vec<Parameter> {
        Vec::new()
    }
    fn parse(_: Option<&str>) -> Result<Self, AppError> {
        Ok(NoQuery)
    }
}

/// A validated query string of type `T`; unknown keys are rejected.
pub struct Query<T>(pub T);

fn query_params<T: IntoParams>() -> Vec<Parameter> {
    T::into_params(|| Some(ParameterIn::Query))
}

impl<T: IntoParams + DeserializeOwned + Send + 'static> QueryInput for Query<T> {
    fn params() -> Vec<Parameter> {
        query_params::<T>()
    }

    fn parse(raw: Option<&str>) -> Result<Self, AppError> {
        let spec = validate::cached_schema::<T>(|| {
            serde_json::to_value(query_params::<T>()).unwrap_or(Value::Array(Vec::new()))
        });
        let params: Vec<QueryParam> = spec
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|p| QueryParam {
                        name: p["name"].as_str().unwrap_or_default().to_owned(),
                        required: p["required"].as_bool().unwrap_or(false),
                        schema: p.get("schema").cloned().unwrap_or(Value::Null),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let value = validate::parse_query(raw, &params).map_err(AppError::validation)?;
        deserialize(value, FieldLocation::Query).map(Query)
    }
}

pub trait BodyInput: Sized + Send + 'static {
    fn schema() -> Option<RefOr<Schema>>;
    fn parse(body: Option<Value>) -> Result<Self, AppError>;
}

/// The route takes no body.
pub struct NoBody;

impl BodyInput for NoBody {
    fn schema() -> Option<RefOr<Schema>> {
        None
    }
    fn parse(_: Option<Value>) -> Result<Self, AppError> {
        Ok(NoBody)
    }
}

/// Rules a body must satisfy beyond its schema (cross-field checks).
pub trait Check {
    fn check(&self) -> Vec<FieldError> {
        Vec::new()
    }
}

/// A validated JSON body of type `T`.
pub struct Body<T>(pub T);

impl<T: ToSchema + DeserializeOwned + Check + Send + 'static> BodyInput for Body<T> {
    fn schema() -> Option<RefOr<Schema>> {
        Some(T::schema())
    }

    fn parse(body: Option<Value>) -> Result<Self, AppError> {
        parse_body(required_body(body)?).map(Body).map_err(|e| AppError::validation(e.errors))
    }
}

/// A body that failed validation: the raw JSON and what is wrong with it.
pub struct InvalidBody {
    pub raw: Value,
    pub errors: Vec<FieldError>,
}

/// A JSON body of type `T` whose validation errors go to the handler instead
/// of straight to a 400, so it can add the errors only it can find (rules that
/// need the database) and report everything in one response.
pub struct CheckedBody<T>(pub Result<T, InvalidBody>);

impl<T: ToSchema + DeserializeOwned + Check + Send + 'static> BodyInput for CheckedBody<T> {
    fn schema() -> Option<RefOr<Schema>> {
        Some(T::schema())
    }

    fn parse(body: Option<Value>) -> Result<Self, AppError> {
        Ok(CheckedBody(parse_body(required_body(body)?)))
    }
}

fn required_body(body: Option<Value>) -> Result<Value, AppError> {
    body.ok_or_else(|| {
        AppError::validation(vec![FieldError {
            location: FieldLocation::Body,
            field: "(root)".into(),
            message: "Required".into(),
            code: "required".into(),
        }])
    })
}

fn parse_body<T: ToSchema + DeserializeOwned + Check + 'static>(value: Value) -> Result<T, InvalidBody> {
    // The body schema plus the components it references.
    let spec = validate::cached_schema::<T>(|| {
        let mut components = Vec::new();
        T::schemas(&mut components);
        let components: serde_json::Map<String, Value> = components
            .into_iter()
            .map(|(name, schema)| (name, serde_json::to_value(schema).unwrap_or(Value::Null)))
            .collect();
        serde_json::json!({ "schema": T::schema(), "components": components })
    });
    let errors = validate::check(&spec["schema"], &value, FieldLocation::Body, spec["components"].as_object());
    if !errors.is_empty() {
        return Err(InvalidBody { raw: value, errors });
    }
    let parsed: T = match deserialize(value.clone(), FieldLocation::Body) {
        Ok(parsed) => parsed,
        Err(e) => return Err(InvalidBody { raw: value, errors: e.details.unwrap_or_default() }),
    };
    let errors = parsed.check();
    if !errors.is_empty() {
        return Err(InvalidBody { raw: value, errors });
    }
    Ok(parsed)
}

/// After schema validation deserialisation only fails in custom deserialisers,
/// which report "code|message".
fn deserialize<T: DeserializeOwned>(value: Value, location: FieldLocation) -> Result<T, AppError> {
    serde_path_to_error::deserialize(value).map_err(|err| {
        let path = err.path().to_string();
        let inner = err.into_inner().to_string();
        let mut field = if path == "." || path.is_empty() { "(root)".to_owned() } else { path };
        let (code, message) = if let Some(rest) = inner.strip_prefix("missing field `") {
            // Only reachable if a schema forgets to mark a field required.
            let name = rest.split('`').next().unwrap_or_default();
            field = if field == "(root)" { name.to_owned() } else { format!("{field}.{name}") };
            ("required", "Required")
        } else {
            inner.split_once('|').unwrap_or(("invalid", inner.as_str()))
        };
        AppError::validation(vec![FieldError { location, field, message: message.to_owned(), code: code.to_owned() }])
    })
}

// ---------------------------------------------------------------------------
// Outputs
// ---------------------------------------------------------------------------

/// A response schema and the component schemas it references.
pub struct ResponseDoc {
    pub name: String,
    pub schema: RefOr<Schema>,
    pub nested: Vec<(String, RefOr<Schema>)>,
    /// Content type of the success body. Non-JSON bodies are documented inline, not as components.
    pub media_type: &'static str,
}

pub trait Output: Send + 'static {
    fn doc() -> Option<ResponseDoc>;
    fn respond(self, status: StatusCode) -> Response;
}

/// 204 No Content.
pub struct NoContent;

impl Output for NoContent {
    fn doc() -> Option<ResponseDoc> {
        None
    }
    fn respond(self, _: StatusCode) -> Response {
        StatusCode::NO_CONTENT.into_response()
    }
}

fn doc_of<T: ToSchema>() -> ResponseDoc {
    let mut nested = Vec::new();
    T::schemas(&mut nested);
    ResponseDoc { name: T::name().into_owned(), schema: T::schema(), nested, media_type: "application/json" }
}

/// A JSON body with the route's success status.
pub struct Json<T>(pub T);

impl<T: ToSchema + Serialize + Send + 'static> Output for Json<T> {
    fn doc() -> Option<ResponseDoc> {
        Some(doc_of::<T>())
    }
    fn respond(self, status: StatusCode) -> Response {
        (status, axum::Json(self.0)).into_response()
    }
}

/// A JSON body with a status chosen by the handler (e.g. /readyz answers 503).
pub struct WithStatus<T>(pub StatusCode, pub T);

impl<T: ToSchema + Serialize + Send + 'static> Output for WithStatus<T> {
    fn doc() -> Option<ResponseDoc> {
        Some(doc_of::<T>())
    }
    fn respond(self, _: StatusCode) -> Response {
        (self.0, axum::Json(self.1)).into_response()
    }
}

/// Raw bytes with their own content type (uploaded images). Documented as `image/*`.
pub struct Binary {
    pub content_type: HeaderValue,
    pub body: Vec<u8>,
}

impl Output for Binary {
    fn doc() -> Option<ResponseDoc> {
        let schema = utoipa::openapi::schema::ObjectBuilder::new()
            .schema_type(utoipa::openapi::schema::Type::String)
            .content_media_type("application/octet-stream")
            .into();
        Some(ResponseDoc { name: String::new(), schema: RefOr::T(schema), nested: Vec::new(), media_type: "image/*" })
    }
    fn respond(self, status: StatusCode) -> Response {
        (status, [(header::CONTENT_TYPE, self.content_type)], self.body).into_response()
    }
}

/// Another output plus response headers (ETag, Cache-Control, Content-Disposition).
pub struct WithHeaders<R>(pub R, pub Vec<(header::HeaderName, HeaderValue)>);

impl<R: Output> Output for WithHeaders<R> {
    fn doc() -> Option<ResponseDoc> {
        R::doc()
    }
    fn respond(self, status: StatusCode) -> Response {
        let mut res = self.0.respond(status);
        for (name, value) in self.1 {
            res.headers_mut().insert(name, value);
        }
        res
    }
}

/// A bare status with no body (304 Not Modified).
pub struct StatusOnly(pub StatusCode);

/// Either of two outputs; documented as the first.
pub enum Either<A, B> {
    Left(A),
    Right(B),
}

impl<A: Output, B: Output> Output for Either<A, B> {
    fn doc() -> Option<ResponseDoc> {
        A::doc()
    }
    fn respond(self, status: StatusCode) -> Response {
        match self {
            Either::Left(a) => a.respond(status),
            Either::Right(b) => b.respond(status),
        }
    }
}

impl Output for StatusOnly {
    fn doc() -> Option<ResponseDoc> {
        None
    }
    fn respond(self, _: StatusCode) -> Response {
        self.0.into_response()
    }
}

/// An error answer that still sets cookies (sign-in: the second factor is due).
/// Documented through the route's error codes.
pub struct ErrorWithCookies(pub AppError, pub Vec<HeaderValue>);

impl Output for ErrorWithCookies {
    fn doc() -> Option<ResponseDoc> {
        None
    }
    fn respond(self, _: StatusCode) -> Response {
        let mut res = self.0.into_response();
        for c in self.1 {
            res.headers_mut().append(header::SET_COOKIE, c);
        }
        res
    }
}

/// A redirect for a browser navigation (OIDC sign-in): the route's 3xx
/// status, `Location`, Set-Cookie headers and no body. Never cached.
pub struct Redirect {
    pub location: String,
    pub cookies: Vec<HeaderValue>,
}

impl Output for Redirect {
    fn doc() -> Option<ResponseDoc> {
        None
    }
    fn respond(self, status: StatusCode) -> Response {
        let Ok(location) = HeaderValue::from_str(&self.location) else {
            tracing::error!("redirect target is not a valid header value");
            return AppError::internal().into_response();
        };
        let mut res = status.into_response();
        let headers = res.headers_mut();
        headers.insert(header::LOCATION, location);
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        for c in self.cookies {
            headers.append(header::SET_COOKIE, c);
        }
        res
    }
}

/// Another output plus Set-Cookie headers (login, logout, first-run setup).
pub struct WithCookies<R>(pub R, pub Vec<HeaderValue>);

impl<R: Output> Output for WithCookies<R> {
    fn doc() -> Option<ResponseDoc> {
        R::doc()
    }
    fn respond(self, status: StatusCode) -> Response {
        let mut res = self.0.respond(status);
        for c in self.1 {
            res.headers_mut().append(header::SET_COOKIE, c);
        }
        res
    }
}

// ---------------------------------------------------------------------------
// Route
// ---------------------------------------------------------------------------

pub struct Route {
    pub method: Method,
    /// OpenAPI and axum path syntax, e.g. /api/v1/statuses/{id}
    pub path: String,
    pub operation_id: String,
    pub tag: String,
    pub summary: String,
    pub description: Option<String>,
    /// Marked deprecated in the OpenAPI document (still served).
    pub deprecated: bool,
    pub status: StatusCode,
    pub access: Access,
    /// API tokens are refused (403): the route needs a browser session.
    pub session_only: bool,
    /// Answers a session that must set up MFA before anything else.
    pub before_mfa_enrolment: bool,
    /// Error codes beyond VALIDATION_ERROR / INTERNAL_ERROR / DATABASE_UNAVAILABLE
    /// (and the 401/403 implied by `access`).
    pub errors: Vec<ErrorCode>,
    /// Other statuses that return the success schema.
    pub also_returns: Vec<(StatusCode, String)>,
    pub path_params: Vec<Parameter>,
    pub query_params: Vec<Parameter>,
    pub body: Option<RefOr<Schema>>,
    pub response: Option<ResponseDoc>,
    pub handler: MethodRouter<AppState>,
}

pub struct RouteBuilder {
    method: Method,
    path: String,
    operation_id: String,
    tag: String,
    summary: String,
    description: Option<String>,
    status: Option<StatusCode>,
    access: Access,
    session_only: bool,
    before_mfa_enrolment: bool,
    errors: Vec<ErrorCode>,
    also_returns: Vec<(StatusCode, String)>,
    body_limit: Option<usize>,
}

pub fn route(method: Method, path: impl Into<String>, operation_id: impl Into<String>) -> RouteBuilder {
    RouteBuilder {
        method,
        path: path.into(),
        operation_id: operation_id.into(),
        tag: String::new(),
        summary: String::new(),
        description: None,
        status: None,
        access: Access::Authenticated,
        session_only: false,
        before_mfa_enrolment: false,
        errors: Vec::new(),
        also_returns: Vec::new(),
        body_limit: None,
    }
}

/// Handler inputs as one value, so handlers are `|api, In(path, query, body)|`.
pub struct In<P, Q, B>(pub P, pub Q, pub B);

impl RouteBuilder {
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.tag = tag.into();
        self
    }
    pub fn summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = summary.into();
        self
    }
    /// An empty description is ignored.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        let description = description.into();
        if !description.is_empty() {
            self.description = Some(description);
        }
        self
    }
    /// Success status; defaults to 200, or 204 when there is no response body.
    pub fn status(mut self, status: StatusCode) -> Self {
        self.status = Some(status);
        self
    }
    /// Callable without a session.
    pub fn public(mut self) -> Self {
        self.access = Access::Public;
        self
    }
    /// Requires this global permission (403 FORBIDDEN without it).
    pub fn requires(mut self, permission: GlobalPermission) -> Self {
        self.access = Access::Permission(permission);
        self
    }
    /// Refuse API tokens (403 FORBIDDEN): sign-out, password changes, token,
    /// user account and identity provider administration need a signed-in
    /// session, so a token cannot outlive its revocation by minting another
    /// credential (a token, an account, a password or a sign-in path).
    pub fn session_only(mut self) -> Self {
        self.session_only = true;
        self
    }
    /// Reachable by a session whose user holds a profile requiring MFA and has
    /// not set it up yet (sign-out, the current session, MFA set-up). Every
    /// other route answers such a session 403 MFA_ENROLMENT_REQUIRED.
    pub fn before_mfa_enrolment(mut self) -> Self {
        self.before_mfa_enrolment = true;
        self
    }
    /// The service checks per-class permissions, so the route can answer 403.
    pub fn class_checked(self) -> Self {
        self.errors(&[ErrorCode::Forbidden])
    }
    pub fn errors(mut self, errors: &[ErrorCode]) -> Self {
        for e in errors {
            if !self.errors.contains(e) {
                self.errors.push(*e);
            }
        }
        self
    }
    /// Accept bodies up to this many bytes instead of [`BODY_LIMIT`] (config import).
    pub fn body_limit(mut self, bytes: usize) -> Self {
        self.body_limit = Some(bytes);
        self
    }
    pub fn also_returns(mut self, status: StatusCode, description: impl Into<String>) -> Self {
        self.also_returns.push((status, description.into()));
        self
    }

    pub fn handle<P, Q, B, R, F, Fut>(self, f: F) -> Route
    where
        P: PathInput,
        Q: QueryInput,
        B: BodyInput,
        R: Output,
        F: Fn(Api, In<P, Q, B>) -> Fut + Clone + Send + Sync + 'static,
        Fut: Future<Output = Result<R, AppError>> + Send + 'static,
    {
        let response = R::doc();
        let status = self.status.unwrap_or(if response.is_some() { StatusCode::OK } else { StatusCode::NO_CONTENT });
        let filter = MethodFilter::try_from(self.method.clone()).expect("supported HTTP method");
        let access = self.access;
        let session_only = self.session_only;
        let before_mfa_enrolment = self.before_mfa_enrolment;
        let safe_method = self.method == Method::GET || self.method == Method::HEAD;
        let body_limit = self.body_limit.unwrap_or(match access {
            Access::Public => PUBLIC_BODY_LIMIT,
            _ => BODY_LIMIT,
        });
        let (method, operation_id) = (self.method.clone(), Arc::<str>::from(self.operation_id.as_str()));

        let handler = move |State(state): State<AppState>,
                            uri: Uri,
                            raw_path: RawPathParams,
                            RawQuery(raw_query): RawQuery,
                            headers: HeaderMap,
                            peer: Option<Extension<ConnectInfo<SocketAddr>>>,
                            body: RequestBody| {
            let f = f.clone();
            let (method, operation_id) = (method.clone(), operation_id.clone());
            async move {
                let run = async move {
                    // AUDIT_CAPTURE_*: what is not captured is never stored (sessions, audit_log) or logged.
                    let capture = state.capture;
                    let peer = peer.map(|Extension(ConnectInfo(a))| a.ip());
                    let peer_ip = peer.filter(|_| capture.ip);
                    let client = ClientInfo {
                        ip: auth::session::client_ip(&headers, peer_ip).filter(|_| capture.ip),
                        peer_ip,
                        user_agent: auth::session::user_agent(&headers).filter(|_| capture.user_agent),
                        net: auth::throttle::Net::of(auth::session::client_ip(&headers, peer)),
                    };
                    let used = auth::token::Use { method: &method, path: uri.path(), operation_id: &operation_id };
                    let rule = Rule { access, session_only, before_mfa_enrolment, safe_method };
                    // Authorise before reading the body: an anonymous caller must not make
                    // the server buffer up to body_limit bytes only to be answered 401.
                    let ctx = authorise(&state, &headers, rule, client, used).await?;
                    // The permit is taken only once the caller is authorised, so rejected
                    // requests never hold capacity. Public routes that take a body draw from
                    // their own pool and get a short deadline for it, so anonymous slow
                    // senders cannot hold the capacity signed-in users need; bodiless public
                    // routes (health) take none.
                    let public = access == Access::Public;
                    let _permit = match (public, safe_method) {
                        (true, true) => None,
                        _ => Some(state.capacity.acquire(public)?),
                    };
                    let body = if public {
                        let limit = state.capacity.public_body_timeout;
                        tokio::time::timeout(limit, read_body(&headers, body, body_limit)).await.map_err(|_| {
                            AppError::new(
                                ErrorCode::RequestTimeout,
                                format!("The request body was not received within {} s", limit.as_secs()),
                            )
                        })??
                    } else {
                        read_body(&headers, body, body_limit).await?
                    };
                    let input = In(P::parse(&raw_path)?, Q::parse(raw_query.as_deref())?, B::parse(body)?);
                    let upgrade = ctx
                        .principal()
                        .and_then(|p| p.csrf_token())
                        .and_then(|csrf| auth::session::upgrade_cookies(&state.auth.config, &headers, csrf));
                    let api = Api { pool: state.pool, ctx, auth: state.auth, headers };
                    let mut res = f(api, input).await?.respond(status);
                    // A session from before the __Host- names moves over on its first
                    // HTTPS answer, unless the route set the session cookies itself (logout).
                    if let Some(cookies) = upgrade
                        && !res.headers().contains_key(header::SET_COOKIE)
                    {
                        res.headers_mut().extend(cookies.into_iter().map(|c| (header::SET_COOKIE, c)));
                    }
                    Ok::<_, AppError>(res)
                };
                run.await.unwrap_or_else(IntoResponse::into_response)
            }
        };

        Route {
            method: self.method,
            path: self.path,
            operation_id: self.operation_id,
            tag: self.tag,
            summary: self.summary,
            description: self.description,
            deprecated: false,
            status,
            access,
            session_only,
            before_mfa_enrolment,
            errors: self.errors,
            also_returns: self.also_returns,
            path_params: P::params(),
            query_params: Q::params(),
            body: B::schema(),
            response,
            handler: on(filter, handler),
        }
    }
}

/// A route's access rule.
#[derive(Clone, Copy)]
struct Rule {
    access: Access,
    session_only: bool,
    before_mfa_enrolment: bool,
    safe_method: bool,
}

/// Resolves the caller and enforces the route's access rule: 401 without a
/// live session or a valid API token, 403 CSRF_TOKEN_INVALID for a
/// state-changing request without the session's token, 403 FORBIDDEN without
/// the required permission (or for a token on a session-only route), 403
/// MFA_ENROLMENT_REQUIRED for a session that must set up MFA first.
///
/// An `Authorization: Bearer` header selects token authentication and the
/// cookies are then ignored: a bad token is 401, never a fall-back to the
/// session, so the header cannot be used to skip the session's CSRF check.
async fn authorise(
    state: &AppState,
    headers: &HeaderMap,
    rule: Rule,
    client: ClientInfo,
    used: auth::token::Use<'_>,
) -> Result<RequestContext, AppError> {
    let request_id = request_id::current();
    if rule.access == Access::Public {
        return Ok(RequestContext::anonymous(request_id).with_client(client));
    }
    if let Some(secret) = auth::token::bearer(headers) {
        let required = match rule.access {
            Access::Permission(p) => Some(p),
            _ => None,
        };
        return auth::token::authenticate(&state.pool, secret, required, rule.session_only, request_id, client, used)
            .await;
    }
    let Some(principal) = auth::authenticate(&state.pool, &state.auth.config, headers, &client).await? else {
        return Err(unauthenticated());
    };
    if !rule.safe_method && !auth::csrf_ok(&principal, headers) {
        return Err(AppError::new(
            ErrorCode::CsrfTokenInvalid,
            "Missing or wrong X-CSRF-Token header (send the csrfToken from /api/v1/auth/me)",
        ));
    }
    if principal.mfa_enrolment_required() && !rule.before_mfa_enrolment {
        return Err(AppError::new(
            ErrorCode::MfaEnrolmentRequired,
            "Your permission profile requires two-factor authentication: set it up first (POST /api/v1/auth/mfa/totp)",
        ));
    }
    if let Access::Permission(p) = rule.access
        && !principal.permissions.has(p)
    {
        return Err(forbidden(format!("This requires the {} permission", p.as_str())));
    }
    Ok(RequestContext::user(Arc::new(principal), request_id).with_client(client))
}

/// Largest request body a route accepts unless it sets [`RouteBuilder::body_limit`].
pub const BODY_LIMIT: usize = 1024 * 1024;
/// Largest body of a public route (setup, sign-in): anyone can send one, and
/// none of them needs more than a few hundred bytes.
pub const PUBLIC_BODY_LIMIT: usize = 64 * 1024;

/// Reads the body, at most `limit` bytes: a larger declared Content-Length is
/// refused without reading, a larger streamed body as soon as it passes the limit.
/// JSON is the only accepted body type. An empty body counts as no body
/// (clients often send Content-Type: application/json on DELETE).
async fn read_body(headers: &HeaderMap, body: RequestBody, limit: usize) -> Result<Option<Value>, AppError> {
    let too_large = || AppError::new(ErrorCode::PayloadTooLarge, "Request body is too large");
    let declared =
        headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
    if declared.is_some_and(|n| n > limit as u64) {
        return Err(too_large());
    }
    let bytes = match axum::body::to_bytes(body, limit).await {
        Ok(b) => b,
        Err(e) if is_length_limit(&e) => return Err(too_large()),
        Err(e) => return Err(root_error(&format!("Failed to read the request body: {e}"), "bad_request")),
    };
    if bytes.is_empty() {
        return Ok(None);
    }
    let is_json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|essence| essence.trim().eq_ignore_ascii_case("application/json"));
    if !is_json {
        return Err(AppError::new(ErrorCode::UnsupportedMediaType, "Request bodies must be application/json"));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| root_error(&format!("Body is not valid JSON: {e}"), "invalid_json"))
}

fn is_length_limit(err: &axum::Error) -> bool {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = source {
        if e.is::<http_body_util::LengthLimitError>() {
            return true;
        }
        source = e.source();
    }
    false
}

fn root_error(message: &str, code: &str) -> AppError {
    let mut err = AppError::validation(vec![FieldError {
        location: FieldLocation::Body,
        field: "(root)".into(),
        message: message.to_owned(),
        code: code.to_owned(),
    }]);
    err.message = message.to_owned();
    err
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use axum::body::{Body, Bytes};
    use axum::http::{Request, header};
    use futures_util::stream;
    use serde_json::json;
    use tower::ServiceExt;

    use std::time::{Duration, Instant};

    use axum::http::HeaderMap;

    use crate::db::scratch;
    use crate::http::Capacity;
    use crate::modules::api_tokens::tests::{Creds, app, app_with_capacity, call, code};

    const IMPORT: &str = "/api/v1/admin/config/import";

    /// A body of `len` bytes in one chunk that records whether it was ever polled.
    fn spy_body(len: usize) -> (Body, Arc<AtomicBool>) {
        let polled = Arc::new(AtomicBool::new(false));
        let flag = polled.clone();
        let chunk = stream::once(async move {
            flag.store(true, Ordering::SeqCst);
            Ok::<_, std::convert::Infallible>(Bytes::from(vec![b' '; len]))
        });
        (Body::from_stream(chunk), polled)
    }

    async fn send(
        app: &axum::Router,
        method: &str,
        path: &str,
        creds: &Creds,
        body: Body,
        content_length: Option<usize>,
    ) -> (u16, String) {
        let (status, code, _) = send_full(app, method, path, creds, body, content_length).await;
        (status, code)
    }

    async fn send_full(
        app: &axum::Router,
        method: &str,
        path: &str,
        creds: &Creds,
        body: Body,
        content_length: Option<usize>,
    ) -> (u16, String, HeaderMap) {
        let mut req = Request::builder().method(method).uri(path).header(header::CONTENT_TYPE, "application/json");
        if let Some(n) = content_length {
            req = req.header(header::CONTENT_LENGTH, n);
        }
        if let Some(c) = &creds.cookie {
            req = req.header(header::COOKIE, c);
        }
        if let Some(c) = &creds.csrf {
            req = req.header("x-csrf-token", c);
        }
        if let Some(b) = &creds.bearer {
            req = req.header(header::AUTHORIZATION, format!("Bearer {b}"));
        }
        let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
        let (status, headers) = (res.status().as_u16(), res.headers().clone());
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
        (status, code(&v).to_owned(), headers)
    }

    /// Runs first-run setup and returns the owner's session.
    async fn set_up_owner(app: &axum::Router) -> Creds {
        let setup = json!({ "username": "owner", "displayName": "Owner", "password": "correct horse battery" });
        let (status, me, headers) = call(app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
        assert_eq!(status, 201, "{me}");
        let cookie = headers
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned())
            .collect::<Vec<_>>()
            .join("; ");
        Creds { cookie: Some(cookie), csrf: me["csrfToken"].as_str().map(str::to_owned), bearer: None }
    }

    /// GH#181: bodies (16 MiB on config import) were buffered before the
    /// caller was authenticated, so anonymous clients could exhaust memory.
    #[tokio::test]
    async fn bodies_are_read_only_after_authorisation_and_within_the_route_limit() {
        let Some(db) = scratch::database("bodies_are_read_only_after_authorisation").await else { return };
        let app = app(db.pool.clone());
        const MIB: usize = 1024 * 1024;

        // Anonymous, and with a bad API token: 401 and the body is never polled.
        let (body, polled) = spy_body(16 * MIB);
        assert_eq!(
            send(&app, "POST", IMPORT, &Creds::default(), body, Some(16 * MIB)).await,
            (401, "UNAUTHENTICATED".into())
        );
        assert!(!polled.load(Ordering::SeqCst), "an anonymous request body was read");
        let bad_token = Creds { bearer: Some("scmdb_nope".into()), ..Creds::default() };
        let (body, polled) = spy_body(16 * MIB);
        assert_eq!(send(&app, "POST", IMPORT, &bad_token, body, None).await, (401, "UNAUTHENTICATED".into()));
        assert!(!polled.load(Ordering::SeqCst), "the body of a request with a bad token was read");

        // Public routes (sign-in) take at most 64 KiB, read or declared.
        let (body, _) = spy_body(super::PUBLIC_BODY_LIMIT + 1);
        let login = "/api/v1/auth/login";
        assert_eq!(send(&app, "POST", login, &Creds::default(), body, None).await, (413, "PAYLOAD_TOO_LARGE".into()));
        let (body, polled) = spy_body(1);
        let declared = Some(super::PUBLIC_BODY_LIMIT + 1);
        assert_eq!(
            send(&app, "POST", login, &Creds::default(), body, declared).await,
            (413, "PAYLOAD_TOO_LARGE".into())
        );
        assert!(!polled.load(Ordering::SeqCst), "an oversized declared body was read");

        let session = set_up_owner(&app).await;

        // Signed in, but without the CSRF token: still refused before the body is read.
        let no_csrf = Creds { csrf: None, ..session.clone() };
        let (body, polled) = spy_body(16 * MIB);
        assert_eq!(send(&app, "POST", IMPORT, &no_csrf, body, None).await, (403, "CSRF_TOKEN_INVALID".into()));
        assert!(!polled.load(Ordering::SeqCst));

        // Authorised: the route's own limit applies, declared or streamed.
        let (body, polled) = spy_body(1);
        assert_eq!(
            send(&app, "POST", IMPORT, &session, body, Some(16 * MIB + 1)).await,
            (413, "PAYLOAD_TOO_LARGE".into())
        );
        assert!(!polled.load(Ordering::SeqCst), "an oversized declared body was read");
        let (body, _) = spy_body(16 * MIB + 1);
        assert_eq!(send(&app, "POST", IMPORT, &session, body, None).await, (413, "PAYLOAD_TOO_LARGE".into()));
        // Up to 16 MiB reaches the import, which parses it (whitespace is not JSON).
        let (body, polled) = spy_body(2 * MIB);
        assert_eq!(send(&app, "POST", IMPORT, &session, body, None).await, (400, "VALIDATION_ERROR".into()));
        assert!(polled.load(Ordering::SeqCst));
        // Other routes keep the 1 MiB default.
        let (body, _) = spy_body(super::BODY_LIMIT + 1);
        let settings = "/api/v1/ui-settings";
        assert_eq!(send(&app, "PUT", settings, &session, body, None).await, (413, "PAYLOAD_TOO_LARGE".into()));

        db.drop().await;
    }

    /// PR #212 review: anonymous callers that send a public route's body slowly
    /// must not hold the capacity signed-in users need, and must be cut off
    /// after HTTP_HEADER_READ_TIMEOUT_SECS instead of HTTP_REQUEST_TIMEOUT_SECS.
    #[tokio::test]
    async fn slow_public_bodies_cannot_exhaust_the_capacity_of_signed_in_users() {
        let Some(db) = scratch::database("slow_public_bodies_cannot_exhaust_capacity").await else { return };
        const PUBLIC: usize = 2;
        let capacity = Capacity::with_sizes(1, PUBLIC, Duration::from_millis(500));
        let app = app_with_capacity(db.pool.clone(), capacity.clone());
        let session = set_up_owner(&app).await;
        let (login, me) = ("/api/v1/auth/login", "/api/v1/auth/me");
        let never = || Body::from_stream(stream::pending::<Result<Bytes, std::convert::Infallible>>());

        // Fill the public pool with sign-ins whose body never arrives.
        let started = Instant::now();
        let slow: Vec<_> = (0..PUBLIC)
            .map(|_| {
                let app = app.clone();
                tokio::spawn(async move { send(&app, "POST", login, &Creds::default(), never(), Some(100)).await })
            })
            .collect();
        while capacity.available(true) > 0 {
            assert!(started.elapsed() < Duration::from_secs(5), "the slow sign-ins never started");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // One more is refused at once, without reading its body.
        let (body, polled) = spy_body(10);
        let (status, code, headers) = send_full(&app, "POST", login, &Creds::default(), body, Some(10)).await;
        assert_eq!((status, code.as_str()), (503, "SERVER_BUSY"));
        assert_eq!(headers.get(header::RETRY_AFTER).and_then(|v| v.to_str().ok()), Some("1"));
        assert!(!polled.load(Ordering::SeqCst));
        // Signed-in users and health checks are unaffected.
        assert_eq!(send(&app, "GET", me, &session, Body::empty(), None).await.0, 200);
        for path in ["/healthz", "/readyz", "/api/v1/version"] {
            assert_eq!(send(&app, "GET", path, &Creds::default(), Body::empty(), None).await.0, 200, "{path}");
        }

        // The slow bodies time out after the short public deadline, not the 120 s request timeout.
        for task in slow {
            assert_eq!(task.await.unwrap(), (408, "REQUEST_TIMEOUT".into()));
        }
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(capacity.available(true), PUBLIC, "public permits were not released");

        // The reverse: a full global pool refuses signed-in requests, while sign-in still works.
        let held = capacity.acquire(false).unwrap();
        assert_eq!(send(&app, "GET", me, &session, Body::empty(), None).await, (503, "SERVER_BUSY".into()));
        // Permits are taken after authorisation: an anonymous caller is refused 401, never queued.
        assert_eq!(send(&app, "GET", me, &Creds::default(), Body::empty(), None).await.0, 401);
        let wrong = Body::from(json!({ "username": "owner", "password": "wrong" }).to_string());
        assert_eq!(send(&app, "POST", login, &Creds::default(), wrong, None).await.0, 401);
        drop(held);
        assert_eq!(send(&app, "GET", me, &session, Body::empty(), None).await.0, 200);

        db.drop().await;
    }
}
