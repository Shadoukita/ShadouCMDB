//! QA edge tests (SHAA-2979): attribute actions of slice S2 (SHAA-2732,
//! PR #840) through the real router against PostgreSQL: literals of the
//! wrong type, required and read-only targets, a field's rules changed or
//! the field archived after publishing, the rights a run writes with (direct
//! run, API token, final approval, an actor without a Person), and the audit
//! row and history entry every write leaves, with its actor.

use serde_json::{Value, json};
use uuid::Uuid;

use super::approvals_runtime_tests::{audit_ok, decide, publish, request, setup, started};
use super::runtime_tests::{DEFS, RUN, World, details, id, pairs, world};
use crate::db::scratch;
use crate::modules::api_tokens::tests::{Creds, code};

async fn field(w: &World, key: &str, data_type: &str, extra: Value) -> Uuid {
    let mut body = json!({ "classId": w.server, "key": key, "label": key, "dataType": data_type });
    body.as_object_mut().unwrap().extend(extra.as_object().cloned().unwrap_or_default());
    id(&w.ok("POST", "/api/v1/attribute-definitions", body).await)
}

async fn instance(w: &World, instance: Uuid) -> Value {
    let (status, v) = w.call(&w.admin, "GET", &format!("{RUN}/{instance}"), None).await;
    assert_eq!(status, 200, "{v}");
    v["instance"].clone()
}

async fn run_as(w: &World, creds: &Creds, i: Uuid, key: &str, fields: Value) -> (u16, Value) {
    let version = instance(w, i).await["version"].clone();
    let body = json!({ "transitionKey": key, "expectedVersion": version, "fields": fields, "comment": "ok" });
    w.transition(creds, i, body).await
}

async fn count(w: &World, sql: &str, id: Uuid) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned())).bind(id).fetch_one(&w.pool).await.unwrap()
}

/// The world's version 1 graph with `go_live` setting `actions`.
fn graph(actions: Value) -> Value {
    json!({
        "initialState": "planned",
        "states": [
            { "key": "planned", "name": "Planned", "category": "open", "stateValue": "planned" },
            { "key": "approved", "name": "Approved", "category": "active", "stateValue": "approved" },
            { "key": "done", "name": "In production", "category": "done", "terminal": true, "stateValue": "live" }
        ],
        "transitions": [
            { "key": "approve", "name": "Approve", "from": "planned", "to": "approved",
              "fields": [ { "attribute": "owner_team", "required": true } ] },
            { "key": "go_live", "name": "Go live", "from": "approved", "to": "done", "setAttributes": actions }
        ]
    })
}

/// A CI of the current version, approved and ready to go live: (CI, instance).
async fn approved(w: &World) -> (Uuid, Uuid) {
    let ci = w.ci(w.server).await;
    let (status, v) = w.start(&w.admin, ci).await;
    assert_eq!(status, 201, "{v}");
    let i = id(&v["instance"]);
    let (status, v) = run_as(w, &w.admin, i, "approve", json!({ "owner_team": "ops" })).await;
    assert_eq!(status, 200, "{v}");
    (ci, i)
}

/// (path, code) of the problems on attribute actions, sorted.
fn action_problems(v: &Value) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = v["problems"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["path"].as_str().unwrap().contains("setAttributes"))
        .map(|p| (p["path"].as_str().unwrap().to_owned(), p["code"].as_str().unwrap().to_owned()))
        .collect();
    out.sort();
    out
}

async fn validate_draft(w: &World, actions: Value) -> Value {
    let draft = format!("{DEFS}/{}/draft", w.definition);
    w.ok("PUT", &draft, graph(actions)).await;
    let (status, v) = w.call(&w.admin, "POST", &format!("{draft}/validate"), None).await;
    assert_eq!(status, 200, "{v}");
    v
}

/// The `update` rows the actions wrote on `ci`: (actor type, actor id, actor name, request id, new value).
async fn action_rows(w: &World, ci: Uuid) -> Vec<(String, Option<String>, Option<String>, Option<String>, Value)> {
    sqlx::query_as(
        "SELECT actor_type, actor_id, actor_name, request_id, new_value FROM audit_log
         WHERE entity_id = $1 AND action = 'update' AND new_value -> 'source' ->> 'kind' = 'workflow_action'
         ORDER BY id",
    )
    .bind(ci)
    .fetch_all(&w.pool)
    .await
    .unwrap()
}

/// Replaces the grants of the world's workflow: every transition to `profiles`.
async fn grant(w: &World, profiles: &[&str]) {
    let def = w.ok("GET", &format!("{DEFS}/{}", w.definition), json!(null)).await;
    let grants: Vec<Value> = ["approve", "go_live", "_cancel"]
        .iter()
        .map(|t| json!({ "transitionKey": t, "profiles": profiles }))
        .collect();
    w.ok("PUT", &format!("{DEFS}/{}/grants", w.definition), json!({ "version": def["version"], "grants": grants }))
        .await;
}

/// A literal of the wrong type for its field never reaches a CI: the draft
/// saves (drafts are not linted), validation names every one at `.value`,
/// and publishing refuses them. Covers text into integer, number, boolean,
/// date, datetime, ip and cidr fields, an integer into a text field, and a
/// number out of an integer field's bounds.
#[tokio::test]
async fn qa_s2_literals_of_the_wrong_type_are_refused_at_publish() {
    let Some(db) = scratch::database("qa_s2_literal_types").await else { return };
    let w = world(&db).await;
    field(&w, "count", "integer", json!({ "validation": { "min": 0, "max": 10 } })).await;
    field(&w, "ratio", "number", json!({})).await;
    field(&w, "flag", "boolean", json!({})).await;
    field(&w, "born", "date", json!({})).await;
    field(&w, "seen", "datetime", json!({})).await;
    field(&w, "addr", "ip", json!({})).await;
    field(&w, "net", "cidr", json!({})).await;
    field(&w, "note", "text", json!({})).await;
    let actions = json!([
        { "attribute": "count", "value": "abc" },          // 0 text into integer
        { "attribute": "ratio", "value": "1,5" },          // 1 text into number
        { "attribute": "flag", "value": "yes" },           // 2 text into boolean
        { "attribute": "born", "value": "31.12.2026" },    // 3 not ISO
        { "attribute": "seen", "value": "yesterday" },     // 4 not a timestamp
        { "attribute": "addr", "value": "999.1.1.1" },     // 5 not an address
        { "attribute": "net", "value": "not-a-network" },  // 6 not a network
        { "attribute": "note", "value": 42 }               // 7 integer into text
    ]);
    let v = validate_draft(&w, actions.clone()).await;
    assert_eq!(v["valid"], false, "{v}");
    let problems = action_problems(&v);
    for j in 0..8 {
        let path = format!("transitions[1].setAttributes[{j}].value");
        assert!(problems.iter().any(|(p, _)| *p == path), "no problem at {path}: {problems:?}");
    }
    assert_eq!(problems.len(), 8, "one problem each: {problems:?}");

    // Valid strings of the right shape, a whole number in "count" out of bounds, and 3 as "3".
    let edge = json!([
        { "attribute": "count", "value": 11 },
        { "attribute": "ratio", "value": "1.5" },
        { "attribute": "born", "value": "2026-02-30" }
    ]);
    let v = validate_draft(&w, edge).await;
    let problems = action_problems(&v);
    for j in 0..3 {
        let path = format!("transitions[1].setAttributes[{j}].value");
        assert!(problems.iter().any(|(p, _)| *p == path), "no problem at {path}: {problems:?}");
    }

    // Publishing refuses; no version 2, nothing an instance could run.
    let draft = format!("{DEFS}/{}/draft", w.definition);
    let d = w.ok("PUT", &draft, graph(actions)).await;
    let (status, p) = w
        .call(&w.admin, "POST", &format!("{draft}/publish"), Some(json!({ "expectedDraftChecksum": d["checksum"] })))
        .await;
    assert_eq!((status, code(&p)), (400, "VALIDATION_ERROR"), "{p}");
    let versions = count(&w, "SELECT count(*) FROM workflow_versions WHERE definition_id = $1 AND status <> 'draft'", w.definition).await;
    assert_eq!(versions, 1, "only version 1 is published");

    // The same values of the right type publish and are written as sent.
    let good = json!([
        { "attribute": "count", "value": 7 },
        { "attribute": "ratio", "value": 1.5 },
        { "attribute": "flag", "value": true },
        { "attribute": "born", "value": "2026-12-31" },
        { "attribute": "seen", "value": "2026-12-31T08:00:00Z" },
        { "attribute": "addr", "value": "192.0.2.10" },
        { "attribute": "net", "value": "192.0.2.0/24" },
        { "attribute": "note", "value": "42" }
    ]);
    publish(&w, graph(good)).await;
    let (ci, i) = approved(&w).await;
    let (status, v) = run_as(&w, &w.admin, i, "go_live", json!({})).await;
    assert_eq!(status, 200, "{v}");
    let a = w.ci_values(ci).await["attributes"].clone();
    assert_eq!(
        (a["count"].as_f64(), a["ratio"].as_f64(), &a["flag"], &a["born"], &a["addr"], &a["note"]),
        (Some(7.0), Some(1.5), &json!(true), &json!("2026-12-31"), &json!("192.0.2.10"), &json!("42")),
        "{a}"
    );
    db.drop().await;
}

/// Required fields: `clear` is refused at publish, `value: null` is no value
/// at all (400 when the draft is saved), and a field made required after
/// publishing fails the run with 422 WORKFLOW_ACTION_INVALID and nothing
/// written, exactly like a PATCH that clears it would.
#[tokio::test]
async fn qa_s2_required_targets_cannot_be_emptied() {
    let Some(db) = scratch::database("qa_s2_required").await else { return };
    let w = world(&db).await;
    field(&w, "mandatory", "text", json!({ "isRequired": true, "defaultValue": "x" })).await;
    let later = field(&w, "later", "text", json!({})).await;

    // `clear` on a required field: refused at publish.
    let v = validate_draft(&w, json!([{ "attribute": "mandatory", "valueFrom": "clear" }])).await;
    assert_eq!(
        action_problems(&v),
        pairs(&[("transitions[1].setAttributes[0].valueFrom", "required_attribute")]),
        "{v}"
    );

    // `value: null` is no value: the draft is refused as one without a value.
    let draft = format!("{DEFS}/{}/draft", w.definition);
    let (status, v) =
        w.call(&w.admin, "PUT", &draft, Some(graph(json!([{ "attribute": "mandatory", "value": null }])))).await;
    assert_eq!(status, 400, "{v}");
    assert_eq!(details(&v), pairs(&[("transitions[1].setAttributes[0]", "value_or_value_from")]), "{v}");

    // `later` becomes required after a version that clears it is published.
    publish(&w, graph(json!([{ "attribute": "later", "valueFrom": "clear" }]))).await;
    let (ci, i) = approved(&w).await;
    w.ok("PATCH", &format!("/api/v1/configuration-items/{ci}"), json!({ "attributes": { "later": "keep" } })).await;
    w.ok("PATCH", &format!("/api/v1/attribute-definitions/{later}"), json!({ "isRequired": true })).await;
    let before = w.ci_values(ci).await;
    let audit = count(&w, "SELECT count(*) FROM audit_log WHERE entity_id = $1", ci).await;
    let (status, v) = run_as(&w, &w.admin, i, "go_live", json!({})).await;
    assert_eq!((status, code(&v)), (422, "WORKFLOW_ACTION_INVALID"), "{v}");
    assert_eq!(details(&v), pairs(&[("action", "set_attributes"), ("attributes.later", "required")]), "{v}");
    assert_eq!(w.ci_values(ci).await, before, "nothing written");
    assert_eq!(instance(&w, i).await["state"]["key"], "approved", "the instance did not move");
    assert_eq!(count(&w, "SELECT count(*) FROM audit_log WHERE entity_id = $1", ci).await, audit, "no audit row");
    // A PATCH clearing it is refused the same way.
    let (status, v) = w
        .call(&w.admin, "PATCH", &format!("/api/v1/configuration-items/{ci}"), Some(json!({ "attributes": { "later": null } })))
        .await;
    assert_eq!((status, details(&v)), (400, pairs(&[("attributes.later", "required")])), "{v}");
    db.drop().await;
}

/// A rule tightened after publishing (an integer's maximum, a text's maximum
/// length, a lookup value retired) fails the run with 422 naming the field;
/// nothing is written, and the run passes again once the rule is relaxed.
#[tokio::test]
async fn qa_s2_rules_tightened_after_publish_fail_the_run_closed() {
    let Some(db) = scratch::database("qa_s2_tightened").await else { return };
    let w = world(&db).await;
    let count_id = field(&w, "count", "integer", json!({})).await;
    let note = field(&w, "note", "text", json!({})).await;
    let list = id(&w.ok("POST", "/api/v1/lookup-lists", json!({ "key": "ops", "name": "ops" })).await);
    let retired = id(&w.ok("POST", "/api/v1/lookup-list-values", json!({ "listId": list, "key": "retired", "name": "r" })).await);
    field(&w, "ops", "lookup", json!({ "lookupListId": list })).await;
    publish(
        &w,
        graph(json!([
            { "attribute": "count", "value": 50 },
            { "attribute": "note", "value": "a rather long note" },
            { "attribute": "ops", "value": "retired" }
        ])),
    )
    .await;
    let (ci, i) = approved(&w).await;
    let before = w.ci_values(ci).await;

    async fn fails(w: &World, i: Uuid, ci: Uuid, before: &Value, field: &str, problem: &str) {
        let (status, v) = run_as(w, &w.admin, i, "go_live", json!({})).await;
        assert_eq!((status, code(&v)), (422, "WORKFLOW_ACTION_INVALID"), "{v}");
        let d = details(&v);
        assert!(d.contains(&(format!("attributes.{field}"), problem.to_owned())), "{field} {problem}: {v}");
        assert_eq!(&w.ci_values(ci).await, before, "nothing written");
    }
    let def = |id: Uuid| format!("/api/v1/attribute-definitions/{id}");
    w.ok("PATCH", &def(count_id), json!({ "validation": { "max": 10 } })).await;
    fails(&w, i, ci, &before, "count", "too_big").await;
    w.ok("PATCH", &def(count_id), json!({ "validation": null })).await;
    w.ok("PATCH", &def(note), json!({ "validation": { "maxLength": 5 } })).await;
    fails(&w, i, ci, &before, "note", "too_big").await;
    w.ok("PATCH", &def(note), json!({ "validation": null })).await;
    w.ok("PATCH", &format!("/api/v1/lookup-list-values/{retired}"), json!({ "isActive": false })).await;
    fails(&w, i, ci, &before, "ops", "lookup_value_inactive").await;
    w.ok("PATCH", &format!("/api/v1/lookup-list-values/{retired}"), json!({ "isActive": true })).await;

    let (status, v) = run_as(&w, &w.admin, i, "go_live", json!({})).await;
    assert_eq!(status, 200, "{v}");
    let a = w.ci_values(ci).await["attributes"].clone();
    assert_eq!((a["count"].as_f64(), &a["ops"]), (Some(50.0), &json!(retired.to_string())), "{a}");
    db.drop().await;
}

/// Read-only and foreign targets: the Person type's Name (as its Email) and
/// an identifying field are refused at publish; a field of another type is
/// refused when the draft is saved.
#[tokio::test]
async fn qa_s2_read_only_and_foreign_targets_are_refused() {
    let Some(db) = scratch::database("qa_s2_read_only").await else { return };
    let w = world(&db).await;
    let person: Uuid = sqlx::query_scalar("SELECT id FROM ci_classes WHERE system_role = 'person'")
        .fetch_one(&w.pool)
        .await
        .unwrap();
    let name: String = sqlx::query_scalar("SELECT key FROM ci_attribute_definitions WHERE system_role = 'person_name'")
        .fetch_one(&w.pool)
        .await
        .unwrap();
    let p = id(&w.ok("POST", DEFS, json!({ "key": "people", "name": "People", "classId": person })).await);
    let people = json!({ "initialState": "a", "states": [
            { "key": "a", "name": "A", "category": "open" },
            { "key": "b", "name": "B", "category": "done", "terminal": true } ],
        "transitions": [ { "key": "end", "name": "End", "from": "a", "to": "b",
                           "setAttributes": [ { "attribute": name, "value": "Somebody Else" } ] } ] });
    w.ok("PUT", &format!("{DEFS}/{p}/draft"), people).await;
    let (_, v) = w.call(&w.admin, "POST", &format!("{DEFS}/{p}/draft/validate"), None).await;
    assert_eq!(
        action_problems(&v),
        pairs(&[("transitions[0].setAttributes[0].attribute", "read_only_attribute")]),
        "{v}"
    );

    field(&w, "serial", "text", json!({ "isIdentifying": true })).await;
    let v = validate_draft(&w, json!([{ "attribute": "serial", "valueFrom": "clear" }])).await;
    assert_eq!(
        action_problems(&v),
        pairs(&[("transitions[1].setAttributes[0].attribute", "identifying_attribute")]),
        "clearing an identifying field is refused too: {v}"
    );

    let body = json!({ "classId": w.network, "key": "vlan", "label": "vlan", "dataType": "text" });
    w.ok("POST", "/api/v1/attribute-definitions", body).await;
    let (status, v) = w
        .call(&w.admin, "PUT", &format!("{DEFS}/{}/draft", w.definition), Some(graph(json!([{ "attribute": "vlan", "value": "1" }]))))
        .await;
    assert_eq!((status, details(&v)), (400, pairs(&[("transitions[1].setAttributes[0].attribute", "unknown_attribute")])), "{v}");
    db.drop().await;
}

/// A field an action of a published version sets cannot be archived; nor
/// while an instance still runs on a retired version that sets it. Once no
/// version that runs sets it, it is archived; a draft that still sets it then
/// fails validation with `inactive_attribute`.
#[tokio::test]
async fn qa_s2_a_target_is_not_archived_while_a_version_that_runs_sets_it() {
    let Some(db) = scratch::database("qa_s2_archive").await else { return };
    let w = world(&db).await;
    let note = field(&w, "note", "text", json!({})).await;
    publish(&w, graph(json!([{ "attribute": "note", "value": "set" }]))).await;
    let path = format!("/api/v1/attribute-definitions/{note}");
    let (status, v) = w.call(&w.admin, "PATCH", &path, Some(json!({ "isActive": false }))).await;
    assert_eq!((status, code(&v)), (409, "IN_USE"), "{v}");
    let (status, v) = w.call(&w.admin, "DELETE", &path, None).await;
    assert_eq!((status, code(&v)), (409, "IN_USE"), "{v}");

    // An instance on version 2; version 3 no longer sets the field.
    let (ci, i) = approved(&w).await;
    publish(&w, graph(json!([]))).await;
    let (status, v) = w.call(&w.admin, "PATCH", &path, Some(json!({ "isActive": false }))).await;
    assert_eq!((status, code(&v)), (409, "IN_USE"), "the retired version 2 still runs: {v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("v2 (retired)"), "{v}");

    // The instance on version 2 still applies its action.
    let (status, v) = run_as(&w, &w.admin, i, "go_live", json!({})).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(w.ci_values(ci).await["attributes"]["note"], "set");

    // Nothing runs on version 2 any more: the field is archived; a draft setting it is refused at validation.
    let (status, v) = w.call(&w.admin, "PATCH", &path, Some(json!({ "isActive": false }))).await;
    assert_eq!(status, 200, "{v}");
    let v = validate_draft(&w, json!([{ "attribute": "note", "value": "again" }])).await;
    assert_eq!(
        action_problems(&v),
        pairs(&[("transitions[1].setAttributes[0].attribute", "inactive_attribute")]),
        "{v}"
    );
    db.drop().await;
}

/// The rights an action writes with (§7.1). A user granted the transition
/// who may only view the type gets 403 on a direct run and nothing written;
/// one who may edit the type but not view Person still gets `valueFrom:
/// actor` (the documented exception); an API token writes as its owner with
/// actor type api_client; an actor linked to no Person fails with 422
/// `no_person` and nothing written.
#[tokio::test]
async fn qa_s2_actions_write_with_the_runner_rights_and_add_none() {
    let Some(db) = scratch::database("qa_s2_rights").await else { return };
    let w = world(&db).await;
    let person: Uuid = sqlx::query_scalar("SELECT id FROM ci_classes WHERE system_role = 'person'")
        .fetch_one(&w.pool)
        .await
        .unwrap();
    field(&w, "retired_by", "reference", json!({ "referenceClassId": person })).await;
    field(&w, "note", "text", json!({})).await;
    publish(&w, graph(json!([{ "attribute": "retired_by", "valueFrom": "actor" }, { "attribute": "note", "value": "done" }]))).await;
    let watchers = w.profile("Watchers", &[(w.server, false)]).await;
    grant(&w, &["Approvers", "Watchers"]).await;
    let (watcher, _) = w.user("watcher", &[watchers]).await;
    let (approver, approver_id) = w.user("approver", &[w.approvers]).await;

    // View only: 403, nothing written, no audit row.
    let (ci, i) = approved(&w).await;
    let before = w.ci_values(ci).await;
    let (status, v) = run_as(&w, &watcher, i, "go_live", json!({})).await;
    assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{v}");
    assert_eq!(w.ci_values(ci).await, before);
    assert!(action_rows(&w, ci).await.is_empty());

    // Edit on servers, no view on Person: the actor's own Person is written.
    let (status, v) = w.call(&approver, "GET", &format!("/api/v1/configuration-items?classId={person}"), None).await;
    assert!(status == 403 || v["page"]["total"] == 0, "the approver may not view Person: {status} {v}");
    let (status, v) = run_as(&w, &approver, i, "go_live", json!({})).await;
    assert_eq!(status, 200, "{v}");
    let me: Option<Uuid> = sqlx::query_scalar("SELECT person_ci_id FROM users WHERE id = $1")
        .bind(approver_id)
        .fetch_one(&w.pool)
        .await
        .unwrap();
    assert_eq!(w.ci_values(ci).await["attributes"]["retired_by"], json!(me.unwrap().to_string()));
    let rows = action_rows(&w, ci).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let approver_str = approver_id.to_string();
    assert_eq!((rows[0].0.as_str(), rows[0].1.as_deref(), rows[0].2.as_deref()), ("user", Some(approver_str.as_str()), Some("approver")));

    // An API token of the approver: actor type api_client, the owner named.
    let token = w.token(approver_id, w.approvers).await;
    let (ci2, i2) = approved(&w).await;
    let (status, v) = run_as(&w, &token, i2, "go_live", json!({})).await;
    assert_eq!(status, 200, "{v}");
    let rows = action_rows(&w, ci2).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!((rows[0].0.as_str(), rows[0].1.as_deref()), ("api_client", Some(approver_str.as_str())), "{rows:?}");
    assert_eq!(w.ci_values(ci2).await["attributes"]["retired_by"], json!(me.unwrap().to_string()));

    // No Person (as an account without an e-mail address): 422 no_person, nothing written.
    sqlx::query("UPDATE users SET person_ci_id = NULL WHERE id = $1").bind(approver_id).execute(&w.pool).await.unwrap();
    let (ci3, i3) = approved(&w).await;
    let before = w.ci_values(ci3).await;
    let (status, v) = run_as(&w, &approver, i3, "go_live", json!({})).await;
    assert_eq!((status, code(&v)), (422, "WORKFLOW_ACTION_INVALID"), "{v}");
    assert_eq!(details(&v), pairs(&[("action", "set_attributes"), ("attributes.retired_by", "no_person")]), "{v}");
    assert_eq!(w.ci_values(ci3).await, before);
    assert!(action_rows(&w, ci3).await.is_empty());
    db.drop().await;
}

/// The final approval writes as the decider, who may only view the type
/// (approvals A-Q7, actions §7.1: the transition grant and the approver role
/// are the right, narrowed to editing this CI). The decider is the actor of
/// the action's audit row, and of nothing else.
#[tokio::test]
async fn qa_s2_a_view_only_decider_applies_the_actions_as_actor() {
    let Some(db) = scratch::database("qa_s2_decider").await else { return };
    let w = world(&db).await;
    let p = setup(&w).await;
    field(&w, "approved_note", "text", json!({})).await;
    let mut g = super::approvals_runtime_tests::graph();
    g["transitions"][0]["setAttributes"] = json!([{ "attribute": "approved_note", "value": "approved by CAB" }]);
    publish(&w, g).await;
    let (ci, instance) = started(&w).await;
    let (status, v) = request(&w, &p.req.0, instance, "approve", json!({ "owner_team": "ops" })).await;
    assert_eq!(status, 202, "{v}");
    for creds in [&p.tech.0, &p.a1.0, &p.a2.0] {
        let (status, v) = decide(&w, creds, instance, "approve", None).await;
        assert!(status == 200 || status == 201, "{v}");
    }
    // a2 cannot edit a server by PATCH...
    let (status, _) = w
        .call(&p.a2.0, "PATCH", &format!("/api/v1/configuration-items/{ci}"), Some(json!({ "attributes": { "approved_note": "x" } })))
        .await;
    assert_eq!(status, 403, "a2 may view servers only");
    // ...but the final approval wrote the action as a2.
    assert_eq!(w.ci_values(ci).await["attributes"]["approved_note"], "approved by CAB");
    let rows = action_rows(&w, ci).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let a2 = p.a2.1.to_string();
    assert_eq!((rows[0].0.as_str(), rows[0].1.as_deref(), rows[0].2.as_deref()), ("user", Some(a2.as_str()), Some("a2")));
    audit_ok(&w).await;
    db.drop().await;
}

/// Every action write leaves one audit row in the transition's request, by
/// the runner, listed in the CI's history (`GET /audit-log` on the CI); a run
/// whose actions change nothing writes none; a bulk run writes one per item;
/// the audit chain verifies.
#[tokio::test]
async fn qa_s2_every_action_write_is_audited_in_the_ci_history() {
    let Some(db) = scratch::database("qa_s2_audit").await else { return };
    let w = world(&db).await;
    field(&w, "note", "text", json!({})).await;
    field(&w, "count", "integer", json!({})).await;
    publish(&w, graph(json!([{ "attribute": "note", "value": "live" }, { "attribute": "count", "value": 3 }]))).await;
    let (approver, approver_id) = w.user("approver", &[w.approvers]).await;

    // A direct run.
    let (ci, i) = approved(&w).await;
    let (status, v) = run_as(&w, &approver, i, "go_live", json!({})).await;
    assert_eq!(status, 200, "{v}");
    let rows = action_rows(&w, ci).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let (_, actor_id, actor, request_id, new) = &rows[0];
    assert_eq!((actor_id.clone(), actor.as_deref()), (Some(approver_id.to_string()), Some("approver")));
    let transition_request: Option<String> = sqlx::query_scalar(
        "SELECT request_id FROM audit_log WHERE entity_id = $1 AND action = 'workflow.transition' ORDER BY id DESC LIMIT 1",
    )
    .bind(ci)
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!(request_id, &transition_request, "the same request as the transition");
    assert_eq!((new["attributes"]["note"].clone(), new["attributes"]["count"].as_f64()), (json!("live"), Some(3.0)));

    // The CI's history shows it, with the actor.
    let (status, h) = w
        .call(&w.admin, "GET", &format!("/api/v1/audit-log?entityType=configuration_items&entityId={ci}&action=update"), None)
        .await;
    assert_eq!(status, 200, "{h}");
    let entry = h["data"].as_array().unwrap().iter().find(|e| e["newValue"]["source"]["kind"] == "workflow_action").cloned();
    let entry = entry.unwrap_or_else(|| panic!("no action entry in the history: {h}"));
    assert_eq!((entry["actorName"].as_str(), entry["actorType"].as_str()), (Some("approver"), Some("user")), "{entry}");
    assert_eq!(entry["newValue"]["source"]["transitionKey"], "go_live");

    // The actions change nothing: no row, no origin marker.
    let (ci2, i2) = approved(&w).await;
    w.ok("PATCH", &format!("/api/v1/configuration-items/{ci2}"), json!({ "attributes": { "note": "live", "count": 3 } })).await;
    let (status, v) = run_as(&w, &approver, i2, "go_live", json!({})).await;
    assert_eq!(status, 200, "{v}");
    assert!(action_rows(&w, ci2).await.is_empty(), "nothing changed, nothing audited");
    let (_, events) = w.call(&w.admin, "GET", &format!("{RUN}/{i2}/events"), None).await;
    let last = events["data"].as_array().unwrap().iter().rfind(|e| e["kind"] == "transition").unwrap().clone();
    assert!(last["fieldChanges"].get("note").is_none(), "{last}");

    // Bulk: one row per item, by the bulk caller.
    let mut items = Vec::new();
    let mut cis = Vec::new();
    for _ in 0..2 {
        let (c, i) = approved(&w).await;
        let version = instance(&w, i).await["version"].clone();
        items.push(json!({ "instanceId": i, "transitionKey": "go_live", "expectedVersion": version }));
        cis.push(c);
    }
    let (status, v) = w.call(&approver, "POST", &format!("{RUN}/bulk-transitions"), Some(json!({ "items": items }))).await;
    assert_eq!((status, v["succeeded"].as_i64()), (200, Some(2)), "{v}");
    for c in cis {
        let rows = action_rows(&w, c).await;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].2.as_deref(), Some("approver"));
    }
    audit_ok(&w).await;
    db.drop().await;
}
