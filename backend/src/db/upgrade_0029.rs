//! Migration 0029 (bulk import, SHAA-799) against an install with profiles
//! holding every right and a varied audit log: nobody gains `cis.import`,
//! import starts switched off, every existing audit row still satisfies the
//! new constraints, and the permission constraint gains `cis.import`.

use std::collections::BTreeSet;

use sqlx::{Executor, PgPool};

use crate::db::{MIGRATOR, scratch};

const BEFORE: &str = "
INSERT INTO users (id, username, display_name, password_hash) VALUES
  ('00000000-0000-4000-8000-00000000000a', 'alice', 'Alice', '$argon2id$v=19$upgrade-test');
INSERT INTO permission_profiles (id, name) VALUES
  ('00000000-0000-4000-8000-0000000000f1', 'Everything but administrator'),
  ('00000000-0000-4000-8000-0000000000f2', 'Auditors');
INSERT INTO permission_profile_global_permissions (profile_id, permission)
  SELECT '00000000-0000-4000-8000-0000000000f1', p
  FROM unnest(ARRAY['users.manage', 'profiles.manage', 'datamodel.manage', 'customization.manage',
                    'config.export_import', 'audit.view']) p;
INSERT INTO permission_profile_global_permissions (profile_id, permission) VALUES
  ('00000000-0000-4000-8000-0000000000f2', 'audit.view');
INSERT INTO audit_log (actor_type, actor_id, actor_name, action, entity_type, entity_id, old_value, new_value) VALUES
  ('user', '00000000-0000-4000-8000-00000000000a', 'alice', 'create', 'users',
   '00000000-0000-4000-8000-00000000000a', NULL, '{\"username\": \"alice\"}'),
  ('user', '00000000-0000-4000-8000-00000000000a', 'alice', 'update', 'users',
   '00000000-0000-4000-8000-00000000000a', '{\"username\": \"alice\"}', '{\"username\": \"alice2\"}'),
  ('user', '00000000-0000-4000-8000-00000000000a', 'alice', 'login.success', 'sessions',
   gen_random_uuid(), NULL, '{}'),
  ('user', '00000000-0000-4000-8000-00000000000a', 'alice', 'token.use', 'api_tokens',
   gen_random_uuid(), NULL, '{}'),
  ('user', '00000000-0000-4000-8000-00000000000a', 'alice', 'mfa.enrol', 'users',
   '00000000-0000-4000-8000-00000000000a', NULL, '{}'),
  ('user', '00000000-0000-4000-8000-00000000000a', 'alice', 'schema_change.refused', 'ci_classes',
   gen_random_uuid(), NULL, '{}'),
  ('import', '00000000-0000-4000-8000-00000000000a', 'alice', 'create', 'configuration_items',
   gen_random_uuid(), NULL, '{}');
";

/// The quoted values of a `CHECK (col IN ('a', 'b', …))` constraint definition.
pub(crate) fn literals(def: &str) -> BTreeSet<String> {
    def.split('\'').skip(1).step_by(2).map(str::to_owned).collect()
}

pub(crate) async fn constraint_def(pool: &PgPool, table: &str, name: &str) -> String {
    sqlx::query_scalar(
        "SELECT pg_get_constraintdef(c.oid) FROM pg_constraint c JOIN pg_class t ON t.oid = c.conrelid
         JOIN pg_namespace n ON n.oid = t.relnamespace
         WHERE n.nspname = 'cmdb' AND t.relname = $1 AND c.conname = $2",
    )
    .bind(table)
    .bind(name)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn import_starts_off_nobody_gains_the_right_and_old_audit_rows_stay_valid() {
    let Some(db) = scratch::empty("import_starts_off_nobody_gains_the_right").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(28, pool).await.expect("migrations up to 0028");
    pool.execute(BEFORE).await.expect("data before the upgrade");
    let audit_before: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log").fetch_one(pool).await.unwrap();
    MIGRATOR.run_to(29, pool).await.expect("migration 0029");

    let granted: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM permission_profile_global_permissions WHERE permission = 'cis.import'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(granted, 0, "no profile gains cis.import");
    let rights: i64 =
        sqlx::query_scalar("SELECT count(*) FROM permission_profile_global_permissions").fetch_one(pool).await.unwrap();
    assert_eq!(rights, 7, "every existing grant is kept");

    let settings: Vec<(bool,)> = sqlx::query_as("SELECT enabled FROM import_settings").fetch_all(pool).await.unwrap();
    assert_eq!(settings, [(false,)], "exactly one settings row, switched off");
    assert!(sqlx::query("INSERT INTO import_settings (id) VALUES (false)").execute(pool).await.is_err());

    // The constraints were re-added over the existing rows, so they all still pass.
    let audit_after: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log").fetch_one(pool).await.unwrap();
    assert_eq!(audit_after, audit_before);
    let actions = literals(&constraint_def(pool, "audit_log", "audit_log_action_valid").await);
    for a in ["schema_change.refused", "token.use", "mfa.recovery_codes", "import.commit", "import.report_read"] {
        assert!(actions.contains(a), "{a}");
    }
    for (action, old, ok) in
        [("import.commit", None, true), ("import.report_read", None, true), ("import.commit", Some("{}"), false)]
    {
        let res = sqlx::query(
            "INSERT INTO audit_log (actor_type, actor_name, action, entity_type, entity_id, old_value, new_value)
             VALUES ('import', 'alice', $1, 'import_jobs', gen_random_uuid(), $2::jsonb, '{}')",
        )
        .bind(action)
        .bind(old)
        .execute(pool)
        .await;
        assert_eq!(res.is_ok(), ok, "{action} with old value {old:?}: {res:?}");
    }
    // The retention function still compiles and knows both scopes.
    for scope in ["auth", "changes"] {
        sqlx::query("SELECT * FROM prune_audit_log(interval '400 days', $1, true)")
            .bind(scope)
            .fetch_all(pool)
            .await
            .unwrap_or_else(|e| panic!("{scope}: {e}"));
    }

    // An upload in progress has no size or hash yet; a queued job has both.
    let job = |status: &str, size: i64, sha: Option<&str>| {
        sqlx::query(
            "INSERT INTO import_jobs (created_by_name, status, file_name, file_format, file_size, file_sha256, expires_at)
             VALUES ('alice', $1, 'a.csv', 'csv', $2, $3, now() + interval '1 day')",
        )
        .bind(status.to_owned())
        .bind(size)
        .bind(sha.map(str::to_owned))
        .execute(pool)
    };
    let sha = "0".repeat(64);
    assert!(job("uploading", 0, None).await.is_ok());
    assert!(job("queued", 0, Some(&sha)).await.is_err());
    assert!(job("queued", 10, None).await.is_err());
    assert!(job("queued", 10, Some(&sha)).await.is_ok());
    assert!(job("queued", 10, Some("xyz")).await.is_err());

    // The list of 0029; 0039 adds views.share (its test checks the full list, T8).
    let rights =
        constraint_def(pool, "permission_profile_global_permissions", "permission_profile_global_permissions_valid")
            .await;
    let expected = [
        "users.manage",
        "profiles.manage",
        "datamodel.manage",
        "customization.manage",
        "config.export_import",
        "audit.view",
        "cis.import",
    ];
    assert_eq!(literals(&rights), expected.into_iter().map(str::to_owned).collect::<BTreeSet<_>>());
    db.drop().await;
}
