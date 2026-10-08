//! Builds the OpenAPI 3.1 document from the route table. Schemas come from the
//! utoipa `ToSchema` / `IntoParams` implementations of the request and response
//! types; error responses are derived from each route's declared error codes.

use std::collections::BTreeMap;

use axum::http::Method;
use utoipa::openapi::path::{HttpMethod, OperationBuilder, Parameter, PathItem};
use utoipa::openapi::request_body::RequestBodyBuilder;
use utoipa::openapi::schema::{ArrayItems, Schema};
use utoipa::openapi::security::{
    ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityRequirement, SecurityScheme,
};
use utoipa::openapi::tag::TagBuilder;
use utoipa::openapi::{
    ComponentsBuilder, ContentBuilder, InfoBuilder, OpenApi, OpenApiBuilder, PathsBuilder, Ref, RefOr, Required,
    ResponseBuilder, ResponsesBuilder, ServerBuilder,
};
use utoipa::{PartialSchema, ToSchema};

use super::route::{Access, Route};
use crate::auth::session::SESSION_COOKIE;
use crate::http::error::ErrorCode;

/// `info.version`: the release, so the document never claims another version than the server it came from.
pub const API_VERSION: &str = env!("CARGO_PKG_VERSION");

const TAG_DESCRIPTIONS: &[(&str, &str)] = &[
    ("Health", "Liveness and readiness probes for orchestrators and load balancers."),
    (
        "Authentication",
        "First-run setup, sign-in with a local username and password, sign-out, and the current user's permissions.",
    ),
    ("Configuration items", "CIs: the tracked assets. Includes the relationship graph around a CI."),
    (
        "Bulk import",
        "Import CIs from CSV and Excel files: upload, map columns, dry run, commit. Session only; needs the switch on, cis.import, and the class rights for every row.",
    ),
    ("Search", "Global search across CIs and their attribute values."),
    (
        "Impact analysis",
        "Which CIs are affected if a CI fails (downstream) and which it depends on (upstream), along the relationship \
         types that propagate impact. Bounded, and limited to the CIs the caller may view.",
    ),
    (
        "Business services",
        "Business services: CIs of the built-in business service class, with members (CIs of any class, including \
         other services), technical and business owners (users and groups) and a criticality. Members of classes the \
         caller may not view are neither listed nor counted.",
    ),
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
    (
        "Saved views",
        "Named states of the inventory list and the search page, personal or shared with every user, and each user's \
         default view per list. A view stores a query by key, never data or rights, so it never widens what a user \
         sees. Session only; shared views need views.share to change.",
    ),
    (
        "Notifications",
        "Each user's in-app notifications (approvals waiting, approval outcomes, workflow transitions on instances \
         they started, imports that ended), with the unread count for the bell. Written by the server in the \
         transaction of the event; nothing is sent outside the application. Session only.",
    ),
    ("Audit log", "Read-only change history written in the same transaction as every change."),
    ("Users", "Administration: local user accounts, passwords and the permission profiles they hold."),
    (
        "User groups",
        "Administration: named sets of users that can own business services. Managed with users.manage; not part of the configuration export.",
    ),
    (
        "Permission profiles",
        "Administration: named sets of global and per-CI-class permissions. Users can hold several; the built-in Administrator profile holds everything.",
    ),
    (
        "API tokens",
        "Administration: tokens for scripts and services. A token acts as its owner, limited to one permission profile, until it expires or is revoked. Needs users.manage and a signed-in session.",
    ),
];

// `{public}` is replaced with the operations that need no session.
const DESCRIPTION: &str = "REST API for ShadouCMDB. This API is the only database client; the web UI uses nothing else.

- Collections are paginated with `limit`/`offset` and return `{ data, page: { limit, offset, total } }`, except `listIdentityProviders` (a plain array), `listCiClassEffectiveAttributes`, `listTemplates` and `listImportMappings` (`{ data }` with every item), `listSavedViews` (`{ data, limits }` with every item), `countSavedViews` (`{ data, cap, truncated }`, at most 50 views), `searchPrincipals` (`{ data }`, at most 20 matches), `listConfigurationItemServices` (`{ data, truncated, visibility }`, at most 200 services), `getWorkflowInstanceSummary` (`{ data }`, one count per workflow and state), `getConfigurationItemWorkflows` (`{ data, startable }`, running instances and the 20 that ended last) and the `.../usage` operations (`{ inUse, data }`).
- `sort=field` ascending, `sort=-field` descending. `q` searches. Filters that take ids accept comma-separated lists.
- Every error uses the `ErrorEnvelope` shape; invalid input is always 400 `VALIDATION_ERROR` with per-field `details`.
- Sign in with `POST /api/v1/auth/login`; the session travels in the `shadoucmdb_session` cookie (`__Host-shadoucmdb_session` behind HTTPS). Without a live session every operation answers 401 `UNAUTHENTICATED`, except these public ones: {public}.
- POST, PUT, PATCH and DELETE also need the `X-CSRF-Token` header (the `csrfToken` from login or `/api/v1/auth/me`, also in the `shadoucmdb_csrf` cookie, `__Host-shadoucmdb_csrf` behind HTTPS); without it: 403 `CSRF_TOKEN_INVALID`. So do the audited CSV exports (`exportConfigurationItemImpact`, `exportBusinessServiceMembers`, `exportConfig`), although they are GETs, so a link on another site cannot run them as the signed-in user.
- Scripts and services use an API token instead (`Authorization: Bearer scmdb_...`, created under `/api/v1/admin/api-tokens`). With that header the cookies are ignored and no CSRF token is needed; an invalid, expired or revoked token is 401. A token may do what both its owner and its permission profile allow. Operations that need a signed-in session say so in their description and answer 403 `FORBIDDEN` to a token. Every request made with a token is recorded in the audit log.
- Permissions come from the permission profiles a user holds. A missing global permission (named in each operation's description) or class permission (view/create/edit/delete) answers 403 `FORBIDDEN`. Lists only contain CIs of classes the user may view.
- Writes are recorded in the audit log (`/api/v1/audit-log`) with the signed-in user as the actor.
- Send `X-Request-Id` to correlate a request; it is echoed back and stored with audit rows.
- Request bodies are limited to 1 MiB (64 KiB on public operations; configuration import allows more): 413 `PAYLOAD_TOO_LARGE`. A request not answered within `HTTP_REQUEST_TIMEOUT_SECS` answers 408 `REQUEST_TIMEOUT`, and so does a request body that does not arrive within `HTTP_BODY_TIMEOUT_SECS` (`HTTP_HEADER_READ_TIMEOUT_SECS` on public operations; configuration import and file uploads excepted).";

/// [`DESCRIPTION`] with the public operations listed.
fn description(routes: &[Route]) -> String {
    let public: Vec<String> = routes
        .iter()
        .filter(|r| r.access == Access::Public)
        .map(|r| format!("`{} {}` ({})", r.method, r.path, r.operation_id))
        .collect();
    DESCRIPTION.replace("{public}", &public.join(", "))
}

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
    /// At most 100 problems; when there are more, a last entry with code `truncated` counts the rest
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
        ErrorCode::Unauthenticated => (
            401,
            "Not signed in, session expired, invalid/expired/revoked API token, or wrong credentials (code UNAUTHENTICATED)",
        ),
        ErrorCode::MfaRequired => (
            401,
            "Wrong credentials (code UNAUTHENTICATED), or the password was right and the second factor is due (code \
             MFA_REQUIRED)",
        ),
        ErrorCode::Forbidden
        | ErrorCode::CsrfTokenInvalid
        | ErrorCode::MfaEnrolmentRequired
        | ErrorCode::EmailRequired => (
            403,
            "Missing permission (code FORBIDDEN) or X-CSRF-Token (code CSRF_TOKEN_INVALID), MFA must be set up \
             first (code MFA_ENROLMENT_REQUIRED), or the account must enter its e-mail first (code EMAIL_REQUIRED)",
        ),
        ErrorCode::MfaRequiredForToken => (
            403,
            "Missing permission (code FORBIDDEN) or X-CSRF-Token (code CSRF_TOKEN_INVALID), MFA must be set up first \
             (code MFA_ENROLMENT_REQUIRED), the account must enter its e-mail first (code EMAIL_REQUIRED), or the \
             token's owner must use two-factor authentication and this session did not sign in with a second factor \
             (code MFA_REQUIRED_FOR_TOKEN)",
        ),
        ErrorCode::ReauthenticationRequired => (
            403,
            "Missing permission (code FORBIDDEN) or X-CSRF-Token (code CSRF_TOKEN_INVALID), MFA must be set up first \
             (code MFA_ENROLMENT_REQUIRED), or the session's owner has not confirmed their credentials in the last 10 \
             minutes (code REAUTHENTICATION_REQUIRED; POST /api/v1/auth/reauthenticate, then send the request again)",
        ),
        ErrorCode::WorkflowApprovalSelf => (
            403,
            "Missing permission or not eligible to decide (code FORBIDDEN; details[0].code not_eligible, \
             session_required or token_not_self_minted), X-CSRF-Token (code CSRF_TOKEN_INVALID), MFA must be set up \
             first (code MFA_ENROLMENT_REQUIRED), the account must enter its e-mail first (code EMAIL_REQUIRED), or \
             four-eyes and separation of duties (code WORKFLOW_APPROVAL_SELF; details[0].code requester, \
             token_creator, earlier_step or actor_of:<transitionKey>). Nothing was changed",
        ),
        ErrorCode::NotFound => (404, "Not found (code NOT_FOUND)"),
        ErrorCode::Gone => (410, "The operation was removed (code GONE); the message names its replacement"),
        ErrorCode::Conflict | ErrorCode::InUse | ErrorCode::VersionConflict | ErrorCode::LastAdministrator => (
            409,
            "Conflict: CONFLICT (duplicate or not allowed in this state), IN_USE, VERSION_CONFLICT or LAST_ADMINISTRATOR",
        ),
        ErrorCode::WorkflowControlledField => (
            409,
            "Conflict: CONFLICT (duplicate or not allowed in this state), VERSION_CONFLICT, or \
             WORKFLOW_CONTROLLED_FIELD (a state field an active workflow drives was given another value; \
             details[].field names it as `attributes.<key>`, details[].code workflow_controlled). Nothing was changed",
        ),
        ErrorCode::WorkflowApprovalPending => (
            409,
            "Conflict: CONFLICT (not allowed in this state), VERSION_CONFLICT, or WORKFLOW_APPROVAL_PENDING (the \
             instance has a pending approval request, so no other transition runs; details[0].field is \
             `approvalRequestId`, details[0].message names the request). Nothing was changed",
        ),
        ErrorCode::WorkflowApprovalStale => (
            409,
            "Conflict: CONFLICT (the request is not pending, the step is no longer active, or you already decided \
             it), VERSION_CONFLICT (stale `expectedVersion`), or WORKFLOW_APPROVAL_STALE (the final approval cannot \
             apply the transition: details[].field `fields.<key>` with code changed, not_a_transition_field, \
             state_field, required or condition). Nothing was changed, not even the decision",
        ),
        ErrorCode::InvalidName | ErrorCode::SchemaChangeRefused => (
            422,
            "Refused: INVALID_NAME (technical name malformed, reserved or taken) or SCHEMA_CHANGE_REFUSED (the \
             change would lose or break stored data); details name the field and the reason",
        ),
        ErrorCode::SecretRequired => (
            422,
            "The stored secret must be entered again (code SECRET_REQUIRED): the patch changes the server address or \
             bind DN it would be sent to; details name the secret's field (code secret_required). Nothing was changed",
        ),
        ErrorCode::IdempotencyKeyReused => (
            422,
            "The Idempotency-Key was already used for another operation or target (code IDEMPOTENCY_KEY_REUSED, \
             details[0].code idempotency_key_reused). Nothing was changed",
        ),
        ErrorCode::WorkflowConditionFailed => (
            422,
            "The transition cannot run yet (code WORKFLOW_CONDITION_FAILED): one detail per condition, required \
             field or comment that is not satisfied (details[].code condition, required or comment_required; \
             details[].field names the field as `fields.<key>`, or `comment`). Nothing was changed",
        ),
        ErrorCode::RateLimited => (
            429,
            "Too many requests (code RATE_LIMITED): failed password attempts, or the limit named in details[0].code; \
             see the Retry-After header",
        ),
        ErrorCode::UnsupportedMediaType => {
            (415, "Body is not of an accepted media type (application/json unless the operation lists others)")
        }
        ErrorCode::PayloadTooLarge => (413, "Body too large (code PAYLOAD_TOO_LARGE)"),
        ErrorCode::RequestTimeout => (408, "Request not completed in time (code REQUEST_TIMEOUT)"),
        ErrorCode::DatabaseUnavailable | ErrorCode::SchemaNotMigrated | ErrorCode::ServerBusy => (
            503,
            "Database unreachable (code DATABASE_UNAVAILABLE), migrations pending (code SCHEMA_NOT_MIGRATED; run \
             `shadoucmdb migrate`), or too many requests in progress (code SERVER_BUSY; see the Retry-After header)",
        ),
        ErrorCode::IdentityProviderUnavailable => (
            503,
            "The LDAP directory could not be reached (code IDENTITY_PROVIDER_UNAVAILABLE; local accounts still sign \
             in), the database is unreachable (code DATABASE_UNAVAILABLE), or migrations are pending (code \
             SCHEMA_NOT_MIGRATED)",
        ),
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
const TOKEN_SCHEME: &str = "apiToken";

/// Who may call the operation, as OpenAPI security requirements (alternatives).
fn security(r: &Route) -> Vec<SecurityRequirement> {
    let none = Vec::<String>::new;
    let mut alternatives = match r.access {
        Access::Public => return Vec::new(),
        _ if !r.csrf => vec![SecurityRequirement::new(SESSION_SCHEME, none())],
        _ => vec![SecurityRequirement::new(SESSION_SCHEME, none()).add(CSRF_SCHEME, none())],
    };
    if !r.session_only {
        alternatives.push(SecurityRequirement::new(TOKEN_SCHEME, none()));
    }
    alternatives
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
            // An operation that only answers an error (410 GONE) is documented by its error code.
            None if r.status.is_client_error() => {}
            None if r.status.is_redirection() => {
                let description = "Redirect: the browser follows the Location header";
                responses.insert(r.status.as_u16(), ResponseBuilder::new().description(description).build());
            }
            None => {
                responses.insert(r.status.as_u16(), ResponseBuilder::new().description("Success, no content").build());
            }
        }

        let mut codes = Vec::new();
        // First, so its description (which names every 403 code) is the one kept.
        if r.reauthentication {
            codes.push(ErrorCode::ReauthenticationRequired);
        }
        if !r.path_params.is_empty() || !r.query_params.is_empty() || r.body.is_some() {
            codes.push(ErrorCode::ValidationError);
        }
        match r.access {
            Access::Public => {}
            Access::Authenticated if r.session_only => codes.extend([ErrorCode::Unauthenticated, ErrorCode::Forbidden]),
            Access::Authenticated => codes.push(ErrorCode::Unauthenticated),
            Access::Permission(_) => codes.extend([ErrorCode::Unauthenticated, ErrorCode::Forbidden]),
        }
        if r.access != Access::Public && r.csrf {
            codes.push(ErrorCode::CsrfTokenInvalid);
        }
        if r.access != Access::Public && !r.before_mfa_enrolment {
            codes.push(ErrorCode::MfaEnrolmentRequired);
        }
        if r.access != Access::Public && !r.before_email_entry {
            codes.push(ErrorCode::EmailRequired);
        }
        codes.extend(r.errors.iter().copied());
        codes.push(ErrorCode::InternalError);
        if r.path.starts_with("/api/") {
            codes.extend([ErrorCode::DatabaseUnavailable, ErrorCode::SchemaNotMigrated, ErrorCode::ServerBusy]);
        }
        if r.body.is_some() {
            codes.extend([ErrorCode::UnsupportedMediaType, ErrorCode::PayloadTooLarge]);
        }
        // The request timeout (http/mod.rs) bounds every route.
        codes.push(ErrorCode::RequestTimeout);
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

        let mut description = match (r.access, &r.description) {
            (Access::Permission(p), Some(d)) => Some(format!("Requires `{}`. {d}", p.as_str())),
            (Access::Permission(p), None) => Some(format!("Requires `{}`.", p.as_str())),
            (_, d) => d.clone(),
        };
        if r.session_only {
            let note = "Needs a signed-in session: API tokens get 403 FORBIDDEN.";
            description = Some(description.map_or_else(|| note.to_owned(), |d| format!("{d} {note}")));
        }
        if r.reauthentication {
            let note = "The session's owner must have signed in or confirmed their credentials \
                        (`reauthenticate`, POST /api/v1/auth/reauthenticate) in the last 10 minutes: 403 \
                        REAUTHENTICATION_REQUIRED otherwise, audited as `session.reauthentication_required`.";
            description = Some(description.map_or_else(|| note.to_owned(), |d| format!("{d} {note}")));
        }
        let mut op = OperationBuilder::new()
            .operation_id(Some(r.operation_id.clone()))
            .tag(r.tag.clone())
            .summary(Some(r.summary.clone()))
            .description(description)
            .deprecated(r.deprecated.then_some(utoipa::openapi::Deprecated::True))
            .securities(Some(security(r)))
            .parameters(Some(parameters))
            .responses(
                responses
                    .into_iter()
                    .fold(ResponsesBuilder::new(), |b, (status, resp)| b.response(status.to_string(), resp)),
            );
        if let Some(body) = &r.body {
            let media: &[&str] = if r.body_media.is_empty() { &["application/json"] } else { r.body_media };
            let request = media.iter().fold(RequestBodyBuilder::new().required(Some(Required::True)), |b, m| {
                b.content(*m, ContentBuilder::new().schema(Some(body.clone())).build())
            });
            op = op.request_body(Some(request.build()));
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
                "Session cookie set by POST /api/v1/auth/login (HttpOnly, SameSite=Lax); named __Host-shadoucmdb_session, with Secure, behind HTTPS",
            ))),
        )
        .security_scheme(
            CSRF_SCHEME,
            SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::with_description(
                "X-CSRF-Token",
                "The session's csrfToken; required on POST, PUT, PATCH and DELETE, and on the audited CSV export GETs",
            ))),
        )
        .security_scheme(
            TOKEN_SCHEME,
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .description(Some(
                        "API token (`scmdb_` + 64 hex characters) from POST /api/v1/admin/api-tokens. No CSRF token needed; cookies are ignored.",
                    ))
                    .build(),
            ),
        );
    let mut seen = std::collections::HashSet::new();
    for (name, mut schema) in schemas {
        if seen.insert(name.clone()) {
            unwrap_described_inlines(&mut schema);
            components = components.schema(name, schema);
        }
    }

    let mut doc = OpenApiBuilder::new()
        .info(
            InfoBuilder::new()
                .title("ShadouCMDB API")
                .version(API_VERSION)
                .description(Some(description(routes)))
                .build(),
        )
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
    doc.security = Some(vec![
        SecurityRequirement::new(SESSION_SCHEME, Vec::<String>::new()),
        SecurityRequirement::new(TOKEN_SCHEME, Vec::<String>::new()),
    ]);
    doc
}

/// utoipa turns a doc comment or a `default` on an `#[schema(inline)]` field
/// into `allOf: [<the type>, {type: object, description, default}]`. The second
/// member makes the property an object as well, which no enum or count value
/// is, and code generators emit an impossible type (`"A" & Record<string,
/// never>`). Fold the description and default into the inlined schema instead.
fn unwrap_described_inlines(schema: &mut RefOr<Schema>) {
    let RefOr::T(s) = schema else { return };
    match s {
        Schema::AllOf(all_of) => {
            all_of.items.iter_mut().for_each(unwrap_described_inlines);
            let [RefOr::T(Schema::Object(inner)), RefOr::T(Schema::Object(extra))] = all_of.items.as_slice() else {
                return;
            };
            let mut bare = extra.clone();
            bare.description = None;
            bare.default = None;
            if serde_json::to_value(&bare).ok() != Some(serde_json::json!({"type": "object"})) {
                return;
            }
            let mut inner = inner.clone();
            inner.description = extra.description.clone().or(inner.description);
            inner.default = extra.default.clone().or(inner.default);
            *s = Schema::Object(inner);
        }
        Schema::OneOf(o) => o.items.iter_mut().for_each(unwrap_described_inlines),
        Schema::AnyOf(o) => o.items.iter_mut().for_each(unwrap_described_inlines),
        Schema::Object(o) => o.properties.values_mut().for_each(unwrap_described_inlines),
        Schema::Array(a) => {
            if let ArrayItems::RefOrSchema(items) = &mut a.items {
                unwrap_described_inlines(items);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    fn spec() -> Value {
        serde_json::from_str(&crate::api::openapi_json()).expect("the document is JSON")
    }

    fn operations(spec: &Value) -> impl Iterator<Item = (&String, &Value)> {
        spec["paths"].as_object().unwrap().values().flat_map(|item| item.as_object().unwrap())
    }

    #[test]
    fn version_is_the_release() {
        assert_eq!(spec()["info"]["version"], env!("CARGO_PKG_VERSION"));
        let committed: Value =
            serde_json::from_str(include_str!("../../openapi.json")).expect("backend/openapi.json is JSON");
        assert_eq!(
            committed["info"]["version"],
            env!("CARGO_PKG_VERSION"),
            "backend/openapi.json is stale: run `shadoucmdb openapi > openapi.json` after a version bump"
        );
    }

    #[test]
    fn description_lists_every_public_operation() {
        let spec = spec();
        let description = spec["info"]["description"].as_str().unwrap();
        let public: Vec<&str> = operations(&spec)
            .filter(|(_, op)| op["security"].as_array().is_some_and(Vec::is_empty))
            .map(|(_, op)| op["operationId"].as_str().unwrap())
            .collect();
        assert!(!public.is_empty());
        for id in public {
            assert!(description.contains(&format!("({id})")), "{id} is public but not listed");
        }
        assert!(!description.contains("{public}"));
    }

    /// The collections sentence of [`DESCRIPTION`] names every list without `page`.
    #[test]
    fn unpaginated_lists_are_named() {
        let spec = spec();
        let description = spec["info"]["description"].as_str().unwrap();
        for (method, op) in operations(&spec) {
            let id = op["operationId"].as_str().unwrap();
            let Some(name) = op["responses"]["200"]["content"]["application/json"]["schema"]["$ref"].as_str() else {
                continue;
            };
            let schema = &spec["components"]["schemas"][name.rsplit('/').next().unwrap()];
            let props = &schema["properties"];
            let list = schema["type"] == "array" || props["data"]["type"] == "array";
            if method != "get" || !list || props.get("page").is_some() || props.get("inUse").is_some() {
                continue;
            }
            assert!(description.contains(&format!("`{id}`")), "{id} returns a list without `page`");
        }
    }

    #[test]
    fn size_and_timeout_errors_are_published() {
        let spec = spec();
        for (_, op) in operations(&spec) {
            let id = op["operationId"].as_str().unwrap();
            assert!(op["responses"].get("408").is_some(), "{id} lacks 408");
            if op.get("requestBody").is_some() {
                assert!(op["responses"].get("413").is_some(), "{id} lacks 413");
            }
        }
    }

    #[test]
    fn described_inline_fields_keep_their_type() {
        let spec = spec();
        let code = &spec["components"]["schemas"]["WorkflowWarning"]["properties"]["code"];
        assert_eq!(code["type"], "string");
        assert_eq!(code["enum"], serde_json::json!(["UNINSTANCED_CIS"]));
        assert!(code["description"].as_str().unwrap().starts_with("`UNINSTANCED_CIS`"));
        assert!(!crate::api::openapi_json().contains("\"allOf\": [\n"), "an allOf wrapper is left in the document");
    }

    #[test]
    fn no_component_is_named_after_a_rust_type() {
        let spec = spec();
        for name in spec["components"]["schemas"].as_object().unwrap().keys() {
            assert!(!["Vec", "Option", "HashMap", "BTreeMap", "String"].contains(&name.as_str()), "schema {name}");
        }
    }
}
