//! A route is declared once, with Rust types for its path, query, body and
//! response. The same declaration drives request validation (api/validate.rs),
//! the axum handler, and the OpenAPI document (api/openapi.rs), so the spec
//! cannot drift from the code and a route cannot exist without a spec entry.

use std::future::Future;

use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{RawPathParams, RawQuery, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
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

use super::context::RequestContext;
use super::schemas;
use super::validate::{self, QueryParam};
use crate::http::AppState;
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::http::request_id;

/// What a handler gets besides its validated inputs.
pub struct Api {
    pub pool: PgPool,
    pub ctx: RequestContext,
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
    /// Error codes beyond VALIDATION_ERROR / INTERNAL_ERROR / DATABASE_UNAVAILABLE.
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
    pub fn errors(mut self, errors: &[ErrorCode]) -> Self {
        self.errors = errors.to_vec();
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

        let handler = move |State(state): State<AppState>,
                            raw_path: RawPathParams,
                            RawQuery(raw_query): RawQuery,
                            headers: HeaderMap,
                            body: Result<Bytes, BytesRejection>| {
            let f = f.clone();
            async move {
                let run = async move {
                    let body = read_body(&headers, body)?;
                    let input = In(P::parse(&raw_path)?, Q::parse(raw_query.as_deref())?, B::parse(body)?);
                    let actor = state.actors.resolve(&headers).await?;
                    let api =
                        Api { pool: state.pool, ctx: RequestContext { actor, request_id: request_id::current() } };
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
