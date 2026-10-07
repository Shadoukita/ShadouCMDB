//! Slice S6 (SHAA-1427) through the real router against PostgreSQL: moving
//! running instances to a newer version (dry run and real), bulk transitions
//! with mixed results, and the workflow history of a purged CI moving to the
//! archive.

use serde_json::{Value, json};
use uuid::Uuid;

use super::runtime_tests::{DEFS, RUN, World, details, id, pairs, world};
use crate::db::scratch;
use crate::modules::api_tokens::tests::code;

async fn audit_clean(w: &World) {
    let problems: Vec<(i64, String)> =
        sqlx::query_as("SELECT chain_seq, problem FROM audit_log_verify()").fetch_all(&w.pool).await.unwrap();
    assert_eq!(problems, vec![]);
}

/// Publishes the draft `graph` as the next version.
async fn publish(w: &World, graph: Value) {
    let draft = w.ok("PUT", &format!("{DEFS}/{}/draft", w.definition), graph).await;
    w.ok(
        "POST",
        &format!("{DEFS}/{}/draft/publish", w.definition),
        json!({ "expectedDraftChecksum": draft["checksum"], "changeNote": "next" }),
    )
    .await;
}

/// The world's graph with `planned` renamed `proposed`.
fn version_2() -> Value {
    json!({
        "initialState": "proposed",
        "states": [
            { "key": "proposed", "name": "Proposed", "category": "open", "stateValue": "planned" },
            { "key": "approved", "name": "Approved", "category": "active", "stateValue": "approved" },
            { "key": "done", "name": "In production", "category": "done", "terminal": true, "stateValue": "live" }
        ],
        "transitions": [
            { "key": "approve", "name": "Approve", "from": "proposed", "to": "approved" },
            { "key": "go_live", "name": "Go live", "from": "approved", "to": "done" }
        ]
    })
}

async fn instance(w: &World, instance: Uuid) -> Value {
    let (status, v) = w.call(&w.admin, "GET", &format!("{RUN}/{instance}"), None).await;
    assert_eq!(status, 200, "{v}");
    v["instance"].clone()
}

async fn force(w: &World, instance: Uuid, version: i64, state: &str) {
    w.ok(
        "POST",
        &format!("{RUN}/{instance}/force"),
        json!({ "expectedVersion": version, "stateKey": state, "reason": "test setup" }),
    )
    .await;
}

#[tokio::test]
async fn instances_migrate_to_a_newer_version_after_a_dry_run() {
    let Some(db) = scratch::database("workflow_instance_migration").await else { return };
    let w = world(&db).await;
    let path = format!("{DEFS}/{}/instance-migrations", w.definition);
    let (planned_ci, approved_ci) = (w.ci(w.server).await, w.ci(w.server).await);
    let (_, v) = w.start(&w.admin, planned_ci).await;
    let planned = id(&v["instance"]);
    let (_, v) = w.start(&w.admin, approved_ci).await;
    let approved = id(&v["instance"]);
    force(&w, approved, 1, "approved").await;
    // A completed instance stays where it is.
    let done_ci = w.ci(w.server).await;
    let (_, v) = w.start(&w.admin, done_ci).await;
    let done = id(&v["instance"]);
    force(&w, done, 1, "done").await;
    publish(&w, version_2()).await;

    let body = |from: i64, to: i64, map: Value, dry: bool| json!({ "fromVersionNo": from, "toVersionNo": to, "stateMap": map, "dryRun": dry });
    // Refusals, in a dry run as in a real one.
    for (b, expected) in [
        (body(9, 2, json!({}), true), pairs(&[("fromVersionNo", "unknown_version"), ("toVersionNo", "not_newer")])),
        (
            body(9, 10, json!({}), true),
            pairs(&[("fromVersionNo", "unknown_version"), ("toVersionNo", "unknown_version")]),
        ),
        (body(2, 1, json!({}), true), pairs(&[("toVersionNo", "not_newer")])),
        (body(1, 9, json!({}), true), pairs(&[("toVersionNo", "unknown_version")])),
        (body(1, 2, json!({ "nope": "approved" }), true), pairs(&[("stateMap.nope", "unknown_state")])),
        (body(1, 2, json!({ "planned": "nope" }), true), pairs(&[("stateMap.planned", "unknown_target_state")])),
        (body(1, 2, json!({ "planned": "done" }), true), pairs(&[("stateMap.planned", "terminal_target")])),
        (body(1, 2, json!({ "done": "approved" }), false), pairs(&[("stateMap.done", "terminal_source")])),
        // v2 has no `planned`, and one instance is in it.
        (body(1, 2, json!({}), true), pairs(&[("stateMap.planned", "unmapped")])),
        (body(1, 2, json!({}), false), pairs(&[("stateMap.planned", "unmapped")])),
    ] {
        let (status, v) = w.call(&w.admin, "POST", &path, Some(b.clone())).await;
        assert_eq!((status, details(&v)), (400, expected), "{b}: {v}");
    }
    let (status, v) = w
        .call(
            &w.admin,
            "POST",
            &format!("{DEFS}/{}/instance-migrations", Uuid::new_v4()),
            Some(body(1, 2, json!({}), true)),
        )
        .await;
    assert_eq!((status, code(&v)), (404, "NOT_FOUND"), "{v}");
    // A draft is no target.
    w.ok("PUT", &format!("{DEFS}/{}/draft", w.definition), version_2()).await;
    let (status, v) = w.call(&w.admin, "POST", &path, Some(body(1, 3, json!({ "planned": "proposed" }), true))).await;
    assert_eq!((status, details(&v)), (409, pairs(&[("toVersionNo", "not_published")])), "{v}");
    // Only someone who may edit every type the workflow runs on.
    let managers = w.profile("Network managers", &[(w.network, true)]).await;
    sqlx::query(
        "INSERT INTO permission_profile_global_permissions (profile_id, permission) VALUES ($1, 'workflows.manage')",
    )
    .bind(managers)
    .execute(&w.pool)
    .await
    .unwrap();
    let (manager, _) = w.user("network_manager", &[managers]).await;
    // GH#695: a workflow on a type they may not view answers like a missing one.
    let (status, v) = w.call(&manager, "POST", &path, Some(body(1, 2, json!({ "planned": "proposed" }), true))).await;
    assert_eq!((status, code(&v)), (404, "NOT_FOUND"), "{v}");
    assert!(!v.to_string().contains("server_lifecycle"), "{v}");
    let viewers = w.profile("Server viewers", &[(w.server, false)]).await;
    sqlx::query(
        "INSERT INTO permission_profile_global_permissions (profile_id, permission) VALUES ($1, 'workflows.manage')",
    )
    .bind(viewers)
    .execute(&w.pool)
    .await
    .unwrap();
    let (viewer, _) = w.user("server_viewer", &[viewers]).await;
    let (status, v) = w.call(&viewer, "POST", &path, Some(body(1, 2, json!({ "planned": "proposed" }), true))).await;
    assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{v}");

    // planned → approved (named, and the state field changes), approved → approved (same key).
    let map = json!({ "planned": "approved" });
    let audit_before: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log").fetch_one(&w.pool).await.unwrap();
    let dry = w.ok("POST", &path, body(1, 2, map.clone(), true)).await;
    assert_eq!(
        dry,
        json!({ "dryRun": true, "definitionKey": "server_lifecycle", "fromVersionNo": 1, "toVersionNo": 2,
            "total": 2, "migrated": 0, "batches": 0, "pendingApprovals": 0, "skipped": 0, "states": [
                { "fromState": "planned", "toState": "approved", "mappedBy": "explicit", "count": 1 },
                { "fromState": "approved", "toState": "approved", "mappedBy": "same_key", "count": 1 }
            ] })
    );
    let audit_after: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log").fetch_one(&w.pool).await.unwrap();
    assert_eq!(audit_after, audit_before, "a dry run writes nothing");
    assert_eq!(instance(&w, planned).await["versionNo"], 1);

    let updates = |rows: Vec<(String, Option<String>)>| rows.into_iter().filter(|(a, _)| a == "update").count();
    let approved_updates = updates(w.audit_rows(approved_ci).await);
    let real = w.ok("POST", &path, body(1, 2, map.clone(), false)).await;
    assert_eq!(
        (real["total"].as_i64(), real["migrated"].as_i64(), real["batches"].as_i64()),
        (Some(2), Some(2), Some(1))
    );
    assert_eq!(real["states"], dry["states"]);
    let p = instance(&w, planned).await;
    assert_eq!(
        (p["versionNo"].as_i64(), p["state"]["key"].as_str(), p["version"].as_i64()),
        (Some(2), Some("approved"), Some(2))
    );
    let a = instance(&w, approved).await;
    assert_eq!(
        (a["versionNo"].as_i64(), a["state"]["key"].as_str(), a["version"].as_i64()),
        (Some(2), Some("approved"), Some(3))
    );
    let d = instance(&w, done).await;
    assert_eq!((d["versionNo"].as_i64(), d["status"].as_str()), (Some(1), Some("completed")));
    assert_eq!(w.ci_values(planned_ci).await["attributes"]["lifecycle"], json!(w.value("approved").to_string()));

    // History: a migrate event with both versions; on the CI, the field update and workflow.migrate of one request.
    let (_, v) = w.call(&w.admin, "GET", &format!("{RUN}/{planned}/events"), None).await;
    let last = &v["data"][1];
    assert_eq!(
        (last["kind"].as_str(), last["fromStateKey"].as_str(), last["toStateKey"].as_str()),
        (Some("migrate"), Some("planned"), Some("approved"))
    );
    assert_eq!((last["fromVersionNo"].as_i64(), last["toVersionNo"].as_i64()), (Some(1), Some(2)));
    assert_eq!(last["fieldChanges"]["lifecycle"]["new"], json!(w.value("approved").to_string()));
    let rows = w.audit_rows(planned_ci).await;
    let tail: Vec<&str> = rows.iter().rev().take(2).map(|(a, _)| a.as_str()).collect();
    assert_eq!(tail, ["workflow.migrate", "update"]);
    assert_eq!(rows[rows.len() - 1].1, rows[rows.len() - 2].1, "one request");
    let (old, new): (Value, Value) = sqlx::query_as(
        "SELECT old_value, new_value FROM audit_log WHERE action = 'workflow.migrate' AND entity_id = $1",
    )
    .bind(approved_ci)
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!((old["versionNo"].as_i64(), old["stateKey"].as_str()), (Some(1), Some("approved")));
    assert_eq!(
        (new["versionNo"].as_i64(), new["stateKey"].as_str(), new["fields"].clone()),
        (Some(2), Some("approved"), Value::Null)
    );
    assert_eq!(
        updates(w.audit_rows(approved_ci).await),
        approved_updates,
        "no CI update when the state field keeps its value"
    );

    // Nothing is left on version 1, so a second run moves nothing; the moved instances run on version 2.
    let again = w.ok("POST", &path, body(1, 2, map, false)).await;
    assert_eq!(
        (again["total"].as_i64(), again["migrated"].as_i64(), again["batches"].as_i64()),
        (Some(0), Some(0), Some(0))
    );
    let (status, v) =
        w.transition(&w.admin, planned, json!({ "transitionKey": "go_live", "expectedVersion": 2 })).await;
    assert_eq!((status, v["status"].as_str(), v["versionNo"].as_i64()), (200, Some("completed"), Some(2)), "{v}");
    audit_clean(&w).await;
    db.drop().await;
}

#[tokio::test]
async fn bulk_transitions_commit_what_runs_and_report_the_rest() {
    let Some(db) = scratch::database("workflow_bulk_transitions").await else { return };
    let w = world(&db).await;
    let mut instances = Vec::new();
    let mut cis = Vec::new();
    for _ in 0..4 {
        let ci = w.ci(w.server).await;
        w.ok("PATCH", &format!("/api/v1/configuration-items/{ci}"), json!({ "attributes": { "environment": "prod" } }))
            .await;
        let (_, v) = w.start(&w.admin, ci).await;
        instances.push(id(&v["instance"]));
        cis.push(ci);
    }
    let bulk = format!("{RUN}/bulk-transitions");
    let approve = |instance: Uuid, version: i64, comment: Option<&str>| {
        json!({ "instanceId": instance, "transitionKey": "approve", "expectedVersion": version,
            "fields": { "owner_team": "ops" }, "comment": comment })
    };
    let missing = Uuid::new_v4();
    let body = json!({ "items": [
        approve(instances[0], 1, Some("CAB ok")),
        approve(instances[1], 1, None),
        approve(instances[2], 7, Some("CAB ok")),
        approve(missing, 1, Some("CAB ok")),
        // The same instance again, after the first item: runs on the version that one left.
        { "instanceId": instances[0], "transitionKey": "go_live", "expectedVersion": 2 },
        { "instanceId": instances[3], "transitionKey": "approve", "expectedVersion": 1, "fields": { "nope": 1 } },
    ] });
    let (status, v) = w.call(&w.admin, "POST", &bulk, Some(body)).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!((v["succeeded"].as_i64(), v["failed"].as_i64()), (Some(2), Some(4)), "{v}");
    let results = v["results"].as_array().unwrap();
    let summary: Vec<(i64, bool, &str)> = results
        .iter()
        .map(|r| (r["index"].as_i64().unwrap(), r["ok"].as_bool().unwrap(), r["error"]["code"].as_str().unwrap_or("")))
        .collect();
    assert_eq!(
        summary,
        [
            (0, true, ""),
            (1, false, "WORKFLOW_CONDITION_FAILED"),
            (2, false, "VERSION_CONFLICT"),
            (3, false, "NOT_FOUND"),
            (4, true, ""),
            (5, false, "VALIDATION_ERROR"),
        ]
    );
    assert_eq!(results[1]["error"]["details"][0]["field"], "comment");
    assert_eq!(results[5]["error"]["details"][0]["code"], "not_a_transition_field");
    assert_eq!(
        (results[4]["instance"]["status"].as_str(), results[4]["instance"]["version"].as_i64()),
        (Some("completed"), Some(3))
    );
    assert_eq!(results[1]["instance"], Value::Null);

    // The refused items wrote nothing: still planned at version 1, no audit row after the start.
    for i in [1, 2, 3] {
        let v = instance(&w, instances[i]).await;
        assert_eq!((v["state"]["key"].as_str(), v["version"].as_i64()), (Some("planned"), Some(1)), "{i}");
        let actions: Vec<String> = w.audit_rows(cis[i]).await.into_iter().map(|(a, _)| a).collect();
        assert_eq!(actions.last().map(String::as_str), Some("workflow.start"), "{i}: {actions:?}");
        assert!(!actions.iter().any(|a| a == "workflow.transition"), "{i}: {actions:?}");
    }
    // The ones that ran: one request, both transitions on the CI.
    let rows = w.audit_rows(cis[0]).await;
    let transitions: Vec<&Option<String>> =
        rows.iter().filter(|(a, _)| a == "workflow.transition").map(|(_, r)| r).collect();
    assert_eq!(transitions.len(), 2);
    assert_eq!(transitions[0], transitions[1], "one request");
    assert_eq!(w.ci_values(cis[0]).await["attributes"]["lifecycle"], json!(w.value("live").to_string()));

    // The caller's rights apply per item: an editor is granted nothing, a restricted reader sees no instance.
    let (editor, _) = w.user("editor", &[w.editors]).await;
    let (status, v) =
        w.call(&editor, "POST", &bulk, Some(json!({ "items": [approve(instances[1], 1, Some("x"))] }))).await;
    assert_eq!((status, v["results"][0]["error"]["code"].as_str()), (200, Some("FORBIDDEN")), "{v}");
    let networks = w.profile("Networks", &[(w.network, true)]).await;
    let (outsider, _) = w.user("outsider", &[networks]).await;
    let (status, v) =
        w.call(&outsider, "POST", &bulk, Some(json!({ "items": [approve(instances[1], 1, Some("x"))] }))).await;
    assert_eq!((status, v["results"][0]["error"]["code"].as_str()), (200, Some("NOT_FOUND")), "{v}");

    // The body: 1 to 500 items.
    let (status, v) = w.call(&w.admin, "POST", &bulk, Some(json!({ "items": [] }))).await;
    assert_eq!((status, code(&v)), (400, "VALIDATION_ERROR"), "{v}");
    let many: Vec<Value> = (0..501).map(|_| approve(instances[1], 1, Some("x"))).collect();
    let (status, v) = w.call(&w.admin, "POST", &bulk, Some(json!({ "items": many }))).await;
    assert_eq!((status, code(&v)), (400, "VALIDATION_ERROR"), "{v}");
    audit_clean(&w).await;
    db.drop().await;
}

/// Two bulk runs over the same CIs in opposite orders: both lock the CIs in
/// id order, so the second queues behind the first instead of deadlocking (a
/// 40P01 would be a 500). One runs every item, the other finds every
/// instance moved on.
#[tokio::test]
async fn opposite_bulk_runs_do_not_deadlock() {
    let Some(db) = scratch::database("workflow_bulk_no_deadlock").await else { return };
    let w = world(&db).await;
    let bulk = format!("{RUN}/bulk-transitions");
    for round in 0..5 {
        let mut instances = Vec::new();
        for _ in 0..10 {
            let ci = w.ci(w.server).await;
            w.ok(
                "PATCH",
                &format!("/api/v1/configuration-items/{ci}"),
                json!({ "attributes": { "environment": "prod" } }),
            )
            .await;
            let (_, v) = w.start(&w.admin, ci).await;
            instances.push(id(&v["instance"]));
        }
        let item = |i: &Uuid| {
            json!({ "instanceId": i, "transitionKey": "approve", "expectedVersion": 1,
            "fields": { "owner_team": "ops" }, "comment": "race" })
        };
        let forward = json!({ "items": instances.iter().map(item).collect::<Vec<_>>() });
        let backward = json!({ "items": instances.iter().rev().map(item).collect::<Vec<_>>() });
        let ((s1, v1), (s2, v2)) = tokio::join!(
            w.call(&w.admin, "POST", &bulk, Some(forward)),
            w.call(&w.admin, "POST", &bulk, Some(backward))
        );
        assert_eq!((s1, s2), (200, 200), "round {round}: {v1} {v2}");
        let mut succeeded = [v1["succeeded"].as_i64().unwrap(), v2["succeeded"].as_i64().unwrap()];
        succeeded.sort();
        assert_eq!(succeeded, [0, 10], "round {round}: {v1} {v2}");
    }
    audit_clean(&w).await;
    db.drop().await;
}

#[tokio::test]
async fn purging_a_type_moves_the_workflow_history_of_its_cis_to_the_archive() {
    let Some(db) = scratch::database("workflow_archive_on_purge").await else { return };
    let w = world(&db).await;
    let blade =
        id(&w.ok("POST", "/api/v1/ci-classes", json!({ "key": "blade", "name": "Blade", "parentId": w.server })).await);
    let blade_ci = id(&w
        .ok("POST", "/api/v1/configuration-items", json!({ "classId": blade, "attributes": { "environment": "test" } }))
        .await);
    let (_, v) = w.start(&w.admin, blade_ci).await;
    let instance = id(&v["instance"]);
    force(&w, instance, 1, "approved").await;
    let server_ci = w.ci(w.server).await;
    let (_, v) = w.start(&w.admin, server_ci).await;
    let kept = id(&v["instance"]);

    // Archive the type, then purge it: the CI goes, and its instance with it.
    let (status, v) = w.call(&w.admin, "DELETE", &format!("/api/v1/ci-classes/{blade}"), None).await;
    assert_eq!(status, 204, "{v}");
    let (status, v) = w
        .call(&w.admin, "POST", &format!("/api/v1/ci-classes/{blade}/purge"), Some(json!({ "confirm": "blade" })))
        .await;
    assert_eq!(status, 200, "{v}");
    let left: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM workflow_instances WHERE id = $1),
                (SELECT count(*) FROM workflow_instance_events WHERE instance_id = $1)",
    )
    .bind(instance)
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!(left, (0, 0));
    assert_eq!(instance_status(&w, kept).await, "active", "other CIs keep theirs");

    // The archive holds the instance with every event, and joins the CI's delete row.
    let (status, v) = w.call(&w.admin, "GET", &format!("/api/v1/admin/workflow-archive?ciId={blade_ci}"), None).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["page"]["total"], 1, "{v}");
    let a = &v["data"][0];
    assert_eq!(
        (a["instanceId"].as_str(), a["classKey"].as_str(), a["definitionKey"].as_str(), a["stateKey"].as_str()),
        (Some(instance.to_string().as_str()), Some("blade"), Some("server_lifecycle"), Some("approved"))
    );
    assert_eq!((a["status"].as_str(), a["versionNo"].as_i64()), (Some("active"), Some(1)));
    let kinds: Vec<&str> = a["events"].as_array().unwrap().iter().map(|e| e["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["start", "force"]);
    let delete_request: Option<String> = sqlx::query_scalar(
        "SELECT request_id FROM audit_log WHERE action = 'delete' AND entity_type = 'configuration_items' AND entity_id = $1",
    )
    .bind(blade_ci)
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert!(delete_request.is_some());
    assert_eq!(a["requestId"].as_str(), delete_request.as_deref());
    let (_, v) = w.call(&w.admin, "GET", "/api/v1/admin/workflow-archive?definitionKey=server_lifecycle", None).await;
    assert_eq!(v["page"]["total"], 1, "{v}");

    // A manager whose profile limits the types they may view gets nothing.
    let managers = w.profile("Server managers", &[(w.server, true)]).await;
    sqlx::query(
        "INSERT INTO permission_profile_global_permissions (profile_id, permission) VALUES ($1, 'workflows.manage')",
    )
    .bind(managers)
    .execute(&w.pool)
    .await
    .unwrap();
    let (manager, _) = w.user("server_manager", &[managers]).await;
    let (status, v) = w.call(&manager, "GET", "/api/v1/admin/workflow-archive", None).await;
    assert_eq!((status, v["page"]["total"].as_i64()), (200, Some(0)), "{v}");

    // The archive and the events stay append-only.
    let mut c = w.pool.acquire().await.unwrap();
    for stmt in [
        "UPDATE workflow_instance_archive SET state_key = 'x'",
        "DELETE FROM workflow_instance_archive",
        "TRUNCATE workflow_instance_archive",
        "DELETE FROM workflow_instance_events",
    ] {
        let err = sqlx::query(stmt).execute(&mut *c).await.expect_err(stmt);
        assert_eq!(err.as_database_error().and_then(|d| d.code()).as_deref(), Some("42501"), "{stmt}");
    }
    drop(c);
    audit_clean(&w).await;
    db.drop().await;
}

async fn instance_status(w: &World, instance: Uuid) -> String {
    sqlx::query_scalar("SELECT status FROM workflow_instances WHERE id = $1")
        .bind(instance)
        .fetch_one(&w.pool)
        .await
        .unwrap()
}

/// On a three-role install the API role may read the archive but not write
/// it, and still cannot delete events itself; deleting a CI row as the API
/// role archives its history through the trigger all the same.
#[tokio::test]
async fn the_api_role_archives_through_the_trigger_only() {
    let Some(roles) = scratch::Roles::create("the_api_role_archives_through_the_trigger_only").await else { return };
    let db = roles.database().await;
    let owner = &db.pool;
    for (privilege, held) in
        [("SELECT", true), ("INSERT", false), ("UPDATE", false), ("DELETE", false), ("TRUNCATE", false)]
    {
        let has: bool = sqlx::query_scalar("SELECT has_table_privilege($1, 'cmdb.workflow_instance_archive', $2)")
            .bind(&roles.app)
            .bind(privilege)
            .fetch_one(owner)
            .await
            .unwrap();
        assert_eq!(has, held, "{privilege}");
    }
    let class: Uuid = sqlx::query_scalar("SELECT id FROM ci_classes WHERE system_role = 'business_service'")
        .fetch_one(owner)
        .await
        .unwrap();
    let ci: Uuid =
        sqlx::query_scalar("INSERT INTO configuration_items (class_id, label) VALUES ($1, 'one') RETURNING id")
            .bind(class)
            .fetch_one(owner)
            .await
            .unwrap();
    let f = crate::db::upgrade_0046::workflow_fixture(owner, "lifecycle", class, ci, None).await;

    let api = roles.api_pool(&db).await;
    let mut c = api.acquire().await.unwrap();
    // Setting the archive variable does not open the events to the API role.
    sqlx::query("BEGIN").execute(&mut *c).await.unwrap();
    sqlx::query("SELECT set_config('shadoucmdb.workflow_archive', 'on', true)").execute(&mut *c).await.unwrap();
    let err = sqlx::query("DELETE FROM cmdb.workflow_instance_events WHERE instance_id = $1")
        .bind(f.instance)
        .execute(&mut *c)
        .await
        .unwrap_err();
    assert_eq!(err.as_database_error().and_then(|d| d.code()).as_deref(), Some("42501"));
    sqlx::query("ROLLBACK").execute(&mut *c).await.unwrap();
    let err = sqlx::query("INSERT INTO cmdb.workflow_instance_archive (instance_id, ci_id, ci_ident, ci_label, class_key,
           definition_id, definition_key, version_no, state_key, status, started_at, started_by_name, last_transition_at, events)
         VALUES (gen_random_uuid(), $1, 'x', 'x', 'x', gen_random_uuid(), 'x', 1, 'x', 'active', now(), 'x', now(), '[]')")
        .bind(ci)
        .execute(&mut *c)
        .await
        .unwrap_err();
    assert_eq!(err.as_database_error().and_then(|d| d.code()).as_deref(), Some("42501"));

    // Deleting the CI row moves the instance and both events.
    sqlx::query("BEGIN").execute(&mut *c).await.unwrap();
    sqlx::query("SELECT set_config('shadoucmdb.request_id', 'req-purge', true)").execute(&mut *c).await.unwrap();
    sqlx::query("DELETE FROM cmdb.configuration_items WHERE id = $1").bind(ci).execute(&mut *c).await.unwrap();
    sqlx::query("COMMIT").execute(&mut *c).await.unwrap();
    let (events, request, status): (Value, Option<String>, String) =
        sqlx::query_as("SELECT events, request_id, status FROM cmdb.workflow_instance_archive WHERE instance_id = $1")
            .bind(f.instance)
            .fetch_one(&mut *c)
            .await
            .unwrap();
    let kinds: Vec<&str> = events.as_array().unwrap().iter().map(|e| e["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["start", "transition"]);
    assert_eq!(events[1]["comment"], "CAB approved");
    assert_eq!((request.as_deref(), status.as_str()), (Some("req-purge"), "active"));
    let left: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM cmdb.workflow_instances WHERE id = $1)
              + (SELECT count(*) FROM cmdb.workflow_instance_events WHERE instance_id = $1)",
    )
    .bind(f.instance)
    .fetch_one(&mut *c)
    .await
    .unwrap();
    assert_eq!(left, 0);
    // The definition never kept a live instance, so it can go; the archive keeps its key.
    drop(c);
    api.close().await;
    sqlx::query("DELETE FROM workflow_definitions WHERE id = $1").bind(f.definition).execute(owner).await.unwrap();
    db.drop().await;
    roles.drop().await;
}
