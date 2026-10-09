//! The operations API of the workflow action outbox (design SHAA-2725 §4.4,
//! §8, §9, §11.2; slice S3b): the deliveries of one workflow, a delivery's
//! detail, retry and discard (one or in bulk), and the per-action summary.
//!
//! Everything needs `workflows.manage` and the view right on the workflow's
//! types (as the definition itself, 404 otherwise); webhook deliveries are
//! listed and changed only with `webhooks.manage` as well (§8). A delivery's
//! CI label is shown only with the view right on its type.
//!
//! - **Retry** gives a `dead` or `held` delivery of a sending channel fresh
//!   attempts: `pending`, `attempts` 0, due now, its max age counted from the
//!   retry (`retried_at`, 0076). The delivery loop claims it like any other.
//! - **Discard** gives up on a `pending`, `held` or `dead` one: `dead` with
//!   reason `discarded`, never claimed again. The API shows it as status
//!   `discarded`.
//!
//! A delivery being sent (`sending`) is neither: its lease decides. Each
//! change is one audit row per delivery (`workflow.action_retry`,
//! `workflow.action_discard`, the caller as actor), with the status before and
//! after and the CI in `ciId`, never the recipient's address.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgConnection, PgPool, QueryBuilder};
use utoipa::openapi::schema::{ObjectBuilder, Schema, Type};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::WorkflowActionKind;
use crate::api::context::RequestContext;
use crate::api::route::{Check, PathInput};
use crate::api::schemas::{self as api_schemas, Page, Paged, Sort, key_schema, ts, ts_opt};
use crate::api::validate;
use crate::auth::permissions::GlobalPermission;
use crate::config::WorkflowActionsConfig;
use crate::data::crud::{self, AuditAction, AuditEntry, Where};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::modules::workflows::service;
use crate::paged;

/// Deliveries one bulk request changes at most.
pub const MAX_BULK: usize = 1000;

// ---------------------------------------------------------------------------
// API shapes
// ---------------------------------------------------------------------------

/// Where a delivery stands
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum WorkflowActionDeliveryStatus {
    /// Waiting for its next attempt (`nextAttemptAt`)
    Pending,
    /// An attempt is in flight
    Sending,
    /// Held while its webhook endpoint is suspended
    Held,
    Delivered,
    /// Not sent, by rule (`statusReason`: `no_view`, `inactive`, `actor`, ...)
    Skipped,
    /// Given up (`statusReason`: `max_attempts`, `expired`, `restored`, a permanent failure)
    Dead,
    /// Given up by an administrator (stored as `dead` with reason `discarded`)
    Discarded,
}

impl WorkflowActionDeliveryStatus {
    /// The condition on `d` (a delivery) that matches this status.
    fn sql(self) -> &'static str {
        match self {
            Self::Pending => "d.status = 'pending'",
            Self::Sending => "d.status = 'sending'",
            Self::Held => "d.status = 'held'",
            Self::Delivered => "d.status = 'delivered'",
            Self::Skipped => "d.status = 'skipped'",
            Self::Dead => "d.status = 'dead' AND d.status_reason <> 'discarded'",
            Self::Discarded => "d.status = 'dead' AND d.status_reason = 'discarded'",
        }
    }
}

/// The API status of `d`.
const STATUS: &str = "CASE WHEN d.status = 'dead' AND d.status_reason = 'discarded' THEN 'discarded' ELSE d.status END";
/// Retry: dead or held, of a channel that sends (the inbox delivers at fan-out).
const RETRYABLE: &str =
    "r.kind <> 'inbox' AND (d.status = 'held' OR (d.status = 'dead' AND d.status_reason <> 'discarded'))";
/// Discard: anything not sent, not in flight and not discarded yet.
const DISCARDABLE: &str = "(d.status IN ('pending', 'held') OR (d.status = 'dead' AND d.status_reason <> 'discarded'))";

/// What a delivery is addressed to
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowActionRecipientKind {
    /// A user (inbox, e-mail to their current address)
    User,
    /// A fixed e-mail address (shown masked)
    Address,
    /// A webhook endpoint
    Endpoint,
}

/// Who or what a delivery goes to
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowActionDeliveryRecipient {
    pub kind: WorkflowActionRecipientKind,
    /// The user's or the endpoint's id; null once it was deleted, and for an address
    pub id: Option<Uuid>,
    /// The user's display name or the endpoint's name; null once it was deleted, and for an address
    pub name: Option<String>,
    /// The user's username
    pub username: Option<String>,
    /// A fixed address, masked (`c***@corp.example`)
    pub address: Option<String>,
}

/// One message of an action to one recipient
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowActionDelivery {
    pub id: Uuid,
    pub run_id: i64,
    /// The instance event that queued the run
    pub event_id: i64,
    pub action_key: String,
    /// The action's name; null once the action was deleted
    pub action_name: Option<String>,
    pub kind: WorkflowActionKind,
    pub instance_id: Uuid,
    pub ci_id: Uuid,
    /// The CI's label; null when the CI is gone or the caller may not view its type
    pub ci_label: Option<String>,
    pub recipient: WorkflowActionDeliveryRecipient,
    pub status: WorkflowActionDeliveryStatus,
    pub status_reason: Option<String>,
    /// Attempts made (since the last retry)
    pub attempts: i16,
    /// When a pending delivery is due; null otherwise
    #[serde(serialize_with = "ts_opt::serialize")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub next_attempt_at: Option<DateTime<Utc>>,
    /// The last attempt's HTTP or SMTP status code
    pub last_status_code: Option<i32>,
    /// When the run queued it
    #[serde(serialize_with = "ts::serialize")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: DateTime<Utc>,
    /// When it was delivered, skipped or given up
    #[serde(serialize_with = "ts_opt::serialize")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub completed_at: Option<DateTime<Utc>>,
    /// When an administrator last retried it
    #[serde(serialize_with = "ts_opt::serialize")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub retried_at: Option<DateTime<Utc>>,
}

/// The instance event a delivery tells of
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowActionDeliveryEvent {
    /// `transition`, `start`, `cancel`, `force`, `approval_requested`, ...
    pub kind: String,
    pub transition_key: Option<String>,
    pub from_state_key: Option<String>,
    pub to_state_key: String,
    /// Who caused it, as recorded
    pub actor_name: Option<String>,
    #[serde(serialize_with = "ts::serialize")]
    #[schema(value_type = String, format = DateTime)]
    pub occurred_at: DateTime<Utc>,
}

/// A delivery with its last error, its run and its event
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowActionDeliveryDetail {
    pub delivery: WorkflowActionDelivery,
    /// The last attempt's error (at most 1 KiB): connection, TLS or protocol detail
    pub last_error: Option<String>,
    /// The run's status: `fanned_out` once its deliveries were written
    pub run_status: String,
    pub run_status_reason: Option<String>,
    /// The event; null once it went to the archive with its CI
    pub event: Option<WorkflowActionDeliveryEvent>,
}

fn delivery_sort() -> Schema {
    api_schemas::sort_schema(
        &["createdAt", "completedAt", "nextAttemptAt", "attempts", "status", "actionKey"],
        "-createdAt",
    )
}

fn from_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .format(Some(utoipa::openapi::SchemaFormat::KnownFormat(utoipa::openapi::KnownFormat::DateTime)))
        .description(Some("Only deliveries queued at or after this time"))
        .into()
}

fn to_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .format(Some(utoipa::openapi::SchemaFormat::KnownFormat(utoipa::openapi::KnownFormat::DateTime)))
        .description(Some("Only deliveries queued before this time"))
        .into()
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct WorkflowActionDeliveryList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    pub limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    pub offset: i64,
    /// Only deliveries of this action
    #[param(schema_with = key_schema)]
    pub action_key: Option<String>,
    #[param(inline)]
    pub status: Option<WorkflowActionDeliveryStatus>,
    #[param(inline)]
    pub kind: Option<WorkflowActionKind>,
    #[param(schema_with = from_schema)]
    pub from: Option<DateTime<Utc>>,
    #[param(schema_with = to_schema)]
    pub to: Option<DateTime<Utc>>,
    /// Only deliveries of this instance
    pub instance_id: Option<Uuid>,
    /// Only deliveries of this instance event
    #[param(minimum = 1)]
    pub event_id: Option<i64>,
    /// Only deliveries about this CI
    pub ci_id: Option<Uuid>,
    #[param(required = false, schema_with = delivery_sort)]
    pub sort: Sort,
}
paged!(WorkflowActionDeliveryList);

/// The deliveries a bulk retry or discard selects (all optional; at most 1,000, oldest first)
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowActionDeliveryFilter {
    #[schema(schema_with = key_schema)]
    pub action_key: Option<String>,
    pub status: Option<WorkflowActionDeliveryStatus>,
    pub kind: Option<WorkflowActionKind>,
    /// Queued at or after
    #[schema(value_type = Option<String>, format = DateTime)]
    pub from: Option<DateTime<Utc>>,
    /// Queued before
    #[schema(value_type = Option<String>, format = DateTime)]
    pub to: Option<DateTime<Utc>>,
    pub instance_id: Option<Uuid>,
    #[schema(minimum = 1)]
    pub event_id: Option<i64>,
    pub ci_id: Option<Uuid>,
}

impl From<&WorkflowActionDeliveryList> for WorkflowActionDeliveryFilter {
    fn from(q: &WorkflowActionDeliveryList) -> Self {
        WorkflowActionDeliveryFilter {
            action_key: q.action_key.clone(),
            status: q.status,
            kind: q.kind,
            from: q.from,
            to: q.to,
            instance_id: q.instance_id,
            event_id: q.event_id,
            ci_id: q.ci_id,
        }
    }
}

/// Retry or discard several deliveries: by id, or every one a filter selects
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowActionDeliveryBulk {
    /// These deliveries (1-1000)
    #[schema(min_items = 1, max_items = 1000)]
    pub ids: Option<Vec<Uuid>>,
    /// The first 1,000 deliveries this selects that the operation applies to, oldest first
    pub filter: Option<WorkflowActionDeliveryFilter>,
}

impl Check for WorkflowActionDeliveryBulk {
    fn check(&self) -> Vec<FieldError> {
        let error = |field: &str, code: &str, message: &str| FieldError {
            location: FieldLocation::Body,
            field: field.into(),
            message: message.into(),
            code: code.into(),
        };
        match (&self.ids, &self.filter) {
            (Some(_), Some(_)) => vec![error("filter", "not_applicable", "Send either ids or filter, not both")],
            (None, None) => vec![error("ids", "required", "Send ids or filter")],
            (Some(ids), None) if ids.is_empty() => {
                vec![error("ids", "too_small", "Too small: expected array to have >=1 items")]
            }
            (Some(ids), None) if ids.len() > MAX_BULK => {
                vec![error("ids", "too_big", "Too big: expected array to have <=1000 items")]
            }
            _ => Vec::new(),
        }
    }
}

/// Why a delivery was left as it was
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[allow(clippy::enum_variant_names)]
pub enum WorkflowActionDeliveryRefusal {
    /// No such delivery of this workflow that the caller may see
    NotFound,
    /// Retry: only `dead` and `held` deliveries of e-mail and webhook actions (inbox entries are written at fan-out)
    NotRetryable,
    /// Discard: only `pending`, `held` and `dead` deliveries
    NotDiscardable,
}

/// A delivery a bulk request left as it was
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowActionDeliveryRefused {
    pub id: Uuid,
    pub reason: WorkflowActionDeliveryRefusal,
    /// Its status, when it exists
    pub status: Option<WorkflowActionDeliveryStatus>,
}

/// What a bulk retry or discard did
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowActionDeliveryBulkResult {
    /// Deliveries changed (one audit row each)
    pub changed: i64,
    /// Their ids
    pub ids: Vec<Uuid>,
    /// With `ids`: those left as they were, and why
    pub refused: Vec<WorkflowActionDeliveryRefused>,
    /// With `filter`: more deliveries match than one request changes; send it again
    pub more: bool,
}

/// Deliveries by status
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowActionDeliveryCounts {
    pub pending: i64,
    pub sending: i64,
    pub held: i64,
    pub delivered: i64,
    pub skipped: i64,
    pub dead: i64,
    pub discarded: i64,
}

impl WorkflowActionDeliveryCounts {
    fn add(&mut self, status: WorkflowActionDeliveryStatus, n: i64) {
        use WorkflowActionDeliveryStatus as S;
        *match status {
            S::Pending => &mut self.pending,
            S::Sending => &mut self.sending,
            S::Held => &mut self.held,
            S::Delivered => &mut self.delivered,
            S::Skipped => &mut self.skipped,
            S::Dead => &mut self.dead,
            S::Discarded => &mut self.discarded,
        } += n;
    }
}

/// One action's deliveries
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowActionSummary {
    pub key: String,
    /// Null for an action deleted since (its deliveries keep its key)
    pub name: Option<String>,
    pub kind: WorkflowActionKind,
    /// False when disabled or deleted
    pub enabled: bool,
    /// Queued in the last 24 hours, by status now
    pub last24h: WorkflowActionDeliveryCounts,
    /// Queued in the last 7 days, by status now
    pub last7d: WorkflowActionDeliveryCounts,
    /// Runs suppressed in the last 24 hours (queue full, loop breaker): nothing was delivered for them
    pub suppressed24h: i64,
    /// When the oldest pending delivery was queued (or retried)
    #[serde(serialize_with = "ts_opt::serialize")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub oldest_pending_at: Option<DateTime<Utc>>,
    /// Its age in seconds
    pub oldest_pending_age_seconds: Option<i64>,
}

/// The whole action queue (every workflow) and its limits
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowActionQueue {
    /// Pending runs and deliveries reached `WORKFLOW_ACTIONS_QUEUE_MAX`: new runs are suppressed
    pub overloaded: bool,
    /// Pending runs and pending or held deliveries, as the workers last counted them (capped at `queueMax`)
    pub backlog: i64,
    /// When the workers last counted; old when no worker runs
    #[serde(serialize_with = "ts::serialize")]
    #[schema(value_type = String, format = DateTime)]
    pub checked_at: DateTime<Utc>,
    /// `WORKFLOW_ACTIONS_QUEUE_MAX`
    pub queue_max: i64,
    /// `WORKFLOW_ACTIONS_MAX_PER_INSTANCE_PER_HOUR`, as the workers last wrote it
    pub max_per_instance_per_hour: i32,
    /// `WORKFLOW_ACTIONS_MAX_ATTEMPTS`
    pub max_attempts: i16,
    /// `WORKFLOW_ACTIONS_MAX_AGE_HOURS`
    pub max_age_hours: i32,
}

/// How a workflow's actions are being delivered
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowActionsSummary {
    /// The workflow's actions in order, then deleted actions that still have deliveries
    pub actions: Vec<WorkflowActionSummary>,
    pub queue: WorkflowActionQueue,
}

/// `{id}/action-deliveries/{deliveryId}`: a definition and one of its deliveries.
pub struct DeliveryPath(pub Uuid, pub Uuid);

impl PathInput for DeliveryPath {
    fn params() -> Vec<utoipa::openapi::path::Parameter> {
        use utoipa::openapi::Required;
        use utoipa::openapi::path::{ParameterBuilder, ParameterIn};
        ["id", "deliveryId"]
            .into_iter()
            .map(|name| {
                ParameterBuilder::new()
                    .name(name)
                    .parameter_in(ParameterIn::Path)
                    .required(Required::True)
                    .schema(Some(api_schemas::uuid_builder()))
                    .build()
            })
            .collect()
    }
    fn parse(raw: &axum::extract::RawPathParams) -> Result<Self, AppError> {
        let get = |name: &str| -> Result<Uuid, FieldError> {
            let value = raw.iter().find(|(k, _)| *k == name).map(|(_, v)| v).unwrap_or_default();
            validate::is_uuid(value).then(|| Uuid::parse_str(value).ok()).flatten().ok_or_else(|| FieldError {
                location: FieldLocation::Params,
                field: name.into(),
                message: "Invalid UUID".into(),
                code: "invalid_format".into(),
            })
        };
        match (get("id"), get("deliveryId")) {
            (Ok(d), Ok(x)) => Ok(DeliveryPath(d, x)),
            (a, b) => Err(AppError::validation([a.err(), b.err()].into_iter().flatten().collect())),
        }
    }
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// The kinds the caller may see deliveries of (§8).
fn kinds(ctx: &RequestContext) -> Vec<String> {
    let mut k = vec!["inbox".to_owned(), "email".to_owned()];
    if ctx.require(GlobalPermission::WebhooksManage).is_ok() {
        k.push("webhook".to_owned());
    }
    k
}

const FROM: &str = "cmdb.workflow_action_deliveries d
    JOIN cmdb.workflow_action_runs r ON r.id = d.run_id
    LEFT JOIN cmdb.workflow_actions a ON a.id = r.action_id
    LEFT JOIN cmdb.configuration_items ci ON ci.id = r.ci_id
    LEFT JOIN cmdb.users u ON u.id = d.user_id
    LEFT JOIN cmdb.webhook_endpoints e ON e.id = d.endpoint_id";
const COUNT_FROM: &str = "cmdb.workflow_action_deliveries d JOIN cmdb.workflow_action_runs r ON r.id = d.run_id";

fn columns() -> String {
    format!(
        "d.id, d.run_id, r.event_id, r.action_key, a.name AS action_name, r.kind, r.instance_id, r.ci_id,
         ci.class_id AS ci_class, ci.label AS ci_label, d.recipient_key, d.user_id, u.username, u.display_name,
         d.endpoint_id, e.name AS endpoint_name, {STATUS} AS status, d.status_reason, d.attempts, d.next_attempt_at,
         d.last_status_code, d.created_at, d.completed_at, d.retried_at"
    )
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    run_id: i64,
    event_id: i64,
    action_key: String,
    action_name: Option<String>,
    kind: WorkflowActionKind,
    instance_id: Uuid,
    ci_id: Uuid,
    ci_class: Option<Uuid>,
    ci_label: Option<String>,
    recipient_key: String,
    user_id: Option<Uuid>,
    username: Option<String>,
    display_name: Option<String>,
    endpoint_id: Option<Uuid>,
    endpoint_name: Option<String>,
    status: WorkflowActionDeliveryStatus,
    status_reason: Option<String>,
    attempts: i16,
    next_attempt_at: DateTime<Utc>,
    last_status_code: Option<i32>,
    created_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
    retried_at: Option<DateTime<Utc>>,
}

/// `c***@corp.example`: enough to tell lists apart, not to harvest them (N-Q3).
fn mask(address: &str) -> String {
    match address.split_once('@') {
        Some((local, domain)) => format!("{}***@{domain}", local.chars().next().unwrap_or('*')),
        None => "***".into(),
    }
}

fn recipient(r: &Row) -> WorkflowActionDeliveryRecipient {
    use WorkflowActionRecipientKind as K;
    let (kind, rest) = r.recipient_key.split_once(':').unwrap_or(("", ""));
    match kind {
        "addr" => WorkflowActionDeliveryRecipient {
            kind: K::Address,
            id: None,
            name: None,
            username: None,
            address: Some(mask(rest)),
        },
        "endpoint" => WorkflowActionDeliveryRecipient {
            kind: K::Endpoint,
            id: r.endpoint_id,
            name: r.endpoint_name.clone(),
            username: None,
            address: None,
        },
        _ => WorkflowActionDeliveryRecipient {
            kind: K::User,
            id: r.user_id,
            name: r.display_name.clone(),
            username: r.username.clone(),
            address: None,
        },
    }
}

fn delivery(ctx: &RequestContext, r: Row) -> WorkflowActionDelivery {
    let recipient = recipient(&r);
    let visible = r.ci_class.is_some_and(|c| ctx.may_view_all(&[c]));
    WorkflowActionDelivery {
        id: r.id,
        run_id: r.run_id,
        event_id: r.event_id,
        action_key: r.action_key,
        action_name: r.action_name,
        kind: r.kind,
        instance_id: r.instance_id,
        ci_id: r.ci_id,
        ci_label: r.ci_label.filter(|_| visible),
        recipient,
        status: r.status,
        status_reason: r.status_reason,
        attempts: r.attempts,
        next_attempt_at: (r.status == WorkflowActionDeliveryStatus::Pending).then_some(r.next_attempt_at),
        last_status_code: r.last_status_code,
        created_at: r.created_at,
        completed_at: r.completed_at,
        retried_at: r.retried_at,
    }
}

/// The deliveries of `definition` of kinds `kinds` that `f` selects.
fn push_filter(w: &mut Where<'_>, definition: Uuid, kinds: &[String], f: &WorkflowActionDeliveryFilter) {
    w.and().push("r.definition_id = ").push_bind(definition);
    w.and().push("r.kind = ANY(").push_bind(kinds.to_vec()).push("::text[])");
    if let Some(key) = &f.action_key {
        w.and().push("r.action_key = ").push_bind(key.clone());
    }
    if let Some(status) = f.status {
        w.and_sql(&format!("({})", status.sql()));
    }
    if let Some(kind) = f.kind {
        w.and().push("r.kind = ").push_bind(kind);
    }
    if let Some(from) = f.from {
        w.and().push("d.created_at >= ").push_bind(from);
    }
    if let Some(to) = f.to {
        w.and().push("d.created_at < ").push_bind(to);
    }
    if let Some(instance) = f.instance_id {
        w.and().push("r.instance_id = ").push_bind(instance);
    }
    if let Some(event) = f.event_id {
        w.and().push("r.event_id = ").push_bind(event);
    }
    if let Some(ci) = f.ci_id {
        w.and().push("r.ci_id = ").push_bind(ci);
    }
}

pub async fn list(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    q: &WorkflowActionDeliveryList,
) -> Result<Page<WorkflowActionDelivery>, AppError> {
    let mut conn = pool.acquire().await?;
    let d = service::load_for(&mut conn, ctx, id, false, service::Access::Read).await?;
    let kinds = kinds(ctx);
    let f = WorkflowActionDeliveryFilter::from(q);
    let filter = |w: &mut Where<'_>| push_filter(w, d.id, &kinds, &f);
    let column = match q.sort.field.as_str() {
        "completedAt" => "d.completed_at",
        "nextAttemptAt" => "d.next_attempt_at",
        "attempts" => "d.attempts",
        "status" => STATUS,
        "actionKey" => "r.action_key",
        _ => "d.created_at",
    };
    let dir = q.sort.dir();
    let order = format!("{column} {dir} NULLS LAST, d.created_at {dir}, d.id {dir}");
    let (rows, total) =
        crud::select_page_counted::<Row>(&mut conn, FROM, COUNT_FROM, &columns(), &filter, &order, q.limit, q.offset)
            .await?;
    Ok(Page { data: rows.into_iter().map(|r| delivery(ctx, r)).collect(), page: q.page_meta(total) })
}

async fn load_row(conn: &mut PgConnection, definition: Uuid, kinds: &[String], id: Uuid) -> Result<Row, AppError> {
    let mut qb = QueryBuilder::new(format!("SELECT {} FROM {FROM}", columns()));
    let mut w = Where::new(&mut qb);
    push_filter(&mut w, definition, kinds, &WorkflowActionDeliveryFilter::default());
    w.and().push("d.id = ").push_bind(id);
    qb.build_query_as::<Row>()
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| AppError::missing("Workflow action delivery", id))
}

pub async fn get(
    pool: &PgPool,
    ctx: &RequestContext,
    path: &DeliveryPath,
) -> Result<WorkflowActionDeliveryDetail, AppError> {
    let mut conn = pool.acquire().await?;
    let d = service::load_for(&mut conn, ctx, path.0, false, service::Access::Read).await?;
    let row = load_row(&mut conn, d.id, &kinds(ctx), path.1).await?;
    let more: (Option<String>, String, Option<String>) = sqlx::query_as(
        "SELECT d.last_error, r.status, r.status_reason FROM cmdb.workflow_action_deliveries d
         JOIN cmdb.workflow_action_runs r ON r.id = d.run_id WHERE d.id = $1",
    )
    .bind(row.id)
    .fetch_one(&mut *conn)
    .await?;
    let event: Option<WorkflowActionDeliveryEvent> = sqlx::query_as(
        "SELECT kind, transition_key, from_state_key, to_state_key, actor_name, occurred_at
         FROM cmdb.workflow_instance_events WHERE id = $1",
    )
    .bind(row.event_id)
    .fetch_optional(&mut *conn)
    .await?;
    let (last_error, run_status, run_status_reason) = more;
    Ok(WorkflowActionDeliveryDetail { delivery: delivery(ctx, row), last_error, run_status, run_status_reason, event })
}

// ---------------------------------------------------------------------------
// Retry and discard
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Retry,
    Discard,
}

impl Op {
    fn eligible(self) -> &'static str {
        match self {
            Op::Retry => RETRYABLE,
            Op::Discard => DISCARDABLE,
        }
    }

    fn refusal(self) -> WorkflowActionDeliveryRefusal {
        match self {
            Op::Retry => WorkflowActionDeliveryRefusal::NotRetryable,
            Op::Discard => WorkflowActionDeliveryRefusal::NotDiscardable,
        }
    }

    fn set(self) -> &'static str {
        match self {
            Op::Retry => {
                "status = 'pending', status_reason = NULL, attempts = 0, next_attempt_at = now(), completed_at = NULL,
                 retried_at = now(), lease_owner = NULL, lease_until = NULL"
            }
            Op::Discard => {
                "status = 'dead', status_reason = 'discarded', completed_at = now(), lease_owner = NULL,
                 lease_until = NULL"
            }
        }
    }

    fn audit(self) -> AuditAction {
        match self {
            Op::Retry => AuditAction::WorkflowActionRetry,
            Op::Discard => AuditAction::WorkflowActionDiscard,
        }
    }

    fn after(self) -> WorkflowActionDeliveryStatus {
        match self {
            Op::Retry => WorkflowActionDeliveryStatus::Pending,
            Op::Discard => WorkflowActionDeliveryStatus::Discarded,
        }
    }
}

/// A delivery as one change found it.
#[derive(sqlx::FromRow)]
struct Changed {
    id: Uuid,
    status: WorkflowActionDeliveryStatus,
    status_reason: Option<String>,
    attempts: i16,
    run_id: i64,
    action_key: String,
    kind: WorkflowActionKind,
    ci_id: Uuid,
}

/// Applies `op` to those of `ids` it applies to, locking them first so a
/// worker's outcome and this change never interleave, and audits each.
async fn apply(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    definition: Uuid,
    kinds: &[String],
    op: Op,
    ids: &[Uuid],
    bulk: bool,
) -> Result<Vec<Uuid>, AppError> {
    let sql = format!(
        "WITH target AS (
           SELECT d.id, {STATUS} AS status, d.status_reason, d.attempts, r.id AS run_id, r.action_key, r.kind, r.ci_id
           FROM cmdb.workflow_action_deliveries d JOIN cmdb.workflow_action_runs r ON r.id = d.run_id
           WHERE d.id = ANY($1) AND r.definition_id = $2 AND r.kind = ANY($3::text[]) AND {}
           ORDER BY d.id FOR UPDATE OF d)
         UPDATE cmdb.workflow_action_deliveries d SET {}
         FROM target t WHERE d.id = t.id
         RETURNING t.id, t.status, t.status_reason, t.attempts, t.run_id, t.action_key, t.kind, t.ci_id",
        op.eligible(),
        op.set()
    );
    let mut changed: Vec<Changed> =
        sqlx::query_as(sqlx::AssertSqlSafe(sql)).bind(ids).bind(definition).bind(kinds).fetch_all(&mut *conn).await?;
    changed.sort_by_key(|c| c.id);
    let after = op.after();
    let entries = changed
        .iter()
        .map(|c| AuditEntry {
            action: op.audit(),
            entity_type: "workflow_action_deliveries",
            entity_id: c.id,
            // An event (0073): old_value stays empty; before and after are both in new_value.
            old_value: None,
            new_value: Some(json!({
                "before": { "status": c.status, "statusReason": c.status_reason, "attempts": c.attempts },
                "after": { "status": after },
                "runId": c.run_id, "actionKey": c.action_key, "kind": c.kind, "ciId": c.ci_id, "bulk": bulk,
            })),
        })
        .collect();
    crud::write_audit(conn, ctx, entries).await?;
    Ok(changed.into_iter().map(|c| c.id).collect())
}

/// Retries or discards one delivery; 409 CONFLICT when its status does not allow it.
pub async fn change_one(
    pool: &PgPool,
    ctx: &RequestContext,
    path: &DeliveryPath,
    op: Op,
) -> Result<WorkflowActionDelivery, AppError> {
    let mut tx = pool.begin().await?;
    let d = service::load_for(&mut tx, ctx, path.0, false, service::Access::Read).await?;
    let kinds = kinds(ctx);
    let before = load_row(&mut tx, d.id, &kinds, path.1).await?;
    if apply(&mut tx, ctx, d.id, &kinds, op, &[path.1], false).await?.is_empty() {
        let (code, message) = match op {
            Op::Retry if before.kind == WorkflowActionKind::Inbox => (
                "not_retryable",
                "In-app deliveries are written when the run fans out; there is nothing to send again".to_owned(),
            ),
            Op::Retry => (
                "not_retryable",
                format!("Only dead and held deliveries can be retried; this one is {}", status_str(before.status)),
            ),
            Op::Discard => (
                "not_discardable",
                format!(
                    "Only pending, held and dead deliveries can be discarded; this one is {}",
                    status_str(before.status)
                ),
            ),
        };
        return Err(AppError::new(ErrorCode::Conflict, message.clone()).with_details(vec![FieldError {
            location: FieldLocation::Params,
            field: "deliveryId".into(),
            message,
            code: code.into(),
        }]));
    }
    let after = load_row(&mut tx, d.id, &kinds, path.1).await?;
    tx.commit().await?;
    Ok(delivery(ctx, after))
}

fn status_str(s: WorkflowActionDeliveryStatus) -> String {
    serde_json::to_value(s).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
}

/// Retries or discards the deliveries `b` names or selects.
pub async fn change_many(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    b: &WorkflowActionDeliveryBulk,
    op: Op,
) -> Result<WorkflowActionDeliveryBulkResult, AppError> {
    let mut tx = pool.begin().await?;
    let d = service::load_for(&mut tx, ctx, id, false, service::Access::Read).await?;
    let kinds = kinds(ctx);
    let (ids, more) = match (&b.ids, &b.filter) {
        (Some(ids), _) => {
            let mut ids = ids.clone();
            ids.sort();
            ids.dedup();
            (ids, false)
        }
        (None, Some(f)) => {
            let mut qb = QueryBuilder::new(format!("SELECT d.id FROM {COUNT_FROM}"));
            let mut w = Where::new(&mut qb);
            push_filter(&mut w, d.id, &kinds, f);
            w.and_sql(op.eligible());
            qb.push(" ORDER BY d.created_at, d.id LIMIT ").push_bind(MAX_BULK as i64 + 1);
            let mut ids: Vec<Uuid> = qb.build_query_scalar().fetch_all(&mut *tx).await?;
            let more = ids.len() > MAX_BULK;
            ids.truncate(MAX_BULK);
            (ids, more)
        }
        (None, None) => (Vec::new(), false),
    };
    let changed = apply(&mut tx, ctx, d.id, &kinds, op, &ids, true).await?;
    let mut refused = Vec::new();
    if b.ids.is_some() && changed.len() < ids.len() {
        let rest: Vec<Uuid> = ids.iter().copied().filter(|i| changed.binary_search(i).is_err()).collect();
        let mut qb = QueryBuilder::new(format!("SELECT d.id, {STATUS} AS status FROM {COUNT_FROM}"));
        let mut w = Where::new(&mut qb);
        push_filter(&mut w, d.id, &kinds, &WorkflowActionDeliveryFilter::default());
        w.and().push("d.id = ANY(").push_bind(rest.clone()).push(")");
        let found: Vec<(Uuid, WorkflowActionDeliveryStatus)> = qb.build_query_as().fetch_all(&mut *tx).await?;
        for i in rest {
            let status = found.iter().find(|(f, _)| *f == i).map(|(_, s)| *s);
            let reason = if status.is_some() { op.refusal() } else { WorkflowActionDeliveryRefusal::NotFound };
            refused.push(WorkflowActionDeliveryRefused { id: i, reason, status });
        }
    }
    tx.commit().await?;
    Ok(WorkflowActionDeliveryBulkResult {
        changed: i64::try_from(changed.len()).unwrap_or(i64::MAX),
        ids: changed,
        refused,
        more,
    })
}

// ---------------------------------------------------------------------------
// Summary
// ---------------------------------------------------------------------------

pub async fn summary(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    cfg: &WorkflowActionsConfig,
) -> Result<WorkflowActionsSummary, AppError> {
    let mut conn = pool.acquire().await?;
    let d = service::load_for(&mut conn, ctx, id, false, service::Access::Read).await?;
    let kinds = kinds(ctx);
    let configured: Vec<(String, String, WorkflowActionKind, bool)> = sqlx::query_as(
        "SELECT key, name, kind, enabled FROM cmdb.workflow_actions
         WHERE definition_id = $1 AND kind = ANY($2::text[]) ORDER BY position, key",
    )
    .bind(d.id)
    .bind(&kinds)
    .fetch_all(&mut *conn)
    .await?;
    // A delivery is written moments after its run: runs of the last 8 days cover deliveries of the last 7.
    let counts: Vec<(String, WorkflowActionKind, WorkflowActionDeliveryStatus, i64, i64)> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT r.action_key, r.kind, {STATUS} AS status,
                    count(*) FILTER (WHERE d.created_at >= now() - interval '24 hours'), count(*)
             FROM cmdb.workflow_action_runs r JOIN cmdb.workflow_action_deliveries d ON d.run_id = r.id
             WHERE r.definition_id = $1 AND r.kind = ANY($2::text[]) AND r.created_at >= now() - interval '8 days'
               AND d.created_at >= now() - interval '7 days'
             GROUP BY 1, 2, 3"
        )))
        .bind(d.id)
        .bind(&kinds)
        .fetch_all(&mut *conn)
        .await?;
    let oldest: Vec<(String, DateTime<Utc>, i64)> = sqlx::query_as(
        "SELECT r.action_key, min(coalesce(d.retried_at, d.created_at)),
                extract(epoch FROM now() - min(coalesce(d.retried_at, d.created_at)))::bigint
         FROM cmdb.workflow_action_deliveries d JOIN cmdb.workflow_action_runs r ON r.id = d.run_id
         WHERE d.status = 'pending' AND r.definition_id = $1 AND r.kind = ANY($2::text[])
         GROUP BY 1",
    )
    .bind(d.id)
    .bind(&kinds)
    .fetch_all(&mut *conn)
    .await?;
    let suppressed: Vec<(String, WorkflowActionKind, i64)> = sqlx::query_as(
        "SELECT action_key, kind, count(*) FROM cmdb.workflow_action_runs
         WHERE definition_id = $1 AND kind = ANY($2::text[]) AND status = 'suppressed'
           AND created_at >= now() - interval '24 hours'
         GROUP BY 1, 2",
    )
    .bind(d.id)
    .bind(&kinds)
    .fetch_all(&mut *conn)
    .await?;

    let blank = |key: String, name: Option<String>, kind, enabled| WorkflowActionSummary {
        key,
        name,
        kind,
        enabled,
        last24h: WorkflowActionDeliveryCounts::default(),
        last7d: WorkflowActionDeliveryCounts::default(),
        suppressed24h: 0,
        oldest_pending_at: None,
        oldest_pending_age_seconds: None,
    };
    let mut actions: Vec<WorkflowActionSummary> =
        configured.into_iter().map(|(key, name, kind, enabled)| blank(key, Some(name), kind, enabled)).collect();
    let mut gone: Vec<(String, WorkflowActionKind)> = counts
        .iter()
        .map(|(k, kind, ..)| (k.clone(), *kind))
        .chain(suppressed.iter().map(|(k, kind, _)| (k.clone(), *kind)))
        .filter(|(k, _)| !actions.iter().any(|a| a.key == *k))
        .collect();
    gone.sort();
    gone.dedup_by(|a, b| a.0 == b.0);
    actions.extend(gone.into_iter().map(|(key, kind)| blank(key, None, kind, false)));
    for (key, _, status, day, week) in counts {
        if let Some(a) = actions.iter_mut().find(|a| a.key == key) {
            a.last24h.add(status, day);
            a.last7d.add(status, week);
        }
    }
    for (key, at, age) in oldest {
        if let Some(a) = actions.iter_mut().find(|a| a.key == key) {
            a.oldest_pending_at = Some(at);
            a.oldest_pending_age_seconds = Some(age.max(0));
        }
    }
    for (key, _, n) in suppressed {
        if let Some(a) = actions.iter_mut().find(|a| a.key == key) {
            a.suppressed24h += n;
        }
    }

    let (overloaded, backlog, checked_at, max_per_instance_per_hour): (bool, i32, DateTime<Utc>, i32) = sqlx::query_as(
        "SELECT overloaded, backlog, checked_at, max_per_instance_per_hour FROM cmdb.workflow_action_queue_state",
    )
    .fetch_one(&mut *conn)
    .await?;
    Ok(WorkflowActionsSummary {
        actions,
        queue: WorkflowActionQueue {
            overloaded,
            backlog: i64::from(backlog),
            checked_at,
            queue_max: cfg.queue_max,
            max_per_instance_per_hour,
            max_attempts: cfg.max_attempts,
            max_age_hours: cfg.max_age_hours,
        },
    })
}
