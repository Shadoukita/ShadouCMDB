//! Workflow runtime performance gate (SHAA-1411 §9, SHAA-1424). Not part of
//! the normal test run:
//!
//! ```sh
//! SHADOUCMDB_TEST_DATABASE_URL=postgres://… cargo test --release workflow_runtime_performance -- --ignored --nocapture
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

use std::time::Instant;

use serde_json::json;
use uuid::Uuid;

use super::runtime_tests::{World, world};
use crate::db::scratch;
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
