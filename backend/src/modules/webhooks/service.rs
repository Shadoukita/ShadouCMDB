//! Webhook endpoints and the host allowlist (design SHAA-2725 §5, §8, §11.1).
//!
//! An endpoint's signing secret is generated here and shown once, on create
//! and on rotation; it and the optional auth header value are sealed
//! (`secrets::sealed`, bound to the endpoint's id) and never read back
//! through the API, the audit log or a configuration export.

use chrono::{DateTime, Duration as TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use utoipa::openapi::schema::{AnyOfBuilder, ObjectBuilder, Schema, Type};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::Webhooks;
use super::client::RESERVED_HEADERS;
use super::hosts::{AllowedHost, Ceiling, HostMatch, HostPattern, UrlRefusal, check_url};
use crate::api::context::RequestContext;
use crate::api::route::Check;
use crate::api::schemas::{self, Page, Paged};
use crate::auth::permissions::GlobalPermission;
use crate::auth::secret::Secret as InputSecret;
use crate::data::crud::{self, AuditAction, AuditEntry};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::secrets::sealed::{self, EndpointSecret};

const ENTITY: &str = "webhook_endpoints";
const HOSTS: &str = "webhook_allowed_hosts";
/// Longest auth header value.
const MAX_HEADER_VALUE: usize = 4096;
/// Hours the previous secret stays valid after a rotation, by default and at most.
const DEFAULT_GRACE_HOURS: i32 = 24;
const MAX_GRACE_HOURS: i32 = 168;

// ---------------------------------------------------------------------------
// API shapes
// ---------------------------------------------------------------------------

/// Whether an endpoint is called
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum WebhookEndpointStatus {
    /// Deliveries are sent
    Active,
    /// Paused by an administrator: deliveries are held until it is resumed
    Paused,
    /// Suspended by the server (see `suspendedReason`): deliveries are held until an administrator resumes it
    Suspended,
}

/// A registered webhook receiver. Never carries its signing secret or header value.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebhookEndpoint {
    pub id: Uuid,
    pub key: String,
    pub name: String,
    pub status: WebhookEndpointStatus,
    /// Why the server suspended it: `breaker` (too many failures in a row), `host_not_allowed` (its host left the
    /// allowlist), `restored` (restored from a backup), `secret_required` (created by a configuration import, or its
    /// secret could not be decrypted)
    pub suspended_reason: Option<String>,
    /// The fields below are null to a caller without `webhooks.manage`
    pub url: Option<String>,
    /// The URL is plain http (allowed by the operator and the allowlist entry): requests are not encrypted
    pub unencrypted: Option<bool>,
    pub payload_version: Option<i16>,
    pub timeout_ms: Option<i32>,
    pub max_per_minute: Option<i32>,
    pub max_in_flight: Option<i16>,
    /// The auth header's name; its value is never returned
    pub auth_header_name: Option<String>,
    pub auth_header_set: Option<bool>,
    /// Until when the signing secret before the last rotation is still sent (a second `v1=` in the signature)
    pub previous_secret_until: Option<DateTime<Utc>>,
    pub consecutive_failures: Option<i32>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_failure_at: Option<DateTime<Utc>>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    /// Send it back with a change; a stale one fails with 409 VERSION_CONFLICT
    pub version: i32,
}

/// An endpoint just created, with its signing secret: shown this once, never again
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebhookEndpointCreated {
    pub endpoint: WebhookEndpoint,
    /// `whsec_...`: the receiver keys HMAC-SHA256 with this whole string to check `X-ShadouCMDB-Signature`
    pub secret: String,
}

/// A new signing secret, shown this once
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebhookSecretRotated {
    pub endpoint: WebhookEndpoint,
    pub secret: String,
}

fn endpoint_key_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .pattern(Some("^[a-z][a-z0-9_-]{0,62}$"))
        .description(Some("Stable key: lower case, digits, _ and -; actions and configuration files name it"))
        .into()
}

fn url_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .min_length(Some(1))
        .max_length(Some(super::hosts::MAX_URL))
        .description(Some(
            "Absolute https URL (http only with WEBHOOK_ALLOW_HTTP=true and an allowlist entry that allows it), no \
             user name or password, no fragment; its host and port must be on the allowlist",
        ))
        .into()
}

fn endpoint_name_schema() -> Schema {
    ObjectBuilder::new().schema_type(Type::String).min_length(Some(1)).max_length(Some(100)).pattern(Some(r"\S")).into()
}

/// A static header sent with every request (e.g. `Authorization: Bearer ...`); its value is write-only
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebhookAuthHeaderInput {
    #[schema(pattern = "^[A-Za-z0-9-]{1,64}$")]
    pub name: String,
    #[schema(schema_with = header_value_schema)]
    pub value: InputSecret,
}

fn header_value_schema() -> Schema {
    schemas::secret_builder().min_length(Some(1)).max_length(Some(MAX_HEADER_VALUE)).into()
}

fn nullable_auth_header_schema() -> Schema {
    AnyOfBuilder::new()
        .item(utoipa::openapi::Ref::from_schema_name("WebhookAuthHeaderInput"))
        .item(ObjectBuilder::new().schema_type(Type::Null))
        .description(Some("An object replaces the header, null removes it, left out keeps it"))
        .into()
}

/// Register a webhook receiver
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebhookEndpointCreate {
    #[schema(schema_with = endpoint_key_schema)]
    pub key: String,
    #[schema(schema_with = endpoint_name_schema)]
    #[serde(deserialize_with = "schemas::trimmed")]
    pub name: String,
    #[schema(schema_with = url_schema)]
    pub url: String,
    /// Time allowed per attempt (1,000-30,000 ms, default 10,000)
    #[schema(minimum = 1000, maximum = 30000)]
    #[serde(default)]
    pub timeout_ms: Option<i32>,
    /// Requests per minute at most (1-6,000, default 120)
    #[schema(minimum = 1, maximum = 6000)]
    #[serde(default)]
    pub max_per_minute: Option<i32>,
    /// Requests in flight at most (1-16, default 2)
    #[schema(minimum = 1, maximum = 16)]
    #[serde(default)]
    pub max_in_flight: Option<i16>,
    #[serde(default)]
    pub auth_header: Option<WebhookAuthHeaderInput>,
}

/// Change an endpoint (partial)
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebhookEndpointUpdate {
    #[schema(minimum = 1)]
    pub version: i32,
    #[schema(schema_with = endpoint_name_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    pub name: Option<String>,
    #[schema(schema_with = url_schema)]
    #[serde(default)]
    pub url: Option<String>,
    #[schema(minimum = 1000, maximum = 30000)]
    #[serde(default)]
    pub timeout_ms: Option<i32>,
    #[schema(minimum = 1, maximum = 6000)]
    #[serde(default)]
    pub max_per_minute: Option<i32>,
    #[schema(minimum = 1, maximum = 16)]
    #[serde(default)]
    pub max_in_flight: Option<i16>,
    #[schema(schema_with = nullable_auth_header_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    pub auth_header: Option<Option<WebhookAuthHeaderInput>>,
}

/// Replace the signing secret
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebhookSecretRotate {
    /// Hours the current secret is still sent next to the new one (0-168, default 24); 0 drops it at once
    #[schema(minimum = 0, maximum = 168)]
    #[serde(default)]
    pub grace_hours: Option<i32>,
}

fn body_error(field: &str, code: &str, message: impl Into<String>) -> FieldError {
    FieldError { location: FieldLocation::Body, field: field.into(), message: message.into(), code: code.into() }
}

fn header_problems(h: &WebhookAuthHeaderInput, out: &mut Vec<FieldError>) {
    let name = h.name.to_ascii_lowercase();
    if name.is_empty() || name.len() > 64 || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        out.push(body_error("authHeader.name", "invalid_format", "Letters, digits and -, at most 64"));
    } else if RESERVED_HEADERS.contains(&name.as_str()) || name.starts_with("x-shadoucmdb-") {
        out.push(body_error(
            "authHeader.name",
            "reserved",
            format!("{} is set by the server; use another header name", h.name),
        ));
    }
    let v = h.value.expose();
    if v.is_empty() || v.len() > MAX_HEADER_VALUE {
        out.push(body_error("authHeader.value", "invalid_format", format!("1 to {MAX_HEADER_VALUE} characters")));
    } else if v.bytes().any(|b| b < 0x20 && b != b'\t' || b == 0x7f) {
        out.push(body_error("authHeader.value", "invalid_format", "No line breaks or control characters"));
    }
}

fn limits(timeout: Option<i32>, per_minute: Option<i32>, in_flight: Option<i16>, out: &mut Vec<FieldError>) {
    if timeout.is_some_and(|t| !(1000..=30_000).contains(&t)) {
        out.push(body_error("timeoutMs", "out_of_range", "1,000 to 30,000"));
    }
    if per_minute.is_some_and(|t| !(1..=6000).contains(&t)) {
        out.push(body_error("maxPerMinute", "out_of_range", "1 to 6,000"));
    }
    if in_flight.is_some_and(|t| !(1..=16).contains(&t)) {
        out.push(body_error("maxInFlight", "out_of_range", "1 to 16"));
    }
}

fn key_ok(key: &str) -> bool {
    key.len() <= 63
        && key.starts_with(|c: char| c.is_ascii_lowercase())
        && key.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

impl Check for WebhookEndpointCreate {
    fn check(&self) -> Vec<FieldError> {
        let mut out = Vec::new();
        if !key_ok(&self.key) {
            out.push(body_error(
                "key",
                "invalid_format",
                "Lower case letters, digits, _ and -, starting with a letter",
            ));
        }
        if self.name.is_empty() || self.name.chars().count() > 100 {
            out.push(body_error("name", "invalid_format", "1 to 100 characters"));
        }
        limits(self.timeout_ms, self.max_per_minute, self.max_in_flight, &mut out);
        if let Some(h) = &self.auth_header {
            header_problems(h, &mut out);
        }
        out
    }
}

impl Check for WebhookEndpointUpdate {
    fn check(&self) -> Vec<FieldError> {
        let mut out = Vec::new();
        if self.name.as_ref().is_some_and(|n| n.is_empty() || n.chars().count() > 100) {
            out.push(body_error("name", "invalid_format", "1 to 100 characters"));
        }
        limits(self.timeout_ms, self.max_per_minute, self.max_in_flight, &mut out);
        if let Some(Some(h)) = &self.auth_header {
            header_problems(h, &mut out);
        }
        out
    }
}

impl Check for WebhookSecretRotate {
    fn check(&self) -> Vec<FieldError> {
        match self.grace_hours {
            Some(h) if !(0..=MAX_GRACE_HOURS).contains(&h) => {
                vec![body_error("graceHours", "out_of_range", format!("0 to {MAX_GRACE_HOURS}"))]
            }
            _ => Vec::new(),
        }
    }
}

/// `GET /admin/webhook-endpoints`
#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ListWebhookEndpointsQuery {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    pub limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0)]
    pub offset: i64,
    /// Only endpoints in this status
    #[param(inline)]
    pub status: Option<WebhookEndpointStatus>,
}
crate::paged!(ListWebhookEndpointsQuery);

/// An allowlist entry: a host webhooks may reach
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebhookAllowedHost {
    pub id: Uuid,
    /// An exact host (IDNA form), `*.domain` (any name below it), or an IP address
    pub host_pattern: String,
    /// The only port allowed; null allows any
    pub port: Option<i32>,
    /// Plain http to this host, if the operator also allows it (WEBHOOK_ALLOW_HTTP)
    pub allow_http: bool,
    pub comment: Option<String>,
    pub created_at: DateTime<Utc>,
    pub created_by_name: String,
}

/// The allowlist, every entry (it is short)
#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WebhookAllowedHostList {
    pub data: Vec<WebhookAllowedHost>,
}

/// Allow webhooks to a host
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebhookAllowedHostCreate {
    /// `itsm.corp.example`, `*.corp.example` (one label or more below it, never a bare `*`), or an IP address
    #[schema(min_length = 1, max_length = 255)]
    pub host_pattern: String,
    #[schema(minimum = 1, maximum = 65535)]
    #[serde(default)]
    pub port: Option<i32>,
    #[serde(default)]
    pub allow_http: bool,
    #[schema(schema_with = comment_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    pub comment: Option<String>,
}

fn comment_schema() -> Schema {
    schemas::nullable_trimmed_schema(500)
}

impl Check for WebhookAllowedHostCreate {
    fn check(&self) -> Vec<FieldError> {
        let mut out = Vec::new();
        if let Err(e) = HostMatch::parse(&self.host_pattern) {
            out.push(body_error("hostPattern", "invalid_format", format!("Host pattern {e}")));
        }
        if self.port.is_some_and(|p| !(1..=65535).contains(&p)) {
            out.push(body_error("port", "out_of_range", "1 to 65,535"));
        }
        if self.comment.as_ref().is_some_and(|c| c.chars().count() > 500) {
            out.push(body_error("comment", "too_long", "At most 500 characters"));
        }
        out
    }
}

/// The allowlist entry was removed
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebhookAllowedHostDeleted {
    /// Keys of the endpoints suspended (`host_not_allowed`) because no entry allows their URL any more
    pub suspended_endpoints: Vec<String>,
}

/// What the test request came to
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebhookPingResult {
    /// The receiver answered 2xx
    pub ok: bool,
    /// The receiver's HTTP status, if it answered
    pub status_code: Option<u16>,
    /// Why it failed, as a delivery would record it: `host_not_allowed`, `http_not_allowed`,
    /// `address_blocked:<ip>`, `redirect_not_followed`, `http_status`, `unreachable`, `secret_unreadable`
    pub reason: Option<String>,
    pub message: String,
    pub duration_ms: i64,
}

// ---------------------------------------------------------------------------
// Stored form
// ---------------------------------------------------------------------------

pub(crate) const COLUMNS: &str = "id, key, name, url, status, suspended_reason, payload_version, timeout_ms, \
     max_per_minute, max_in_flight, secret_ciphertext, secret_key_id, previous_secret_ciphertext, \
     previous_secret_key_id, previous_secret_until, auth_header_name, auth_header_ciphertext, auth_header_key_id, \
     consecutive_failures, last_success_at, last_failure_at, created_at, updated_at, version";

#[derive(sqlx::FromRow)]
pub(crate) struct EndpointRow {
    pub id: Uuid,
    pub key: String,
    pub name: String,
    pub url: String,
    pub status: WebhookEndpointStatus,
    pub suspended_reason: Option<String>,
    pub payload_version: i16,
    pub timeout_ms: i32,
    pub max_per_minute: i32,
    pub max_in_flight: i16,
    pub secret_ciphertext: Vec<u8>,
    pub secret_key_id: i32,
    pub previous_secret_ciphertext: Option<Vec<u8>>,
    pub previous_secret_key_id: Option<i32>,
    pub previous_secret_until: Option<DateTime<Utc>>,
    pub auth_header_name: Option<String>,
    pub auth_header_ciphertext: Option<Vec<u8>>,
    pub auth_header_key_id: Option<i32>,
    pub consecutive_failures: i32,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_failure_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub version: i32,
}

impl EndpointRow {
    pub fn dto(&self) -> WebhookEndpoint {
        WebhookEndpoint {
            id: self.id,
            key: self.key.clone(),
            name: self.name.clone(),
            status: self.status,
            suspended_reason: self.suspended_reason.clone(),
            url: Some(self.url.clone()),
            unencrypted: Some(self.url.starts_with("http://")),
            payload_version: Some(self.payload_version),
            timeout_ms: Some(self.timeout_ms),
            max_per_minute: Some(self.max_per_minute),
            max_in_flight: Some(self.max_in_flight),
            auth_header_name: self.auth_header_name.clone(),
            auth_header_set: Some(self.auth_header_ciphertext.is_some()),
            previous_secret_until: self.previous_secret_until.filter(|u| *u > Utc::now()),
            consecutive_failures: Some(self.consecutive_failures),
            last_success_at: self.last_success_at,
            last_failure_at: self.last_failure_at,
            created_at: Some(self.created_at),
            updated_at: Some(self.updated_at),
            version: self.version,
        }
    }

    /// What the audit log records: the configuration, never a secret or header value.
    pub fn audited(&self) -> Value {
        json!({
            "key": self.key, "name": self.name, "url": self.url, "status": self.status,
            "suspendedReason": self.suspended_reason, "timeoutMs": self.timeout_ms,
            "maxPerMinute": self.max_per_minute, "maxInFlight": self.max_in_flight,
            "authHeaderName": self.auth_header_name, "authHeaderSet": self.auth_header_ciphertext.is_some(),
        })
    }

    /// The signing secrets to sign with now: the current one, and the previous one during its grace period.
    pub fn signing_secrets(&self, w: &Webhooks) -> Result<Vec<crate::secrets::Secret>, crate::secrets::OpenError> {
        let mut out = vec![sealed::open_endpoint_secret(
            &w.keyring,
            self.id,
            EndpointSecret::Secret,
            self.secret_key_id,
            &self.secret_ciphertext,
        )?];
        if let (Some(bytes), Some(k), Some(until)) =
            (&self.previous_secret_ciphertext, self.previous_secret_key_id, self.previous_secret_until)
            && until > Utc::now()
        {
            out.push(sealed::open_endpoint_secret(&w.keyring, self.id, EndpointSecret::PreviousSecret, k, bytes)?);
        }
        Ok(out)
    }

    /// The auth header's name and value, if one is set.
    pub fn auth_header(
        &self,
        w: &Webhooks,
    ) -> Result<Option<(String, crate::secrets::Secret)>, crate::secrets::OpenError> {
        match (&self.auth_header_name, &self.auth_header_ciphertext, self.auth_header_key_id) {
            (Some(name), Some(bytes), Some(k)) => Ok(Some((
                name.clone(),
                sealed::open_endpoint_secret(&w.keyring, self.id, EndpointSecret::AuthHeader, k, bytes)?,
            ))),
            _ => Ok(None),
        }
    }
}

pub(crate) async fn row(conn: &mut PgConnection, id: Uuid, for_update: bool) -> Result<EndpointRow, AppError> {
    let lock = if for_update { " FOR UPDATE" } else { "" };
    sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {COLUMNS} FROM cmdb.webhook_endpoints WHERE id = $1{lock}")))
        .bind(id)
        .fetch_optional(conn)
        .await?
        .ok_or_else(|| AppError::missing("Webhook endpoint", id))
}

#[derive(sqlx::FromRow)]
struct HostRow {
    id: Uuid,
    host_pattern: String,
    port: Option<i32>,
    allow_http: bool,
    comment: Option<String>,
    created_at: DateTime<Utc>,
    created_by_name: String,
}

impl HostRow {
    fn dto(&self) -> WebhookAllowedHost {
        WebhookAllowedHost {
            id: self.id,
            host_pattern: self.host_pattern.clone(),
            port: self.port,
            allow_http: self.allow_http,
            comment: self.comment.clone(),
            created_at: self.created_at,
            created_by_name: self.created_by_name.clone(),
        }
    }

    fn allowed(&self) -> Option<AllowedHost> {
        let host = HostMatch::parse(&self.host_pattern).ok()?;
        Some(AllowedHost {
            pattern: HostPattern { host, port: self.port.and_then(|p| u16::try_from(p).ok()) },
            allow_http: self.allow_http,
        })
    }
}

const HOST_COLUMNS: &str = "id, host_pattern, port, allow_http, comment, created_at, created_by_name";

/// The administrator's allowlist, as URL checks use it.
pub(crate) async fn allowlist(conn: &mut PgConnection) -> Result<Vec<AllowedHost>, AppError> {
    let rows: Vec<HostRow> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {HOST_COLUMNS} FROM cmdb.webhook_allowed_hosts")))
            .fetch_all(conn)
            .await?;
    Ok(rows.iter().filter_map(HostRow::allowed).collect())
}

impl Webhooks {
    pub(crate) fn ceiling(&self) -> Ceiling<'_> {
        Ceiling { hosts: self.cfg.allowed_hosts.as_ref(), allow_http: self.cfg.allow_http }
    }

    /// 409 WEBHOOKS_DISABLED unless the operator enabled webhooks.
    pub(crate) fn require_enabled(&self) -> Result<(), AppError> {
        if self.cfg.allowed {
            return Ok(());
        }
        Err(AppError::new(
            ErrorCode::WebhooksDisabled,
            "Webhooks are switched off on this server: the operator must set WEBHOOKS_ALLOWED=true. Nothing was changed",
        ))
    }
}

fn url_error(e: UrlRefusal) -> AppError {
    AppError::validation(vec![body_error("url", e.code, e.message)])
}

// ---------------------------------------------------------------------------
// Endpoints
// ---------------------------------------------------------------------------

/// A caller without `webhooks.manage` (but with `workflows.manage`) sees key, name and status.
fn limited(e: WebhookEndpoint) -> WebhookEndpoint {
    WebhookEndpoint {
        url: None,
        unencrypted: None,
        payload_version: None,
        timeout_ms: None,
        max_per_minute: None,
        max_in_flight: None,
        auth_header_name: None,
        auth_header_set: None,
        previous_secret_until: None,
        consecutive_failures: None,
        last_success_at: None,
        last_failure_at: None,
        created_at: None,
        updated_at: None,
        ..e
    }
}

pub async fn list(
    pool: &PgPool,
    ctx: &RequestContext,
    q: &ListWebhookEndpointsQuery,
) -> Result<Page<WebhookEndpoint>, AppError> {
    let full = ctx.require(GlobalPermission::WebhooksManage).is_ok();
    if !full {
        ctx.require(GlobalPermission::WorkflowsManage)?;
    }
    let status = q.status;
    let rows: Vec<EndpointRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM cmdb.webhook_endpoints WHERE $1::text IS NULL OR status = $1
         ORDER BY key LIMIT $2 OFFSET $3"
    )))
    .bind(status)
    .bind(q.limit)
    .bind(q.offset)
    .fetch_all(pool)
    .await?;
    let total: i64 =
        sqlx::query_scalar("SELECT count(*) FROM cmdb.webhook_endpoints WHERE $1::text IS NULL OR status = $1")
            .bind(status)
            .fetch_one(pool)
            .await?;
    let data = rows.iter().map(|r| if full { r.dto() } else { limited(r.dto()) }).collect();
    Ok(Page { data, page: q.page_meta(total) })
}

pub async fn get(pool: &PgPool, id: Uuid) -> Result<WebhookEndpoint, AppError> {
    Ok(row(&mut *pool.acquire().await?, id, false).await?.dto())
}

fn audit(action: AuditAction, entity_id: Uuid, old: Option<Value>, new: Option<Value>) -> AuditEntry {
    AuditEntry { action, entity_type: ENTITY, entity_id, old_value: old, new_value: new }
}

fn conflict_on_key(e: sqlx::Error, key: &str) -> AppError {
    if let sqlx::Error::Database(db) = &e
        && db.code().as_deref() == Some("23505")
    {
        return AppError::new(ErrorCode::Conflict, format!("A webhook endpoint with key {key} exists"))
            .with_details(vec![body_error("key", "duplicate", "Taken")]);
    }
    e.into()
}

pub async fn create(
    pool: &PgPool,
    ctx: &RequestContext,
    w: &Webhooks,
    b: &WebhookEndpointCreate,
) -> Result<WebhookEndpointCreated, AppError> {
    w.require_enabled()?;
    let mut tx = pool.begin().await?;
    let list = allowlist(&mut tx).await?;
    let url = check_url(&b.url, w.ceiling(), &list).map_err(url_error)?;
    let id = Uuid::new_v4();
    let secret = super::signing::generate();
    let sealed_secret = sealed::seal_endpoint_secret(&w.keyring, id, EndpointSecret::Secret, &secret);
    let header = b.auth_header.as_ref().map(|h| {
        (
            h.name.clone(),
            sealed::seal_endpoint_secret(&w.keyring, id, EndpointSecret::AuthHeader, h.value.expose().as_bytes()),
        )
    });
    sqlx::query(
        "INSERT INTO cmdb.webhook_endpoints (id, key, name, url, timeout_ms, max_per_minute, max_in_flight,
           secret_ciphertext, secret_key_id, auth_header_name, auth_header_ciphertext, auth_header_key_id)
         VALUES ($1, $2, $3, $4, coalesce($5, 10000), coalesce($6, 120), coalesce($7, 2), $8, $9, $10, $11, $12)",
    )
    .bind(id)
    .bind(&b.key)
    .bind(&b.name)
    .bind(url.as_str())
    .bind(b.timeout_ms)
    .bind(b.max_per_minute)
    .bind(b.max_in_flight)
    .bind(&sealed_secret.bytes)
    .bind(sealed_secret.key_id.0)
    .bind(header.as_ref().map(|h| &h.0))
    .bind(header.as_ref().map(|h| &h.1.bytes))
    .bind(header.as_ref().map(|h| h.1.key_id.0))
    .execute(&mut *tx)
    .await
    .map_err(|e| conflict_on_key(e, &b.key))?;
    let r = row(&mut tx, id, false).await?;
    crud::write_audit(&mut tx, ctx, vec![audit(AuditAction::Create, id, None, Some(r.audited()))]).await?;
    tx.commit().await?;
    Ok(WebhookEndpointCreated { endpoint: r.dto(), secret: super::signing::display(&secret) })
}

fn check_version(sent: i32, current: i32) -> Result<(), AppError> {
    if sent == current {
        return Ok(());
    }
    Err(AppError::new(
        ErrorCode::VersionConflict,
        format!(
            "The webhook endpoint was changed by someone else (you sent version {sent}, current is {current}). \
             Reload and retry."
        ),
    )
    .with_details(vec![body_error("version", "stale", format!("Current is {current}"))]))
}

pub async fn update(
    pool: &PgPool,
    ctx: &RequestContext,
    w: &Webhooks,
    id: Uuid,
    b: &WebhookEndpointUpdate,
) -> Result<WebhookEndpoint, AppError> {
    w.require_enabled()?;
    let mut tx = pool.begin().await?;
    let before = row(&mut tx, id, true).await?;
    check_version(b.version, before.version)?;
    let url = match &b.url {
        Some(u) => Some(check_url(u, w.ceiling(), &allowlist(&mut tx).await?).map_err(url_error)?.to_string()),
        None => None,
    };
    let (header_change, name, bytes, key_id) = match &b.auth_header {
        None => (false, None, None, None),
        Some(None) => (true, None, None, None),
        Some(Some(h)) => {
            let s =
                sealed::seal_endpoint_secret(&w.keyring, id, EndpointSecret::AuthHeader, h.value.expose().as_bytes());
            (true, Some(h.name.clone()), Some(s.bytes), Some(s.key_id.0))
        }
    };
    sqlx::query(
        "UPDATE cmdb.webhook_endpoints SET name = coalesce($2, name), url = coalesce($3, url),
           timeout_ms = coalesce($4, timeout_ms), max_per_minute = coalesce($5, max_per_minute),
           max_in_flight = coalesce($6, max_in_flight),
           auth_header_name = CASE WHEN $7 THEN $8 ELSE auth_header_name END,
           auth_header_ciphertext = CASE WHEN $7 THEN $9 ELSE auth_header_ciphertext END,
           auth_header_key_id = CASE WHEN $7 THEN $10 ELSE auth_header_key_id END,
           version = version + 1
         WHERE id = $1",
    )
    .bind(id)
    .bind(&b.name)
    .bind(&url)
    .bind(b.timeout_ms)
    .bind(b.max_per_minute)
    .bind(b.max_in_flight)
    .bind(header_change)
    .bind(name)
    .bind(bytes)
    .bind(key_id)
    .execute(&mut *tx)
    .await?;
    let after = row(&mut tx, id, false).await?;
    if after.audited() == before.audited() && !header_change {
        // Nothing changed: no new version, no audit row.
        tx.rollback().await?;
        return Ok(before.dto());
    }
    crud::write_audit(
        &mut tx,
        ctx,
        vec![audit(AuditAction::Update, id, Some(before.audited()), Some(after.audited()))],
    )
    .await?;
    tx.commit().await?;
    Ok(after.dto())
}

pub async fn remove(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    let before = row(&mut tx, id, true).await?;
    let users: Vec<(String, String)> = sqlx::query_as(
        "SELECT d.key, a.key FROM cmdb.workflow_actions a JOIN cmdb.workflow_definitions d ON d.id = a.definition_id
         WHERE a.endpoint_id = $1 ORDER BY d.key, a.key",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    if !users.is_empty() {
        let named: Vec<String> = users.iter().map(|(d, a)| format!("{d}.{a}")).collect();
        return Err(AppError::new(
            ErrorCode::InUse,
            format!(
                "Webhook endpoint {} is used by workflow actions {}; remove it from them first, or pause it",
                before.key,
                named.join(", ")
            ),
        ));
    }
    // Deliveries still waiting for it die now, audited, rather than expire unnoticed.
    let dead: Vec<Uuid> = sqlx::query_scalar(
        "UPDATE cmdb.workflow_action_deliveries SET status = 'dead', status_reason = 'endpoint_deleted',
           completed_at = now(), last_error = 'The webhook endpoint was deleted'
         WHERE endpoint_id = $1 AND status IN ('pending', 'held') RETURNING id",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    crate::modules::workflows::actions::outbox::audit_dead(&mut tx, &dead).await?;
    sqlx::query("DELETE FROM cmdb.webhook_endpoints WHERE id = $1").bind(id).execute(&mut *tx).await?;
    crud::write_audit(&mut tx, ctx, vec![audit(AuditAction::Delete, id, Some(before.audited()), None)]).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn rotate(
    pool: &PgPool,
    ctx: &RequestContext,
    w: &Webhooks,
    id: Uuid,
    b: &WebhookSecretRotate,
) -> Result<WebhookSecretRotated, AppError> {
    w.require_enabled()?;
    let grace = b.grace_hours.unwrap_or(DEFAULT_GRACE_HOURS);
    let mut tx = pool.begin().await?;
    let before = row(&mut tx, id, true).await?;
    let secret = super::signing::generate();
    let sealed_secret = sealed::seal_endpoint_secret(&w.keyring, id, EndpointSecret::Secret, &secret);
    // The current secret becomes the previous one for the grace period, if it
    // still opens (one that does not, e.g. after a restore under another key,
    // cannot sign anything and is dropped).
    let keep_previous = grace > 0
        && sealed::open_endpoint_secret(
            &w.keyring,
            id,
            EndpointSecret::Secret,
            before.secret_key_id,
            &before.secret_ciphertext,
        )
        .is_ok();
    let until = keep_previous.then(|| Utc::now() + TimeDelta::hours(i64::from(grace)));
    sqlx::query(
        "UPDATE cmdb.webhook_endpoints SET secret_ciphertext = $2, secret_key_id = $3,
           previous_secret_ciphertext = CASE WHEN $4 THEN secret_ciphertext END,
           previous_secret_key_id = CASE WHEN $4 THEN secret_key_id END,
           previous_secret_until = $5,
           status = CASE WHEN suspended_reason = 'secret_required' THEN 'paused' ELSE status END,
           suspended_reason = CASE WHEN suspended_reason = 'secret_required' THEN NULL ELSE suspended_reason END,
           version = version + 1
         WHERE id = $1",
    )
    .bind(id)
    .bind(&sealed_secret.bytes)
    .bind(sealed_secret.key_id.0)
    .bind(keep_previous)
    .bind(until)
    .execute(&mut *tx)
    .await?;
    let after = row(&mut tx, id, false).await?;
    crud::write_audit(
        &mut tx,
        ctx,
        vec![audit(
            AuditAction::WebhookEndpointRotateSecret,
            id,
            None,
            Some(json!({ "key": after.key, "graceHours": if keep_previous { grace } else { 0 } })),
        )],
    )
    .await?;
    tx.commit().await?;
    Ok(WebhookSecretRotated { endpoint: after.dto(), secret: super::signing::display(&secret) })
}

/// Holds the endpoint's waiting deliveries (it is not active any more).
pub(crate) async fn hold(conn: &mut PgConnection, id: Uuid) -> sqlx::Result<u64> {
    Ok(sqlx::query(
        "UPDATE cmdb.workflow_action_deliveries SET status = 'held' WHERE endpoint_id = $1 AND status = 'pending'",
    )
    .bind(id)
    .execute(conn)
    .await?
    .rows_affected())
}

/// Suspends endpoint `id` with `reason`, holds its deliveries and audits it
/// as `actor`. Returns false when it was not active or paused.
pub(crate) async fn suspend(
    conn: &mut PgConnection,
    actor: &RequestContext,
    id: Uuid,
    reason: &str,
) -> Result<bool, AppError> {
    let before: Option<(String, i32, WebhookEndpointStatus)> =
        sqlx::query_as("SELECT key, consecutive_failures, status FROM cmdb.webhook_endpoints WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
    let Some((key, failures, from)) = before.filter(|b| b.2 != WebhookEndpointStatus::Suspended) else {
        return Ok(false);
    };
    sqlx::query(
        "UPDATE cmdb.webhook_endpoints SET status = 'suspended', suspended_reason = $2, version = version + 1
         WHERE id = $1",
    )
    .bind(id)
    .bind(reason)
    .execute(&mut *conn)
    .await?;
    hold(&mut *conn, id).await?;
    crud::write_audit(
        &mut *conn,
        actor,
        vec![audit(
            AuditAction::WebhookEndpointSuspend,
            id,
            None,
            Some(json!({ "key": key, "reason": reason, "from": from, "consecutiveFailures": failures })),
        )],
    )
    .await?;
    Ok(true)
}

pub async fn pause(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<WebhookEndpoint, AppError> {
    let mut tx = pool.begin().await?;
    let before = row(&mut tx, id, true).await?;
    if before.status == WebhookEndpointStatus::Active {
        sqlx::query("UPDATE cmdb.webhook_endpoints SET status = 'paused', version = version + 1 WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        hold(&mut tx, id).await?;
        crud::write_audit(
            &mut tx,
            ctx,
            vec![audit(
                AuditAction::WebhookEndpointSuspend,
                id,
                None,
                Some(json!({ "key": before.key, "reason": "paused", "from": "active",
                    "consecutiveFailures": before.consecutive_failures })),
            )],
        )
        .await?;
    }
    let after = row(&mut tx, id, false).await?;
    tx.commit().await?;
    Ok(after.dto())
}

/// Resumes a paused or suspended endpoint: its URL must still be allowed and
/// its secrets must decrypt; its held deliveries are sent again.
pub async fn resume(pool: &PgPool, ctx: &RequestContext, w: &Webhooks, id: Uuid) -> Result<WebhookEndpoint, AppError> {
    w.require_enabled()?;
    let mut tx = pool.begin().await?;
    let before = row(&mut tx, id, true).await?;
    if before.status == WebhookEndpointStatus::Active {
        tx.rollback().await?;
        return Ok(before.dto());
    }
    check_url(&before.url, w.ceiling(), &allowlist(&mut tx).await?).map_err(url_error)?;
    if before.suspended_reason.as_deref() == Some("secret_required") || before.signing_secrets(w).is_err() {
        return Err(AppError::new(
            ErrorCode::SecretRequired,
            format!(
                "The signing secret of webhook endpoint {} cannot be used here (created by an import, or encrypted \
                 with another key): rotate the secret, share it with the receiver, then resume",
                before.key
            ),
        )
        .with_details(vec![body_error("secret", "secret_required", "Rotate the signing secret first")]));
    }
    if before.auth_header(w).is_err() {
        return Err(AppError::new(
            ErrorCode::SecretRequired,
            format!(
                "The auth header of webhook endpoint {} cannot be decrypted: set it again, then resume",
                before.key
            ),
        )
        .with_details(vec![body_error("authHeader", "secret_required", "Set the header again")]));
    }
    sqlx::query(
        "UPDATE cmdb.webhook_endpoints SET status = 'active', suspended_reason = NULL, consecutive_failures = 0,
           version = version + 1 WHERE id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    let released = sqlx::query(
        "UPDATE cmdb.workflow_action_deliveries SET status = 'pending', next_attempt_at = now()
         WHERE endpoint_id = $1 AND status = 'held'",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    crud::write_audit(
        &mut tx,
        ctx,
        vec![audit(
            AuditAction::WebhookEndpointResume,
            id,
            None,
            Some(json!({ "key": before.key, "from": before.status, "reason": before.suspended_reason,
                "released": released })),
        )],
    )
    .await?;
    let after = row(&mut tx, id, false).await?;
    tx.commit().await?;
    Ok(after.dto())
}

// ---------------------------------------------------------------------------
// Allowlist
// ---------------------------------------------------------------------------

pub async fn list_hosts(pool: &PgPool) -> Result<WebhookAllowedHostList, AppError> {
    let rows: Vec<HostRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {HOST_COLUMNS} FROM cmdb.webhook_allowed_hosts ORDER BY host_pattern, port NULLS FIRST"
    )))
    .fetch_all(pool)
    .await?;
    Ok(WebhookAllowedHostList { data: rows.iter().map(HostRow::dto).collect() })
}

fn host_audit(r: &HostRow) -> Value {
    json!({ "hostPattern": r.host_pattern, "port": r.port, "allowHttp": r.allow_http, "comment": r.comment })
}

pub async fn create_host(
    pool: &PgPool,
    ctx: &RequestContext,
    w: &Webhooks,
    b: &WebhookAllowedHostCreate,
) -> Result<WebhookAllowedHost, AppError> {
    w.require_enabled()?;
    let host = HostMatch::parse(&b.host_pattern).map_err(|e| {
        AppError::validation(vec![body_error("hostPattern", "invalid_format", format!("Host pattern {e}"))])
    })?;
    let pattern = HostPattern { host, port: b.port.and_then(|p| u16::try_from(p).ok()) };
    if let Some(ceiling) = &w.cfg.allowed_hosts
        && !ceiling.contains(&pattern)
    {
        return Err(AppError::validation(vec![body_error(
            "hostPattern",
            "host_not_allowed",
            format!(
                "{pattern} is wider than or outside the hosts the server's operator allows (WEBHOOK_ALLOWED_HOSTS); \
                 the allowlist can only narrow them"
            ),
        )]));
    }
    if b.allow_http && !w.cfg.allow_http {
        return Err(AppError::validation(vec![body_error(
            "allowHttp",
            "http_not_allowed",
            "The server's operator does not allow plain http (WEBHOOK_ALLOW_HTTP=false)",
        )]));
    }
    let mut tx = pool.begin().await?;
    let (_, name) = crate::modules::workflows::service::actor(ctx);
    let r: HostRow = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "INSERT INTO cmdb.webhook_allowed_hosts (host_pattern, port, allow_http, comment, created_by_name)
         VALUES ($1, $2, $3, $4, $5) RETURNING {HOST_COLUMNS}"
    )))
    .bind(pattern.host.stored())
    .bind(b.port)
    .bind(b.allow_http)
    .bind(&b.comment)
    .bind(name)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.code().as_deref() == Some("23505") => {
            AppError::new(ErrorCode::Conflict, format!("{pattern} is on the allowlist already"))
        }
        _ => e.into(),
    })?;
    crud::write_audit(
        &mut tx,
        ctx,
        vec![AuditEntry {
            action: AuditAction::Create,
            entity_type: HOSTS,
            entity_id: r.id,
            old_value: None,
            new_value: Some(host_audit(&r)),
        }],
    )
    .await?;
    tx.commit().await?;
    Ok(r.dto())
}

/// Removes an entry and suspends (`host_not_allowed`) the endpoints whose URL no entry allows any more.
pub async fn delete_host(
    pool: &PgPool,
    ctx: &RequestContext,
    w: &Webhooks,
    id: Uuid,
) -> Result<WebhookAllowedHostDeleted, AppError> {
    let mut tx = pool.begin().await?;
    // One allowlist change at a time, so two removals cannot each miss the other's effect.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('shadoucmdb:webhook-allowlist'))").execute(&mut *tx).await?;
    let r: HostRow = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "DELETE FROM cmdb.webhook_allowed_hosts WHERE id = $1 RETURNING {HOST_COLUMNS}"
    )))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::missing("Allowlist entry", id))?;
    crud::write_audit(
        &mut tx,
        ctx,
        vec![AuditEntry {
            action: AuditAction::Delete,
            entity_type: HOSTS,
            entity_id: r.id,
            old_value: Some(host_audit(&r)),
            new_value: None,
        }],
    )
    .await?;
    let list = allowlist(&mut tx).await?;
    let endpoints: Vec<(Uuid, String, String)> =
        sqlx::query_as("SELECT id, key, url FROM cmdb.webhook_endpoints WHERE status <> 'suspended' ORDER BY key")
            .fetch_all(&mut *tx)
            .await?;
    let mut suspended = Vec::new();
    for (eid, key, url) in endpoints {
        if check_url(&url, w.ceiling(), &list).is_err() && suspend(&mut tx, ctx, eid, "host_not_allowed").await? {
            suspended.push(key);
        }
    }
    tx.commit().await?;
    Ok(WebhookAllowedHostDeleted { suspended_endpoints: suspended })
}

// ---------------------------------------------------------------------------
// Lost key
// ---------------------------------------------------------------------------

/// `shadoucmdb webhooks reset-undecryptable`: suspends each endpoint
/// (`secret_required`), replaces its signing secret with a fresh one that is
/// never shown (the administrator rotates it to get one), and clears what
/// else does not decrypt. Audited as an update without secrets.
pub async fn reset_undecryptable(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    keyring: &crate::secrets::Keyring,
    endpoints: &[sealed::UndecryptableEndpoint],
) -> Result<(), AppError> {
    for e in endpoints {
        let before: EndpointRow = row(&mut *conn, e.id, true).await?;
        let fresh = sealed::seal_endpoint_secret(keyring, e.id, EndpointSecret::Secret, &super::signing::generate());
        let known = [Some(keyring.active_id().0), keyring.previous_id().map(|k| k.0)];
        let header_lost = before.auth_header_key_id.is_some_and(|k| !known.contains(&Some(k)));
        sqlx::query(
            "UPDATE cmdb.webhook_endpoints SET secret_ciphertext = $2, secret_key_id = $3,
               previous_secret_ciphertext = NULL, previous_secret_key_id = NULL, previous_secret_until = NULL,
               auth_header_name = CASE WHEN $4 THEN NULL ELSE auth_header_name END,
               auth_header_ciphertext = CASE WHEN $4 THEN NULL ELSE auth_header_ciphertext END,
               auth_header_key_id = CASE WHEN $4 THEN NULL ELSE auth_header_key_id END,
               status = 'suspended', suspended_reason = 'secret_required', version = version + 1
             WHERE id = $1",
        )
        .bind(e.id)
        .bind(&fresh.bytes)
        .bind(fresh.key_id.0)
        .bind(header_lost)
        .execute(&mut *conn)
        .await?;
        hold(&mut *conn, e.id).await?;
        let after = row(&mut *conn, e.id, false).await?;
        crud::write_audit(
            &mut *conn,
            ctx,
            vec![audit(AuditAction::Update, e.id, Some(before.audited()), Some(after.audited()))],
        )
        .await?;
    }
    Ok(())
}
