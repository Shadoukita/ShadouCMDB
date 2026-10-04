//! Run time: workflow instances on CIs (v0.4.0 slice S3, SHAA-1424; design on
//! SHAA-1411 §3.2, §4, §5.1, §6.2-§6.4).
//!
//! **Lock order.** Every path that changes an instance locks the CI row first
//! (`data::items::lock`, as a CI PATCH or delete does) and the instance row
//! second: start, transition, cancel, force and the cancel cascade of a CI
//! soft delete. A transition reads the instance's CI without a lock, locks
//! the CI, then locks the instance and checks again that it is active and at
//! the version the caller loaded. A concurrent transition and CI delete
//! therefore queue on the CI row instead of deadlocking (Amendment 1).
//!
//! **Rights** (§4.1). Viewing an instance needs the view right on its CI's
//! type, else it does not exist (404). Starting needs the edit right on the
//! type. A transition needs the edit right and a grant of its key to one of
//! the caller's profiles; for an API token, also to the token's narrowing
//! profile (Q6). The Administrator profile is granted every transition.
//! Cancelling needs `workflows.manage`, or the `_cancel` grant and the edit
//! right; forcing a state needs `workflows.manage` and the edit right.
//!
//! **Audit** (§5.1). Starts, transitions, cancels and forced states are audit
//! rows on the CI (`configuration_items`, the CI's id), so the CI's history
//! shows them and the item visibility rules apply. The CI fields a step writes
//! (transition fields, the state field) are the usual CI `update` row of the
//! same request. Each step is also an event of the instance (append-only).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{Map, Value, json};
use sqlx::types::Json as SqlJson;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::eval::{self, Subject};
use super::graph::{self, VERSION_COLUMNS, VersionRow};
use super::runtime_schemas::*;
use super::schemas::WorkflowStateCategory;
use super::state_field::StateFields;
use crate::api::context::{Caller, RequestContext};
use crate::api::schemas::{Page, Paged};
use crate::auth::Credential;
use crate::auth::permissions::{ClassOp, GlobalPermission};
use crate::data::crud::{self, AuditAction, AuditEntry, Where};
use crate::data::items as item_data;
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::modules::items::service as items;
use crate::schema::model::{Field, Model};

const INSTANCE: &str = "Workflow instance";
const CI: &str = "Configuration item";
/// The `_cancel` pseudo transition key of the grants.
const CANCEL_KEY: &str = "_cancel";
/// Ended instances `GET /configuration-items/{id}/workflows` lists.
const RECENT_ENDED: i64 = 20;

// ---------------------------------------------------------------------------
// The pinned graph of a version
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub(super) struct PinnedState {
    pub(super) id: Uuid,
    pub(super) key: String,
    pub(super) name: String,
    category: WorkflowStateCategory,
    pub(super) is_terminal: bool,
    pub(super) state_value_id: Option<Uuid>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct PinnedTransition {
    id: Uuid,
    key: String,
    name: String,
    from_state_id: Uuid,
    to_state_id: Uuid,
    requires_comment: bool,
    conditions: Option<SqlJson<Value>>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct PinnedField {
    transition_id: Uuid,
    attribute_id: Uuid,
    is_required: bool,
}

/// A published (or retired) version's graph by id: what running an instance needs.
#[derive(Debug)]
pub(super) struct Pinned {
    pub(super) initial_state_id: Option<Uuid>,
    pub(super) states: Vec<PinnedState>,
    transitions: Vec<PinnedTransition>,
    fields: Vec<PinnedField>,
}

impl Pinned {
    pub(super) fn state(&self, id: Uuid) -> Option<&PinnedState> {
        self.states.iter().find(|s| s.id == id)
    }

    fn state_by_key(&self, key: &str) -> Option<&PinnedState> {
        self.states.iter().find(|s| s.key == key)
    }

    fn out_of(&self, state: Uuid) -> impl Iterator<Item = &PinnedTransition> {
        self.transitions.iter().filter(move |t| t.from_state_id == state)
    }

    fn fields_of(&self, transition: Uuid) -> impl Iterator<Item = &PinnedField> {
        self.fields.iter().filter(move |f| f.transition_id == transition)
    }
}

fn state_ref(s: &PinnedState) -> WorkflowStateRef {
    WorkflowStateRef { key: s.key.clone(), name: s.name.clone(), category: s.category, terminal: s.is_terminal }
}

/// Graphs of published versions never change (the database refuses it), so
/// they are cached by version id for the life of the process.
fn cache() -> &'static Mutex<HashMap<Uuid, Arc<Pinned>>> {
    static CACHE: OnceLock<Mutex<HashMap<Uuid, Arc<Pinned>>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Versions kept in the cache; it is emptied when full.
const CACHE_SIZE: usize = 1024;

pub(super) async fn pinned(conn: &mut PgConnection, version_id: Uuid) -> Result<Arc<Pinned>, AppError> {
    if let Some(p) = cache().lock().ok().and_then(|c| c.get(&version_id).cloned()) {
        return Ok(p);
    }
    let (initial_state_id, status): (Option<Uuid>, String) =
        sqlx::query_as("SELECT initial_state_id, status FROM cmdb.workflow_versions WHERE id = $1")
            .bind(version_id)
            .fetch_one(&mut *conn)
            .await?;
    let states = sqlx::query_as::<_, PinnedState>(
        "SELECT id, key, name, category, is_terminal, state_value_id FROM cmdb.workflow_states
         WHERE version_id = $1 ORDER BY sort_order, key",
    )
    .bind(version_id)
    .fetch_all(&mut *conn)
    .await?;
    let transitions = sqlx::query_as::<_, PinnedTransition>(
        "SELECT id, key, name, from_state_id, to_state_id, requires_comment, conditions
         FROM cmdb.workflow_transitions WHERE version_id = $1 ORDER BY sort_order, key",
    )
    .bind(version_id)
    .fetch_all(&mut *conn)
    .await?;
    let fields = sqlx::query_as::<_, PinnedField>(
        "SELECT f.transition_id, f.attribute_id, f.is_required
         FROM cmdb.workflow_transition_fields f JOIN cmdb.workflow_transitions t ON t.id = f.transition_id
         WHERE t.version_id = $1 ORDER BY f.sort_order, f.attribute_id",
    )
    .bind(version_id)
    .fetch_all(&mut *conn)
    .await?;
    let p = Arc::new(Pinned { initial_state_id, states, transitions, fields });
    // A draft can still change: never cached (no instance runs on one).
    if status != "draft"
        && let Ok(mut c) = cache().lock()
    {
        if c.len() >= CACHE_SIZE {
            c.clear();
        }
        c.insert(version_id, p.clone());
    }
    Ok(p)
}

// ---------------------------------------------------------------------------
// Rights
// ---------------------------------------------------------------------------

/// The transition keys of one definition the caller is granted.
enum Granted {
    All,
    Keys(HashSet<String>),
}

impl Granted {
    fn has(&self, key: &str) -> bool {
        match self {
            Granted::All => true,
            Granted::Keys(k) => k.contains(key),
        }
    }

    fn and(self, other: Granted) -> Granted {
        match (self, other) {
            (Granted::All, g) | (g, Granted::All) => g,
            (Granted::Keys(a), Granted::Keys(b)) => Granted::Keys(a.intersection(&b).cloned().collect()),
        }
    }
}

/// What the caller is granted on `definition`: through one of their profiles
/// (the Administrator profile is granted everything) and, for an API token,
/// through its narrowing profile as well (Q6) and, for a token minted by
/// someone else, through one of its active creator's profiles too (GH#607).
/// Read live from the profiles, as the token's permissions are.
async fn granted(conn: &mut PgConnection, ctx: &RequestContext, definition: Uuid) -> Result<Granted, AppError> {
    let p = match &ctx.caller {
        Caller::System => return Ok(Granted::All),
        Caller::Anonymous => return Ok(Granted::Keys(HashSet::new())),
        Caller::User(p) => p,
    };
    if p.permissions.administrator {
        return Ok(Granted::All);
    }
    let (token_profile, creator) = match p.credential {
        Credential::Token { profile_id, creator_id } => (profile_id, creator_id),
        Credential::Session { .. } => (None, None),
    };
    let rows: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT 'owner_admin', NULL::text FROM cmdb.user_permission_profiles up
           JOIN cmdb.permission_profiles p ON p.id = up.profile_id WHERE up.user_id = $2 AND p.is_builtin
         UNION ALL
         SELECT 'owner', g.transition_key FROM cmdb.workflow_transition_grants g
           JOIN cmdb.user_permission_profiles up ON up.profile_id = g.profile_id
         WHERE g.definition_id = $1 AND up.user_id = $2
         UNION ALL
         SELECT 'token_admin', NULL FROM cmdb.permission_profiles p WHERE p.id = $3 AND p.is_builtin
         UNION ALL
         SELECT 'token', g.transition_key FROM cmdb.workflow_transition_grants g
         WHERE g.definition_id = $1 AND g.profile_id = $3
         UNION ALL
         SELECT 'creator_admin', NULL FROM cmdb.user_permission_profiles up
           JOIN cmdb.permission_profiles p ON p.id = up.profile_id JOIN cmdb.users u ON u.id = up.user_id
         WHERE up.user_id = $4 AND p.is_builtin AND u.is_active
         UNION ALL
         SELECT 'creator', g.transition_key FROM cmdb.workflow_transition_grants g
           JOIN cmdb.user_permission_profiles up ON up.profile_id = g.profile_id
           JOIN cmdb.users u ON u.id = up.user_id
         WHERE g.definition_id = $1 AND up.user_id = $4 AND u.is_active",
    )
    .bind(definition)
    .bind(p.user_id)
    .bind(token_profile)
    .bind(creator)
    .fetch_all(&mut *conn)
    .await?;
    let side = |admin: &str, keys: &str| {
        if rows.iter().any(|(k, _)| k == admin) {
            Granted::All
        } else {
            Granted::Keys(rows.iter().filter(|(k, _)| k == keys).filter_map(|(_, t)| t.clone()).collect())
        }
    };
    let mut g = side("owner_admin", "owner");
    if token_profile.is_some() {
        g = g.and(side("token_admin", "token"));
    }
    // An inactive creator has no rows, so grants nothing.
    if creator.is_some() {
        g = g.and(side("creator_admin", "creator"));
    }
    Ok(g)
}

fn may_manage(ctx: &RequestContext) -> bool {
    ctx.require(GlobalPermission::WorkflowsManage).is_ok()
}

fn may_edit(ctx: &RequestContext, class_id: Uuid) -> bool {
    ctx.require_class(class_id, ClassOp::Edit).is_ok()
}

// ---------------------------------------------------------------------------
// Instances
// ---------------------------------------------------------------------------

const COLUMNS: &str = "wi.id, wi.definition_id, d.key AS definition_key, d.name AS definition_name, \
     d.state_attribute_id, wi.version_id, v.version_no, wi.ci_id, ci.ident AS ci_ident, ci.label AS ci_label, \
     ci.class_id, c.key AS class_key, ci.deleted_at IS NOT NULL AS ci_deleted, ci.version AS ci_version, \
     wi.current_state_id, s.key AS state_key, s.name AS state_name, s.category AS state_category, \
     s.is_terminal AS state_terminal, wi.status, wi.started_at, wi.started_by_name, wi.last_transition_at, \
     wi.ended_at, wi.version";
const FROM: &str = "cmdb.workflow_instances wi JOIN cmdb.workflow_definitions d ON d.id = wi.definition_id \
     JOIN cmdb.workflow_versions v ON v.id = wi.version_id \
     JOIN cmdb.configuration_items ci ON ci.id = wi.ci_id JOIN cmdb.ci_classes c ON c.id = ci.class_id \
     JOIN cmdb.workflow_states s ON s.id = wi.current_state_id";

#[derive(Debug, Clone, sqlx::FromRow)]
struct InstanceRow {
    id: Uuid,
    definition_id: Uuid,
    definition_key: String,
    definition_name: String,
    state_attribute_id: Option<Uuid>,
    version_id: Uuid,
    version_no: i32,
    ci_id: Uuid,
    ci_ident: String,
    ci_label: String,
    class_id: Uuid,
    class_key: String,
    ci_deleted: bool,
    ci_version: i32,
    current_state_id: Uuid,
    state_key: String,
    state_name: String,
    state_category: WorkflowStateCategory,
    state_terminal: bool,
    status: WorkflowInstanceStatus,
    started_at: chrono::DateTime<chrono::Utc>,
    started_by_name: String,
    last_transition_at: chrono::DateTime<chrono::Utc>,
    ended_at: Option<chrono::DateTime<chrono::Utc>>,
    version: i32,
}

impl InstanceRow {
    fn dto(&self) -> WorkflowInstance {
        WorkflowInstance {
            id: self.id,
            definition_id: self.definition_id,
            definition_key: self.definition_key.clone(),
            definition_name: self.definition_name.clone(),
            version_no: self.version_no,
            ci_id: self.ci_id,
            ci_ident: self.ci_ident.clone(),
            ci_label: self.ci_label.clone(),
            class_key: self.class_key.clone(),
            status: self.status,
            state: WorkflowStateRef {
                key: self.state_key.clone(),
                name: self.state_name.clone(),
                category: self.state_category,
                terminal: self.state_terminal,
            },
            started_at: self.started_at,
            started_by_name: self.started_by_name.clone(),
            last_transition_at: self.last_transition_at,
            ended_at: self.ended_at,
            version: self.version,
            ci_version: self.ci_version,
        }
    }
}

async fn instance(conn: &mut PgConnection, id: Uuid) -> Result<Option<InstanceRow>, AppError> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {COLUMNS} FROM {FROM} WHERE wi.id = $1")))
        .bind(id)
        .fetch_optional(conn)
        .await?)
}

/// An instance the caller may view (else 404, whether or not it exists).
async fn visible_instance(conn: &mut PgConnection, ctx: &RequestContext, id: Uuid) -> Result<InstanceRow, AppError> {
    let row = instance(conn, id).await?.ok_or_else(|| AppError::missing(INSTANCE, id))?;
    ctx.require_class_visible(row.class_id, INSTANCE, id)?;
    Ok(row)
}

/// Locks an instance for a change, CI first (Amendment 1): its CI is read
/// without a lock, the CI row is locked, then the instance row. The view
/// right is checked on the CI's type as it is once locked.
async fn lock(conn: &mut PgConnection, ctx: &RequestContext, id: Uuid) -> Result<InstanceRow, AppError> {
    let ci: Option<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT wi.ci_id, ci.class_id FROM cmdb.workflow_instances wi
         JOIN cmdb.configuration_items ci ON ci.id = wi.ci_id WHERE wi.id = $1",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    let (ci_id, class_id) = ci.ok_or_else(|| AppError::missing(INSTANCE, id))?;
    ctx.require_class_visible(class_id, INSTANCE, id)?;
    let locked = item_data::lock(conn, ci_id).await?.ok_or_else(|| AppError::missing(INSTANCE, id))?;
    ctx.require_class_visible(locked.class_id, INSTANCE, id)?;
    let still: Option<Uuid> = sqlx::query_scalar("SELECT ci_id FROM cmdb.workflow_instances WHERE id = $1 FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
    if still != Some(ci_id) {
        return Err(AppError::missing(INSTANCE, id));
    }
    instance(conn, id).await?.ok_or_else(|| AppError::missing(INSTANCE, id))
}

fn detail_error(code: ErrorCode, message: String, field: &str, detail_code: &str) -> AppError {
    AppError::new(code, message.clone()).with_details(vec![FieldError {
        location: FieldLocation::Body,
        field: field.into(),
        message,
        code: detail_code.into(),
    }])
}

/// 409 CONFLICT unless the instance is running.
fn check_active(row: &InstanceRow) -> Result<(), AppError> {
    if row.status == WorkflowInstanceStatus::Active {
        return Ok(());
    }
    let status = match row.status {
        WorkflowInstanceStatus::Completed => "completed",
        _ => "cancelled",
    };
    Err(AppError::new(ErrorCode::Conflict, format!("This workflow instance is {status}: it can no longer change"))
        .with_details(vec![FieldError {
            location: FieldLocation::Params,
            field: "id".into(),
            message: format!("The instance is {status}"),
            code: "not_active".into(),
        }]))
}

/// 409 VERSION_CONFLICT unless the caller loaded the current version.
fn check_version(row: &InstanceRow, expected: i32) -> Result<(), AppError> {
    if row.version == expected {
        return Ok(());
    }
    let current = row.version;
    Err(detail_error(
        ErrorCode::VersionConflict,
        format!(
            "The workflow instance was changed by someone else (you sent version {expected}, current is {current}). \
             Reload and retry."
        ),
        "expectedVersion",
        "stale",
    ))
}

// ---------------------------------------------------------------------------
// Values and conditions
// ---------------------------------------------------------------------------

/// A field of the CI's type (own or inherited) by key.
fn field_by_key<'m>(model: &'m Model, class_id: Uuid, key: &str) -> Option<&'m Field> {
    model.lineage(class_id).into_iter().find_map(|c| model.own_fields(c.id).find(|f| f.key == key))
}

/// The CI's values (API form, by key) as conditions read them: by field id,
/// lookup values as the key of the value.
async fn condition_values(
    conn: &mut PgConnection,
    model: &Model,
    class_id: Uuid,
    values: &Map<String, Value>,
) -> Result<HashMap<Uuid, Value>, AppError> {
    let mut out = HashMap::new();
    let mut lookups: Vec<(Uuid, Uuid)> = Vec::new();
    for (key, v) in values {
        let Some(f) = field_by_key(model, class_id, key) else { continue };
        match (f.data_type, v.as_str().and_then(|s| s.parse::<Uuid>().ok())) {
            (crate::modules::classes::AttributeDataType::Lookup, Some(value)) => lookups.push((f.id, value)),
            (crate::modules::classes::AttributeDataType::Lookup, None) => {}
            _ => {
                out.insert(f.id, v.clone());
            }
        }
    }
    if !lookups.is_empty() {
        let ids: Vec<Uuid> = lookups.iter().map(|(_, v)| *v).collect();
        let keys: HashMap<Uuid, String> =
            sqlx::query_as::<_, (Uuid, String)>("SELECT id, key FROM cmdb.lookup_list_values WHERE id = ANY($1)")
                .bind(&ids)
                .fetch_all(&mut *conn)
                .await?
                .into_iter()
                .collect();
        for (field, value) in lookups {
            if let Some(k) = keys.get(&value) {
                out.insert(field, Value::String(k.clone()));
            }
        }
    }
    Ok(out)
}

fn is_set(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// The CI's current values (API form, by key), unredacted: for checks only.
async fn current_values(conn: &mut PgConnection, model: &Model, ci: Uuid) -> Result<Map<String, Value>, AppError> {
    Ok(items::details(conn, model, &[ci]).await?.pop().map(|c| c.attributes).unwrap_or_default())
}

/// What runs on the CI as it stands, for one instance.
struct Context<'a> {
    model: &'a Model,
    values: &'a Map<String, Value>,
    by_id: &'a HashMap<Uuid, Value>,
}

fn blocked_by(t: &PinnedTransition, cx: &Context<'_>) -> Vec<WorkflowBlockedReason> {
    let Some(c) = &t.conditions else { return Vec::new() };
    eval::failures(&c.0, &Subject { model: cx.model, values: cx.by_id })
        .into_iter()
        .map(|f| WorkflowBlockedReason {
            field: format!("fields.{}", f.key),
            code: "condition".into(),
            message: f.message,
        })
        .collect()
}

fn available(
    row: &InstanceRow,
    p: &Pinned,
    granted: &Granted,
    may_edit: bool,
    cx: &Context<'_>,
) -> Vec<WorkflowAvailableTransition> {
    if row.status != WorkflowInstanceStatus::Active || row.ci_deleted || !may_edit {
        return Vec::new();
    }
    p.out_of(row.current_state_id)
        .filter(|t| granted.has(&t.key))
        .filter_map(|t| {
            let to = p.state(t.to_state_id)?;
            let fields = p
                .fields_of(t.id)
                .filter_map(|f| {
                    let field = cx.model.field(f.attribute_id)?;
                    Some(WorkflowTransitionFieldView {
                        key: field.key.clone(),
                        label: field.label.clone(),
                        data_type: field.data_type,
                        required: f.is_required,
                        current_value: cx.values.get(&field.key).filter(|v| !v.is_null()).cloned(),
                    })
                })
                .collect();
            Some(WorkflowAvailableTransition {
                key: t.key.clone(),
                name: t.name.clone(),
                to_state: state_ref(to),
                requires_comment: t.requires_comment,
                fields,
                blocked_by: blocked_by(t, cx),
            })
        })
        .collect()
}

async fn view(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    model: &Model,
    row: &InstanceRow,
    values: &Map<String, Value>,
    by_id: &HashMap<Uuid, Value>,
) -> Result<WorkflowInstanceView, AppError> {
    let p = pinned(conn, row.version_id).await?;
    let edit = may_edit(ctx, row.class_id);
    let g = granted(conn, ctx, row.definition_id).await?;
    let cx = Context { model, values, by_id };
    let available_transitions = available(row, &p, &g, edit, &cx);
    let can_cancel = row.status == WorkflowInstanceStatus::Active
        && !row.ci_deleted
        && (may_manage(ctx) || (edit && g.has(CANCEL_KEY)));
    Ok(WorkflowInstanceView { instance: row.dto(), available_transitions, can_cancel })
}

async fn detail(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    row: &InstanceRow,
) -> Result<WorkflowInstanceDetail, AppError> {
    let model = Model::load(conn).await?;
    let values = current_values(conn, &model, row.ci_id).await?;
    let by_id = condition_values(conn, &model, row.class_id, &values).await?;
    let v = view(conn, ctx, &model, row, &values, &by_id).await?;
    let version: VersionRow = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {VERSION_COLUMNS} FROM cmdb.workflow_versions WHERE id = $1"
    )))
    .bind(row.version_id)
    .fetch_one(&mut *conn)
    .await?;
    let rendered = graph::load(conn, version).await?.render(&model, false);
    Ok(WorkflowInstanceDetail {
        instance: v.instance,
        graph: WorkflowPinnedGraph {
            initial_state: rendered.initial_state.unwrap_or_default(),
            states: rendered.states,
            transitions: rendered.transitions,
        },
        available_transitions: v.available_transitions,
        can_cancel: v.can_cancel,
    })
}

// ---------------------------------------------------------------------------
// Writing a step
// ---------------------------------------------------------------------------

/// Who the event names.
struct EventActor {
    actor_type: &'static str,
    id: Option<String>,
    name: Option<String>,
}

fn actor_of(ctx: &RequestContext) -> EventActor {
    EventActor { actor_type: ctx.actor.actor_type.as_str(), id: ctx.actor.id.clone(), name: ctx.actor.name.clone() }
}

pub(super) fn starter(ctx: &RequestContext) -> (Option<Uuid>, String) {
    let id = ctx.principal().map(|p| p.user_id);
    let name = ctx.principal().map(|p| p.username.clone()).or_else(|| ctx.actor.name.clone()).unwrap_or_default();
    (id, name)
}

pub(super) struct NewEvent<'a> {
    pub(super) instance: Uuid,
    pub(super) kind: &'static str,
    pub(super) transition_key: Option<&'a str>,
    pub(super) from_state_key: Option<&'a str>,
    pub(super) to_state_key: &'a str,
    pub(super) to_version_no: i32,
    pub(super) comment: Option<&'a str>,
    pub(super) field_changes: Option<Value>,
}

pub(super) async fn insert_event(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    e: NewEvent<'_>,
) -> Result<(), AppError> {
    let actor = actor_of(ctx);
    sqlx::query(
        "INSERT INTO cmdb.workflow_instance_events
           (instance_id, kind, transition_key, from_state_key, to_state_key, to_version_no,
            actor_type, actor_id, actor_name, comment, field_changes, request_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(e.instance)
    .bind(e.kind)
    .bind(e.transition_key)
    .bind(e.from_state_key)
    .bind(e.to_state_key)
    .bind(e.to_version_no)
    .bind(actor.actor_type)
    .bind(actor.id)
    .bind(actor.name)
    .bind(e.comment)
    .bind(e.field_changes.map(SqlJson))
    .bind(&ctx.request_id)
    .execute(conn)
    .await?;
    Ok(())
}

/// The CI values a step sets on its CI, through the item write path; returns
/// `{key: {old, new}}` of what changed (None when nothing did).
async fn write_ci(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    ci: Uuid,
    class_id: Uuid,
    attributes: Map<String, Value>,
) -> Result<Option<Value>, AppError> {
    let keys: Vec<String> = attributes.keys().cloned().collect();
    let w = items::update_for_workflow(conn, ctx, ci, class_id, attributes, true).await?;
    let mut changes = Map::new();
    for k in keys {
        let (old, new) = (w.before.attributes.get(&k), w.after.attributes.get(&k));
        if old != new {
            changes.insert(k, json!({ "old": old, "new": new }));
        }
    }
    Ok((!changes.is_empty()).then_some(Value::Object(changes)))
}

/// `{stateFieldKey: valueId}` when the workflow drives a state field and `to` maps to one of its values.
fn state_value(model: &Model, row_state_attribute: Option<Uuid>, to: &PinnedState) -> Map<String, Value> {
    let mut m = Map::new();
    if let (Some(field), Some(value)) = (row_state_attribute.and_then(|a| model.field(a)), to.state_value_id) {
        m.insert(field.key.clone(), Value::String(value.to_string()));
    }
    m
}

/// Moves a locked, active instance to `to` (completing it on a terminal state).
async fn move_to(conn: &mut PgConnection, id: Uuid, to: &PinnedState) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE cmdb.workflow_instances
         SET current_state_id = $2, version = version + 1, last_transition_at = now(),
             status = CASE WHEN $3 THEN 'completed' ELSE status END,
             ended_at = CASE WHEN $3 THEN now() ELSE ended_at END
         WHERE id = $1",
    )
    .bind(id)
    .bind(to.id)
    .bind(to.is_terminal)
    .execute(conn)
    .await?;
    Ok(())
}

async fn reload(conn: &mut PgConnection, id: Uuid) -> Result<InstanceRow, AppError> {
    instance(conn, id).await?.ok_or_else(AppError::internal)
}

// ---------------------------------------------------------------------------
// Start
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct DefinitionRow {
    id: Uuid,
    key: String,
    class_id: Uuid,
    include_subclasses: bool,
    state_attribute_id: Option<Uuid>,
    is_active: bool,
    current_version_id: Option<Uuid>,
    current_version_no: Option<i32>,
}

pub async fn start(
    pool: &PgPool,
    ctx: &RequestContext,
    b: &WorkflowInstanceStart,
) -> Result<WorkflowInstanceDetail, AppError> {
    let mut tx = pool.begin().await?;
    let locked = item_data::lock(&mut tx, b.ci_id).await?.ok_or_else(|| AppError::missing(CI, b.ci_id))?;
    ctx.require_class_visible(locked.class_id, CI, b.ci_id)?;
    ctx.require_class(locked.class_id, ClassOp::Edit)?;
    let d: Option<DefinitionRow> = sqlx::query_as(
        "SELECT d.id, d.key, d.class_id, d.include_subclasses, d.state_attribute_id, d.is_active,
                d.current_version_id, v.version_no AS current_version_no
         FROM cmdb.workflow_definitions d LEFT JOIN cmdb.workflow_versions v ON v.id = d.current_version_id
         WHERE d.id = $1 OR lower(d.key) = lower($2)",
    )
    .bind(b.definition_id)
    .bind(&b.definition_key)
    .fetch_optional(&mut *tx)
    .await?;
    let named = b.definition_key.clone().or(b.definition_id.map(|i| i.to_string())).unwrap_or_default();
    // A workflow of a type the caller may not view is missing, like the CIs it runs on.
    let d = d
        .filter(|d| ctx.class_scope(ClassOp::View).is_none_or(|v| v.contains(&d.class_id)))
        .ok_or_else(|| AppError::missing("Workflow definition", &named))?;
    if locked.deleted_at.is_some() {
        return Err(detail_error(
            ErrorCode::Conflict,
            "This configuration item is deleted: no workflow can start on it".into(),
            "ciId",
            "deleted",
        ));
    }
    let model = Model::load(&mut tx).await?;
    let covers = d.class_id == locked.class_id
        || (d.include_subclasses && model.lineage(locked.class_id).iter().any(|c| c.id == d.class_id));
    if !covers {
        return Err(AppError::field(
            "ciId",
            format!("Workflow {} does not run on CIs of this type", d.key),
            "not_covered",
        ));
    }
    let (Some(version_id), Some(version_no)) = (d.current_version_id, d.current_version_no) else {
        return Err(detail_error(
            ErrorCode::Conflict,
            format!("Workflow {} has no published version yet", d.key),
            "definitionId",
            "unpublished",
        ));
    };
    if !d.is_active {
        return Err(detail_error(
            ErrorCode::Conflict,
            format!("Workflow {} is inactive: it starts no new instances", d.key),
            "definitionId",
            "inactive",
        ));
    }
    let running: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM cmdb.workflow_instances WHERE definition_id = $1 AND ci_id = $2 AND status = 'active')",
    )
    .bind(d.id)
    .bind(b.ci_id)
    .fetch_one(&mut *tx)
    .await?;
    if running {
        return Err(detail_error(
            ErrorCode::Conflict,
            format!("Workflow {} is already running on this configuration item", d.key),
            "ciId",
            "already_running",
        ));
    }
    let p = pinned(&mut tx, version_id).await?;
    let initial = p.initial_state_id.and_then(|id| p.state(id)).ok_or_else(AppError::internal)?;
    let (user_id, user_name) = starter(ctx);
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO cmdb.workflow_instances
           (definition_id, version_id, ci_id, current_state_id, status, started_by_id, started_by_name)
         VALUES ($1, $2, $3, $4, 'active', $5, $6) RETURNING id",
    )
    .bind(d.id)
    .bind(version_id)
    .bind(b.ci_id)
    .bind(initial.id)
    .bind(user_id)
    .bind(&user_name)
    .fetch_one(&mut *tx)
    .await?;
    let changes =
        write_ci(&mut tx, ctx, b.ci_id, locked.class_id, state_value(&model, d.state_attribute_id, initial)).await?;
    let comment = b.comment.as_deref().filter(|c| !c.trim().is_empty());
    insert_event(
        &mut tx,
        ctx,
        NewEvent {
            instance: id,
            kind: "start",
            transition_key: None,
            from_state_key: None,
            to_state_key: &initial.key,
            to_version_no: version_no,
            comment,
            field_changes: changes,
        },
    )
    .await?;
    let entry = AuditEntry {
        action: AuditAction::WorkflowStart,
        entity_type: "configuration_items",
        entity_id: b.ci_id,
        old_value: None,
        new_value: Some(json!({
            "instanceId": id, "definitionKey": d.key, "versionNo": version_no, "stateKey": initial.key,
            "comment": comment,
        })),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    let row = reload(&mut tx, id).await?;
    let out = detail(&mut tx, ctx, &row).await?;
    tx.commit().await?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

pub async fn get(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<WorkflowInstanceDetail, AppError> {
    let mut conn = pool.acquire().await?;
    let row = visible_instance(&mut conn, ctx, id).await?;
    detail(&mut conn, ctx, &row).await
}

pub async fn events(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    q: &WorkflowEventList,
) -> Result<Page<WorkflowEvent>, AppError> {
    let mut conn = pool.acquire().await?;
    visible_instance(&mut conn, ctx, id).await?;
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM cmdb.workflow_instance_events WHERE instance_id = $1")
        .bind(id)
        .fetch_one(&mut *conn)
        .await?;
    let data = sqlx::query_as::<_, WorkflowEvent>(
        "SELECT id, kind, transition_key, from_state_key, to_state_key, from_version_no, to_version_no, occurred_at,
                actor_type, actor_name, comment, field_changes, request_id
         FROM cmdb.workflow_instance_events WHERE instance_id = $1 ORDER BY id LIMIT $2 OFFSET $3",
    )
    .bind(id)
    .bind(q.limit)
    .bind(q.offset)
    .fetch_all(&mut *conn)
    .await?;
    Ok(Page { data, page: q.page_meta(total) })
}

pub async fn list(
    pool: &PgPool,
    ctx: &RequestContext,
    q: &WorkflowInstanceList,
) -> Result<Page<WorkflowInstance>, AppError> {
    // Every filter is a condition on `workflow_instances` alone, so the page and the count are found on
    // that table (and its sort indexes) and only the page's rows are joined.
    let scope = ctx.class_scope(ClassOp::View);
    let filter = |w: &mut Where<'_>| {
        if let Some(classes) = &scope {
            w.and()
                .push("wi.ci_id IN (SELECT id FROM cmdb.configuration_items WHERE class_id = ANY(")
                .push_bind(classes.clone())
                .push("))");
        }
        if let Some(k) = &q.definition_key {
            w.and()
                .push("wi.definition_id IN (SELECT id FROM cmdb.workflow_definitions WHERE lower(key) = lower(")
                .push_bind(k.clone())
                .push("))");
        }
        if let Some(k) = &q.state_key {
            w.and()
                .push("wi.current_state_id IN (SELECT id FROM cmdb.workflow_states WHERE key = ")
                .push_bind(k.clone())
                .push(")");
        }
        if let Some(st) = q.status {
            w.and().push("wi.status = ").push_bind(st);
        }
        if let Some(k) = &q.class_key {
            w.and()
                .push(
                    "wi.ci_id IN (SELECT ci.id FROM cmdb.configuration_items ci \
                     JOIN cmdb.ci_classes c ON c.id = ci.class_id WHERE c.key = ",
                )
                .push_bind(k.clone())
                .push(")");
        }
        if let Some(ci) = q.ci_id {
            w.and().push("wi.ci_id = ").push_bind(ci);
        }
    };
    let column = match q.sort.field.as_str() {
        "startedAt" => "wi.started_at",
        _ => "wi.last_transition_at",
    };
    let order = format!("{column} {dir}, wi.id {dir}", dir = q.sort.dir());
    let mut conn = pool.acquire().await?;
    let (ids, total) = crud::select_page_counted::<(Uuid,)>(
        &mut conn,
        "cmdb.workflow_instances wi",
        "cmdb.workflow_instances wi",
        "wi.id",
        &filter,
        &order,
        q.limit,
        q.offset,
    )
    .await?;
    let ids: Vec<Uuid> = ids.into_iter().map(|(id,)| id).collect();
    let rows = sqlx::query_as::<_, InstanceRow>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM {FROM} WHERE wi.id = ANY($1) ORDER BY {order}"
    )))
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await?;
    Ok(Page { data: rows.iter().map(InstanceRow::dto).collect(), page: q.page_meta(total) })
}

pub async fn summary(
    pool: &PgPool,
    ctx: &RequestContext,
    q: &WorkflowInstanceSummaryQuery,
) -> Result<WorkflowInstanceSummary, AppError> {
    // Counted per state id on the instances first (the CI join only for a restricted caller), then the few
    // states are named and merged across versions.
    let data = sqlx::query_as::<_, WorkflowStateCount>(
        "WITH per_state AS (
           SELECT wi.current_state_id, count(*) AS n
           FROM cmdb.workflow_instances wi
           WHERE wi.status = 'active'
             AND ($1::uuid[] IS NULL
                  OR wi.ci_id IN (SELECT id FROM cmdb.configuration_items WHERE class_id = ANY($1)))
             AND ($2::text IS NULL
                  OR wi.definition_id IN (SELECT id FROM cmdb.workflow_definitions WHERE lower(key) = lower($2)))
           GROUP BY wi.current_state_id)
         SELECT d.id AS definition_id, d.key AS definition_key, s.key AS state_key,
                (array_agg(s.name ORDER BY v.version_no DESC))[1] AS state_name,
                (array_agg(s.category ORDER BY v.version_no DESC))[1] AS category,
                sum(p.n)::bigint AS count
         FROM per_state p
         JOIN cmdb.workflow_states s ON s.id = p.current_state_id
         JOIN cmdb.workflow_versions v ON v.id = s.version_id
         JOIN cmdb.workflow_definitions d ON d.id = v.definition_id
         GROUP BY d.id, d.key, s.key
         ORDER BY d.key, min(s.sort_order), s.key",
    )
    .bind(ctx.class_scope(ClassOp::View))
    .bind(&q.definition_key)
    .fetch_all(pool)
    .await?;
    Ok(WorkflowInstanceSummary { data })
}

pub async fn of_ci(pool: &PgPool, ctx: &RequestContext, ci: Uuid) -> Result<CiWorkflows, AppError> {
    let mut conn = pool.acquire().await?;
    let found: Option<(Uuid, bool)> =
        sqlx::query_as("SELECT class_id, deleted_at IS NOT NULL FROM cmdb.configuration_items WHERE id = $1")
            .bind(ci)
            .fetch_optional(&mut *conn)
            .await?;
    let (class_id, deleted) = found.ok_or_else(|| AppError::missing(CI, ci))?;
    ctx.require_class_visible(class_id, CI, ci)?;
    let rows: Vec<InstanceRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM {FROM}
         WHERE wi.ci_id = $1 AND (wi.status = 'active' OR wi.id IN (
           SELECT e.id FROM cmdb.workflow_instances e WHERE e.ci_id = $1 AND e.status <> 'active'
           ORDER BY e.ended_at DESC LIMIT $2))
         ORDER BY (wi.status = 'active') DESC, coalesce(wi.ended_at, wi.started_at) DESC, wi.id"
    )))
    .bind(ci)
    .bind(RECENT_ENDED)
    .fetch_all(&mut *conn)
    .await?;
    let model = Model::load(&mut conn).await?;
    let values = current_values(&mut conn, &model, ci).await?;
    let by_id = condition_values(&mut conn, &model, class_id, &values).await?;
    let mut data = Vec::with_capacity(rows.len());
    for row in &rows {
        data.push(view(&mut conn, ctx, &model, row, &values, &by_id).await?);
    }
    let startable = if deleted || !may_edit(ctx, class_id) {
        Vec::new()
    } else {
        let lineage: Vec<Uuid> = model.lineage(class_id).iter().map(|c| c.id).collect();
        // A workflow of a parent type the caller may not view is missing to
        // `start`, so it is not offered here either (GH#608).
        sqlx::query_as::<_, WorkflowStartable>(
            "SELECT d.id AS definition_id, d.key AS definition_key, d.name AS definition_name, v.version_no
             FROM cmdb.workflow_definitions d JOIN cmdb.workflow_versions v ON v.id = d.current_version_id
             WHERE d.is_active AND d.class_id = ANY($1) AND (d.include_subclasses OR d.class_id = $2)
               AND ($4::uuid[] IS NULL OR d.class_id = ANY($4))
               AND NOT EXISTS (SELECT 1 FROM cmdb.workflow_instances wi
                               WHERE wi.definition_id = d.id AND wi.ci_id = $3 AND wi.status = 'active')
             ORDER BY lower(d.name), d.key",
        )
        .bind(&lineage)
        .bind(class_id)
        .bind(ci)
        .bind(ctx.class_scope(ClassOp::View))
        .fetch_all(&mut *conn)
        .await?
    };
    let controlled_fields = StateFields::load(&mut conn).await?.driven_keys(&model, class_id);
    Ok(CiWorkflows { data, startable, controlled_fields })
}

// ---------------------------------------------------------------------------
// Transition
// ---------------------------------------------------------------------------

/// Item validation errors name `attributes.<key>`; the transition body calls them `fields.<key>`.
fn as_transition_fields(mut e: AppError) -> AppError {
    for d in e.details.iter_mut().flatten() {
        if let Some(rest) = d.field.strip_prefix("attributes.") {
            d.field = format!("fields.{rest}");
        }
    }
    e
}

pub async fn transition(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    b: &WorkflowTransitionRun,
) -> Result<WorkflowInstance, AppError> {
    let mut tx = pool.begin().await?;
    let row = lock(&mut tx, ctx, id).await?;
    check_active(&row)?;
    check_version(&row, b.expected_version)?;
    ctx.require_class(row.class_id, ClassOp::Edit)?;
    let p = pinned(&mut tx, row.version_id).await?;
    let Some(t) = p.transitions.iter().find(|t| t.key == b.transition_key) else {
        return Err(AppError::field(
            "transitionKey",
            format!(
                "Version {} of workflow {} has no transition {}",
                row.version_no, row.definition_key, b.transition_key
            ),
            "unknown_transition",
        ));
    };
    if t.from_state_id != row.current_state_id {
        return Err(detail_error(
            ErrorCode::Conflict,
            format!("Transition {} does not leave the instance's state {}", t.key, row.state_key),
            "transitionKey",
            "not_from_current_state",
        ));
    }
    if !granted(&mut tx, ctx, row.definition_id).await?.has(&t.key) {
        return Err(AppError::new(
            ErrorCode::Forbidden,
            format!(
                "None of your permission profiles is granted transition {} of workflow {}",
                t.key, row.definition_key
            ),
        ));
    }
    let to = p.state(t.to_state_id).ok_or_else(AppError::internal)?;
    let model = Model::load(&mut tx).await?;

    // The fields sent: only the transition's, validated as a PATCH would.
    let allowed: HashMap<&str, &PinnedField> =
        p.fields_of(t.id).filter_map(|f| model.field(f.attribute_id).map(|a| (a.key.as_str(), f))).collect();
    let unknown: Vec<FieldError> = b
        .fields
        .keys()
        .filter(|k| !allowed.contains_key(k.as_str()))
        .map(|k| FieldError {
            location: FieldLocation::Body,
            field: format!("fields.{k}"),
            message: format!("Transition {} does not take field {k}", t.key),
            code: "not_a_transition_field".into(),
        })
        .collect();
    if !unknown.is_empty() {
        return Err(AppError::validation(unknown));
    }
    items::update_for_workflow(&mut tx, ctx, row.ci_id, row.class_id, b.fields.clone(), false)
        .await
        .map_err(as_transition_fields)?;

    // Required fields, the comment and the conditions, on the CI's values with the ones sent.
    let mut values = current_values(&mut tx, &model, row.ci_id).await?;
    for (k, v) in &b.fields {
        values.insert(k.clone(), v.clone());
    }
    let mut failed: Vec<FieldError> = Vec::new();
    for (key, f) in &allowed {
        if f.is_required && !is_set(values.get(*key)) {
            failed.push(FieldError {
                location: FieldLocation::Body,
                field: format!("fields.{key}"),
                message: format!(
                    "{} is required for transition {}",
                    model.field(f.attribute_id).map_or(*key, |a| &a.label),
                    t.key
                ),
                code: "required".into(),
            });
        }
    }
    let comment = b.comment.as_deref().filter(|c| !c.trim().is_empty());
    if t.requires_comment && comment.is_none() {
        failed.push(FieldError {
            location: FieldLocation::Body,
            field: "comment".into(),
            message: format!("Transition {} requires a comment", t.key),
            code: "comment_required".into(),
        });
    }
    let by_id = condition_values(&mut tx, &model, row.class_id, &values).await?;
    let cx = Context { model: &model, values: &values, by_id: &by_id };
    failed.extend(blocked_by(t, &cx).into_iter().map(|r| FieldError {
        location: FieldLocation::Body,
        field: r.field,
        message: r.message,
        code: r.code,
    }));
    if !failed.is_empty() {
        failed.sort_by(|a, b| a.field.cmp(&b.field));
        let n = failed.len();
        return Err(AppError::new(
            ErrorCode::WorkflowConditionFailed,
            format!(
                "Transition {} cannot run: {n} condition{} not met (see details)",
                t.key,
                if n == 1 { " is" } else { "s are" }
            ),
        )
        .with_details(failed));
    }

    // Write: the fields sent and the state field, then the instance, its event and the audit row.
    let mut attributes = b.fields.clone();
    attributes.extend(state_value(&model, row.state_attribute_id, to));
    let changes = write_ci(&mut tx, ctx, row.ci_id, row.class_id, attributes).await.map_err(as_transition_fields)?;
    move_to(&mut tx, id, to).await?;
    insert_event(
        &mut tx,
        ctx,
        NewEvent {
            instance: id,
            kind: "transition",
            transition_key: Some(&t.key),
            from_state_key: Some(&row.state_key),
            to_state_key: &to.key,
            to_version_no: row.version_no,
            comment,
            field_changes: changes.clone(),
        },
    )
    .await?;
    let entry = AuditEntry {
        action: AuditAction::WorkflowTransition,
        entity_type: "configuration_items",
        entity_id: row.ci_id,
        old_value: Some(json!({ "instanceId": id, "definitionKey": row.definition_key, "stateKey": row.state_key,
            "version": row.version })),
        new_value: Some(json!({ "instanceId": id, "definitionKey": row.definition_key, "transitionKey": t.key,
            "stateKey": to.key, "version": row.version + 1, "comment": comment, "fields": changes })),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    let after = reload(&mut tx, id).await?;
    tx.commit().await?;
    Ok(after.dto())
}

// ---------------------------------------------------------------------------
// Cancel and force
// ---------------------------------------------------------------------------

pub async fn cancel(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    b: &WorkflowInstanceCancel,
) -> Result<WorkflowInstance, AppError> {
    let mut tx = pool.begin().await?;
    let row = lock(&mut tx, ctx, id).await?;
    check_active(&row)?;
    check_version(&row, b.expected_version)?;
    if !may_manage(ctx) {
        ctx.require_class(row.class_id, ClassOp::Edit)?;
        if !granted(&mut tx, ctx, row.definition_id).await?.has(CANCEL_KEY) {
            return Err(AppError::new(
                ErrorCode::Forbidden,
                format!(
                    "Cancelling workflow {} needs the workflows.manage permission or its _cancel grant",
                    row.definition_key
                ),
            ));
        }
    }
    let reason = b.reason.trim();
    sqlx::query(
        "UPDATE cmdb.workflow_instances SET status = 'cancelled', ended_at = now(), version = version + 1
         WHERE id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    insert_event(
        &mut tx,
        ctx,
        NewEvent {
            instance: id,
            kind: "cancel",
            transition_key: None,
            from_state_key: Some(&row.state_key),
            to_state_key: &row.state_key,
            to_version_no: row.version_no,
            comment: Some(reason),
            field_changes: None,
        },
    )
    .await?;
    let entry = AuditEntry {
        action: AuditAction::WorkflowCancel,
        entity_type: "configuration_items",
        entity_id: row.ci_id,
        old_value: None,
        new_value: Some(json!({ "instanceId": id, "definitionKey": row.definition_key, "stateKey": row.state_key,
            "version": row.version + 1, "reason": reason })),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    let after = reload(&mut tx, id).await?;
    tx.commit().await?;
    Ok(after.dto())
}

pub async fn force(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    b: &WorkflowInstanceForce,
) -> Result<WorkflowInstance, AppError> {
    let mut tx = pool.begin().await?;
    let row = lock(&mut tx, ctx, id).await?;
    check_active(&row)?;
    check_version(&row, b.expected_version)?;
    // It writes the state field like a transition does.
    ctx.require_class(row.class_id, ClassOp::Edit)?;
    let p = pinned(&mut tx, row.version_id).await?;
    let Some(to) = p.state_by_key(&b.state_key) else {
        return Err(AppError::field(
            "stateKey",
            format!("Version {} of workflow {} has no state {}", row.version_no, row.definition_key, b.state_key),
            "unknown_state",
        ));
    };
    if to.id == row.current_state_id {
        return Err(detail_error(
            ErrorCode::Conflict,
            format!("The instance is already in state {}", to.key),
            "stateKey",
            "same_state",
        ));
    }
    let model = Model::load(&mut tx).await?;
    let reason = b.reason.trim();
    let changes =
        write_ci(&mut tx, ctx, row.ci_id, row.class_id, state_value(&model, row.state_attribute_id, to)).await?;
    move_to(&mut tx, id, to).await?;
    insert_event(
        &mut tx,
        ctx,
        NewEvent {
            instance: id,
            kind: "force",
            transition_key: None,
            from_state_key: Some(&row.state_key),
            to_state_key: &to.key,
            to_version_no: row.version_no,
            comment: Some(reason),
            field_changes: changes.clone(),
        },
    )
    .await?;
    let entry = AuditEntry {
        action: AuditAction::WorkflowForce,
        entity_type: "configuration_items",
        entity_id: row.ci_id,
        old_value: Some(json!({ "instanceId": id, "definitionKey": row.definition_key, "stateKey": row.state_key,
            "version": row.version })),
        new_value: Some(json!({ "instanceId": id, "definitionKey": row.definition_key, "stateKey": to.key,
            "version": row.version + 1, "reason": reason, "fields": changes })),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    let after = reload(&mut tx, id).await?;
    tx.commit().await?;
    Ok(after.dto())
}

/// A CI soft delete cancels its running instances, in the delete's
/// transaction (the caller holds the CI row lock, so the order is CI then
/// instance). Their events name the system as actor; the returned
/// `workflow.cancel` audit rows (reason `ci_deleted`) go with the delete's.
pub async fn cancel_for_deleted_ci(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    ci: Uuid,
) -> Result<Vec<AuditEntry>, AppError> {
    let rows: Vec<(Uuid, String, String, i32, i32)> = sqlx::query_as(
        "UPDATE cmdb.workflow_instances wi SET status = 'cancelled', ended_at = now(), version = wi.version + 1
         FROM cmdb.workflow_definitions d, cmdb.workflow_states s, cmdb.workflow_versions v
         WHERE wi.ci_id = $1 AND wi.status = 'active'
           AND d.id = wi.definition_id AND s.id = wi.current_state_id AND v.id = wi.version_id
         RETURNING wi.id, d.key, s.key, v.version_no, wi.version",
    )
    .bind(ci)
    .fetch_all(&mut *conn)
    .await?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    const REASON: &str = "The configuration item was deleted";
    let ids: Vec<Uuid> = rows.iter().map(|r| r.0).collect();
    let states: Vec<&str> = rows.iter().map(|r| r.2.as_str()).collect();
    let versions: Vec<i32> = rows.iter().map(|r| r.3).collect();
    sqlx::query(
        "INSERT INTO cmdb.workflow_instance_events
           (instance_id, kind, from_state_key, to_state_key, to_version_no, actor_type, actor_name, comment, request_id)
         SELECT u.id, 'cancel', u.state, u.state, u.version_no, 'system', 'system', $4, $5
         FROM unnest($1::uuid[], $2::text[], $3::int[]) AS u(id, state, version_no)",
    )
    .bind(&ids)
    .bind(&states)
    .bind(&versions)
    .bind(REASON)
    .bind(&ctx.request_id)
    .execute(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, key, state, _, version)| AuditEntry {
            action: AuditAction::WorkflowCancel,
            entity_type: "configuration_items",
            entity_id: ci,
            old_value: None,
            new_value: Some(json!({ "instanceId": id, "definitionKey": key, "stateKey": state, "version": version,
                "reason": "ci_deleted" })),
        })
        .collect())
}
