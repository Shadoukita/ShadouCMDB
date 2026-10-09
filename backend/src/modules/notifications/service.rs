//! Notifications: reading, marking read, dismissing and retention (SHAA-2356).
//!
//! The rows are written by the triggers of migration 0072, in the transaction
//! of the event they report; this module never creates one. A user sees only
//! their own: anyone else's is the same `404` as one that does not exist,
//! administrators included (GDPR data minimisation). A notification about a CI
//! is shown only while the caller may view the CI's class, judged when read,
//! so losing access to a class hides its CI labels at once; nothing is
//! deleted. Marking read and dismissing change only the caller's own inbox and
//! are not audited, like saved-view defaults (SHAA-578 §3.4).

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::api::context::RequestContext;
use crate::api::route::Check;
use crate::api::schemas::{Page, Paged};
use crate::auth::permissions::ClassOp;
use crate::config::NotificationConfig;
use crate::http::error::AppError;

const ENTITY: &str = "notification";

/// Most notifications kept per user; the retention sweep deletes older ones.
pub const MAX_PER_USER: i64 = 500;

// ---------------------------------------------------------------------------
// API shapes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = NotificationKind)]
pub enum Kind {
    /// A step of an approval request the recipient may decide became active
    ApprovalRequested,
    /// The recipient's approval request closed (approved, rejected or closed with the instance or CI)
    ApprovalClosed,
    /// Someone else ran a transition on, cancelled or forced a workflow instance the recipient started
    WorkflowTransition,
    /// The recipient's import ended (completed, completed with errors, or failed)
    ImportFinished,
    /// A workflow's notification action names the recipient (configured per workflow)
    WorkflowAction,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::ApprovalRequested => "approval_requested",
            Kind::ApprovalClosed => "approval_closed",
            Kind::WorkflowTransition => "workflow_transition",
            Kind::ImportFinished => "import_finished",
            Kind::WorkflowAction => "workflow_action",
        }
    }
}

/// What a notification opens
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
#[schema(as = NotificationEntityType)]
pub enum EntityType {
    WorkflowApprovalRequests,
    WorkflowInstances,
    ImportJobs,
}

/// One notification of the caller
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Notification {
    pub id: Uuid,
    pub kind: Kind,
    /// The record to open: an approval request, a workflow instance or an import job. It may be gone by now (an
    /// import record past its retention); the client then says so.
    pub entity_type: EntityType,
    pub entity_id: Uuid,
    /// The CI it is about; null when it is about no CI (imports)
    pub ci_id: Option<Uuid>,
    /// Display values as they were at the event, by kind. Approvals: `instanceId`, `ciId`, `ciLabel`, `ciIdent`,
    /// `definitionName`, `transitionKey`, `transitionName`, `requestNo`; `approval_requested` also `stepKey`,
    /// `stepName`, `dueAt`, `requestedByName`; `approval_closed` also `status`, `closeReason`, `closedByName`.
    /// `workflow_transition`: `instanceId`, `ciId`, `ciLabel`, `ciIdent`, `definitionName`, `event` (`transition`,
    /// `cancel`, `force`), `transitionKey`, `transitionName`, `fromStateKey`, `fromStateName`, `toStateKey`,
    /// `toStateName`, `actorName`. `import_finished`: `fileName`, `classKey`, `status`, `errorCode`. `workflow_action`: those of
    /// `workflow_transition` plus `actionKey`, `actionName`, `approvalRequestId` and `requestNo` (`event` is the
    /// workflow event's kind: `transition`, `approval_request`, `approval_decision`, `approval_close`,
    /// `approval_overdue`, `cancel`, `force`, ...). Any may be null.
    #[schema(value_type = Object)]
    pub data: Value,
    pub created_at: DateTime<Utc>,
    /// Null while unread
    pub read_at: Option<DateTime<Utc>>,
}

/// `GET /notifications` filters.
#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ListNotificationsQuery {
    /// Page size (1-100)
    #[param(required = false, default = 20, minimum = 1, maximum = 100)]
    pub limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000)]
    pub offset: i64,
    /// `true`: only unread ones; `false`: only read ones
    #[param(required = false, schema_with = bool_schema)]
    pub unread: Option<crate::api::schemas::QueryBool>,
    /// Only this kind
    #[param(inline)]
    pub kind: Option<Kind>,
}
crate::paged!(ListNotificationsQuery);

fn bool_schema() -> utoipa::openapi::schema::Schema {
    utoipa::openapi::schema::ObjectBuilder::new()
        .schema_type(utoipa::openapi::schema::Type::String)
        .enum_values(Some(["true", "false"]))
        .into()
}

/// The bell
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UnreadCount {
    /// Unread notifications the caller may see
    pub unread: i64,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateNotification {
    /// `true` marks it read (keeping the first time), `false` unread again
    pub read: bool,
}

impl Check for UpdateNotification {}

#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarkNotificationsRead {
    /// Only notifications created at or before this time: the newest one the user saw, so one that arrived since
    /// stays unread. Omitted: all of them.
    pub up_to: Option<DateTime<Utc>>,
}

impl Check for MarkNotificationsRead {}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MarkedRead {
    /// Notifications that were unread and are now read
    pub updated: i64,
}

// ---------------------------------------------------------------------------
// Reading and marking
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    kind: String,
    entity_type: EntityType,
    entity_id: Uuid,
    ci_id: Option<Uuid>,
    data: Value,
    created_at: DateTime<Utc>,
    read_at: Option<DateTime<Utc>>,
}

impl Row {
    fn dto(self) -> Result<Notification, AppError> {
        let kind = serde_json::from_value(Value::String(self.kind)).map_err(|_| AppError::internal())?;
        Ok(Notification {
            id: self.id,
            kind,
            entity_type: self.entity_type,
            entity_id: self.entity_id,
            ci_id: self.ci_id,
            data: self.data,
            created_at: self.created_at,
            read_at: self.read_at,
        })
    }
}

const COLUMNS: &str = "n.id, n.kind, n.entity_type, n.entity_id, n.ci_id, n.data, n.created_at, n.read_at";

/// The caller's own rows they may see: `$1` the user, `$2` the classes they
/// may view (NULL: every class). A CI-less notification is always visible.
const VISIBLE: &str = "n.user_id = $1
    AND ($2::uuid[] IS NULL OR n.ci_id IS NULL
         OR EXISTS (SELECT 1 FROM cmdb.configuration_items ci WHERE ci.id = n.ci_id AND ci.class_id = ANY ($2)))";

/// The signed-in user and the classes they may view; routes are session only.
fn reader(ctx: &RequestContext) -> Result<(Uuid, Option<Vec<Uuid>>), AppError> {
    let user = ctx.principal().map(|p| p.user_id).ok_or_else(crate::api::context::unauthenticated)?;
    Ok((user, ctx.class_scope(ClassOp::View)))
}

pub async fn list(
    pool: &PgPool,
    ctx: &RequestContext,
    q: &ListNotificationsQuery,
) -> Result<Page<Notification>, AppError> {
    let (user, scope) = reader(ctx)?;
    let unread = q.unread.map(bool::from);
    let kind = q.kind.map(Kind::as_str);
    let filter = format!(
        "{VISIBLE} AND ($3::boolean IS NULL OR (n.read_at IS NULL) = $3) AND ($4::text IS NULL OR n.kind = $4)"
    );
    let mut conn = pool.acquire().await?;
    let total: i64 =
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM cmdb.notifications n WHERE {filter}")))
            .bind(user)
            .bind(&scope)
            .bind(unread)
            .bind(kind)
            .fetch_one(&mut *conn)
            .await?;
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM cmdb.notifications n WHERE {filter}
         ORDER BY n.created_at DESC, n.id LIMIT $5 OFFSET $6"
    )))
    .bind(user)
    .bind(&scope)
    .bind(unread)
    .bind(kind)
    .bind(q.limit)
    .bind(q.offset)
    .fetch_all(&mut *conn)
    .await?;
    Ok(Page { data: rows.into_iter().map(Row::dto).collect::<Result<_, _>>()?, page: q.page_meta(total) })
}

pub async fn unread_count(pool: &PgPool, ctx: &RequestContext) -> Result<UnreadCount, AppError> {
    let (user, scope) = reader(ctx)?;
    let unread: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT count(*) FROM cmdb.notifications n WHERE {VISIBLE} AND n.read_at IS NULL"
    )))
    .bind(user)
    .bind(&scope)
    .fetch_one(pool)
    .await?;
    Ok(UnreadCount { unread })
}

pub async fn update(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    b: &UpdateNotification,
) -> Result<Notification, AppError> {
    let (user, scope) = reader(ctx)?;
    // Marking read keeps the first time; marking unread clears it.
    let row: Option<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "UPDATE cmdb.notifications n
            SET read_at = CASE WHEN $4 THEN coalesce(n.read_at, now()) END
          WHERE n.id = $3 AND {VISIBLE}
         RETURNING {COLUMNS}"
    )))
    .bind(user)
    .bind(&scope)
    .bind(id)
    .bind(b.read)
    .fetch_optional(pool)
    .await?;
    row.ok_or_else(|| AppError::missing(ENTITY, id))?.dto()
}

pub async fn mark_read(pool: &PgPool, ctx: &RequestContext, b: &MarkNotificationsRead) -> Result<MarkedRead, AppError> {
    let (user, scope) = reader(ctx)?;
    let updated = sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE cmdb.notifications n SET read_at = now()
          WHERE {VISIBLE} AND n.read_at IS NULL AND ($3::timestamptz IS NULL OR n.created_at <= $3)"
    )))
    .bind(user)
    .bind(&scope)
    .bind(b.up_to)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(MarkedRead { updated: i64::try_from(updated).unwrap_or(i64::MAX) })
}

pub async fn delete(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<(), AppError> {
    let (user, scope) = reader(ctx)?;
    let n = sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM cmdb.notifications n WHERE n.id = $3 AND {VISIBLE}")))
        .bind(user)
        .bind(&scope)
        .bind(id)
        .execute(pool)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(AppError::missing(ENTITY, id));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Retention
// ---------------------------------------------------------------------------

/// What one sweep deleted.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Swept {
    /// Older than the retention period.
    pub expired: u64,
    /// Beyond the newest [`MAX_PER_USER`] of a user.
    pub over_limit: u64,
}

/// Deletes notifications older than `retention_days` (judged at the
/// database's clock) and all but each user's newest [`MAX_PER_USER`].
/// Idempotent, so every replica may run it.
pub async fn sweep(conn: &mut PgConnection, retention_days: i32) -> sqlx::Result<Swept> {
    let expired = sqlx::query("DELETE FROM cmdb.notifications WHERE created_at < now() - make_interval(days => $1)")
        .bind(retention_days)
        .execute(&mut *conn)
        .await?
        .rows_affected();
    let over_limit = sqlx::query(
        "DELETE FROM cmdb.notifications d USING (
           SELECT id FROM (
             SELECT id, row_number() OVER (PARTITION BY user_id ORDER BY created_at DESC, id) AS rn
               FROM cmdb.notifications
              WHERE user_id IN (SELECT user_id FROM cmdb.notifications GROUP BY user_id HAVING count(*) > $1)
           ) ranked WHERE rn > $1
         ) old WHERE d.id = old.id",
    )
    .bind(MAX_PER_USER)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    Ok(Swept { expired, over_limit })
}

const SWEEP_EVERY: Duration = Duration::from_secs(60 * 60);

/// The retention task of this server process.
pub struct Retention {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl Retention {
    pub fn spawn(pool: PgPool, cfg: NotificationConfig) -> Self {
        let (stop, rx) = watch::channel(false);
        Retention { stop, task: tokio::spawn(retention_loop(pool, cfg, rx)) }
    }

    pub async fn stop(self) {
        let _ = self.stop.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(10), self.task).await;
    }
}

async fn retention_loop(pool: PgPool, cfg: NotificationConfig, mut stop: watch::Receiver<bool>) {
    // Soon after start, then every hour.
    let mut wait = Duration::from_secs(60);
    loop {
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = stop.changed() => return,
        }
        wait = SWEEP_EVERY;
        let swept = async {
            let mut conn = pool.acquire().await?;
            sweep(&mut conn, cfg.retention_days).await
        }
        .await;
        match swept {
            Ok(s) if s != Swept::default() => {
                tracing::info!(expired = s.expired, over_limit = s.over_limit, "notification retention")
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "notification retention failed; retried in an hour"),
        }
    }
}
