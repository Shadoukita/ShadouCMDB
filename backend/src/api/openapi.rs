//! Builds the OpenAPI 3.1 document from the route table. Schemas come from the
//! utoipa `ToSchema` / `IntoParams` implementations of the request and response
//! types; error responses are derived from each route's declared error codes.

use std::collections::BTreeMap;

use axum::http::Method;
use utoipa::openapi::path::{HttpMethod, OperationBuilder, Parameter, PathItem};
use utoipa::openapi::request_body::RequestBodyBuilder;
use utoipa::openapi::schema::Schema;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, SecurityRequirement, SecurityScheme};
use utoipa::openapi::tag::TagBuilder;
use utoipa::openapi::{
    ComponentsBuilder, ContentBuilder, InfoBuilder, OpenApi, OpenApiBuilder, PathsBuilder, Ref, RefOr, Required,
    ResponseBuilder, ResponsesBuilder, ServerBuilder,
};
use utoipa::{PartialSchema, ToSchema};

use super::route::{Access, Route};
use crate::auth::session::SESSION_COOKIE;
use crate::http::error::ErrorCode;

pub const API_VERSION: &str = "0.1.0";

const TAG_DESCRIPTIONS: &[(&str, &str)] = &[
    ("Health", "Liveness and readiness probes for orchestrators and load balancers."),
    (
        "Authentication",
        "First-run setup, sign-in with a local username and password, sign-out, and the current user's permissions.",
    ),
    ("Configuration items", "CIs: the tracked assets. Includes the relationship graph around a CI."),
    ("Search", "Global search across CIs and their attribute values."),
    ("Relationships", "Typed, directional edges between CIs. Removal is a soft delete."),
    (
        "CI classes",
        "CI types in an inheritance tree. Adding a class is data entry, not a migration. Changing the data model needs datamodel.manage.",
    ),
    ("Attribute definitions", "Typed custom fields per class, inherited by subclasses."),
    (
        "Relationship types",
        "Relationship types (runs_on, depends_on, ...) and the rules for which classes they may connect.",
    ),
    ("Statuses", "CI lifecycle statuses."),
    ("Environments", "Deployment environments."),
    ("Locations", "Location hierarchy (region > site > room > rack)."),
    ("Owners", "People and teams accountable for CIs."),
    (
        "Lookup lists",
        "Lists an administrator defines (e.g. \"Support contract\") whose values \"lookup\" attributes store.",
    ),
    (
        "Templates",
        "Administration: starter data models (classes, attributes, relationship rules and lookups) installed on a bare database.",
    ),
    (
        "UI settings",
        "Branding, navigation, dashboard, list views and detail/form layouts for every user: one versioned, audited document plus the logo and favicon. Reading needs a session (branding and images are public for the login page); changing needs customization.manage.",
    ),
    (
        "Configuration export/import",
        "Administration: the whole configuration (data model, lookups, permission profiles, UI settings) as one JSON file, and importing such a file with a dry-run diff first. Needs config.export_import.",
    ),
    ("Audit log", "Read-only change history written in the same transaction as every change."),
    ("Users", "Administration: local user accounts, passwords and the permission profiles they hold."),
    (
        "Permission profiles",
        "Administration: named sets of global and per-CI-class permissions. Users can hold several; the built-in Administrator profile holds everything.",
    ),
];

const DESCRIPTION: &str = "REST API for ShadouCMDB. This API is the only database client; the web UI uses nothing else.

- Collections are paginated with `limit`/`offset` and return `{ data, page: { limit, offset, total } }`.
- `sort=field` ascending, `sort=-field` descending. `q` searches. Filters that take ids accept comma-separated lists.
- Every error uses the `ErrorEnvelope` shape; invalid input is always 400 `VALIDATION_ERROR` with per-field `details`.
- Sign in with `POST /api/v1/auth/login`; the session travels in the `shadoucmdb_session` cookie. Every operation except the health probes, login and first-run setup answers 401 `UNAUTHENTICATED` without a live session.
- POST, PUT, PATCH and DELETE also need the `X-CSRF-Token` header (the `csrfToken` from login or `/api/v1/auth/me`, also in the `shadoucmdb_csrf` cookie); without it: 403 `CSRF_TOKEN_INVALID`.
- Permissions come from the permission profiles a user holds. A missing global permission (named in each operation's description) or class permission (view/create/edit/delete) answers 403 `FORBIDDEN`. Lists only contain CIs of classes the user may view.
- Writes are recorded in the audit log (`/api/v1/audit-log`) with the signed-in user as the actor.
- Send `X-Request-Id` to correlate a request; it is echoed back and stored with audit rows.";

// Documentation shape of the error envelope (see http/error.rs).
#[derive(ToSchema)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct ErrorEnvelope {
    #[schema(inline)]
    error: ErrorBody,
}

#[derive(ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[allow(dead_code)]
struct ErrorBody {
    #[schema(inline)]
    code: ErrorCode,
    message: String,
    #[schema(inline, nullable = false)]
    details: Option<Vec<FieldErrorDoc>>,
    request_id: String,
}

#[derive(ToSchema)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct FieldErrorDoc {
    #[serde(rename = "in")]
    #[schema(inline)]
    location: crate::http::error::FieldLocation,
    /// Dotted path, e.g. "attributes.cpu_cores" or "limit"
    field: String,
    message: String,
    /// Machine-readable reason, e.g. "invalid_type", "required", "unique"
    code: String,
}

fn error_status(code: ErrorCode) -> (u16, &'static str) {
    match code {
        ErrorCode::ValidationError => (400, "Invalid input (code VALIDATION_ERROR) with per-field details"),
        ErrorCode::Unauthenticated => {
            (401, "Not signed in, session expired, or wrong credentials (code UNAUTHENTICATED)")
        }
        ErrorCode::Forbidden | ErrorCode::CsrfTokenInvalid => {
            (403, "Missing permission (code FORBIDDEN) or X-CSRF-Token (code CSRF_TOKEN_INVALID)")
        }
        ErrorCode::NotFound => (404, "Not found (code NOT_FOUND)"),
        ErrorCode::Conflict | ErrorCode::InUse | ErrorCode::VersionConflict | ErrorCode::LastAdministrator => (
            409,
            "Conflict: CONFLICT (duplicate or not allowed in this state), IN_USE, VERSION_CONFLICT or LAST_ADMINISTRATOR",
        ),
        ErrorCode::RateLimited => {
            (429, "Too many failed password attempts (code RATE_LIMITED); see the Retry-After header")
        }
        ErrorCode::UnsupportedMediaType => (415, "Body is not application/json"),
        ErrorCode::PayloadTooLarge => (413, "Body too large"),
        ErrorCode::DatabaseUnavailable => (503, "Database unreachable (code DATABASE_UNAVAILABLE)"),
        ErrorCode::InternalError => (500, "Unexpected server error (code INTERNAL_ERROR)"),
    }
}

fn http_method(m: &Method) -> HttpMethod {
    match *m {
        Method::POST => HttpMethod::Post,
        Method::PATCH => HttpMethod::Patch,
        Method::PUT => HttpMethod::Put,
        Method::DELETE => HttpMethod::Delete,
        _ => HttpMethod::Get,
    }
}

fn json_ref(name: &str) -> utoipa::openapi::Content {
    ContentBuilder::new().schema(Some(RefOr::Ref(Ref::from_schema_name(name)))).build()
}

const SESSION_SCHEME: &str = "sessionCookie";
const CSRF_SCHEME: &str = "csrfHeader";

/// Who may call the operation, as OpenAPI security requirements.
fn security(r: &Route) -> Vec<SecurityRequirement> {
    match r.access {
        Access::Public => Vec::new(),
        _ if r.method == Method::GET || r.method == Method::HEAD => {
            vec![SecurityRequirement::new(SESSION_SCHEME, Vec::<String>::new())]
        }
        _ => {
            vec![SecurityRequirement::new(SESSION_SCHEME, Vec::<String>::new()).add(CSRF_SCHEME, Vec::<String>::new())]
        }
    }
}

/// Descriptions of query parameters built from a schema live on the schema;
/// the parameter is where readers look for them.
fn lift_description(mut p: Parameter) -> Parameter {
    if p.description.is_none()
        && let Some(RefOr::T(schema)) = &mut p.schema
    {
        let d = match schema {
            Schema::Object(o) => o.description.take(),
            Schema::AnyOf(a) => a.description.take(),
            Schema::OneOf(o) => o.description.take(),
            _ => None,
        };
        p.description = d;
    }
    p
}

pub fn document(routes: &[Route]) -> OpenApi {
    let mut paths = PathsBuilder::new();
    let mut items: Vec<(String, PathItem)> = Vec::new();
    let mut schemas: Vec<(String, RefOr<Schema>)> = vec![(ErrorEnvelope::name().into_owned(), ErrorEnvelope::schema())];
    let mut tags: Vec<String> = Vec::new();

    for r in routes {
        if !tags.contains(&r.tag) {
            tags.push(r.tag.clone());
        }

        let mut responses: BTreeMap<u16, utoipa::openapi::Response> = BTreeMap::new();
        match &r.response {
            Some(doc) if doc.name.is_empty() => {
                responses.insert(
                    r.status.as_u16(),
                    ResponseBuilder::new()
                        .description("Success")
                        .content(doc.media_type, ContentBuilder::new().schema(Some(doc.schema.clone())).build())
                        .build(),
                );
                // Other statuses of a raw body (304 Not Modified) carry no content.
                for (status, description) in &r.also_returns {
                    responses.insert(status.as_u16(), ResponseBuilder::new().description(description.clone()).build());
                }
            }
            Some(doc) => {
                responses.insert(
                    r.status.as_u16(),
                    ResponseBuilder::new().description("Success").content(doc.media_type, json_ref(&doc.name)).build(),
                );
                for (status, description) in &r.also_returns {
                    responses.insert(
                        status.as_u16(),
                        ResponseBuilder::new()
                            .description(description.clone())
                            .content("application/json", json_ref(&doc.name))
                            .build(),
                    );
                }
                schemas.push((doc.name.clone(), doc.schema.clone()));
                schemas.extend(doc.nested.iter().cloned());
            }
            None => {
                responses.insert(r.status.as_u16(), ResponseBuilder::new().description("Success, no content").build());
            }
        }

        let mut codes = Vec::new();
        if !r.path_params.is_empty() || !r.query_params.is_empty() || r.body.is_some() {
            codes.push(ErrorCode::ValidationError);
        }
        match r.access {
            Access::Public => {}
            Access::Authenticated => codes.push(ErrorCode::Unauthenticated),
            Access::Permission(_) => codes.extend([ErrorCode::Unauthenticated, ErrorCode::Forbidden]),
        }
        if r.access != Access::Public && r.method != Method::GET {
            codes.push(ErrorCode::CsrfTokenInvalid);
        }
        codes.extend(r.errors.iter().copied());
        codes.push(ErrorCode::InternalError);
        if r.path.starts_with("/api/") {
            codes.push(ErrorCode::DatabaseUnavailable);
        }
        if r.body.is_some() {
            codes.push(ErrorCode::UnsupportedMediaType);
        }
        for code in codes {
            let (status, description) = error_status(code);
            responses.entry(status).or_insert_with(|| {
                ResponseBuilder::new()
                    .description(description)
                    .content("application/json", json_ref("ErrorEnvelope"))
                    .build()
            });
        }

        let mut parameters: Vec<Parameter> = r.path_params.clone();
        parameters.extend(r.query_params.iter().cloned().map(lift_description));

        let description = match (r.access, &r.description) {
            (Access::Permission(p), Some(d)) => Some(format!("Requires `{}`. {d}", p.as_str())),
            (Access::Permission(p), None) => Some(format!("Requires `{}`.", p.as_str())),
            (_, d) => d.clone(),
        };
        let mut op = OperationBuilder::new()
            .operation_id(Some(r.operation_id.clone()))
            .tag(r.tag.clone())
            .summary(Some(r.summary.clone()))
            .description(description)
            .securities(Some(security(r)))
            .parameters(Some(parameters))
            .responses(
                responses
                    .into_iter()
                    .fold(ResponsesBuilder::new(), |b, (status, resp)| b.response(status.to_string(), resp)),
            );
        if let Some(body) = &r.body {
            op = op.request_body(Some(
                RequestBodyBuilder::new()
                    .required(Some(Required::True))
                    .content("application/json", ContentBuilder::new().schema(Some(body.clone())).build())
                    .build(),
            ));
        }
        let op = op.build();

        match items.iter_mut().find(|(p, _)| *p == r.path) {
            Some((_, item)) => item.merge_operations(PathItem::new(http_method(&r.method), op)),
            None => items.push((r.path.clone(), PathItem::new(http_method(&r.method), op))),
        }
    }
    for (path, item) in items {
        paths = paths.path(path, item);
    }

    let mut components = ComponentsBuilder::new()
        .security_scheme(
            SESSION_SCHEME,
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::with_description(
                SESSION_COOKIE,
                "Session cookie set by POST /api/v1/auth/login (HttpOnly, SameSite=Lax)",
            ))),
        )
        .security_scheme(
            CSRF_SCHEME,
            SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::with_description(
                "X-CSRF-Token",
                "The session's csrfToken; required on POST, PUT, PATCH and DELETE",
            ))),
        );
    let mut seen = std::collections::HashSet::new();
    for (name, schema) in schemas {
        if seen.insert(name.clone()) {
            components = components.schema(name, schema);
        }
    }

    let mut doc = OpenApiBuilder::new()
        .info(InfoBuilder::new().title("ShadouCMDB API").version(API_VERSION).description(Some(DESCRIPTION)).build())
        .servers(Some([ServerBuilder::new()
            .url("/")
            .description(Some("Same origin the document was fetched from"))
            .build()]))
        .tags(Some(tags.iter().map(|name| {
            let description = TAG_DESCRIPTIONS.iter().find(|(t, _)| t == name).map(|(_, d)| *d);
            TagBuilder::new().name(name.clone()).description(description).build()
        })))
        .paths(paths.build())
        .components(Some(components.build()))
        .build();
    // Default for every operation; each operation states its own requirement too.
    doc.security = Some(vec![SecurityRequirement::new(SESSION_SCHEME, Vec::<String>::new())]);
    doc
}
