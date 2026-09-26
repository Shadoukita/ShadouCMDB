//! Schema acceptance checks against a migrated and seeded database.
//!
//! Everything runs inside one transaction that is ROLLED BACK, so this is safe to
//! point at any environment: it leaves no rows behind.

use anyhow::{Context, anyhow, bail};
use sqlx::postgres::PgConnection;
use sqlx::{Connection, Executor};
use uuid::Uuid;

use crate::config::DatabaseConfig;

const CHECKS: &[&str] = &[
    "New CI class with custom attributes is pure data entry",
    "Server -> Application -> Database and Device -> Location are expressible",
    "Demo graph traversal (recursive, downstream of the CRM service)",
    "Guard: self-edge",
    "Guard: duplicate edge",
    "Guard: reverse duplicate of non-directional connected_to",
    "Guard: illegal endpoint classes (database located_in location)",
    "Guard: CI of an abstract class",
    "Guard: attribute from another class",
    "Guard: value stored in the wrong type column",
    "Guard: enum value not in the allowed list",
    "Guard: reference attribute pointing at the wrong class",
    "Guard: class hierarchy cycle",
    "Guard: unknown status (foreign key)",
    "Guard: audit_log is append-only",
    "Soft delete: removed edge can be re-created; deleted CI cannot be linked",
    "Indexes used by the UI queries",
];

async fn id_by_key(c: &mut PgConnection, table: &'static str, key: &str) -> anyhow::Result<Uuid> {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT id FROM {table} WHERE key = $1")))
        .bind(key)
        .fetch_optional(c)
        .await?
        .ok_or_else(|| anyhow!("no row in {table} with key {key}"))
}

async fn attribute_id(c: &mut PgConnection, class: &str, key: &str) -> anyhow::Result<Uuid> {
    sqlx::query_scalar(
        "SELECT d.id FROM ci_attribute_definitions d JOIN ci_classes k ON k.id = d.class_id WHERE k.key = $1 AND d.key = $2",
    )
    .bind(class)
    .bind(key)
    .fetch_optional(c)
    .await?
    .ok_or_else(|| anyhow!("no attribute {class}.{key}"))
}

async fn new_ci(c: &mut PgConnection, class: &str, name: &str) -> sqlx::Result<Uuid> {
    sqlx::query_scalar(
        "INSERT INTO configuration_items (class_id, name, status_id)
         VALUES ((SELECT id FROM ci_classes WHERE key = $1), $2, (SELECT id FROM statuses WHERE key = 'in_service'))
         RETURNING id",
    )
    .bind(class)
    .bind(name)
    .fetch_one(c)
    .await
}

async fn link(c: &mut PgConnection, ty: &str, src: Uuid, tgt: Uuid) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO ci_relationships (relationship_type_id, source_ci_id, target_ci_id)
         VALUES ((SELECT id FROM relationship_types WHERE key = $1), $2, $3)",
    )
    .bind(ty)
    .bind(src)
    .bind(tgt)
    .execute(c)
    .await
    .map(|_| ())
}

/// Turns the result of a statement that must fail with `constraint` into a
/// check result. The caller wraps the statement in `SAVEPOINT sp`.
async fn expect_reject(c: &mut PgConnection, constraint: &str, result: sqlx::Result<()>) -> anyhow::Result<String> {
    match result {
        Ok(()) => bail!("expected {constraint} violation, but statement succeeded"),
        Err(err) => {
            c.execute("ROLLBACK TO SAVEPOINT sp").await?;
            let Some(db) = err.as_database_error() else { return Err(err.into()) };
            let code = db.code().unwrap_or_default().into_owned();
            if db.constraint() == Some(constraint) || db.message().contains(constraint) {
                Ok(format!("rejected ({code} {constraint})"))
            } else {
                bail!("expected {constraint}, got {code} {}: {}", db.constraint().unwrap_or(""), db.message())
            }
        }
    }
}

/// Runs `$stmt` inside `SAVEPOINT sp` and expects it to violate `$constraint`.
macro_rules! reject {
    ($c:expr, $constraint:expr, $stmt:expr) => {{
        $c.execute("SAVEPOINT sp").await?;
        let result = $stmt.await.map(|_| ());
        expect_reject($c, $constraint, result).await
    }};
}

async fn run_check(i: usize, c: &mut PgConnection) -> anyhow::Result<String> {
    match i {
        0 => {
            let parent = id_by_key(c, "ci_classes", "network_device").await?;
            let lb: Uuid = sqlx::query_scalar(
                "INSERT INTO ci_classes (key, name, parent_id) VALUES ('load_balancer', 'Load balancer', $1) RETURNING id",
            )
            .bind(parent)
            .fetch_one(&mut *c)
            .await?;
            let vip: Uuid = sqlx::query_scalar(
                "INSERT INTO ci_attribute_definitions (class_id, key, label, data_type) VALUES ($1, 'vip', 'Virtual IP', 'ip') RETURNING id",
            )
            .bind(lb)
            .fetch_one(&mut *c)
            .await?;
            let algo: Uuid = sqlx::query_scalar(
                "INSERT INTO ci_attribute_definitions (class_id, key, label, data_type, enum_values)
                 VALUES ($1, 'algorithm', 'Algorithm', 'enum', '[\"round_robin\",\"least_conn\"]') RETURNING id",
            )
            .bind(lb)
            .fetch_one(&mut *c)
            .await?;
            let ci = new_ci(c, "load_balancer", "fra1-lb-01").await?;
            let role = attribute_id(c, "network_device", "device_role").await?;
            sqlx::query(
                "INSERT INTO ci_attribute_values (ci_id, attribute_id, value_ip) VALUES ($1, $2, '192.0.2.10')",
            )
            .bind(ci)
            .bind(vip)
            .execute(&mut *c)
            .await?;
            sqlx::query(
                "INSERT INTO ci_attribute_values (ci_id, attribute_id, value_text) VALUES ($1, $2, 'least_conn')",
            )
            .bind(ci)
            .bind(algo)
            .execute(&mut *c)
            .await?;
            // Inherited from network_device, which inherits from hardware.
            sqlx::query(
                "INSERT INTO ci_attribute_values (ci_id, attribute_id, value_text) VALUES ($1, $2, 'load_balancer')",
            )
            .bind(ci)
            .bind(role)
            .execute(&mut *c)
            .await?;
            let attrs: Vec<String> = sqlx::query_scalar(
                "SELECT d.key FROM ci_class_lineage($1) l JOIN ci_attribute_definitions d ON d.class_id = l.class_id
                 ORDER BY l.depth, d.sort_order, d.key",
            )
            .bind(lb)
            .fetch_all(&mut *c)
            .await?;
            // Inherited relationship rules apply too: a load balancer is hardware, so it can be located_in a location.
            let rack: Uuid = sqlx::query_scalar("SELECT id FROM configuration_items WHERE name = 'FRA1 Rack A01'")
                .fetch_one(&mut *c)
                .await
                .context("demo CI 'FRA1 Rack A01' not found; run `seed --demo` first")?;
            link(c, "located_in", ci, rack).await?;
            Ok(format!(
                "class load_balancer + 2 attributes inserted; effective attributes: {}; located_in rack accepted via inherited rule",
                attrs.join(", ")
            ))
        }
        1 => {
            let srv = new_ci(c, "server", "verify-srv").await?;
            let app = new_ci(c, "application", "verify-app").await?;
            let db = new_ci(c, "database", "verify-db").await?;
            let dev = new_ci(c, "network_device", "verify-switch").await?;
            let loc = new_ci(c, "location", "verify-room").await?;
            link(c, "runs_on", app, srv).await?;
            link(c, "depends_on", app, db).await?;
            link(c, "runs_on", db, srv).await?;
            link(c, "located_in", dev, loc).await?;
            let paths: Vec<String> = sqlx::query_scalar(
                "SELECT s.name || ' <-runs_on- ' || a.name || ' -depends_on-> ' || d.name
                 FROM ci_relationships r1
                 JOIN relationship_types t1 ON t1.id = r1.relationship_type_id AND t1.key = 'runs_on'
                 JOIN ci_relationships r2 ON r2.source_ci_id = r1.source_ci_id
                 JOIN relationship_types t2 ON t2.id = r2.relationship_type_id AND t2.key = 'depends_on'
                 JOIN configuration_items s ON s.id = r1.target_ci_id
                 JOIN configuration_items a ON a.id = r1.source_ci_id
                 JOIN configuration_items d ON d.id = r2.target_ci_id
                 WHERE a.id = $1",
            )
            .bind(app)
            .fetch_all(&mut *c)
            .await?;
            Ok(paths.join("; ") + "; verify-switch -located_in-> verify-room")
        }
        2 => {
            let lines: Vec<String> = sqlx::query_scalar(
                "WITH RECURSIVE g AS (
                   SELECT r.source_ci_id, r.target_ci_id, r.relationship_type_id, 1 AS depth
                   FROM ci_relationships r JOIN configuration_items ci ON ci.id = r.source_ci_id
                   WHERE ci.name = 'Customer Relationship Management' AND r.deleted_at IS NULL
                   UNION
                   SELECT r.source_ci_id, r.target_ci_id, r.relationship_type_id, g.depth + 1
                   FROM ci_relationships r JOIN g ON r.source_ci_id = g.target_ci_id
                   WHERE r.deleted_at IS NULL AND g.depth < 6
                 )
                 SELECT s.name || ' ' || t.forward_label || ' ' || d.name AS line
                 FROM g JOIN configuration_items s ON s.id = g.source_ci_id
                 JOIN configuration_items d ON d.id = g.target_ci_id
                 JOIN relationship_types t ON t.id = g.relationship_type_id
                 GROUP BY line ORDER BY min(g.depth), line",
            )
            .fetch_all(&mut *c)
            .await?;
            Ok(lines.join(" | "))
        }
        3 => {
            let a = new_ci(c, "application", "self-edge-app").await?;
            reject!(c, "ci_relationships_no_self_edge", link(c, "depends_on", a, a))
        }
        4 => {
            let a = new_ci(c, "application", "dup-app").await?;
            let d = new_ci(c, "database", "dup-db").await?;
            link(c, "depends_on", a, d).await?;
            reject!(c, "ci_relationships_live_edge_uq", link(c, "depends_on", a, d))
        }
        5 => {
            let s = new_ci(c, "server", "conn-srv").await?;
            let n = new_ci(c, "network_device", "conn-sw").await?;
            link(c, "connected_to", s, n).await?;
            reject!(c, "ci_relationships_live_edge_uq", link(c, "connected_to", n, s))
        }
        6 => {
            let d = new_ci(c, "database", "rule-db").await?;
            let l = new_ci(c, "location", "rule-loc").await?;
            reject!(c, "ci_relationships_endpoint_rule", link(c, "located_in", d, l))
        }
        7 => reject!(c, "configuration_items_class_concrete", new_ci(c, "hardware", "abstract-ci")),
        8 => {
            let a = new_ci(c, "application", "attr-app").await?;
            let def = attribute_id(c, "database", "engine_version").await?;
            reject!(
                c,
                "ci_attribute_values_attribute_in_class",
                sqlx::query("INSERT INTO ci_attribute_values (ci_id, attribute_id, value_text) VALUES ($1, $2, '1.0')")
                    .bind(a)
                    .bind(def)
                    .execute(&mut *c)
            )
        }
        9 => {
            let s = new_ci(c, "server", "type-srv").await?;
            let def = attribute_id(c, "server", "cpu_cores").await?;
            reject!(
                c,
                "ci_attribute_values_type_match",
                sqlx::query(
                    "INSERT INTO ci_attribute_values (ci_id, attribute_id, value_text) VALUES ($1, $2, 'lots')"
                )
                .bind(s)
                .bind(def)
                .execute(&mut *c)
            )
        }
        10 => {
            let s = new_ci(c, "server", "enum-srv").await?;
            let def = attribute_id(c, "server", "os_family").await?;
            reject!(
                c,
                "ci_attribute_values_enum",
                sqlx::query(
                    "INSERT INTO ci_attribute_values (ci_id, attribute_id, value_text) VALUES ($1, $2, 'amiga')"
                )
                .bind(s)
                .bind(def)
                .execute(&mut *c)
            )
        }
        11 => {
            let a = new_ci(c, "application", "ref-app").await?;
            let s = new_ci(c, "server", "ref-srv").await?;
            let def = attribute_id(c, "application", "primary_database").await?;
            reject!(
                c,
                "ci_attribute_values_reference_class",
                sqlx::query(
                    "INSERT INTO ci_attribute_values (ci_id, attribute_id, value_ref_ci_id) VALUES ($1, $2, $3)"
                )
                .bind(a)
                .bind(def)
                .bind(s)
                .execute(&mut *c)
            )
        }
        12 => {
            let hw = id_by_key(c, "ci_classes", "hardware").await?;
            let srv = id_by_key(c, "ci_classes", "server").await?;
            reject!(
                c,
                "ci_classes_no_cycle",
                sqlx::query("UPDATE ci_classes SET parent_id = $1 WHERE id = $2").bind(srv).bind(hw).execute(&mut *c)
            )
        }
        13 => reject!(
            c,
            "configuration_items_status_id_statuses_id_fk",
            sqlx::query(
                "INSERT INTO configuration_items (class_id, name, status_id)
                 VALUES ((SELECT id FROM ci_classes WHERE key = 'server'), 'fk-srv', gen_random_uuid())",
            )
            .execute(&mut *c)
        ),
        14 => reject!(
            c,
            "audit_log is append-only",
            sqlx::query("UPDATE audit_log SET actor_name = 'tampered'").execute(&mut *c)
        ),
        15 => {
            let a = new_ci(c, "application", "soft-app").await?;
            let d = new_ci(c, "database", "soft-db").await?;
            link(c, "depends_on", a, d).await?;
            sqlx::query("UPDATE ci_relationships SET deleted_at = now() WHERE source_ci_id = $1")
                .bind(a)
                .execute(&mut *c)
                .await?;
            link(c, "depends_on", a, d).await?;
            sqlx::query("UPDATE configuration_items SET deleted_at = now() WHERE id = $1")
                .bind(d)
                .execute(&mut *c)
                .await?;
            let s = new_ci(c, "server", "soft-srv").await?;
            let res = reject!(c, "ci_relationships_live_endpoints", link(c, "runs_on", d, s))?;
            Ok(format!("edge re-created after soft delete; linking deleted CI {res}"))
        }
        16 => {
            c.execute("SET LOCAL enable_seqscan = off").await?;
            // Any of the listed indexes is acceptable (on tiny tables PG18 may prefer a skip scan of a wider index).
            let probes: [(&'static str, &[&str]); 5] = [
                ("SELECT id FROM configuration_items WHERE name ILIKE '%crm%'", &["configuration_items_name_trgm_idx"]),
                (
                    "SELECT id FROM configuration_items WHERE search_vector @@ plainto_tsquery('simple', 'crm')",
                    &["configuration_items_search_idx"],
                ),
                (
                    "SELECT id FROM configuration_items WHERE deleted_at IS NULL ORDER BY lower(name), id LIMIT 50",
                    &["configuration_items_live_name_idx"],
                ),
                (
                    "SELECT target_ci_id FROM ci_relationships WHERE source_ci_id = '00000000-0000-0000-0000-000000000000' AND deleted_at IS NULL",
                    &["ci_relationships_source_idx", "ci_relationships_live_edge_uq"],
                ),
                (
                    "SELECT id FROM configuration_items WHERE ip_address << '10.0.0.0/8'",
                    &["configuration_items_ip_idx"],
                ),
            ];
            let mut hits = Vec::new();
            for (q, indexes) in probes {
                let plan: Vec<String> =
                    sqlx::query_scalar(sqlx::AssertSqlSafe(format!("EXPLAIN {q}"))).fetch_all(&mut *c).await?;
                let plan = plan.join(" ");
                let hit = indexes
                    .iter()
                    .find(|i| plan.contains(*i))
                    .ok_or_else(|| anyhow!("planner used none of {}: {plan}", indexes.join(", ")))?;
                hits.push(*hit);
            }
            Ok(hits.join(", "))
        }
        _ => unreachable!("unknown check {i}"),
    }
}

pub async fn run(cfg: &DatabaseConfig) -> anyhow::Result<()> {
    let mut conn = PgConnection::connect_with(&crate::db::connect_options(cfg)?)
        .await
        .context("could not connect to PostgreSQL")?;
    let mut failed = 0;
    conn.execute("BEGIN").await?;
    for (i, name) in CHECKS.iter().enumerate() {
        conn.execute("SAVEPOINT chk").await?;
        match run_check(i, &mut conn).await {
            Ok(detail) => {
                conn.execute("RELEASE SAVEPOINT chk").await?;
                println!("PASS  {name}\n      {detail}");
            }
            Err(err) => {
                conn.execute("ROLLBACK TO SAVEPOINT chk").await?;
                failed += 1;
                println!("FAIL  {name}\n      {err:#}");
            }
        }
    }
    conn.execute("ROLLBACK").await?;
    conn.close().await?;
    println!("\n{}/{} checks passed (transaction rolled back, no data written)", CHECKS.len() - failed, CHECKS.len());
    if failed > 0 {
        bail!("{failed} check(s) failed");
    }
    Ok(())
}
