//! Attribute actions, slice S2 of the actions design (SHAA-2725 §3.4, §7,
//! §12; SHAA-2732) through the real router against PostgreSQL: the draft
//! `setAttributes`, the publish lint (a–f), IN_USE for a target field, the
//! apply in a direct run, a bulk run and a final approval, with its audit row
//! and event markers, the refusal of a value that no longer validates, and
//! configuration format 14.

use serde_json::{Value, json};
use uuid::Uuid;

use super::approvals_runtime_tests::{decide, publish, request, setup, started};
use super::runtime_tests::{DEFS, RUN, World, details, id, pairs, world};
use crate::db::scratch;
use crate::modules::api_tokens::tests::{Creds, app, call, code, session_of};

impl World {
    async fn field(&self, key: &str, data_type: &str, extra: Value) -> Uuid {
        let mut body = json!({ "classId": self.server, "key": key, "label": key, "dataType": data_type });
        body.as_object_mut().unwrap().extend(extra.as_object().cloned().unwrap_or_default());
        id(&self.ok("POST", "/api/v1/attribute-definitions", body).await)
    }

    async fn person_class(&self) -> Uuid {
        sqlx::query_scalar("SELECT id FROM ci_classes WHERE system_role = 'person'")
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn person_of(&self, user: Uuid) -> Uuid {
        let person: Option<Uuid> = sqlx::query_scalar("SELECT person_ci_id FROM users WHERE id = $1")
            .bind(user)
            .fetch_one(&self.pool)
            .await
            .unwrap();
        person.expect("a user with an e-mail address gets a Person")
    }

    /// A lookup list with these values; returns the value ids by key.
    async fn list(&self, key: &str, values: &[&str]) -> (Uuid, Vec<(String, Uuid)>) {
        let list = id(&self.ok("POST", "/api/v1/lookup-lists", json!({ "key": key, "name": key })).await);
        let mut ids = Vec::new();
        for v in values {
            let body = json!({ "listId": list, "key": v, "name": v });
            ids.push(((*v).to_owned(), id(&self.ok("POST", "/api/v1/lookup-list-values", body).await)));
        }
        (list, ids)
    }

    async fn instance(&self, instance: Uuid) -> Value {
        let (status, v) = self.call(&self.admin, "GET", &format!("{RUN}/{instance}"), None).await;
        assert_eq!(status, 200, "{v}");
        v["instance"].clone()
    }

    async fn run(&self, instance: Uuid, key: &str, fields: Value) -> (u16, Value) {
        let version = self.instance(instance).await["version"].clone();
        let body = json!({ "transitionKey": key, "expectedVersion": version, "fields": fields, "comment": "ok" });
        self.transition(&self.admin, instance, body).await
    }

    async fn count(&self, sql: &str, id: Uuid) -> i64 {
        sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned())).bind(id).fetch_one(&self.pool).await.unwrap()
    }
}

/// The world's version 1 graph, with `go_live` setting `actions` and
/// `approve` setting `approve_actions`.
fn graph(actions: Value, approve_actions: Value) -> Value {
    let mut approve = json!({ "key": "approve", "name": "Approve", "from": "planned", "to": "approved",
        "requiresComment": true,
        "fields": [ { "attribute": "owner_team", "required": true }, { "attribute": "risk", "required": false } ],
        "conditions": { "all": [ { "field": "environment", "op": "in", "value": ["prod"] } ] } });
    if approve_actions != json!([]) {
        approve["setAttributes"] = approve_actions;
    }
    json!({
        "initialState": "planned",
        "states": [
            { "key": "planned", "name": "Planned", "category": "open", "stateValue": "planned" },
            { "key": "approved", "name": "Approved", "category": "active", "stateValue": "approved" },
            { "key": "done", "name": "In production", "category": "done", "terminal": true, "stateValue": "live" }
        ],
        "transitions": [
            approve,
            { "key": "go_live", "name": "Go live", "from": "approved", "to": "done", "setAttributes": actions }
        ]
    })
}

/// (path, code) of the problems on attribute actions, sorted.
fn action_problems(v: &Value) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = v["problems"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["path"].as_str().unwrap().contains("setAttributes"))
        .map(|p| {
            assert_eq!(p["severity"], "error", "{p}");
            (p["path"].as_str().unwrap().to_owned(), p["code"].as_str().unwrap().to_owned())
        })
        .collect();
    out.sort();
    out
}

/// Publish lint §3.4 (a)–(f) and the value checks; 400 on a draft that names
/// an unknown field, gives no or both values, or sets a field twice; a field
/// a published action sets is IN_USE.
#[tokio::test]
async fn the_publish_lint_refuses_every_forbidden_target_and_value() {
    let Some(db) = scratch::database("workflow_actions_lint").await else { return };
    let w = world(&db).await;
    let person = w.person_class().await;
    w.field("serial", "text", json!({ "isIdentifying": true })).await;
    w.field("runs_on", "reference", json!({ "referenceClassId": w.server })).await;
    w.field("retired_by", "reference", json!({ "referenceClassId": person })).await;
    let retired_on = w.field("retired_on", "date", json!({})).await;
    w.field("monitoring_ref", "text", json!({ "validation": { "maxLength": 8 } })).await;
    w.field("mandatory", "text", json!({ "isRequired": true })).await;
    let legacy = w.field("legacy", "text", json!({})).await;
    let (list, _) = w.list("ops_status", &["in_use", "retired"]).await;
    w.field("ops_status", "lookup", json!({ "lookupListId": list })).await;
    let draft = format!("{DEFS}/{}/draft", w.definition);

    // 400 when the draft is saved: an unknown field, no value or both, a field set twice.
    let set = |a: &str, v: Value| {
        let mut e = json!({ "attribute": a });
        e.as_object_mut().unwrap().extend(v.as_object().unwrap().clone());
        e
    };
    let (status, v) = w
        .call(
            &w.admin,
            "PUT",
            &draft,
            Some(graph(
                json!([
                    set("nope", json!({ "value": "x" })),
                    set("legacy", json!({})),
                    set("legacy", json!({ "value": "x", "valueFrom": "clear" })),
                    set("monitoring_ref", json!({ "valueFrom": "clear" })),
                    set("monitoring_ref", json!({ "value": "x" }))
                ]),
                json!([]),
            )),
        )
        .await;
    assert_eq!(status, 400, "{v}");
    assert_eq!(
        details(&v),
        pairs(&[
            ("transitions[1].setAttributes[0].attribute", "unknown_attribute"),
            ("transitions[1].setAttributes[1]", "value_or_value_from"),
            ("transitions[1].setAttributes[2]", "value_or_value_from"),
            ("transitions[1].setAttributes[4].attribute", "duplicate"),
        ]),
        "{v}"
    );

    let actions = json!([
        set("lifecycle", json!({ "value": "live" })), // (a) this workflow's state field
        set("serial", json!({ "value": "SN-1" })),    // (b) identifying
        set("runs_on", json!({ "valueFrom": "clear" })), // (d) a reference to another type than Person
        set("retired_by", json!({ "value": Uuid::new_v4() })), // (d) no literal CI id
        set("legacy", json!({ "value": "x" })),       // (f) archived below
        set("retired_on", json!({ "valueFrom": "actor" })), // actor on a date
        set("monitoring_ref", json!({ "valueFrom": "now" })), // now on a text
        set("environment", json!({ "value": "staging" })), // not an enum value
        set("ops_status", json!({ "value": "scrapped" })), // not a value of the list
        set("mandatory", json!({ "valueFrom": "clear" })), // required
        set("owner_team", json!({ "valueFrom": "today" }))  // today on a text
    ]);
    let approve_actions = json!([set("owner_team", json!({ "value": "ops" }))]); // (c) a field of the same transition
    w.ok("PUT", &draft, graph(actions, approve_actions)).await;
    w.ok("PATCH", &format!("/api/v1/attribute-definitions/{legacy}"), json!({ "isActive": false })).await;
    let (status, v) = w.call(&w.admin, "POST", &format!("{draft}/validate"), None).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["valid"], false);
    let expected = pairs(&[
        ("transitions[0].setAttributes[0].attribute", "transition_field"),
        ("transitions[1].setAttributes[0].attribute", "workflow_managed_attribute"),
        ("transitions[1].setAttributes[10].valueFrom", "value_from_type"),
        ("transitions[1].setAttributes[1].attribute", "identifying_attribute"),
        ("transitions[1].setAttributes[2].attribute", "reference_not_person"),
        ("transitions[1].setAttributes[3].value", "reference_literal"),
        ("transitions[1].setAttributes[4].attribute", "inactive_attribute"),
        ("transitions[1].setAttributes[5].valueFrom", "value_from_type"),
        ("transitions[1].setAttributes[6].valueFrom", "value_from_type"),
        ("transitions[1].setAttributes[7].value", "invalid_value"),
        ("transitions[1].setAttributes[8].value", "unknown_value"),
        ("transitions[1].setAttributes[9].valueFrom", "required_attribute"),
    ]);
    assert_eq!(action_problems(&v), expected, "{v}");
    // The params carry what each message names, for a client to word it (SHAA-3003).
    let params = |path: &str| {
        let p =
            v["problems"].as_array().unwrap().iter().find(|p| p["path"] == path).unwrap_or_else(|| panic!("{path}"));
        p.get("params").cloned().unwrap_or(json!({}))
    };
    assert_eq!(
        params("transitions[0].setAttributes[0].attribute"),
        json!({ "attribute": "owner_team", "transition": "approve" })
    );
    assert_eq!(params("transitions[1].setAttributes[0].attribute"), json!({ "attribute": "lifecycle" }));
    assert_eq!(params("transitions[1].setAttributes[1].attribute"), json!({ "attribute": "serial" }));
    assert_eq!(params("transitions[1].setAttributes[3].value"), json!({ "attribute": "retired_by" }));
    assert_eq!(
        params("transitions[1].setAttributes[5].valueFrom"),
        json!({ "attribute": "retired_on", "dataType": "date", "valueFrom": "actor", "expected": "person_reference" })
    );
    assert_eq!(
        params("transitions[1].setAttributes[6].valueFrom"),
        json!({ "attribute": "monitoring_ref", "dataType": "text", "valueFrom": "now", "expected": "date_or_datetime" })
    );
    let enum_value = params("transitions[1].setAttributes[7].value");
    assert_eq!((&enum_value["attribute"], &enum_value["value"]), (&json!("environment"), &json!("staging")));
    assert!(enum_value["options"].as_str().is_some_and(|o| o.contains("prod")), "{enum_value}");
    assert_eq!(
        params("transitions[1].setAttributes[8].value"),
        json!({ "attribute": "ops_status", "value": "scrapped" })
    );
    assert_eq!(params("transitions[1].setAttributes[9].valueFrom"), json!({ "attribute": "mandatory" }));
    let (status, p) = w
        .call(&w.admin, "POST", &format!("{draft}/publish"), Some(json!({ "expectedDraftChecksum": v["checksum"] })))
        .await;
    assert_eq!((status, code(&p)), (400, "VALIDATION_ERROR"), "{p}");
    let mut refused: Vec<(String, String)> =
        details(&p).into_iter().filter(|(f, _)| f.contains("setAttributes")).collect();
    refused.sort();
    assert_eq!(refused, expected, "{p}");

    // (a) also covers the state field of another active workflow on the type.
    let other = w.field("other_state", "lookup", json!({ "lookupListId": list })).await;
    let body = json!({ "key": "other", "name": "Other", "classId": w.server, "stateAttributeId": other });
    let o = id(&w.ok("POST", DEFS, body).await);
    let one = json!({ "initialState": "a", "states": [
            { "key": "a", "name": "A", "category": "open", "stateValue": "in_use" },
            { "key": "b", "name": "B", "category": "done", "terminal": true, "stateValue": "retired" } ],
        "transitions": [ { "key": "end", "name": "End", "from": "a", "to": "b" } ] });
    let d = w.ok("PUT", &format!("{DEFS}/{o}/draft"), one).await;
    w.ok("POST", &format!("{DEFS}/{o}/draft/publish"), json!({ "expectedDraftChecksum": d["checksum"] })).await;
    let od = w.ok("GET", &format!("{DEFS}/{o}"), json!(null)).await;
    w.ok("PATCH", &format!("{DEFS}/{o}"), json!({ "version": od["version"], "isActive": true })).await;
    w.ok("PUT", &draft, graph(json!([set("other_state", json!({ "value": "retired" }))]), json!([]))).await;
    let (_, v) = w.call(&w.admin, "POST", &format!("{draft}/validate"), None).await;
    assert_eq!(
        action_problems(&v),
        pairs(&[("transitions[1].setAttributes[0].attribute", "workflow_managed_attribute")]),
        "{v}"
    );
    let managed = v["problems"].as_array().unwrap().iter().find(|p| p["code"] == "workflow_managed_attribute").unwrap();
    assert_eq!(managed["params"]["attribute"], "other_state", "{managed}");
    assert_eq!(managed["params"]["workflow"], "other", "{managed}");

    // (e) the key fields of the built-in Person type are read-only.
    let email: String =
        sqlx::query_scalar("SELECT key FROM ci_attribute_definitions WHERE system_role = 'person_email'")
            .fetch_one(&w.pool)
            .await
            .unwrap();
    let body = json!({ "key": "people", "name": "People", "classId": person });
    let p = id(&w.ok("POST", DEFS, body).await);
    let people = json!({ "initialState": "a", "states": [
            { "key": "a", "name": "A", "category": "open" },
            { "key": "b", "name": "B", "category": "done", "terminal": true } ],
        "transitions": [ { "key": "end", "name": "End", "from": "a", "to": "b",
                           "setAttributes": [ { "attribute": email, "value": "someone@example.test" } ] } ] });
    w.ok("PUT", &format!("{DEFS}/{p}/draft"), people).await;
    let (_, v) = w.call(&w.admin, "POST", &format!("{DEFS}/{p}/draft/validate"), None).await;
    assert_eq!(
        action_problems(&v),
        pairs(&[("transitions[0].setAttributes[0].attribute", "read_only_attribute")]),
        "{v}"
    );

    // A valid draft publishes; the draft and the published version show the actions as sent.
    let valid = json!([
        set("retired_on", json!({ "valueFrom": "now" })),
        set("retired_by", json!({ "valueFrom": "actor" })),
        set("monitoring_ref", json!({ "valueFrom": "clear" })),
        set("ops_status", json!({ "value": "retired" }))
    ]);
    let d = w.ok("PUT", &draft, graph(valid.clone(), json!([]))).await;
    assert_eq!(d["transitions"][1]["setAttributes"], valid, "{d}");
    assert!(d["transitions"][0].get("setAttributes").is_none(), "left out when empty: {d}");
    let v = w.ok("POST", &format!("{draft}/publish"), json!({ "expectedDraftChecksum": d["checksum"] })).await;
    assert_eq!(v["transitions"][1]["setAttributes"], valid, "{v}");
    assert_eq!(v["checksum"], d["checksum"]);
    let audit: Value = sqlx::query_scalar(
        "SELECT new_value FROM audit_log WHERE action = 'workflow.publish' AND entity_id = $1 ORDER BY id DESC LIMIT 1",
    )
    .bind(w.definition)
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!(audit["graph"]["transitions"][1]["setAttributes"], valid, "the publish row records the actions");

    // A target of a published action cannot be retyped or deleted.
    let path = format!("/api/v1/attribute-definitions/{retired_on}");
    let (status, v) = w.call(&w.admin, "PATCH", &path, Some(json!({ "dataType": "text" }))).await;
    assert_eq!((status, code(&v)), (409, "IN_USE"), "{v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("server_lifecycle v2 (published)"), "{v}");
    let (status, v) = w.call(&w.admin, "DELETE", &path, None).await;
    assert_eq!((status, code(&v)), (409, "IN_USE"), "{v}");
    db.drop().await;
}

/// The retire example of §3.4: a direct run sets the literal, `now`, `actor`
/// and `clear` targets and writes exactly one extra audit row; a value deleted
/// since publishing fails the run with 422 and nothing written; a bulk run
/// reports that item and applies the others.
#[tokio::test]
async fn attribute_actions_apply_in_the_transition_and_fail_closed() {
    let Some(db) = scratch::database("workflow_actions_apply").await else { return };
    let w = world(&db).await;
    let person = w.person_class().await;
    w.field("retired_by", "reference", json!({ "referenceClassId": person })).await;
    w.field("retired_on", "date", json!({})).await;
    w.field("retired_at", "datetime", json!({})).await;
    w.field("monitoring_ref", "text", json!({})).await;
    w.field("note", "text", json!({})).await;
    let (list, values) = w.list("ops_status", &["in_use", "retired", "scrapped"]).await;
    w.field("ops_status", "lookup", json!({ "lookupListId": list })).await;
    let retire = |status: &str| {
        json!([
            { "attribute": "ops_status", "value": status },
            { "attribute": "note", "value": "Retired by the workflow" },
            { "attribute": "retired_on", "valueFrom": "now" },
            { "attribute": "retired_at", "valueFrom": "now" },
            { "attribute": "retired_by", "valueFrom": "actor" },
            { "attribute": "monitoring_ref", "valueFrom": "clear" }
        ])
    };
    publish(&w, graph(retire("retired"), json!([]))).await;
    let admin: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE username = 'admin'").fetch_one(&w.pool).await.unwrap();
    let me = w.person_of(admin).await;

    // Three CIs on version 2, each approved and ready to go live.
    let mut instances = Vec::new();
    for _ in 0..3 {
        let ci = w.ci(w.server).await;
        let body = json!({ "attributes": { "environment": "prod", "monitoring_ref": "MON-1", "note": "in use" } });
        w.ok("PATCH", &format!("/api/v1/configuration-items/{ci}"), body).await;
        let (status, v) = w.start(&w.admin, ci).await;
        assert_eq!(status, 201, "{v}");
        let instance = id(&v["instance"]);
        let (status, v) = w.run(instance, "approve", json!({ "owner_team": "ops" })).await;
        assert_eq!(status, 200, "{v}");
        instances.push((ci, instance));
    }

    // Retire the first directly.
    let (ci, instance) = instances[0];
    let (status, v) = w.run(instance, "go_live", json!({})).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["state"]["key"], "done");
    let today: String = sqlx::query_scalar("SELECT current_date::text").fetch_one(&w.pool).await.unwrap();
    let a = w.ci_values(ci).await["attributes"].clone();
    let retired = values.iter().find(|(k, _)| k == "retired").unwrap().1;
    assert_eq!(a["ops_status"], json!(retired.to_string()), "{a}");
    assert_eq!(a["note"], "Retired by the workflow");
    assert_eq!(a["retired_on"], json!(today));
    assert!(a["retired_at"].as_str().is_some_and(|t| chrono::DateTime::parse_from_rfc3339(t).is_ok()), "{a}");
    assert_eq!(a["retired_by"], json!(me.to_string()));
    assert_eq!(a["monitoring_ref"], Value::Null);
    assert_eq!(a["lifecycle"], json!(w.value("live").to_string()), "the state field moved as well");

    // The go-live request wrote the transition's own update (the state field), exactly one action row, and the step.
    let request: String = sqlx::query_scalar(
        "SELECT request_id FROM audit_log WHERE entity_id = $1 AND action = 'workflow.transition' ORDER BY id DESC LIMIT 1",
    )
    .bind(ci)
    .fetch_one(&w.pool)
    .await
    .unwrap();
    let rows: Vec<(String, Option<Value>, Option<Value>)> = sqlx::query_as(
        "SELECT action, old_value, new_value FROM audit_log WHERE entity_id = $1 AND request_id = $2 ORDER BY id",
    )
    .bind(ci)
    .bind(&request)
    .fetch_all(&w.pool)
    .await
    .unwrap();
    let actions: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(actions, ["update", "update", "workflow.transition"]);
    let sourced: Vec<_> = rows.iter().filter(|r| r.2.as_ref().is_some_and(|n| n.get("source").is_some())).collect();
    assert_eq!(sourced.len(), 1, "one extra row: {rows:?}");
    let (old, new) = (sourced[0].1.clone().unwrap(), sourced[0].2.clone().unwrap());
    assert_eq!(
        new["source"],
        json!({ "kind": "workflow_action", "definitionKey": "server_lifecycle", "versionNo": 2, "transitionKey": "go_live" })
    );
    assert_eq!(new["instanceId"], json!(instance.to_string()));
    let mut keys: Vec<&String> = new["attributes"].as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(keys, ["monitoring_ref", "note", "ops_status", "retired_at", "retired_by", "retired_on"], "{new}");
    assert_eq!((&old["attributes"]["monitoring_ref"], &old["attributes"]["note"]), (&json!("MON-1"), &json!("in use")));
    assert_eq!(new["attributes"]["monitoring_ref"], Value::Null);
    assert!(rows[0].2.as_ref().unwrap().get("source").is_none(), "the transition's own row is unchanged");
    let sourced_total = w
        .count(
            "SELECT count(*) FROM audit_log WHERE entity_id = $1 AND new_value -> 'source' ->> 'kind' = 'workflow_action'",
            ci,
        )
        .await;
    assert_eq!(sourced_total, 1, "approve has no actions");

    // The event marks the fields the actions set; the workflow.transition row carries the same.
    let (_, events) = w.call(&w.admin, "GET", &format!("{RUN}/{instance}/events"), None).await;
    let last = events["data"].as_array().unwrap().iter().rfind(|e| e["kind"] == "transition").unwrap().clone();
    let changes = &last["fieldChanges"];
    assert_eq!(changes["ops_status"]["origin"], "action", "{last}");
    assert_eq!(changes["retired_by"]["new"], json!(me.to_string()));
    assert!(changes["lifecycle"].get("origin").is_none(), "{last}");
    assert_eq!(rows[2].2.as_ref().unwrap()["fields"]["note"]["origin"], "action");

    // Version 3 sets a value that is then deleted: the run fails closed.
    publish(&w, graph(retire("scrapped"), json!([]))).await;
    let ci4 = w.ci(w.server).await;
    let body = json!({ "attributes": { "environment": "prod", "monitoring_ref": "MON-4" } });
    w.ok("PATCH", &format!("/api/v1/configuration-items/{ci4}"), body).await;
    let (_, v) = w.start(&w.admin, ci4).await;
    let i4 = id(&v["instance"]);
    let (status, v) = w.run(i4, "approve", json!({ "owner_team": "ops" })).await;
    assert_eq!(status, 200, "{v}");
    let scrapped = values.iter().find(|(k, _)| k == "scrapped").unwrap().1;
    let (status, v) = w.call(&w.admin, "DELETE", &format!("/api/v1/lookup-list-values/{scrapped}"), None).await;
    assert_eq!(status, 204, "{v}");
    let (before_ci, before_instance) = (w.ci_values(ci4).await, w.instance(i4).await);
    let audit = "SELECT count(*) FROM audit_log WHERE entity_id = $1";
    let before_audit = w.count(audit, ci4).await;
    let events = "SELECT count(*) FROM workflow_instance_events WHERE instance_id = $1";
    let before_events = w.count(events, i4).await;
    let (status, v) = w.run(i4, "go_live", json!({})).await;
    assert_eq!((status, code(&v)), (422, "WORKFLOW_ACTION_INVALID"), "{v}");
    assert_eq!(details(&v), pairs(&[("action", "set_attributes"), ("attributes.ops_status", "not_found")]), "{v}");
    assert_eq!(v["error"]["details"][0]["field"], "action", "details[0] names the action");
    assert_eq!(w.ci_values(ci4).await, before_ci, "the CI is unchanged");
    assert_eq!(w.instance(i4).await, before_instance, "the instance did not move");
    assert_eq!((w.count(audit, ci4).await, w.count(events, i4).await), (before_audit, before_events));

    // Bulk: the version 3 item fails and is reported; the version 2 items apply.
    let mut items = Vec::new();
    for i in [instances[1].1, i4, instances[2].1] {
        let version = w.instance(i).await["version"].clone();
        items.push(json!({ "instanceId": i, "transitionKey": "go_live", "expectedVersion": version }));
    }
    let v = w.ok("POST", &format!("{RUN}/bulk-transitions"), json!({ "items": items })).await;
    assert_eq!((v["succeeded"].as_i64(), v["failed"].as_i64()), (Some(2), Some(1)), "{v}");
    let failed = &v["results"][1];
    assert_eq!((failed["ok"].as_bool(), failed["instanceId"].as_str()), (Some(false), Some(i4.to_string().as_str())));
    assert_eq!(failed["error"]["code"], "WORKFLOW_ACTION_INVALID", "{v}");
    for (ci, _) in [instances[1], instances[2]] {
        assert_eq!(w.ci_values(ci).await["attributes"]["ops_status"], json!(retired.to_string()));
    }
    assert_eq!(w.ci_values(ci4).await, before_ci, "the failed item wrote nothing");
    db.drop().await;
}

/// A final approval applies the actions with the decider as actor (§7.1) and
/// names the request and its requester in the action's audit row.
#[tokio::test]
async fn a_final_approval_applies_the_actions_as_the_decider() {
    let Some(db) = scratch::database("workflow_actions_approval").await else { return };
    let w = world(&db).await;
    let p = setup(&w).await;
    let person = w.person_class().await;
    w.field("approved_by", "reference", json!({ "referenceClassId": person })).await;
    w.field("approved_on", "date", json!({})).await;
    let mut g = super::approvals_runtime_tests::graph();
    g["transitions"][0]["setAttributes"] = json!([{ "attribute": "approved_by", "valueFrom": "actor" }, { "attribute": "approved_on", "valueFrom": "today" }]);
    publish(&w, g).await;
    let (ci, instance) = started(&w).await;
    let (status, v) = request(&w, &p.req.0, instance, "approve", json!({ "owner_team": "ops", "risk": 2 })).await;
    assert_eq!(status, 202, "{v}");
    assert_eq!(w.ci_values(ci).await["attributes"]["approved_by"], Value::Null, "nothing applied on request");
    for creds in [&p.tech.0, &p.a1.0, &p.a2.0] {
        let (status, v) = decide(&w, creds, instance, "approve", None).await;
        assert!(status == 200 || status == 201, "{v}");
    }
    assert_eq!(w.instance(instance).await["state"]["key"], "approved");

    // The decider (a2, who may view servers only) is the actor; the request and its requester are named.
    let a = w.ci_values(ci).await["attributes"].clone();
    assert_eq!(a["approved_by"], json!(w.person_of(p.a2.1).await.to_string()), "{a}");
    assert_eq!(a["owner_team"], "ops");
    let request: Uuid = sqlx::query_scalar("SELECT id FROM workflow_approval_requests WHERE instance_id = $1")
        .bind(instance)
        .fetch_one(&w.pool)
        .await
        .unwrap();
    let rows: Vec<(Option<String>, Value)> = sqlx::query_as(
        "SELECT actor_name, new_value FROM audit_log
         WHERE entity_id = $1 AND action = 'update' AND new_value -> 'source' ->> 'kind' = 'workflow_action'",
    )
    .bind(ci)
    .fetch_all(&w.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    let (actor, new) = &rows[0];
    assert_eq!(actor.as_deref(), Some("a2"));
    assert_eq!(new["approvalRequestId"], json!(request.to_string()), "{new}");
    assert_eq!(new["requestedBy"]["name"], "req", "{new}");
    assert_eq!(new["source"]["transitionKey"], "approve");
    db.drop().await;
}

/// Format 14 (§12): `setAttributes` round-trips through the configuration
/// file with the same checksum; a format 13 file (no actions) still imports,
/// with no action.
#[tokio::test]
async fn attribute_actions_round_trip_and_a_format_13_file_imports_without_actions() {
    let Some(src) = scratch::database("workflow_actions_config_src").await else { return };
    let Some(dst) = scratch::database("workflow_actions_config_dst").await else { return };
    let w = world(&src).await;
    let (list, _) = w.list("ops_status", &["in_use", "retired"]).await;
    w.field("ops_status", "lookup", json!({ "lookupListId": list })).await;
    w.field("retired_on", "date", json!({})).await;
    let actions =
        json!([{ "attribute": "ops_status", "value": "retired" }, { "attribute": "retired_on", "valueFrom": "today" }]);
    publish(&w, graph(actions.clone(), json!([]))).await;
    let (status, file, _) = call(&w.app, "GET", "/api/v1/admin/config/export", &w.admin, None).await;
    assert_eq!(status, 200, "{file}");
    assert_eq!(file["formatVersion"], 14);
    let flow = file["workflows"].as_array().unwrap().iter().find(|f| f["key"] == "server_lifecycle").unwrap();
    assert_eq!(flow["graph"]["transitions"][1]["setAttributes"], actions);
    assert!(flow["graph"]["transitions"][0].get("setAttributes").is_none());
    let src_sum: Vec<u8> = sqlx::query_scalar(
        "SELECT v.checksum FROM workflow_versions v JOIN workflow_definitions d ON d.current_version_id = v.id
             WHERE d.id = $1",
    )
    .bind(w.definition)
    .fetch_one(&w.pool)
    .await
    .unwrap();

    let password = format!("test passphrase {}", Uuid::new_v4());
    let dst_app = app(dst.pool.clone());
    let setup = json!({ "username": "admin", "email": "admin@example.test", "displayName": "Admin",
        "password": password, "setupToken": crate::auth::setup_token::TEST_TOKEN });
    let (status, me, headers) = call(&dst_app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
    assert_eq!(status, 201, "{me}");
    let admin = session_of(&me, &headers);
    let import = |f: Value| {
        let (app, admin) = (dst_app.clone(), admin.clone());
        async move { call(&app, "POST", "/api/v1/admin/config/import?mode=apply", &admin, Some(f)).await }
    };
    let set_attributes = "SELECT count(*) FROM workflow_transition_set_attributes";

    // Format 13: the same file without setAttributes imports, and no action exists.
    let mut v13 = file.clone();
    v13["formatVersion"] = json!(13);
    for f in v13["workflows"].as_array_mut().unwrap() {
        for t in f["graph"]["transitions"].as_array_mut().unwrap() {
            t.as_object_mut().unwrap().remove("setAttributes");
        }
    }
    let (status, v, _) = import(v13).await;
    assert_eq!(status, 200, "{v}");
    let n: i64 = sqlx::query_scalar(set_attributes).fetch_one(&dst.pool).await.unwrap();
    assert_eq!(n, 0);

    // Format 14: a new version with the actions, checksummed as on the source.
    let (status, v, _) = import(file).await;
    assert_eq!(status, 200, "{v}");
    let n: i64 = sqlx::query_scalar(set_attributes).fetch_one(&dst.pool).await.unwrap();
    assert_eq!(n, 2);
    let dst_sum: Vec<u8> = sqlx::query_scalar(
        "SELECT v.checksum FROM workflow_versions v JOIN workflow_definitions d ON d.current_version_id = v.id
         WHERE d.key = 'server_lifecycle'",
    )
    .fetch_one(&dst.pool)
    .await
    .unwrap();
    assert_eq!(dst_sum, src_sum, "the same actions have the same checksum on both installs");
    src.drop().await;
    dst.drop().await;
}
