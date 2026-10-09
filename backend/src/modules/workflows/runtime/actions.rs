//! Attribute actions at run time (actions design SHAA-2725 §3.4, §7): the
//! fields a transition sets on its CI when it runs, synchronously, in the
//! transition's transaction (direct, bulk item, final approval).
//!
//! **Rights** (§7.1). The action adds no rights: it writes with the identity
//! the transition runs as (the decider on a final approval), narrowed to the
//! edit right on the CI's type and no view right beyond that identity's own,
//! through the item write path with every rule of a PATCH, the state field
//! guard included. Only the action's own fields are sent. The one addition:
//! `valueFrom: actor` writes a reference to the actor's own Person, chosen
//! by the server and not by the caller, so the write may name it even when
//! the actor may not view the Person type (a reference to a CI one may not
//! view is otherwise refused).
//!
//! **Failure** (§7.2). A value that no longer validates (a lookup value
//! deleted since publishing, a pattern tightened) fails the transition with
//! 422 WORKFLOW_ACTION_INVALID: details[0] names the action, the others each
//! field as `attributes.<key>`. The caller's transaction then writes nothing.
//!
//! **Audit** (§7.3). One CI `update` row per run, separate from the
//! transition's own: the changed fields only, with `source` naming the
//! workflow, version and transition (and on a final approval the request and
//! its requester). The event's `field_changes` mark them `"origin": "action"`.

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{Map, Value, json};
use sqlx::PgConnection;
use std::sync::Arc;
use uuid::Uuid;

use super::super::schemas::WorkflowValueFrom;
use super::{InstanceRow, Pinned, PinnedTransition};
use crate::api::context::{Caller, RequestContext};
use crate::auth::Principal;
use crate::auth::permissions::{ClassRights, Permissions};
use crate::data::classes as class_data;
use crate::data::crud::{self, AuditAction, AuditEntry};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::modules::classes::AttributeDataType;
use crate::modules::items::service as items;
use crate::schema::model::Model;

/// One run of a transition's attribute actions.
pub(super) struct Run<'a> {
    pub(super) row: &'a InstanceRow,
    pub(super) transition: &'a PinnedTransition,
    /// The user `valueFrom: actor` names: who runs the transition, or decides its final approval.
    pub(super) actor: Option<Uuid>,
    /// Added to the audit row (a final approval's request and requester).
    pub(super) note: Map<String, Value>,
}

/// The write rights of a workflow step (approvals A-Q7, actions §7.1): the
/// edit right on the CI's type only, and the view rights `own` has (a
/// reference to a CI the writer may not view is refused, as a PATCH refuses it).
pub(super) fn write_permissions(own: Option<&Permissions>, class_id: Uuid) -> Permissions {
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
    permissions
}

/// `ctx` with its principal's rights narrowed by [`write_permissions`], plus
/// the view right on the types in `actor_types` (`valueFrom: actor` targets).
fn narrowed(ctx: &RequestContext, class_id: Uuid, actor_types: &[Uuid]) -> RequestContext {
    let Some(p) = ctx.principal() else { return ctx.clone() };
    let mut permissions = write_permissions(Some(&p.permissions), class_id);
    for t in actor_types {
        permissions.classes.entry(*t).or_default().view = true;
    }
    let principal =
        Principal { user_id: p.user_id, username: p.username.clone(), credential: p.credential.clone(), permissions };
    RequestContext { caller: Caller::User(Arc::new(principal)), ..ctx.clone() }
}

fn invalid(run: &Run<'_>, mut details: Vec<FieldError>) -> AppError {
    let (row, t) = (run.row, run.transition);
    details.sort_by(|a, b| a.field.cmp(&b.field).then_with(|| a.code.cmp(&b.code)));
    details.insert(
        0,
        FieldError {
            location: FieldLocation::Body,
            field: "action".into(),
            message: format!(
                "The attribute actions of transition {} (workflow {} version {})",
                t.key, row.definition_key, row.version_no
            ),
            code: "set_attributes".into(),
        },
    );
    AppError::new(
        ErrorCode::WorkflowActionInvalid,
        format!(
            "Transition {} cannot run: a field its attribute actions set does not accept the value (see details). \
             Nothing was changed. A workflow manager must publish a version whose actions set valid values",
            t.key
        ),
    )
    .with_details(details)
}

fn problem(key: &str, message: String, code: &str) -> FieldError {
    FieldError { location: FieldLocation::Body, field: format!("attributes.{key}"), message, code: code.into() }
}

/// Applies the attribute actions of `run.transition` to the locked CI of
/// `run.row`, after the transition's own fields. Returns the changes
/// `{key: {old, new, origin: "action"}}`, None when the transition has no
/// action or nothing changed (then no audit row is written).
pub(super) async fn apply(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    p: &Pinned,
    run: Run<'_>,
) -> Result<Option<Map<String, Value>>, AppError> {
    let actions: Vec<_> = p.set_attributes.iter().filter(|a| a.transition_id == run.transition.id).collect();
    if actions.is_empty() {
        return Ok(None);
    }
    let row = run.row;
    let model = Model::load(&mut *conn).await?;
    let defs = class_data::effective_attributes(&mut *conn, row.class_id).await?;
    let (now, today): (DateTime<Utc>, NaiveDate) =
        sqlx::query_as("SELECT now(), current_date").fetch_one(&mut *conn).await?;

    let mut attributes = Map::new();
    let mut actor_types = Vec::new();
    let mut failed = Vec::new();
    for a in &actions {
        let (Some(field), Some(def)) = (model.field(a.attribute_id), defs.iter().find(|d| d.id == a.attribute_id))
        else {
            failed.push(FieldError {
                location: FieldLocation::Body,
                field: "attributes".into(),
                message: format!("Field {} is no longer a field of type {}", a.attribute_id, row.class_key),
                code: "unknown_attribute".into(),
            });
            continue;
        };
        let key = field.key.as_str();
        let value = match (WorkflowValueFrom::parse(&a.value_from), a.value.as_ref().map(|v| &v.0)) {
            (None, Some(literal)) if def.data_type == AttributeDataType::Lookup => {
                // Lookup values are kept by key, so a value deleted (or a key renamed) since publishing fails here.
                let id: Option<Uuid> = match literal.as_str() {
                    Some(k) => {
                        sqlx::query_scalar("SELECT id FROM cmdb.lookup_list_values WHERE list_id = $1 AND key = $2")
                            .bind(def.lookup_list_id)
                            .bind(k)
                            .fetch_optional(&mut *conn)
                            .await?
                    }
                    None => None,
                };
                match id {
                    Some(id) => Value::String(id.to_string()),
                    None => {
                        failed.push(problem(
                            key,
                            format!("{} has no list value {literal} any more", def.label),
                            "not_found",
                        ));
                        continue;
                    }
                }
            }
            (None, Some(literal)) => literal.clone(),
            (Some(WorkflowValueFrom::Now), _) if def.data_type == AttributeDataType::Date => {
                Value::String(today.to_string())
            }
            (Some(WorkflowValueFrom::Now), _) => Value::String(now.to_rfc3339()),
            (Some(WorkflowValueFrom::Today), _) => Value::String(today.to_string()),
            (Some(WorkflowValueFrom::Actor), _) => {
                let person: Option<Uuid> = match run.actor {
                    Some(user) => sqlx::query_scalar("SELECT person_ci_id FROM cmdb.users WHERE id = $1")
                        .bind(user)
                        .fetch_optional(&mut *conn)
                        .await?
                        .flatten(),
                    None => None,
                };
                match person {
                    Some(id) => {
                        actor_types.extend(def.reference_class_id);
                        Value::String(id.to_string())
                    }
                    None => {
                        failed.push(problem(
                            key,
                            format!(
                                "{} is set to the person who runs the transition, but {} is linked to no Person",
                                def.label,
                                ctx.actor.name.as_deref().unwrap_or("the caller")
                            ),
                            "no_person",
                        ));
                        continue;
                    }
                }
            }
            (Some(WorkflowValueFrom::Clear), _) | (None, None) => Value::Null,
        };
        attributes.insert(key.to_owned(), value);
    }
    if !failed.is_empty() {
        return Err(invalid(&run, failed));
    }

    let writer = narrowed(ctx, row.class_id, &actor_types);
    let keys: Vec<String> = attributes.keys().cloned().collect();
    let w = match items::update_for_action(&mut *conn, &writer, row.ci_id, row.class_id, attributes).await {
        Ok(w) => w,
        Err(e) if e.code.status().is_client_error() => {
            let mut details = e.details.unwrap_or_default();
            if details.is_empty() {
                details.push(FieldError {
                    location: FieldLocation::Body,
                    field: "attributes".into(),
                    message: e.message,
                    code: "invalid".into(),
                });
            }
            return Err(invalid(&run, details));
        }
        Err(e) => return Err(e),
    };
    let (mut old, mut new, mut changes) = (Map::new(), Map::new(), Map::new());
    for k in keys {
        let (before, after) = (w.before.attributes.get(&k), w.after.attributes.get(&k));
        if before != after {
            old.insert(k.clone(), before.cloned().unwrap_or(Value::Null));
            new.insert(k.clone(), after.cloned().unwrap_or(Value::Null));
            changes.insert(k, json!({ "old": before, "new": after, "origin": "action" }));
        }
    }
    if changes.is_empty() {
        return Ok(None);
    }
    let mut new_value = Map::new();
    new_value.insert("classId".into(), json!(row.class_id));
    new_value.insert("attributes".into(), Value::Object(new));
    new_value.insert(
        "source".into(),
        json!({ "kind": "workflow_action", "definitionKey": row.definition_key, "versionNo": row.version_no,
                "transitionKey": run.transition.key }),
    );
    new_value.insert("instanceId".into(), json!(row.id));
    new_value.extend(run.note);
    let entry = AuditEntry {
        action: AuditAction::Update,
        entity_type: "configuration_items",
        entity_id: row.ci_id,
        old_value: Some(json!({ "classId": row.class_id, "attributes": old })),
        new_value: Some(Value::Object(new_value)),
    };
    crud::write_audit(&mut *conn, ctx, vec![entry]).await?;
    Ok(Some(changes))
}

/// The transition's own field changes with the actions' (which win on a key
/// both set; the publish lint refuses that).
pub(super) fn merge(own: Option<Value>, actions: Option<Map<String, Value>>) -> Option<Value> {
    let Some(actions) = actions else { return own };
    let mut all = match own {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };
    all.extend(actions);
    Some(Value::Object(all))
}
