//! Impact analysis performance gate (SHAA-883 spec §6.2). Not part of the
//! normal test run: it seeds 100 000 CIs and 300 000 relationships.
//!
//! ```sh
//! SHADOUCMDB_TEST_DATABASE_URL=postgres://… cargo test --release impact_performance -- --ignored --nocapture
//! ```
//! (`tools/perf/impact.sh` wraps it.) It prints the measurements and fails when
//! a threshold is missed.
//!
//! The data set: 100 000 CIs in 20 classes, 300 000 live relationships over 6
//! types (three target_to_source, one source_to_target, one both, one none),
//! including a hub with 50 000 relationships, a chain of 1 000 CIs and a
//! strongly connected component of 500 CIs. 30 % of the CIs are in 6 classes
//! the test profile may not view.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::engine::{Way, hop_types};
use super::schemas::{AnalysisDirection, ImpactQuery, TruncatedReason};
use super::{ImpactState, service};
use crate::api::context::RequestContext;
use crate::api::schemas::QueryBool;
use crate::auth::permissions::{ClassRights, Permissions};
use crate::auth::{Credential, Principal};
use crate::data::impact as data;
use crate::db::scratch;
use crate::http::error::ErrorCode;
use crate::modules::api_tokens::tests::{Creds, app, call};
use crate::schema::model::Model;

pub(crate) const CIS: i64 = 100_000;
pub(crate) const CLASSES: usize = 20;
pub(crate) const HIDDEN_CLASSES: usize = 6;
const RANDOM_EDGES: i64 = 300_000 - 50_000 - 999 - 1_000;
const HUB_EDGES: i64 = 50_000;
const CHAIN: i64 = 1_000;
const SCC: i64 = 500;

pub(crate) fn viewer(classes: &[Uuid]) -> RequestContext {
    let permissions = Permissions {
        classes: classes.iter().map(|id| (*id, ClassRights { view: true, ..Default::default() })).collect(),
        ..Default::default()
    };
    let principal =
        Principal { user_id: Uuid::new_v4(), username: "perf".into(), credential: Credential::Token, permissions };
    RequestContext::user(Arc::new(principal), "impact-perf".into())
}

fn query(depth: i32, max_nodes: i32) -> ImpactQuery {
    ImpactQuery {
        direction: AnalysisDirection::Downstream,
        depth: Some(depth),
        relationship_type_id: None,
        include_inactive: QueryBool::True,
        max_nodes: Some(max_nodes),
    }
}

pub(crate) fn p95(mut ms: Vec<f64>) -> (f64, f64, f64) {
    ms.sort_by(|a, b| a.total_cmp(b));
    let at = |q: f64| ms[((ms.len() as f64 * q).ceil() as usize).clamp(1, ms.len()) - 1];
    (at(0.5), at(0.95), ms[ms.len() - 1])
}

pub(crate) async fn exec(pool: &PgPool, sql: &str) {
    sqlx::query(sqlx::AssertSqlSafe(sql.to_owned())).execute(pool).await.unwrap_or_else(|e| panic!("{e}: {sql}"));
}

/// Pseudo-random CI number in 1..=CIS from a seed expression (deterministic).
fn pick(seed: &str) -> String {
    format!("(1 + (('x' || substr(md5({seed}), 1, 8))::bit(32)::bigint % {CIS}))")
}

pub(crate) struct Seeded {
    pub classes: Vec<Uuid>,
    hub: Uuid,
    chain_start: Uuid,
    scc_member: Uuid,
}

pub(crate) async fn seed(pool: &PgPool) -> Seeded {
    let app = app(pool.clone());
    let setup = json!({ "username": "owner", "email": "owner@example.test", "displayName": "Owner", "password": "correct horse battery", "setupToken": crate::auth::setup_token::TEST_TOKEN });
    let (status, me, headers) = call(&app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
    assert_eq!(status, 201, "{me}");
    let cookie = headers
        .get_all(axum::http::header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned())
        .collect::<Vec<_>>()
        .join("; ");
    let session = Creds { cookie: Some(cookie), csrf: me["csrfToken"].as_str().map(str::to_owned), bearer: None };
    let mut classes = Vec::new();
    for i in 0..CLASSES {
        let body = json!({ "key": format!("perf_{i:02}"), "name": format!("Perf {i:02}") });
        let (status, v, _) = call(&app, "POST", "/api/v1/ci-classes", &session, Some(body)).await;
        assert_eq!(status, 201, "{v}");
        classes.push(v["id"].as_str().unwrap().parse::<Uuid>().unwrap());
    }

    let t = Instant::now();
    // CIs n = 1..CIS, class by n (the last 6 classes hold 30 %).
    exec(pool, "CREATE TABLE perf_ci (n bigint PRIMARY KEY, id uuid NOT NULL, class_id uuid NOT NULL)").await;
    sqlx::query(
        "INSERT INTO perf_ci (n, id, class_id)
         SELECT n, gen_random_uuid(), ($1::uuid[])[1 + ((n - 1) * 20 / $2)] FROM generate_series(1, $2) n",
    )
    .bind(&classes)
    .bind(CIS)
    .execute(pool)
    .await
    .unwrap();
    exec(
        pool,
        "INSERT INTO configuration_items (id, class_id, ident, label, valid_from)
         SELECT id, class_id, 'PERF-' || n, 'perf ' || lpad(n::text, 6, '0'), now() - interval '1 day' FROM perf_ci ORDER BY n",
    )
    .await;
    let model = Model::load(&mut pool.acquire().await.unwrap()).await.unwrap();
    for c in &classes {
        let table = model.table(*c).unwrap();
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO {} (id) SELECT id FROM perf_ci WHERE class_id = $1",
            table.sql()
        )))
        .bind(c)
        .execute(pool)
        .await
        .unwrap();
    }
    eprintln!("seeded {CIS} CIs in {:?}", t.elapsed());

    let t = Instant::now();
    let mut types = Vec::new();
    for (i, dir) in ["target_to_source", "target_to_source", "target_to_source", "source_to_target", "both", "none"]
        .iter()
        .enumerate()
    {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO relationship_types (key, name, forward_label, reverse_label, impact_direction)
             VALUES ($1, $1, $1, $1, $2) RETURNING id",
        )
        .bind(format!("perf_t{i}"))
        .bind(dir)
        .fetch_one(pool)
        .await
        .unwrap();
        types.push(id);
    }
    exec(
        pool,
        "INSERT INTO relationship_type_rules (relationship_type_id, source_class_id, target_class_id)
         SELECT t.id, a.id, b.id FROM relationship_types t, ci_classes a, ci_classes b
         WHERE t.key LIKE 'perf_t%' AND a.key LIKE 'perf_%' AND b.key LIKE 'perf_%'",
    )
    .await;
    // Random edges, spread over the 6 types, no self-edges or duplicates.
    let (s, g) = (pick("i::text || 's'"), pick("i::text || 't'"));
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO ci_relationships (relationship_type_id, source_ci_id, target_ci_id, created_at)
         SELECT DISTINCT ON (x.t, x.s, x.g) ($1::uuid[])[1 + x.t], a.id, b.id, now() - interval '1 hour' + x.i * interval '1 microsecond'
         FROM (SELECT i, i % 6 AS t, {s} AS s, {g} AS g FROM generate_series(1, $2) i) x
         JOIN perf_ci a ON a.n = x.s JOIN perf_ci b ON b.n = x.g
         WHERE x.s <> x.g ORDER BY x.t, x.s, x.g, x.i"
    )))
    .bind(&types)
    .bind(RANDOM_EDGES)
    .execute(pool)
    .await
    .unwrap();
    // The hub: CI 1, which 50 000 CIs depend on (type 0, target_to_source).
    sqlx::query(
        "INSERT INTO ci_relationships (relationship_type_id, source_ci_id, target_ci_id, created_at)
         SELECT $1, a.id, h.id, now() - interval '30 minutes' + a.n * interval '1 microsecond'
         FROM perf_ci a, perf_ci h WHERE h.n = 1 AND a.n BETWEEN 2 AND $2 + 1
           AND NOT EXISTS (SELECT 1 FROM ci_relationships r WHERE r.relationship_type_id = $1
                           AND r.source_ci_id = a.id AND r.target_ci_id = h.id)",
    )
    .bind(types[0])
    .bind(HUB_EDGES)
    .execute(pool)
    .await
    .unwrap();
    // The chain: CIs 60001..61000 in a visible class, each depending on the previous one (type 1).
    sqlx::query(
        "INSERT INTO ci_relationships (relationship_type_id, source_ci_id, target_ci_id, created_at)
         SELECT $1, b.id, a.id, now() - interval '20 minutes' + a.n * interval '1 microsecond'
         FROM perf_ci a JOIN perf_ci b ON b.n = a.n + 1 WHERE a.n BETWEEN 60001 AND 60000 + $2 - 1
         ON CONFLICT DO NOTHING",
    )
    .bind(types[1])
    .bind(CHAIN)
    .execute(pool)
    .await
    .unwrap();
    // The strongly connected component: CIs 30001..30500 in a ring (type 2) with chords (type 4, both).
    sqlx::query(
        "INSERT INTO ci_relationships (relationship_type_id, source_ci_id, target_ci_id, created_at)
         SELECT $1, b.id, a.id, now() - interval '10 minutes' + a.n * interval '1 microsecond'
         FROM perf_ci a JOIN perf_ci b ON b.n = 30001 + ((a.n - 30001 + 1) % $3)
         WHERE a.n BETWEEN 30001 AND 30000 + $3
         UNION ALL
         SELECT $2, b.id, a.id, now() - interval '5 minutes' + a.n * interval '1 microsecond'
         FROM perf_ci a JOIN perf_ci b ON b.n = 30001 + ((a.n - 30001 + 37) % $3)
         WHERE a.n BETWEEN 30001 AND 30000 + $3
         ON CONFLICT DO NOTHING",
    )
    .bind(types[2])
    .bind(types[4])
    .bind(SCC)
    .execute(pool)
    .await
    .unwrap();
    exec(pool, "ANALYZE").await;
    let edges: i64 = sqlx::query_scalar("SELECT count(*) FROM ci_relationships WHERE deleted_at IS NULL")
        .fetch_one(pool)
        .await
        .unwrap();
    eprintln!("seeded {edges} relationships in {:?}", t.elapsed());
    assert!(edges >= 295_000, "{edges} relationships");

    let id = |n: i64| async move {
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM perf_ci WHERE n = $1").bind(n).fetch_one(pool).await.unwrap()
    };
    Seeded { classes, hub: id(1).await, chain_start: id(60001).await, scc_member: id(30001).await }
}

/// Visible roots with at least one propagating relationship, spread over the data set.
async fn roots(pool: &PgPool, visible: &[Uuid], n: i64) -> Vec<Uuid> {
    sqlx::query_scalar(
        "SELECT p.id FROM perf_ci p WHERE p.class_id = ANY($1) AND p.n > 1
           AND EXISTS (SELECT 1 FROM ci_relationships r WHERE r.target_ci_id = p.id)
         ORDER BY md5(p.n::text) LIMIT $2",
    )
    .bind(visible)
    .bind(n)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn timed(pool: &PgPool, ctx: &RequestContext, root: Uuid, q: &ImpactQuery) -> (f64, Value) {
    let state = Arc::new(ImpactState::default());
    let t = Instant::now();
    let a = service::analyse(pool, ctx, &state, root, q).await.unwrap_or_else(|e| panic!("{e:?}"));
    let body = serde_json::to_value(&a.result).unwrap();
    (t.elapsed().as_secs_f64() * 1000.0, body)
}

#[tokio::test]
#[ignore = "performance gate: seeds 100k CIs; run with --ignored (tools/perf/impact.sh)"]
async fn impact_performance() {
    let Some(db) = scratch::database("impact_performance").await else { return };
    let pool = &db.pool;
    let s = seed(pool).await;
    let visible: Vec<Uuid> = s.classes[..CLASSES - HIDDEN_CLASSES].to_vec();
    let restricted = viewer(&visible);
    let admin = RequestContext::system("perf", "impact-perf");
    let mut report = Vec::new();
    let mut failures = Vec::new();
    let version: String = sqlx::query_scalar("SHOW server_version").fetch_one(pool).await.unwrap();
    report.push(format!("PostgreSQL {version}; {CIS} CIs, ~300k relationships, 30% hidden from the test profile"));

    let sample = roots(pool, &visible, 200).await;
    // Warm the cache once.
    for r in sample.iter().take(20) {
        timed(pool, &restricted, *r, &query(3, 500)).await;
    }
    for (label, ctx) in [("restricted", &restricted), ("admin", &admin)] {
        for (name, q, n, limit) in [
            ("defaults (depth 3, 500 nodes)", query(3, 500), 200, 500.0),
            ("maximum (depth 10, 2000 nodes)", query(10, 2000), 50, 1500.0),
        ] {
            let mut ms = Vec::new();
            let mut items = 0usize;
            let mut truncated = 0;
            for r in sample.iter().take(n) {
                let (t, body) = timed(pool, ctx, *r, &q).await;
                ms.push(t);
                items += body["items"].as_array().unwrap().len();
                truncated += usize::from(body["truncated"] == true);
            }
            let (p50, p95, max) = p95(ms);
            report.push(format!(
                "{label} {name}: {n} roots, p50 {p50:.1} ms, p95 {p95:.1} ms, max {max:.1} ms, avg {:.0} items, {truncated} truncated (threshold p95 < {limit} ms)",
                items as f64 / n as f64
            ));
            if p95 >= limit {
                failures.push(format!("{label} {name}: p95 {p95:.1} ms >= {limit} ms"));
            }
        }
    }

    // The hub answers max_edges quickly.
    for (label, ctx) in [("restricted", &restricted), ("admin", &admin)] {
        let mut ms = Vec::new();
        for _ in 0..10 {
            let (t, body) = timed(pool, ctx, s.hub, &query(3, 500)).await;
            assert_eq!(body["truncatedReason"], json!(TruncatedReason::MaxEdges), "{label} hub");
            ms.push(t);
        }
        let (p50, p95, max) = p95(ms);
        report.push(format!("{label} hub (50k relationships): max_edges, p50 {p50:.1} ms, p95 {p95:.1} ms, max {max:.1} ms (threshold < 300 ms)"));
        if max >= 300.0 {
            failures.push(format!("{label} hub: {max:.1} ms >= 300 ms"));
        }
    }

    // The chain and the strongly connected component terminate and stay bounded.
    let (t, body) = timed(pool, &restricted, s.chain_start, &query(10, 2000)).await;
    report.push(format!(
        "chain of 1000 from its head, depth 10: {} items, hasMoreBeyondDepth {}, {t:.1} ms",
        body["items"].as_array().unwrap().len(),
        body["hasMoreBeyondDepth"]
    ));
    assert!(body["hasMoreBeyondDepth"] == true);
    let (t, body) = timed(pool, &admin, s.scc_member, &query(10, 2000)).await;
    report.push(format!(
        "500-node strongly connected component, depth 10: {} items, truncated {} ({}), {t:.1} ms",
        body["items"].as_array().unwrap().len(),
        body["truncated"],
        body["truncatedReason"]
    ));

    // 16 parallel callers (distinct users) at the maximum: within the caps, no error, nothing slower than 5 s.
    let state = Arc::new(ImpactState::default());
    let started = Instant::now();
    let mut tasks = Vec::new();
    for i in 0..16usize {
        let (pool, state, ctx) = (pool.clone(), state.clone(), viewer(&visible));
        let my_roots: Vec<Uuid> = sample.iter().skip(i * 4).take(4).copied().collect();
        tasks.push(tokio::spawn(async move {
            let mut out = Vec::new();
            for r in my_roots {
                let t = Instant::now();
                let res = service::analyse(&pool, &ctx, &state, r, &query(10, 2000)).await;
                out.push((t.elapsed(), res.map(|_| ()).map_err(|e| e.code)));
            }
            out
        }));
    }
    let (mut ok, mut busy, mut slowest) = (0, 0, Duration::ZERO);
    for t in tasks {
        for (d, r) in t.await.unwrap() {
            slowest = slowest.max(d);
            match r {
                Ok(()) => ok += 1,
                Err(ErrorCode::ServerBusy) => busy += 1,
                Err(e) => failures.push(format!("parallel caller: unexpected {e:?}")),
            }
        }
    }
    report.push(format!(
        "16 parallel callers x 4 analyses at the maximum: {ok} answered, {busy} refused 503 (IMPACT_MAX_CONCURRENT 8), slowest {slowest:?}, wall {:?}",
        started.elapsed()
    ));
    if slowest >= Duration::from_secs(5) {
        failures.push(format!("parallel: an analysis took {slowest:?}"));
    }

    // EXPLAIN: index scans on the relationship indexes, no sequential scan of ci_relationships.
    let mut conn = pool.acquire().await.unwrap();
    let all_types = data::types(&mut conn).await.unwrap();
    let frontier: Vec<Uuid> = sample.iter().take(50).copied().collect();
    for (label, way) in [("downstream", Way::Downstream), ("upstream", Way::Upstream)] {
        let types = hop_types(&all_types, None, way);
        let reach = data::Reach { visible: Some(&visible), include_inactive: true };
        let plan = data::explain_hop(&mut conn, &frontier, &frontier, &types, reach, 2501).await.unwrap();
        let text = plan.to_string();
        let seq_on_edges = plan_has_seq_scan(&plan, "ci_relationships");
        let index_names: Vec<String> = index_names(&plan);
        report.push(format!(
            "EXPLAIN {label} hop: indexes {index_names:?}, seq scan on ci_relationships: {seq_on_edges}"
        ));
        if seq_on_edges {
            failures.push(format!("{label} hop: sequential scan on ci_relationships: {text}"));
        }
    }
    drop(conn);

    eprintln!("\n=== Impact analysis performance (spec §6.2) ===");
    for line in &report {
        eprintln!("- {line}");
    }
    db.drop().await;
    assert!(failures.is_empty(), "thresholds missed: {failures:#?}");
}

pub(crate) fn plan_has_seq_scan(v: &Value, relation: &str) -> bool {
    match v {
        Value::Object(m) => {
            (m.get("Node Type").and_then(Value::as_str) == Some("Seq Scan")
                && m.get("Relation Name").and_then(Value::as_str) == Some(relation))
                || m.values().any(|x| plan_has_seq_scan(x, relation))
        }
        Value::Array(a) => a.iter().any(|x| plan_has_seq_scan(x, relation)),
        _ => false,
    }
}

pub(crate) fn index_names(v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Object(m) => {
                if let Some(n) = m.get("Index Name").and_then(Value::as_str)
                    && !out.iter().any(|o| o == n)
                {
                    out.push(n.to_owned());
                }
                m.values().for_each(|x| walk(x, out));
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    walk(v, &mut out);
    out
}
