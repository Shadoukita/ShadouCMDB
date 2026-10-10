//! Approval delegations (approvals design SHAA-1869 §6.1, §10.2; slice A4,
//! SHAA-2643).
//!
//! A user lends their own approvals to a deputy for a window of at most 90
//! days, for every workflow or one. An administrator (`users.manage`) can do
//! it for someone who is absent, but never to themselves (SHAA-1872 C2; the
//! table's `not_creator` check backs it). Rows are never deleted: revoked, so
//! "who acted for whom" stays answerable.
//!
//! A delegation **qualifies** a decision when, at the database's clock, it is
//! in its window, not revoked, both accounts exist and are active, and it is
//! unscoped or scoped to the request's workflow. It lends only the
//! principal's own eligibility (no chaining) and never lends visibility: the
//! delegate must view the CI's type, and so must the principal.
//!
//! Delegation writes take no runtime lock (§9). The principal's user row is
//! locked while a delegation is made, so two at once cannot pass the cap of
//! [`MAX_ACTIVE_DELEGATIONS`].

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::super::approval_schemas::*;
use crate::api::context::RequestContext;
use crate::api::schemas::{Page, Paged};
use crate::auth::permissions::ClassOp;
use crate::data::crud::{self, AuditAction, AuditEntry};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::modules::business_services::service::may_browse_directory;

const ENTITY: &str = "Approval delegation";
const ENTITY_TYPE: &str = "workflow_approval_delegations";

/// A delegation that lets `delegate` decide for its principal now.
#[derive(Debug, Clone, sqlx::FromRow)]
pub(super) struct Live {
    pub(super) delegation_id: Uuid,
    pub(super) principal_id: Uuid,
    pub(super) principal_name: String,
    /// None: every workflow.
    pub(super) definition_id: Option<Uuid>,
}

/// The delegations that qualify `delegate` now: for workflow `definition`
/// only when given, else every one (the inbox filters by workflow itself).
/// Ordered by principal; for one principal and workflow the oldest comes first.
pub(super) async fn live(
    conn: &mut PgConnection,
    delegate: Uuid,
    definition: Option<Uuid>,
) -> Result<Vec<Live>, AppError> {
    Ok(sqlx::query_as(
        "SELECT d.id AS delegation_id, d.principal_id, pu.username AS principal_name, d.definition_id
         FROM cmdb.workflow_approval_delegations d
         JOIN cmdb.users pu ON pu.id = d.principal_id AND pu.is_active
         JOIN cmdb.users du ON du.id = d.delegate_id AND du.is_active
         WHERE d.delegate_id = $1 AND d.revoked_at IS NULL AND d.starts_at <= now() AND now() < d.ends_at
           AND ($2::uuid IS NULL OR d.definition_id IS NULL OR d.definition_id = $2)
         ORDER BY pu.username, d.principal_id, d.definition_id NULLS LAST, d.created_at, d.id",
    )
    .bind(delegate)
    .bind(definition)
    .fetch_all(conn)
    .await?)
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

const COLUMNS: &str = "d.id, d.principal_id, d.principal_name, d.delegate_id, d.delegate_name, d.definition_id, \
     wd.key AS definition_key, wd.name AS definition_name, wd.class_id AS definition_class_id, d.starts_at, \
     d.ends_at, d.reason, d.created_at, d.created_by_id, d.created_by_name, d.revoked_at, d.revoked_by_name, \
     CASE WHEN d.revoked_at IS NOT NULL THEN 'revoked' WHEN now() < d.starts_at THEN 'scheduled' \
          WHEN now() >= d.ends_at THEN 'ended' ELSE 'active' END AS status";

const FROM: &str = "cmdb.workflow_approval_delegations d \
     LEFT JOIN cmdb.workflow_definitions wd ON wd.id = d.definition_id";

#[derive(Debug, sqlx::FromRow)]
struct Row {
    id: Uuid,
    principal_id: Option<Uuid>,
    principal_name: String,
    delegate_id: Option<Uuid>,
    delegate_name: String,
    definition_id: Option<Uuid>,
    definition_key: Option<String>,
    definition_name: Option<String>,
    definition_class_id: Option<Uuid>,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    reason: Option<String>,
    created_at: DateTime<Utc>,
    created_by_id: Option<Uuid>,
    created_by_name: String,
    revoked_at: Option<DateTime<Utc>>,
    revoked_by_name: Option<String>,
    status: String,
}

fn status(s: &str) -> WorkflowApprovalDelegationStatus {
    match s {
        "scheduled" => WorkflowApprovalDelegationStatus::Scheduled,
        "active" => WorkflowApprovalDelegationStatus::Active,
        "ended" => WorkflowApprovalDelegationStatus::Ended,
        _ => WorkflowApprovalDelegationStatus::Revoked,
    }
}

impl Row {
    /// As `ctx` may see it: the workflow is named only to a caller who may
    /// view its type (§8, the `redact` rule for delegation rows).
    fn dto(self, ctx: &RequestContext) -> WorkflowApprovalDelegation {
        let shown = self.definition_class_id.is_some_and(|c| may_view(ctx, c));
        WorkflowApprovalDelegation {
            id: self.id,
            principal: WorkflowApprovalDelegationUser { id: self.principal_id, name: self.principal_name },
            delegate: WorkflowApprovalDelegationUser { id: self.delegate_id, name: self.delegate_name },
            scoped: self.definition_id.is_some(),
            definition_key: self.definition_key.filter(|_| shown),
            definition_name: self.definition_name.filter(|_| shown),
            starts_at: self.starts_at,
            ends_at: self.ends_at,
            reason: self.reason,
            status: status(&self.status),
            created_at: self.created_at,
            created_by: WorkflowApprovalDelegationUser { id: self.created_by_id, name: self.created_by_name },
            revoked_at: self.revoked_at,
            revoked_by_name: self.revoked_by_name,
        }
    }

    /// The audit value: the whole delegation, users by name.
    fn audit(&self) -> Value {
        let definition = self.definition_id.map(|id| {
            json!({ "id": id, "key": self.definition_key, "name": self.definition_name,
                    "classId": self.definition_class_id })
        });
        json!({
            "principal": { "id": self.principal_id, "name": self.principal_name },
            "delegate": { "id": self.delegate_id, "name": self.delegate_name },
            "definition": definition,
            "startsAt": self.starts_at, "endsAt": self.ends_at, "reason": self.reason,
            "createdBy": { "id": self.created_by_id, "name": self.created_by_name },
            "revokedAt": self.revoked_at, "revokedByName": self.revoked_by_name,
        })
    }
}

fn may_view(ctx: &RequestContext, class: Uuid) -> bool {
    ctx.class_scope(ClassOp::View).is_none_or(|s| s.contains(&class))
}

async fn by_id(conn: &mut PgConnection, id: Uuid, lock: bool) -> Result<Option<Row>, AppError> {
    let suffix = if lock { " FOR UPDATE OF d" } else { "" };
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {COLUMNS} FROM {FROM} WHERE d.id = $1{suffix}")))
        .bind(id)
        .fetch_optional(conn)
        .await?)
}

fn invalid(field: &str, code: &str, message: impl Into<String>) -> AppError {
    AppError::validation(vec![FieldError {
        location: FieldLocation::Body,
        field: field.into(),
        message: message.into(),
        code: code.into(),
    }])
}

// ---------------------------------------------------------------------------
// Create
// ---------------------------------------------------------------------------

/// The fields both create bodies share.
struct New<'a> {
    principal: Uuid,
    delegate: Uuid,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    definition_key: Option<&'a str>,
    reason: Option<&'a str>,
    /// Made by an administrator for someone else: the principal field is
    /// `principalUserId`, and the creator may not be the delegate.
    admin: bool,
}

async fn create(pool: &PgPool, ctx: &RequestContext, n: New<'_>) -> Result<WorkflowApprovalDelegation, AppError> {
    let me = ctx.principal().ok_or_else(crate::api::context::unauthenticated)?;
    let principal_field = if n.admin { "principalUserId" } else { "delegateUserId" };
    let mut tx = pool.begin().await?;
    // Locked: the cap below holds against a concurrent create for the same principal.
    let principal: Option<(String, bool)> =
        sqlx::query_as("SELECT username, is_active FROM cmdb.users WHERE id = $1 FOR UPDATE")
            .bind(n.principal)
            .fetch_optional(&mut *tx)
            .await?;
    let principal_name = match principal {
        Some((name, true)) => name,
        Some(_) => return Err(invalid(principal_field, "inactive", "The principal's account is disabled")),
        None => return Err(invalid(principal_field, "unknown", "No such user")),
    };
    if n.delegate == n.principal {
        return Err(invalid("delegateUserId", "self", "A user cannot delegate approvals to themselves"));
    }
    // SHAA-1872 C2: an administrator never turns "approved by P" into "approved by an administrator".
    if n.admin && n.delegate == me.user_id {
        return Err(invalid(
            "delegateUserId",
            "creator",
            "You cannot delegate someone else's approvals to yourself; name another delegate",
        ));
    }
    let delegate: Option<(String, bool)> = sqlx::query_as("SELECT username, is_active FROM cmdb.users WHERE id = $1")
        .bind(n.delegate)
        .fetch_optional(&mut *tx)
        .await?;
    let delegate_name = match delegate {
        Some((name, true)) => name,
        Some(_) => return Err(invalid("delegateUserId", "inactive", "The delegate's account is disabled")),
        None => return Err(invalid("delegateUserId", "unknown", "No such user")),
    };
    // A workflow on a type the caller may not view is answered as unknown, so
    // the key cannot be probed for (GH#818).
    let definition: Option<Uuid> = match n.definition_key {
        None => None,
        Some(key) => {
            let found: Option<(Uuid, Uuid)> =
                sqlx::query_as("SELECT id, class_id FROM cmdb.workflow_definitions WHERE lower(key) = lower($1)")
                    .bind(key)
                    .fetch_optional(&mut *tx)
                    .await?;
            match found {
                Some((id, class)) if may_view(ctx, class) => Some(id),
                _ => return Err(invalid("definitionKey", "unknown", format!("No workflow has the key {key}"))),
            }
        }
    };
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT now()").fetch_one(&mut *tx).await?;
    if n.ends_at <= now {
        return Err(invalid("endsAt", "in_past", "The delegation must end in the future"));
    }
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM cmdb.workflow_approval_delegations
         WHERE principal_id = $1 AND revoked_at IS NULL AND ends_at > now()",
    )
    .bind(n.principal)
    .fetch_one(&mut *tx)
    .await?;
    if active >= MAX_ACTIVE_DELEGATIONS {
        return Err(AppError::new(
            ErrorCode::Conflict,
            format!(
                "{principal_name} already has {MAX_ACTIVE_DELEGATIONS} delegations that are scheduled or active: \
                 revoke one first"
            ),
        )
        .with_details(vec![FieldError {
            location: FieldLocation::Body,
            field: principal_field.into(),
            message: format!("At most {MAX_ACTIVE_DELEGATIONS} delegations at once"),
            code: "limit".into(),
        }]));
    }
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO cmdb.workflow_approval_delegations
           (principal_id, principal_name, delegate_id, delegate_name, definition_id, starts_at, ends_at, reason,
            created_by_id, created_by_name)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
    )
    .bind(n.principal)
    .bind(&principal_name)
    .bind(n.delegate)
    .bind(&delegate_name)
    .bind(definition)
    .bind(n.starts_at)
    .bind(n.ends_at)
    .bind(n.reason.map(str::trim).filter(|r| !r.is_empty()))
    .bind(me.user_id)
    .bind(&me.username)
    .fetch_one(&mut *tx)
    .await?;
    let row = by_id(&mut tx, id, false).await?.ok_or_else(AppError::internal)?;
    let entry = AuditEntry {
        action: AuditAction::Create,
        entity_type: ENTITY_TYPE,
        entity_id: id,
        old_value: None,
        new_value: Some(row.audit()),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    tx.commit().await?;
    Ok(row.dto(ctx))
}

pub async fn create_mine(
    pool: &PgPool,
    ctx: &RequestContext,
    b: &WorkflowApprovalDelegationCreate,
) -> Result<WorkflowApprovalDelegation, AppError> {
    let me = ctx.principal().ok_or_else(crate::api::context::unauthenticated)?;
    let n = New {
        principal: me.user_id,
        delegate: b.delegate_user_id,
        starts_at: b.starts_at,
        ends_at: b.ends_at,
        definition_key: b.definition_key.as_deref(),
        reason: b.reason.as_deref(),
        admin: false,
    };
    create(pool, ctx, n).await
}

pub async fn create_for(
    pool: &PgPool,
    ctx: &RequestContext,
    b: &WorkflowApprovalDelegationAdminCreate,
) -> Result<WorkflowApprovalDelegation, AppError> {
    let me = ctx.principal().ok_or_else(crate::api::context::unauthenticated)?;
    let n = New {
        principal: b.principal_user_id,
        delegate: b.delegate_user_id,
        starts_at: b.starts_at,
        ends_at: b.ends_at,
        definition_key: b.definition_key.as_deref(),
        reason: b.reason.as_deref(),
        // An administrator delegating their own approvals is a self-service delegation.
        admin: b.principal_user_id != me.user_id,
    };
    create(pool, ctx, n).await
}

// ---------------------------------------------------------------------------
// Delegate picker
// ---------------------------------------------------------------------------

/// Exact-username lookups a caller without the directory right may make in
/// [`LOOKUP_WINDOW`]: enough for a picker, too few to guess a directory.
const MAX_EXACT_LOOKUPS: u32 = 30;
const LOOKUP_WINDOW: Duration = Duration::from_secs(60);

/// Per user: when their window started and the lookups in it. In-process, so
/// each server process counts on its own.
static EXACT_LOOKUPS: LazyLock<Mutex<HashMap<Uuid, (Instant, u32)>>> = LazyLock::new(Mutex::default);

/// Counts one exact-username lookup by `user`; 429 RATE_LIMITED past the cap.
fn admit_exact_lookup(user: Uuid) -> Result<(), AppError> {
    let now = Instant::now();
    let mut seen = EXACT_LOOKUPS.lock().map_err(|_| AppError::internal())?;
    if seen.len() > 10_000 {
        seen.retain(|_, (start, _)| now.duration_since(*start) < LOOKUP_WINDOW);
    }
    let (start, n) = seen.entry(user).or_insert((now, 0));
    if now.duration_since(*start) >= LOOKUP_WINDOW {
        (*start, *n) = (now, 0);
    }
    if *n >= MAX_EXACT_LOOKUPS {
        let wait = LOOKUP_WINDOW.saturating_sub(now.duration_since(*start)).as_secs().max(1);
        let mut err = AppError::new(
            ErrorCode::RateLimited,
            format!("Too many user lookups. Try again in {wait} s, or enter the exact username."),
        );
        err.retry_after = Some(wait);
        return Err(err);
    }
    *n += 1;
    Ok(())
}

/// Users the caller may delegate their approvals to: a search for a caller
/// who may look up users, else the one user with exactly that username, so
/// the picker is no way round `GET /principals` (GH#839). Disabled accounts
/// and the caller are never offered, and an exact miss looks the same
/// whichever of the three it was.
pub async fn candidates(
    pool: &PgPool,
    ctx: &RequestContext,
    q: &WorkflowApprovalDelegateQuery,
) -> Result<WorkflowApprovalDelegateCandidateList, AppError> {
    let me = ctx.principal().ok_or_else(crate::api::context::unauthenticated)?.user_id;
    let query_error = |code: &str, message: &str| {
        AppError::validation(vec![FieldError {
            location: FieldLocation::Query,
            field: "q".into(),
            message: message.into(),
            code: code.into(),
        }])
    };
    let text = q.q.as_deref().map(str::trim).ok_or_else(|| query_error("required", "Required"))?;
    match text.chars().count() {
        0..2 => return Err(query_error("too_small", "Too small: expected string to have >=2 characters")),
        n if n > MAX_DELEGATE_QUERY => {
            return Err(query_error(
                "too_big",
                &format!("Too big: expected string to have <={MAX_DELEGATE_QUERY} characters"),
            ));
        }
        _ => {}
    }
    let mut conn = pool.acquire().await?;
    if may_browse_directory(&mut conn, ctx).await? {
        let pattern = crate::api::schemas::like_pattern(text);
        let prefix = format!("{}%", crate::api::schemas::escape_like(&text.to_lowercase()));
        let data = sqlx::query_as(
            "SELECT id, username, display_name FROM cmdb.users
             WHERE is_active AND id <> $1 AND (display_name ILIKE $2 OR username ILIKE $2)
             ORDER BY (lower(display_name) LIKE $3 OR lower(username) LIKE $3) DESC, lower(display_name), id
             LIMIT $4",
        )
        .bind(me)
        .bind(pattern)
        .bind(prefix)
        .bind(MAX_DELEGATE_CANDIDATES)
        .fetch_all(&mut *conn)
        .await?;
        return Ok(WorkflowApprovalDelegateCandidateList { data, exact_match_only: false });
    }
    admit_exact_lookup(me)?;
    let data = sqlx::query_as(
        "SELECT id, username, display_name FROM cmdb.users
         WHERE lower(username) = lower($2) AND is_active AND id <> $1",
    )
    .bind(me)
    .bind(text)
    .fetch_all(&mut *conn)
    .await?;
    Ok(WorkflowApprovalDelegateCandidateList { data, exact_match_only: true })
}

// ---------------------------------------------------------------------------
// Revoke
// ---------------------------------------------------------------------------

/// Revokes delegation `id`: the principal or the delegate (declining) through
/// `/me`, anyone through `/admin` (`users.manage`, checked by the route).
pub async fn revoke(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    admin: bool,
) -> Result<WorkflowApprovalDelegation, AppError> {
    let me = ctx.principal().ok_or_else(crate::api::context::unauthenticated)?;
    let mut tx = pool.begin().await?;
    let before = by_id(&mut tx, id, true).await?.ok_or_else(|| AppError::missing(ENTITY, id))?;
    // Someone else's delegation does not exist for /me.
    if !admin && before.principal_id != Some(me.user_id) && before.delegate_id != Some(me.user_id) {
        return Err(AppError::missing(ENTITY, id));
    }
    match before.status.as_str() {
        "revoked" | "ended" => {
            return Err(AppError::new(
                ErrorCode::Conflict,
                format!("This delegation is already {}: there is nothing to revoke", before.status),
            )
            .with_details(vec![FieldError {
                location: FieldLocation::Params,
                field: "id".into(),
                message: before.status.clone(),
                code: before.status.clone(),
            }]));
        }
        _ => {}
    }
    sqlx::query("UPDATE cmdb.workflow_approval_delegations SET revoked_at = now(), revoked_by_name = $2 WHERE id = $1")
        .bind(id)
        .bind(&me.username)
        .execute(&mut *tx)
        .await?;
    let after = by_id(&mut tx, id, false).await?.ok_or_else(AppError::internal)?;
    let entry = AuditEntry {
        action: AuditAction::Update,
        entity_type: ENTITY_TYPE,
        entity_id: id,
        old_value: Some(before.audit()),
        new_value: Some(after.audit()),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    tx.commit().await?;
    Ok(after.dto(ctx))
}

// ---------------------------------------------------------------------------
// Lists
// ---------------------------------------------------------------------------

/// Newest window first.
async fn page(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    filter: &(dyn Fn(&mut sqlx::QueryBuilder<sqlx::Postgres>) + Sync),
    limit: i64,
    offset: i64,
) -> Result<(Vec<WorkflowApprovalDelegation>, i64), AppError> {
    let mut count = sqlx::QueryBuilder::new(format!("SELECT count(*) FROM {FROM} WHERE TRUE"));
    filter(&mut count);
    let total: i64 = count.build_query_scalar().fetch_one(&mut *conn).await?;
    let mut select = sqlx::QueryBuilder::new(format!("SELECT {COLUMNS} FROM {FROM} WHERE TRUE"));
    filter(&mut select);
    select.push(" ORDER BY d.starts_at DESC, d.id LIMIT ").push_bind(limit).push(" OFFSET ").push_bind(offset);
    let rows: Vec<Row> = select.build_query_as().fetch_all(&mut *conn).await?;
    Ok((rows.into_iter().map(|r| r.dto(ctx)).collect(), total))
}

fn push_active(qb: &mut sqlx::QueryBuilder<sqlx::Postgres>, active: Option<bool>) {
    match active {
        Some(true) => qb.push(" AND d.revoked_at IS NULL AND d.ends_at > now()"),
        Some(false) => qb.push(" AND (d.revoked_at IS NOT NULL OR d.ends_at <= now())"),
        None => qb,
    };
}

pub async fn list_mine(
    pool: &PgPool,
    ctx: &RequestContext,
    q: &WorkflowApprovalMyDelegationList,
) -> Result<Page<WorkflowApprovalDelegation>, AppError> {
    let me = ctx.principal().ok_or_else(crate::api::context::unauthenticated)?.user_id;
    let active = q.active.map(bool::from);
    let filter = |qb: &mut sqlx::QueryBuilder<sqlx::Postgres>| {
        match q.role {
            Some(WorkflowApprovalDelegationRole::Principal) => qb.push(" AND d.principal_id = ").push_bind(me),
            Some(WorkflowApprovalDelegationRole::Delegate) => qb.push(" AND d.delegate_id = ").push_bind(me),
            None => {
                qb.push(" AND (d.principal_id = ").push_bind(me).push(" OR d.delegate_id = ").push_bind(me).push(")")
            }
        };
        push_active(qb, active);
    };
    let mut conn = pool.acquire().await?;
    let (data, total) = page(&mut conn, ctx, &filter, q.limit, q.offset).await?;
    Ok(Page { data, page: q.page_meta(total) })
}

pub async fn list(
    pool: &PgPool,
    ctx: &RequestContext,
    q: &WorkflowApprovalDelegationList,
) -> Result<Page<WorkflowApprovalDelegation>, AppError> {
    let active = q.active.map(bool::from);
    let filter = |qb: &mut sqlx::QueryBuilder<sqlx::Postgres>| {
        if let Some(p) = q.principal {
            qb.push(" AND d.principal_id = ").push_bind(p);
        }
        if let Some(d) = q.delegate {
            qb.push(" AND d.delegate_id = ").push_bind(d);
        }
        push_active(qb, active);
    };
    let mut conn = pool.acquire().await?;
    let (data, total) = page(&mut conn, ctx, &filter, q.limit, q.offset).await?;
    Ok(Page { data, page: q.page_meta(total) })
}

/// The principals of `live` grouped: for each, None when one of their
/// delegations covers every workflow, else the workflows it covers.
pub(super) fn by_principal(live: &[Live]) -> Vec<(Uuid, Option<Vec<Uuid>>)> {
    let mut out: Vec<(Uuid, Option<Vec<Uuid>>)> = Vec::new();
    let mut at: HashMap<Uuid, usize> = HashMap::new();
    for l in live {
        let i = *at.entry(l.principal_id).or_insert_with(|| {
            out.push((l.principal_id, Some(Vec::new())));
            out.len() - 1
        });
        match (&mut out[i].1, l.definition_id) {
            (scope, None) => *scope = None,
            (Some(defs), Some(d)) => defs.push(d),
            (None, Some(_)) => {}
        }
    }
    out
}
