//! The workflow history of CIs deleted for good (v0.4.0 slice S6, SHAA-1427;
//! design on SHAA-1411 §3.2, §8.2, Q2).
//!
//! Deleting a CI row (only a type purge does) moves its instances, each with
//! all of its events, into `cmdb.workflow_instance_archive` in the same
//! transaction (the trigger of migration 0050). The archive is append-only
//! and outside audit retention. Reading it needs `workflows.manage`; like
//! audit entries of a CI that no longer exists, it is shown only to a caller
//! whose profile does not limit the types they may view.

use sqlx::PgPool;

use super::runtime_schemas::*;
use crate::api::context::RequestContext;
use crate::api::schemas::{Page, Paged};
use crate::auth::permissions::ClassOp;
use crate::data::crud::{self, Where};

const COLUMNS: &str = "instance_id, ci_id, ci_ident, ci_label, class_key, definition_id, definition_key, version_no, \
     state_key, status, started_at, started_by_name, last_transition_at, ended_at, events, archived_at, request_id";

pub async fn list(
    pool: &PgPool,
    ctx: &RequestContext,
    q: &WorkflowArchiveList,
) -> Result<Page<WorkflowArchivedInstance>, crate::http::error::AppError> {
    // A purged CI has no type left to judge the view right by.
    let restricted = ctx.class_scope(ClassOp::View).is_some();
    let filter = |w: &mut Where<'_>| {
        if restricted {
            w.and().push("false");
        }
        if let Some(ci) = q.ci_id {
            w.and().push("ci_id = ").push_bind(ci);
        }
        if let Some(k) = &q.definition_key {
            w.and().push("lower(definition_key) = lower(").push_bind(k.clone()).push(")");
        }
    };
    let mut conn = pool.acquire().await?;
    let (data, total) = crud::select_page_counted::<WorkflowArchivedInstance>(
        &mut conn,
        "cmdb.workflow_instance_archive",
        "cmdb.workflow_instance_archive",
        COLUMNS,
        &filter,
        "archived_at DESC, instance_id DESC",
        q.limit,
        q.offset,
    )
    .await?;
    Ok(Page { data, page: q.page_meta(total) })
}
