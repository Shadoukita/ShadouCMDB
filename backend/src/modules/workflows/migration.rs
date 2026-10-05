//! Moving running instances to a newer version of their workflow (v0.4.0
//! slice S6, SHAA-1427; design on SHAA-1411 §2, §5.1, §6.1).
//!
//! Publishing never changes a running instance: it stays on the version it
//! started on until an administrator moves it here, with an explicit map from
//! the old version's states to the new one's. A dry run reports where every
//! running instance would go and writes nothing.
//!
//! **Batches.** A real run moves [`MIGRATION_BATCH`] instances per
//! transaction, so a workflow with 100,000 running instances neither holds
//! 100,000 CI locks nor the audit chain for the whole run. A run cut short
//! (an error, a restart) leaves the moved batches moved; running it again
//! moves the rest, because it only ever picks instances still on the old
//! version.
//!
//! **Lock order.** Per batch, the CIs are locked first (in id order) and the
//! instances second, as every runtime path does (Amendment 1); an instance
//! that ended or moved on in between is skipped.
//!
//! **Audit.** Each moved instance is a `migrate` event and a `workflow.migrate`
//! audit row on its CI (version and state before and after). When the
//! workflow drives a state field and the new state maps to another value, the
//! field is written through the item write path (a CI `update` row of the same
//! request), as a transition writes it.

use std::collections::HashMap;

use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::runtime::{self, Pinned, PinnedState};
use super::runtime_schemas::*;
use crate::api::context::RequestContext;
use crate::auth::permissions::ClassOp;
use crate::data::crud::{self, AuditAction, AuditEntry};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::schema::model::Model;

#[derive(sqlx::FromRow)]
struct Definition {
    key: String,
    class_id: Uuid,
    include_subclasses: bool,
    state_attribute_id: Option<Uuid>,
}

#[derive(sqlx::FromRow)]
struct Version {
    id: Uuid,
    status: String,
}

fn problem(field: String, message: String, code: &str) -> FieldError {
    FieldError { location: FieldLocation::Body, field, message, code: code.into() }
}

async fn version(conn: &mut PgConnection, definition: Uuid, no: i32) -> Result<Option<Version>, AppError> {
    Ok(sqlx::query_as("SELECT id, status FROM cmdb.workflow_versions WHERE definition_id = $1 AND version_no = $2")
        .bind(definition)
        .bind(no)
        .fetch_optional(conn)
        .await?)
}

/// A state of the old version and where its instances go (None: nowhere).
type Target<'p> = (&'p PinnedState, Option<(&'p PinnedState, WorkflowStateMapSource)>);

/// The target of each non-terminal state of `from`: from `stateMap`, else the
/// state of the same key in `to`. Bad entries of `stateMap` are 400.
fn plan<'p>(from: &'p Pinned, to: &'p Pinned, b: &WorkflowInstanceMigration) -> Result<Vec<Target<'p>>, AppError> {
    let mut problems = Vec::new();
    for (old, new) in &b.state_map {
        let field = format!("stateMap.{old}");
        match from.state_by_key(old) {
            None => problems.push(problem(
                field.clone(),
                format!("Version {} has no state {old}", b.from_version_no),
                "unknown_state",
            )),
            Some(s) if s.is_terminal => problems.push(problem(
                field.clone(),
                format!("State {old} is terminal: no instance is running in it"),
                "terminal_source",
            )),
            Some(_) => {}
        }
        match to.state_by_key(new) {
            None => problems.push(problem(
                field,
                format!("Version {} has no state {new}", b.to_version_no),
                "unknown_target_state",
            )),
            Some(s) if s.is_terminal => problems.push(problem(
                field,
                format!("State {new} is terminal: a migration does not complete instances"),
                "terminal_target",
            )),
            Some(_) => {}
        }
    }
    if !problems.is_empty() {
        return Err(AppError::validation(problems));
    }
    Ok(from
        .states
        .iter()
        .filter(|s| !s.is_terminal)
        .map(|s| {
            let target = match b.state_map.get(&s.key) {
                Some(k) => to.state_by_key(k).map(|t| (t, WorkflowStateMapSource::Explicit)),
                None => {
                    to.state_by_key(&s.key).filter(|t| !t.is_terminal).map(|t| (t, WorkflowStateMapSource::SameKey))
                }
            };
            (s, target)
        })
        .collect())
}

pub async fn migrate(
    pool: &PgPool,
    ctx: &RequestContext,
    definition: Uuid,
    b: &WorkflowInstanceMigration,
) -> Result<WorkflowInstanceMigrationReport, AppError> {
    let mut conn = pool.acquire().await?;
    let d: Definition = sqlx::query_as(
        "SELECT key, class_id, include_subclasses, state_attribute_id FROM cmdb.workflow_definitions WHERE id = $1",
    )
    .bind(definition)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| AppError::missing("Workflow definition", definition))?;

    // It writes the state field of CIs of every type the workflow runs on.
    let model = Model::load(&mut conn).await?;
    let covered = if d.include_subclasses { model.subtree(d.class_id) } else { vec![d.class_id] };
    if covered
        .iter()
        .any(|c| ctx.require_class(*c, ClassOp::View).is_err() || ctx.require_class(*c, ClassOp::Edit).is_err())
    {
        return Err(AppError::new(
            ErrorCode::Forbidden,
            format!(
                "Migrating the instances of workflow {} needs the view and edit rights on every type it runs on",
                d.key
            ),
        ));
    }

    let from = version(&mut conn, definition, b.from_version_no).await?;
    let to = version(&mut conn, definition, b.to_version_no).await?;
    let mut problems = Vec::new();
    if from.as_ref().is_none_or(|v| v.status == "draft") {
        problems.push(problem(
            "fromVersionNo".into(),
            format!("Workflow {} has no published or retired version {}", d.key, b.from_version_no),
            "unknown_version",
        ));
    }
    if to.is_none() {
        problems.push(problem(
            "toVersionNo".into(),
            format!("Workflow {} has no version {}", d.key, b.to_version_no),
            "unknown_version",
        ));
    } else if b.to_version_no <= b.from_version_no {
        problems.push(problem("toVersionNo".into(), "Instances move to a newer version only".into(), "not_newer"));
    }
    let (Some(from), Some(to)) = (from, to) else { return Err(AppError::validation(problems)) };
    if !problems.is_empty() {
        return Err(AppError::validation(problems));
    }
    if to.status != "published" {
        return Err(AppError::new(
            ErrorCode::Conflict,
            format!(
                "Version {} of workflow {} is {}: instances move to a published version",
                b.to_version_no, d.key, to.status
            ),
        )
        .with_details(vec![problem(
            "toVersionNo".into(),
            format!("The version is {}", to.status),
            "not_published",
        )]));
    }

    let from_graph = runtime::pinned(&mut conn, from.id).await?;
    let to_graph = runtime::pinned(&mut conn, to.id).await?;
    let targets = plan(&from_graph, &to_graph, b)?;
    let counts: HashMap<Uuid, i64> = sqlx::query_as::<_, (Uuid, i64)>(
        "SELECT current_state_id, count(*) FROM cmdb.workflow_instances
         WHERE version_id = $1 AND status = 'active' GROUP BY current_state_id",
    )
    .bind(from.id)
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .collect();
    let count = |s: &PinnedState| counts.get(&s.id).copied().unwrap_or(0);

    // Every state with running instances needs somewhere to go.
    let unmapped: Vec<FieldError> = targets
        .iter()
        .filter(|(s, t)| t.is_none() && count(s) > 0)
        .map(|(s, _)| {
            problem(
                format!("stateMap.{}", s.key),
                format!(
                    "{} running instance{} in state {} and version {} has no non-terminal state of that key: \
                     map it to a state of version {}",
                    count(s),
                    if count(s) == 1 { " is" } else { "s are" },
                    s.key,
                    b.to_version_no,
                    b.to_version_no
                ),
                "unmapped",
            )
        })
        .collect();
    if !unmapped.is_empty() {
        return Err(AppError::validation(unmapped));
    }
    let states: Vec<WorkflowMigrationStateMove> = targets
        .iter()
        .filter_map(|(s, t)| {
            let (to, mapped_by) = t.as_ref()?;
            (count(s) > 0 || mapped_by == &WorkflowStateMapSource::Explicit).then(|| WorkflowMigrationStateMove {
                from_state: s.key.clone(),
                to_state: to.key.clone(),
                mapped_by: *mapped_by,
                count: count(s),
            })
        })
        .collect();
    let total: i64 = counts.values().sum();
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM cmdb.workflow_instances wi WHERE wi.version_id = $1 AND wi.status = 'active'
           AND EXISTS (SELECT 1 FROM cmdb.workflow_approval_requests r WHERE r.instance_id = wi.id AND r.status = 'pending')",
    )
    .bind(from.id)
    .fetch_one(&mut *conn)
    .await?;
    let cancel = b.pending_approvals == WorkflowMigrationPendingApprovals::Cancel;
    let mut report = WorkflowInstanceMigrationReport {
        dry_run: b.dry_run,
        definition_key: d.key.clone(),
        from_version_no: b.from_version_no,
        to_version_no: b.to_version_no,
        total,
        migrated: 0,
        batches: 0,
        pending_approvals: pending,
        skipped: 0,
        states,
    };
    if b.dry_run {
        return Ok(report);
    }
    drop(conn);

    let map: HashMap<Uuid, &PinnedState> =
        targets.iter().filter_map(|(s, t)| t.as_ref().map(|(to, _)| (s.id, *to))).collect();
    let step = Step {
        d: &d,
        model: &model,
        from: &from_graph,
        map: &map,
        from_no: b.from_version_no,
        to_no: b.to_version_no,
        cancel,
    };
    loop {
        let mut tx = pool.begin().await?;
        let moved = step.batch(&mut tx, ctx, from.id, to.id).await?;
        if moved.picked == 0 {
            break;
        }
        tx.commit().await?;
        report.batches += 1;
        report.migrated += moved.migrated;
        if moved.picked < MIGRATION_BATCH {
            break;
        }
    }
    if !cancel {
        let mut conn = pool.acquire().await?;
        report.skipped = sqlx::query_scalar(
            "SELECT count(*) FROM cmdb.workflow_instances wi WHERE wi.version_id = $1 AND wi.status = 'active'
               AND EXISTS (SELECT 1 FROM cmdb.workflow_approval_requests r
                           WHERE r.instance_id = wi.id AND r.status = 'pending')",
        )
        .bind(from.id)
        .fetch_one(&mut *conn)
        .await?;
    }
    Ok(report)
}

struct Step<'a> {
    d: &'a Definition,
    model: &'a Model,
    from: &'a Pinned,
    map: &'a HashMap<Uuid, &'a PinnedState>,
    from_no: i32,
    to_no: i32,
    /// Close pending approval requests and move their instances (else they stay).
    cancel: bool,
}

struct Moved {
    /// Instances the batch looked at.
    picked: i64,
    migrated: i64,
}

#[derive(sqlx::FromRow)]
struct Locked {
    id: Uuid,
    ci_id: Uuid,
    class_id: Uuid,
    current_state_id: Uuid,
    version: i32,
}

impl Step<'_> {
    async fn batch(
        &self,
        tx: &mut PgConnection,
        ctx: &RequestContext,
        from: Uuid,
        to: Uuid,
    ) -> Result<Moved, AppError> {
        // Instances waiting for approval stay unless their requests are cancelled.
        let picked: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT wi.id, wi.ci_id FROM cmdb.workflow_instances wi WHERE wi.version_id = $1 AND wi.status = 'active'
               AND ($3 OR NOT EXISTS (SELECT 1 FROM cmdb.workflow_approval_requests r
                                      WHERE r.instance_id = wi.id AND r.status = 'pending'))
             ORDER BY wi.ci_id, wi.id LIMIT $2",
        )
        .bind(from)
        .bind(MIGRATION_BATCH)
        .bind(self.cancel)
        .fetch_all(&mut *tx)
        .await?;
        if picked.is_empty() {
            return Ok(Moved { picked: 0, migrated: 0 });
        }
        let ids: Vec<Uuid> = picked.iter().map(|p| p.0).collect();
        let cis: Vec<Uuid> = picked.iter().map(|p| p.1).collect();
        // CI first, then instance; whatever ended or moved on meanwhile is left out.
        sqlx::query("SELECT 1 FROM cmdb.configuration_items WHERE id = ANY($1) ORDER BY id FOR UPDATE")
            .bind(&cis)
            .execute(&mut *tx)
            .await?;
        let rows: Vec<Locked> = sqlx::query_as(
            "SELECT wi.id, wi.ci_id, ci.class_id, wi.current_state_id, wi.version
             FROM cmdb.workflow_instances wi JOIN cmdb.configuration_items ci ON ci.id = wi.ci_id
             WHERE wi.id = ANY($1) AND wi.version_id = $2 AND wi.status = 'active'
               AND ($3 OR NOT EXISTS (SELECT 1 FROM cmdb.workflow_approval_requests r
                                      WHERE r.instance_id = wi.id AND r.status = 'pending'))
             ORDER BY wi.id FOR UPDATE OF wi",
        )
        .bind(&ids)
        .bind(from)
        .bind(self.cancel)
        .fetch_all(&mut *tx)
        .await?;
        // A request is never carried to another version: CI → instance → request.
        let mut entries = if self.cancel {
            let locked: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
            runtime::approvals::close_for_migration(&mut *tx, ctx, &locked).await?
        } else {
            Vec::new()
        };

        let mut targets: Vec<Uuid> = Vec::with_capacity(rows.len());
        let mut from_keys: Vec<&str> = Vec::with_capacity(rows.len());
        let mut to_keys: Vec<&str> = Vec::with_capacity(rows.len());
        let mut changes: Vec<Option<serde_json::Value>> = Vec::with_capacity(rows.len());
        for r in &rows {
            let source = self.from.state(r.current_state_id).ok_or_else(AppError::internal)?;
            // An instance moved into an unmapped state after the plan was made.
            let Some(target) = self.map.get(&r.current_state_id) else {
                return Err(AppError::new(
                    ErrorCode::Conflict,
                    format!(
                        "An instance moved into state {} while the migration ran, and that state is not mapped. \
                         Instances moved so far stay moved; run a dry run again and map the state",
                        source.key
                    ),
                )
                .with_details(vec![problem(
                    format!("stateMap.{}", source.key),
                    "Not mapped".into(),
                    "unmapped",
                )]));
            };
            let write = if target.state_value_id != source.state_value_id {
                runtime::state_value(self.model, self.d.state_attribute_id, target)
            } else {
                Default::default()
            };
            let changed = runtime::write_ci(&mut *tx, ctx, r.ci_id, r.class_id, write, None).await?;
            targets.push(target.id);
            from_keys.push(&source.key);
            to_keys.push(&target.key);
            entries.push(AuditEntry {
                action: AuditAction::WorkflowMigrate,
                entity_type: "configuration_items",
                entity_id: r.ci_id,
                old_value: Some(json!({ "instanceId": r.id, "definitionKey": self.d.key, "versionNo": self.from_no,
                    "stateKey": source.key, "version": r.version })),
                new_value: Some(json!({ "instanceId": r.id, "definitionKey": self.d.key, "versionNo": self.to_no,
                    "stateKey": target.key, "version": r.version + 1, "fields": changed })),
            });
            changes.push(changed);
        }
        let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
        sqlx::query(
            "UPDATE cmdb.workflow_instances wi SET version_id = $2, current_state_id = u.state, version = wi.version + 1
             FROM unnest($1::uuid[], $3::uuid[]) AS u(id, state) WHERE wi.id = u.id",
        )
        .bind(&ids)
        .bind(to)
        .bind(&targets)
        .execute(&mut *tx)
        .await?;
        let actor = runtime::actor_of(ctx);
        let changes: Vec<Option<sqlx::types::Json<serde_json::Value>>> =
            changes.into_iter().map(|c| c.map(sqlx::types::Json)).collect();
        sqlx::query(
            "INSERT INTO cmdb.workflow_instance_events
               (instance_id, kind, from_state_key, to_state_key, from_version_no, to_version_no,
                actor_type, actor_id, actor_name, field_changes, request_id)
             SELECT u.id, 'migrate', u.from_key, u.to_key, $4, $5, $6, $7, $8, u.changes, $9
             FROM unnest($1::uuid[], $2::text[], $3::text[], $10::jsonb[]) AS u(id, from_key, to_key, changes)",
        )
        .bind(&ids)
        .bind(&from_keys)
        .bind(&to_keys)
        .bind(self.from_no)
        .bind(self.to_no)
        .bind(actor.actor_type)
        .bind(actor.id)
        .bind(actor.name)
        .bind(&ctx.request_id)
        .bind(&changes)
        .execute(&mut *tx)
        .await?;
        crud::write_audit(tx, ctx, entries).await?;
        Ok(Moved { picked: picked.len() as i64, migrated: rows.len() as i64 })
    }
}
