//! Adopting workflows on CIs (slice S3b, SHAA-1698; design §3.2, §8.2, §9,
//! Amendment 2, Q2b).
//!
//! **Auto-start.** A workflow with `auto_start` starts on every new CI it
//! covers: a CI created through the API (and so the UI) gets its instance,
//! start event and `workflow.start` audit row in the create's transaction,
//! and its state field the initial state's value from the start. An import
//! starts them set-based for the CIs each chunk creates, with one
//! `workflow.start` audit row for the whole import carrying the count (Q2b);
//! the start events are still one per instance.
//!
//! **Bootstrap.** `POST /admin/workflow-definitions/{id}/bootstrap` starts
//! the workflow on the live CIs it covers that have no running instance of
//! it, each in the state its state field value maps to. It runs in batches
//! of [`BATCH`] CIs, each its own transaction, which locks the batch's CI rows
//! first (in id order) and inserts instances second, the lock order of every
//! workflow path. The `workflow_instances_one_active` index makes it
//! idempotent: a second run starts nothing.

use std::collections::HashMap;

use serde_json::{Map, Value, json};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::runtime::{self, NewEvent, PinnedState};
use super::schemas::*;
use super::service;
use crate::api::context::RequestContext;
use crate::data::crud::{AuditAction, AuditEntry};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::schema::model::{Field, Model};

/// CIs per bootstrap transaction.
pub const BATCH: i64 = 1_000;

// ---------------------------------------------------------------------------
// Auto-start
// ---------------------------------------------------------------------------

/// A workflow that starts on its own on new CIs of a type.
#[derive(Debug, Clone)]
pub struct AutoStart {
    definition_id: Uuid,
    key: String,
    version_id: Uuid,
    version_no: i32,
    initial: PinnedState,
    /// The state field, when the workflow drives one and its initial state sets it.
    state: Option<(Field, Uuid)>,
}

#[derive(sqlx::FromRow)]
struct AutoStartRow {
    id: Uuid,
    key: String,
    class_id: Uuid,
    include_subclasses: bool,
    state_attribute_id: Option<Uuid>,
    version_id: Uuid,
    version_no: i32,
}

/// The active, published workflows that start on their own on a new CI of `class_id`.
pub async fn auto_starts(conn: &mut PgConnection, model: &Model, class_id: Uuid) -> Result<Vec<AutoStart>, AppError> {
    let rows: Vec<AutoStartRow> = sqlx::query_as(
        "SELECT d.id, d.key, d.class_id, d.include_subclasses, d.state_attribute_id,
                v.id AS version_id, v.version_no
         FROM cmdb.workflow_definitions d JOIN cmdb.workflow_versions v ON v.id = d.current_version_id
         WHERE d.is_active AND d.auto_start
         ORDER BY d.key",
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut out = Vec::new();
    for r in rows {
        let covers = r.class_id == class_id
            || (r.include_subclasses && model.lineage(class_id).iter().any(|c| c.id == r.class_id));
        if !covers {
            continue;
        }
        let p = runtime::pinned(conn, r.version_id).await?;
        let initial = p.initial_state_id.and_then(|id| p.state(id)).ok_or_else(AppError::internal)?.clone();
        let field = r.state_attribute_id.and_then(|a| model.field(a)).cloned();
        let state = field.zip(initial.state_value_id);
        out.push(AutoStart {
            definition_id: r.id,
            key: r.key,
            version_id: r.version_id,
            version_no: r.version_no,
            initial,
            state,
        });
    }
    Ok(out)
}

/// Whether any of `starts` sets a state field.
pub fn sets_state(starts: &[AutoStart]) -> bool {
    starts.iter().any(|s| s.state.is_some())
}

/// Gives a new CI's attributes the state field values its auto-started workflows start with.
pub fn seed(starts: &[AutoStart], attributes: &mut Map<String, Value>) {
    for s in starts {
        if let Some((field, value)) = &s.state {
            attributes.insert(field.key.clone(), Value::String(value.to_string()));
        }
    }
}

async fn insert_instance(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    s: &AutoStart,
    ci: Uuid,
) -> Result<Uuid, AppError> {
    let (user_id, user_name) = runtime::starter(ctx);
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO cmdb.workflow_instances
           (definition_id, version_id, ci_id, current_state_id, status, started_by_id, started_by_name)
         VALUES ($1, $2, $3, $4, 'active', $5, $6) RETURNING id",
    )
    .bind(s.definition_id)
    .bind(s.version_id)
    .bind(ci)
    .bind(s.initial.id)
    .bind(user_id)
    .bind(&user_name)
    .fetch_one(conn)
    .await?;
    Ok(id)
}

/// Starts `starts` on the CI `ci` this transaction just created (its state
/// field already holds the initial values, see [`seed`]). Returns the
/// `workflow.start` audit rows, for the caller to write after the CI's `create`.
pub async fn start_created(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    ci: Uuid,
    starts: &[AutoStart],
) -> Result<Vec<AuditEntry>, AppError> {
    let mut entries = Vec::with_capacity(starts.len());
    for s in starts {
        let id = insert_instance(conn, ctx, s, ci).await?;
        let event = NewEvent {
            instance: id,
            kind: "start",
            transition_key: None,
            from_state_key: None,
            to_state_key: &s.initial.key,
            to_version_no: s.version_no,
            comment: None,
            field_changes: None,
        };
        runtime::insert_event(conn, ctx, event).await?;
        entries.push(AuditEntry {
            action: AuditAction::WorkflowStart,
            entity_type: "configuration_items",
            entity_id: ci,
            old_value: None,
            new_value: Some(json!({
                "instanceId": id, "definitionKey": s.key, "versionNo": s.version_no, "stateKey": s.initial.key,
                "autoStart": true,
            })),
        });
    }
    Ok(entries)
}

/// Starts `starts` on the CIs `ids` an import chunk just created, set-based:
/// their state field, the instances and the start events in a few statements
/// of the chunk's transaction. No audit row: the import writes one for all
/// of them when it ends (Q2b). Returns the instances started.
pub async fn start_imported(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    model: &Model,
    ids: &[Uuid],
    starts: &[AutoStart],
) -> Result<u32, AppError> {
    if ids.is_empty() {
        return Ok(0);
    }
    let (user_id, user_name) = runtime::starter(ctx);
    let mut started = 0;
    for s in starts {
        if let Some((field, value)) = &s.state {
            let table = model.table(field.class_id).ok_or_else(AppError::internal)?;
            let sql = format!("UPDATE {} SET {} = $1 WHERE id = ANY($2)", table.sql(), field.column());
            sqlx::query(sqlx::AssertSqlSafe(sql)).persistent(false).bind(value).bind(ids).execute(&mut *conn).await?;
        }
        let n = sqlx::query(
            "WITH started AS (
               INSERT INTO cmdb.workflow_instances
                 (definition_id, version_id, ci_id, current_state_id, status, started_by_id, started_by_name)
               SELECT $1, $2, u.id, $3, 'active', $4, $5 FROM unnest($6::uuid[]) AS u(id)
               ON CONFLICT DO NOTHING
               RETURNING id)
             INSERT INTO cmdb.workflow_instance_events
               (instance_id, kind, to_state_key, to_version_no, actor_type, actor_id, actor_name, request_id)
             SELECT id, 'start', $7, $8, $9, $10, $11, $12 FROM started",
        )
        .bind(s.definition_id)
        .bind(s.version_id)
        .bind(s.initial.id)
        .bind(user_id)
        .bind(&user_name)
        .bind(ids)
        .bind(&s.initial.key)
        .bind(s.version_no)
        .bind(ctx.actor.actor_type.as_str())
        .bind(&ctx.actor.id)
        .bind(&ctx.actor.name)
        .bind(&ctx.request_id)
        .execute(&mut *conn)
        .await?
        .rows_affected();
        started += n as u32;
    }
    Ok(started)
}

/// The one `workflow.start` audit row of an import that auto-started instances (Q2b).
pub fn import_audit(job: Uuid, class_key: Option<&str>, started: u32) -> AuditEntry {
    AuditEntry {
        action: AuditAction::WorkflowStart,
        entity_type: "import_jobs",
        entity_id: job,
        old_value: None,
        new_value: Some(json!({ "importJobId": job, "classKey": class_key, "instances": started, "autoStart": true })),
    }
}

// ---------------------------------------------------------------------------
// Bootstrap
// ---------------------------------------------------------------------------

fn conflict(message: String, code: &str) -> AppError {
    AppError::new(ErrorCode::Conflict, message).with_details(vec![FieldError {
        location: FieldLocation::Params,
        field: "id".into(),
        message: "See the message".into(),
        code: code.into(),
    }])
}

/// The state each state field value starts in: the first non-terminal state
/// that maps it, else the first terminal one (in the version's state order).
fn state_by_value(states: &[PinnedState]) -> HashMap<Uuid, &PinnedState> {
    let mut out: HashMap<Uuid, &PinnedState> = HashMap::new();
    for s in states {
        let Some(v) = s.state_value_id else { continue };
        match out.get(&v) {
            Some(have) if !have.is_terminal || s.is_terminal => {}
            _ => {
                out.insert(v, s);
            }
        }
    }
    out
}

/// Counts of a bootstrap, by state and unmapped value.
#[derive(Default)]
struct Tally {
    started: i64,
    per_state: HashMap<Uuid, i64>,
    unmapped: HashMap<Option<Uuid>, i64>,
}

impl Tally {
    fn add(&mut self, value: Option<Uuid>, mapped: Option<&PinnedState>, n: i64) {
        match mapped {
            Some(s) => {
                *self.per_state.entry(s.id).or_default() += n;
                if !s.is_terminal {
                    self.started += n;
                }
            }
            None => *self.unmapped.entry(value).or_default() += n,
        }
    }
}

/// The definition a bootstrap runs, checked: active, published, with a state field.
struct Target {
    definition: WorkflowDefinition,
    version_id: Uuid,
    field: Field,
    classes: Vec<Uuid>,
}

async fn target(conn: &mut PgConnection, ctx: &RequestContext, id: Uuid) -> Result<Target, AppError> {
    let d = service::load(conn, id, false).await?;
    let model = Model::load(conn).await?;
    let Some(field) = d.state_attribute_id.and_then(|a| model.field(a)).cloned() else {
        return Err(conflict(
            format!("Workflow {} drives no state field: start its instances one by one", d.key),
            "no_state_field",
        ));
    };
    let version_id: Option<Uuid> =
        sqlx::query_scalar("SELECT current_version_id FROM cmdb.workflow_definitions WHERE id = $1")
            .bind(id)
            .fetch_one(&mut *conn)
            .await?;
    let Some(version_id) = version_id else {
        return Err(conflict(format!("Workflow {} has no published version yet", d.key), "unpublished"));
    };
    if !d.is_active {
        return Err(conflict(format!("Workflow {} is inactive: it starts no new instances", d.key), "inactive"));
    }
    let classes = service::covered(&model, &d);
    // The counts would tell about CIs the caller may not view.
    if !ctx.may_view_all(&classes) {
        return Err(AppError::new(
            ErrorCode::Forbidden,
            "This workflow covers types you may not view; ask an administrator who may view them all",
        ));
    }
    Ok(Target { definition: d, version_id, field, classes })
}

/// Live CIs it covers that already run it.
async fn already_running(conn: &mut PgConnection, t: &Target) -> Result<i64, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT count(*) FROM cmdb.workflow_instances wi JOIN cmdb.configuration_items ci ON ci.id = wi.ci_id
         WHERE wi.definition_id = $1 AND wi.status = 'active' AND ci.deleted_at IS NULL AND ci.class_id = ANY($2)",
    )
    .bind(t.definition.id)
    .bind(&t.classes)
    .fetch_one(conn)
    .await?)
}

async fn result(
    conn: &mut PgConnection,
    t: &Target,
    states: &[PinnedState],
    version_no: i32,
    dry_run: bool,
    already_running: i64,
    tally: Tally,
) -> Result<WorkflowBootstrapResult, AppError> {
    let mut out_states = Vec::new();
    let mut skipped_terminal = 0;
    for s in states {
        let Some(n) = tally.per_state.get(&s.id).copied() else { continue };
        if s.is_terminal {
            skipped_terminal += n;
        }
        out_states.push((s, n));
    }
    let value_ids: Vec<Uuid> = out_states
        .iter()
        .filter_map(|(s, _)| s.state_value_id)
        .chain(tally.unmapped.keys().flatten().copied())
        .collect();
    let values: HashMap<Uuid, (String, String)> = sqlx::query_as::<_, (Uuid, String, String)>(
        "SELECT id, key, name FROM cmdb.lookup_list_values WHERE id = ANY($1)",
    )
    .bind(&value_ids)
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|(id, k, n)| (id, (k, n)))
    .collect();
    let mut unmapped: Vec<WorkflowBootstrapUnmapped> = tally
        .unmapped
        .iter()
        .map(|(v, n)| {
            let named = v.and_then(|v| values.get(&v));
            WorkflowBootstrapUnmapped {
                value_id: *v,
                value_key: named.map(|x| x.0.clone()),
                value_name: named.map(|x| x.1.clone()),
                count: *n,
            }
        })
        .collect();
    unmapped.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.value_key.cmp(&b.value_key)));
    Ok(WorkflowBootstrapResult {
        dry_run,
        definition_key: t.definition.key.clone(),
        version_no,
        started: tally.started,
        already_running,
        states: out_states
            .into_iter()
            .map(|(s, n)| WorkflowBootstrapState {
                state_key: s.key.clone(),
                state_name: s.name.clone(),
                value_key: s.state_value_id.and_then(|v| values.get(&v)).map(|x| x.0.clone()).unwrap_or_default(),
                terminal: s.is_terminal,
                count: n,
            })
            .collect(),
        skipped_terminal,
        skipped_unmapped: unmapped.iter().map(|u| u.count).sum(),
        unmapped,
    })
}

/// `SELECT ci.id, <state field> FROM` the covered live CIs without a running instance.
fn candidates_sql(t: &Target, model: &Model) -> Result<String, AppError> {
    let table = model.table(t.field.class_id).ok_or_else(AppError::internal)?;
    Ok(format!(
        "SELECT ci.id, f.{col} AS value
         FROM cmdb.configuration_items ci LEFT JOIN {table} f ON f.id = ci.id
         WHERE ci.class_id = ANY($1) AND ci.deleted_at IS NULL
           AND NOT EXISTS (SELECT 1 FROM cmdb.workflow_instances wi
                           WHERE wi.ci_id = ci.id AND wi.definition_id = $2 AND wi.status = 'active')",
        col = t.field.column(),
        table = table.sql(),
    ))
}

pub async fn bootstrap(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    b: &WorkflowBootstrap,
) -> Result<WorkflowBootstrapResult, AppError> {
    let mut conn = pool.acquire().await?;
    let t = target(&mut conn, ctx, id).await?;
    let model = Model::load(&mut conn).await?;
    let p = runtime::pinned(&mut conn, t.version_id).await?;
    let version_no: i32 = sqlx::query_scalar("SELECT version_no FROM cmdb.workflow_versions WHERE id = $1")
        .bind(t.version_id)
        .fetch_one(&mut *conn)
        .await?;
    let by_value = state_by_value(&p.states);
    let candidates = candidates_sql(&t, &model)?;
    let running = already_running(&mut conn, &t).await?;

    if b.dry_run {
        let rows: Vec<(Option<Uuid>, i64)> =
            sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT c.value, count(*) FROM ({candidates}) c GROUP BY 1")))
                .bind(&t.classes)
                .bind(t.definition.id)
                .fetch_all(&mut *conn)
                .await?;
        let mut tally = Tally::default();
        for (value, n) in rows {
            tally.add(value, value.and_then(|v| by_value.get(&v).copied()), n);
        }
        return result(&mut conn, &t, &p.states, version_no, true, running, tally).await;
    }
    drop(conn);

    // Who the instances and their audit rows name: the system, on behalf of the caller.
    let (user_id, user_name) = runtime::starter(ctx);
    let system = RequestContext::system(format!("Workflow bootstrap by {user_name}"), ctx.request_id.clone());
    let mut tally = Tally::default();
    let mut after = Uuid::nil();
    loop {
        let mut tx = pool.begin().await?;
        // The workflow may have changed since the last batch; it must still run this version.
        let now: Option<(bool, Option<Uuid>)> =
            sqlx::query_as("SELECT is_active, current_version_id FROM cmdb.workflow_definitions WHERE id = $1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        if now != Some((true, Some(t.version_id))) {
            return Err(conflict(
                format!(
                    "Workflow {} was deactivated or published again during the bootstrap, after {} instances were \
                     started. Run the bootstrap again: CIs that run it already are left alone.",
                    t.definition.key, tally.started
                ),
                "changed_during_bootstrap",
            ));
        }
        let ids: Vec<Uuid> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT c.id FROM ({candidates}) c WHERE c.id > $3 ORDER BY c.id LIMIT $4"
        )))
        .bind(&t.classes)
        .bind(t.definition.id)
        .bind(after)
        .bind(BATCH)
        .fetch_all(&mut *tx)
        .await?;
        let Some(last) = ids.last().copied() else { break };
        after = last;
        // CI rows first, in id order (the lock order of every workflow path), then their values.
        let locked: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM cmdb.configuration_items WHERE id = ANY($1) AND deleted_at IS NULL ORDER BY id FOR UPDATE",
        )
        .bind(&ids)
        .fetch_all(&mut *tx)
        .await?;
        let rows: Vec<(Uuid, Option<Uuid>)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT c.id, c.value FROM ({candidates}) c WHERE c.id = ANY($3)"
        )))
        .bind(&t.classes)
        .bind(t.definition.id)
        .bind(&locked)
        .fetch_all(&mut *tx)
        .await?;
        let mut ci_ids = Vec::new();
        let mut state_ids = Vec::new();
        let mut batch = Tally::default();
        for (ci, value) in rows {
            let mapped = value.and_then(|v| by_value.get(&v).copied());
            if let Some(s) = mapped.filter(|s| !s.is_terminal) {
                ci_ids.push(ci);
                state_ids.push(s.id);
            } else {
                batch.add(value, mapped, 1);
            }
        }
        let started: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
            "INSERT INTO cmdb.workflow_instances
               (definition_id, version_id, ci_id, current_state_id, status, started_by_id, started_by_name)
             SELECT $1, $2, u.ci, u.state, 'active', $3, $4 FROM unnest($5::uuid[], $6::uuid[]) AS u(ci, state)
             ON CONFLICT DO NOTHING
             RETURNING id, ci_id, current_state_id",
        )
        .bind(t.definition.id)
        .bind(t.version_id)
        .bind(user_id)
        .bind(&user_name)
        .bind(&ci_ids)
        .bind(&state_ids)
        .fetch_all(&mut *tx)
        .await?;
        let state_of = |id: Uuid| p.states.iter().find(|s| s.id == id);
        let instances: Vec<Uuid> = started.iter().map(|(i, _, _)| *i).collect();
        let keys: Vec<String> =
            started.iter().map(|(_, _, s)| state_of(*s).map(|s| s.key.clone()).unwrap_or_default()).collect();
        sqlx::query(
            "INSERT INTO cmdb.workflow_instance_events
               (instance_id, kind, to_state_key, to_version_no, actor_type, actor_id, actor_name, request_id)
             SELECT u.id, 'start', u.state_key, $3, 'system', NULL, $4, $5
             FROM unnest($1::uuid[], $2::text[]) AS u(id, state_key)",
        )
        .bind(&instances)
        .bind(&keys)
        .bind(version_no)
        .bind(&system.actor.name)
        .bind(&ctx.request_id)
        .execute(&mut *tx)
        .await?;
        let entries: Vec<AuditEntry> = started
            .iter()
            .zip(&keys)
            .map(|((instance, ci, _), key)| AuditEntry {
                action: AuditAction::WorkflowStart,
                entity_type: "configuration_items",
                entity_id: *ci,
                old_value: None,
                new_value: Some(json!({
                    "instanceId": instance, "definitionKey": t.definition.key, "versionNo": version_no,
                    "stateKey": key, "bootstrap": true, "requestedBy": user_name,
                })),
            })
            .collect();
        crate::data::crud::write_audit(&mut tx, &system, entries).await?;
        tx.commit().await?;
        for (_, _, s) in &started {
            batch.add(None, state_of(*s), 1);
        }
        tally.started += batch.started;
        for (k, n) in batch.per_state {
            *tally.per_state.entry(k).or_default() += n;
        }
        for (k, n) in batch.unmapped {
            *tally.unmapped.entry(k).or_default() += n;
        }
    }
    let mut conn = pool.acquire().await?;
    let out = result(&mut conn, &t, &p.states, version_no, false, running, tally).await?;
    tracing::info!(definition = %t.definition.key, started = out.started, already_running = out.already_running,
        skipped_unmapped = out.skipped_unmapped, skipped_terminal = out.skipped_terminal, "workflow bootstrap finished");
    Ok(out)
}
