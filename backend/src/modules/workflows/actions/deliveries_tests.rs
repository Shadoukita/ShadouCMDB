//! The operations API of the action outbox (SHAA-2831, design SHAA-2725
//! slice S3b) through the real router on a scratch database: the list and its
//! filters, the detail, retry and discard (one and in bulk) with their audit
//! rows, a retried delivery sent again by the delivery loop and a discarded
//! one never claimed, the rights, the summary, and the list's first page over
//! 10,000 deliveries.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::WorkflowActionKind;
use super::outbox::{self, Channel, Channels, Outbox, Outcome};
use crate::config::WorkflowActionsConfig;
use crate::db::scratch;
use crate::modules::api_tokens::tests::code;
use crate::modules::workflows::runtime_tests::{DEFS, World, world};

fn cfg() -> WorkflowActionsConfig {
    WorkflowActionsConfig { poll: Duration::from_millis(50), ..WorkflowActionsConfig::default() }
}

fn base(w: &World) -> String {
    format!("{DEFS}/{}/action-deliveries", w.definition)
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
        "INSERT INTO workflow_action_deliveries (run_id, recipient_key, status, status_reason, attempts, last_error,
           last_status_code, completed_at)
         VALUES ($1, $2, $3, $4, CASE WHEN $3 = 'dead' THEN 8 ELSE 0 END,
                 CASE WHEN $3 = 'dead' THEN '451 try later' END, CASE WHEN $3 = 'dead' THEN 451 END,
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

async fn status_of(pool: &PgPool, d: Uuid) -> (String, Option<String>, i16) {
    sqlx::query_as("SELECT status, status_reason, attempts FROM workflow_action_deliveries WHERE id = $1")
        .bind(d)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A user with `workflows.manage` (and `webhooks.manage` when `webhooks`) and
/// these class rights: view, and edit as well when the flag is set.
async fn manager(
    w: &World,
    name: &str,
    classes: &[(Uuid, bool)],
    webhooks: bool,
) -> crate::modules::api_tokens::tests::Creds {
    let profile = w.profile(name, classes).await;
    let mut global = vec!["workflows.manage"];
    if webhooks {
        global.push("webhooks.manage");
    }
    for p in global {
        sqlx::query("INSERT INTO permission_profile_global_permissions (profile_id, permission) VALUES ($1, $2)")
            .bind(profile)
            .bind(p)
            .execute(&w.pool)
            .await
            .unwrap();
    }
    w.user(&format!("{name}_user"), &[profile]).await.0
}

fn ids(v: &Value) -> Vec<String> {
    let mut out: Vec<String> =
        v["data"].as_array().unwrap().iter().map(|d| d["id"].as_str().unwrap().to_owned()).collect();
    out.sort();
    out
}

fn sorted(ids: &[Uuid]) -> Vec<String> {
    let mut out: Vec<String> = ids.iter().map(Uuid::to_string).collect();
    out.sort();
    out
}

/// List, detail, retry, discard and their audit rows, the rights, and a
/// retried delivery sent again by the delivery loop while a discarded one is not.
#[tokio::test]
async fn deliveries_are_listed_retried_discarded_and_audited() {
    let Some(db) = scratch::database("workflow_action_deliveries_api").await else { return };
    let w = world(&db).await;
    let ci = w.ci(w.server).await;
    let (status, v) = w.start(&w.admin, ci).await;
    assert_eq!(status, 201, "{v}");
    let instance: Uuid = v["instance"]["id"].as_str().unwrap().parse().unwrap();

    let mail = run(&w, "mail", "email", instance, ci).await;
    let hook = run(&w, "hook", "webhook", instance, ci).await;
    let inbox = run(&w, "tell", "inbox", instance, ci).await;
    let dead = add(&w.pool, mail, "addr:cab@corp.example", "dead", Some("max_attempts")).await;
    let held = add(&w.pool, mail, "addr:held@corp.example", "held", Some("endpoint_suspended")).await;
    let pending = add(&w.pool, mail, "addr:pending@corp.example", "pending", None).await;
    let delivered = add(&w.pool, mail, "addr:done@corp.example", "delivered", None).await;
    let hook_dead = add(&w.pool, hook, "endpoint:x", "dead", Some("rejected")).await;
    let inbox_dead = add(&w.pool, inbox, "user:x", "dead", Some("restored")).await;
    // Queued two days ago: past WORKFLOW_ACTIONS_MAX_AGE_HOURS, as a dead letter typically is when retried.
    sqlx::query("UPDATE workflow_action_deliveries SET created_at = now() - interval '2 days' WHERE id = $1")
        .bind(dead)
        .execute(&w.pool)
        .await
        .unwrap();

    // The list: everything for the administrator, newest first; filters; the address masked.
    let (status, v) = w.call(&w.admin, "GET", &base(&w), None).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["page"]["total"], 6);
    assert_eq!(v["data"][5]["id"], json!(dead), "oldest last");
    let row = v["data"].as_array().unwrap().iter().find(|d| d["id"] == json!(dead)).unwrap().clone();
    assert_eq!(
        (row["recipient"]["kind"].as_str(), row["recipient"]["address"].as_str(), row["status"].as_str()),
        (Some("address"), Some("c***@corp.example"), Some("dead"))
    );
    assert!(!v.to_string().contains("cab@corp.example"), "no address in clear");
    assert_eq!(
        (row["actionKey"].as_str(), row["ciId"].clone(), row["attempts"].as_i64()),
        (Some("mail"), json!(ci), Some(8))
    );
    assert!(row["ciLabel"].is_string());
    let (_, v) = w.call(&w.admin, "GET", &format!("{}?status=dead", base(&w)), None).await;
    assert_eq!(ids(&v), sorted(&[dead, hook_dead, inbox_dead]));
    let (_, v) = w.call(&w.admin, "GET", &format!("{}?status=dead&kind=email&actionKey=mail", base(&w)), None).await;
    assert_eq!(ids(&v), sorted(&[dead]));
    let (_, v) =
        w.call(&w.admin, "GET", &format!("{}?instanceId={instance}&sort=attempts&limit=2", base(&w)), None).await;
    assert_eq!((v["page"]["total"].as_i64(), v["data"].as_array().unwrap().len()), (Some(6), 2));
    let (status, v) = w.call(&w.admin, "GET", &format!("{}?status=lost", base(&w)), None).await;
    assert_eq!((status, code(&v)), (400, "VALIDATION_ERROR"), "{v}");

    // Rights: without webhooks.manage no webhook delivery; without view on the type, 404.
    let ops = manager(&w, "ops", &[(w.server, true)], false).await;
    let (status, v) = w.call(&ops, "GET", &base(&w), None).await;
    assert_eq!((status, v["page"]["total"].as_i64()), (200, Some(5)), "{v}");
    let (status, _) = w.call(&ops, "GET", &format!("{}/{hook_dead}", base(&w)), None).await;
    assert_eq!(status, 404);
    let (status, _) = w.call(&ops, "POST", &format!("{}/{hook_dead}/retry", base(&w)), None).await;
    assert_eq!(status, 404, "a webhook delivery is not changed without webhooks.manage");
    let hooks = manager(&w, "hooks", &[(w.server, true)], true).await;
    let (_, v) = w.call(&hooks, "GET", &base(&w), None).await;
    assert_eq!(v["page"]["total"], 6);
    let blind = manager(&w, "blind", &[(w.network, true)], true).await;
    let (status, v) = w.call(&blind, "GET", &base(&w), None).await;
    assert_eq!((status, code(&v)), (404, "NOT_FOUND"), "{v}");
    let (status, v) = w.call(&blind, "GET", &format!("{DEFS}/{}/actions/summary", w.definition), None).await;
    assert_eq!(status, 404, "{v}");
    // GH#877: view without edit on the type reads the deliveries but changes none (403), as for
    // every other change to the workflow (GH#667).
    let viewer = manager(&w, "viewer", &[(w.server, false)], true).await;
    let (status, v) = w.call(&viewer, "GET", &base(&w), None).await;
    assert_eq!((status, v["page"]["total"].as_i64()), (200, Some(6)), "{v}");
    let (status, _) = w.call(&viewer, "GET", &format!("{}/{dead}", base(&w)), None).await;
    assert_eq!(status, 200);
    let (status, _) = w.call(&viewer, "GET", &format!("{DEFS}/{}/actions/summary", w.definition), None).await;
    assert_eq!(status, 200);
    for (path, body) in [
        (format!("{}/{dead}/retry", base(&w)), None),
        (format!("{}/{pending}/discard", base(&w)), None),
        (format!("{}/retry", base(&w)), Some(json!({ "ids": [dead, held] }))),
        (format!("{}/discard", base(&w)), Some(json!({ "filter": { "status": "pending" } }))),
    ] {
        let (status, v) = w.call(&viewer, "POST", &path, body).await;
        assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{path}: {v}");
    }
    assert_eq!(status_of(&w.pool, dead).await, ("dead".into(), Some("max_attempts".into()), 8));
    assert_eq!(status_of(&w.pool, pending).await, ("pending".into(), None, 0));
    assert_eq!(status_of(&w.pool, held).await, ("held".into(), Some("endpoint_suspended".into()), 0));

    // Detail: the last error, the run and the event.
    let (status, v) = w.call(&w.admin, "GET", &format!("{}/{dead}", base(&w)), None).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        (v["lastError"].as_str(), v["delivery"]["lastStatusCode"].as_i64(), v["runStatus"].as_str()),
        (Some("451 try later"), Some(451), Some("fanned_out"))
    );
    assert_eq!(v["event"]["kind"], "start");
    let (status, _) = w.call(&w.admin, "GET", &format!("{}/{}", base(&w), Uuid::new_v4()), None).await;
    assert_eq!(status, 404);

    // Refusals: 409 with the reason, nothing changed, nothing audited.
    for (id, op, reason) in [
        (delivered, "retry", "not_retryable"),
        (pending, "retry", "not_retryable"),
        (inbox_dead, "retry", "not_retryable"),
        (delivered, "discard", "not_discardable"),
    ] {
        let (status, v) = w.call(&w.admin, "POST", &format!("{}/{id}/{op}", base(&w)), None).await;
        assert_eq!(
            (status, code(&v), v["error"]["details"][0]["code"].as_str()),
            (409, "CONFLICT", Some(reason)),
            "{v}"
        );
    }
    let audited = |action: &'static str| {
        let pool = w.pool.clone();
        async move {
            sqlx::query_as::<_, (Uuid, String, Option<String>, Value, Value)>(
                "SELECT entity_id, actor_type, actor_name, new_value -> 'before', new_value FROM audit_log
                 WHERE action = $1 AND old_value IS NULL ORDER BY id",
            )
            .bind(action)
            .fetch_all(&pool)
            .await
            .unwrap()
        }
    };
    assert!(audited("workflow.action_retry").await.is_empty());

    // Retry the dead letter: pending, due now, fresh attempts, one audit row by the caller.
    let (status, v) = w.call(&w.admin, "POST", &format!("{}/{dead}/retry", base(&w)), None).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!((v["status"].as_str(), v["attempts"].as_i64()), (Some("pending"), Some(0)));
    assert!(v["nextAttemptAt"].is_string() && v["retriedAt"].is_string());
    assert_eq!(status_of(&w.pool, dead).await, ("pending".into(), None, 0));
    let rows = audited("workflow.action_retry").await;
    assert_eq!(rows.len(), 1);
    let (entity, actor_type, actor, old, new) = &rows[0];
    assert_eq!((entity, actor_type.as_str(), actor.as_deref()), (&dead, "user", Some("admin")));
    assert_eq!(
        (old["status"].as_str(), old["statusReason"].as_str(), old["attempts"].as_i64()),
        (Some("dead"), Some("max_attempts"), Some(8))
    );
    assert_eq!(
        (new["after"]["status"].as_str(), new["ciId"].clone(), new["kind"].as_str()),
        (Some("pending"), json!(ci), Some("email"))
    );
    assert!(!old.to_string().contains("cab@") && !new.to_string().contains("cab@"), "no address audited");
    let (status, v) = w.call(&w.admin, "POST", &format!("{}/{dead}/retry", base(&w)), None).await;
    assert_eq!((status, v["error"]["details"][0]["code"].as_str()), (409, Some("not_retryable")), "pending now");

    // Discard the pending one: discarded, one audit row.
    let (status, v) = w.call(&w.admin, "POST", &format!("{}/{pending}/discard", base(&w)), None).await;
    assert_eq!(
        (status, v["status"].as_str(), v["statusReason"].as_str()),
        (200, Some("discarded"), Some("discarded")),
        "{v}"
    );
    assert_eq!(status_of(&w.pool, pending).await, ("dead".into(), Some("discarded".into()), 0));
    let rows = audited("workflow.action_discard").await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].0, rows[0].3["status"].as_str(), rows[0].4["after"]["status"].as_str()),
        (pending, Some("pending"), Some("discarded"))
    );
    let (_, v) = w.call(&w.admin, "GET", &format!("{}?status=discarded", base(&w)), None).await;
    assert_eq!(ids(&v), sorted(&[pending]));
    let (_, v) = w.call(&w.admin, "GET", &format!("{}?status=dead", base(&w)), None).await;
    assert_eq!(ids(&v), sorted(&[hook_dead, inbox_dead]), "a discarded delivery is not counted as dead");

    // The delivery loop: the retried one is sent (not expired by its age), the discarded one never.
    let sent = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = sent.clone();
    let channel = Channel {
        send: Arc::new(move |c| {
            seen.lock().unwrap().push(c.id);
            Box::pin(async { Outcome::Delivered { status_code: Some(250) } })
        }),
        timeout: Duration::from_secs(5),
    };
    let outbox = Outbox::start(w.pool.clone(), cfg(), Channels::default().with(WorkflowActionKind::Email, channel));
    let started = Instant::now();
    while status_of(&w.pool, dead).await.0 != "delivered" {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the retried delivery was not sent: {:?}",
            status_of(&w.pool, dead).await
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    outbox.stop().await;
    assert_eq!(*sent.lock().unwrap(), vec![dead], "sent once; the discarded and the held ones not");
    assert_eq!(status_of(&w.pool, dead).await, ("delivered".into(), None, 1));
    assert_eq!(status_of(&w.pool, pending).await, ("dead".into(), Some("discarded".into()), 0));
    assert!(
        outbox::claim_deliveries(&w.pool, "w", WorkflowActionKind::Email, Duration::from_secs(5), 100)
            .await
            .unwrap()
            .is_empty(),
        "nothing left to claim"
    );

    // Bulk by ids: the held one retried, the others refused with their reason.
    let missing = Uuid::new_v4();
    let body = json!({ "ids": [held, delivered, missing, held] });
    let (status, v) = w.call(&w.admin, "POST", &format!("{}/retry", base(&w)), Some(body)).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!((v["changed"].as_i64(), v["ids"].clone(), v["more"].as_bool()), (Some(1), json!([held]), Some(false)));
    let mut refused: Vec<(String, String)> = v["refused"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["id"].as_str().unwrap().to_owned(), r["reason"].as_str().unwrap().to_owned()))
        .collect();
    refused.sort();
    let mut expected =
        vec![(delivered.to_string(), "not_retryable".to_owned()), (missing.to_string(), "not_found".to_owned())];
    expected.sort();
    assert_eq!(refused, expected);
    assert_eq!(audited("workflow.action_retry").await.len(), 2);
    assert_eq!(audited("workflow.action_retry").await[1].4["bulk"], true);

    // Bulk by filter: every dead e-mail and webhook delivery the caller sees is discarded.
    let body = json!({ "filter": { "status": "dead" } });
    let (status, v) = w.call(&ops, "POST", &format!("{}/discard", base(&w)), Some(body.clone())).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!((v["changed"].as_i64(), v["ids"].clone()), (Some(1), json!([inbox_dead])), "not the webhook one: {v}");
    let rows = audited("workflow.action_discard").await;
    assert_eq!((rows.len(), rows[1].2.as_deref()), (2, Some("ops_user")), "the caller is the actor");
    let (_, v) = w.call(&w.admin, "POST", &format!("{}/discard", base(&w)), Some(body)).await;
    assert_eq!(v["ids"], json!([hook_dead]));

    // Body validation.
    for body in
        [json!({}), json!({ "ids": [], }), json!({ "ids": [held], "filter": {} }), json!({ "filter": { "nope": 1 } })]
    {
        let (status, v) = w.call(&w.admin, "POST", &format!("{}/retry", base(&w)), Some(body.clone())).await;
        assert_eq!((status, code(&v)), (400, "VALIDATION_ERROR"), "{body}: {v}");
    }
    db.drop().await;
}

/// The summary: per action, counts by status over 24 h and 7 days, the oldest
/// pending delivery, suppressed runs, and the queue's flag and limits.
#[tokio::test]
async fn the_summary_counts_per_action_and_reports_the_queue() {
    let Some(db) = scratch::database("workflow_action_deliveries_summary").await else { return };
    let w = world(&db).await;
    let ci = w.ci(w.server).await;
    let (_, v) = w.start(&w.admin, ci).await;
    let instance: Uuid = v["instance"]["id"].as_str().unwrap().parse().unwrap();
    let version = w.ok("GET", &format!("{DEFS}/{}/actions", w.definition), json!(null)).await["version"].clone();
    w.ok(
        "PUT",
        &format!("{DEFS}/{}/actions", w.definition),
        json!({ "version": version, "actions": [{ "key": "tell", "name": "Tell ops", "kind": "inbox",
            "trigger": "transition", "transition": "approve", "recipients": [{ "source": "user", "user": "admin" }] }] }),
    )
    .await;
    let mail = run(&w, "mail", "email", instance, ci).await;
    let tell = run(&w, "tell", "inbox", instance, ci).await;
    add(&w.pool, tell, "user:a", "delivered", None).await;
    add(&w.pool, tell, "user:b", "skipped", Some("no_view")).await;
    let old = add(&w.pool, mail, "addr:a@corp.example", "pending", None).await;
    add(&w.pool, mail, "addr:b@corp.example", "pending", None).await;
    let week = add(&w.pool, mail, "addr:c@corp.example", "dead", Some("max_attempts")).await;
    add(&w.pool, mail, "addr:d@corp.example", "dead", Some("discarded")).await;
    sqlx::query("UPDATE workflow_action_deliveries SET created_at = now() - interval '3 hours' WHERE id = $1")
        .bind(old)
        .execute(&w.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE workflow_action_deliveries SET created_at = now() - interval '3 days' WHERE id = $1")
        .bind(week)
        .execute(&w.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO workflow_action_runs (event_id, action_key, kind, definition_id, instance_id, ci_id, status,
           status_reason)
         VALUES (999, 'tell', 'inbox', $1, $2, $3, 'suppressed', 'queue_full')",
    )
    .bind(w.definition)
    .bind(instance)
    .bind(ci)
    .execute(&w.pool)
    .await
    .unwrap();
    outbox::housekeeping(&w.pool, &cfg()).await.unwrap();

    let v = w.ok("GET", &format!("{DEFS}/{}/actions/summary", w.definition), json!(null)).await;
    let keys: Vec<&str> = v["actions"].as_array().unwrap().iter().map(|a| a["key"].as_str().unwrap()).collect();
    assert_eq!(keys, ["tell", "mail"], "configured first, then deleted ones with deliveries: {v}");
    let tell = &v["actions"][0];
    assert_eq!(
        (tell["name"].as_str(), tell["enabled"].as_bool(), tell["kind"].as_str()),
        (Some("Tell ops"), Some(true), Some("inbox"))
    );
    assert_eq!(
        (tell["last24h"]["delivered"].as_i64(), tell["last24h"]["skipped"].as_i64(), tell["suppressed24h"].as_i64()),
        (Some(1), Some(1), Some(1))
    );
    let mail = &v["actions"][1];
    assert_eq!((mail["name"].clone(), mail["enabled"].as_bool()), (json!(null), Some(false)));
    assert_eq!(
        (mail["last24h"]["pending"].as_i64(), mail["last24h"]["dead"].as_i64(), mail["last24h"]["discarded"].as_i64()),
        (Some(2), Some(0), Some(1))
    );
    assert_eq!(mail["last7d"]["dead"].as_i64(), Some(1));
    let age = mail["oldestPendingAgeSeconds"].as_i64().unwrap();
    assert!((3 * 3600 - 5..=3 * 3600 + 60).contains(&age), "oldest pending {age} s");
    let q = &v["queue"];
    assert_eq!(
        (q["overloaded"].as_bool(), q["backlog"].as_i64(), q["queueMax"].as_i64(), q["maxAttempts"].as_i64()),
        (Some(false), Some(2), Some(100_000), Some(8))
    );
    db.drop().await;
}

/// The first page of a status filter over 10,000 deliveries, through the
/// router: under 200 ms at the 95th percentile (acceptance criterion).
#[tokio::test]
async fn perf_the_first_page_of_10k_deliveries_is_under_200_ms() {
    let Some(db) = scratch::database("workflow_action_deliveries_perf").await else { return };
    let w = world(&db).await;
    let ci = w.ci(w.server).await;
    let (_, v) = w.start(&w.admin, ci).await;
    let instance: Uuid = v["instance"]["id"].as_str().unwrap().parse().unwrap();
    // 1,000 runs of 10 deliveries each, a tenth of them dead.
    sqlx::query(
        "INSERT INTO workflow_action_runs (event_id, action_key, kind, definition_id, instance_id, ci_id, status, created_at)
         SELECT g, 'mail', 'email', $1, $2, $3, 'fanned_out', now() - g * interval '1 minute' FROM generate_series(1, 1000) g",
    )
    .bind(w.definition)
    .bind(instance)
    .bind(ci)
    .execute(&w.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workflow_action_deliveries (run_id, recipient_key, status, status_reason, attempts, completed_at, created_at)
         SELECT r.id, 'addr:u' || n || '@corp.example', CASE WHEN n = 1 THEN 'dead' ELSE 'delivered' END,
                CASE WHEN n = 1 THEN 'max_attempts' END, 1, now(), r.created_at
         FROM workflow_action_runs r, generate_series(1, 10) n",
    )
    .execute(&w.pool)
    .await
    .unwrap();
    for t in ["workflow_action_deliveries", "workflow_action_runs"] {
        sqlx::query(sqlx::AssertSqlSafe(format!("ANALYZE {t}"))).execute(&w.pool).await.unwrap();
    }
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM workflow_action_deliveries").fetch_one(&w.pool).await.unwrap();
    assert_eq!(n, 10_000);

    let mut times = Vec::new();
    for i in 0..40 {
        let status = if i % 2 == 0 { "dead" } else { "delivered" };
        let started = Instant::now();
        let (code, v) = w.call(&w.admin, "GET", &format!("{}?status={status}&limit=50", base(&w)), None).await;
        times.push(started.elapsed());
        assert_eq!(code, 200, "{v}");
        assert_eq!(v["data"].as_array().unwrap().len(), 50);
    }
    times.sort();
    let p95 = times[(times.len() * 95).div_ceil(100) - 1];
    println!("first page of 10k deliveries by status: p50 {:?}, p95 {p95:?}", times[times.len() / 2]);
    assert!(p95 < Duration::from_millis(200), "p95 {p95:?}");
    db.drop().await;
}
