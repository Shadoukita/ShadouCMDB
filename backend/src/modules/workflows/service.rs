//! Design time: definitions, drafts, publishing, retiring and grants.
//!
//! Every change locks the definition row first, so two administrators saving
//! the same workflow serialise. Drafts are scratch and not audited (§5.1);
//! creating, changing and deleting a definition, publishing (with the whole
//! graph), retiring and grant changes are.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::graph::{self, Fields, LintContext, Stored, VERSION_COLUMNS, VersionRow};
use super::schemas::*;
use crate::api::context::{Count, RequestContext};
use crate::api::schemas::{Page, Paged, like_pattern};
use crate::api::validate;
use crate::data::crud::{self, AuditAction, AuditEntry, Val, Where};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::modules::classes::AttributeDataType;
use crate::schema::model::Model;

const TABLE: &str = "workflow_definitions";
const LABEL: &str = "Workflow definition";

const COLUMNS: &str = "d.id, d.key, d.name, d.description, d.class_id, c.key AS class_key, d.include_subclasses, \
     d.state_attribute_id, a.key AS state_attribute_key, d.auto_start, d.is_active, \
     cv.version_no AS current_version_no, \
     (SELECT w.version_no FROM cmdb.workflow_versions w WHERE w.definition_id = d.id AND w.status = 'draft') \
       AS draft_version_no, \
     d.version, d.created_at, d.created_by_name, d.updated_at, d.updated_by_name";
const FROM: &str = "cmdb.workflow_definitions d JOIN cmdb.ci_classes c ON c.id = d.class_id \
     LEFT JOIN cmdb.ci_attribute_definitions a ON a.id = d.state_attribute_id \
     LEFT JOIN cmdb.workflow_versions cv ON cv.id = d.current_version_id";

/// Who is acting, for the `*_by` columns.
fn actor(ctx: &RequestContext) -> (Option<Uuid>, String) {
    let id = ctx.principal().map(|p| p.user_id);
    let name = ctx.principal().map(|p| p.username.clone()).or_else(|| ctx.actor.name.clone()).unwrap_or_default();
    (id, name)
}

async fn load(conn: &mut PgConnection, id: Uuid, for_update: bool) -> Result<WorkflowDefinition, AppError> {
    let lock = if for_update { " FOR UPDATE OF d" } else { "" };
    sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {COLUMNS} FROM {FROM} WHERE d.id = $1{lock}")))
        .bind(id)
        .fetch_optional(conn)
        .await?
        .ok_or_else(|| AppError::missing(LABEL, id))
}

fn stale(what: &str, field: &str, sent: &str, current: &str) -> AppError {
    AppError::new(
        ErrorCode::VersionConflict,
        format!("The {what} was changed by someone else (you sent {sent}, current is {current}). Reload and retry."),
    )
    .with_details(vec![FieldError {
        location: FieldLocation::Body,
        field: field.into(),
        message: format!("Current is {current}"),
        code: "stale".into(),
    }])
}

fn check_version(sent: i32, current: i32) -> Result<(), AppError> {
    if sent == current {
        return Ok(());
    }
    Err(stale("workflow", "version", &format!("version {sent}"), &format!("version {current}")))
}

async fn version_row(
    conn: &mut PgConnection,
    definition: Uuid,
    filter: &str,
    no: Option<i32>,
    for_update: bool,
) -> Result<Option<VersionRow>, AppError> {
    let lock = if for_update { " FOR UPDATE" } else { "" };
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {VERSION_COLUMNS} FROM cmdb.workflow_versions WHERE definition_id = $1 AND {filter}{lock}"
    )))
    .bind(definition)
    .bind(no)
    .fetch_optional(conn)
    .await?)
}

async fn draft_row(
    conn: &mut PgConnection,
    definition: Uuid,
    for_update: bool,
) -> Result<Option<VersionRow>, AppError> {
    version_row(conn, definition, "status = 'draft' AND $2::int IS NULL", None, for_update).await
}

fn no_draft(id: Uuid) -> AppError {
    AppError::new(ErrorCode::NotFound, format!("Workflow definition {id} has no draft"))
}

/// The classes a definition covers: its type, and the types below it when it includes subtypes.
fn covered(model: &Model, d: &WorkflowDefinition) -> Vec<Uuid> {
    if d.include_subclasses { model.subtree(d.class_id) } else { vec![d.class_id] }
}

async fn grant_rows(conn: &mut PgConnection, definition: Uuid) -> Result<Vec<WorkflowGrant>, AppError> {
    let rows: Vec<(String, Uuid, String)> = sqlx::query_as(
        "SELECT g.transition_key, p.id, p.name FROM cmdb.workflow_transition_grants g
         JOIN cmdb.permission_profiles p ON p.id = g.profile_id
         WHERE g.definition_id = $1 ORDER BY g.transition_key, lower(p.name)",
    )
    .bind(definition)
    .fetch_all(&mut *conn)
    .await?;
    let mut grants: BTreeMap<String, Vec<WorkflowGrantProfile>> = BTreeMap::new();
    for (key, id, name) in rows {
        grants.entry(key).or_default().push(WorkflowGrantProfile { id, name });
    }
    Ok(grants.into_iter().map(|(transition_key, profiles)| WorkflowGrant { transition_key, profiles }).collect())
}

/// `{transitionKey: [profile names]}` for the audit log.
fn grants_by_name(grants: &[WorkflowGrant]) -> Value {
    Value::Object(
        grants
            .iter()
            .map(|g| (g.transition_key.clone(), json!(g.profiles.iter().map(|p| &p.name).collect::<Vec<_>>())))
            .collect(),
    )
}

/// The definition as the audit log records it: its settings and grants.
async fn audit_value(conn: &mut PgConnection, d: &WorkflowDefinition) -> Result<Value, AppError> {
    let mut v = crud::json(d);
    v["grants"] = grants_by_name(&grant_rows(conn, d.id).await?);
    Ok(v)
}

async fn detail(
    conn: &mut PgConnection,
    d: WorkflowDefinition,
    warnings: Vec<WorkflowWarning>,
) -> Result<WorkflowDefinitionDetail, AppError> {
    let checksum = draft_row(conn, d.id, false).await?.and_then(|v| v.checksum).map(hex::encode);
    Ok(WorkflowDefinitionDetail::new(d, checksum, warnings))
}

/// Amendment 2 of the design review: activating a workflow that drives a state
/// field makes that field read-only on every CI it covers (Q3). Tell how many
/// live CIs have no running instance yet, so their status is not locked by
/// surprise (`bootstrap` starts them).
async fn activation_warnings(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    d: &WorkflowDefinition,
) -> Result<Vec<WorkflowWarning>, AppError> {
    if !d.is_active || d.state_attribute_id.is_none() {
        return Ok(Vec::new());
    }
    let model = Model::load(conn).await?;
    let classes = covered(&model, d);
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM cmdb.configuration_items ci
         WHERE ci.class_id = ANY($1) AND ci.deleted_at IS NULL
           AND NOT EXISTS (SELECT 1 FROM cmdb.workflow_instances wi
                           WHERE wi.ci_id = ci.id AND wi.definition_id = $2 AND wi.status = 'active')",
    )
    .bind(&classes)
    .bind(d.id)
    .fetch_one(&mut *conn)
    .await?;
    if n == 0 {
        return Ok(Vec::new());
    }
    let count = Count::scoped(ctx, &classes, n);
    let field = d.state_attribute_key.as_deref().unwrap_or_default();
    let message = match count.exact() {
        Some(n) => format!(
            "{n} live CIs of type {} have no running instance of this workflow: their {field} field cannot be \
             edited until an instance is started on them",
            d.class_key
        ),
        None => format!(
            "Live CIs of type {} have no running instance of this workflow: their {field} field cannot be edited \
             until an instance is started on them",
            d.class_key
        ),
    };
    Ok(vec![WorkflowWarning { code: WorkflowWarningCode::UninstancedCis, count, message }])
}

/// A state field must be an active lookup field of the type (own or inherited).
fn check_state_attribute(fields: &Fields, id: Uuid) -> Result<(), AppError> {
    let problem = match fields.model.field(id) {
        None => "No such field",
        Some(f) if !fields.on_type(f) => "Not a field of the workflow's type (own or inherited)",
        Some(f) if f.data_type != AttributeDataType::Lookup => "The state field must be a lookup field",
        Some(f) if !f.is_active => "The field is archived",
        Some(_) => return Ok(()),
    };
    Err(AppError::field("stateAttributeId", problem, "invalid_state_attribute"))
}

/// One active workflow per state field (the partial unique index); answered
/// first with the name of the other workflow.
async fn check_state_driver(conn: &mut PgConnection, d_id: Option<Uuid>, attribute: Uuid) -> Result<(), AppError> {
    let other: Option<String> = sqlx::query_scalar(
        "SELECT key FROM cmdb.workflow_definitions
         WHERE state_attribute_id = $1 AND is_active AND id IS DISTINCT FROM $2",
    )
    .bind(attribute)
    .bind(d_id)
    .fetch_optional(&mut *conn)
    .await?;
    match other {
        None => Ok(()),
        Some(key) => {
            let message = format!("The active workflow {key} already drives this field; deactivate it first");
            Err(AppError::new(ErrorCode::Conflict, message.clone()).with_details(vec![FieldError {
                location: FieldLocation::Body,
                field: "stateAttributeId".into(),
                message,
                code: "state_attribute_driven".into(),
            }]))
        }
    }
}

// ---------------------------------------------------------------------------
// Definitions
// ---------------------------------------------------------------------------

pub async fn list(pool: &PgPool, q: &WorkflowDefinitionList) -> Result<Page<WorkflowDefinition>, AppError> {
    let filter = |w: &mut Where<'_>| {
        if let Some(text) = &q.q {
            let p = like_pattern(text);
            w.and()
                .push("(d.key ILIKE ")
                .push_bind(p.clone())
                .push(" OR d.name ILIKE ")
                .push_bind(p.clone())
                .push(" OR d.description ILIKE ")
                .push_bind(p)
                .push(")");
        }
        if let Some(class) = &q.class_key {
            w.and().push("c.key = ").push_bind(class.clone());
        }
        if let Some(active) = q.active {
            w.and().push("d.is_active = ").push_bind(bool::from(active));
        }
    };
    let column = match q.sort.field.as_str() {
        "key" => "d.key",
        "createdAt" => "d.created_at",
        "updatedAt" => "d.updated_at",
        _ => "lower(d.name)",
    };
    let order = format!("{column} {}, d.key, d.id", q.sort.dir());
    let (data, total) = crud::select_page_counted::<WorkflowDefinition>(
        &mut *pool.acquire().await?,
        FROM,
        FROM,
        COLUMNS,
        &filter,
        &order,
        q.limit,
        q.offset,
    )
    .await?;
    Ok(Page { data, page: q.page_meta(total) })
}

pub async fn get(pool: &PgPool, id: Uuid) -> Result<WorkflowDefinitionDetail, AppError> {
    let mut conn = pool.acquire().await?;
    let d = load(&mut conn, id, false).await?;
    detail(&mut conn, d, Vec::new()).await
}

/// Stores an empty draft as version `no` and returns its id.
async fn new_draft(conn: &mut PgConnection, definition: Uuid, no: i32) -> Result<Uuid, AppError> {
    let empty = graph::checksum(&None, &[], &[]);
    Ok(sqlx::query_scalar(
        "INSERT INTO cmdb.workflow_versions (definition_id, version_no, status, checksum)
         VALUES ($1, $2, 'draft', $3) RETURNING id",
    )
    .bind(definition)
    .bind(no)
    .bind(hex::decode(empty).unwrap_or_default())
    .fetch_one(&mut *conn)
    .await?)
}

pub async fn create(
    pool: &PgPool,
    ctx: &RequestContext,
    b: &WorkflowDefinitionCreate,
) -> Result<WorkflowDefinitionDetail, AppError> {
    let mut tx = pool.begin().await?;
    let class: Option<Uuid> = sqlx::query_scalar("SELECT id FROM cmdb.ci_classes WHERE id = $1")
        .bind(b.class_id)
        .fetch_optional(&mut *tx)
        .await?;
    if class.is_none() {
        return Err(AppError::field("classId", "Type does not exist", "not_found"));
    }
    let taken: Option<String> =
        sqlx::query_scalar("SELECT key FROM cmdb.workflow_definitions WHERE lower(key) = lower($1)")
            .bind(&b.key)
            .fetch_optional(&mut *tx)
            .await?;
    if taken.is_some() {
        let message = format!("Another workflow already has the key {}", b.key);
        return Err(AppError::new(ErrorCode::Conflict, message.clone()).with_details(vec![FieldError {
            location: FieldLocation::Body,
            field: "key".into(),
            message,
            code: "unique".into(),
        }]));
    }
    let is_active = b.is_active.unwrap_or(false);
    if let Some(attribute) = b.state_attribute_id {
        let fields = Fields::load(&mut tx, b.class_id).await?;
        check_state_attribute(&fields, attribute)?;
        if is_active {
            check_state_driver(&mut tx, None, attribute).await?;
        }
    }
    let (user_id, user_name) = actor(ctx);
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO cmdb.workflow_definitions
           (key, name, description, class_id, include_subclasses, state_attribute_id, auto_start, is_active,
            created_by_id, created_by_name, updated_by_id, updated_by_name)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $9, $10) RETURNING id",
    )
    .bind(&b.key)
    .bind(&b.name)
    .bind(&b.description)
    .bind(b.class_id)
    .bind(b.include_subclasses.unwrap_or(true))
    .bind(b.state_attribute_id)
    .bind(b.auto_start.unwrap_or(false))
    .bind(is_active)
    .bind(user_id)
    .bind(&user_name)
    .fetch_one(&mut *tx)
    .await?;
    new_draft(&mut tx, id, 1).await?;
    let d = load(&mut tx, id, false).await?;
    let entry = AuditEntry {
        action: AuditAction::Create,
        entity_type: TABLE,
        entity_id: id,
        old_value: None,
        new_value: Some(audit_value(&mut tx, &d).await?),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    let warnings = activation_warnings(&mut tx, ctx, &d).await?;
    let out = detail(&mut tx, d, warnings).await?;
    tx.commit().await?;
    Ok(out)
}

pub async fn update(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    b: &WorkflowDefinitionUpdate,
) -> Result<WorkflowDefinitionDetail, AppError> {
    let mut tx = pool.begin().await?;
    let before = load(&mut tx, id, true).await?;
    check_version(b.version, before.version)?;
    let attribute = b.state_attribute_id.unwrap_or(before.state_attribute_id);
    if attribute != before.state_attribute_id {
        let published: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM cmdb.workflow_versions WHERE definition_id = $1 AND status <> 'draft')",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        if published {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "The state field of a workflow cannot change once a version is published: its states map to values \
                 of that field's list. Create a new workflow instead.",
            )
            .with_details(vec![FieldError {
                location: FieldLocation::Body,
                field: "stateAttributeId".into(),
                message: "Fixed once a version is published".into(),
                code: "published".into(),
            }]));
        }
        if let Some(a) = attribute {
            check_state_attribute(&Fields::load(&mut tx, before.class_id).await?, a)?;
        }
    }
    let active = b.is_active.unwrap_or(before.is_active);
    if let Some(a) = attribute
        && active
        && (!before.is_active || attribute != before.state_attribute_id)
    {
        check_state_driver(&mut tx, Some(id), a).await?;
    }
    let (user_id, user_name) = actor(ctx);
    let mut columns = b.columns();
    columns.0.push(("version", Val::Int(Some(before.version + 1))));
    columns.0.push(("updated_by_id", Val::Uuid(user_id)));
    columns.0.push(("updated_by_name", Val::Text(Some(user_name))));
    let _: (Uuid,) = crud::update_row(&mut tx, "cmdb.workflow_definitions", "id", id, columns).await?;
    let after = load(&mut tx, id, false).await?;
    let entry = AuditEntry {
        action: AuditAction::Update,
        entity_type: TABLE,
        entity_id: id,
        old_value: Some(audit_value(&mut tx, &before).await?),
        new_value: Some(audit_value(&mut tx, &after).await?),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    let newly_driving = after.is_active
        && after.state_attribute_id.is_some()
        && (!before.is_active || after.state_attribute_id != before.state_attribute_id);
    let warnings = if newly_driving { activation_warnings(&mut tx, ctx, &after).await? } else { Vec::new() };
    let out = detail(&mut tx, after, warnings).await?;
    tx.commit().await?;
    Ok(out)
}

pub async fn remove(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    let before = load(&mut tx, id, true).await?;
    let instances: i64 = sqlx::query_scalar("SELECT count(*) FROM cmdb.workflow_instances WHERE definition_id = $1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    if instances > 0 {
        let message = format!(
            "Workflow {} has run on CIs, and their history keeps it: deactivate it (isActive: false) instead of \
             deleting it",
            before.key
        );
        return Err(AppError::new(ErrorCode::InUse, message.clone()).with_details(vec![FieldError {
            location: FieldLocation::Params,
            field: "id".into(),
            message,
            code: "has_instances".into(),
        }]));
    }
    let old = audit_value(&mut tx, &before).await?;
    // Versions, their graphs and the grants go with it (the immutability
    // triggers let a cascade from the definition through).
    sqlx::query("UPDATE cmdb.workflow_definitions SET current_version_id = NULL WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    crud::delete_row(&mut tx, "cmdb.workflow_definitions", id).await?;
    let entry = AuditEntry {
        action: AuditAction::Delete,
        entity_type: TABLE,
        entity_id: id,
        old_value: Some(old),
        new_value: None,
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Versions
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct SummaryRow {
    version_no: i32,
    status: WorkflowVersionStatus,
    is_current: bool,
    state_count: i64,
    transition_count: i64,
    active_instances: i64,
    checksum: Option<Vec<u8>>,
    change_note: Option<String>,
    published_at: Option<chrono::DateTime<chrono::Utc>>,
    published_by_name: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
}

pub async fn versions(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    q: &WorkflowVersionList,
) -> Result<Page<WorkflowVersionSummary>, AppError> {
    let mut conn = pool.acquire().await?;
    let d = load(&mut conn, id, false).await?;
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM cmdb.workflow_versions WHERE definition_id = $1")
        .bind(id)
        .fetch_one(&mut *conn)
        .await?;
    let rows: Vec<SummaryRow> = sqlx::query_as(
        "SELECT w.version_no, w.status, (w.id = d.current_version_id) IS TRUE AS is_current,
                (SELECT count(*) FROM cmdb.workflow_states s WHERE s.version_id = w.id) AS state_count,
                (SELECT count(*) FROM cmdb.workflow_transitions t WHERE t.version_id = w.id) AS transition_count,
                (SELECT count(*) FROM cmdb.workflow_instances i WHERE i.version_id = w.id AND i.status = 'active')
                  AS active_instances,
                w.checksum, w.change_note, w.published_at, w.published_by_name, w.created_at
         FROM cmdb.workflow_versions w JOIN cmdb.workflow_definitions d ON d.id = w.definition_id
         WHERE w.definition_id = $1 ORDER BY w.version_no DESC LIMIT $2 OFFSET $3",
    )
    .bind(id)
    .bind(q.limit)
    .bind(q.offset)
    .fetch_all(&mut *conn)
    .await?;
    let classes = covered(&Model::load(&mut conn).await?, &d);
    let data = rows
        .into_iter()
        .map(|r| WorkflowVersionSummary {
            version_no: r.version_no,
            status: r.status,
            is_current: r.is_current,
            state_count: r.state_count,
            transition_count: r.transition_count,
            active_instance_count: Count::scoped(ctx, &classes, r.active_instances),
            checksum: r.checksum.map(hex::encode),
            change_note: r.change_note,
            published_at: r.published_at,
            published_by_name: r.published_by_name,
            created_at: r.created_at,
        })
        .collect();
    Ok(Page { data, page: q.page_meta(total) })
}

async fn render(conn: &mut PgConnection, d: &WorkflowDefinition, row: VersionRow) -> Result<WorkflowVersion, AppError> {
    let is_current = d.current_version_no == Some(row.version_no) && row.status == WorkflowVersionStatus::Published;
    let stored = graph::load(conn, row).await?;
    let model = Model::load(conn).await?;
    Ok(stored.render(&model, is_current))
}

pub async fn version(pool: &PgPool, id: Uuid, no: i32) -> Result<WorkflowVersion, AppError> {
    let mut conn = pool.acquire().await?;
    let d = load(&mut conn, id, false).await?;
    let row = version_row(&mut conn, id, "version_no = $2", Some(no), false)
        .await?
        .ok_or_else(|| AppError::new(ErrorCode::NotFound, format!("Workflow {} has no version {no}", d.key)))?;
    render(&mut conn, &d, row).await
}

pub async fn draft(pool: &PgPool, id: Uuid) -> Result<WorkflowVersion, AppError> {
    let mut conn = pool.acquire().await?;
    let d = load(&mut conn, id, false).await?;
    let row = draft_row(&mut conn, id, false).await?.ok_or_else(|| no_draft(id))?;
    render(&mut conn, &d, row).await
}

pub async fn replace_draft(pool: &PgPool, id: Uuid, b: &WorkflowDraftReplace) -> Result<WorkflowVersion, AppError> {
    let mut tx = pool.begin().await?;
    let d = load(&mut tx, id, true).await?;
    let current = draft_row(&mut tx, id, true).await?;
    if let Some(expected) = &b.expected_checksum {
        let sum = current.as_ref().and_then(|v| v.checksum.as_ref()).map(hex::encode);
        if sum.as_deref() != Some(expected.as_str()) {
            return Err(stale(
                "draft",
                "expectedChecksum",
                &format!("checksum {expected}"),
                sum.as_deref().unwrap_or("no draft"),
            ));
        }
    }
    let version_id = match current {
        Some(v) => v.id,
        None => {
            let next: i32 = sqlx::query_scalar(
                "SELECT coalesce(max(version_no), 0) + 1 FROM cmdb.workflow_versions WHERE definition_id = $1",
            )
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
            new_draft(&mut tx, id, next).await?
        }
    };
    let fields = Fields::load(&mut tx, d.class_id).await?;
    graph::store_draft(&mut tx, version_id, &fields, d.state_attribute_id, b).await?;
    let row = version_row(&mut tx, id, "id = (SELECT id FROM cmdb.workflow_versions WHERE status = 'draft' AND definition_id = $1) AND $2::int IS NULL", None, false)
        .await?
        .ok_or_else(AppError::internal)?;
    let stored = graph::load(&mut tx, row).await?;
    let sum = stored.checksum(&fields.model);
    sqlx::query("UPDATE cmdb.workflow_versions SET checksum = $2 WHERE id = $1")
        .bind(version_id)
        .bind(hex::decode(&sum).unwrap_or_default())
        .execute(&mut *tx)
        .await?;
    let mut out = stored.render(&fields.model, false);
    out.checksum = Some(sum);
    tx.commit().await?;
    Ok(out)
}

pub async fn delete_draft(pool: &PgPool, id: Uuid) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    load(&mut tx, id, true).await?;
    let row = draft_row(&mut tx, id, true).await?.ok_or_else(|| no_draft(id))?;
    sqlx::query("DELETE FROM cmdb.workflow_versions WHERE id = $1").bind(row.id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

async fn granted_keys(conn: &mut PgConnection, id: Uuid) -> Result<HashSet<String>, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT DISTINCT transition_key FROM cmdb.workflow_transition_grants WHERE definition_id = $1",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .collect())
}

/// The draft with its lint and checksum (recomputed from the stored graph).
async fn lint_draft(
    conn: &mut PgConnection,
    d: &WorkflowDefinition,
    for_update: bool,
) -> Result<(Stored, Fields, Vec<WorkflowProblem>, String), AppError> {
    let row = draft_row(conn, d.id, for_update).await?.ok_or_else(|| no_draft(d.id))?;
    let stored = graph::load(conn, row).await?;
    let fields = Fields::load(conn, d.class_id).await?;
    let granted = granted_keys(conn, d.id).await?;
    let problems = graph::lint(
        &stored,
        &LintContext { fields: &fields, state_attribute: d.state_attribute_id, granted: &granted },
    );
    let sum = stored.checksum(&fields.model);
    Ok((stored, fields, problems, sum))
}

pub async fn validate_draft(pool: &PgPool, id: Uuid) -> Result<WorkflowValidation, AppError> {
    let mut conn = pool.acquire().await?;
    let d = load(&mut conn, id, false).await?;
    let (_, _, problems, checksum) = lint_draft(&mut conn, &d, false).await?;
    let valid = !problems.iter().any(|p| p.severity == WorkflowProblemSeverity::Error);
    Ok(WorkflowValidation { valid, checksum, problems })
}

pub async fn publish(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    b: &WorkflowPublish,
) -> Result<WorkflowVersion, AppError> {
    let mut tx = pool.begin().await?;
    let d = load(&mut tx, id, true).await?;
    let (stored, fields, problems, sum) = lint_draft(&mut tx, &d, true).await?;
    let saved = stored.version.checksum.as_ref().map(hex::encode);
    if saved.as_deref() != Some(b.expected_draft_checksum.as_str()) {
        return Err(stale(
            "draft",
            "expectedDraftChecksum",
            &format!("checksum {}", b.expected_draft_checksum),
            saved.as_deref().unwrap_or("none"),
        ));
    }
    let errors: Vec<FieldError> = problems
        .iter()
        .filter(|p| p.severity == WorkflowProblemSeverity::Error)
        .map(|p| FieldError {
            location: FieldLocation::Body,
            field: p.path.clone(),
            message: p.message.clone(),
            code: p.code.clone(),
        })
        .collect();
    if !errors.is_empty() {
        let n = errors.len();
        return Err(AppError::new(
            ErrorCode::ValidationError,
            format!("The draft cannot be published: {n} problem{} (see details)", if n == 1 { "" } else { "s" }),
        )
        .with_details(errors));
    }
    let version_id = stored.version.id;
    let mut refs = stored.attributes();
    refs.extend(d.state_attribute_id);
    refs.sort();
    refs.dedup();
    // While the version is still a draft: the references are part of its graph.
    sqlx::query(
        "INSERT INTO cmdb.workflow_version_attribute_refs (version_id, attribute_id)
         SELECT $1, a FROM unnest($2::uuid[]) AS a",
    )
    .bind(version_id)
    .bind(&refs)
    .execute(&mut *tx)
    .await?;
    let (user_id, user_name) = actor(ctx);
    sqlx::query(
        "UPDATE cmdb.workflow_versions
         SET status = 'published', published_at = now(), published_by_id = $2, published_by_name = $3,
             change_note = $4, checksum = $5
         WHERE id = $1",
    )
    .bind(version_id)
    .bind(user_id)
    .bind(&user_name)
    .bind(&b.change_note)
    .bind(hex::decode(&sum).unwrap_or_default())
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE cmdb.workflow_definitions
         SET current_version_id = $2, version = version + 1, updated_by_id = $3, updated_by_name = $4
         WHERE id = $1",
    )
    .bind(id)
    .bind(version_id)
    .bind(user_id)
    .bind(&user_name)
    .execute(&mut *tx)
    .await?;
    let row = version_row(
        &mut tx,
        id,
        "id = (SELECT current_version_id FROM cmdb.workflow_definitions WHERE id = $1) AND $2::int IS NULL",
        None,
        false,
    )
    .await?
    .ok_or_else(AppError::internal)?;
    let published = graph::load(&mut tx, row).await?.render(&fields.model, true);
    let entry = AuditEntry {
        action: AuditAction::WorkflowPublish,
        entity_type: TABLE,
        entity_id: id,
        old_value: None,
        new_value: Some(json!({
            "key": d.key,
            "versionNo": published.version_no,
            "checksum": published.checksum,
            "changeNote": published.change_note,
            "graph": {
                "initialState": published.initial_state,
                "states": published.states,
                "transitions": published.transitions,
            },
        })),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    tx.commit().await?;
    Ok(published)
}

pub async fn retire(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    no: i32,
) -> Result<WorkflowVersionSummary, AppError> {
    let mut tx = pool.begin().await?;
    let d = load(&mut tx, id, true).await?;
    let row = version_row(&mut tx, id, "version_no = $2", Some(no), true)
        .await?
        .ok_or_else(|| AppError::new(ErrorCode::NotFound, format!("Workflow {} has no version {no}", d.key)))?;
    if row.status != WorkflowVersionStatus::Published {
        let what =
            if row.status == WorkflowVersionStatus::Draft { "a draft (delete it instead)" } else { "already retired" };
        return Err(AppError::conflict(format!("Version {no} of workflow {} is {what}", d.key)));
    }
    sqlx::query("UPDATE cmdb.workflow_versions SET status = 'retired' WHERE id = $1")
        .bind(row.id)
        .execute(&mut *tx)
        .await?;
    // New instances start on the newest version still published.
    let (user_id, user_name) = actor(ctx);
    sqlx::query(
        "UPDATE cmdb.workflow_definitions
         SET current_version_id = (SELECT w.id FROM cmdb.workflow_versions w
                                   WHERE w.definition_id = $1 AND w.status = 'published'
                                   ORDER BY w.version_no DESC LIMIT 1),
             version = version + 1, updated_by_id = $2, updated_by_name = $3
         WHERE id = $1",
    )
    .bind(id)
    .bind(user_id)
    .bind(&user_name)
    .execute(&mut *tx)
    .await?;
    let after = load(&mut tx, id, false).await?;
    let entry = AuditEntry {
        action: AuditAction::Update,
        entity_type: TABLE,
        entity_id: id,
        old_value: Some(json!({ "versionNo": no, "status": "published", "currentVersionNo": d.current_version_no })),
        new_value: Some(json!({ "versionNo": no, "status": "retired", "currentVersionNo": after.current_version_no })),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    let model = Model::load(&mut tx).await?;
    let classes = covered(&model, &after);
    let r: SummaryRow = sqlx::query_as(
        "SELECT w.version_no, w.status, false AS is_current,
                (SELECT count(*) FROM cmdb.workflow_states s WHERE s.version_id = w.id) AS state_count,
                (SELECT count(*) FROM cmdb.workflow_transitions t WHERE t.version_id = w.id) AS transition_count,
                (SELECT count(*) FROM cmdb.workflow_instances i WHERE i.version_id = w.id AND i.status = 'active')
                  AS active_instances,
                w.checksum, w.change_note, w.published_at, w.published_by_name, w.created_at
         FROM cmdb.workflow_versions w WHERE w.id = $1",
    )
    .bind(row.id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(WorkflowVersionSummary {
        version_no: r.version_no,
        status: r.status,
        is_current: r.is_current,
        state_count: r.state_count,
        transition_count: r.transition_count,
        active_instance_count: Count::scoped(ctx, &classes, r.active_instances),
        checksum: r.checksum.map(hex::encode),
        change_note: r.change_note,
        published_at: r.published_at,
        published_by_name: r.published_by_name,
        created_at: r.created_at,
    })
}

// ---------------------------------------------------------------------------
// Grants
// ---------------------------------------------------------------------------

pub async fn grants(pool: &PgPool, id: Uuid) -> Result<WorkflowGrants, AppError> {
    let mut conn = pool.acquire().await?;
    let d = load(&mut conn, id, false).await?;
    Ok(WorkflowGrants { version: d.version, grants: grant_rows(&mut conn, id).await? })
}

pub async fn replace_grants(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    b: &WorkflowGrantsReplace,
) -> Result<WorkflowGrants, AppError> {
    let mut tx = pool.begin().await?;
    let before = load(&mut tx, id, true).await?;
    check_version(b.version, before.version)?;
    let profiles: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, name FROM cmdb.permission_profiles").fetch_all(&mut *tx).await?;
    let by_id: HashMap<Uuid, Uuid> = profiles.iter().map(|(id, _)| (*id, *id)).collect();
    let by_name: HashMap<String, Uuid> = profiles.iter().map(|(id, n)| (n.to_lowercase(), *id)).collect();
    let mut errors = Vec::new();
    let mut rows: Vec<(String, Uuid)> = Vec::new();
    for (i, g) in b.grants.iter().enumerate() {
        let mut seen = HashSet::new();
        for (j, p) in g.profiles.iter().enumerate() {
            let found = if validate::is_uuid(p) {
                p.parse::<Uuid>().ok().and_then(|u| by_id.get(&u).copied())
            } else {
                by_name.get(&p.to_lowercase()).copied()
            };
            match found {
                Some(profile) if seen.insert(profile) => rows.push((g.transition_key.clone(), profile)),
                Some(_) => errors.push(FieldError {
                    location: FieldLocation::Body,
                    field: format!("grants[{i}].profiles[{j}]"),
                    message: "Listed more than once".into(),
                    code: "duplicate".into(),
                }),
                None => errors.push(FieldError {
                    location: FieldLocation::Body,
                    field: format!("grants[{i}].profiles[{j}]"),
                    message: format!("No permission profile \"{p}\""),
                    code: "not_found".into(),
                }),
            }
        }
    }
    if !errors.is_empty() {
        return Err(AppError::validation(errors));
    }
    let old = grant_rows(&mut tx, id).await?;
    sqlx::query("DELETE FROM cmdb.workflow_transition_grants WHERE definition_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let keys: Vec<&str> = rows.iter().map(|(k, _)| k.as_str()).collect();
    let ids: Vec<Uuid> = rows.iter().map(|(_, p)| *p).collect();
    sqlx::query(
        "INSERT INTO cmdb.workflow_transition_grants (definition_id, transition_key, profile_id)
         SELECT $1, k, p FROM unnest($2::text[], $3::uuid[]) AS u(k, p)",
    )
    .bind(id)
    .bind(&keys)
    .bind(&ids)
    .execute(&mut *tx)
    .await?;
    let new = grant_rows(&mut tx, id).await?;
    let (old_v, new_v) = (grants_by_name(&old), grants_by_name(&new));
    if old_v == new_v {
        tx.commit().await?;
        return Ok(WorkflowGrants { version: before.version, grants: new });
    }
    let (user_id, user_name) = actor(ctx);
    sqlx::query(
        "UPDATE cmdb.workflow_definitions SET version = version + 1, updated_by_id = $2, updated_by_name = $3
         WHERE id = $1",
    )
    .bind(id)
    .bind(user_id)
    .bind(&user_name)
    .execute(&mut *tx)
    .await?;
    let entry = AuditEntry {
        action: AuditAction::Update,
        entity_type: TABLE,
        entity_id: id,
        old_value: Some(json!({ "version": before.version, "grants": old_v })),
        new_value: Some(json!({ "version": before.version + 1, "grants": new_v })),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    tx.commit().await?;
    Ok(WorkflowGrants { version: before.version + 1, grants: new })
}
