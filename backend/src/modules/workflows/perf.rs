//! Workflow performance gates (SHAA-1411 §9; runtime SHAA-1424, bootstrap SHAA-1698). Not part of
//! the normal test run:
//!
//! ```sh
//! SHADOUCMDB_TEST_DATABASE_URL=postgres://… cargo test --release modules::workflows::perf -- --ignored --nocapture --test-threads 1
//! ```
//! (`tools/perf/workflows.sh` wraps it.) It prints the measurements and fails
//! when a threshold is missed.
//!
//! 500 000 server CIs, each with a running instance of the design example's
//! workflow and its start event, then measured through the real router (as
//! a signed-in administrator, so authentication is part of every request):
//! transitions on instances spread over the table, the instance list (first
//! page, a state filter, a deep page), the per-state summary, one CI's
//! workflows and one instance with its graph and available transitions.
//!
//! Approvals (SHAA-1869 §13, A3; SHAA-1880): the same 500 000 CIs on a version
//! whose `approve` needs a technical review and then two CAB approvals, 10 000
//! of them with a pending request (half at each step). Decisions and the
//! approvals inbox are measured as class-restricted approvers.

use std::time::Instant;

use serde_json::json;
use uuid::Uuid;

use super::approvals_runtime_tests::{REQUESTS, setup};
use super::runtime_tests::{World, world};
use crate::db::scratch;
use crate::modules::api_tokens::tests::Creds;
use crate::modules::impact::perf::{exec, p95};
use crate::schema::model::Model;

const CIS: i64 = 500_000;
const TRANSITIONS: usize = 300;
const READS: usize = 30;
/// §9: p95 of a transition.
const TRANSITION_P95_MS: f64 = 50.0;
/// The list, summary and detail reads.
const READ_P95_MS: f64 = 300.0;

async fn seed(w: &World) {
    let t = Instant::now();
    let pool = &w.pool;
    let model = Model::load(&mut pool.acquire().await.unwrap()).await.unwrap();
    let table = model.table(w.server).unwrap().sql();
    let column = |key: &str| model.own_fields(w.server).find(|f| f.key == key).unwrap().column().to_string();
    exec(pool, "CREATE TABLE perf_ci (n bigint PRIMARY KEY, id uuid NOT NULL)").await;
    exec(pool, &format!("INSERT INTO perf_ci SELECT n, gen_random_uuid() FROM generate_series(1, {CIS}) n")).await;
    sqlx::query(
        "INSERT INTO configuration_items (id, class_id, ident, label, valid_from)
         SELECT id, $1, 'PERF-' || n, 'server ' || lpad(n::text, 6, '0'), now() - interval '1 day'
         FROM perf_ci ORDER BY n",
    )
    .bind(w.server)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO {table} (id, {}, {}) SELECT id, 'prod', $1 FROM perf_ci",
        column("environment"),
        column("lifecycle")
    )))
    .bind(w.value("planned"))
    .execute(pool)
    .await
    .unwrap();
    let (version, planned, version_no): (Uuid, Uuid, i32) = sqlx::query_as(
        "SELECT v.id, s.id, v.version_no FROM workflow_definitions d
         JOIN workflow_versions v ON v.id = d.current_version_id
         JOIN workflow_states s ON s.version_id = v.id AND s.key = 'planned' WHERE d.id = $1",
    )
    .bind(w.definition)
    .fetch_one(pool)
    .await
    .unwrap();
    // Start times spread over a year, so lists sort on real data.
    sqlx::query(
        "INSERT INTO workflow_instances
           (definition_id, version_id, ci_id, current_state_id, status, started_by_name, started_at, last_transition_at)
         SELECT $1, $2, id, $3, 'active', 'perf', now() - (n || ' minutes')::interval, now() - (n || ' minutes')::interval
         FROM perf_ci",
    )
    .bind(w.definition)
    .bind(version)
    .bind(planned)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workflow_instance_events (instance_id, kind, to_state_key, to_version_no, actor_type, actor_name, occurred_at)
         SELECT id, 'start', 'planned', $1, 'system', 'perf', started_at FROM workflow_instances",
    )
    .bind(version_no)
    .execute(pool)
    .await
    .unwrap();
    exec(pool, "ANALYZE").await;
    eprintln!("seeded {CIS} CIs with a running instance each in {:?}", t.elapsed());
}

fn report(what: &str, ms: Vec<f64>, limit: f64, failures: &mut Vec<String>) {
    let (p50, p95, max) = p95(ms);
    eprintln!("{what:<44} p50 {p50:>7.1} ms   p95 {p95:>7.1} ms   max {max:>7.1} ms   (limit p95 {limit} ms)");
    if p95 > limit {
        failures.push(format!("{what}: p95 {p95:.1} ms > {limit} ms"));
    }
}

#[tokio::test]
#[ignore = "performance gate: run with --ignored --release (tools/perf/workflows.sh)"]
async fn workflow_runtime_performance() {
    let Some(db) = scratch::database("workflow_runtime_performance").await else { return };
    let w = world(&db).await;
    seed(&w).await;
    let mut failures = Vec::new();

    // Transitions on instances spread over the table (every 1 600th CI).
    let picks: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT wi.id, wi.ci_id FROM workflow_instances wi JOIN perf_ci p ON p.id = wi.ci_id
         WHERE p.n % ($1 / $2) = 0 ORDER BY p.n LIMIT $2",
    )
    .bind(CIS)
    .bind(TRANSITIONS as i64)
    .fetch_all(&w.pool)
    .await
    .unwrap();
    assert_eq!(picks.len(), TRANSITIONS);
    let mut ms = Vec::with_capacity(TRANSITIONS);
    for (instance, _) in &picks {
        let body = json!({ "transitionKey": "approve", "expectedVersion": 1, "fields": { "owner_team": "ops" },
            "comment": "CAB approved" });
        let t = Instant::now();
        let (status, v) = w.transition(&w.admin, *instance, body).await;
        ms.push(t.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(status, 200, "{v}");
    }
    report("transition (approve, 2 fields, comment)", ms, TRANSITION_P95_MS, &mut failures);

    let reads = [
        ("list: first page", "/api/v1/workflow-instances?limit=50".to_owned()),
        ("list: state filter", "/api/v1/workflow-instances?stateKey=approved&status=active&limit=50".to_owned()),
        ("list: page at offset 100 000", "/api/v1/workflow-instances?limit=50&offset=100000".to_owned()),
        ("summary per state", "/api/v1/workflow-instances/summary".to_owned()),
        ("one CI's workflows", format!("/api/v1/configuration-items/{}/workflows", picks[7].1)),
        ("one instance with graph", format!("/api/v1/workflow-instances/{}", picks[9].0)),
    ];
    for (what, path) in reads {
        let mut ms = Vec::with_capacity(READS);
        for _ in 0..READS {
            let t = Instant::now();
            let (status, v) = w.call(&w.admin, "GET", &path, None).await;
            ms.push(t.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(status, 200, "{path}: {v}");
        }
        report(what, ms, READ_P95_MS, &mut failures);
    }
    db.drop().await;
    assert!(failures.is_empty(), "thresholds missed:\n{}", failures.join("\n"));
}

/// Bootstrap of the whole inventory (SHAA-1698): with HTTP_REQUEST_TIMEOUT_SECS at its default.
const BOOTSTRAP_LIMIT_S: f64 = 120.0;

/// Bootstrap (SHAA-1698, §8.2): 500 000 server CIs without an instance, in
/// the design example's states (60 % planned, 30 % approved, 9.9 % live, a
/// terminal state, and 0.1 % with a value no state maps), adopted through the
/// real router: the dry run, the run in batches of 1 000, and a second run that
/// starts nothing. Then the audit chain is verified.
#[tokio::test]
#[ignore = "performance gate: run with --ignored --release (tools/perf/workflows.sh)"]
async fn workflow_bootstrap_performance() {
    let Some(db) = scratch::database("workflow_bootstrap_performance").await else { return };
    let w = world(&db).await;
    let pool = &w.pool;
    let d = w.ok("GET", &format!("/api/v1/admin/workflow-definitions/{}", w.definition), json!(null)).await;
    let path = format!("/api/v1/admin/workflow-definitions/{}", w.definition);
    w.ok("PATCH", &path, json!({ "version": d["version"], "isActive": false })).await;
    let t = Instant::now();
    let model = Model::load(&mut pool.acquire().await.unwrap()).await.unwrap();
    let table = model.table(w.server).unwrap().sql();
    let column = |key: &str| model.own_fields(w.server).find(|f| f.key == key).unwrap().column().to_string();
    exec(pool, "CREATE TABLE perf_ci (n bigint PRIMARY KEY, id uuid NOT NULL)").await;
    exec(pool, &format!("INSERT INTO perf_ci SELECT n, gen_random_uuid() FROM generate_series(1, {CIS}) n")).await;
    sqlx::query(
        "INSERT INTO configuration_items (id, class_id, ident, label, valid_from)
         SELECT id, $1, 'PERF-' || n, 'server ' || lpad(n::text, 6, '0'), now() - interval '1 day'
         FROM perf_ci ORDER BY n",
    )
    .bind(w.server)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO {table} (id, {}, {})
         SELECT id, 'prod', CASE WHEN n % 1000 = 0 THEN NULL WHEN n % 10 < 6 THEN $1 WHEN n % 10 < 9 THEN $2 ELSE $3 END
         FROM perf_ci",
        column("environment"),
        column("lifecycle")
    )))
    .bind(w.value("planned"))
    .bind(w.value("approved"))
    .bind(w.value("live"))
    .execute(pool)
    .await
    .unwrap();
    exec(pool, "ANALYZE").await;
    eprintln!("seeded {CIS} CIs without an instance in {:?}", t.elapsed());
    let d = w.ok("GET", &path, json!(null)).await;
    w.ok("PATCH", &path, json!({ "version": d["version"], "isActive": true })).await;

    let bootstrap = format!("{path}/bootstrap");
    let mut failures = Vec::new();
    for (what, dry_run) in [("dry run", true), ("run", false), ("second run", false)] {
        let t = Instant::now();
        let v = w.ok("POST", &bootstrap, json!({ "stateFromAttribute": true, "dryRun": dry_run })).await;
        let s = t.elapsed().as_secs_f64();
        eprintln!(
            "bootstrap {what:<11} {s:>7.1} s   started {:>7}   already running {:>7}   terminal {:>6}   unmapped {:>4}",
            v["started"], v["alreadyRunning"], v["skippedTerminal"], v["skippedUnmapped"]
        );
        if s > BOOTSTRAP_LIMIT_S {
            failures.push(format!("bootstrap {what}: {s:.1} s > {BOOTSTRAP_LIMIT_S} s"));
        }
    }
    let running: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_instances WHERE status = 'active'")
        .fetch_one(pool)
        .await
        .unwrap();
    // Planned and approved start; every 1 000th CI (a planned one) has no value.
    assert_eq!(running, CIS / 10 * 9 - CIS / 1000);
    let t = Instant::now();
    let problems: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log_verify()").fetch_one(pool).await.unwrap();
    eprintln!("audit_log_verify() over the chain: {problems} problems in {:?}", t.elapsed());
    assert_eq!(problems, 0);
    db.drop().await;
    assert!(failures.is_empty(), "thresholds missed:\n{}", failures.join("\n"));
}

/// Pending approval requests (every 50th CI).
const PENDING: i64 = 10_000;
const DECISIONS: usize = 300;
/// §13: p95 of a decision, and of an inbox page.
const DECISION_P95_MS: f64 = 50.0;
const INBOX_P95_MS: f64 = 200.0;

/// 10 000 pending requests of `approve`, made by `req`: the odd ones at the
/// technical review, the even ones at the CAB step (tech approved step 1).
async fn seed_requests(w: &World, req: Uuid, tech: Uuid) {
    let t = Instant::now();
    let pool = &w.pool;
    let profile = |name: &str| {
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM permission_profiles WHERE name = $1").bind(name.to_owned())
    };
    let (tech_profile, cab, blind) = (
        profile("Tech").fetch_one(pool).await.unwrap(),
        profile("CAB").fetch_one(pool).await.unwrap(),
        profile("Blind").fetch_one(pool).await.unwrap(),
    );
    exec(
        pool,
        &format!(
            "CREATE TABLE perf_req AS
             SELECT gen_random_uuid() AS id, wi.id AS instance_id, wi.version_id, row_number() OVER (ORDER BY p.n) AS k
             FROM workflow_instances wi JOIN perf_ci p ON p.id = wi.ci_id WHERE p.n % ({CIS} / {PENDING}) = 0"
        ),
    )
    .await;
    sqlx::query(
        "INSERT INTO workflow_approval_requests
           (id, instance_id, version_id, transition_key, request_no, status, current_step_no, requested_at,
            requested_by_id, requested_by_name, excluded_user_ids, staged_fields, field_baseline)
         SELECT id, instance_id, version_id, 'approve', 1, 'pending', 2 - k % 2, now() - (k || ' minutes')::interval,
                $1, 'req', ARRAY[$1::uuid], '{\"owner_team\": \"ops\"}', '{\"owner_team\": null}'
         FROM perf_req",
    )
    .bind(req)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workflow_approval_request_steps
           (request_id, step_no, step_key, required_approvals, status, activated_at, due_at, completed_at,
            eligible_count, resolved_at)
         SELECT id, 1, 'tech', 1, CASE WHEN k % 2 = 0 THEN 'approved' ELSE 'active' END, now() - interval '1 day',
                now() + (k || ' minutes')::interval, CASE WHEN k % 2 = 0 THEN now() END, 1, now()
         FROM perf_req
         UNION ALL
         SELECT id, 2, 'cab', 2, CASE WHEN k % 2 = 0 THEN 'active' ELSE 'waiting' END,
                CASE WHEN k % 2 = 0 THEN now() END, CASE WHEN k % 2 = 0 THEN now() + (k || ' minutes')::interval END,
                NULL, CASE WHEN k % 2 = 0 THEN 3 END, CASE WHEN k % 2 = 0 THEN now() END
         FROM perf_req",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workflow_approval_eligibility (request_id, step_no, role, principal_kind, principal_id, via)
         SELECT id, 1, 'approver', 'profile', $1, '{\"source\": \"profile\", \"label\": \"profile Tech\"}'::jsonb FROM perf_req
         UNION ALL
         SELECT id, 2, 'approver', 'profile', $2, '{\"source\": \"profile\", \"label\": \"profile CAB\"}'::jsonb
         FROM perf_req WHERE k % 2 = 0
         UNION ALL
         SELECT id, 2, 'approver', 'profile', $3, '{\"source\": \"profile\", \"label\": \"profile Blind\"}'::jsonb
         FROM perf_req WHERE k % 2 = 0",
    )
    .bind(tech_profile)
    .bind(cab)
    .bind(blind)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workflow_approval_decisions (request_id, step_no, decision, actor_id, actor_name, credential, via)
         SELECT id, 1, 'approve', $1, 'tech', 'session', '[{\"source\": \"profile\", \"label\": \"profile Tech\"}]'
         FROM perf_req WHERE k % 2 = 0",
    )
    .bind(tech)
    .execute(pool)
    .await
    .unwrap();
    exec(pool, "ANALYZE").await;
    eprintln!("seeded {PENDING} pending approval requests in {:?}", t.elapsed());
}

/// `DECISIONS` requests spread over the pending ones, at step `step` (1 or 2).
async fn spread(w: &World, step: i64) -> Vec<Uuid> {
    sqlx::query_scalar(
        "SELECT id FROM perf_req WHERE 2 - k % 2 = $1 AND (k / 2) % ($2 / 2 / $3) = 0 ORDER BY k LIMIT $3",
    )
    .bind(step)
    .bind(PENDING)
    .bind(DECISIONS as i64)
    .fetch_all(&w.pool)
    .await
    .unwrap()
}

/// One approval through the router: (milliseconds, response).
async fn decide(w: &World, creds: &Creds, id: Uuid, step: &str, version: i32) -> (f64, serde_json::Value) {
    let body = json!({ "stepKey": step, "decision": "approve", "expectedVersion": version });
    let t = Instant::now();
    let (status, v) = w.call(creds, "POST", &format!("{REQUESTS}/{id}/decisions"), Some(body)).await;
    assert_eq!(status, 200, "{v}");
    (t.elapsed().as_secs_f64() * 1000.0, v)
}

#[tokio::test]
#[ignore = "performance gate: run with --ignored --release (tools/perf/workflows.sh)"]
async fn workflow_approvals_performance() {
    let Some(db) = scratch::database("workflow_approvals_performance").await else { return };
    let w = world(&db).await;
    let p = setup(&w).await;
    seed(&w).await;
    seed_requests(&w, p.req.1, p.tech.1).await;
    let mut failures = Vec::new();

    // Reads first, on the full 10 000.
    let reads = [
        ("inbox: first page (CAB member)", &p.a1.0, format!("{REQUESTS}?limit=50"), INBOX_P95_MS),
        ("inbox: page at offset 2 000", &p.a1.0, format!("{REQUESTS}?limit=50&offset=2000"), INBOX_P95_MS),
        ("inbox: overdue only", &p.a1.0, format!("{REQUESTS}?overdue=true&limit=50"), INBOX_P95_MS),
        ("inbox: first page (tech)", &p.tech.0, format!("{REQUESTS}?limit=50"), INBOX_P95_MS),
        ("decided by me (tech, 5 000)", &p.tech.0, format!("{REQUESTS}?view=decided&limit=50"), INBOX_P95_MS),
        (
            "runbook: pending by requester",
            &w.admin,
            format!("{REQUESTS}?view=all&status=pending&requestedBy={}&limit=50", p.req.1),
            INBOX_P95_MS,
        ),
        (
            "instances awaiting approval",
            &w.admin,
            "/api/v1/workflow-instances?awaitingApproval=true&limit=50".into(),
            READ_P95_MS,
        ),
        ("summary with awaitingApproval", &w.admin, "/api/v1/workflow-instances/summary".into(), READ_P95_MS),
    ];
    for (what, creds, path, limit) in reads {
        let mut ms = Vec::with_capacity(READS);
        for _ in 0..READS {
            let t = Instant::now();
            let (status, v) = w.call(creds, "GET", &path, None).await;
            ms.push(t.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(status, 200, "{path}: {v}");
        }
        report(what, ms, limit, &mut failures);
    }
    let (_, v) = w.call(&p.a1.0, "GET", &format!("{REQUESTS}?limit=1"), None).await;
    assert_eq!(v["page"]["total"], PENDING / 2, "{v}");

    // Step 1: the tech approval activates the CAB step and resolves its approvers.
    let mut ms = Vec::with_capacity(DECISIONS);
    for id in spread(&w, 1).await {
        let (t, v) = decide(&w, &p.tech.0, id, "tech", 1).await;
        assert_eq!(v["request"]["currentStepNo"], 2, "{v}");
        ms.push(t);
    }
    report("decision: step approved, next step active", ms, DECISION_P95_MS, &mut failures);
    // Step 2: one of two, then the final approval that applies the transition.
    let cab = spread(&w, 2).await;
    let mut ms = Vec::with_capacity(DECISIONS);
    for id in &cab {
        ms.push(decide(&w, &p.a1.0, *id, "cab", 1).await.0);
    }
    report("decision: 1 of 2", ms, DECISION_P95_MS, &mut failures);
    let mut ms = Vec::with_capacity(DECISIONS);
    for id in &cab {
        let (t, v) = decide(&w, &p.a2.0, *id, "cab", 2).await;
        assert_eq!(v["instance"]["state"]["key"], "approved", "{v}");
        ms.push(t);
    }
    report("decision: final, applies the transition", ms, DECISION_P95_MS, &mut failures);
    let problems: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log_verify()").fetch_one(&w.pool).await.unwrap();
    assert_eq!(problems, 0);
    db.drop().await;
    assert!(failures.is_empty(), "thresholds missed:\n{}", failures.join("\n"));
}
