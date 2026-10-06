//! Lists of approval requests (approvals design SHAA-1869 §4.2, §10.2; slice
//! A3b, SHAA-1880): the inbox and its other views, and the request history of
//! one instance.
//!
//! **Visibility.** Requests on CIs of types the caller may not view are left
//! out in SQL, before counting, so neither the page nor `page.total` reveals
//! them (the GH#264 rule).
//!
//! **The inbox** (`view=actionable`) is the SQL form of `check_decider`: the
//! pending requests whose active step the caller may decide in person now.
//! Both read the same eligibility rows and the same `excludeActorsOf` set, so
//! a request listed here is one `myEligibility.canDecide` says yes to.

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::super::approval_schemas::*;
use super::approvals::actors_of;
use super::visible_instance;
use crate::api::context::RequestContext;
use crate::api::schemas::{Page, Paged, Sort};
use crate::auth::Credential;
use crate::auth::permissions::ClassOp;
use crate::data::crud::{self, Where};

use crate::http::error::AppError;

/// The requests with the active (or last reached) step and the instance's CI:
/// every filter and sort below is a condition on these.
const FROM: &str = "cmdb.workflow_approval_requests r \
     JOIN cmdb.workflow_approval_request_steps st ON st.request_id = r.id AND st.step_no = r.current_step_no \
     JOIN cmdb.workflow_instances wi ON wi.id = r.instance_id \
     JOIN cmdb.configuration_items ci ON ci.id = wi.ci_id";

/// The page's rows, by id.
const ITEM_SQL: &str = "SELECT r.id, r.instance_id, wi.ci_id, ci.ident AS ci_ident, ci.label AS ci_label, \
       c.key AS class_key, d.key AS definition_key, d.name AS definition_name, v.version_no, r.transition_key, \
       tr.name AS transition_name, fs.key AS from_state, ts.key AS to_state, r.request_no, r.status, r.close_reason, \
       r.requested_at, r.requested_by_id, r.requested_by_name, st.step_no, st.step_key, \
       coalesce(ps.name, st.step_key) AS step_name, st.status AS step_status, st.required_approvals, st.due_at, \
       st.overdue_at IS NOT NULL AS overdue, \
       (SELECT count(*) FROM cmdb.workflow_approval_decisions dc \
        WHERE dc.request_id = r.id AND dc.step_no = st.step_no AND dc.decision = 'approve') AS approvals, \
       (SELECT count(*)::smallint FROM cmdb.workflow_approval_request_steps n WHERE n.request_id = r.id) \
         AS step_count, \
       r.closed_at, r.closed_by_name, r.version \
     FROM cmdb.workflow_approval_requests r \
     JOIN cmdb.workflow_approval_request_steps st ON st.request_id = r.id AND st.step_no = r.current_step_no \
     JOIN cmdb.workflow_instances wi ON wi.id = r.instance_id \
     JOIN cmdb.configuration_items ci ON ci.id = wi.ci_id \
     JOIN cmdb.ci_classes c ON c.id = ci.class_id \
     JOIN cmdb.workflow_definitions d ON d.id = wi.definition_id \
     JOIN cmdb.workflow_versions v ON v.id = r.version_id \
     JOIN cmdb.workflow_transitions tr ON tr.version_id = r.version_id AND tr.key = r.transition_key \
     JOIN cmdb.workflow_states fs ON fs.id = tr.from_state_id \
     JOIN cmdb.workflow_states ts ON ts.id = tr.to_state_id \
     LEFT JOIN cmdb.workflow_transition_approval_steps ps \
       ON ps.transition_id = tr.id AND ps.step_no = r.current_step_no \
     WHERE r.id = ANY($1)";

#[derive(Debug, sqlx::FromRow)]
struct ItemRow {
    id: Uuid,
    instance_id: Uuid,
    ci_id: Uuid,
    ci_ident: String,
    ci_label: String,
    class_key: String,
    definition_key: String,
    definition_name: String,
    version_no: i32,
    transition_key: String,
    transition_name: String,
    from_state: String,
    to_state: String,
    request_no: i32,
    status: WorkflowApprovalStatus,
    close_reason: Option<WorkflowApprovalCloseReason>,
    requested_at: DateTime<Utc>,
    requested_by_id: Option<Uuid>,
    requested_by_name: String,
    step_no: i16,
    step_key: String,
    step_name: String,
    step_status: WorkflowApprovalStepStatus,
    required_approvals: i16,
    due_at: Option<DateTime<Utc>>,
    overdue: bool,
    approvals: i64,
    step_count: i16,
    closed_at: Option<DateTime<Utc>>,
    closed_by_name: Option<String>,
    version: i32,
}

impl ItemRow {
    fn dto(self) -> WorkflowApprovalRequestItem {
        WorkflowApprovalRequestItem {
            id: self.id,
            instance_id: self.instance_id,
            ci_id: self.ci_id,
            ci_ident: self.ci_ident,
            ci_label: self.ci_label,
            class_key: self.class_key,
            definition_key: self.definition_key,
            definition_name: self.definition_name,
            version_no: self.version_no,
            transition_key: self.transition_key,
            transition_name: self.transition_name,
            from_state: self.from_state,
            to_state: self.to_state,
            request_no: self.request_no,
            status: self.status,
            close_reason: self.close_reason,
            requested_at: self.requested_at,
            requested_by: WorkflowApprovalRequestedBy { id: self.requested_by_id, name: self.requested_by_name },
            current_step: WorkflowApprovalCurrentStep {
                step_no: self.step_no,
                key: self.step_key,
                name: self.step_name,
                status: self.step_status,
                approvals: self.approvals,
                required_approvals: self.required_approvals,
                due_at: self.due_at,
                overdue: self.overdue,
            },
            step_count: self.step_count,
            closed_at: self.closed_at,
            closed_by_name: self.closed_by_name,
            version: self.version,
        }
    }
}

/// The rows of `ids`, in `order` (an ORDER BY over `r` and `st`).
async fn items(
    conn: &mut PgConnection,
    ids: &[Uuid],
    order: &str,
) -> Result<Vec<WorkflowApprovalRequestItem>, AppError> {
    let rows: Vec<ItemRow> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("{ITEM_SQL} ORDER BY {order}"))).bind(ids).fetch_all(conn).await?;
    Ok(rows.into_iter().map(ItemRow::dto).collect())
}

fn order(sort: &Sort) -> String {
    let dir = sort.dir();
    match sort.field.as_str() {
        "requestedAt" => format!("r.requested_at {dir}, r.id {dir}"),
        "closedAt" => format!("r.closed_at {dir} NULLS LAST, r.id {dir}"),
        _ => format!("st.due_at {dir} NULLS LAST, r.requested_at {dir}, r.id {dir}"),
    }
}

/// Who the caller is, for the views about them; None for a caller that is
/// not a user, who has no inbox.
struct Me {
    id: Uuid,
    /// Through an API token: the step must allow tokens, and only a token
    /// its owner minted decides (SHAA-1872 C1).
    token: bool,
    self_minted: bool,
}

fn me(ctx: &RequestContext) -> Option<Me> {
    let p = ctx.principal()?;
    let (token, self_minted) = match p.credential {
        Credential::Token { minted_by, .. } => (true, minted_by == Some(p.user_id)),
        Credential::Session { .. } => (false, false),
    };
    Some(Me { id: p.user_id, token, self_minted })
}

/// `view=actionable`: what `check_decider` lets the caller decide in person,
/// condition by condition (§4.2, §4.3).
///
/// The conditions only read the request tables, so they are evaluated in a
/// subquery that starts from the caller's eligibility rows; the `OFFSET 0`
/// keeps the planner from merging it, so the instance and CI (for the class
/// scope) are looked up only for the requests the caller may decide.
fn actionable(w: &mut Where<'_>, me: &Me) {
    if me.token && !me.self_minted {
        w.and().push("false");
        return;
    }
    // A principal of the active step: the user, one of their profiles or groups.
    let qb = w.and();
    qb.push(
        "r.id IN (SELECT r.id FROM cmdb.workflow_approval_eligibility e \
         JOIN cmdb.workflow_approval_requests r ON r.id = e.request_id AND r.current_step_no = e.step_no \
         JOIN cmdb.workflow_approval_request_steps st ON st.request_id = r.id AND st.step_no = r.current_step_no \
         WHERE r.status = 'pending' AND st.status = 'active' \
           AND (e.role = 'approver' OR st.overdue_at IS NOT NULL) \
           AND ((e.principal_kind = 'user' AND e.principal_id = ",
    )
    .push_bind(me.id)
    .push(
        ") OR (e.principal_kind = 'profile' AND e.principal_id IN \
                (SELECT profile_id FROM cmdb.user_permission_profiles WHERE user_id = ",
    )
    .push_bind(me.id)
    .push(
        ")) OR (e.principal_kind = 'group' AND e.principal_id IN \
                (SELECT group_id FROM cmdb.user_group_members WHERE user_id = ",
    )
    .push_bind(me.id)
    .push(")))");
    // Four-eyes: never the requester or the requesting token's creator.
    qb.push(" AND NOT (").push_bind(me.id).push(" = ANY(r.excluded_user_ids))");
    // Not decided by them yet.
    qb.push(
        " AND NOT EXISTS (SELECT 1 FROM cmdb.workflow_approval_decisions dd \
         WHERE dd.request_id = r.id AND dd.step_no = r.current_step_no AND (dd.actor_id = ",
    )
    .push_bind(me.id)
    .push(" OR dd.on_behalf_of_id = ")
    .push_bind(me.id)
    .push("))");
    // The step's policy, from the pinned version.
    qb.push(
        " AND EXISTS (SELECT 1 FROM cmdb.workflow_transitions tr \
         JOIN cmdb.workflow_transition_approval_steps ps ON ps.transition_id = tr.id AND ps.step_no = r.current_step_no \
         WHERE tr.version_id = r.version_id AND tr.key = r.transition_key",
    );
    if me.token {
        qb.push(" AND ps.allow_api_tokens");
    }
    qb.push(
        " AND (NOT ps.distinct_from_earlier OR r.current_step_no = 1 OR NOT EXISTS ( \
           SELECT 1 FROM cmdb.workflow_approval_decisions de \
           WHERE de.request_id = r.id AND de.step_no < r.current_step_no AND de.decision = 'approve' \
             AND (de.actor_id = ",
    )
    .push_bind(me.id)
    .push(" OR de.on_behalf_of_id = ")
    .push_bind(me.id)
    .push(format!(
        "))) AND (cardinality(ps.exclude_actors_of) = 0 OR NOT EXISTS ( \
           SELECT 1 FROM ({}) AS a(key, id) WHERE a.id = ",
        actors_of("r.instance_id", "ps.exclude_actors_of")
    ))
    .push_bind(me.id)
    .push("))) OFFSET 0)");
}

pub async fn list(
    pool: &PgPool,
    ctx: &RequestContext,
    q: &WorkflowApprovalRequestList,
) -> Result<Page<WorkflowApprovalRequestItem>, AppError> {
    let scope = ctx.class_scope(ClassOp::View);
    let me = me(ctx);
    let filter = |w: &mut Where<'_>| {
        if let Some(classes) = &scope {
            w.and().push("ci.class_id = ANY(").push_bind(classes.clone()).push(")");
        }
        match (q.view, &me) {
            (WorkflowApprovalView::All, _) => {}
            (_, None) => {
                w.and().push("false");
            }
            (WorkflowApprovalView::Actionable, Some(me)) => actionable(w, me),
            (WorkflowApprovalView::Requested, Some(me)) => {
                w.and().push("r.requested_by_id = ").push_bind(me.id);
            }
            (WorkflowApprovalView::Decided, Some(me)) => {
                w.and()
                    .push("r.id IN (SELECT request_id FROM cmdb.workflow_approval_decisions WHERE actor_id = ")
                    .push_bind(me.id)
                    .push(" UNION SELECT request_id FROM cmdb.workflow_approval_decisions WHERE on_behalf_of_id = ")
                    .push_bind(me.id)
                    .push(")");
            }
        }
        if let Some(st) = q.status {
            w.and().push("r.status = ").push_bind(st);
        }
        if let Some(k) = &q.definition_key {
            w.and()
                .push("wi.definition_id IN (SELECT id FROM cmdb.workflow_definitions WHERE lower(key) = lower(")
                .push_bind(k.clone())
                .push("))");
        }
        if let Some(ci) = q.ci_id {
            w.and().push("wi.ci_id = ").push_bind(ci);
        }
        if let Some(user) = q.requested_by {
            w.and().push("r.requested_by_id = ").push_bind(user);
        }
        if let Some(overdue) = q.overdue.map(bool::from) {
            w.and().push(if overdue {
                "(st.status = 'active' AND st.overdue_at IS NOT NULL)"
            } else {
                "NOT (st.status = 'active' AND st.overdue_at IS NOT NULL)"
            });
        }
    };
    let order = order(&q.sort);
    let mut conn = pool.acquire().await?;
    let (ids, total) = crud::select_ids_counted(&mut conn, FROM, "r.id", &filter, &order, q.limit, q.offset).await?;
    let data = items(&mut conn, &ids, &order).await?;
    Ok(Page { data, page: q.page_meta(total) })
}

/// The approval requests of one instance, newest first.
pub async fn of_instance(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    q: &WorkflowApprovalHistoryList,
) -> Result<Page<WorkflowApprovalRequestItem>, AppError> {
    let mut conn = pool.acquire().await?;
    visible_instance(&mut conn, ctx, id).await?;
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM cmdb.workflow_approval_requests WHERE instance_id = $1")
        .bind(id)
        .fetch_one(&mut *conn)
        .await?;
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM cmdb.workflow_approval_requests WHERE instance_id = $1
         ORDER BY request_no DESC LIMIT $2 OFFSET $3",
    )
    .bind(id)
    .bind(q.limit)
    .bind(q.offset)
    .fetch_all(&mut *conn)
    .await?;
    let data = items(&mut conn, &ids, "r.request_no DESC").await?;
    Ok(Page { data, page: q.page_meta(total) })
}
