//! A route is declared once, with Rust types for its path, query, body and
//! response. The same declaration drives request validation (api/validate.rs),
//! the axum handler, and the OpenAPI document (api/openapi.rs), so the spec
//! cannot drift from the code and a route cannot exist without a spec entry.
//!
//! Access control is part of the declaration too: every route needs a signed-in
//! user unless it is marked [`RouteBuilder::public`], and may require a global
//! permission ([`RouteBuilder::requires`]). The session is resolved, CSRF is
//! checked for state-changing methods and the permission is checked before the
//! request is validated, so an unauthenticated caller learns nothing about a
//! route beyond 401.

use std::future::Future;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{RawPathParams, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
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

use super::context::{RequestContext, forbidden, unauthenticated};
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
    /// Request headers (the auth routes read User-Agent and the forwarded protocol).
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
        let Some(value) = body else {
            return Err(AppError::validation(vec![FieldError {
                location: FieldLocation::Body,
                field: "(root)".into(),
                message: "Required".into(),
                code: "required".into(),
            }]));
        };
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
            return Err(AppError::validation(errors));
        }
        let parsed: T = deserialize(value, FieldLocation::Body)?;
        let errors = parsed.check();
        if !errors.is_empty() {
            return Err(AppError::validation(errors));
        }
        Ok(Body(parsed))
    }
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
    ResponseDoc { name: T::name().into_owned(), schema: T::schema(), nested }
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
    pub status: StatusCode,
    pub access: Access,
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
    errors: Vec<ErrorCode>,
    also_returns: Vec<(StatusCode, String)>,
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
        errors: Vec::new(),
        also_returns: Vec::new(),
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
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
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
        let safe_method = self.method == Method::GET || self.method == Method::HEAD;

        let handler = move |State(state): State<AppState>,
                            raw_path: RawPathParams,
                            RawQuery(raw_query): RawQuery,
                            headers: HeaderMap,
                            body: Result<Bytes, BytesRejection>| {
            let f = f.clone();
            async move {
                let run = async move {
                    let ctx = authorise(&state, &headers, access, safe_method).await?;
                    let body = read_body(&headers, body)?;
                    let input = In(P::parse(&raw_path)?, Q::parse(raw_query.as_deref())?, B::parse(body)?);
                    let api = Api { pool: state.pool, ctx, auth: state.auth, headers };
                    Ok::<_, AppError>(f(api, input).await?.respond(status))
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
            status,
            access,
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

/// Resolves the caller and enforces the route's access rule: 401 without a
/// live session, 403 CSRF_TOKEN_INVALID for a state-changing request without
/// the session's token, 403 FORBIDDEN without the required permission.
async fn authorise(
    state: &AppState,
    headers: &HeaderMap,
    access: Access,
    safe_method: bool,
) -> Result<RequestContext, AppError> {
    let request_id = request_id::current();
    if access == Access::Public {
        return Ok(RequestContext::anonymous(request_id));
    }
    let Some(principal) = auth::authenticate(&state.pool, &state.auth.config, headers).await? else {
        return Err(unauthenticated());
    };
    if !safe_method && !auth::csrf_ok(&principal, headers) {
        return Err(AppError::new(
            ErrorCode::CsrfTokenInvalid,
            "Missing or wrong X-CSRF-Token header (send the csrfToken from /api/v1/auth/me)",
        ));
    }
    if let Access::Permission(p) = access
        && !principal.permissions.has(p)
    {
        return Err(forbidden(format!("This requires the {} permission", p.as_str())));
    }
    Ok(RequestContext::user(Arc::new(principal), request_id))
}

/// JSON is the only accepted body type. An empty body counts as no body
/// (clients often send Content-Type: application/json on DELETE).
fn read_body(headers: &HeaderMap, body: Result<Bytes, BytesRejection>) -> Result<Option<Value>, AppError> {
    let bytes = match body {
        Ok(b) => b,
        Err(rejection) if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            return Err(AppError::new(ErrorCode::PayloadTooLarge, "Request body is too large"));
        }
        Err(rejection) => return Err(root_error(&rejection.body_text(), "bad_request")),
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
