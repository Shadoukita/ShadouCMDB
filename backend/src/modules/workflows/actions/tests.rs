//! Notification actions and their outbox (SHAA-2733, design SHAA-2725 slice
//! S3) on a scratch database: the API (validation, audit, lint, preview), the
//! inbox fan-out through a real transition, exactly-once delivery with two
//! worker pools, a worker lost mid-fan-out, backoff and the dead letter, and
//! the queue and loop limits of the enqueue trigger.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::WorkflowActionKind;
use super::outbox::{self, Channel, Channels, Claimed, FanOut, Outbox, Outcome};
use crate::config::WorkflowActionsConfig;
use crate::db::scratch;
use crate::db::upgrade_0046::{Fixture, id, ok, workflow_fixture};
use crate::modules::api_tokens::tests::{Creds, code};
use crate::modules::workflows::runtime_tests::{DEFS, RUN, World, details, world};

fn cfg() -> WorkflowActionsConfig {
    WorkflowActionsConfig { poll: Duration::from_millis(100), ..WorkflowActionsConfig::default() }
}

async fn count(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

fn actions(w: &World) -> String {
    format!("{DEFS}/{}/actions", w.definition)
}

async fn version(w: &World) -> i64 {
    w.ok("GET", &actions(w), json!(null)).await["version"].as_i64().unwrap()
}

fn inbox(key: &str, trigger: &str, transition: Option<&str>, recipients: Value) -> Value {
    json!({ "key": key, "name": key, "kind": "inbox", "trigger": trigger, "transition": transition,
        "recipients": recipients })
}

/// Fans out every pending run in this task, as one worker would.
async fn drain(pool: &PgPool, cfg: &WorkflowActionsConfig) -> usize {
    let mut n = 0;
    loop {
        let ids = outbox::claim_runs(pool, "test-worker", 100).await.unwrap();
        if ids.is_empty() {
            return n;
        }
        for id in ids {
            assert_ne!(outbox::fan_out(pool, cfg, id, "test-worker").await.unwrap(), FanOut::Lost);
            n += 1;
        }
    }
}

/// The API: what is refused and why, a save that is audited, the lint and
/// the preview.
#[tokio::test]
async fn actions_are_validated_saved_audited_and_previewed() {
    let Some(db) = scratch::database("workflow_actions_api").await else { return };
    let w = world(&db).await;
    let ops = w.profile("Ops", &[(w.server, false)]).await;
    let blind = w.profile("Blind", &[(w.network, false)]).await;
    let (_, o1) = w.user("o1", &[ops]).await;
    let (_, b1) = w.user("b1", &[blind]).await;

    let v = w.ok("GET", &actions(&w), json!(null)).await;
    assert_eq!((v["actions"].clone(), v["problems"].clone()), (json!([]), json!([])));
    let version = v["version"].as_i64().unwrap();

    // Refusals.
    let put = |actions: Value| json!({ "version": version, "actions": actions });
    let refused = [
        (
            json!([{ "key": "mail", "name": "Mail", "kind": "email", "trigger": "transition", "transition": "approve",
                     "recipients": [{ "source": "profile", "profile": "Ops" }] }]),
            vec![("actions[0].kind", "kind_unavailable")],
        ),
        (
            json!([inbox("a", "transition", Some("approve"), json!([{ "source": "ci_owner" }]))]),
            vec![("actions[0].recipients[0].source", "source_unavailable")],
        ),
        (
            json!([inbox("a", "transition", Some("nope"), json!([{ "source": "user", "user": "o1" }]))]),
            vec![("actions[0].transition", "unknown_transition")],
        ),
        (
            json!([inbox("a", "instance_cancelled", Some("approve"), json!([{ "source": "user", "user": "o1" }]))]),
            vec![("actions[0].transition", "not_applicable")],
        ),
        (
            json!([inbox("a", "transition", Some("approve"), json!([{ "source": "user", "group": "o1" }]))]),
            vec![("actions[0].recipients[0].group", "source_mismatch"), ("actions[0].recipients[0].user", "required")],
        ),
        (
            json!([inbox("a", "transition", Some("approve"), json!([{ "source": "user", "user": "nobody" }]))]),
            vec![("actions[0].recipients[0].user", "not_found")],
        ),
        (json!([inbox("a", "transition", Some("approve"), json!([])),]), vec![("actions[0].recipients", "required")]),
        (
            json!([
                inbox("a", "transition", Some("approve"), json!([{ "source": "user", "user": "o1" }])),
                inbox("a", "transition", Some("go_live"), json!([{ "source": "user", "user": "o1" }]))
            ]),
            vec![("actions[1].key", "duplicate")],
        ),
    ];
    for (body, expected) in refused {
        let (status, v) = w.call(&w.admin, "PUT", &actions(&w), Some(put(body.clone()))).await;
        let mut got = details(&v);
        got.sort();
        let expected: Vec<(String, String)> =
            expected.iter().map(|(f, c)| ((*f).to_owned(), (*c).to_owned())).collect();
        assert_eq!((status, got), (400, expected), "{body}: {v}");
    }
    let eleven: Vec<Value> = (0..11)
        .map(|i| inbox(&format!("a{i}"), "transition", Some("approve"), json!([{ "source": "user", "user": "o1" }])))
        .collect();
    let (status, v) = w.call(&w.admin, "PUT", &actions(&w), Some(put(json!(eleven)))).await;
    assert_eq!((status, details(&v)), (400, vec![("actions[10]".to_owned(), "too_many_actions".to_owned())]), "{v}");
    let (status, v) = w.call(&w.admin, "PUT", &actions(&w), Some(json!({ "version": 999, "actions": [] }))).await;
    assert_eq!((status, code(&v)), (409, "VERSION_CONFLICT"), "{v}");
    assert_eq!(count(&w.pool, "SELECT count(*) FROM workflow_actions").await, 0, "nothing saved");

    // A valid set: saved in order, the version bumped, one audit row by name.
    let body = put(json!([
        inbox(
            "tell_ops",
            "transition",
            Some("approve"),
            json!([{ "source": "profile", "profile": "ops" },
            { "source": "user", "user": "b1" }])
        ),
        inbox("on_cancel", "instance_cancelled", None, json!([{ "source": "profile", "profile": "Blind" }])),
    ]));
    let v = w.ok("PUT", &actions(&w), body.clone()).await;
    assert_eq!(v["version"].as_i64(), Some(version + 1));
    let keys: Vec<&str> = v["actions"].as_array().unwrap().iter().map(|a| a["key"].as_str().unwrap()).collect();
    assert_eq!(keys, ["tell_ops", "on_cancel"]);
    assert_eq!(v["actions"][0]["recipients"][0]["profile"]["name"], "Ops");
    assert_eq!(v["actions"][0]["recipients"][1]["user"]["id"], json!(b1));
    let tell_ops = v["actions"][0]["id"].clone();
    let problems: Vec<(&str, &str)> = v["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["path"].as_str().unwrap(), p["code"].as_str().unwrap()))
        .collect();
    assert_eq!(
        problems,
        [
            ("actions[0].recipients[1]", "recipients_cannot_view"),
            ("actions[1].recipients[0]", "recipients_cannot_view")
        ]
    );
    let audit: Vec<(Value, Value)> = sqlx::query_as(
        "SELECT old_value, new_value FROM audit_log WHERE entity_type = 'workflow_definitions' AND entity_id = $1
         AND action = 'update' AND new_value ? 'actions'",
    )
    .bind(w.definition)
    .fetch_all(&w.pool)
    .await
    .unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].0["actions"], json!([]));
    assert_eq!(audit[0].1["actions"][0]["recipients"], json!([{ "profile": "Ops" }, { "user": "b1" }]));

    // The same set again changes nothing; a rename keeps the action's id.
    let body = json!({ "version": version + 1, "actions": body["actions"] });
    let v = w.ok("PUT", &actions(&w), body.clone()).await;
    assert_eq!(v["version"].as_i64(), Some(version + 1), "unchanged: no new version");
    let mut renamed = body.clone();
    renamed["actions"][0]["name"] = json!("Tell Ops");
    let v = w.ok("PUT", &actions(&w), renamed).await;
    assert_eq!((v["actions"][0]["id"].clone(), v["actions"][0]["name"].clone()), (tell_ops, json!("Tell Ops")));

    // Moving actions between triggers at the limit: parked first, so the database's own limit never trips.
    let mut ten: Vec<Value> = (0..10)
        .map(|i| inbox(&format!("a{i}"), "transition", Some("approve"), json!([{ "source": "user", "user": "o1" }])))
        .collect();
    let version = v["version"].as_i64().unwrap();
    let v = w.ok("PUT", &actions(&w), json!({ "version": version, "actions": ten })).await;
    for (i, a) in ten.iter_mut().enumerate() {
        a["transition"] = json!(if i < 5 { "go_live" } else { "approve" });
    }
    ten.push(inbox("x", "transition", Some("go_live"), json!([{ "source": "user", "user": "o1" }])));
    w.ok("PUT", &actions(&w), json!({ "version": v["version"], "actions": ten })).await;
    assert_eq!(count(&w.pool, "SELECT count(*) FROM workflow_actions").await, 11);

    // Preview: who is in, who is out and why.
    let version = self::version(&w).await;
    w.ok(
        "PUT",
        &actions(&w),
        json!({ "version": version, "actions": [inbox("tell_ops", "transition", Some("approve"),
            json!([{ "source": "profile", "profile": "Ops" }, { "source": "user", "user": "b1" }]))] }),
    )
    .await;
    let ci = w.ci(w.server).await;
    let v = w.ok("GET", &format!("{}/tell_ops/preview?ciId={ci}", actions(&w)), json!(null)).await;
    let users: Vec<(&str, &str)> = v["users"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| (u["username"].as_str().unwrap(), u["reason"].as_str().unwrap()))
        .collect();
    assert_eq!(users, [("o1", "included"), ("b1", "no_view")]);
    assert_eq!((v["included"].as_i64(), v["excludesActor"].as_bool()), (Some(1), Some(true)));
    assert_eq!(v["users"][0]["sources"], json!(["profile Ops"]));
    let (status, _) = w.call(&w.admin, "GET", &format!("{}/nope/preview", actions(&w)), None).await;
    assert_eq!(status, 404);
    let _ = o1;
    db.drop().await;
}

/// A workflow with only a draft (no current version) reads and saves its
/// actions: the draft's transitions are known, and the lint says no current
/// version has them.
#[tokio::test]
async fn a_workflow_with_only_a_draft_has_actions_too() {
    let Some(db) = scratch::database("workflow_actions_draft_only").await else { return };
    let w = world(&db).await;
    w.profile("Ops", &[(w.server, false)]).await;
    let d = w.ok("POST", DEFS, json!({ "key": "review", "name": "Review", "classId": w.server })).await;
    let d = d["id"].as_str().unwrap();
    let graph = json!({ "initialState": "a", "states": [
            { "key": "a", "name": "A", "category": "open" },
            { "key": "b", "name": "B", "category": "done", "terminal": true } ],
        "transitions": [ { "key": "end", "name": "End", "from": "a", "to": "b" } ] });
    w.ok("PUT", &format!("{DEFS}/{d}/draft"), graph).await;
    let url = format!("{DEFS}/{d}/actions");

    let v = w.ok("GET", &url, json!(null)).await;
    assert_eq!((v["actions"].clone(), v["problems"].clone()), (json!([]), json!([])), "{v}");
    let body = json!({ "version": v["version"], "actions": [inbox("tell_ops", "transition", Some("end"),
        json!([{ "source": "profile", "profile": "Ops" }]))] });
    let v = w.ok("PUT", &url, body).await;
    let codes: Vec<&str> = v["problems"].as_array().unwrap().iter().map(|p| p["code"].as_str().unwrap()).collect();
    // Ops has no user yet, so the lint warns about that too.
    assert_eq!(codes, ["unknown_transition", "recipients_cannot_view"], "{v}");
    db.drop().await;
}

/// A transition with an inbox action: after commit, one fan-out notifies the
/// recipients who may view the CI once; the others are skipped with the
/// reason and get no row; a refused transition queues nothing.
#[tokio::test]
async fn an_inbox_action_reaches_viewers_only_and_a_refused_transition_queues_nothing() {
    let Some(db) = scratch::database("workflow_actions_inbox").await else { return };
    let w = world(&db).await;
    let ops = w.profile("Ops", &[(w.server, false)]).await;
    let blind = w.profile("Blind", &[(w.network, false)]).await;
    let (approver, approver_id) = w.user("approver", &[w.approvers]).await;
    let (_, o1) = w.user("o1", &[ops]).await;
    let (_, b1) = w.user("b1", &[blind]).await;
    let version = version(&w).await;
    w.ok(
        "PUT",
        &actions(&w),
        json!({ "version": version, "actions": [inbox("tell_ops", "transition", Some("approve"),
            json!([{ "source": "profile", "profile": "Ops" }, { "source": "user", "user": "b1" },
                   { "source": "user", "user": "approver" }]))] }),
    )
    .await;
    let ci = w.ci(w.server).await;
    let (status, v) = w.start(&w.admin, ci).await;
    assert_eq!(status, 201, "{v}");
    let instance: Uuid = v["instance"]["id"].as_str().unwrap().parse().unwrap();

    // Refused (conditions): rolled back, no run.
    let run = |version: i64, fields: Value| {
        json!({ "transitionKey": "approve", "expectedVersion": version,
        "fields": fields, "comment": "ok" })
    };
    let (status, v) = w.transition(&approver, instance, run(1, json!({}))).await;
    assert_eq!(status, 422, "{v}");
    assert_eq!(count(&w.pool, "SELECT count(*) FROM workflow_action_runs").await, 0);

    w.ok("PATCH", &format!("/api/v1/configuration-items/{ci}"), json!({ "attributes": { "environment": "prod" } }))
        .await;
    let (status, v) = w.transition(&approver, instance, run(1, json!({ "owner_team": "ops", "risk": 1 }))).await;
    assert_eq!(status, 200, "{v}");
    let pending: Vec<(String, String)> =
        sqlx::query_as("SELECT action_key, status FROM workflow_action_runs").fetch_all(&w.pool).await.unwrap();
    assert_eq!(pending, [("tell_ops".to_owned(), "pending".to_owned())], "queued, not sent, by the request");
    assert_eq!(count(&w.pool, "SELECT count(*) FROM notifications WHERE kind = 'workflow_action'").await, 0);

    assert_eq!(drain(&w.pool, &cfg()).await, 1);
    let deliveries: Vec<(Uuid, String, Option<String>)> = sqlx::query_as(
        "SELECT user_id, status, status_reason FROM workflow_action_deliveries ORDER BY status, status_reason",
    )
    .fetch_all(&w.pool)
    .await
    .unwrap();
    assert_eq!(
        deliveries,
        [
            (o1, "delivered".to_owned(), None),
            (approver_id, "skipped".to_owned(), Some("actor".to_owned())),
            (b1, "skipped".to_owned(), Some("no_view".to_owned())),
        ]
    );
    let rows: Vec<(Uuid, String, String, Value)> =
        sqlx::query_as("SELECT user_id, kind, entity_type, data FROM notifications WHERE kind = 'workflow_action'")
            .fetch_all(&w.pool)
            .await
            .unwrap();
    assert_eq!(rows.len(), 1, "o1 only: no row for b1, who may not view servers");
    assert_eq!((rows[0].0, rows[0].2.as_str()), (o1, "workflow_instances"));
    let d = &rows[0].3;
    assert_eq!(
        (d["actionKey"].as_str(), d["transitionName"].as_str(), d["toStateName"].as_str(), d["actorName"].as_str()),
        (Some("tell_ops"), Some("Approve"), Some("Approved"), Some("approver"))
    );
    assert_eq!(d["ciId"], json!(ci));
    let status: String =
        sqlx::query_scalar("SELECT status FROM workflow_action_runs").fetch_one(&w.pool).await.unwrap();
    assert_eq!(status, "fanned_out");
    db.drop().await;
}

/// The SQL fixture of the upgrade tests with `n` transition events, each
/// queueing one run of an inbox action for one user who may view the CI.
async fn queued(pool: &PgPool, n: i64) -> (Fixture, Uuid) {
    let class = id(pool, "SELECT id FROM ci_classes WHERE system_role = 'business_service'").await;
    let ci =
        id(pool, &format!("INSERT INTO configuration_items (class_id, label) VALUES ('{class}', 'one') RETURNING id"))
            .await;
    let f = workflow_fixture(pool, "lifecycle", class, ci, None).await;
    let user = id(
        pool,
        "INSERT INTO users (username, display_name, password_hash) VALUES ('alice', 'Alice', '$argon2id$v=19$test')
         RETURNING id",
    )
    .await;
    let profile = id(pool, "INSERT INTO permission_profiles (name) VALUES ('Viewers') RETURNING id").await;
    ok(
        pool,
        &format!(
            "INSERT INTO permission_profile_class_permissions (profile_id, class_id, can_view, can_create, can_edit,
               can_delete) VALUES ('{profile}', '{class}', true, false, false, false);
             INSERT INTO user_permission_profiles (user_id, profile_id) VALUES ('{user}', '{profile}');
             UPDATE workflow_action_queue_state SET max_per_instance_per_hour = 10000"
        ),
    )
    .await;
    let action = id(
        pool,
        &format!(
            "INSERT INTO workflow_actions (definition_id, key, name, kind, trigger, transition_key)
             VALUES ('{}', 'tell', 'Tell', 'inbox', 'transition', 'finish') RETURNING id",
            f.definition
        ),
    )
    .await;
    ok(
        pool,
        &format!(
            "INSERT INTO workflow_action_recipients (action_id, position, source, user_id)
             VALUES ('{action}', 1, 'user', '{user}')"
        ),
    )
    .await;
    if n > 0 {
        ok(
            pool,
            &format!(
                "INSERT INTO workflow_instance_events (instance_id, kind, transition_key, from_state_key, to_state_key,
                   to_version_no, actor_type, actor_name)
                 SELECT '{}', 'transition', 'finish', 'planned', 'done', 1, 'system', 'bulk' FROM generate_series(1, {n})",
                f.instance
            ),
        )
        .await;
    }
    (f, user)
}

/// Two worker pools on one database deliver each of 1,000 inbox runs exactly once.
#[tokio::test]
async fn two_processes_deliver_each_of_1000_runs_exactly_once() {
    let Some(db) = scratch::database("workflow_actions_two_workers").await else { return };
    let pool = &db.pool;
    let cfg = WorkflowActionsConfig { max_per_instance_per_hour: 10_000, ..cfg() };
    // Queued in separate transactions, as transitions would be.
    let (f, user) = queued(pool, 0).await;
    for _ in 0..10 {
        ok(
            pool,
            &format!(
                "INSERT INTO workflow_instance_events (instance_id, kind, transition_key, from_state_key, to_state_key,
                   to_version_no, actor_type, actor_name)
                 SELECT '{}', 'transition', 'finish', 'planned', 'done', 1, 'system', 'bulk' FROM generate_series(1, 100)",
                f.instance
            ),
        )
        .await;
    }
    assert_eq!(count(pool, "SELECT count(*) FROM workflow_action_runs WHERE status = 'pending'").await, 1000);

    let a = Outbox::start(pool.clone(), cfg, Channels::default());
    let b = Outbox::start(pool.clone(), cfg, Channels::default());
    let started = Instant::now();
    while count(pool, "SELECT count(*) FROM workflow_action_runs WHERE status = 'fanned_out'").await < 1000 {
        assert!(started.elapsed() < Duration::from_secs(60), "not all runs fanned out within 60 s");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    a.stop().await;
    b.stop().await;
    assert_eq!(count(pool, "SELECT count(*) FROM workflow_action_deliveries WHERE status = 'delivered'").await, 1000);
    let rows = count(pool, &format!("SELECT count(*) FROM notifications WHERE user_id = '{user}'")).await;
    let distinct = count(pool, "SELECT count(DISTINCT dedupe_key) FROM notifications").await;
    assert_eq!((rows, distinct), (1000, 1000), "one notification per run, none twice");
    let owners = count(
        pool,
        "SELECT count(DISTINCT split_part(lease_owner, '/', 1)) FROM workflow_action_runs WHERE lease_owner IS NOT NULL",
    )
    .await;
    assert_eq!(owners, 0, "every lease released");
    db.drop().await;
}

/// A worker that stops between claiming a run and finishing its fan-out
/// leaves nothing; after the lease ends another worker fans it out once, and
/// the first one's late attempt writes nothing.
#[tokio::test]
async fn a_worker_lost_mid_fan_out_leaves_no_duplicate() {
    let Some(db) = scratch::database("workflow_actions_lost_worker").await else { return };
    let pool = &db.pool;
    let cfg = cfg();
    queued(pool, 1).await;
    let claimed = outbox::claim_runs(pool, "dead-worker", 10).await.unwrap();
    assert_eq!(claimed.len(), 1);
    let run = claimed[0];

    // Killed mid-fan-out: the transaction never commits.
    {
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("INSERT INTO workflow_action_deliveries (run_id, recipient_key, status, completed_at) VALUES ($1, 'user:x', 'delivered', now())")
            .bind(run)
            .execute(&mut *tx)
            .await
            .unwrap();
        drop(tx);
    }
    assert_eq!(outbox::claim_runs(pool, "other", 10).await.unwrap(), Vec::<i64>::new(), "still leased");
    assert_eq!(outbox::housekeeping(pool, &cfg).await.unwrap().runs_released, 0, "the lease has not ended");
    ok(pool, "UPDATE workflow_action_runs SET lease_until = now() - interval '1 second'").await;
    assert_eq!(outbox::housekeeping(pool, &cfg).await.unwrap().runs_released, 1);
    assert_eq!(drain(pool, &cfg).await, 1);
    assert_eq!(outbox::fan_out(pool, &cfg, run, "dead-worker").await.unwrap(), FanOut::Lost, "late: writes nothing");
    assert_eq!(count(pool, "SELECT count(*) FROM workflow_action_deliveries").await, 1);
    assert_eq!(count(pool, "SELECT count(*) FROM notifications").await, 1);
    db.drop().await;
}

/// A delivery of a sending channel (a stand-in for e-mail): written pending, as S4's fan-out will.
async fn delivery(pool: &PgPool) -> Uuid {
    let (f, _) = queued(pool, 0).await;
    let run: i64 = sqlx::query_scalar(
        "INSERT INTO workflow_action_runs (event_id, action_key, kind, definition_id, instance_id, ci_id, status)
         SELECT 1, 'mail', 'email', $1, id, ci_id, 'fanned_out' FROM workflow_instances WHERE id = $2 RETURNING id",
    )
    .bind(f.definition)
    .bind(f.instance)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query_scalar(
        "INSERT INTO workflow_action_deliveries (run_id, recipient_key, status) VALUES ($1, 'addr:a@b.example', 'pending')
         RETURNING id",
    )
    .bind(run)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn claim_one(pool: &PgPool) -> Claimed {
    let c = outbox::claim_deliveries(pool, "w", WorkflowActionKind::Email, Duration::from_secs(10), 10).await.unwrap();
    assert_eq!(c.len(), 1, "{c:?}");
    c.into_iter().next().unwrap()
}

/// Seconds until the delivery's next attempt.
async fn next_in(pool: &PgPool, d: Uuid) -> f64 {
    sqlx::query_scalar(
        "SELECT extract(epoch FROM next_attempt_at - now())::float8 FROM workflow_action_deliveries WHERE id = $1",
    )
    .bind(d)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Transient failures are retried on the backoff schedule up to the last
/// attempt; a permanent failure is dead at once; every dead letter is
/// audited; a stale lease writes nothing.
#[tokio::test]
async fn failures_back_off_die_and_are_audited() {
    let Some(db) = scratch::database("workflow_actions_backoff").await else { return };
    let pool = &db.pool;
    let cfg = WorkflowActionsConfig { max_attempts: 3, ..cfg() };
    let d = delivery(pool).await;
    let transient = || Outcome::Transient { status_code: Some(503), error: "busy".into(), retry_after: None };

    // Attempt 1 fails: about 30 s (0.8 to 1.2 jitter).
    let c = claim_one(pool).await;
    assert_eq!((c.attempts, c.epoch), (1, 1));
    assert!(outbox::record(pool, &cfg, &c, transient()).await.unwrap());
    let wait = next_in(pool, d).await;
    assert!((23.0..=36.5).contains(&wait), "first retry after {wait} s");
    assert!(
        outbox::claim_deliveries(pool, "w", WorkflowActionKind::Email, Duration::from_secs(10), 10)
            .await
            .unwrap()
            .is_empty(),
        "not due yet"
    );

    // Attempt 2 fails: about 60 s; a Retry-After replaces it.
    ok(pool, "UPDATE workflow_action_deliveries SET next_attempt_at = now()").await;
    let c = claim_one(pool).await;
    assert_eq!(c.attempts, 2);
    // A stale lease (an earlier epoch) writes nothing.
    let stale = Claimed { epoch: 1, ..c.clone() };
    assert!(!outbox::record(pool, &cfg, &stale, Outcome::Delivered { status_code: Some(200) }).await.unwrap());
    let retry = Outcome::Transient {
        status_code: Some(429),
        error: "slow down".into(),
        retry_after: Some(Duration::from_secs(120)),
    };
    assert!(outbox::record(pool, &cfg, &c, retry).await.unwrap());
    let wait = next_in(pool, d).await;
    assert!((118.0..=120.5).contains(&wait), "Retry-After honoured: {wait} s");

    // Attempt 3 is the last: dead, with one audit row.
    ok(pool, "UPDATE workflow_action_deliveries SET next_attempt_at = now()").await;
    let c = claim_one(pool).await;
    assert!(outbox::record(pool, &cfg, &c, transient()).await.unwrap());
    let row: (String, Option<String>, i16, Option<i32>) =
        sqlx::query_as("SELECT status, status_reason, attempts, last_status_code FROM workflow_action_deliveries")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(row, ("dead".into(), Some("max_attempts".into()), 3, Some(503)));
    let audit: Vec<(String, String, Uuid, Value)> = sqlx::query_as(
        "SELECT actor_type, action, entity_id, new_value FROM audit_log WHERE action = 'workflow.action_dead'",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!((audit[0].0.as_str(), audit[0].2), ("system", d));
    assert_eq!(
        (audit[0].3["reason"].as_str(), audit[0].3["attempts"].as_i64(), audit[0].3["kind"].as_str()),
        (Some("max_attempts"), Some(3), Some("email"))
    );
    assert!(audit[0].3.get("recipientKey").is_none() && !audit[0].3.to_string().contains("a@b.example"), "no address");

    // A permanent failure is dead on the first attempt.
    let d2 = delivery_again(pool).await;
    let c = claim_one(pool).await;
    assert_eq!(c.id, d2);
    let permanent =
        Outcome::Permanent { status_code: Some(550), reason: "rejected".into(), error: "no such user".into() };
    assert!(outbox::record(pool, &cfg, &c, permanent).await.unwrap());
    let row: (String, Option<String>, i16) =
        sqlx::query_as("SELECT status, status_reason, attempts FROM workflow_action_deliveries WHERE id = $1")
            .bind(d2)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(row, ("dead".into(), Some("rejected".into()), 1));
    assert_eq!(count(pool, "SELECT count(*) FROM audit_log WHERE action = 'workflow.action_dead'").await, 2);

    // Too old: dead as expired, audited, never sent.
    let d3 = delivery_again(pool).await;
    sqlx::query("UPDATE workflow_action_deliveries SET created_at = now() - interval '25 hours' WHERE id = $1")
        .bind(d3)
        .execute(pool)
        .await
        .unwrap();
    assert_eq!(outbox::housekeeping(pool, &cfg).await.unwrap().expired, 1);
    let reason: Option<String> =
        sqlx::query_scalar("SELECT status_reason FROM workflow_action_deliveries WHERE id = $1")
            .bind(d3)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(reason.as_deref(), Some("expired"));
    assert_eq!(count(pool, "SELECT count(*) FROM audit_log WHERE action = 'workflow.action_dead'").await, 3);
    db.drop().await;
}

/// Another pending delivery on the first run.
async fn delivery_again(pool: &PgPool) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO workflow_action_deliveries (run_id, recipient_key, status)
         SELECT min(id), 'addr:' || gen_random_uuid() || '@b.example', 'pending' FROM workflow_action_runs RETURNING id",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

/// The backoff schedule: 30 s doubling to at most an hour, with jitter; Retry-After capped at an hour.
#[test]
fn backoff_doubles_to_an_hour_with_jitter() {
    let secs = |a: i16, j: f64| outbox::backoff(a, None, j).as_secs_f64();
    assert_eq!(
        [secs(1, 1.0), secs(2, 1.0), secs(3, 1.0), secs(7, 1.0), secs(8, 1.0)],
        [30.0, 60.0, 120.0, 1920.0, 3600.0]
    );
    assert_eq!(secs(20, 1.0), 3600.0);
    assert_eq!((secs(1, 0.8), secs(1, 1.2)), (24.0, 36.0));
    assert_eq!((secs(1, 0.1), secs(1, 9.0)), (24.0, 36.0), "jitter is clamped");
    assert_eq!(outbox::backoff(1, Some(Duration::from_secs(5)), 1.0), Duration::from_secs(5));
    assert_eq!(outbox::backoff(1, Some(Duration::from_secs(86_400)), 1.0), Duration::from_secs(3600));
}

/// With a queue of at most 100, the 101st run is suppressed as queue_full and
/// one `workflow.action_suppressed` row is written; a second pass in the same
/// minute writes no second row, and the backlog going down lifts the flag.
#[tokio::test]
async fn a_full_queue_suppresses_new_runs_and_audits_once() {
    let Some(db) = scratch::database("workflow_actions_queue_full").await else { return };
    let pool = &db.pool;
    let cfg = WorkflowActionsConfig { queue_max: 100, max_per_instance_per_hour: 10_000, ..cfg() };
    let (f, _) = queued(pool, 100).await;
    let h = outbox::housekeeping(pool, &cfg).await.unwrap();
    assert_eq!((h.backlog, h.overloaded), (100, true));
    let event = format!(
        "INSERT INTO workflow_instance_events (instance_id, kind, transition_key, from_state_key, to_state_key,
           to_version_no, actor_type, actor_name) VALUES ('{}', 'transition', 'finish', 'planned', 'done', 1, 'system', 'x')",
        f.instance
    );
    ok(pool, &event).await;
    let runs: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT status, status_reason FROM workflow_action_runs ORDER BY id DESC LIMIT 1")
            .fetch_all(pool)
            .await
            .unwrap();
    assert_eq!(runs, [("suppressed".to_owned(), Some("queue_full".to_owned()))]);
    assert_eq!(count(pool, "SELECT count(*) FROM workflow_action_runs WHERE status = 'pending'").await, 100);
    assert_eq!(outbox::housekeeping(pool, &cfg).await.unwrap().suppressed_audited, 1);
    let audit: Vec<(String, Uuid, Value)> = sqlx::query_as(
        "SELECT actor_type, entity_id, new_value FROM audit_log WHERE action = 'workflow.action_suppressed'",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!((audit[0].0.as_str(), audit[0].1), ("system", f.definition));
    assert_eq!((audit[0].2["reason"].as_str(), audit[0].2["count"].as_i64()), (Some("queue_full"), Some(1)));

    // More in the same minute: no second row until the minute is over.
    ok(pool, &event).await;
    assert_eq!(outbox::housekeeping(pool, &cfg).await.unwrap().suppressed_audited, 0);
    assert_eq!(count(pool, "SELECT count(*) FROM audit_log WHERE action = 'workflow.action_suppressed'").await, 1);
    ok(pool, "UPDATE workflow_action_rate_windows SET window_start = window_start - interval '1 minute'").await;
    assert_eq!(outbox::housekeeping(pool, &cfg).await.unwrap().suppressed_audited, 1);
    assert_eq!(count(pool, "SELECT count(*) FROM audit_log WHERE action = 'workflow.action_suppressed'").await, 2);

    // Worked off: the flag drops, and new runs queue again.
    drain(pool, &cfg).await;
    assert!(!outbox::housekeeping(pool, &cfg).await.unwrap().overloaded);
    ok(pool, &event).await;
    assert_eq!(count(pool, "SELECT count(*) FROM workflow_action_runs WHERE status = 'pending'").await, 1);
    db.drop().await;
}

/// 51 events on one instance in an hour: the 51st is suppressed as
/// instance_rate. Ten transitions in a row caused by our own deliveries are
/// an echo loop: the tenth is suppressed at once.
#[tokio::test]
async fn loops_are_broken_per_instance_and_on_echo() {
    let Some(db) = scratch::database("workflow_actions_loops").await else { return };
    let pool = &db.pool;
    let cfg = cfg();
    let (f, _) = queued(pool, 0).await;
    outbox::housekeeping(pool, &cfg).await.unwrap();
    let limit: i32 = sqlx::query_scalar("SELECT max_per_instance_per_hour FROM workflow_action_queue_state")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(limit, 50, "written by the workers from WORKFLOW_ACTIONS_MAX_PER_INSTANCE_PER_HOUR");
    let event = |cause: &str| {
        format!(
            "INSERT INTO workflow_instance_events (instance_id, kind, transition_key, from_state_key, to_state_key,
               to_version_no, actor_type, actor_name, caused_by_delivery_id)
             VALUES ('{}', 'transition', 'finish', 'planned', 'done', 1, 'user', 'x', {cause})",
            f.instance
        )
    };
    for _ in 0..51 {
        ok(pool, &event("NULL")).await;
    }
    let statuses: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT status, status_reason FROM workflow_action_runs ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap();
    assert_eq!(statuses.len(), 51);
    assert!(statuses[..50].iter().all(|s| s.0 == "pending"), "{statuses:?}");
    assert_eq!(statuses[50], ("suppressed".to_owned(), Some("instance_rate".to_owned())));
    // An hour later the instance may enqueue again.
    ok(pool, "UPDATE workflow_action_instance_rate SET minute = minute - interval '1 hour'").await;
    ok(pool, &event("NULL")).await;
    let last: String = sqlx::query_scalar("SELECT status FROM workflow_action_runs ORDER BY id DESC LIMIT 1")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(last, "pending", "the window slides");

    // Echo loop: a fresh hour, ten transitions caused by deliveries.
    ok(pool, "DELETE FROM workflow_action_runs").await;
    ok(pool, "DELETE FROM workflow_action_instance_rate").await;
    for i in 0..10 {
        ok(pool, &event("gen_random_uuid()")).await;
        let last: (String, Option<String>) =
            sqlx::query_as("SELECT status, status_reason FROM workflow_action_runs ORDER BY id DESC LIMIT 1")
                .fetch_one(pool)
                .await
                .unwrap();
        let expected =
            if i < 9 { ("pending".to_owned(), None) } else { ("suppressed".to_owned(), Some("echo_loop".to_owned())) };
        assert_eq!(last, expected, "transition {}", i + 1);
    }
    ok(pool, &event("NULL")).await;
    let last: String = sqlx::query_scalar("SELECT status FROM workflow_action_runs ORDER BY id DESC LIMIT 1")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(last, "pending", "a transition a person ran breaks the chain");
    db.drop().await;
}

/// Runs transition `body` on `instance` as `creds`, claiming `cause`.
async fn transition_caused_by(w: &World, creds: &Creds, instance: &str, body: Value, cause: Uuid) -> Value {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    let mut req = Request::builder()
        .method("POST")
        .uri(format!("{RUN}/{instance}/transitions"))
        .header("content-type", "application/json")
        .header("x-shadoucmdb-cause", cause.to_string());
    if let Some(c) = &creds.cookie {
        req = req.header("cookie", c).header("x-csrf-token", creds.csrf.clone().unwrap());
    }
    if let Some(b) = &creds.bearer {
        req = req.header("authorization", format!("Bearer {b}"));
    }
    let res = w.app.clone().oneshot(req.body(Body::from(body.to_string())).unwrap()).await.unwrap();
    let status = res.status().as_u16();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert_eq!(status, 200, "{v}");
    v
}

/// The last event's recorded cause.
async fn last_cause(pool: &PgPool) -> Option<Uuid> {
    sqlx::query_scalar("SELECT caused_by_delivery_id FROM workflow_instance_events ORDER BY id DESC LIMIT 1")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// GH#845: a made-up `X-ShadouCMDB-Cause` is not recorded, so it cannot
/// silence an instance's actions. A session's cause is never taken, a token's
/// only when it names a webhook delivery sent for that instance within
/// `WORKFLOW_ACTIONS_MAX_AGE_HOURS`.
#[tokio::test]
async fn only_a_tokens_cause_naming_a_sent_webhook_delivery_is_recorded() {
    let Some(db) = scratch::database("workflow_actions_cause").await else { return };
    let w = world(&db).await;
    // A loop, so one instance can take a dozen transitions.
    let graph = json!({
        "initialState": "planned",
        "states": [
            { "key": "planned", "name": "Planned", "category": "open", "stateValue": "planned" },
            { "key": "approved", "name": "Approved", "category": "active", "stateValue": "approved" },
            { "key": "done", "name": "In production", "category": "done", "terminal": true, "stateValue": "live" }
        ],
        "transitions": [
            { "key": "approve", "name": "Approve", "from": "planned", "to": "approved" },
            { "key": "rework", "name": "Rework", "from": "approved", "to": "planned" },
            { "key": "go_live", "name": "Go live", "from": "approved", "to": "done" }
        ]
    });
    let draft = w.ok("PUT", &format!("{DEFS}/{}/draft", w.definition), graph).await;
    w.ok(
        "POST",
        &format!("{DEFS}/{}/draft/publish", w.definition),
        json!({ "expectedDraftChecksum": draft["checksum"], "changeNote": "loop" }),
    )
    .await;
    let def = w.ok("GET", &format!("{DEFS}/{}", w.definition), json!(null)).await;
    w.ok(
        "PUT",
        &format!("{DEFS}/{}/grants", w.definition),
        json!({ "version": def["version"], "grants": [
            { "transitionKey": "approve", "profiles": ["Approvers"] },
            { "transitionKey": "rework", "profiles": ["Approvers"] },
            { "transitionKey": "go_live", "profiles": ["Approvers"] }
        ] }),
    )
    .await;
    let version = version(&w).await;
    let to_admin = json!([{ "source": "user", "user": "admin" }]);
    w.ok(
        "PUT",
        &actions(&w),
        json!({ "version": version, "actions": [
            inbox("on_approve", "transition", Some("approve"), to_admin.clone()),
            inbox("on_rework", "transition", Some("rework"), to_admin),
        ] }),
    )
    .await;
    let ci = w.ci(w.server).await;
    let (status, v) = w.start(&w.admin, ci).await;
    assert_eq!(status, 201, "{v}");
    let instance = v["instance"]["id"].as_str().unwrap().to_owned();
    let mut expected = v["instance"]["version"].as_i64().unwrap();
    let step = |i: usize| json!({ "transitionKey": if i.is_multiple_of(2) { "approve" } else { "rework" } });

    // A signed-in user sends a random cause on 12 transitions: none is recorded, nothing is suppressed.
    for i in 0..12 {
        let mut body = step(i);
        body["expectedVersion"] = json!(expected);
        let v = transition_caused_by(&w, &w.admin, &instance, body, Uuid::new_v4()).await;
        expected = v["version"].as_i64().unwrap();
        assert_eq!(last_cause(&w.pool).await, None, "transition {}", i + 1);
    }
    let runs: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT status, status_reason FROM workflow_action_runs ORDER BY id")
            .fetch_all(&w.pool)
            .await
            .unwrap();
    assert_eq!(runs.len(), 12, "{runs:?}");
    assert!(runs.iter().all(|r| r.0 == "pending"), "nothing suppressed: {runs:?}");

    // An API token: a random id, and a delivery of another instance, are not taken.
    let (_, robot) = w.user("robot", &[w.approvers]).await;
    let token = w.token(robot, w.approvers).await;
    let webhook_delivery = |instance: Uuid, attempts: i16, age: &str| {
        let pool = w.pool.clone();
        let age = age.to_owned();
        async move {
            let run: i64 = sqlx::query_scalar(
                "INSERT INTO workflow_action_runs (event_id, action_key, kind, definition_id, instance_id, ci_id, status)
                 VALUES (0, 'hook', 'webhook', gen_random_uuid(), $1, gen_random_uuid(), 'fanned_out') RETURNING id",
            )
            .bind(instance)
            .fetch_one(&pool)
            .await
            .unwrap();
            sqlx::query_scalar::<_, Uuid>(sqlx::AssertSqlSafe(format!(
                "INSERT INTO workflow_action_deliveries (run_id, recipient_key, status, attempts, created_at)
                 VALUES ($1, 'endpoint:x', 'delivered', $2, now() - interval '{age}') RETURNING id"
            )))
            .bind(run)
            .bind(attempts)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let this: Uuid = instance.parse().unwrap();
    let other = webhook_delivery(Uuid::new_v4(), 1, "1 minute").await;
    let unsent = webhook_delivery(this, 0, "1 minute").await;
    let stale = webhook_delivery(this, 1, "25 hours").await;
    let sent = webhook_delivery(this, 1, "1 minute").await;
    for (i, (cause, recorded)) in
        [(Uuid::new_v4(), None), (other, None), (unsent, None), (stale, None), (sent, Some(sent))]
            .into_iter()
            .enumerate()
    {
        let mut body = step(i);
        body["expectedVersion"] = json!(expected);
        let v = transition_caused_by(&w, &token, &instance, body, cause).await;
        expected = v["version"].as_i64().unwrap();
        assert_eq!(last_cause(&w.pool).await, recorded, "case {i}");
    }
    // The same real delivery, claimed by a session, is not taken either.
    let mut body = step(5);
    body["expectedVersion"] = json!(expected);
    transition_caused_by(&w, &w.admin, &instance, body, sent).await;
    assert_eq!(last_cause(&w.pool).await, None);
    db.drop().await;
}

/// Retention deletes finished rows past their periods and keeps the rest.
#[tokio::test]
async fn retention_deletes_finished_rows_past_their_periods() {
    let Some(db) = scratch::database("workflow_actions_retention").await else { return };
    let pool = &db.pool;
    let cfg = cfg();
    queued(pool, 2).await;
    drain(pool, &cfg).await;
    let d = delivery_again(pool).await;
    ok(pool, "UPDATE workflow_action_runs SET created_at = now() - interval '31 days' WHERE id = (SELECT min(id) FROM workflow_action_runs)").await;
    ok(pool, "UPDATE workflow_action_deliveries SET created_at = now() - interval '31 days'").await;
    let r = outbox::retention(pool, &cfg).await.unwrap();
    assert_eq!((r.deliveries, r.runs), (2, 0), "the old run still holds a pending delivery");
    assert_eq!(count(pool, "SELECT count(*) FROM workflow_action_deliveries").await, 1);
    sqlx::query("UPDATE workflow_action_deliveries SET status = 'dead', status_reason = 'x' WHERE id = $1")
        .bind(d)
        .execute(pool)
        .await
        .unwrap();
    assert_eq!(outbox::retention(pool, &cfg).await.unwrap(), outbox::Retention::default(), "dead: kept 90 days");
    ok(pool, "UPDATE workflow_action_deliveries SET created_at = now() - interval '91 days'").await;
    let r = outbox::retention(pool, &cfg).await.unwrap();
    assert_eq!((r.dead, r.runs), (1, 1));
    assert_eq!(count(pool, "SELECT count(*) FROM workflow_action_runs").await, 1, "the recent run stays");
    db.drop().await;
}

// ---------------------------------------------------------------------------
// Performance (not part of the normal run):
//   cargo test --release modules::workflows::actions::tests::perf -- --ignored --nocapture --test-threads 1
// ---------------------------------------------------------------------------

fn p95(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[(v.len() * 95 / 100).min(v.len() - 1)]
}

/// The time an event's transaction takes with 3 matching actions, against
/// none: the enqueue trigger must add under 1 ms at p95.
#[tokio::test]
#[ignore = "performance gate"]
async fn perf_enqueue_adds_under_a_millisecond() {
    let Some(db) = scratch::database("workflow_actions_perf_enqueue").await else { return };
    let pool = &db.pool;
    let (f, _) = queued(pool, 0).await;
    // The highest limit: the per-instance check must stay cheap on a hot instance too.
    ok(pool, "UPDATE workflow_action_queue_state SET max_per_instance_per_hour = 10000").await;
    for k in ["tell2", "tell3"] {
        ok(
            pool,
            &format!(
                "INSERT INTO workflow_actions (definition_id, key, name, kind, trigger, transition_key)
            VALUES ('{}', '{k}', '{k}', 'inbox', 'transition', 'finish')",
                f.definition
            ),
        )
        .await;
    }
    // The same trigger runs for both events; only `finish` matches the 3 actions. The two are
    // interleaved sample by sample, so load on the host weighs on both alike.
    let event = |transition: &str| {
        format!(
            "INSERT INTO workflow_instance_events (instance_id, kind, transition_key, from_state_key, to_state_key,
               to_version_no, actor_type, actor_name) VALUES ('{}', 'transition', '{transition}', 'planned', 'done', 1,
               'user', 'x')",
            f.instance
        )
    };
    let (with_actions, without_actions) = (event("finish"), event("other"));
    let mut conn = pool.acquire().await.unwrap();
    let (mut with, mut without) = (Vec::with_capacity(2000), Vec::with_capacity(2000));
    for i in 0..2200 {
        for (sql, out) in [(&with_actions, &mut with), (&without_actions, &mut without)] {
            let t = Instant::now();
            let mut tx = sqlx::Connection::begin(&mut *conn).await.unwrap();
            sqlx::query(sqlx::AssertSqlSafe(sql.clone())).execute(&mut *tx).await.unwrap();
            tx.commit().await.unwrap();
            if i >= 200 {
                out.push(t.elapsed().as_secs_f64() * 1000.0);
            }
        }
    }
    drop(conn);
    let (with, without) = (p95(with), p95(without));
    println!(
        "event transaction p95: {without:.3} ms without actions, {with:.3} ms with 3 actions (+{:.3} ms)",
        with - without
    );
    assert_eq!(count(pool, "SELECT count(*) FROM workflow_action_runs").await, 3 * 2200);
    assert!(with - without < 1.0, "the enqueue trigger adds {:.3} ms at p95", with - without);
    db.drop().await;
}

/// 10,000 pending deliveries of a sending channel are claimed and recorded
/// at 200 a second or more by one process.
#[tokio::test]
#[ignore = "performance gate"]
async fn perf_one_process_claims_200_deliveries_a_second() {
    let Some(db) = scratch::database("workflow_actions_perf_claim").await else { return };
    let pool = &db.pool;
    delivery(pool).await;
    ok(
        pool,
        "INSERT INTO workflow_action_deliveries (run_id, recipient_key, status)
         SELECT (SELECT min(id) FROM workflow_action_runs), 'addr:' || g || '@b.example', 'pending'
         FROM generate_series(1, 9999) g",
    )
    .await;
    let sent = Arc::new(AtomicUsize::new(0));
    let counter = sent.clone();
    let channel = Channel {
        send: Arc::new(move |_c: Claimed| -> futures_util::future::BoxFuture<'static, Outcome> {
            counter.fetch_add(1, Ordering::Relaxed);
            Box::pin(async { Outcome::Delivered { status_code: Some(250) } })
        }),
        timeout: Duration::from_secs(10),
    };
    let cfg = WorkflowActionsConfig { concurrency: 4, ..cfg() };
    let started = Instant::now();
    let outbox = Outbox::start(pool.clone(), cfg, Channels::default().with(WorkflowActionKind::Email, channel));
    while count(pool, "SELECT count(*) FROM workflow_action_deliveries WHERE status = 'delivered'").await < 10_000 {
        assert!(started.elapsed() < Duration::from_secs(120), "too slow");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let rate = 10_000.0 / started.elapsed().as_secs_f64();
    outbox.stop().await;
    println!("10,000 deliveries claimed, sent and recorded at {rate:.0}/s by one process");
    assert_eq!(sent.load(Ordering::Relaxed), 10_000, "each sent once");
    assert!(rate >= 200.0, "{rate:.0}/s");
    db.drop().await;
}
