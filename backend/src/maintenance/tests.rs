//! Against a real PostgreSQL (see `db::scratch`).

use sqlx::postgres::PgConnection;

use super::archive::{self, Header};
use super::{Table, app_object_count, app_tables, backup, ident, reset, restore, stored_columns};
use crate::db::{MIGRATOR, scratch};

/// Every stored value of a table, order-independent.
async fn fingerprint(c: &mut PgConnection, table: &Table) -> String {
    let mut cols = stored_columns(c, table).await.unwrap();
    cols.sort();
    let cols = cols.iter().map(|c| ident(c)).collect::<Vec<_>>().join(", ");
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT coalesce(md5(string_agg(r, E'\\n' ORDER BY r)), '') || ':' || count(*)
         FROM (SELECT row_to_json(x)::text AS r FROM (SELECT {cols} FROM {}) x) y",
        table.sql()
    )))
    .fetch_one(c)
    .await
    .unwrap()
}

async fn fingerprints(c: &mut PgConnection) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for t in app_tables(c).await.unwrap() {
        if t.to_string() != "cmdb.sessions" {
            let f = fingerprint(c, &t).await;
            out.push((t.to_string(), f));
        }
    }
    out
}

/// Demo inventory, a user with a session, a binary asset and an audit trail.
async fn populate(pool: &sqlx::PgPool) {
    crate::seed::install_template(pool, "it_infrastructure").await.unwrap();
    crate::seed::seed_demo_data(pool).await.unwrap();
    sqlx::query(
        "WITH u AS (INSERT INTO users (username, display_name, password_hash)
                    VALUES ('admin', 'Ädmin \"quoted\"\nline', '$argon2id$v=19$test') RETURNING id),
              p AS (INSERT INTO user_permission_profiles (user_id, profile_id)
                    SELECT u.id, (SELECT id FROM permission_profiles WHERE is_builtin) FROM u)
         INSERT INTO sessions (token_hash, user_id, csrf_token, expires_at)
         SELECT sha256('t'), u.id, 'csrf', now() + interval '1 hour' FROM u",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO ui_assets (kind, content_type, data, sha256)
         VALUES ('favicon', 'image/x-icon', '\\x00ff0a0d22'::bytea, encode(sha256('\\x00ff0a0d22'::bytea), 'hex'))",
    )
    .execute(pool)
    .await
    .unwrap();
}

async fn take_backup(c: &mut PgConnection) -> (Vec<u8>, Header) {
    let mut buf = Vec::new();
    let header = backup::write(c, &mut buf).await.unwrap();
    let checked = archive::verify(buf.as_slice()).unwrap();
    assert_eq!(checked.total_rows(), header.total_rows());
    (buf, header)
}

#[tokio::test]
async fn a_backup_restores_into_another_database_value_for_value() {
    let Some(a) = scratch::database("backup_roundtrip_a").await else { return };
    let Some(b) = scratch::database("backup_roundtrip_b").await else { return };
    populate(&a.pool).await;
    let mut ca = a.pool.acquire().await.unwrap();
    let mut cb = b.pool.acquire().await.unwrap();

    // A required field that older assets still lack a value for: the engine left
    // it nullable (reconcile is lenient), so the restore must too.
    let required: Vec<(String, String)> = sqlx::query_as(
        "SELECT format('%I.%I', n.nspname, c.relname), quote_ident(a.attname)
         FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = 'infrastruktur' AND c.relkind = 'r' AND a.attnum > 0 AND a.attnotnull AND a.attname <> 'id'
         ORDER BY 1, 2",
    )
    .fetch_all(&mut *ca)
    .await
    .unwrap();
    let mut emptied = 0;
    for (table, column) in &required {
        let sql = format!("ALTER TABLE {table} ALTER COLUMN {column} DROP NOT NULL");
        sqlx::query(sqlx::AssertSqlSafe(sql)).execute(&mut *ca).await.unwrap();
        let sql = format!("UPDATE {table} SET {column} = NULL WHERE id = (SELECT id FROM {table} LIMIT 1)");
        let done = sqlx::query(sqlx::AssertSqlSafe(sql)).execute(&mut *ca).await.unwrap().rows_affected();
        if done == 0 {
            // No assets of this type: keep the column required.
            let sql = format!("ALTER TABLE {table} ALTER COLUMN {column} SET NOT NULL");
            sqlx::query(sqlx::AssertSqlSafe(sql)).execute(&mut *ca).await.unwrap();
        }
        emptied += done;
        if emptied > 0 {
            break;
        }
    }
    assert!(emptied > 0, "the demo data has a required field with values: {required:?}");

    let (buf, header) = take_backup(&mut ca).await;
    assert!(header.total_rows() > 20, "demo data is in the backup");
    assert_eq!(header.excluded_tables, vec!["cmdb.sessions".to_owned()]);
    assert!(!header.tables.iter().any(|t| t.name == "sessions"));
    // The values of the demo assets live in the tables of their types.
    let server = header.tables.iter().find(|t| t.schema == "infrastruktur" && t.name == "server");
    assert!(server.is_some_and(|t| t.rows > 0), "type tables are in the backup: {:?}", header.tables);

    // The target is a migrated install: it has to be replaced.
    let report = restore::restore(&mut cb, buf.as_slice(), &header, true, true).await.unwrap();
    assert_eq!(report.users, 1);
    assert_eq!(report.migrations_applied_after, 0);
    assert_eq!(fingerprints(&mut ca).await, fingerprints(&mut cb).await);
    let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions").fetch_one(&mut *cb).await.unwrap();
    assert_eq!(sessions, 0, "sessions are never restored");

    // Sequences continue where the source was, and the triggers are back on.
    let seq = "SELECT last_value FROM cmdb.audit_log_id_seq";
    let (sa, sb): (i64, i64) = (
        sqlx::query_scalar(seq).fetch_one(&mut *ca).await.unwrap(),
        sqlx::query_scalar(seq).fetch_one(&mut *cb).await.unwrap(),
    );
    assert_eq!(sa, sb);
    let err = sqlx::query("DELETE FROM audit_log").execute(&mut *cb).await.unwrap_err();
    assert!(err.to_string().contains("append-only"), "{err}");

    // A backup of the restored database is the same data.
    let (_, again) = take_backup(&mut cb).await;
    assert_eq!(again.tables, header.tables);

    // The reporting views and NOT NULL columns are back as well, and no schema
    // change was recorded that the source does not have.
    let shape = "SELECT (SELECT count(*) FROM pg_views WHERE schemaname = 'infrastruktur'),
                        (SELECT count(*) FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid
                         WHERE c.relnamespace = 'infrastruktur'::regnamespace AND c.relkind = 'r'
                           AND a.attnum > 0 AND a.attnotnull)";
    let (va, na): (i64, i64) = sqlx::query_as(shape).fetch_one(&mut *ca).await.unwrap();
    let (vb, nb): (i64, i64) = sqlx::query_as(shape).fetch_one(&mut *cb).await.unwrap();
    assert!(va > 0);
    assert_eq!((va, na), (vb, nb));

    drop((ca, cb));
    a.drop().await;
    b.drop().await;
}

#[tokio::test]
async fn a_failed_or_dry_run_restore_changes_nothing() {
    let Some(a) = scratch::database("restore_all_or_nothing").await else { return };
    populate(&a.pool).await;
    let mut c = a.pool.acquire().await.unwrap();
    let (buf, header) = take_backup(&mut c).await;
    sqlx::query("DELETE FROM configuration_items WHERE name = 'crm-app-01'").execute(&mut *c).await.ok();
    let before = fingerprints(&mut c).await;

    let report = restore::restore(&mut c, buf.as_slice(), &header, true, false).await.unwrap();
    assert_eq!(report.rows, header.total_rows());
    assert_eq!(fingerprints(&mut c).await, before, "dry run rolled back");

    // A header that promises a row the file does not have: the load fails, the database is untouched.
    let mut wrong = header.clone();
    wrong.tables[0].rows += 1;
    assert!(restore::restore(&mut c, buf.as_slice(), &wrong, true, true).await.is_err());
    assert_eq!(fingerprints(&mut c).await, before);

    // A backup from a newer release is refused before anything happens.
    let mut newer = header.clone();
    newer.migrations.push(archive::MigrationEntry {
        version: 9999,
        description: "future".into(),
        checksum: "00".into(),
    });
    let err = restore::restore(&mut c, buf.as_slice(), &newer, true, true).await.unwrap_err().to_string();
    assert!(err.contains("does not know"), "{err}");

    drop(c);
    a.drop().await;
}

#[tokio::test]
async fn a_backup_from_an_older_schema_is_upgraded_on_restore() {
    let Some(a) = scratch::database("restore_older_level").await else { return };
    let Some(b) = scratch::database("restore_older_level_b").await else { return };
    let latest = MIGRATOR.iter().map(|m| m.version).max().unwrap();
    let mut ca = a.pool.acquire().await.unwrap();
    reset::decommission(&mut ca).await.unwrap();
    MIGRATOR.run_to(latest - 1, &mut *ca).await.unwrap();
    let (buf, header) = take_backup(&mut ca).await;
    assert_eq!(header.migration_level(), Some(latest - 1));

    let mut cb = b.pool.acquire().await.unwrap();
    let report = restore::restore(&mut cb, buf.as_slice(), &header, true, true).await.unwrap();
    assert_eq!(report.migrations_applied_after, 1);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM public._sqlx_migrations").fetch_one(&mut *cb).await.unwrap();
    assert_eq!(n as usize, crate::db::expected_count());

    drop((ca, cb));
    a.drop().await;
    b.drop().await;
}

#[tokio::test]
async fn factory_reset_returns_to_first_run_and_decommission_leaves_nothing() {
    let Some(a) = scratch::database("factory_reset").await else { return };
    populate(&a.pool).await;
    let mut c = a.pool.acquire().await.unwrap();
    let areas: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_namespace WHERE nspname = 'infrastruktur'")
        .fetch_one(&mut *c)
        .await
        .unwrap();
    assert_eq!(areas, 1, "the template built its area schema");

    reset::factory_reset(&mut c).await.unwrap();
    let (users, cis, classes, audit): (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM users), (SELECT count(*) FROM configuration_items),
                (SELECT count(*) FROM ci_classes), (SELECT count(*) FROM audit_log)",
    )
    .fetch_one(&mut *c)
    .await
    .unwrap();
    assert_eq!((users, cis, classes, audit), (0, 0, 0, 0));
    let leftovers: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_namespace WHERE nspname = 'infrastruktur'")
        .fetch_one(&mut *c)
        .await
        .unwrap();
    assert_eq!(leftovers, 0, "the area schemas are gone");
    drop(c);
    assert!(crate::modules::auth::setup_required(&a.pool).await.unwrap(), "setup is forced");
    crate::seed::seed_system_rows(&a.pool).await.unwrap();

    let mut c = a.pool.acquire().await.unwrap();
    reset::decommission(&mut c).await.unwrap();
    assert_eq!(app_object_count(&mut c).await.unwrap(), 0);
    let schemas: i64 =
        sqlx::query_scalar("SELECT count(*) FROM pg_namespace WHERE nspname IN ('cmdb', 'infrastruktur')")
            .fetch_one(&mut *c)
            .await
            .unwrap();
    assert_eq!(schemas, 0);
    // The empty database can be installed again.
    MIGRATOR.run(&mut *c).await.unwrap();
    assert!(app_object_count(&mut c).await.unwrap() > 0);

    drop(c);
    a.drop().await;
}
