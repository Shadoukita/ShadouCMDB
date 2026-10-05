//! What the data model may not take away from a workflow (design §3.3): a
//! field a version depends on (`workflow_version_attribute_refs`, transition
//! fields, the definition's state field, a reference field that names
//! approvers, approvals design SHAA-1869 §3.1) and a lookup value a state maps to.
//! The schema-change paths ask here first and answer 409 IN_USE naming the
//! workflows and versions; the foreign keys (ON DELETE RESTRICT) are the
//! backstop.

use sqlx::PgConnection;
use uuid::Uuid;

use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};

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

/// "server_lifecycle v2 (published)", one per version or workflow that uses field `id`.
pub async fn attribute_users(conn: &mut PgConnection, id: Uuid, reach: Reach) -> Result<Vec<String>, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT u FROM (
           SELECT d.key || ' v' || w.version_no || ' (' || w.status || ')' AS u, d.key AS k, w.version_no AS n
           FROM cmdb.workflow_versions w JOIN cmdb.workflow_definitions d ON d.id = w.definition_id
           WHERE (EXISTS (SELECT 1 FROM cmdb.workflow_version_attribute_refs r
                          WHERE r.version_id = w.id AND r.attribute_id = $1)
                  OR EXISTS (SELECT 1 FROM cmdb.workflow_transition_fields f
                             JOIN cmdb.workflow_transitions t ON t.id = f.transition_id
                             WHERE t.version_id = w.id AND f.attribute_id = $1))
             AND ($2 OR w.status = 'published'
                  OR (w.status = 'retired' AND EXISTS (SELECT 1 FROM cmdb.workflow_instances i
                                                       WHERE i.version_id = w.id AND i.status = 'active')))
           UNION ALL
           SELECT d.key || ' (state field)', d.key, 0 FROM cmdb.workflow_definitions d WHERE d.state_attribute_id = $1
           UNION ALL
           SELECT DISTINCT d.key || ' (approvers of ' || a.transition_key || '.' || a.step_key || ')', d.key, 0
           FROM cmdb.workflow_approval_assignments a JOIN cmdb.workflow_definitions d ON d.id = a.definition_id
           WHERE a.attribute_id = $1
         ) x ORDER BY k, n, u",
    )
    .bind(id)
    .bind(reach == Reach::All)
    .fetch_all(&mut *conn)
    .await?)
}

/// 409 IN_USE when a workflow depends on field `id`, or Ok.
pub async fn check_attribute(
    conn: &mut PgConnection,
    id: Uuid,
    key: &str,
    reach: Reach,
    what: &str,
) -> Result<(), AppError> {
    let users = attribute_users(conn, id, reach).await?;
    if users.is_empty() {
        return Ok(());
    }
    let message = format!(
        "Field {key} cannot be {what}: workflows depend on it ({}). Publish versions without it and retire the old \
         ones, change the workflows' state field, or remove it from their approvers, first.",
        users.join(", ")
    );
    Err(AppError::new(ErrorCode::InUse, message).with_details(
        users
            .into_iter()
            .map(|u| FieldError {
                location: FieldLocation::Params,
                field: "id".into(),
                message: format!("Used by workflow {u}"),
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
