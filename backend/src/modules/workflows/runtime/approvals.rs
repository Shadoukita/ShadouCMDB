//! Run-time approvals (approvals design SHAA-1869 §4-§5, §8-§10; slice A3,
//! SHAA-1880).
//!
//! Running a transition that has an approval policy creates an **approval
//! request**: the fields sent are validated as for any transition and staged
//! in the request, nothing is written to the CI, and the instance stays in its
//! state. Each step needs `requiredApprovals` approvals from the users its
//! assignments resolve to; any rejection rejects the request (veto). The final
//! approval applies the transition in the deciding transaction.
//!
//! **Four-eyes** is always on and keyed by user account (§4.3): the requester
//! and, for a request made through a token someone else minted, that token's
//! creator are frozen in `excluded_user_ids` and may never decide it, with any
//! profile or credential. A step can also refuse approvers of an earlier step
//! (`distinctFromEarlier`) and the actors of other transitions on the instance
//! (`excludeActorsOf`, by user id: SHAA-1872 C4). A token decides only on a
//! step that allows tokens, and only a token its owner minted (C1).
//!
//! **Visibility.** Every endpoint goes through the instance's CI: a request
//! on a CI of a type the caller may not view does not exist (404). Deciding
//! needs no edit right: the final apply writes only the transition's fields
//! the request staged, through the item write path, with the decider as actor
//! and only the edit right on the CI's type (A-Q7, SHAA-1872).
//!
//! **Locks**: CI row → instance row → request row, as every runtime path.
//! Delegation, the SLA sweep and re-resolution after a staffing change are
//! slice A4; the lists are in `approval_lists`.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use sqlx::types::Json as SqlJson;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::super::approval_schemas::*;
use super::super::approvers::{self, Source};
use super::super::eval::{self, Subject};
use super::super::runtime_schemas::WorkflowBlockedReason;
use super::super::schemas::{WorkflowApprovalDroppedSource, WorkflowApproverRole, WorkflowApproverSource};
use super::{
    CANCEL_KEY, InstanceRow, NewEvent, Pinned, PinnedStep, PinnedTransition, as_transition_fields, check_active,
    condition_values, current_values, granted, insert_event, instance, is_set, may_edit, may_manage, move_to, pinned,
    starter, state_value, write_ci,
};
use crate::api::context::{Caller, RequestContext, forbidden};
use crate::auth::permissions::{ClassOp, ClassRights, Permissions};
use crate::auth::{Credential, Principal};
use crate::data::auth as auth_data;
use crate::data::crud::{self, AuditAction, AuditEntry};
use crate::data::items as item_data;
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::schema::model::Model;

const REQUEST: &str = "Approval request";

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

const REQUEST_COLUMNS: &str = "id, instance_id, version_id, transition_key, request_no, status, close_reason, \
     current_step_no, requested_at, requested_by_id, requested_by_name, excluded_user_ids, comment, staged_fields, \
     field_baseline, closed_at, closed_by_name, version";

#[derive(Debug, sqlx::FromRow)]
struct RequestRow {
    id: Uuid,
    instance_id: Uuid,
    version_id: Uuid,
    transition_key: String,
    request_no: i32,
    status: WorkflowApprovalStatus,
    close_reason: Option<WorkflowApprovalCloseReason>,
    current_step_no: i16,
    requested_at: DateTime<Utc>,
    requested_by_id: Option<Uuid>,
    requested_by_name: String,
    excluded_user_ids: Vec<Uuid>,
    comment: Option<String>,
    staged_fields: SqlJson<Map<String, Value>>,
    field_baseline: SqlJson<Map<String, Value>>,
    closed_at: Option<DateTime<Utc>>,
    closed_by_name: Option<String>,
    version: i32,
}

#[derive(Debug, sqlx::FromRow)]
struct StepState {
    step_no: i16,
    status: WorkflowApprovalStepStatus,
    activated_at: Option<DateTime<Utc>>,
    due_at: Option<DateTime<Utc>>,
    overdue_at: Option<DateTime<Utc>>,
    completed_at: Option<DateTime<Utc>>,
    eligible_count: Option<i32>,
    dropped_sources: SqlJson<Vec<WorkflowApprovalDroppedSource>>,
}

async fn load(conn: &mut PgConnection, id: Uuid, lock: bool) -> Result<Option<RequestRow>, AppError> {
    let suffix = if lock { " FOR UPDATE" } else { "" };
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {REQUEST_COLUMNS} FROM cmdb.workflow_approval_requests WHERE id = $1{suffix}"
    )))
    .bind(id)
    .fetch_optional(conn)
    .await?)
}

/// Locks a request for a change in the runtime order: its CI, its instance,
/// then the request. A request on a CI the caller may not view is missing.
async fn lock_request(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    id: Uuid,
) -> Result<(RequestRow, InstanceRow), AppError> {
    let found: Option<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "SELECT r.instance_id, wi.ci_id, ci.class_id FROM cmdb.workflow_approval_requests r
         JOIN cmdb.workflow_instances wi ON wi.id = r.instance_id
         JOIN cmdb.configuration_items ci ON ci.id = wi.ci_id WHERE r.id = $1",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    let (instance_id, ci_id, class_id) = found.ok_or_else(|| AppError::missing(REQUEST, id))?;
    ctx.require_class_visible(class_id, REQUEST, id)?;
    let locked = item_data::lock(conn, ci_id).await?.ok_or_else(|| AppError::missing(REQUEST, id))?;
    ctx.require_class_visible(locked.class_id, REQUEST, id)?;
    sqlx::query("SELECT 1 FROM cmdb.workflow_instances WHERE id = $1 FOR UPDATE")
        .bind(instance_id)
        .execute(&mut *conn)
        .await?;
    let req = load(conn, id, true).await?.ok_or_else(|| AppError::missing(REQUEST, id))?;
    let row = instance(conn, instance_id).await?.ok_or_else(|| AppError::missing(REQUEST, id))?;
    Ok((req, row))
}

fn detail(code: ErrorCode, field: &str, reason: &str, message: String) -> AppError {
    AppError::new(code, message.clone()).with_details(vec![FieldError {
        location: FieldLocation::Body,
        field: field.into(),
        message,
        code: reason.into(),
    }])
}

fn check_pending(req: &RequestRow) -> Result<(), AppError> {
    if req.status == WorkflowApprovalStatus::Pending {
        return Ok(());
    }
    let status = serde_json::to_value(req.status).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default();
    Err(detail(
        ErrorCode::Conflict,
        "id",
        "not_pending",
        format!("Approval request #{} is {status}: it can no longer change", req.request_no),
    ))
}

fn check_version(req: &RequestRow, expected: i32) -> Result<(), AppError> {
    if req.version == expected {
        return Ok(());
    }
    Err(detail(
        ErrorCode::VersionConflict,
        "expectedVersion",
        "stale",
        format!(
            "The approval request was changed by someone else (you sent version {expected}, current is {}). Reload \
             and retry.",
            req.version
        ),
    ))
}

/// What `availableTransitions[].blockedBy` says while a request is pending.
pub(super) fn pending_reason(p: &WorkflowPendingApproval) -> WorkflowBlockedReason {
    WorkflowBlockedReason {
        field: "approvalRequestId".into(),
        code: "approval_pending".into(),
        message: format!(
            "Approval request #{} for transition {} is pending (step {} of {}): no other transition runs until it is \
             decided, withdrawn or cancelled",
            p.request_no, p.transition_key, p.step_no, p.step_count
        ),
    }
}

/// 409 WORKFLOW_APPROVAL_PENDING: a transition on an instance that waits for approval.
pub(super) fn pending_error(p: &WorkflowPendingApproval) -> AppError {
    let message = format!(
        "This workflow instance waits for approval of transition {} (request #{}): no other transition runs until \
         the request is decided, withdrawn or cancelled",
        p.transition_key, p.request_no
    );
    AppError::new(ErrorCode::WorkflowApprovalPending, message).with_details(vec![FieldError {
        location: FieldLocation::Params,
        field: "approvalRequestId".into(),
        message: p.request_id.to_string(),
        code: "approval_pending".into(),
    }])
}

// ---------------------------------------------------------------------------
// Eligibility (§4.2)
// ---------------------------------------------------------------------------

fn kind_str(k: WorkflowApprovalPrincipalKind) -> &'static str {
    match k {
        WorkflowApprovalPrincipalKind::User => "user",
        WorkflowApprovalPrincipalKind::Profile => "profile",
        WorkflowApprovalPrincipalKind::Group => "group",
    }
}

/// Resolves the `approver` assignments of step `s` into the request's
/// eligibility rows: profiles and groups stay principals (membership is read
/// at decision time), a CI field and service owners resolve to users now.
/// Records how many active users who may view the CI could decide it.
///
/// A CI field source is dropped when the field's current value was set by an
/// `excluded` user (directly or through an API token one of them minted:
/// GH#709), or by an API token or import that recorded no user, so a
/// requester cannot pick their approver by editing the field (GH#664). The
/// same holds for a service owner an excluded user made an owner, and for the
/// owners of a service an excluded user added the CI to (GH#708). The dropped
/// sources are kept on the step with the change that named the approvers.
async fn resolve(
    conn: &mut PgConnection,
    request: Uuid,
    row: &InstanceRow,
    values: &Map<String, Value>,
    transition: &str,
    s: &PinnedStep,
    excluded: &[Uuid],
) -> Result<(), AppError> {
    use WorkflowApprovalPrincipalKind as K;
    let assignments = approvers::load(&mut *conn, row.definition_id).await?;
    let mut rows: Vec<(K, Uuid, Value)> = Vec::new();
    let mut dropped: Vec<WorkflowApprovalDroppedSource> = Vec::new();
    for a in assignments
        .iter()
        .filter(|a| a.transition_key == transition && a.step_key == s.key && a.role == WorkflowApproverRole::Approver)
    {
        let via = json!({ "source": a.source.kind().as_str(), "label": a.source.label() });
        match &a.source {
            Source::Profile { id, .. } => rows.push((K::Profile, *id, via)),
            Source::Group { id, .. } => rows.push((K::Group, *id, via)),
            Source::User { id, .. } => rows.push((K::User, *id, via)),
            Source::Attribute { key, .. } => {
                let Some(person) = values.get(key).and_then(Value::as_str).and_then(|v| v.parse::<Uuid>().ok()) else {
                    continue;
                };
                let change = approvers::last_change(&mut *conn, row.ci_id, key).await?;
                let label = a.source.label();
                if let Some(c) = change.clone()
                    && let Some((reason, message)) = approvers::field_drop_reason(&c, excluded, &label)
                {
                    dropped.push(WorkflowApprovalDroppedSource {
                        source: a.source.kind(),
                        label,
                        reason,
                        message,
                        field_last_changed: c,
                    });
                    continue;
                }
                let mut via = via;
                via["fieldLastChanged"] = json!(change);
                let users: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM cmdb.users WHERE person_ci_id = $1")
                    .bind(person)
                    .fetch_all(&mut *conn)
                    .await?;
                rows.extend(users.into_iter().map(|u| (K::User, u, via.clone())));
            }
            Source::ServiceOwner(role) => {
                // Direct membership only (A-Q3), less the owners and memberships an excluded user set (GH#708).
                let owners = approvers::service_owners(&mut *conn, row.ci_id, *role, excluded).await?;
                rows.extend(owners.users.into_iter().map(|u| (K::User, u, via.clone())));
                rows.extend(owners.groups.into_iter().map(|g| (K::Group, g, via.clone())));
                dropped.extend(owners.dropped);
            }
        }
    }
    sqlx::query("DELETE FROM cmdb.workflow_approval_eligibility WHERE request_id = $1 AND step_no = $2")
        .bind(request)
        .bind(s.step_no)
        .execute(&mut *conn)
        .await?;
    let kinds: Vec<&str> = rows.iter().map(|r| kind_str(r.0)).collect();
    let ids: Vec<Uuid> = rows.iter().map(|r| r.1).collect();
    let vias: Vec<SqlJson<Value>> = rows.iter().map(|r| SqlJson(r.2.clone())).collect();
    sqlx::query(
        "INSERT INTO cmdb.workflow_approval_eligibility (request_id, step_no, role, principal_kind, principal_id, via)
         SELECT $1, $2, 'approver', u.kind, u.id, u.via FROM unnest($3::text[], $4::uuid[], $5::jsonb[]) AS u(kind, id, via)
         ON CONFLICT DO NOTHING",
    )
    .bind(request)
    .bind(s.step_no)
    .bind(&kinds)
    .bind(&ids)
    .bind(&vias)
    .execute(&mut *conn)
    .await?;
    let pick = |k: K| -> Vec<Uuid> { rows.iter().filter(|r| r.0 == k).map(|r| r.1).collect() };
    let users: Vec<Uuid> = sqlx::query_scalar(
        "SELECT u.id FROM cmdb.users u
         WHERE u.is_active AND NOT (u.id = ANY($4)) AND u.id IN (
           SELECT user_id FROM cmdb.user_permission_profiles WHERE profile_id = ANY($1)
           UNION SELECT user_id FROM cmdb.user_group_members WHERE group_id = ANY($2)
           UNION SELECT unnest($3::uuid[]))",
    )
    .bind(pick(K::Profile))
    .bind(pick(K::Group))
    .bind(pick(K::User))
    .bind(excluded)
    .fetch_all(&mut *conn)
    .await?;
    let permissions = auth_data::load_permissions_of(&mut *conn, &users).await?;
    let count = users.iter().filter(|u| permissions.get(u).is_some_and(|p| p.can(row.class_id, ClassOp::View))).count();
    sqlx::query(
        "UPDATE cmdb.workflow_approval_request_steps SET eligible_count = $3, dropped_sources = $4, resolved_at = now()
         WHERE request_id = $1 AND step_no = $2",
    )
    .bind(request)
    .bind(s.step_no)
    .bind(count as i32)
    .bind(SqlJson(&dropped))
    .execute(&mut *conn)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Request (§5.1)
// ---------------------------------------------------------------------------

/// What a gated transition stages: the fields sent, the CI's values as they
/// were (the baseline of the stale check), and the comment.
pub(super) struct Staged<'a> {
    pub(super) fields: &'a Map<String, Value>,
    pub(super) current: &'a Map<String, Value>,
    pub(super) comment: Option<&'a str>,
}

/// Creates the approval request of gated transition `t` on the locked
/// instance `row`, whose checks all passed; returns its id.
pub(super) async fn create(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    row: &InstanceRow,
    p: &Pinned,
    t: &PinnedTransition,
    staged: Staged<'_>,
) -> Result<Uuid, AppError> {
    let Some(me) = ctx.principal() else {
        return Err(forbidden("Only a signed-in user or an API token can request an approval"));
    };
    let (token_id, minted_by, creator) = match me.credential {
        Credential::Token { token_id, minted_by, creator_id, .. } => (token_id, minted_by, creator_id),
        Credential::Session { .. } => (None, None, None),
    };
    // Every identity behind the credential (§4.3). A token whose creator is
    // unknown excludes only its owner; its id on the request keeps that visible.
    let mut excluded = vec![me.user_id];
    excluded.extend(creator.filter(|c| *c != me.user_id));
    let baseline: Map<String, Value> =
        staged.fields.keys().map(|k| (k.clone(), staged.current.get(k).cloned().unwrap_or(Value::Null))).collect();
    let (id, request_no): (Uuid, i32) = sqlx::query_as(
        "INSERT INTO cmdb.workflow_approval_requests
           (instance_id, version_id, transition_key, request_no, status, requested_by_id, requested_by_name,
            excluded_user_ids, token_id, token_creator_id, comment, staged_fields, field_baseline)
         VALUES ($1, $2, $3,
                 (SELECT coalesce(max(request_no), 0) + 1 FROM cmdb.workflow_approval_requests WHERE instance_id = $1),
                 'pending', $4, $5, $6, $7, $8, $9, $10, $11)
         RETURNING id, request_no",
    )
    .bind(row.id)
    .bind(row.version_id)
    .bind(&t.key)
    .bind(me.user_id)
    .bind(&me.username)
    .bind(&excluded)
    .bind(token_id)
    .bind(token_id.and(minted_by))
    .bind(staged.comment)
    .bind(SqlJson(staged.fields))
    .bind(SqlJson(&baseline))
    .fetch_one(&mut *conn)
    .await?;
    let steps: Vec<&PinnedStep> = p.steps_of(t.id).collect();
    for s in &steps {
        sqlx::query(
            "INSERT INTO cmdb.workflow_approval_request_steps
               (request_id, step_no, step_key, required_approvals, status, activated_at, due_at)
             VALUES ($1, $2, $3, $4, CASE WHEN $2 = 1 THEN 'active' ELSE 'waiting' END,
                     CASE WHEN $2 = 1 THEN now() END, CASE WHEN $2 = 1 THEN now() + make_interval(mins => $5) END)",
        )
        .bind(id)
        .bind(s.step_no)
        .bind(&s.key)
        .bind(s.required_approvals)
        .bind(s.due_minutes)
        .execute(&mut *conn)
        .await?;
    }
    if steps.is_empty() {
        return Err(AppError::internal());
    }
    // Every step's approvers are fixed now: an edit of a field that names
    // them, while the request is pending, changes nothing (GH#664).
    for s in &steps {
        resolve(&mut *conn, id, row, staged.current, &t.key, s, &excluded).await?;
    }
    sqlx::query("UPDATE cmdb.workflow_instances SET version = version + 1 WHERE id = $1")
        .bind(row.id)
        .execute(&mut *conn)
        .await?;
    insert_event(
        &mut *conn,
        ctx,
        NewEvent {
            instance: row.id,
            kind: "approval_request",
            transition_key: None,
            from_state_key: Some(&row.state_key),
            to_state_key: &row.state_key,
            to_version_no: row.version_no,
            comment: staged.comment,
            field_changes: None,
            approval: Some((id, Some(1))),
        },
    )
    .await?;
    // Key, required approvals, due, eligible count and dropped sources of each step.
    type Due = (String, i16, Option<DateTime<Utc>>, Option<i32>, SqlJson<Value>);
    let due: Vec<Due> = sqlx::query_as(
        "SELECT step_key, required_approvals, due_at, eligible_count, dropped_sources
         FROM cmdb.workflow_approval_request_steps WHERE request_id = $1 ORDER BY step_no",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let to = p.state(t.to_state_id).map(|s| s.key.as_str()).unwrap_or_default();
    let entry = AuditEntry {
        action: AuditAction::WorkflowApprovalRequest,
        entity_type: "configuration_items",
        entity_id: row.ci_id,
        old_value: None,
        new_value: Some(json!({
            "instanceId": row.id, "definitionKey": row.definition_key, "requestId": id, "requestNo": request_no,
            "transitionKey": t.key, "fromStateKey": row.state_key, "toStateKey": to, "stagedFields": staged.fields,
            "comment": staged.comment, "tokenId": token_id, "tokenCreatorId": token_id.and(minted_by),
            "excludedUserIds": excluded,
            "steps": due.iter().map(|(k, n, d, e, x)| json!({ "key": k, "required": n, "dueAt": d,
                "eligibleCount": e, "droppedSources": x.0 })).collect::<Vec<_>>(),
        })),
    };
    crud::write_audit(&mut *conn, ctx, vec![entry]).await?;
    Ok(id)
}

// ---------------------------------------------------------------------------
// Who may decide (§4.3)
// ---------------------------------------------------------------------------

/// The caller as a decider of the active step.
struct Decider {
    user_id: Uuid,
    name: String,
    token_id: Option<Uuid>,
    minted_by: Option<Uuid>,
    /// The eligibility rows that qualify them.
    via: Vec<Value>,
}

/// `(transition_key, user_id)` of everyone `excludeActorsOf` refuses on
/// instance `instance` for the transition keys `keys` (SQL expressions), by
/// user id only (SHAA-1872 C4): who ran those transitions (a final
/// approval's decider is its actor), who requested them, and every approver
/// of an approved request for them, not only the one whose vote completed the
/// quorum (GH#635). Shared by a decision and the inbox, so both agree.
pub(super) fn actors_of(instance: &str, keys: &str) -> String {
    format!(
        "SELECT e.transition_key, e.actor_id::uuid FROM cmdb.workflow_instance_events e
         WHERE e.instance_id = {instance} AND e.kind = 'transition' AND e.transition_key = ANY({keys})
           AND e.actor_type IN ('user', 'api_client')
           AND e.actor_id ~ '^[0-9a-f]{{8}}-[0-9a-f]{{4}}-[0-9a-f]{{4}}-[0-9a-f]{{4}}-[0-9a-f]{{12}}$'
         UNION
         SELECT ar.transition_key, x.id FROM cmdb.workflow_approval_requests ar, unnest(ar.excluded_user_ids) AS x(id)
         WHERE ar.instance_id = {instance} AND ar.transition_key = ANY({keys})
         UNION
         SELECT ar.transition_key, x.id FROM cmdb.workflow_approval_requests ar
         JOIN cmdb.workflow_approval_decisions ad ON ad.request_id = ar.id AND ad.decision = 'approve'
         CROSS JOIN LATERAL (VALUES (ad.actor_id), (ad.on_behalf_of_id)) AS x(id)
         WHERE ar.instance_id = {instance} AND ar.transition_key = ANY({keys}) AND ar.status = 'approved'
           AND x.id IS NOT NULL"
    )
}

/// Whether the caller may decide step `s` of `req` now, with the reason
/// they may not as the error a decision would get. Reads only.
async fn check_decider(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    req: &RequestRow,
    row: &InstanceRow,
    s: &PinnedStep,
    overdue: bool,
) -> Result<Decider, AppError> {
    use ErrorCode::{Forbidden, WorkflowApprovalSelf as SelfApproval};
    let Some(me) = ctx.principal() else {
        return Err(detail(Forbidden, "decision", "not_eligible", "Only a user can decide an approval request".into()));
    };
    let (token, token_id, minted_by) = match me.credential {
        Credential::Token { token_id, minted_by, .. } => (true, token_id, minted_by),
        Credential::Session { .. } => (false, None, None),
    };
    // Four-eyes: never the requester or the requesting token's creator, whatever the profile or credential.
    if req.excluded_user_ids.contains(&me.user_id) {
        return Err(detail(
            SelfApproval,
            "decision",
            "requester",
            "Four-eyes: you made this request (or minted the token it was made with), so someone else must decide it"
                .into(),
        ));
    }
    if minted_by.is_some_and(|c| req.excluded_user_ids.contains(&c)) {
        return Err(detail(
            SelfApproval,
            "decision",
            "token_creator",
            "Four-eyes: this API token was minted by the requester, so it cannot decide the request".into(),
        ));
    }
    if s.distinct_from_earlier && s.step_no > 1 {
        let earlier: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM cmdb.workflow_approval_decisions
                            WHERE request_id = $1 AND step_no < $2 AND decision = 'approve'
                              AND (actor_id = $3 OR on_behalf_of_id = $3))",
        )
        .bind(req.id)
        .bind(s.step_no)
        .bind(me.user_id)
        .fetch_one(&mut *conn)
        .await?;
        if earlier {
            return Err(detail(
                SelfApproval,
                "decision",
                "earlier_step",
                format!("You approved an earlier step of this request, so step {} needs someone else", s.key),
            ));
        }
    }
    if !s.exclude_actors_of.is_empty() {
        let actors: Vec<(String, Uuid)> = sqlx::query_as(sqlx::AssertSqlSafe(actors_of("$1", "$2")))
            .bind(row.id)
            .bind(&s.exclude_actors_of)
            .fetch_all(&mut *conn)
            .await?;
        if let Some((key, _)) = actors.iter().find(|(_, u)| *u == me.user_id) {
            return Err(detail(
                SelfApproval,
                "decision",
                &format!("actor_of:{key}"),
                format!("You took part in transition {key} of this instance, so step {} needs someone else", s.key),
            ));
        }
    }
    // Tokens (SHAA-1872 C1): only where the step allows them, and only one the owner minted.
    if token {
        if !s.allow_api_tokens {
            return Err(detail(
                Forbidden,
                "decision",
                "session_required",
                format!("Step {} must be decided in a signed-in session, not with an API token", s.key),
            ));
        }
        if minted_by != Some(me.user_id) {
            return Err(detail(
                Forbidden,
                "decision",
                "token_not_self_minted",
                "Only an API token you minted for yourself can decide an approval request".into(),
            ));
        }
    }
    // Eligibility, read live: an active account matching a principal of the step.
    let via: Vec<SqlJson<Value>> = sqlx::query_scalar(
        "SELECT e.via FROM cmdb.workflow_approval_eligibility e JOIN cmdb.users u ON u.id = $3 AND u.is_active
         WHERE e.request_id = $1 AND e.step_no = $2 AND (e.role = 'approver' OR $4)
           AND ((e.principal_kind = 'user' AND e.principal_id = $3)
             OR (e.principal_kind = 'profile' AND e.principal_id IN
                   (SELECT profile_id FROM cmdb.user_permission_profiles WHERE user_id = $3))
             OR (e.principal_kind = 'group' AND e.principal_id IN
                   (SELECT group_id FROM cmdb.user_group_members WHERE user_id = $3)))
         ORDER BY e.role, e.principal_kind, e.principal_id",
    )
    .bind(req.id)
    .bind(s.step_no)
    .bind(me.user_id)
    .bind(overdue)
    .fetch_all(&mut *conn)
    .await?;
    if via.is_empty() {
        return Err(detail(
            Forbidden,
            "decision",
            "not_eligible",
            format!("You are not an approver of step {} of this request", s.key),
        ));
    }
    Ok(Decider {
        user_id: me.user_id,
        name: me.username.clone(),
        token_id,
        minted_by,
        via: via.into_iter().map(|v| v.0).collect(),
    })
}

// ---------------------------------------------------------------------------
// Decide (§5.1, §5.2)
// ---------------------------------------------------------------------------

/// The active step of `req` in its pinned policy, and whether it is overdue.
async fn active_step<'p>(
    conn: &mut PgConnection,
    p: &'p Pinned,
    t: &PinnedTransition,
    req: &RequestRow,
) -> Result<(&'p PinnedStep, bool), AppError> {
    let s = p.steps_of(t.id).find(|s| s.step_no == req.current_step_no).ok_or_else(AppError::internal)?;
    let overdue: bool = sqlx::query_scalar(
        "SELECT overdue_at IS NOT NULL FROM cmdb.workflow_approval_request_steps WHERE request_id = $1 AND step_no = $2",
    )
    .bind(req.id)
    .bind(s.step_no)
    .fetch_one(&mut *conn)
    .await?;
    Ok((s, overdue))
}

async fn bump_instance(conn: &mut PgConnection, id: Uuid) -> Result<(), AppError> {
    sqlx::query("UPDATE cmdb.workflow_instances SET version = version + 1 WHERE id = $1")
        .bind(id)
        .execute(conn)
        .await?;
    Ok(())
}

pub async fn decide(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    b: &WorkflowApprovalDecide,
) -> Result<WorkflowApprovalOutcome, AppError> {
    let mut tx = pool.begin().await?;
    let (req, row) = lock_request(&mut tx, ctx, id).await?;
    check_pending(&req)?;
    check_version(&req, b.expected_version)?;
    let p = pinned(&mut tx, req.version_id).await?;
    let t = p.transition(&req.transition_key).ok_or_else(AppError::internal)?;
    let (s, overdue) = active_step(&mut tx, &p, t, &req).await?;
    if s.key != b.step_key {
        return Err(detail(
            ErrorCode::Conflict,
            "stepKey",
            "step_not_active",
            format!("Step {} is not the active step of this request; step {} is. Reload and retry.", b.step_key, s.key),
        ));
    }
    let decider = check_decider(&mut tx, ctx, &req, &row, s, overdue).await?;
    let already: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM cmdb.workflow_approval_decisions
                        WHERE request_id = $1 AND step_no = $2 AND (actor_id = $3 OR on_behalf_of_id = $3))",
    )
    .bind(req.id)
    .bind(s.step_no)
    .bind(decider.user_id)
    .fetch_one(&mut *tx)
    .await?;
    if already {
        return Err(detail(
            ErrorCode::Conflict,
            "decision",
            "already_decided",
            format!("You already decided step {} of this request", s.key),
        ));
    }
    let comment = b.comment.as_deref().map(str::trim).filter(|c| !c.is_empty());
    let credential = if decider.token_id.is_some() {
        WorkflowApprovalCredential::Token
    } else {
        WorkflowApprovalCredential::Session
    };
    sqlx::query(
        "INSERT INTO cmdb.workflow_approval_decisions
           (request_id, step_no, decision, actor_id, actor_name, credential, token_id, token_creator_id, via, comment,
            http_request_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(req.id)
    .bind(s.step_no)
    .bind(b.decision)
    .bind(decider.user_id)
    .bind(&decider.name)
    .bind(credential)
    .bind(decider.token_id)
    .bind(decider.token_id.and(decider.minted_by))
    .bind(SqlJson(&decider.via))
    .bind(comment)
    .bind(&ctx.request_id)
    .execute(&mut *tx)
    .await?;
    let approvals: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM cmdb.workflow_approval_decisions
         WHERE request_id = $1 AND step_no = $2 AND decision = 'approve'",
    )
    .bind(req.id)
    .bind(s.step_no)
    .fetch_one(&mut *tx)
    .await?;
    insert_event(
        &mut tx,
        ctx,
        NewEvent {
            instance: row.id,
            kind: "approval_decision",
            transition_key: None,
            from_state_key: Some(&row.state_key),
            to_state_key: &row.state_key,
            to_version_no: row.version_no,
            comment,
            field_changes: None,
            approval: Some((req.id, Some(s.step_no))),
        },
    )
    .await?;
    let entry = AuditEntry {
        action: AuditAction::WorkflowApprovalDecide,
        entity_type: "configuration_items",
        entity_id: row.ci_id,
        old_value: None,
        new_value: Some(json!({
            "instanceId": row.id, "requestId": req.id, "requestNo": req.request_no, "transitionKey": t.key,
            "stepKey": s.key, "decision": b.decision.as_str(), "comment": comment,
            "credential": credential, "tokenId": decider.token_id,
            "tokenCreatorId": decider.token_id.and(decider.minted_by), "via": decider.via,
            "approvals": approvals, "required": s.required_approvals,
        })),
    };

    let next = p.steps_of(t.id).find(|n| n.step_no == s.step_no + 1);
    match b.decision {
        // Any rejection rejects the request (veto, A-Q2); the instance stays where it is.
        WorkflowApprovalVerdict::Reject => {
            sqlx::query(
                "UPDATE cmdb.workflow_approval_request_steps
                 SET status = CASE WHEN step_no = $2 THEN 'rejected' ELSE 'closed' END, completed_at = now()
                 WHERE request_id = $1 AND status IN ('active', 'waiting')",
            )
            .bind(req.id)
            .bind(s.step_no)
            .execute(&mut *tx)
            .await?;
            finish(
                &mut tx,
                &req,
                WorkflowApprovalStatus::Rejected,
                WorkflowApprovalCloseReason::Rejected,
                &decider.name,
            )
            .await?;
            bump_instance(&mut tx, row.id).await?;
            crud::write_audit(&mut tx, ctx, vec![entry]).await?;
        }
        WorkflowApprovalVerdict::Approve if approvals < i64::from(s.required_approvals) => {
            bump_request(&mut tx, req.id).await?;
            bump_instance(&mut tx, row.id).await?;
            crud::write_audit(&mut tx, ctx, vec![entry]).await?;
        }
        WorkflowApprovalVerdict::Approve => {
            sqlx::query(
                "UPDATE cmdb.workflow_approval_request_steps SET status = 'approved', completed_at = now()
                 WHERE request_id = $1 AND step_no = $2",
            )
            .bind(req.id)
            .bind(s.step_no)
            .execute(&mut *tx)
            .await?;
            if let Some(n) = next {
                // Its approvers were resolved with the request (GH#664). Only a
                // request made before that has a step still unresolved; it is
                // resolved now, before the audit row, so the audit chain head is
                // held briefly (§9).
                let unresolved: bool = sqlx::query_scalar(
                    "UPDATE cmdb.workflow_approval_request_steps
                     SET status = 'active', activated_at = now(), due_at = now() + make_interval(mins => $3)
                     WHERE request_id = $1 AND step_no = $2 RETURNING resolved_at IS NULL",
                )
                .bind(req.id)
                .bind(n.step_no)
                .bind(n.due_minutes)
                .fetch_one(&mut *tx)
                .await?;
                if unresolved {
                    let model = Model::load(&mut tx).await?;
                    let values = current_values(&mut tx, &model, row.ci_id).await?;
                    resolve(&mut tx, req.id, &row, &values, &t.key, n, &req.excluded_user_ids).await?;
                }
                sqlx::query(
                    "UPDATE cmdb.workflow_approval_requests SET current_step_no = $2, version = version + 1
                     WHERE id = $1",
                )
                .bind(req.id)
                .bind(n.step_no)
                .execute(&mut *tx)
                .await?;
                bump_instance(&mut tx, row.id).await?;
                crud::write_audit(&mut tx, ctx, vec![entry]).await?;
            } else {
                crud::write_audit(&mut tx, ctx, vec![entry]).await?;
                apply(&mut tx, ctx, &row, &p, t, &req, &decider).await?;
            }
        }
    }
    let out = outcome(&mut tx, ctx, req.id, &p).await?;
    tx.commit().await?;
    Ok(out)
}

async fn bump_request(conn: &mut PgConnection, id: Uuid) -> Result<(), AppError> {
    sqlx::query("UPDATE cmdb.workflow_approval_requests SET version = version + 1 WHERE id = $1")
        .bind(id)
        .execute(conn)
        .await?;
    Ok(())
}

/// Closes the request (approved or rejected by a decision).
async fn finish(
    conn: &mut PgConnection,
    req: &RequestRow,
    status: WorkflowApprovalStatus,
    reason: WorkflowApprovalCloseReason,
    by: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE cmdb.workflow_approval_requests
         SET status = $2, close_reason = $3, closed_at = now(), closed_by_name = $4, version = version + 1
         WHERE id = $1",
    )
    .bind(req.id)
    .bind(status)
    .bind(reason.as_str())
    .bind(by)
    .execute(conn)
    .await?;
    Ok(())
}

fn stale(t: &PinnedTransition, mut failed: Vec<FieldError>) -> AppError {
    failed.sort_by(|a, b| a.field.cmp(&b.field).then_with(|| a.code.cmp(&b.code)));
    AppError::new(
        ErrorCode::WorkflowApprovalStale,
        format!(
            "The final approval cannot apply transition {}: the CI changed since the request was made, or the \
             request no longer matches the transition (see details). Nothing was recorded; reject the request, or \
             ask the requester to withdraw it and request again",
            t.key
        ),
    )
    .with_details(failed)
}

fn problem(field: String, message: String, code: &str) -> FieldError {
    FieldError { location: FieldLocation::Body, field, message, code: code.into() }
}

/// The identity the final apply writes with (A-Q7, SHAA-1872): the decider,
/// with the edit right on the CI's type only, and no view right beyond the
/// decider's own (a staged reference to a CI the decider may not view is
/// refused, as a PATCH would refuse it). Never the system scope: the item
/// write path validates every value and applies every write rule.
fn narrowed(ctx: &RequestContext, decider: &Decider, class_id: Uuid) -> RequestContext {
    let own = ctx.principal().map(|p| &p.permissions);
    let view = ClassRights { view: true, ..ClassRights::default() };
    let mut permissions = Permissions {
        all_classes: if own.is_some_and(|p| p.administrator || p.all_classes.view) {
            view
        } else {
            ClassRights::default()
        },
        ..Permissions::default()
    };
    for (class, rights) in own.map(|p| &p.classes).into_iter().flatten() {
        if rights.view {
            permissions.classes.insert(*class, view);
        }
    }
    permissions.classes.insert(class_id, ClassRights { view: true, edit: true, ..ClassRights::default() });
    let credential = ctx.principal().map(|p| p.credential.clone()).unwrap_or(Credential::Token {
        profile_id: None,
        creator_id: None,
        token_id: decider.token_id,
        minted_by: decider.minted_by,
    });
    let principal = Principal { user_id: decider.user_id, username: decider.name.clone(), credential, permissions };
    RequestContext { caller: Caller::User(Arc::new(principal)), ..ctx.clone() }
}

/// The final approval (§5.2): re-checks, then applies the transition with the
/// staged fields, in the deciding transaction. Any failure writes nothing.
async fn apply(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    row: &InstanceRow,
    p: &Pinned,
    t: &PinnedTransition,
    req: &RequestRow,
    decider: &Decider,
) -> Result<(), AppError> {
    check_active(row)?;
    if row.version_id != req.version_id || row.current_state_id != t.from_state_id {
        return Err(stale(
            t,
            vec![problem("instance".into(), "The instance is no longer where the request left it".into(), "moved")],
        ));
    }
    let to = p.state(t.to_state_id).ok_or_else(AppError::internal)?;
    let model = Model::load(&mut *conn).await?;
    // Only the transition's fields, re-intersected now: the stored request is
    // never trusted to say what may be written (A-Q7.1).
    let allowed: HashMap<String, &super::PinnedField> =
        p.fields_of(t.id).filter_map(|f| model.field(f.attribute_id).map(|a| (a.key.clone(), f))).collect();
    let staged = &req.staged_fields.0;
    let foreign: Vec<FieldError> = staged
        .keys()
        .filter(|k| !allowed.contains_key(k.as_str()))
        .map(|k| {
            problem(
                format!("fields.{k}"),
                format!("Transition {} does not take field {k}", t.key),
                "not_a_transition_field",
            )
        })
        .collect();
    if !foreign.is_empty() {
        return Err(stale(t, foreign));
    }
    let driven = super::state_field_keys(&mut *conn, &model, row).await?;
    let state_fields: Vec<FieldError> = staged
        .keys()
        .filter(|k| driven.contains(k))
        .map(|k| {
            problem(
                format!("fields.{k}"),
                format!("{k} is a workflow state field: a transition cannot set it"),
                "state_field",
            )
        })
        .collect();
    if !state_fields.is_empty() {
        return Err(stale(t, state_fields));
    }
    let current = current_values(&mut *conn, &model, row.ci_id).await?;
    let norm = |v: Option<&Value>| v.cloned().unwrap_or(Value::Null);
    let mut failed: Vec<FieldError> = staged
        .keys()
        .filter(|k| norm(current.get(*k)) != norm(req.field_baseline.0.get(*k)))
        .map(|k| {
            let label = model.field(allowed[k].attribute_id).map_or(k.as_str(), |f| f.label.as_str());
            problem(format!("fields.{k}"), format!("{label} changed since the request was made"), "changed")
        })
        .collect();
    let mut values = current.clone();
    values.extend(staged.clone());
    for (key, f) in &allowed {
        if f.is_required && !is_set(values.get(key)) {
            let label = model.field(f.attribute_id).map_or(key.as_str(), |a| a.label.as_str());
            failed.push(problem(
                format!("fields.{key}"),
                format!("{label} is required for transition {}", t.key),
                "required",
            ));
        }
    }
    if let Some(c) = &t.conditions {
        let by_id = condition_values(&mut *conn, &model, row.class_id, &values).await?;
        failed.extend(
            eval::failures(&c.0, &Subject { model: &model, values: &by_id })
                .into_iter()
                .map(|f| problem(format!("fields.{}", f.key), f.message, "condition")),
        );
    }
    if !failed.is_empty() {
        return Err(stale(t, failed));
    }

    let requested_by = json!({ "id": req.requested_by_id, "name": req.requested_by_name });
    let mut note = Map::new();
    note.insert("approvalRequestId".into(), json!(req.id));
    note.insert("requestedBy".into(), requested_by.clone());
    let writer = narrowed(ctx, decider, row.class_id);
    let mut attributes = staged.clone();
    attributes.extend(state_value(&model, row.state_attribute_id, to));
    let changes =
        write_ci(&mut *conn, &writer, row.ci_id, row.class_id, attributes, Some(&note)).await.map_err(|e| {
            let e = as_transition_fields(e);
            if e.code == ErrorCode::ValidationError { stale(t, e.details.unwrap_or_default()) } else { e }
        })?;
    move_to(&mut *conn, row.id, to).await?;
    insert_event(
        &mut *conn,
        ctx,
        NewEvent {
            instance: row.id,
            kind: "transition",
            transition_key: Some(&t.key),
            from_state_key: Some(&row.state_key),
            to_state_key: &to.key,
            to_version_no: row.version_no,
            comment: req.comment.as_deref(),
            field_changes: changes.clone(),
            approval: Some((req.id, None)),
        },
    )
    .await?;
    let approvers: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT st.step_key, dc.actor_name, dc.on_behalf_of_name FROM cmdb.workflow_approval_decisions dc
         JOIN cmdb.workflow_approval_request_steps st ON st.request_id = dc.request_id AND st.step_no = dc.step_no
         WHERE dc.request_id = $1 AND dc.decision = 'approve' ORDER BY st.step_no, dc.id",
    )
    .bind(req.id)
    .fetch_all(&mut *conn)
    .await?;
    let approvers: Vec<Value> = approvers
        .iter()
        .map(|(step, name, behalf)| json!({ "stepKey": step, "approvedBy": name, "onBehalfOf": behalf }))
        .collect();
    let entry = AuditEntry {
        action: AuditAction::WorkflowTransition,
        entity_type: "configuration_items",
        entity_id: row.ci_id,
        old_value: Some(json!({ "instanceId": row.id, "definitionKey": row.definition_key, "stateKey": row.state_key,
            "version": row.version })),
        new_value: Some(json!({ "instanceId": row.id, "definitionKey": row.definition_key, "transitionKey": t.key,
            "stateKey": to.key, "version": row.version + 1, "comment": req.comment, "fields": changes,
            "approvalRequestId": req.id, "requestedBy": requested_by, "approvers": approvers })),
    };
    crud::write_audit(&mut *conn, ctx, vec![entry]).await?;
    finish(conn, req, WorkflowApprovalStatus::Approved, WorkflowApprovalCloseReason::Approved, &decider.name).await
}

// ---------------------------------------------------------------------------
// Withdraw, cancel, and the closures of the other engine paths (§5.3)
// ---------------------------------------------------------------------------

/// A request a closure ended, with its audit row (written by the caller).
pub(super) struct Closed {
    pub(super) request_id: Uuid,
    pub(super) entry: AuditEntry,
}

/// Closes the pending request of the locked instance `row`, if it has one:
/// its open steps close, the instance gets an event, and the returned audit
/// row is for the caller to write. The instance's version is the caller's.
async fn close(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    row: &InstanceRow,
    status: WorkflowApprovalStatus,
    reason: WorkflowApprovalCloseReason,
    comment: Option<&str>,
    kind: &'static str,
) -> Result<Option<Closed>, AppError> {
    let (_, by) = starter(ctx);
    let closed: Option<(Uuid, i32, String)> = sqlx::query_as(
        "UPDATE cmdb.workflow_approval_requests
         SET status = $2, close_reason = $3, closed_at = now(), closed_by_name = $4, version = version + 1
         WHERE instance_id = $1 AND status = 'pending' RETURNING id, request_no, transition_key",
    )
    .bind(row.id)
    .bind(status)
    .bind(reason.as_str())
    .bind(&by)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((id, request_no, transition_key)) = closed else { return Ok(None) };
    sqlx::query(
        "UPDATE cmdb.workflow_approval_request_steps SET status = 'closed', completed_at = now()
         WHERE request_id = $1 AND status IN ('active', 'waiting')",
    )
    .bind(id)
    .execute(&mut *conn)
    .await?;
    insert_event(
        &mut *conn,
        ctx,
        NewEvent {
            instance: row.id,
            kind,
            transition_key: None,
            from_state_key: Some(&row.state_key),
            to_state_key: &row.state_key,
            to_version_no: row.version_no,
            comment,
            field_changes: None,
            approval: Some((id, None)),
        },
    )
    .await?;
    let entry = AuditEntry {
        action: AuditAction::WorkflowApprovalClose,
        entity_type: "configuration_items",
        entity_id: row.ci_id,
        old_value: None,
        new_value: Some(json!({ "instanceId": row.id, "requestId": id, "requestNo": request_no,
            "transitionKey": transition_key, "status": status, "reason": reason.as_str(), "comment": comment })),
    };
    Ok(Some(Closed { request_id: id, entry }))
}

/// Cancel, force and migration close the pending request of the instance
/// they change (the caller holds the CI and the instance locks).
pub(super) async fn close_pending(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    row: &InstanceRow,
    reason: WorkflowApprovalCloseReason,
    comment: Option<&str>,
) -> Result<Option<Closed>, AppError> {
    close(conn, ctx, row, WorkflowApprovalStatus::Cancelled, reason, comment, "approval_close").await
}

/// A CI soft delete closes the pending requests of the instances it cancels
/// (`instances`, already cancelled and locked); events name the system.
pub(super) async fn close_for_deleted_ci(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    ci: Uuid,
    instances: &[Uuid],
    comment: &str,
) -> Result<Vec<AuditEntry>, AppError> {
    let pending: Vec<Uuid> = sqlx::query_scalar(
        "SELECT instance_id FROM cmdb.workflow_approval_requests
         WHERE instance_id = ANY($1) AND status = 'pending' ORDER BY instance_id",
    )
    .bind(instances)
    .fetch_all(&mut *conn)
    .await?;
    let system = RequestContext::system("system", ctx.request_id.clone());
    let mut out = Vec::with_capacity(pending.len());
    for id in pending {
        let row = instance(&mut *conn, id).await?.ok_or_else(AppError::internal)?;
        debug_assert_eq!(row.ci_id, ci);
        let closed =
            close_pending(&mut *conn, &system, &row, WorkflowApprovalCloseReason::CiDeleted, Some(comment)).await?;
        out.extend(closed.map(|c| c.entry));
    }
    Ok(out)
}

/// A real migration with `pendingApprovals: cancel` closes the pending
/// request of each instance it moves (CI and instance locked).
pub(in crate::modules::workflows) async fn close_for_migration(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    instances: &[Uuid],
) -> Result<Vec<AuditEntry>, AppError> {
    let pending: Vec<Uuid> = sqlx::query_scalar(
        "SELECT instance_id FROM cmdb.workflow_approval_requests
         WHERE instance_id = ANY($1) AND status = 'pending' ORDER BY instance_id",
    )
    .bind(instances)
    .fetch_all(&mut *conn)
    .await?;
    let mut out = Vec::with_capacity(pending.len());
    for id in pending {
        let row = instance(&mut *conn, id).await?.ok_or_else(AppError::internal)?;
        let closed = close_pending(&mut *conn, ctx, &row, WorkflowApprovalCloseReason::InstanceMigrated, None).await?;
        out.extend(closed.map(|c| c.entry));
    }
    Ok(out)
}

pub async fn withdraw(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    b: &WorkflowApprovalWithdraw,
) -> Result<WorkflowApprovalOutcome, AppError> {
    let mut tx = pool.begin().await?;
    let (req, row) = lock_request(&mut tx, ctx, id).await?;
    check_pending(&req)?;
    check_version(&req, b.expected_version)?;
    let me = ctx.principal().map(|p| p.user_id);
    if me.is_none() || me != req.requested_by_id {
        return Err(detail(
            ErrorCode::Forbidden,
            "id",
            "not_requester",
            "Only the requester can withdraw an approval request; a manager can cancel it".into(),
        ));
    }
    let comment = b.comment.as_deref().map(str::trim).filter(|c| !c.is_empty());
    let closed = close(
        &mut tx,
        ctx,
        &row,
        WorkflowApprovalStatus::Withdrawn,
        WorkflowApprovalCloseReason::Withdrawn,
        comment,
        "approval_withdraw",
    )
    .await?
    .ok_or_else(AppError::internal)?;
    bump_instance(&mut tx, row.id).await?;
    crud::write_audit(&mut tx, ctx, vec![closed.entry]).await?;
    let p = pinned(&mut tx, req.version_id).await?;
    let out = outcome(&mut tx, ctx, req.id, &p).await?;
    tx.commit().await?;
    Ok(out)
}

/// A manager (or a holder of the `_cancel` grant and the edit right) closes
/// the request; the instance stays where it is.
pub async fn cancel(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    b: &WorkflowApprovalCancel,
) -> Result<WorkflowApprovalOutcome, AppError> {
    let mut tx = pool.begin().await?;
    let (req, row) = lock_request(&mut tx, ctx, id).await?;
    check_pending(&req)?;
    check_version(&req, b.expected_version)?;
    if !may_manage(ctx)
        && !(may_edit(ctx, row.class_id) && granted(&mut tx, ctx, row.definition_id).await?.has(CANCEL_KEY))
    {
        return Err(forbidden(format!(
            "Cancelling an approval request of workflow {} needs the workflows.manage permission, or its _cancel grant \
             and the edit right on the CI's type",
            row.definition_key
        )));
    }
    let comment = b.comment.trim();
    let closed = close(
        &mut tx,
        ctx,
        &row,
        WorkflowApprovalStatus::Cancelled,
        WorkflowApprovalCloseReason::Withdrawn,
        Some(comment),
        "approval_close",
    )
    .await?
    .ok_or_else(AppError::internal)?;
    bump_instance(&mut tx, row.id).await?;
    crud::write_audit(&mut tx, ctx, vec![closed.entry]).await?;
    let p = pinned(&mut tx, req.version_id).await?;
    let out = outcome(&mut tx, ctx, req.id, &p).await?;
    tx.commit().await?;
    Ok(out)
}

/// Re-resolves the active step's approvers from the workflow's current
/// assignments and the CI's current values (`workflows.manage`), for example
/// after a CI field naming the approver changed. Decisions already cast stand.
/// Audited on the CI as `workflow.approval_refresh` with the approvers before
/// and after (GH#663).
pub async fn refresh(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<WorkflowApprovalRequest, AppError> {
    let mut tx = pool.begin().await?;
    let (req, row) = lock_request(&mut tx, ctx, id).await?;
    check_pending(&req)?;
    let p = pinned(&mut tx, req.version_id).await?;
    let t = p.transition(&req.transition_key).ok_or_else(AppError::internal)?;
    let (s, _) = active_step(&mut tx, &p, t, &req).await?;
    let model = Model::load(&mut tx).await?;
    let values = current_values(&mut tx, &model, row.ci_id).await?;
    let before = approvers_of(&mut tx, req.id, s.step_no).await?;
    resolve(&mut tx, req.id, &row, &values, &t.key, s, &req.excluded_user_ids).await?;
    let after = approvers_of(&mut tx, req.id, s.step_no).await?;
    let entry = AuditEntry {
        action: AuditAction::WorkflowApprovalRefresh,
        entity_type: "configuration_items",
        entity_id: row.ci_id,
        new_value: Some(json!({ "instanceId": row.id, "requestId": req.id, "requestNo": req.request_no,
            "transitionKey": req.transition_key, "stepNo": s.step_no, "stepKey": s.key, "changed": before != after,
            "approvers": after["approvers"], "eligibleCount": after["eligibleCount"],
            "droppedSources": after["droppedSources"] })),
        old_value: Some(before),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    let out = view(&mut tx, ctx, &req, &row, &p).await?;
    tx.commit().await?;
    Ok(out)
}

/// Who may decide step `step_no` of a request, as the refresh audit row
/// records it: the eligibility principals, the count of eligible users and
/// the sources dropped (GH#664).
async fn approvers_of(conn: &mut PgConnection, request: Uuid, step_no: i16) -> Result<Value, AppError> {
    let principals: Vec<(String, Uuid, SqlJson<Value>)> = sqlx::query_as(
        "SELECT principal_kind, principal_id, via FROM cmdb.workflow_approval_eligibility
         WHERE request_id = $1 AND step_no = $2 AND role = 'approver' ORDER BY principal_kind, principal_id",
    )
    .bind(request)
    .bind(step_no)
    .fetch_all(&mut *conn)
    .await?;
    let step: Option<(Option<i32>, SqlJson<Value>)> = sqlx::query_as(
        "SELECT eligible_count, dropped_sources FROM cmdb.workflow_approval_request_steps
         WHERE request_id = $1 AND step_no = $2",
    )
    .bind(request)
    .bind(step_no)
    .fetch_optional(&mut *conn)
    .await?;
    let (count, dropped) = step.map_or((None, Value::Null), |(c, d)| (c, d.0));
    let approvers: Vec<Value> =
        principals.into_iter().map(|(kind, id, via)| json!({ "kind": kind, "id": id, "via": via.0 })).collect();
    Ok(json!({ "approvers": approvers, "eligibleCount": count, "droppedSources": dropped }))
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

pub async fn get(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<WorkflowApprovalRequest, AppError> {
    let mut conn = pool.acquire().await?;
    let req = load(&mut conn, id, false).await?.ok_or_else(|| AppError::missing(REQUEST, id))?;
    let row = instance(&mut conn, req.instance_id).await?.ok_or_else(|| AppError::missing(REQUEST, id))?;
    ctx.require_class_visible(row.class_id, REQUEST, id)?;
    let p = pinned(&mut conn, req.version_id).await?;
    view(&mut conn, ctx, &req, &row, &p).await
}

async fn outcome(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    id: Uuid,
    p: &Pinned,
) -> Result<WorkflowApprovalOutcome, AppError> {
    let req = load(&mut *conn, id, false).await?.ok_or_else(AppError::internal)?;
    let row = instance(&mut *conn, req.instance_id).await?.ok_or_else(AppError::internal)?;
    let request = view(&mut *conn, ctx, &req, &row, p).await?;
    Ok(WorkflowApprovalOutcome { request, instance: row.dto() })
}

/// Whether the requester could still make the request (A-Q7.2): active, the
/// edit right on the CI's type, and a grant of the transition.
async fn requester(
    conn: &mut PgConnection,
    req: &RequestRow,
    row: &InstanceRow,
) -> Result<WorkflowApprovalRequester, AppError> {
    let Some(user) = req.requested_by_id else {
        return Ok(WorkflowApprovalRequester {
            id: None,
            name: req.requested_by_name.clone(),
            active: false,
            still_authorized: false,
        });
    };
    let active: bool = sqlx::query_scalar("SELECT coalesce((SELECT is_active FROM cmdb.users WHERE id = $1), false)")
        .bind(user)
        .fetch_one(&mut *conn)
        .await?;
    let can_edit = active
        && auth_data::load_permissions_of(&mut *conn, &[user])
            .await?
            .get(&user)
            .is_some_and(|p| p.can(row.class_id, ClassOp::Edit));
    let still_authorized = can_edit
        && sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM cmdb.user_permission_profiles up
                              JOIN cmdb.permission_profiles p ON p.id = up.profile_id
                            WHERE up.user_id = $2 AND p.is_builtin)
                 OR EXISTS (SELECT 1 FROM cmdb.workflow_transition_grants g
                              JOIN cmdb.user_permission_profiles up ON up.profile_id = g.profile_id
                            WHERE g.definition_id = $1 AND up.user_id = $2 AND g.transition_key = $3)",
        )
        .bind(row.definition_id)
        .bind(user)
        .bind(&req.transition_key)
        .fetch_one(&mut *conn)
        .await?;
    Ok(WorkflowApprovalRequester { id: Some(user), name: req.requested_by_name.clone(), active, still_authorized })
}

async fn view(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    req: &RequestRow,
    row: &InstanceRow,
    p: &Pinned,
) -> Result<WorkflowApprovalRequest, AppError> {
    let t = p.transition(&req.transition_key).ok_or_else(AppError::internal)?;
    let state_key = |id: Uuid| p.state(id).map(|s| s.key.clone()).unwrap_or_default();
    let states: Vec<StepState> = sqlx::query_as(
        "SELECT step_no, status, activated_at, due_at, overdue_at, completed_at, eligible_count, dropped_sources
         FROM cmdb.workflow_approval_request_steps WHERE request_id = $1 ORDER BY step_no",
    )
    .bind(req.id)
    .fetch_all(&mut *conn)
    .await?;
    let decisions: Vec<WorkflowApprovalDecision> = sqlx::query_as(
        "SELECT id, step_no, decision, actor_name, credential, on_behalf_of_name, comment, decided_at
         FROM cmdb.workflow_approval_decisions WHERE request_id = $1 ORDER BY id",
    )
    .bind(req.id)
    .fetch_all(&mut *conn)
    .await?;
    let steps = states
        .iter()
        .map(|st| {
            let pinned = p.steps_of(t.id).find(|s| s.step_no == st.step_no);
            let decided: Vec<WorkflowApprovalDecision> =
                decisions.iter().filter(|d| d.step_no == st.step_no).cloned().collect();
            let approvals = decided.iter().filter(|d| d.decision == WorkflowApprovalVerdict::Approve).count() as i64;
            let required = pinned.map_or(0, |s| s.required_approvals);
            WorkflowApprovalRequestStep {
                step_no: st.step_no,
                key: pinned.map(|s| s.key.clone()).unwrap_or_default(),
                name: pinned.map(|s| s.name.clone()).unwrap_or_default(),
                required_approvals: required,
                status: st.status,
                activated_at: st.activated_at,
                due_at: st.due_at,
                overdue: st.overdue_at.is_some(),
                completed_at: st.completed_at,
                approvals,
                eligible_count: st.eligible_count,
                understaffed: st.status == WorkflowApprovalStepStatus::Active
                    && st.eligible_count.is_some_and(|n| i64::from(n) < i64::from(required) - approvals),
                dropped_sources: st.dropped_sources.0.clone(),
                decisions: decided,
            }
        })
        .collect();
    let requester = requester(&mut *conn, req, row).await?;
    let my_eligibility = if req.status != WorkflowApprovalStatus::Pending {
        WorkflowApprovalEligibility {
            can_decide: false,
            reason: Some("not_pending".into()),
            message: Some("The request is closed".into()),
        }
    } else {
        let (s, overdue) = active_step(&mut *conn, p, t, req).await?;
        match check_decider(&mut *conn, ctx, req, row, s, overdue).await {
            Ok(_) => WorkflowApprovalEligibility { can_decide: true, reason: None, message: None },
            Err(e) if e.code.status().is_server_error() => return Err(e),
            Err(e) => WorkflowApprovalEligibility {
                can_decide: false,
                reason: e.details.as_ref().and_then(|d| d.first()).map(|d| d.code.clone()),
                message: Some(e.message),
            },
        }
    };
    let approvers = if may_manage(ctx) || my_eligibility.can_decide {
        let rows: Vec<(WorkflowApproverRole, WorkflowApprovalPrincipalKind, Uuid, SqlJson<Value>)> = sqlx::query_as(
            "SELECT role, principal_kind, principal_id, via FROM cmdb.workflow_approval_eligibility
             WHERE request_id = $1 AND step_no = $2 ORDER BY role, principal_kind, via->>'label', principal_id",
        )
        .bind(req.id)
        .bind(req.current_step_no)
        .fetch_all(&mut *conn)
        .await?;
        Some(
            rows.into_iter()
                .map(|(role, kind, id, via)| WorkflowApprovalPrincipal {
                    role,
                    kind,
                    id,
                    source: serde_json::from_value(via.0["source"].clone()).unwrap_or(WorkflowApproverSource::User),
                    label: via.0["label"].as_str().unwrap_or_default().to_owned(),
                    field_last_changed: serde_json::from_value(via.0["fieldLastChanged"].clone()).ok(),
                })
                .collect(),
        )
    } else {
        None
    };
    Ok(WorkflowApprovalRequest {
        id: req.id,
        instance_id: row.id,
        ci_id: row.ci_id,
        ci_ident: row.ci_ident.clone(),
        ci_label: row.ci_label.clone(),
        class_key: row.class_key.clone(),
        definition_key: row.definition_key.clone(),
        definition_name: row.definition_name.clone(),
        version_no: row.version_no,
        transition_key: t.key.clone(),
        transition_name: t.name.clone(),
        from_state: state_key(t.from_state_id),
        to_state: state_key(t.to_state_id),
        request_no: req.request_no,
        status: req.status,
        close_reason: req.close_reason,
        current_step_no: req.current_step_no,
        requested_at: req.requested_at,
        requester,
        comment: req.comment.clone(),
        staged_fields: Value::Object(req.staged_fields.0.clone()),
        steps,
        approvers,
        my_eligibility,
        closed_at: req.closed_at,
        closed_by_name: req.closed_by_name.clone(),
        version: req.version,
    })
}
