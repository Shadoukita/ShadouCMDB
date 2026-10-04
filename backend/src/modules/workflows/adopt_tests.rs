//! Slice S3b (SHAA-1698) through the real router against PostgreSQL: the
//! workflow-driven state field refused on CI create and PATCH (and not while
//! the workflow is inactive), bootstrap (dry run, real run across a batch
//! boundary, unmapped and terminal values, idempotency, the audit chain), and
//! auto-start on CI create. The import paths are in `imports::tests`.

use serde_json::{Value, json};
use uuid::Uuid;

use super::adopt::BATCH;
use super::runtime_tests::{World, world};
use crate::db::scratch;
use crate::modules::api_tokens::tests::code;
use crate::schema::model::Model;

const DEFS: &str = "/api/v1/admin/workflow-definitions";
const CIS: &str = "/api/v1/configuration-items";

fn id(v: &Value) -> Uuid {
    v["id"].as_str().unwrap_or_else(|| panic!("no id in {v}")).parse().unwrap()
}

fn first_detail(v: &Value) -> (&str, &str) {
    let d = &v["error"]["details"][0];
    (d["field"].as_str().unwrap_or_default(), d["code"].as_str().unwrap_or_default())
}

impl World {
    async fn set_active(&self, active: bool, auto_start: bool) {
        let d = self.ok("GET", &format!("{DEFS}/{}", self.definition), json!(null)).await;
        self.ok(
            "PATCH",
            &format!("{DEFS}/{}", self.definition),
            json!({ "version": d["version"], "isActive": active, "autoStart": auto_start }),
        )
        .await;
    }

    async fn create(&self, lifecycle: Option<&str>) -> (u16, Value) {
        let mut attributes = json!({ "environment": "test" });
        if let Some(k) = lifecycle {
            attributes["lifecycle"] = json!(self.value(k).to_string());
        }
        self.call(&self.admin, "POST", CIS, Some(json!({ "classId": self.server, "attributes": attributes }))).await
    }

    async fn lifecycle_of(&self, ci: Uuid) -> Value {
        let (status, v) = self.call(&self.admin, "GET", &format!("{CIS}/{ci}"), None).await;
        assert_eq!(status, 200, "{v}");
        v["attributes"]["lifecycle"].clone()
    }

    async fn bootstrap(&self, dry_run: bool) -> Value {
        self.ok(
            "POST",
            &format!("{DEFS}/{}/bootstrap", self.definition),
            json!({ "stateFromAttribute": true, "dryRun": dry_run }),
        )
        .await
    }

    async fn running(&self) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM workflow_instances WHERE definition_id = $1 AND status = 'active'")
            .bind(self.definition)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }
}

/// Q3: direct writes of the driven field are refused while the workflow is
/// active, whether or not the CI has an instance; the workflow still writes it.
#[tokio::test]
async fn a_driven_state_field_is_refused_on_create_and_patch_while_the_workflow_is_active() {
    let Some(db) = scratch::database("workflow_controlled_field").await else { return };
    let w = world(&db).await;
    let patch = |ci: Uuid, attributes: Value| {
        let w = &w;
        async move { w.call(&w.admin, "PATCH", &format!("{CIS}/{ci}"), Some(json!({ "attributes": attributes }))).await }
    };

    // Create: another value is refused; none, or the value the workflow starts with, is accepted.
    let (status, v) = w.create(Some("approved")).await;
    assert_eq!((status, code(&v)), (409, "WORKFLOW_CONTROLLED_FIELD"), "{v}");
    assert_eq!(first_detail(&v), ("attributes.lifecycle", "workflow_controlled"));
    let (status, v) = w.create(Some("planned")).await;
    assert_eq!(status, 201, "{v}");
    let (status, v) = w.create(None).await;
    assert_eq!(status, 201, "{v}");
    let ci = id(&v);

    // PATCH, without an instance: setting it is refused, as is clearing a set value.
    let approved = json!(w.value("approved").to_string());
    let (status, v) = patch(ci, json!({ "lifecycle": approved })).await;
    assert_eq!((status, code(&v)), (409, "WORKFLOW_CONTROLLED_FIELD"), "{v}");
    assert_eq!(first_detail(&v), ("attributes.lifecycle", "workflow_controlled"));
    // Other fields stay editable, and clearing an empty value is no change.
    let (status, v) = patch(ci, json!({ "owner_team": "ops", "lifecycle": null })).await;
    assert_eq!(status, 200, "{v}");

    // With an instance: the workflow writes the field; resending its value is no change, another is refused.
    let (status, v) = w.start(&w.admin, ci).await;
    assert_eq!(status, 201, "{v}");
    let planned = json!(w.value("planned").to_string());
    assert_eq!(w.lifecycle_of(ci).await, planned);
    let (status, v) = patch(ci, json!({ "lifecycle": planned, "owner_team": "net" })).await;
    assert_eq!(status, 200, "{v}");
    let (status, v) = patch(ci, json!({ "lifecycle": approved })).await;
    assert_eq!((status, code(&v)), (409, "WORKFLOW_CONTROLLED_FIELD"), "{v}");
    let (status, v) = patch(ci, json!({ "lifecycle": null })).await;
    assert_eq!((status, code(&v)), (409, "WORKFLOW_CONTROLLED_FIELD"), "{v}");
    assert_eq!(w.lifecycle_of(ci).await, planned, "a refused write changes nothing");
    let instance: Uuid = sqlx::query_scalar("SELECT id FROM workflow_instances WHERE ci_id = $1")
        .bind(ci)
        .fetch_one(&w.pool)
        .await
        .unwrap();
    let (status, v) = w
        .call(
            &w.admin,
            "POST",
            &format!("/api/v1/workflow-instances/{instance}/force"),
            Some(json!({ "expectedVersion": 1, "stateKey": "approved", "reason": "repair" })),
        )
        .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(w.lifecycle_of(ci).await, approved, "the workflow's own write passes");

    // Inactive: the field is an ordinary field again.
    w.set_active(false, false).await;
    let (status, v) = patch(ci, json!({ "lifecycle": planned })).await;
    assert_eq!(status, 200, "{v}");
    let (status, v) = w.create(Some("approved")).await;
    assert_eq!(status, 201, "{v}");
    db.drop().await;
}

/// Bootstrap (§8.2, Amendment 2): dry run, a real run over more than one
/// batch, unmapped and terminal values, idempotency, audit and events.
#[tokio::test]
async fn bootstrap_starts_the_covered_cis_in_the_state_of_their_value() {
    let Some(db) = scratch::database("workflow_bootstrap").await else { return };
    let w = world(&db).await;
    // Values the CIs take while the workflow is inactive; `retired` is a value no state maps.
    w.set_active(false, false).await;
    let list: Uuid = sqlx::query_scalar("SELECT list_id FROM lookup_list_values WHERE id = $1")
        .bind(w.value("planned"))
        .fetch_one(&w.pool)
        .await
        .unwrap();
    let retired = id(&w
        .ok("POST", "/api/v1/lookup-list-values", json!({ "listId": list, "key": "retired", "name": "Retired" }))
        .await);

    // BATCH + 3 planned servers straight into the tables (crossing a batch boundary), plus a few through the API.
    let model = Model::load(&mut w.pool.acquire().await.unwrap()).await.unwrap();
    let table = model.table(w.server).unwrap().sql();
    let column = model.own_fields(w.server).find(|f| f.key == "lifecycle").unwrap().column().to_string();
    let bulk = BATCH + 3;
    sqlx::query(
        "INSERT INTO configuration_items (id, class_id, ident, label, valid_from)
         SELECT ('00000000-0000-4000-8000-' || lpad(n::text, 12, '0'))::uuid, $1, 'BULK-' || n, 'bulk ' || n, now()
         FROM generate_series(1, $2) n",
    )
    .bind(w.server)
    .bind(bulk)
    .execute(&w.pool)
    .await
    .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO {table} (id, {column}) SELECT id, $1 FROM configuration_items WHERE ident LIKE 'BULK-%'"
    )))
    .bind(w.value("planned"))
    .execute(&w.pool)
    .await
    .unwrap();
    let mut cis = Vec::new();
    for value in ["approved", "approved", "live"] {
        let (status, v) = w.create(Some(value)).await;
        assert_eq!(status, 201, "{v}");
        cis.push(id(&v));
    }
    let (_, v) = w.create(None).await;
    let no_value = id(&v);
    let (_, v) = w
        .call(
            &w.admin,
            "POST",
            CIS,
            Some(json!({ "classId": w.server, "attributes": { "environment": "test", "lifecycle": retired } })),
        )
        .await;
    let retired_ci = id(&v);
    // One already running (left alone), one deleted (skipped), one of a type the workflow does not cover.
    w.set_active(true, false).await;
    let (status, v) = w.start(&w.admin, cis[0]).await;
    assert_eq!(status, 201, "{v}");
    let (_, v) = w.create(Some("planned")).await;
    let deleted = id(&v);
    let (status, _) = w.call(&w.admin, "DELETE", &format!("{CIS}/{deleted}"), None).await;
    assert_eq!(status, 204);
    w.ci(w.network).await;
    assert_eq!(w.running().await, 1);

    // Dry run: counts, writes nothing.
    let dry = w.bootstrap(true).await;
    let expected_states = json!([
        { "stateKey": "planned", "stateName": "Planned", "valueKey": "planned", "terminal": false, "count": bulk },
        { "stateKey": "approved", "stateName": "Approved", "valueKey": "approved", "terminal": false, "count": 1 },
        { "stateKey": "done", "stateName": "In production", "valueKey": "live", "terminal": true, "count": 1 }
    ]);
    assert_eq!(dry["dryRun"], true);
    assert_eq!(dry["states"], expected_states, "{dry}");
    assert_eq!(
        (dry["started"].as_i64(), dry["alreadyRunning"].as_i64(), dry["skippedTerminal"].as_i64()),
        (Some(bulk + 1), Some(1), Some(1)),
        "{dry}"
    );
    let unmapped: Vec<(Option<&str>, i64)> = dry["unmapped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| (u["valueKey"].as_str(), u["count"].as_i64().unwrap()))
        .collect();
    assert_eq!(unmapped.len(), 2, "{dry}");
    assert!(unmapped.contains(&(None, 1)) && unmapped.contains(&(Some("retired"), 1)), "{dry}");
    assert_eq!(dry["skippedUnmapped"], 2);
    assert_eq!(w.running().await, 1, "a dry run writes nothing");

    // The run: the same counts, the instances, one start event and one audit row each.
    let audit_before: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log").fetch_one(&w.pool).await.unwrap();
    let run = w.bootstrap(false).await;
    assert_eq!(run["dryRun"], false);
    for k in ["states", "started", "alreadyRunning", "skippedTerminal", "unmapped", "skippedUnmapped"] {
        if k != "unmapped" {
            assert_eq!(run[k], dry[k], "{k}: {run}");
        }
    }
    assert_eq!(w.running().await, 1 + bulk + 1);
    let started: Vec<(String, String)> = sqlx::query_as(
        "SELECT s.key, e.actor_type FROM workflow_instances wi
         JOIN workflow_states s ON s.id = wi.current_state_id
         JOIN workflow_instance_events e ON e.instance_id = wi.id
         WHERE wi.ci_id = $1",
    )
    .bind(cis[1])
    .fetch_all(&w.pool)
    .await
    .unwrap();
    assert_eq!(started, [("approved".to_owned(), "system".to_owned())]);
    for skipped in [cis[2], no_value, retired_ci, deleted] {
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_instances WHERE ci_id = $1")
            .bind(skipped)
            .fetch_one(&w.pool)
            .await
            .unwrap();
        assert_eq!(n, 0, "{skipped} is skipped");
    }
    let rows: Vec<(String, String, Value)> = sqlx::query_as(
        "SELECT action, actor_type, new_value FROM audit_log WHERE action = 'workflow.start' AND actor_type = 'system'",
    )
    .fetch_all(&w.pool)
    .await
    .unwrap();
    assert_eq!(rows.len() as i64, bulk + 1);
    assert_eq!((rows[0].2["bootstrap"].as_bool(), rows[0].2["requestedBy"].as_str()), (Some(true), Some("admin")));
    let audit_after: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log").fetch_one(&w.pool).await.unwrap();
    assert_eq!(audit_after - audit_before, bulk + 1, "one audit row per instance, nothing else");
    // A bootstrapped instance runs like any other.
    let instance: Uuid = sqlx::query_scalar("SELECT id FROM workflow_instances WHERE ci_id = $1")
        .bind(cis[1])
        .fetch_one(&w.pool)
        .await
        .unwrap();
    let (status, v) =
        w.transition(&w.admin, instance, json!({ "transitionKey": "go_live", "expectedVersion": 1 })).await;
    assert_eq!(status, 200, "{v}");

    // Idempotent: a second run starts nothing.
    let again = w.bootstrap(false).await;
    assert_eq!((again["started"].as_i64(), again["alreadyRunning"].as_i64()), (Some(0), Some(bulk + 1)), "{again}");
    assert_eq!(again["skippedUnmapped"], 2);
    assert_eq!(w.running().await, 1 + bulk);

    let problems: Vec<(i64, String)> =
        sqlx::query_as("SELECT chain_seq, problem FROM audit_log_verify()").fetch_all(&w.pool).await.unwrap();
    assert!(problems.is_empty(), "audit chain after a bootstrap: {problems:?}");

    // Refusals.
    let path = format!("{DEFS}/{}/bootstrap", w.definition);
    let (status, v) = w.call(&w.admin, "POST", &path, Some(json!({ "stateFromAttribute": false }))).await;
    assert_eq!((status, first_detail(&v)), (400, ("stateFromAttribute", "invalid_value")), "{v}");
    w.set_active(false, false).await;
    let (status, v) = w.call(&w.admin, "POST", &path, Some(json!({ "stateFromAttribute": true }))).await;
    assert_eq!((status, first_detail(&v)), (409, ("id", "inactive")), "{v}");
    let missing = format!("{DEFS}/{}/bootstrap", Uuid::new_v4());
    let (status, v) = w.call(&w.admin, "POST", &missing, Some(json!({ "stateFromAttribute": true }))).await;
    assert_eq!((status, code(&v)), (404, "NOT_FOUND"), "{v}");
    db.drop().await;
}

/// Auto-start on a CI created through the API: instance, state field and audit in the create's transaction.
#[tokio::test]
async fn an_auto_start_workflow_starts_on_a_new_ci() {
    let Some(db) = scratch::database("workflow_auto_start_create").await else { return };
    let w = world(&db).await;
    w.set_active(true, true).await;
    let (status, v) = w.create(None).await;
    assert_eq!(status, 201, "{v}");
    let ci = id(&v);
    assert_eq!(v["attributes"]["lifecycle"], json!(w.value("planned").to_string()), "{v}");
    let (status, v) = w.call(&w.admin, "GET", &format!("{CIS}/{ci}/workflows"), None).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["controlledFields"], json!(["lifecycle"]), "{v}");
    let running = v["data"].as_array().unwrap_or_else(|| panic!("{v}"));
    assert_eq!((running.len(), running[0]["instance"]["state"]["key"].as_str()), (1, Some("planned")), "{v}");
    let audit: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT action, request_id FROM audit_log WHERE entity_type = 'configuration_items' AND entity_id = $1
         ORDER BY id",
    )
    .bind(ci)
    .fetch_all(&w.pool)
    .await
    .unwrap();
    let actions: Vec<&str> = audit.iter().map(|(a, _)| a.as_str()).collect();
    assert_eq!(actions, ["create", "workflow.start"], "the state is set by the create itself");
    assert_eq!(audit[0].1, audit[1].1, "one request");

    // A CI of a type it does not cover starts nothing; nor does a new CI once auto-start is off.
    let other = w.ci(w.network).await;
    let (_, v) = w.call(&w.admin, "GET", &format!("{CIS}/{other}/workflows"), None).await;
    assert_eq!(v["controlledFields"], json!([]), "{v}");
    w.set_active(true, false).await;
    let (_, v) = w.create(None).await;
    for ci in [other, id(&v)] {
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_instances WHERE ci_id = $1")
            .bind(ci)
            .fetch_one(&w.pool)
            .await
            .unwrap();
        assert_eq!(n, 0);
    }
    db.drop().await;
}
