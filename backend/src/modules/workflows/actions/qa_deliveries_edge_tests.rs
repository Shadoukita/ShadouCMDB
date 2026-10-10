//! QA edge tests (SHAA-2979): retry and discard of workflow action
//! deliveries (slice S3b, SHAA-2831, PR #844) through the real router on a
//! scratch database: refusals by right (403, never 500), by id (404, 400),
//! a delivery in flight, a discarded, delivered or skipped one, a worker
//! that reports after its lease was taken away, and one audit row per change
//! and none per refusal.

use std::time::Duration;

use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::WorkflowActionKind;
use super::outbox::{self, Outcome};
use crate::config::WorkflowActionsConfig;
use crate::db::scratch;
use crate::modules::api_tokens::tests::{Creds, code};
use crate::modules::workflows::runtime_tests::{DEFS, World, id, world};

fn base(w: &World) -> String {
    format!("{DEFS}/{}/action-deliveries", w.definition)
}

/// A started instance of the world's workflow: (CI, instance).
async fn started(w: &World) -> (Uuid, Uuid) {
    let ci = w.ci(w.server).await;
    let (status, v) = w.start(&w.admin, ci).await;
    assert_eq!(status, 201, "{v}");
    (ci, id(&v["instance"]))
}

/// A fanned-out run of `kind` for action `key`, on the instance's latest event.
async fn run(w: &World, key: &str, kind: &str, instance: Uuid, ci: Uuid) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO workflow_action_runs (event_id, action_key, kind, definition_id, instance_id, ci_id, status)
         SELECT max(id), $1, $2, $3, $4, $5, 'fanned_out' FROM workflow_instance_events WHERE instance_id = $4
         RETURNING id",
    )
    .bind(key)
    .bind(kind)
    .bind(w.definition)
    .bind(instance)
    .bind(ci)
    .fetch_one(&w.pool)
    .await
    .unwrap()
}

async fn add(pool: &PgPool, run: i64, recipient: &str, status: &str, reason: Option<&str>) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO workflow_action_deliveries (run_id, recipient_key, status, status_reason, attempts, completed_at)
         VALUES ($1, $2, $3, $4, CASE WHEN $3 = 'dead' THEN 8 ELSE 0 END,
                 CASE WHEN $3 IN ('dead', 'delivered', 'skipped') THEN now() END)
         RETURNING id",
    )
    .bind(run)
    .bind(recipient)
    .bind(status)
    .bind(reason)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn status_of(pool: &PgPool, d: Uuid) -> (String, Option<String>) {
    sqlx::query_as("SELECT status, status_reason FROM workflow_action_deliveries WHERE id = $1")
        .bind(d)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Retry and discard rows: (entity, actor type, actor id, actor name, new value).
async fn audited(pool: &PgPool) -> Vec<(Uuid, String, Option<String>, Option<String>, Value)> {
    sqlx::query_as(
        "SELECT entity_id, actor_type, actor_id, actor_name, new_value FROM audit_log
         WHERE action IN ('workflow.action_retry', 'workflow.action_discard') ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

/// (status, error code, details[0].code) of a refused POST.
async fn post(w: &World, creds: &Creds, path: &str, body: Option<Value>) -> (u16, String, String) {
    let (status, v) = w.call(creds, "POST", path, body).await;
    (status, code(&v).to_owned(), v["error"]["details"][0]["code"].as_str().unwrap_or("").to_owned())
}

fn cfg() -> WorkflowActionsConfig {
    WorkflowActionsConfig { poll: Duration::from_millis(50), ..WorkflowActionsConfig::default() }
}

/// No `workflows.manage` → 403 on every retry and discard endpoint, an API
/// token → 403 (session only), no session → 401; an unknown workflow or
/// delivery → 404, a delivery of another workflow → 404, a malformed id →
/// 400. Never a 500, nothing changed, nothing audited.
#[tokio::test]
async fn qa_deliveries_refusals_are_403_404_and_400_never_500() {
    let Some(db) = scratch::database("qa_deliveries_refusals").await else { return };
    let w = world(&db).await;
    let (ci, instance) = started(&w).await;
    let mail = run(&w, "mail", "email", instance, ci).await;
    let dead = add(&w.pool, mail, "addr:cab@corp.example", "dead", Some("max_attempts")).await;
    let (editor, _) = w.user("editor", &[w.editors]).await;
    let admin_id: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE username = 'admin'").fetch_one(&w.pool).await.unwrap();
    let builtin: Uuid = sqlx::query_scalar("SELECT id FROM permission_profiles WHERE is_builtin LIMIT 1")
        .fetch_one(&w.pool)
        .await
        .unwrap();
    let token = w.token(admin_id, builtin).await;
    let bulk = json!({ "ids": [dead] });

    for (op, body) in [
        (format!("{}/{dead}/retry", base(&w)), None),
        (format!("{}/{dead}/discard", base(&w)), None),
        (format!("{}/retry", base(&w)), Some(bulk.clone())),
        (format!("{}/discard", base(&w)), Some(bulk.clone())),
    ] {
        let (status, c, _) = post(&w, &editor, &op, body.clone()).await;
        assert_eq!((status, c.as_str()), (403, "FORBIDDEN"), "editor {op}");
        let (status, c, _) = post(&w, &token, &op, body.clone()).await;
        assert_eq!(status, 403, "token {op}: {c}");
        let (status, _, _) = post(&w, &Creds::default(), &op, body.clone()).await;
        assert_eq!(status, 401, "anonymous {op}");
    }

    let other = Uuid::new_v4();
    for path in [
        format!("{DEFS}/{other}/action-deliveries/{dead}/retry"),
        format!("{DEFS}/{other}/action-deliveries/{dead}/discard"),
        format!("{}/{other}/retry", base(&w)),
        format!("{}/{other}/discard", base(&w)),
    ] {
        let (status, c, _) = post(&w, &w.admin, &path, None).await;
        assert_eq!((status, c.as_str()), (404, "NOT_FOUND"), "{path}");
    }
    let (status, c, _) = post(&w, &w.admin, &format!("{DEFS}/{other}/action-deliveries/retry"), Some(bulk)).await;
    assert_eq!((status, c.as_str()), (404, "NOT_FOUND"), "bulk on an unknown workflow");

    // A delivery of this workflow under another workflow of the same type: 404.
    let body = json!({ "key": "second", "name": "Second", "classId": w.server });
    let second = id(&w.ok("POST", DEFS, body).await);
    let (status, c, _) = post(&w, &w.admin, &format!("{DEFS}/{second}/action-deliveries/{dead}/discard"), None).await;
    assert_eq!((status, c.as_str()), (404, "NOT_FOUND"));

    for path in [format!("{}/not-a-uuid/retry", base(&w)), format!("{DEFS}/nope/action-deliveries/{dead}/discard")] {
        let (status, c, _) = post(&w, &w.admin, &path, None).await;
        assert_eq!((status, c.as_str()), (400, "VALIDATION_ERROR"), "{path}");
    }

    assert_eq!(status_of(&w.pool, dead).await, ("dead".into(), Some("max_attempts".into())), "unchanged");
    assert!(audited(&w.pool).await.is_empty(), "no refusal is audited");
    db.drop().await;
}

/// A delivery a worker claimed (`sending`) is neither discarded nor retried
/// (409, one and in bulk); the worker's outcome is then written. When its
/// lease ran out and housekeeping put it back to `pending`, it can be
/// discarded, and the late report of the first worker no longer changes it.
#[tokio::test]
async fn qa_deliveries_an_in_flight_delivery_is_left_to_its_lease() {
    let Some(db) = scratch::database("qa_deliveries_in_flight").await else { return };
    let w = world(&db).await;
    let (ci, instance) = started(&w).await;
    let mail = run(&w, "mail", "email", instance, ci).await;
    let d = add(&w.pool, mail, "addr:a@corp.example", "pending", None).await;
    let claimed =
        outbox::claim_deliveries(&w.pool, "w1", WorkflowActionKind::Email, Duration::from_secs(30), 10).await.unwrap();
    assert_eq!(claimed.iter().map(|c| c.id).collect::<Vec<_>>(), vec![d]);
    assert_eq!(status_of(&w.pool, d).await.0, "sending");

    let (status, c, reason) = post(&w, &w.admin, &format!("{}/{d}/discard", base(&w)), None).await;
    assert_eq!((status, c.as_str(), reason.as_str()), (409, "CONFLICT", "not_discardable"));
    let (status, c, reason) = post(&w, &w.admin, &format!("{}/{d}/retry", base(&w)), None).await;
    assert_eq!((status, c.as_str(), reason.as_str()), (409, "CONFLICT", "not_retryable"));
    let (status, v) = w.call(&w.admin, "POST", &format!("{}/discard", base(&w)), Some(json!({ "ids": [d] }))).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["changed"], 0);
    assert_eq!(v["refused"], json!([{ "id": d, "reason": "not_discardable", "status": "sending" }]));
    let body = json!({ "filter": { "status": "sending" } });
    let (_, v) = w.call(&w.admin, "POST", &format!("{}/discard", base(&w)), Some(body)).await;
    assert_eq!((v["changed"].as_i64(), v["more"].as_bool()), (Some(0), Some(false)), "{v}");
    assert!(audited(&w.pool).await.is_empty());

    // The worker reports: delivered.
    assert!(outbox::record(&w.pool, &cfg(), &claimed[0], Outcome::Delivered { status_code: Some(250) }).await.unwrap());
    assert_eq!(status_of(&w.pool, d).await, ("delivered".into(), None));

    // A second one whose lease runs out: back to pending, discarded, the late report refused.
    let d2 = add(&w.pool, mail, "addr:b@corp.example", "pending", None).await;
    let claimed =
        outbox::claim_deliveries(&w.pool, "w2", WorkflowActionKind::Email, Duration::from_secs(30), 10).await.unwrap();
    assert_eq!(claimed.len(), 1);
    sqlx::query("UPDATE workflow_action_deliveries SET lease_until = now() - interval '1 second' WHERE id = $1")
        .bind(d2)
        .execute(&w.pool)
        .await
        .unwrap();
    outbox::housekeeping(&w.pool, &cfg()).await.unwrap();
    assert_eq!(status_of(&w.pool, d2).await.0, "pending");
    let (status, v) = w.call(&w.admin, "POST", &format!("{}/{d2}/discard", base(&w)), None).await;
    assert_eq!((status, v["status"].as_str()), (200, Some("discarded")), "{v}");
    let late = outbox::record(&w.pool, &cfg(), &claimed[0], Outcome::Delivered { status_code: Some(250) }).await;
    assert!(!late.unwrap(), "the lease is no longer the current one");
    assert_eq!(status_of(&w.pool, d2).await, ("dead".into(), Some("discarded".into())));
    assert!(
        outbox::claim_deliveries(&w.pool, "w3", WorkflowActionKind::Email, Duration::from_secs(5), 10)
            .await
            .unwrap()
            .is_empty(),
        "a discarded delivery is never claimed"
    );
    assert_eq!(audited(&w.pool).await.len(), 1, "one row: the discard");
    db.drop().await;
}

/// What can no longer change: a discarded delivery is neither retried nor
/// discarded again, nor is a delivered or skipped one (409 with the reason
/// and the status in the message); a bulk request lists them in `refused`.
/// A retry of a held delivery and a discard of a dead one each write exactly
/// one audit row, by the caller, listed in the audit log of the delivery.
#[tokio::test]
async fn qa_deliveries_final_states_stay_final_and_changes_are_audited_once() {
    let Some(db) = scratch::database("qa_deliveries_final").await else { return };
    let w = world(&db).await;
    let (ci, instance) = started(&w).await;
    let mail = run(&w, "mail", "email", instance, ci).await;
    let discarded = add(&w.pool, mail, "addr:a@corp.example", "dead", Some("discarded")).await;
    let delivered = add(&w.pool, mail, "addr:b@corp.example", "delivered", None).await;
    let skipped = add(&w.pool, mail, "addr:c@corp.example", "skipped", Some("inactive")).await;
    let held = add(&w.pool, mail, "addr:d@corp.example", "held", Some("endpoint_suspended")).await;
    let dead = add(&w.pool, mail, "addr:e@corp.example", "dead", Some("max_attempts")).await;

    for (d, status) in [(discarded, "discarded"), (delivered, "delivered"), (skipped, "skipped")] {
        for (op, reason) in [("retry", "not_retryable"), ("discard", "not_discardable")] {
            let (code_, v) = w.call(&w.admin, "POST", &format!("{}/{d}/{op}", base(&w)), None).await;
            assert_eq!(
                (code_, code(&v), v["error"]["details"][0]["code"].as_str()),
                (409, "CONFLICT", Some(reason)),
                "{op} {status}: {v}"
            );
            assert!(v["error"]["message"].as_str().unwrap().contains(status), "{v}");
        }
    }
    let (_, v) =
        w.call(&w.admin, "POST", &format!("{}/retry", base(&w)), Some(json!({ "ids": [discarded, delivered] }))).await;
    let mut refused: Vec<(String, String, String)> = v["refused"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["id"].as_str().unwrap().into(),
                r["reason"].as_str().unwrap().into(),
                r["status"].as_str().unwrap().into(),
            )
        })
        .collect();
    refused.sort();
    let mut expected = vec![
        (discarded.to_string(), "not_retryable".to_owned(), "discarded".to_owned()),
        (delivered.to_string(), "not_retryable".to_owned(), "delivered".to_owned()),
    ];
    expected.sort();
    assert_eq!((v["changed"].as_i64(), refused), (Some(0), expected), "{v}");
    let (_, v) = w
        .call(&w.admin, "POST", &format!("{}/retry", base(&w)), Some(json!({ "filter": { "status": "discarded" } })))
        .await;
    assert_eq!(v["changed"], 0, "a filter selects no discarded delivery to retry: {v}");
    assert_eq!(status_of(&w.pool, discarded).await, ("dead".into(), Some("discarded".into())));
    assert!(audited(&w.pool).await.is_empty(), "no refusal is audited");

    // The changes: one row each, by the caller.
    let (status, v) = w.call(&w.admin, "POST", &format!("{}/{held}/retry", base(&w)), None).await;
    assert_eq!((status, v["status"].as_str()), (200, Some("pending")), "{v}");
    let (status, v) = w.call(&w.admin, "POST", &format!("{}/{dead}/discard", base(&w)), None).await;
    assert_eq!((status, v["status"].as_str()), (200, Some("discarded")), "{v}");
    let rows = audited(&w.pool).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    let admin_id: String =
        sqlx::query_scalar("SELECT id::text FROM users WHERE username = 'admin'").fetch_one(&w.pool).await.unwrap();
    for ((entity, actor_type, actor_id, actor, new), (d, before, after)) in
        rows.iter().zip([(held, "held", "pending"), (dead, "dead", "discarded")])
    {
        assert_eq!(*entity, d);
        assert_eq!(
            (actor_type.as_str(), actor_id.as_deref(), actor.as_deref()),
            ("user", Some(admin_id.as_str()), Some("admin"))
        );
        assert_eq!(
            (new["before"]["status"].as_str(), new["after"]["status"].as_str()),
            (Some(before), Some(after)),
            "{new}"
        );
        assert_eq!((new["ciId"].clone(), new["bulk"].as_bool()), (json!(ci), Some(false)), "{new}");
        assert!(!new.to_string().contains("@corp.example"), "no address audited: {new}");
    }
    // Listed in the audit log of the delivery.
    let (status, h) = w
        .call(
            &w.admin,
            "GET",
            &format!("/api/v1/audit-log?entityType=workflow_action_deliveries&entityId={dead}"),
            None,
        )
        .await;
    assert_eq!(status, 200, "{h}");
    let actions: Vec<&str> = h["data"].as_array().unwrap().iter().map(|e| e["action"].as_str().unwrap()).collect();
    assert_eq!(actions, ["workflow.action_discard"], "{h}");

    // A second discard of the one just discarded: 409, still one row.
    let (status, _, reason) = post(&w, &w.admin, &format!("{}/{dead}/discard", base(&w)), None).await;
    assert_eq!((status, reason.as_str()), (409, "not_discardable"));
    assert_eq!(audited(&w.pool).await.len(), 2);
    db.drop().await;
}
