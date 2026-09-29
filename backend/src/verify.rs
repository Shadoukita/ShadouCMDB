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
    "New type with custom fields: data entry, its table built by the DDL engine",
    "Server -> Application -> Database and Device -> Location are expressible",
    "Demo graph traversal (recursive, downstream of the CRM service)",
    "Guard: self-edge",
    "Guard: duplicate edge",
    "Guard: reverse duplicate of non-directional connected_to",
    "Guard: illegal endpoint classes (database located_in location)",
    "Guard: CI of an abstract class",
    "Guard: type row without a registry row (foreign key)",
    "Guard: value of the wrong type for its column",
    "Guard: enum value not in the allowed list",
    "Guard: reference field pointing at a CI that does not exist",
    "Guard: class hierarchy cycle",
    "Guard: unknown lookup value, e.g. a status (foreign key)",
    "Guard: audit_log is append-only",
    "Soft delete: removed edge can be re-created; deleted CI cannot be linked",
    "Indexes used by the UI queries",
    "Guard: usernames are unique regardless of case",
    "Guard: the built-in Administrator profile cannot be deleted or changed",
    "Guard: the last active Administrator cannot be disabled or lose the profile",
    "Guard: lookup values must exist and cannot be deleted while stored",
    "Guard: one UI settings row, append-only version history, image types and sizes",
    "Guard: dependent lookup lists: no cycles, parent values and fields from the parent list",
];

/// Placeholder that satisfies users_password_hash_argon2id; nobody can sign in with it.
const NO_PASSWORD: &str = "$argon2id$v=19$verify-only";

async fn new_user(c: &mut PgConnection, username: &str) -> sqlx::Result<Uuid> {
    sqlx::query_scalar("INSERT INTO users (username, display_name, password_hash) VALUES ($1, $1, $2) RETURNING id")
        .bind(username)
        .bind(NO_PASSWORD)
        .fetch_one(c)
        .await
}

async fn builtin_profile(c: &mut PgConnection) -> sqlx::Result<Uuid> {
    sqlx::query_scalar("SELECT id FROM permission_profiles WHERE is_builtin").fetch_one(c).await
}

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

/// The table of a type, schema-qualified and quoted ("infrastruktur"."server").
async fn type_table(c: &mut PgConnection, class: &str) -> anyhow::Result<String> {
    sqlx::query_scalar("SELECT cmdb.type_table(id) FROM ci_classes WHERE key = $1")
        .bind(class)
        .fetch_optional(c)
        .await?
        .ok_or_else(|| anyhow!("no class {class}"))
}

/// Builds the table and columns of these types, as the API does after a data model write.
async fn build_tables(c: &mut PgConnection, classes: Vec<Uuid>) -> anyhow::Result<usize> {
    let ctx = crate::api::context::RequestContext::system("verify", "verify");
    let change = crate::schema::apply(c, &ctx, "verify", crate::schema::Scope::Classes(classes), Default::default())
        .await
        .map_err(|e| anyhow!("DDL engine: {}", e.message))?;
    Ok(change.map(|ch| ch.statements.len()).unwrap_or(0))
}

/// Stores one field value of a CI in its type table (the CI's row there already exists).
async fn set_value(c: &mut PgConnection, class: &str, ci: Uuid, field: &str, value: &str) -> anyhow::Result<()> {
    set_value_sql(c, class, ci, field, value).await?.execute(c).await?;
    Ok(())
}

/// The UPDATE that stores `value` (cast from text to the column's type) in a CI's type row.
async fn set_value_sql<'q>(
    c: &mut PgConnection,
    class: &str,
    ci: Uuid,
    field: &str,
    value: &'q str,
) -> anyhow::Result<sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>> {
    let table = type_table(c, class).await?;
    let column = crate::schema::naming::Ident::trusted(field);
    let pg_type: String = sqlx::query_scalar(
        "SELECT format_type(atttypid, atttypmod) FROM pg_attribute WHERE attrelid = $1::regclass AND attname = $2",
    )
    .bind(&table)
    .bind(field)
    .fetch_optional(&mut *c)
    .await?
    .ok_or_else(|| anyhow!("no column {field} in {table}"))?;
    Ok(sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE {table} SET {column} = $2::text::{pg_type} WHERE id = $1")))
        .bind(ci)
        .bind(value))
}

/// A CI in the registry plus its rows in the tables of its type and every ancestor type,
/// labelled `name` (the value of its "name" field).
async fn new_ci(c: &mut PgConnection, class: &str, name: &str) -> sqlx::Result<Uuid> {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO configuration_items (class_id, label) VALUES ((SELECT id FROM ci_classes WHERE key = $1), $2)
         RETURNING id",
    )
    .bind(class)
    .bind(name)
    .fetch_one(&mut *c)
    .await?;
    let tables: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT l.class_id, cmdb.type_table(l.class_id) FROM ci_class_lineage((SELECT id FROM ci_classes WHERE key = $1)) l
         ORDER BY l.depth DESC",
    )
    .bind(class)
    .fetch_all(&mut *c)
    .await?;
    for (class_id, t) in tables {
        type_row(c, class_id, &t, id, name).await?;
    }
    Ok(id)
}

/// A CI's row in the table of one type: the name, and required fields (NOT NULL
/// columns) get their default or first allowed value.
async fn type_row(c: &mut PgConnection, class_id: Uuid, table: &str, id: Uuid, name: &str) -> sqlx::Result<()> {
    let required: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT d.key, format_type(a.atttypid, a.atttypmod),
                coalesce(CASE WHEN d.key = 'name' THEN $2 END, d.default_value #>> '{}', d.enum_values ->> 0,
                         (SELECT v.id::text FROM lookup_list_values v WHERE v.list_id = d.lookup_list_id
                          ORDER BY v.sort_order, v.key LIMIT 1))
         FROM ci_attribute_definitions d
         JOIN pg_attribute a ON a.attrelid = cmdb.type_table(d.class_id)::regclass AND a.attname = d.key
         WHERE d.class_id = $1 AND (a.attnotnull OR d.key = 'name')",
    )
    .bind(class_id)
    .bind(name)
    .fetch_all(&mut *c)
    .await?;
    let mut columns = String::from("id");
    let mut params = String::from("$1");
    for (i, (key, pg_type, _)) in required.iter().enumerate() {
        columns.push_str(&format!(", {}", crate::schema::naming::Ident::trusted(key)));
        params.push_str(&format!(", ${}::text::{pg_type}", i + 2));
    }
    let sql = format!("INSERT INTO {table} ({columns}) VALUES ({params})");
    let mut q = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(id);
    for (_, _, value) in required {
        q = q.bind(value);
    }
    q.execute(&mut *c).await.map(|_| ())
}

/// The class in `class`'s lineage that defines field `key`, with the field's id.
async fn field_in_lineage(c: &mut PgConnection, class: &str, key: &str) -> anyhow::Result<(String, Uuid)> {
    sqlx::query_as(
        "SELECT k.key, d.id FROM ci_class_lineage((SELECT id FROM ci_classes WHERE key = $1)) l
         JOIN ci_attribute_definitions d ON d.class_id = l.class_id JOIN ci_classes k ON k.id = d.class_id
         WHERE d.key = $2",
    )
    .bind(class)
    .bind(key)
    .fetch_optional(c)
    .await?
    .ok_or_else(|| anyhow!("no field {key} in the lineage of {class}"))
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
                "INSERT INTO ci_classes (key, name, parent_id, area_id)
                 VALUES ('load_balancer', 'Load balancer', $1, (SELECT area_id FROM ci_classes WHERE id = $1)) RETURNING id",
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
            let statements = build_tables(c, vec![lb]).await?;
            let table = type_table(c, "load_balancer").await?;
            let ci = new_ci(c, "load_balancer", "fra1-lb-01").await?;
            let _ = (vip, algo);
            set_value(c, "load_balancer", ci, "vip", "192.0.2.10").await?;
            set_value(c, "load_balancer", ci, "algorithm", "least_conn").await?;
            // Inherited from network_device, which inherits from hardware: stored in network_device's table.
            set_value(c, "network_device", ci, "device_role", "load_balancer").await?;
            let attrs: Vec<String> = sqlx::query_scalar(
                "SELECT d.key FROM ci_class_lineage($1) l JOIN ci_attribute_definitions d ON d.class_id = l.class_id
                 ORDER BY l.depth, d.sort_order, d.key",
            )
            .bind(lb)
            .fetch_all(&mut *c)
            .await?;
            // Inherited relationship rules apply too: a load balancer is hardware, so it can be located_in a location.
            let rack = new_ci(c, "location", "verify-rack").await?;
            link(c, "located_in", ci, rack).await?;
            Ok(format!(
                "type load_balancer + 2 fields inserted, table {table} built ({statements} statements); effective \
                 fields: {}; located_in rack accepted via inherited rule",
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
                "SELECT s.label || ' <-runs_on- ' || a.label || ' -depends_on-> ' || d.label
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
                   WHERE ci.label = 'Customer Relationship Management' AND r.deleted_at IS NULL
                   UNION
                   SELECT r.source_ci_id, r.target_ci_id, r.relationship_type_id, g.depth + 1
                   FROM ci_relationships r JOIN g ON r.source_ci_id = g.target_ci_id
                   WHERE r.deleted_at IS NULL AND g.depth < 6
                 )
                 SELECT s.label || ' ' || t.forward_label || ' ' || d.label AS line
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
            let table = type_table(c, "server").await?;
            let server = id_by_key(c, "ci_classes", "server").await?;
            reject!(c, "_id_fkey", type_row(c, server, &table, Uuid::new_v4(), "orphan"))
        }
        9 => {
            let s = new_ci(c, "server", "type-srv").await?;
            let table = type_table(c, "server").await?;
            reject!(
                c,
                "invalid input syntax for type bigint",
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "UPDATE {table} SET cpu_cores = $2::text::bigint WHERE id = $1"
                )))
                .bind(s)
                .bind("lots")
                .execute(&mut *c)
            )
        }
        10 => {
            let s = new_ci(c, "server", "enum-srv").await?;
            let def = attribute_id(c, "server", "os_family").await?;
            let check = format!("ck_{}", def.simple());
            let update = set_value_sql(c, "server", s, "os_family", "amiga").await?;
            reject!(c, &check, update.execute(&mut *c))
        }
        11 => {
            let a = new_ci(c, "application", "ref-app").await?;
            let def = attribute_id(c, "application", "primary_database").await?;
            let fk = format!("fk_{}", def.simple());
            let missing = Uuid::new_v4().to_string();
            let update = set_value_sql(c, "application", a, "primary_database", &missing).await?;
            reject!(c, &fk, update.execute(&mut *c))
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
        13 => {
            let s = new_ci(c, "server", "fk-srv").await?;
            // On hardware after a fresh install, on server after the 0016 upgrade.
            let (class, def) = field_in_lineage(c, "server", "status").await?;
            let fk = format!("fk_{}", def.simple());
            let unknown = Uuid::new_v4().to_string();
            let update = set_value_sql(c, &class, s, "status", &unknown).await?;
            reject!(c, &fk, update.execute(&mut *c))
        }
        14 => {
            // The trigger is row-level, so an UPDATE of an empty audit_log would succeed vacuously:
            // write one row first so the check does not depend on `seed --demo`.
            sqlx::query(
                "INSERT INTO audit_log (actor_type, action, entity_type, entity_id, new_value)
                 VALUES ('system', 'create', 'verify', gen_random_uuid(), '{}')",
            )
            .execute(&mut *c)
            .await?;
            // As the API role of a three-role install the privilege check refuses first; as the
            // owner (single-role install, CI) the trigger does. Both are insufficient_privilege.
            let mut outcomes = Vec::new();
            for stmt in ["UPDATE audit_log SET actor_name = 'tampered'", "DELETE FROM audit_log", "TRUNCATE audit_log"]
            {
                c.execute("SAVEPOINT sp").await?;
                let result = sqlx::query(sqlx::AssertSqlSafe(stmt)).execute(&mut *c).await;
                let Err(err) = result else { bail!("expected `{stmt}` to be rejected, but it succeeded") };
                c.execute("ROLLBACK TO SAVEPOINT sp").await?;
                let code = err.as_database_error().and_then(|d| d.code()).unwrap_or_default().into_owned();
                if code != "42501" {
                    bail!("expected `{stmt}` to fail with 42501, got {code}: {err}");
                }
                outcomes.push(stmt.split_whitespace().next().unwrap_or_default());
            }
            Ok(format!("rejected (42501: {})", outcomes.join(", ")))
        }
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
            let tgt = reject!(c, "ci_relationships_live_endpoints", link(c, "depends_on", a, d))?;
            Ok(format!("edge re-created after soft delete; linking deleted CI as source {res}, as target {tgt}"))
        }
        16 => {
            c.execute("SET LOCAL enable_seqscan = off").await?;
            // Any of the listed indexes is acceptable (on tiny tables PG18 may prefer a skip scan of a wider index).
            let probes: [(&'static str, &[&str]); 5] = [
                (
                    "SELECT id FROM configuration_items WHERE label ILIKE '%crm%'",
                    &["configuration_items_label_trgm_idx"],
                ),
                (
                    "SELECT id FROM configuration_items WHERE search_vector @@ plainto_tsquery('simple', 'crm')",
                    &["configuration_items_search_idx"],
                ),
                (
                    "SELECT id FROM configuration_items WHERE deleted_at IS NULL ORDER BY lower(label), id LIMIT 50",
                    &["configuration_items_live_label_idx", "configuration_items_class_label_idx"],
                ),
                (
                    "SELECT target_ci_id FROM ci_relationships WHERE source_ci_id = '00000000-0000-0000-0000-000000000000' AND deleted_at IS NULL",
                    &["ci_relationships_source_idx", "ci_relationships_live_edge_uq"],
                ),
                (
                    "SELECT id FROM configuration_items WHERE lower(ident) = 'ci-0000000'",
                    &["configuration_items_ident_uq"],
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
        17 => {
            new_user(c, "verify-Case").await?;
            reject!(c, "users_username_uq", new_user(c, "VERIFY-case"))
        }
        18 => {
            let admin = builtin_profile(c).await?;
            let deleted = reject!(
                c,
                "permission_profiles_builtin_protected",
                sqlx::query("DELETE FROM permission_profiles WHERE id = $1").bind(admin).execute(&mut *c)
            )?;
            reject!(
                c,
                "permission_profiles_builtin_protected",
                sqlx::query("UPDATE permission_profiles SET name = 'Renamed' WHERE id = $1")
                    .bind(admin)
                    .execute(&mut *c)
            )?;
            reject!(
                c,
                "permission_profiles_builtin_protected",
                sqlx::query("INSERT INTO permission_profile_global_permissions VALUES ($1, 'audit.view')")
                    .bind(admin)
                    .execute(&mut *c)
            )?;
            Ok(format!("delete, rename and permission rows {deleted}"))
        }
        19 => {
            // The check is deferred to commit; SET CONSTRAINTS ... IMMEDIATE runs it now.
            let admin = builtin_profile(c).await?;
            let v = new_user(c, "verify-last-admin").await?;
            sqlx::query("INSERT INTO user_permission_profiles (user_id, profile_id) VALUES ($1, $2)")
                .bind(v)
                .bind(admin)
                .execute(&mut *c)
                .await?;
            // Make v the only active administrator (fine: v still holds the profile).
            sqlx::query("UPDATE users SET is_active = false WHERE id <> $1").bind(v).execute(&mut *c).await?;
            c.execute("SET CONSTRAINTS ALL IMMEDIATE").await?;
            let result = async {
                let removed = reject!(c, "users_last_administrator", async {
                    sqlx::query("DELETE FROM user_permission_profiles WHERE user_id = $1")
                        .bind(v)
                        .execute(&mut *c)
                        .await?;
                    c.execute("SET CONSTRAINTS ALL IMMEDIATE").await
                })?;
                let disabled = reject!(c, "users_last_administrator", async {
                    sqlx::query("UPDATE users SET is_active = false WHERE id = $1").bind(v).execute(&mut *c).await?;
                    c.execute("SET CONSTRAINTS ALL IMMEDIATE").await
                })?;
                let deleted = reject!(c, "users_last_administrator", async {
                    sqlx::query("DELETE FROM users WHERE id = $1").bind(v).execute(&mut *c).await?;
                    c.execute("SET CONSTRAINTS ALL IMMEDIATE").await
                })?;
                Ok::<_, anyhow::Error>(format!(
                    "losing the profile {removed}; disabling {disabled}; deleting {deleted}"
                ))
            }
            .await;
            c.execute("SET CONSTRAINTS ALL DEFERRED").await?;
            result
        }
        20 => {
            let list = |key: &'static str| {
                sqlx::query_scalar::<_, Uuid>("INSERT INTO lookup_lists (key, name) VALUES ($1, $1) RETURNING id")
                    .bind(key)
            };
            let value = |list_id: Uuid, key: &'static str| {
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO lookup_list_values (list_id, key, name) VALUES ($1, $2, $2) RETURNING id",
                )
                .bind(list_id)
                .bind(key)
            };
            let contracts = list("verify_contract").fetch_one(&mut *c).await?;
            let colours = list("verify_colour").fetch_one(&mut *c).await?;
            let gold = value(contracts, "gold").fetch_one(&mut *c).await?;
            let red = value(colours, "red").fetch_one(&mut *c).await?;
            let server = id_by_key(c, "ci_classes", "server").await?;
            let attr: Uuid = sqlx::query_scalar(
                "INSERT INTO ci_attribute_definitions (class_id, key, label, data_type, lookup_list_id)
                 VALUES ($1, 'verify_contract', 'Contract', 'lookup', $2) RETURNING id",
            )
            .bind(server)
            .bind(contracts)
            .fetch_one(&mut *c)
            .await?;
            let server = vec![server];
            build_tables(c, server).await?;
            let ci = new_ci(c, "server", "verify-lookup").await?;
            let fk = format!("fk_{}", attr.simple());
            let _ = red;
            let unknown = Uuid::new_v4().to_string();
            let update = set_value_sql(c, "server", ci, "verify_contract", &unknown).await?;
            let foreign = reject!(c, &fk, update.execute(&mut *c))?;
            set_value(c, "server", ci, "verify_contract", &gold.to_string()).await?;
            let delete = reject!(
                c,
                &fk,
                sqlx::query("DELETE FROM lookup_list_values WHERE id = $1").bind(gold).execute(&mut *c)
            )?;
            let list_delete = reject!(
                c,
                "ci_attribute_definitions_lookup_list_id_fkey",
                sqlx::query("DELETE FROM lookup_lists WHERE id = $1").bind(contracts).execute(&mut *c)
            )?;
            Ok(format!(
                "unknown value {foreign}; deleting a stored value {delete}; deleting a list in use {list_delete} \
                 (that a value belongs to the field's list is checked by the API)"
            ))
        }
        21 => {
            let second = reject!(
                c,
                "ui_settings_singleton_uq",
                sqlx::query("INSERT INTO ui_settings (version, settings) VALUES (1, '{}')").execute(&mut *c)
            )?;
            let rewrite = reject!(
                c,
                "ui_settings_versions_append_only",
                sqlx::query("UPDATE ui_settings_versions SET comment = 'rewritten'").execute(&mut *c)
            )?;
            let unknown = reject!(c, "ui_settings_version_fk", async {
                sqlx::query("UPDATE ui_settings SET version = 2147483647").execute(&mut *c).await?;
                c.execute("SET CONSTRAINTS ALL IMMEDIATE").await
            })?;
            c.execute("SET CONSTRAINTS ALL DEFERRED").await?;
            let html = reject!(
                c,
                "ui_assets_content_type_valid",
                sqlx::query("INSERT INTO ui_assets (kind, content_type, data, sha256) VALUES ('logo', 'text/html', '\\x3c', repeat('0', 64))")
                    .execute(&mut *c)
            )?;
            let big = reject!(
                c,
                "ui_assets_size",
                sqlx::query(
                    "INSERT INTO ui_assets (kind, content_type, data, sha256)
                     VALUES ('favicon', 'image/png', decode(repeat('00', 131073), 'hex'), repeat('0', 64))"
                )
                .execute(&mut *c)
            )?;
            Ok(format!(
                "second settings row {second}; rewriting history {rewrite}; current version not in history {unknown}; \
                 HTML as an image {html}; favicon over 128 KiB {big}"
            ))
        }
        22 => {
            let list = |key: &'static str, parent: Option<Uuid>| {
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO lookup_lists (key, name, parent_list_id) VALUES ($1, $1, $2) RETURNING id",
                )
                .bind(key)
                .bind(parent)
            };
            let value = |list_id: Uuid, key: &'static str, parent: Option<Uuid>| {
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO lookup_list_values (list_id, key, name, parent_value_id) VALUES ($1, $2, $2, $3) RETURNING id",
                )
                .bind(list_id)
                .bind(key)
                .bind(parent)
            };
            let makers = list("verify_maker", None).fetch_one(&mut *c).await?;
            let models = list("verify_model", Some(makers)).fetch_one(&mut *c).await?;
            let cycle = reject!(
                c,
                "lookup_lists_no_cycle",
                sqlx::query("UPDATE lookup_lists SET parent_list_id = $2 WHERE id = $1")
                    .bind(makers)
                    .bind(models)
                    .execute(&mut *c)
            )?;
            let cisco = value(makers, "cisco", None).fetch_one(&mut *c).await?;
            let c9300 = value(models, "c9300", Some(cisco)).fetch_one(&mut *c).await?;
            let orphan =
                reject!(c, "lookup_list_values_parent_required", value(models, "orphan", None).fetch_one(&mut *c))?;
            let wrong =
                reject!(c, "lookup_list_values_parent_list", value(models, "nested", Some(c9300)).fetch_one(&mut *c))?;
            let delete = reject!(
                c,
                "lookup_list_values_parent_value_id_fkey",
                sqlx::query("DELETE FROM lookup_list_values WHERE id = $1").bind(cisco).execute(&mut *c)
            )?;
            let server = id_by_key(c, "ci_classes", "server").await?;
            let field = |key: &'static str, list_id: Uuid, parent: Option<Uuid>| {
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO ci_attribute_definitions (class_id, key, label, data_type, lookup_list_id, parent_attribute_id)
                     VALUES ($1, $2, $2, 'lookup', $3, $4) RETURNING id",
                )
                .bind(server)
                .bind(key)
                .bind(list_id)
                .bind(parent)
            };
            let maker_field = field("verify_maker", makers, None).fetch_one(&mut *c).await?;
            let bad_field = reject!(
                c,
                "ci_attribute_definitions_parent_attribute",
                field("verify_maker2", makers, Some(maker_field)).fetch_one(&mut *c)
            )?;
            field("verify_model", models, Some(maker_field)).fetch_one(&mut *c).await?;
            let stale = reject!(c, "lookup_lists_parent_values", async {
                sqlx::query("UPDATE lookup_lists SET parent_list_id = NULL WHERE id = $1")
                    .bind(models)
                    .execute(&mut *c)
                    .await?;
                c.execute("SET CONSTRAINTS ALL IMMEDIATE").await
            })?;
            c.execute("SET CONSTRAINTS ALL DEFERRED").await?;
            Ok(format!(
                "list cycle {cycle}; value without parent {orphan}; parent from another list {wrong}; deleting a parent \
                 value {delete}; parent field on another list {bad_field}; new parent list with stale parents {stale} \
                 (that a CI's value belongs to its parent field's value is checked by the API)"
            ))
        }
        _ => unreachable!("unknown check {i}"),
    }
}

pub async fn run(cfg: &DatabaseConfig) -> anyhow::Result<()> {
    let mut conn = PgConnection::connect_with(&crate::db::connect_options(cfg)?)
        .await
        .context("could not connect to PostgreSQL")?;
    if let Some(notice) = crate::data::api_tokens::refused_for_mfa_notice(&mut conn).await? {
        println!("Warning: {notice}\n");
    }
    let mut failed = 0;
    conn.execute("BEGIN").await?;
    // The checks build on the IT infrastructure data model. A bare install gets
    // it inside this transaction, so it is rolled back with everything else.
    let ctx = crate::api::context::RequestContext::system("verify", "verify");
    let template = crate::modules::templates::find("it_infrastructure").context("template missing")?;
    let installed = crate::modules::templates::install(&mut conn, &ctx, template)
        .await
        .map_err(|e| anyhow!("installing the IT infrastructure template: {}", e.message))?;
    if installed.created.classes > 0 {
        println!("(bare database: IT infrastructure template installed for the checks, rolled back afterwards)\n");
    }
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
