//! Business services performance gate (SHAA-927 §7.3). Not part of the
//! normal test run: it seeds the impact analysis data set (100 000 CIs,
//! 300 000 relationships, `impact::perf`) and adds 2 000 services.
//!
//! ```sh
//! SHADOUCMDB_TEST_DATABASE_URL=postgres://… cargo test --release business_services_performance -- --ignored --nocapture
//! ```
//! (`tools/perf/business-services.sh` wraps it.) It prints the measurements
//! and fails when a threshold is missed.
//!
//! On top of the impact data set: 2 000 services with 10 members each (CIs of
//! every class, 30 % of them hidden from the test profile), one service with
//! 5 000 members, one CI that is a member of 300 services, 50 chains of
//! services nested 5 levels deep, and an owner on every service.

use std::sync::Arc;
use std::time::Instant;

use sqlx::PgPool;
use uuid::Uuid;

use super::schemas::{BusinessServiceMembersAdd, BusinessServiceQuery, MemberQuery};
use super::service;
use crate::api::context::RequestContext;
use crate::api::schemas::{QueryBool, Sort};
use crate::auth::permissions::{ClassRights, Permissions};
use crate::auth::{Credential, Principal};
use crate::config::BusinessServiceConfig;
use crate::data::business_services::{self as data, ServiceFilters, ServiceSort};
use crate::data::items::{ActiveFilter, ItemFilters};
use crate::db::scratch;
use crate::modules::impact::ImpactState;
use crate::modules::impact::perf::{self as ia, CLASSES, HIDDEN_CLASSES, exec, index_names, p95, plan_has_seq_scan};
use crate::schema::model::Model;

const SERVICES: i64 = 2_000;
const MEMBERS_EACH: i64 = 10;
const BIG: i64 = 5_000;
const SHARED_BY: i64 = 300;
const CHAINS: i64 = 50;
const CHAIN_DEPTH: i64 = 5;

/// May view the visible impact classes and the services, and edit services.
fn restricted(visible: &[Uuid], service_class: Uuid) -> RequestContext {
    let mut classes: std::collections::BTreeMap<Uuid, ClassRights> =
        visible.iter().map(|id| (*id, ClassRights { view: true, ..Default::default() })).collect();
    classes.insert(service_class, ClassRights { view: true, edit: true, ..Default::default() });
    let permissions = Permissions { classes, ..Default::default() };
    let principal = Principal {
        user_id: Uuid::new_v4(),
        username: "perf".into(),
        credential: Credential::Token { profile_id: None, creator_id: None },
        permissions,
    };
    RequestContext::user(Arc::new(principal), "services-perf".into())
}

async fn reconcile(pool: &PgPool) {
    let ctx = RequestContext::system("perf", "perf");
    let mut tx = pool.begin().await.unwrap();
    crate::schema::reconcile(&mut tx, &ctx, "perf").await.unwrap_or_else(|e| panic!("reconcile: {}", e.message));
    tx.commit().await.unwrap();
}

struct Seeded {
    roles: data::Roles,
    big: Uuid,
    shared_member: Uuid,
    /// Services with no members yet, for the add measurements.
    empty: Vec<Uuid>,
}

async fn seed(pool: &PgPool) -> Seeded {
    let t = Instant::now();
    let roles = data::roles(&mut pool.acquire().await.unwrap()).await.unwrap().unwrap();
    let model = Model::load(&mut pool.acquire().await.unwrap()).await.unwrap();
    let table = model.table(roles.service_class).unwrap();
    let name = model.own_fields(roles.service_class).find(|f| f.key == "name").unwrap().column();

    // Services n = 1..SERVICES, a third without criticality.
    exec(pool, "CREATE TABLE perf_svc (n bigint PRIMARY KEY, id uuid NOT NULL)").await;
    sqlx::query("INSERT INTO perf_svc (n, id) SELECT n, gen_random_uuid() FROM generate_series(1, $1) n")
        .bind(SERVICES)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO configuration_items (id, class_id, ident, label, valid_from, criticality_value_id)
         SELECT s.id, $1, 'SVC-' || s.n, 'service ' || lpad(s.n::text, 5, '0'), now() - interval '1 day',
                CASE WHEN s.n % 3 = 0 THEN NULL ELSE (SELECT v.id FROM lookup_list_values v JOIN lookup_lists l ON l.id = v.list_id
                     WHERE l.system_role = 'criticality' ORDER BY md5(v.key || s.n) LIMIT 1) END
         FROM perf_svc s ORDER BY s.n",
    )
    .bind(roles.service_class)
    .execute(pool)
    .await
    .unwrap();
    exec(
        pool,
        &format!(
            "INSERT INTO {} (id, {name}) SELECT id, 'service ' || lpad(n::text, 5, '0') FROM perf_svc",
            table.sql()
        ),
    )
    .await;
    let owner: Uuid =
        sqlx::query_scalar("SELECT id FROM users ORDER BY created_at LIMIT 1").fetch_one(pool).await.unwrap();
    sqlx::query(
        "INSERT INTO business_service_owners (service_ci_id, role, user_id, position) SELECT id, 'technical', $1, 0 FROM perf_svc",
    )
    .bind(owner)
    .execute(pool)
    .await
    .unwrap();

    // 10 members each (any class), for services 2..1000 and 1301..2000 (1001..1300 nest, 1501..1520 stay empty).
    sqlx::query(
        "INSERT INTO ci_relationships (relationship_type_id, source_ci_id, target_ci_id)
         SELECT DISTINCT $1, s.id, c.id FROM perf_svc s
         CROSS JOIN generate_series(1, $2) k
         JOIN perf_ci c ON c.n = 1 + (('x' || substr(md5(s.n || '-' || k), 1, 8))::bit(32)::bigint % 100000)
         WHERE s.n BETWEEN 2 AND 1000 OR (s.n > 1300 AND s.n NOT BETWEEN 1501 AND 1520)",
    )
    .bind(roles.member_type)
    .bind(MEMBERS_EACH)
    .execute(pool)
    .await
    .unwrap();
    // Service 1: 5 000 members.
    sqlx::query(
        "INSERT INTO ci_relationships (relationship_type_id, source_ci_id, target_ci_id)
         SELECT $1, s.id, c.id FROM perf_svc s, (SELECT id FROM perf_ci ORDER BY md5(n::text) LIMIT $2) c WHERE s.n = 1",
    )
    .bind(roles.member_type)
    .bind(BIG)
    .execute(pool)
    .await
    .unwrap();
    // CI 50 (a visible class) is a member of 300 services.
    sqlx::query(
        "INSERT INTO ci_relationships (relationship_type_id, source_ci_id, target_ci_id)
         SELECT $1, s.id, c.id FROM perf_svc s, perf_ci c
         WHERE c.n = 50 AND s.n BETWEEN 2 AND 1 + $2
           AND NOT EXISTS (SELECT 1 FROM ci_relationships r WHERE r.source_ci_id = s.id AND r.target_ci_id = c.id)",
    )
    .bind(roles.member_type)
    .bind(SHARED_BY)
    .execute(pool)
    .await
    .unwrap();
    // 50 chains of 6 services (1001..1300): each includes the next, 5 levels deep.
    for level in 0..CHAIN_DEPTH {
        sqlx::query(
            "INSERT INTO ci_relationships (relationship_type_id, source_ci_id, target_ci_id)
             SELECT $1, a.id, b.id FROM perf_svc a JOIN perf_svc b ON b.n = a.n + 1
             WHERE a.n BETWEEN 1001 AND 1000 + $2 * ($3 + 1) AND (a.n - 1001) % ($3 + 1) = $4",
        )
        .bind(roles.member_type)
        .bind(CHAINS)
        .bind(CHAIN_DEPTH)
        .bind(CHAIN_DEPTH - 1 - level)
        .execute(pool)
        .await
        .unwrap();
    }
    exec(pool, "ANALYZE").await;
    let edges: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ci_relationships WHERE relationship_type_id = $1 AND deleted_at IS NULL",
    )
    .bind(roles.member_type)
    .fetch_one(pool)
    .await
    .unwrap();
    eprintln!("seeded {SERVICES} services and {edges} memberships in {:?}", t.elapsed());

    let svc = |n: i64| async move {
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM perf_svc WHERE n = $1").bind(n).fetch_one(pool).await.unwrap()
    };
    let mut empty = Vec::new();
    for n in 1501..=1520 {
        empty.push(svc(n).await);
    }
    let shared_member: Uuid = sqlx::query_scalar("SELECT id FROM perf_ci WHERE n = 50").fetch_one(pool).await.unwrap();
    Seeded { roles, big: svc(1).await, shared_member, empty }
}

fn list_query(offset: i64, sort: &str) -> BusinessServiceQuery {
    let desc = sort.starts_with('-');
    BusinessServiceQuery {
        limit: 50,
        offset,
        q: None,
        criticality_value_id: None,
        owner_id: None,
        owner_role: None,
        mine: None,
        owner_state: None,
        include_inactive: Some(QueryBool::True),
        sort: Sort { field: sort.trim_start_matches('-').into(), desc },
    }
}

fn member_query(offset: i64) -> MemberQuery {
    MemberQuery {
        limit: 50,
        offset,
        q: None,
        class_id: None,
        kind: None,
        ci_id: None,
        sort: Sort { field: "name".into(), desc: false },
    }
}

#[tokio::test]
#[ignore = "performance gate: seeds 100k CIs and 2000 services; run with --ignored (tools/perf/business-services.sh)"]
async fn business_services_performance() {
    let Some(db) = scratch::database("business_services_performance").await else { return };
    let pool = &db.pool;
    reconcile(pool).await;
    let base = ia::seed(pool).await;
    let s = seed(pool).await;
    let visible: Vec<Uuid> = base.classes[..CLASSES - HIDDEN_CLASSES].to_vec();
    let restricted = restricted(&visible, s.roles.service_class);
    let admin = RequestContext::system("perf", "services-perf");
    let cfg = BusinessServiceConfig::default();
    let mut report = Vec::new();
    let mut failures = Vec::new();
    let version: String = sqlx::query_scalar("SHOW server_version").fetch_one(pool).await.unwrap();
    report.push(format!(
        "PostgreSQL {version}; impact data set plus {SERVICES} services, one with {BIG} members, a CI in {SHARED_BY} \
         services, {CHAINS} chains nested {CHAIN_DEPTH} deep"
    ));
    let mut check = |name: &str, ms: Vec<f64>, limit: f64, extra: String| {
        let n = ms.len();
        let (p50, p95, max) = p95(ms);
        report.push(format!(
            "{name}: {n} runs, p50 {p50:.1} ms, p95 {p95:.1} ms, max {max:.1} ms{extra} (threshold p95 < {limit} ms)"
        ));
        if p95 >= limit {
            failures.push(format!("{name}: p95 {p95:.1} ms >= {limit} ms"));
        }
    };

    // The service list at the defaults (50 per page), sorted by member count, and by criticality.
    for (label, ctx) in [("restricted", &restricted), ("admin", &admin)] {
        for sort in ["-memberCount", "criticality"] {
            let mut ms = Vec::new();
            let mut total = 0;
            for i in 0..60 {
                let t = Instant::now();
                let page = service::list(pool, ctx, &list_query((i * 97) % SERVICES, sort)).await.unwrap();
                if i >= 10 {
                    ms.push(t.elapsed().as_secs_f64() * 1000.0);
                }
                total = page.page.total;
            }
            check(&format!("{label} service list, sort={sort}"), ms, 300.0, format!(", total {total}"));
        }
    }

    // A page of the 5 000-member service.
    for (label, ctx) in [("restricted", &restricted), ("admin", &admin)] {
        let mut ms = Vec::new();
        let mut total = 0;
        for i in 0..60 {
            let t = Instant::now();
            let page = service::members(pool, ctx, s.big, &member_query((i * 53 * 50) % BIG)).await.unwrap();
            if i >= 10 {
                ms.push(t.elapsed().as_secs_f64() * 1000.0);
            }
            total = page.page.total;
        }
        check(&format!("{label} member list page (5000-member service)"), ms, 200.0, format!(", total {total}"));
    }

    // Adding 500 members at once, to 20 empty services (visible CIs not yet members).
    let mut ms = Vec::new();
    for (i, svc) in s.empty.iter().enumerate() {
        let ids: Vec<Uuid> =
            sqlx::query_scalar("SELECT id FROM perf_ci WHERE class_id = ANY($1) ORDER BY md5(n::text || $2) LIMIT 500")
                .bind(&visible)
                .bind(i.to_string())
                .fetch_all(pool)
                .await
                .unwrap();
        let t = Instant::now();
        let added = service::add_members(pool, &restricted, cfg, *svc, &BusinessServiceMembersAdd { member_ids: ids })
            .await
            .unwrap_or_else(|e| panic!("{e:?}"));
        ms.push(t.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(added.added.len(), 500);
    }
    check("restricted add 500 members", ms, 1000.0, String::new());

    // "Part of" for a CI in 300 services (truncated at 200).
    let impact = Arc::new(ImpactState::default());
    for (label, ctx) in [("restricted", &restricted), ("admin", &admin)] {
        let mut ms = Vec::new();
        let mut shown = (0, false);
        for i in 0..60 {
            let t = Instant::now();
            let r = service::part_of(pool, ctx, &impact, cfg, s.shared_member).await.unwrap();
            if i >= 10 {
                ms.push(t.elapsed().as_secs_f64() * 1000.0);
            }
            shown = (r.data.len(), r.truncated);
        }
        assert_eq!(shown, (200, true), "{label}");
        check(&format!("{label} part-of (CI in 300 services)"), ms, 300.0, format!(", {} shown, truncated", shown.0));
    }

    // EXPLAIN: no sequential scan of ci_relationships.
    let mut conn = pool.acquire().await.unwrap();
    let mut visible_with_services = visible.clone();
    visible_with_services.push(s.roles.service_class);
    let filters = ServiceFilters {
        items: ItemFilters {
            class_ids: Some(vec![s.roles.service_class]),
            active: ActiveFilter::Any,
            deleted: Some(crate::api::schemas::Deleted::Exclude),
            ..Default::default()
        },
        ..Default::default()
    };
    let plans = [
        (
            "service list sort=-memberCount",
            data::explain_services(
                &mut conn,
                s.roles,
                Some(&visible_with_services),
                &filters,
                ServiceSort::MemberCount,
                true,
                50,
            )
            .await
            .unwrap(),
        ),
        (
            "member list page",
            data::explain_members(&mut conn, s.roles, s.big, Some(&visible_with_services), 50, 2500).await.unwrap(),
        ),
    ];
    for (label, plan) in plans {
        let seq = plan_has_seq_scan(&plan, "ci_relationships");
        report.push(format!("EXPLAIN {label}: indexes {:?}, seq scan on ci_relationships: {seq}", index_names(&plan)));
        if seq {
            failures.push(format!("{label}: sequential scan on ci_relationships: {plan}"));
        }
    }
    drop(conn);

    eprintln!("\n=== Business services performance (spec §7.3) ===");
    for line in &report {
        eprintln!("- {line}");
    }
    db.drop().await;
    assert!(failures.is_empty(), "thresholds missed: {failures:#?}");
}
