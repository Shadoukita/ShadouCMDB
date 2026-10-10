//! What the data model may not take away from a workflow (design §3.3): a
//! field a version depends on (`workflow_version_attribute_refs`, transition
//! fields, attribute actions (actions design SHAA-2725 §3.4), the definition's
//! state field, a reference field that names approvers, approvals design
//! SHAA-1869 §3.1) and a lookup value a state maps to.
//! The schema-change paths ask here first and answer 409 IN_USE naming the
//! workflows and versions the caller may read; the foreign keys (ON DELETE
//! RESTRICT) are the backstop.

use sqlx::PgConnection;
use uuid::Uuid;

use super::service::coverage;
use crate::api::context::RequestContext;
use crate::auth::permissions::GlobalPermission;
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::schema::model::Model;

/// Which references count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// Archiving or retyping a field: published versions, retired versions
    /// that instances still run on, and workflows that drive it as their
    /// state field.
    Live,
    /// Purging a field: every version that names it, drafts included (their
    /// rows hold it by foreign key), and every workflow with it as state field.
    All,
}

/// A version or workflow that uses a field: "server_lifecycle v2 (published)",
/// and the types its workflow covers.
#[derive(Debug, sqlx::FromRow)]
pub struct AttributeUser {
    pub label: String,
    pub class_id: Uuid,
    pub include_subclasses: bool,
}

/// One per version or workflow that uses field `id`.
pub async fn attribute_users(conn: &mut PgConnection, id: Uuid, reach: Reach) -> Result<Vec<AttributeUser>, AppError> {
    Ok(sqlx::query_as(
        "SELECT u AS label, class_id, include_subclasses FROM (
           SELECT d.key || ' v' || w.version_no || ' (' || w.status || ')' AS u, d.key AS k, w.version_no AS n,
                  d.class_id, d.include_subclasses
           FROM cmdb.workflow_versions w JOIN cmdb.workflow_definitions d ON d.id = w.definition_id
           WHERE (EXISTS (SELECT 1 FROM cmdb.workflow_version_attribute_refs r
                          WHERE r.version_id = w.id AND r.attribute_id = $1)
                  OR EXISTS (SELECT 1 FROM cmdb.workflow_transition_fields f
                             JOIN cmdb.workflow_transitions t ON t.id = f.transition_id
                             WHERE t.version_id = w.id AND f.attribute_id = $1)
                  OR EXISTS (SELECT 1 FROM cmdb.workflow_transition_set_attributes a
                             JOIN cmdb.workflow_transitions t ON t.id = a.transition_id
                             WHERE t.version_id = w.id AND a.attribute_id = $1))
             AND ($2 OR w.status = 'published'
                  OR (w.status = 'retired' AND EXISTS (SELECT 1 FROM cmdb.workflow_instances i
                                                       WHERE i.version_id = w.id AND i.status = 'active')))
           UNION ALL
           SELECT d.key || ' (state field)', d.key, 0, d.class_id, d.include_subclasses
           FROM cmdb.workflow_definitions d WHERE d.state_attribute_id = $1
           UNION ALL
           SELECT DISTINCT d.key || ' (approvers of ' || a.transition_key || '.' || a.step_key || ')', d.key, 0,
                  d.class_id, d.include_subclasses
           FROM cmdb.workflow_approval_assignments a JOIN cmdb.workflow_definitions d ON d.id = a.definition_id
           WHERE a.attribute_id = $1
         ) x ORDER BY k, n, u",
    )
    .bind(id)
    .bind(reach == Reach::All)
    .fetch_all(&mut *conn)
    .await?)
}

/// 409 IN_USE when a workflow depends on field `id`, or Ok. GH#746: the
/// refusal is unconditional, but it names a workflow only to a caller who may
/// read it (`workflows.manage` and the view right on every type it covers,
/// GH#667, GH#718); the others are one entry that names none of them.
pub async fn check_attribute(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    id: Uuid,
    key: &str,
    reach: Reach,
    what: &str,
) -> Result<(), AppError> {
    let users = attribute_users(conn, id, reach).await?;
    if users.is_empty() {
        return Ok(());
    }
    let manager = ctx.require(GlobalPermission::WorkflowsManage).is_ok();
    let model = Model::load(&mut *conn).await?;
    let (named, hidden): (Vec<_>, Vec<_>) = users
        .into_iter()
        .partition(|u| manager && ctx.may_view_all(&coverage(&model, u.class_id, u.include_subclasses)));
    let names: Vec<String> = named.into_iter().map(|u| u.label).collect();
    let message = if names.is_empty() {
        format!(
            "Field {key} cannot be {what}: workflows on CI types you may not view depend on it; ask an administrator."
        )
    } else {
        let others = if hidden.is_empty() { "" } else { ", and workflows on CI types you may not view" };
        format!(
            "Field {key} cannot be {what}: workflows depend on it ({}{others}). Publish versions without it and \
             retire the old ones, change the workflows' state field, or remove it from their approvers, first.",
            names.join(", ")
        )
    };
    let mut details: Vec<String> = names.iter().map(|u| format!("Used by workflow {u}")).collect();
    if !hidden.is_empty() {
        details.push("Used by workflows on CI types you may not view".to_owned());
    }
    Err(AppError::new(ErrorCode::InUse, message).with_details(
        details
            .into_iter()
            .map(|message| FieldError {
                location: FieldLocation::Params,
                field: "id".into(),
                message,
                code: "workflow_reference".into(),
            })
            .collect(),
    ))
}

/// [`crate::modules::simple_resource::Usage::sql`] of the workflow states that map to lookup value `$1` (any
/// version: their rows hold it by foreign key).
pub const LOOKUP_VALUE_STATES: &str = "SELECT count(*) FROM cmdb.workflow_states WHERE state_value_id = $1";

/// [`crate::modules::simple_resource::Usage::sql`] of the workflow definitions on type `$1`.
pub const CLASS_DEFINITIONS: &str = "SELECT count(*) FROM cmdb.workflow_definitions WHERE class_id = $1";
