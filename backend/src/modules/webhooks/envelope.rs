//! The webhook payload, envelope v1 (design SHAA-2725 §5.5; the JSON Schema
//! is `payload.schema.json` next to this file).
//!
//! Built at send time, so a retry hours later carries the CI as it is then.
//! The CI's fields are only the action's `includeAttributes`; the actor is
//! named, never identified by id or e-mail address.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::http::error::AppError;
use crate::modules::items::service as items;
use crate::modules::workflows::actions::WorkflowActionSettings;
use crate::schema::model::Model;

pub const SPEC_VERSION: &str = "1";

#[derive(sqlx::FromRow)]
struct Source {
    event_id: i64,
    action_key: String,
    instance_id: Uuid,
    ci_id: Uuid,
    trigger: Option<String>,
    settings: Option<sqlx::types::Json<WorkflowActionSettings>>,
    event_kind: Option<String>,
    transition_key: Option<String>,
    from_state_key: Option<String>,
    to_state_key: Option<String>,
    to_version_no: Option<i32>,
    occurred_at: Option<DateTime<Utc>>,
    actor_type: Option<String>,
    actor_name: Option<String>,
    approval_request_id: Option<Uuid>,
    approval_step_no: Option<i16>,
    definition_key: Option<String>,
    definition_name: Option<String>,
    version_id: Option<Uuid>,
}

/// The trigger an event fires when no action says which (the action was deleted).
fn trigger_of(kind: &str) -> &'static str {
    match kind {
        "approval_request" => "approval_requested",
        "approval_decision" => "approval_step",
        "approval_close" | "approval_withdraw" => "approval_closed",
        "approval_overdue" => "approval_overdue",
        "cancel" => "instance_cancelled",
        "force" => "instance_forced",
        _ => "transition",
    }
}

/// The `X-ShadouCMDB-Event` value and the envelope of delivery `delivery` of run `run`.
pub async fn build(
    conn: &mut PgConnection,
    run: i64,
    delivery: Uuid,
    public_url: Option<&str>,
) -> Result<Option<(String, Value)>, AppError> {
    let src: Option<Source> = sqlx::query_as(
        "SELECT r.event_id, r.action_key, r.instance_id, r.ci_id, a.trigger, a.settings,
                e.kind AS event_kind, e.transition_key, e.from_state_key, e.to_state_key, e.to_version_no,
                e.occurred_at, e.actor_type, e.actor_name, e.approval_request_id, e.approval_step_no,
                d.key AS definition_key, d.name AS definition_name, v.id AS version_id
         FROM cmdb.workflow_action_runs r
         LEFT JOIN cmdb.workflow_actions a ON a.id = r.action_id
         LEFT JOIN cmdb.workflow_instance_events e ON e.id = r.event_id
         LEFT JOIN cmdb.workflow_definitions d ON d.id = r.definition_id
         LEFT JOIN cmdb.workflow_versions v ON v.definition_id = r.definition_id AND v.version_no = e.to_version_no
         WHERE r.id = $1",
    )
    .bind(run)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(s) = src else { return Ok(None) };
    let trigger = s.trigger.clone().unwrap_or_else(|| trigger_of(s.event_kind.as_deref().unwrap_or_default()).into());
    let event = format!("workflow.{trigger}");

    let names: Vec<(String, String)> = sqlx::query_as(
        "SELECT 's:' || key, name FROM cmdb.workflow_states WHERE version_id = $1 AND key = ANY($2)
         UNION ALL
         SELECT 't:' || key, name FROM cmdb.workflow_transitions WHERE version_id = $1 AND key = $3",
    )
    .bind(s.version_id)
    .bind([s.from_state_key.clone(), s.to_state_key.clone()].into_iter().flatten().collect::<Vec<_>>())
    .bind(&s.transition_key)
    .fetch_all(&mut *conn)
    .await?;
    let names: HashMap<String, String> = names.into_iter().collect();
    let state = |k: &Option<String>| k.as_ref().map(|k| json!({ "key": k, "name": names.get(&format!("s:{k}")) }));
    let transition = s.transition_key.as_ref().map(
        |k| json!({ "key": k, "name": names.get(&format!("t:{k}")), "from": s.from_state_key, "to": s.to_state_key }),
    );

    let approval = match s.approval_request_id {
        Some(r) => {
            let row: Option<(i32, String)> =
                sqlx::query_as("SELECT request_no, status FROM cmdb.workflow_approval_requests WHERE id = $1")
                    .bind(r)
                    .fetch_optional(&mut *conn)
                    .await?;
            json!({ "id": r, "requestNo": row.as_ref().map(|x| x.0), "status": row.map(|x| x.1),
                    "step": s.approval_step_no })
        }
        None => Value::Null,
    };

    let ci = ci_part(conn, s.ci_id, s.settings.as_ref().and_then(|x| x.include_attributes.as_deref())).await?;
    let url = public_url.map(|base| format!("{}/workflows/{}", base.trim_end_matches('/'), s.instance_id));
    let actor = match s.actor_type.as_deref() {
        Some("user") | Some("api_client") => json!({ "type": s.actor_type, "name": s.actor_name }),
        Some(t) => json!({ "type": t, "name": s.actor_name }),
        None => Value::Null,
    };
    let body = json!({
        "specVersion": SPEC_VERSION,
        "id": delivery,
        "event": event,
        "occurredAt": s.occurred_at.map(|t| crate::api::schemas::iso(&t)),
        "sequence": s.event_id,
        "instance": { "id": s.instance_id, "url": url },
        "definition": { "key": s.definition_key, "name": s.definition_name, "versionNo": s.to_version_no },
        "transition": transition,
        "state": state(&s.to_state_key),
        "approval": approval,
        "ci": ci,
        "actor": actor,
        "action": { "key": s.action_key },
    });
    Ok(Some((event, body)))
}

/// `ci`: ids, ident, label and class key, plus the listed fields read now. A
/// reference is `{id, ident, label}`; a CI deleted since is `deleted: true`
/// with no fields; a CI purged since is null.
async fn ci_part(conn: &mut PgConnection, ci: Uuid, include: Option<&[String]>) -> Result<Value, AppError> {
    let model = Model::load(&mut *conn).await?;
    let Some(item) = items::details(&mut *conn, &model, &[ci]).await?.pop() else { return Ok(Value::Null) };
    let s = &item.summary;
    let mut out = json!({ "id": s.id, "ident": s.ident, "label": s.label, "class": s.class.key });
    if s.deleted_at.is_some() {
        out["deleted"] = json!(true);
        return Ok(out);
    }
    // Only fields of the CI's own type and its ancestors, read from the stored
    // values: a key the action saved that has since left the type is skipped.
    let lineage: Vec<Uuid> = model.lineage(s.class.id).iter().map(|c| c.id).collect();
    let include: Vec<&String> = include
        .unwrap_or_default()
        .iter()
        .filter(|k| model.fields.iter().any(|f| &f.key == *k && lineage.contains(&f.class_id)))
        .collect();
    if include.is_empty() {
        return Ok(out);
    }
    let refs: Vec<Uuid> = include
        .iter()
        .filter_map(|k| item.attribute_references.get(k.as_str()))
        .filter_map(|r| r.get("id").and_then(Value::as_str).and_then(|i| i.parse().ok()))
        .collect();
    let idents: HashMap<Uuid, (String, String)> = sqlx::query_as::<_, (Uuid, String, String)>(
        "SELECT id, ident, label FROM cmdb.configuration_items WHERE id = ANY($1)",
    )
    .bind(&refs)
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|(id, ident, label)| (id, (ident, label)))
    .collect();
    let mut fields = Map::new();
    for key in include {
        let value = match item.attribute_references.get(key.as_str()).and_then(|r| r.get("id")).and_then(Value::as_str)
        {
            Some(id) => {
                let found = id.parse::<Uuid>().ok().and_then(|i| idents.get(&i));
                json!({ "id": id, "ident": found.map(|f| &f.0), "label": found.map(|f| &f.1) })
            }
            None => item.attributes.get(key.as_str()).cloned().unwrap_or(Value::Null),
        };
        fields.insert(key.clone(), value);
    }
    out["attributes"] = Value::Object(fields);
    Ok(out)
}

/// The envelope of a `ping` (the endpoint's test button).
pub fn ping(delivery: Uuid, endpoint_key: &str) -> Value {
    json!({
        "specVersion": SPEC_VERSION,
        "id": delivery,
        "event": "ping",
        "occurredAt": crate::api::schemas::iso(&Utc::now()),
        "sequence": null,
        "instance": null,
        "definition": null,
        "transition": null,
        "state": null,
        "approval": null,
        "ci": null,
        "actor": null,
        "action": null,
        "endpoint": { "key": endpoint_key },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The schema checked into the repository is valid JSON and names every top-level field the envelope has.
    #[test]
    fn the_schema_covers_the_envelope() {
        let schema: Value = serde_json::from_str(include_str!("payload.schema.json")).unwrap();
        let props = schema["properties"].as_object().unwrap();
        for key in ping(Uuid::nil(), "x").as_object().unwrap().keys() {
            assert!(props.contains_key(key), "{key} is missing from payload.schema.json");
        }
        assert_eq!(schema["properties"]["specVersion"]["const"], SPEC_VERSION);
    }
}
