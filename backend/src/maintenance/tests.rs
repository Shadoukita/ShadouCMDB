//! Against a real PostgreSQL (see `db::scratch`).

use sqlx::postgres::PgConnection;

use super::archive::{self, Header};
use super::{
    EXCLUDED_TABLES, Table, app_object_count, app_tables, area_schemas, backup, ident, reset, restore, stored_columns,
};
use crate::db::{MIGRATOR, scratch};

/// Every stored value of a table, independent of row and column order.
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
        if !t.is_excluded() {
            let f = fingerprint(c, &t).await;
            out.push((t.display(), f));
        }
    }
    out
}

/// The physical schema of the areas: every column with its type and
/// nullability, every constraint and index, every view's definition.
async fn area_ddl(c: &mut PgConnection) -> Vec<String> {
    let areas = area_schemas(c).await.unwrap();
    sqlx::query_scalar(
        "SELECT format('column %s.%s.%s %s %s', table_schema, table_name, column_name, data_type, is_nullable)
         FROM information_schema.columns WHERE table_schema = ANY ($1)
         UNION ALL
         SELECT format('constraint %s %s %s', co.conrelid::regclass, co.conname, pg_get_constraintdef(co.oid))
         FROM pg_constraint co JOIN pg_namespace n ON n.oid = co.connamespace WHERE n.nspname = ANY ($1)
         UNION ALL
         SELECT format('index %s', indexdef) FROM pg_indexes WHERE schemaname = ANY ($1)
         UNION ALL
         SELECT format('view %s.%s %s', schemaname, viewname, definition) FROM pg_views WHERE schemaname = ANY ($1)
         ORDER BY 1",
    )
    .bind(&areas)
    .fetch_all(c)
    .await
    .unwrap()
}

/// Demo inventory in the "infrastruktur" area (a schema with a table per
/// type), a user with a session, a binary asset and an audit trail.
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
    // An authenticator: encrypted (GH#189), restored as the same bytes.
    let admin: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE username = 'admin'").fetch_one(pool).await.unwrap();
    let sealed = crate::secrets::sealed::seal_totp_secret(&crate::secrets::Keyring::for_tests(), admin, &[9u8; 20]);
    sqlx::query("INSERT INTO user_totp (user_id, secret, key_id, confirmed_at) VALUES ($1, $2, $3, now())")
        .bind(admin)
        .bind(&sealed.bytes)
        .bind(sealed.key_id.0)
        .execute(pool)
        .await
        .unwrap();
    // An OIDC provider with an encrypted client secret (GH#199), restored as the same bytes.
    let provider = uuid::Uuid::new_v4();
    let sealed = crate::secrets::sealed::seal_provider_secret(
        &crate::secrets::Keyring::for_tests(),
        provider,
        crate::secrets::sealed::ProviderSecret::ClientSecret,
        "backup-client-secret",
    );
    sqlx::query(
        "INSERT INTO identity_providers (id, kind, name, issuer_url, client_id, client_secret_enc, secrets_key_id,
           scopes, username_claim, groups_claim, mfa_assurance, required_acr)
         VALUES ($1, 'oidc', 'Entra ID', 'https://idp.example.test', 'cmdb', $2, $3, 'profile', 'preferred_username',
           'groups', 'verify', '{}')",
    )
    .bind(provider)
    .bind(&sealed.bytes)
    .bind(sealed.key_id.0)
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

/// Header tables with their columns sorted: a rebuilt type table may order its columns differently.
fn normalized(h: &Header) -> Vec<archive::TableEntry> {
    let mut tables = h.tables.clone();
    for t in &mut tables {
        t.columns.sort();
    }
    tables
}

#[tokio::test]
async fn a_backup_restores_into_another_database_value_for_value() {
    let Some(a) = scratch::database("backup_roundtrip_a").await else { return };
    let Some(b) = scratch::database("backup_roundtrip_b").await else { return };
    populate(&a.pool).await;
    let mut ca = a.pool.acquire().await.unwrap();
    let mut cb = b.pool.acquire().await.unwrap();

    // A required field with assets that have no value stays nullable (as after
    // migration 0009): the restore must load those rows all the same.
    sqlx::query(
        "UPDATE cmdb.ci_attribute_definitions SET is_required = true
         WHERE key = 'management_ip' AND class_id = (SELECT id FROM cmdb.ci_classes WHERE key = 'server')",
    )
    .execute(&mut *ca)
    .await
    .unwrap();
    let missing: i64 = sqlx::query_scalar("SELECT count(*) FROM infrastruktur.server WHERE management_ip IS NULL")
        .fetch_one(&mut *ca)
        .await
        .unwrap();
    assert!(missing > 0, "demo data has servers without a management IP");

    let (buf, header) = take_backup(&mut ca).await;
    assert!(header.total_rows() > 20, "demo data is in the backup");
    assert_eq!(
        header.excluded_tables,
        [
            "cmdb.import_idempotency_keys",
            "cmdb.import_job_files",
            "cmdb.import_job_issues",
            "cmdb.mfa_challenges",
            "cmdb.server_keys",
            "cmdb.sessions"
        ]
        .map(str::to_owned)
        .to_vec()
    );
    assert!(!header.tables.iter().any(|t| EXCLUDED_TABLES.contains(&t.name.as_str())));
    // The key ids in use are listed (the key itself is not in the file), and a
    // restore where another key is configured warns.
    let key = crate::secrets::Keyring::for_tests().active_id();
    assert_eq!(
        header.encryption_keys,
        vec![
            archive::EncryptionKeyEntry { key_id: key.to_string(), table: "user_totp".into(), rows: 1 },
            archive::EncryptionKeyEntry { key_id: key.to_string(), table: "identity_providers".into(), rows: 1 },
        ]
    );
    let none = crate::config::EncryptionConfig::default();
    let warning = restore::key_warning(&header, &none).unwrap();
    assert!(
        warning.starts_with(&format!(
            "This backup holds 1 authenticator secret and 1 identity provider secret encrypted with key {key}. No \
             key is configured (ENCRYPTION_KEY_FILE is not set). The server will not start until that key is \
             configured, or until \"shadoucmdb mfa reset-undecryptable\" and \"shadoucmdb identity-providers \
             reset-undecryptable\" have been run"
        )),
        "{warning}"
    );
    // The system tables, then the type tables of the area.
    assert!(header.tables.iter().any(|t| t.schema == "cmdb" && t.name == "schema_changes" && t.rows > 0));
    let server = header.tables.iter().find(|t| t.schema == "infrastruktur" && t.name == "server").unwrap();
    assert!(server.rows > 0 && server.columns.contains(&"cpu_cores".to_owned()));
    let first_type = header.tables.iter().position(|t| t.schema == "infrastruktur").unwrap();
    assert!(header.tables[first_type..].iter().all(|t| t.schema == "infrastruktur"));

    // The target is a migrated install: it has to be replaced.
    let report = restore::restore(&mut cb, buf.as_slice(), &header, true, true).await.unwrap();
    assert_eq!(report.users, 1);
    assert_eq!(report.migrations_applied_after, 0);
    assert!(report.warnings.iter().any(|w| w.contains("server.management_ip stays nullable")), "{:?}", report.warnings);
    assert_eq!(fingerprints(&mut ca).await, fingerprints(&mut cb).await);
    let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions").fetch_one(&mut *cb).await.unwrap();
    assert_eq!(sessions, 0, "sessions are never restored");

    // The area schema is rebuilt as it was: the same columns, NOT NULLs,
    // checks, foreign keys, indexes and reporting views.
    let ddl = area_ddl(&mut ca).await;
    assert!(ddl.iter().any(|d| d.starts_with("view infrastruktur.v_server")), "{ddl:?}");
    assert!(ddl.iter().any(|d| d.contains("FOREIGN KEY") && d.contains("configuration_items")), "{ddl:?}");
    assert_eq!(ddl, area_ddl(&mut cb).await);
    // Restoring is not a schema change: the history is the backup's, nothing was added.
    let (sa, sb): (i64, i64) = (
        sqlx::query_scalar("SELECT count(*) FROM cmdb.schema_changes").fetch_one(&mut *ca).await.unwrap(),
        sqlx::query_scalar("SELECT count(*) FROM cmdb.schema_changes").fetch_one(&mut *cb).await.unwrap(),
    );
    assert_eq!(sa, sb);
    // The type tables work: the foreign key to the registry is enforced again.
    let err = sqlx::query("INSERT INTO infrastruktur.server (id) VALUES (gen_random_uuid())")
        .execute(&mut *cb)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("foreign key"), "{err}");

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
    assert_eq!(normalized(&again), normalized(&header));

    drop((ca, cb));
    a.drop().await;
    b.drop().await;
}

/// GH#396: on a three-role install `backup` runs as the API role, which may
/// read the audit hash-chain head (migration 0038) but still not move or lock
/// it. The restored chain verifies and keeps the same head.
#[tokio::test]
async fn the_api_role_backs_up_the_audit_chain_head_but_cannot_move_it() {
    let Some(roles) = scratch::Roles::create("the_api_role_backs_up_the_audit_chain_head_but_cannot_move_it").await
    else {
        return;
    };
    let a = roles.database().await;
    let b = roles.database().await;
    // As in production (`seed`, the running application), the API role creates
    // the area schemas and type tables and so owns them.
    let api = roles.api_pool(&a).await;
    populate(&api).await;
    let mut ca = api.acquire().await.unwrap();
    let mut cb = b.pool.acquire().await.unwrap();
    let head = "SELECT last_seq, encode(last_hash, 'hex') FROM cmdb.audit_log_chain_head";
    let head_a: (i64, String) = sqlx::query_as(head).fetch_one(&a.pool).await.unwrap();
    assert!(head_a.0 > 0, "demo data writes audit entries");

    for denied in [
        "SELECT * FROM cmdb.audit_log_chain_head FOR UPDATE",
        "SELECT * FROM cmdb.audit_log_chain_head FOR KEY SHARE",
        "UPDATE cmdb.audit_log_chain_head SET last_seq = 0",
        "DELETE FROM cmdb.audit_log_chain_head",
        "TRUNCATE cmdb.audit_log_chain_head",
        "INSERT INTO cmdb.audit_log_chain_head (singleton, last_seq, last_hash) VALUES (false, 0, '\\x00')",
    ] {
        let err = sqlx::query(denied).execute(&mut *ca).await.unwrap_err();
        let code = err.as_database_error().and_then(|d| d.code()).unwrap_or_default().into_owned();
        assert_eq!(code, "42501", "{denied}: {err}");
    }
    let (buf, header) = take_backup(&mut ca).await;
    let copied = header.tables.iter().find(|t| t.schema == "cmdb" && t.name == "audit_log_chain_head");
    assert_eq!(copied.map(|t| t.rows), Some(1));

    restore::restore(&mut cb, buf.as_slice(), &header, true, true).await.unwrap();
    let head_b: (i64, String) = sqlx::query_as(head).fetch_one(&mut *cb).await.unwrap();
    assert_eq!(head_b, head_a);
    let problems: i64 =
        sqlx::query_scalar("SELECT count(*) FROM cmdb.audit_log_verify()").fetch_one(&mut *cb).await.unwrap();
    assert_eq!(problems, 0, "the restored audit chain verifies");
    // The restored schema grants the same: read, nothing else.
    let (read, write): (bool, bool) = sqlx::query_as(
        "SELECT has_table_privilege($1, 'cmdb.audit_log_chain_head', 'SELECT'),
                has_table_privilege($1, 'cmdb.audit_log_chain_head', 'INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER')",
    )
    .bind(&roles.app)
    .fetch_one(&mut *cb)
    .await
    .unwrap();
    assert_eq!((read, write), (true, false));

    drop((ca, cb));
    api.close().await;
    a.drop().await;
    b.drop().await;
    roles.drop().await;
}

/// SHAA-714 §6.3: a backup keeps the import switch, saved mappings and job
/// records, never uploaded files, issue rows or idempotency keys. A job that
/// had not finished is expired by the restore itself (T24).
#[tokio::test]
async fn import_files_stay_out_of_backups_and_unfinished_jobs_expire_on_restore() {
    let Some(a) = scratch::database("import_backup_a").await else { return };
    let Some(b) = scratch::database("import_backup_b").await else { return };
    populate(&a.pool).await;
    let mut ca = a.pool.acquire().await.unwrap();
    let mut cb = b.pool.acquire().await.unwrap();
    let user: uuid::Uuid = sqlx::query_scalar("SELECT id FROM cmdb.users LIMIT 1").fetch_one(&mut *ca).await.unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "UPDATE cmdb.import_settings SET enabled = true;
         INSERT INTO cmdb.import_mappings (name, class_key, definition, created_by_name, updated_by_name)
           VALUES ('Vendor export', 'server', '{{\"columns\": []}}', 'admin', 'admin');
         INSERT INTO cmdb.import_jobs (id, created_by_id, created_by_name, status, phase, file_name, file_format,
             file_size, file_sha256, expires_at, lease_owner, lease_until)
           VALUES ('00000000-0000-4000-8000-0000000000c1', '{user}', 'admin', 'committing', 'commit', 'a.csv', 'csv',
             3, repeat('a', 64), now() + interval '1 day', 'worker-1', now() + interval '1 minute'),
                  ('00000000-0000-4000-8000-0000000000c2', '{user}', 'admin', 'completed', NULL, 'b.csv', 'csv',
             3, repeat('b', 64), now() + interval '1 day', NULL, NULL);
         INSERT INTO cmdb.import_job_files (job_id, seq, data)
           VALUES ('00000000-0000-4000-8000-0000000000c1', 0, '\\x613b62'::bytea);
         INSERT INTO cmdb.import_job_issues (job_id, seq, row_no, severity, code, message, phase)
           VALUES ('00000000-0000-4000-8000-0000000000c1', 0, 2, 'error', 'required', 'Name is required', 'validate');
         INSERT INTO cmdb.import_idempotency_keys (user_id, key, operation, job_id)
           VALUES ('{user}', 'k1', 'commit', '00000000-0000-4000-8000-0000000000c1');"
    )))
    .execute(&mut *ca)
    .await
    .unwrap();

    let (buf, header) = take_backup(&mut ca).await;
    let rows = |name: &str| header.tables.iter().find(|t| t.schema == "cmdb" && t.name == name).map(|t| t.rows);
    assert_eq!((rows("import_jobs"), rows("import_mappings"), rows("import_settings")), (Some(2), Some(1), Some(1)));
    for gone in ["import_job_files", "import_job_issues", "import_idempotency_keys"] {
        assert_eq!(rows(gone), None, "{gone} is not backed up");
    }
    restore::restore(&mut cb, buf.as_slice(), &header, true, true).await.unwrap();

    let jobs: Vec<(String, Option<String>, bool)> =
        sqlx::query_as("SELECT status, lease_owner, finished_at IS NOT NULL FROM cmdb.import_jobs ORDER BY file_name")
            .fetch_all(&mut *cb)
            .await
            .unwrap();
    assert_eq!(jobs, [("expired".to_owned(), None, true), ("completed".to_owned(), None, false)]);
    let (enabled, mappings, transient): (bool, i64, i64) = sqlx::query_as(
        "SELECT (SELECT enabled FROM cmdb.import_settings), (SELECT count(*) FROM cmdb.import_mappings),
                (SELECT count(*) FROM cmdb.import_job_files) + (SELECT count(*) FROM cmdb.import_job_issues)
                + (SELECT count(*) FROM cmdb.import_idempotency_keys)",
    )
    .fetch_one(&mut *cb)
    .await
    .unwrap();
    assert_eq!((enabled, mappings, transient), (true, 1, 0));
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
    let original = fingerprints(&mut c).await;
    sqlx::query("UPDATE owners SET name = name || ' (changed)'").execute(&mut *c).await.unwrap();
    sqlx::query("UPDATE infrastruktur.server SET cpu_cores = coalesce(cpu_cores, 0) + 1")
        .execute(&mut *c)
        .await
        .unwrap();
    let before = fingerprints(&mut c).await;
    assert_ne!(before, original);

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

    // Replacing a populated install for real: its area schema is dropped and rebuilt.
    restore::restore(&mut c, buf.as_slice(), &header, true, true).await.unwrap();
    assert_eq!(fingerprints(&mut c).await, original);

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
    // As `shadoucmdb migrate` leaves it: types a migration added have their tables.
    let ctx = crate::api::context::RequestContext::system("test", "test");
    crate::schema::reconcile(&mut ca, &ctx, "Reconcile after migrate").await.map_err(|e| e.message).unwrap();
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

/// Releases at levels 0014–0020 left `oidc_login_states` out of their backups;
/// migration 0021 dropped the table, but restoring such a backup creates it
/// again on the way up and must not refuse it as missing.
#[tokio::test]
async fn a_backup_from_before_the_stateless_oidc_start_still_restores() {
    let Some(a) = scratch::database("restore_level_20").await else { return };
    let Some(b) = scratch::database("restore_level_20_b").await else { return };
    let mut ca = a.pool.acquire().await.unwrap();
    reset::decommission(&mut ca).await.unwrap();
    MIGRATOR.run_to(20, &mut *ca).await.unwrap();
    let (buf, header) = take_backup(&mut ca).await;
    assert_eq!(header.migration_level(), Some(20));
    assert!(header.excluded_tables.contains(&"cmdb.oidc_login_states".to_owned()), "{:?}", header.excluded_tables);
    assert!(!header.tables.iter().any(|t| t.name == "oidc_login_states"));

    let mut cb = b.pool.acquire().await.unwrap();
    let report = restore::restore(&mut cb, buf.as_slice(), &header, true, true).await.unwrap();
    assert!(report.migrations_applied_after >= 1);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM public._sqlx_migrations").fetch_one(&mut *cb).await.unwrap();
    assert_eq!(n as usize, crate::db::expected_count());
    let gone: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('cmdb.oidc_login_states')::text").fetch_one(&mut *cb).await.unwrap();
    assert_eq!(gone, None, "migration 0021 dropped it again");

    drop((ca, cb));
    a.drop().await;
    b.drop().await;
}

#[tokio::test]
async fn factory_reset_returns_to_first_run_and_decommission_leaves_nothing() {
    let Some(a) = scratch::database("factory_reset").await else { return };
    populate(&a.pool).await;
    let mut c = a.pool.acquire().await.unwrap();

    reset::factory_reset(&mut c).await.unwrap();
    // As on a first install, only the built-in business service type is left (migration 0033).
    let (users, cis, classes, audit): (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM users), (SELECT count(*) FROM configuration_items),
                (SELECT count(*) FROM ci_classes WHERE system_role IS NULL), (SELECT count(*) FROM audit_log)",
    )
    .fetch_one(&mut *c)
    .await
    .unwrap();
    assert_eq!((users, cis, classes, audit), (0, 0, 0, 0));
    let (areas, changes): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM pg_namespace WHERE nspname = 'infrastruktur'),
                (SELECT count(*) FROM cmdb.schema_changes)",
    )
    .fetch_one(&mut *c)
    .await
    .unwrap();
    assert_eq!((areas, changes), (0, 0), "the area schema and its history are gone");
    // Bulk import comes back switched off and empty, with no reset code of its own (T24).
    let (enabled, import_rows): (bool, i64) = sqlx::query_as(
        "SELECT (SELECT enabled FROM cmdb.import_settings),
                (SELECT count(*) FROM cmdb.import_jobs) + (SELECT count(*) FROM cmdb.import_job_files)
                + (SELECT count(*) FROM cmdb.import_job_issues) + (SELECT count(*) FROM cmdb.import_mappings)
                + (SELECT count(*) FROM cmdb.import_idempotency_keys)",
    )
    .fetch_one(&mut *c)
    .await
    .unwrap();
    assert_eq!((enabled, import_rows), (false, 0));
    drop(c);
    assert!(crate::modules::auth::setup_required(&a.pool).await.unwrap(), "setup is forced");
    crate::seed::seed_system_rows(&a.pool).await.unwrap();

    crate::seed::install_template(&a.pool, "it_infrastructure").await.unwrap();
    let mut c = a.pool.acquire().await.unwrap();
    reset::decommission(&mut c).await.unwrap();
    assert_eq!(app_object_count(&mut c).await.unwrap(), 0);
    let left: i64 =
        sqlx::query_scalar("SELECT count(*) FROM pg_namespace WHERE nspname IN ('cmdb', 'infrastruktur', 'drizzle')")
            .fetch_one(&mut *c)
            .await
            .unwrap();
    assert_eq!(left, 0);
    // The empty database can be installed again.
    MIGRATOR.run(&mut *c).await.unwrap();
    assert!(app_object_count(&mut c).await.unwrap() > 0);

    drop(c);
    a.drop().await;
}
