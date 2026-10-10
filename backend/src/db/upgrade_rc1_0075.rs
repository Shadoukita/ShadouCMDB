//! QA (SHAA-2849, #830/#842): a v0.1.0-rc.1 install (migration 0006) with
//! data upgraded straight to the workflow actions schema (0073-0075). The
//! CIs, relationships, accounts and their rights survive; nobody gains
//! `webhooks.manage`; every account follows the server's e-mail language;
//! the action tables start empty with their one queue state row; and a
//! migrated account signs in and sets its e-mail language.

use serde_json::json;
use sqlx::PgPool;

use crate::db::{MIGRATOR, reconcile_and_link, scratch};
use crate::modules::api_tokens::tests::{Creds, app, call};

async fn rows(pool: &PgPool, sql: &str) -> Vec<String> {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_all(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

async fn count(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

#[tokio::test]
async fn a_populated_rc1_install_upgrades_to_workflow_actions() {
    let Some(db) = scratch::empty("upgrade_rc1_0075").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(6, pool).await.expect("migrations up to 0006 (v0.1.0-rc.1)");

    let password = format!("rc1 passphrase {}", uuid::Uuid::new_v4());
    let hash = crate::auth::password::hash(&password).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO statuses (key, name, is_operational) VALUES ('live', 'Live', true);
         INSERT INTO ci_classes (id, key, name) VALUES
           ('00000000-0000-4000-8000-0000000000c1', 'server', 'Server'),
           ('00000000-0000-4000-8000-0000000000c2', 'application', 'Application'),
           ('00000000-0000-4000-8000-0000000000c3', 'database', 'Database');
         INSERT INTO configuration_items (id, class_id, name, status_id, hostname, ip_address)
           SELECT v.id::uuid, v.class::uuid, v.name, s.id, v.host, v.ip::inet FROM statuses s, (VALUES
             ('00000000-0000-4000-8000-000000000101', '00000000-0000-4000-8000-0000000000c1', 'srv-01', 'srv-01', '10.0.0.1'),
             ('00000000-0000-4000-8000-000000000201', '00000000-0000-4000-8000-0000000000c2', 'Billing', NULL, NULL),
             ('00000000-0000-4000-8000-000000000301', '00000000-0000-4000-8000-0000000000c3', 'billing-db', NULL, NULL))
           AS v(id, class, name, host, ip);
         INSERT INTO relationship_types (id, key, name, forward_label, reverse_label) VALUES
           ('00000000-0000-4000-8000-0000000000d1', 'runs_on', 'Runs on', 'runs on', 'runs'),
           ('00000000-0000-4000-8000-0000000000d2', 'uses', 'Uses', 'uses', 'used by');
         INSERT INTO relationship_type_rules (relationship_type_id, source_class_id, target_class_id) VALUES
           ('00000000-0000-4000-8000-0000000000d1', '00000000-0000-4000-8000-0000000000c2', '00000000-0000-4000-8000-0000000000c1'),
           ('00000000-0000-4000-8000-0000000000d2', '00000000-0000-4000-8000-0000000000c2', '00000000-0000-4000-8000-0000000000c3');
         INSERT INTO ci_relationships (relationship_type_id, source_ci_id, target_ci_id) VALUES
           ('00000000-0000-4000-8000-0000000000d1', '00000000-0000-4000-8000-000000000201', '00000000-0000-4000-8000-000000000101'),
           ('00000000-0000-4000-8000-0000000000d2', '00000000-0000-4000-8000-000000000201', '00000000-0000-4000-8000-000000000301');
         INSERT INTO permission_profiles (id, name) VALUES ('00000000-0000-4000-8000-0000000000e1', 'Operators');
         INSERT INTO permission_profile_global_permissions (profile_id, permission) VALUES
           ('00000000-0000-4000-8000-0000000000e1', 'users.manage'),
           ('00000000-0000-4000-8000-0000000000e1', 'audit.view');
         INSERT INTO permission_profile_class_permissions (profile_id, class_id, can_view, can_edit) VALUES
           ('00000000-0000-4000-8000-0000000000e1', '00000000-0000-4000-8000-0000000000c1', true, true);",
    )
    .execute(pool)
    .await
    .expect("rc.1 data");
    sqlx::query(
        "INSERT INTO users (username, display_name, email, password_hash) VALUES
           ('admin', 'Ada Admin', 'admin@example.test', $1), ('operator', 'Otto Operator', 'otto@example.test', $1);
         ",
    )
    .bind(&hash)
    .execute(pool)
    .await
    .unwrap();
    sqlx::raw_sql(
        "INSERT INTO user_permission_profiles (user_id, profile_id)
           SELECT id, (SELECT id FROM permission_profiles WHERE is_builtin) FROM users WHERE username = 'admin';
         INSERT INTO user_permission_profiles (user_id, profile_id)
           SELECT id, '00000000-0000-4000-8000-0000000000e1' FROM users WHERE username = 'operator';",
    )
    .execute(pool)
    .await
    .unwrap();

    MIGRATOR.run(pool).await.expect("migrations from 0006 to current");
    reconcile_and_link(pool).await.expect("reconcile after the upgrade");
    let applied: i64 = count(pool, "SELECT max(version) FROM _sqlx_migrations").await;
    assert!(applied >= 75, "at least 0075 applied: {applied}");

    // The data and the rights survive.
    assert_eq!(
        rows(
            pool,
            "SELECT id::text FROM configuration_items WHERE deleted_at IS NULL
             AND class_id::text LIKE '00000000-0000-4000-8000-0000000000c_' ORDER BY id"
        )
        .await,
        [
            "00000000-0000-4000-8000-000000000101",
            "00000000-0000-4000-8000-000000000201",
            "00000000-0000-4000-8000-000000000301"
        ]
    );
    assert_eq!(count(pool, "SELECT count(*) FROM ci_relationships WHERE deleted_at IS NULL").await, 2);
    assert_eq!(
        count(pool, "SELECT count(*) FROM users WHERE person_ci_id IS NOT NULL").await,
        2,
        "each account linked to its Person"
    );
    assert_eq!(
        rows(pool, "SELECT username || ':' || coalesce(locale, '-') FROM users ORDER BY username").await,
        ["admin:-", "operator:-"],
        "every account follows the server's e-mail language"
    );
    assert_eq!(
        rows(
            pool,
            "SELECT permission FROM permission_profile_global_permissions
             WHERE profile_id = '00000000-0000-4000-8000-0000000000e1' ORDER BY 1"
        )
        .await,
        ["audit.view", "users.manage"],
        "the operators' rights are unchanged"
    );
    assert_eq!(
        count(pool, "SELECT count(*) FROM permission_profile_global_permissions WHERE permission = 'webhooks.manage'")
            .await,
        0,
        "nobody gains webhooks.manage"
    );

    // The action tables start empty, with their queue state.
    for table in
        ["workflow_actions", "workflow_action_recipients", "workflow_action_runs", "workflow_action_deliveries"]
    {
        assert_eq!(count(pool, &format!("SELECT count(*) FROM {table}")).await, 0, "{table}");
    }
    assert_eq!(count(pool, "SELECT count(*) FROM workflow_action_queue_state").await, 1);

    // A migrated account signs in, holds its rights and sets its e-mail language.
    let app = app(pool.clone());
    let body = json!({ "username": "operator", "password": password });
    let (status, me, headers) = call(&app, "POST", "/api/v1/auth/login", &Creds::default(), Some(body)).await;
    assert_eq!(status, 200, "{me}");
    assert_eq!(me["locale"], json!(null));
    let global = me["permissions"]["global"].to_string();
    assert!(global.contains("users.manage") && !global.contains("webhooks.manage"), "{me}");
    let session = crate::modules::api_tokens::tests::session_of(&me, &headers);
    let (status, v, _) = call(&app, "PATCH", "/api/v1/auth/me", &session, Some(json!({ "locale": "de" }))).await;
    assert_eq!((status, &v["locale"]), (200, &json!("de")), "{v}");
    assert_eq!(
        count(
            pool,
            "SELECT count(*) FROM audit_log WHERE entity_type = 'users' AND new_value = '{\"locale\": \"de\"}'"
        )
        .await,
        1
    );
    let problems = rows(pool, "SELECT chain_seq || ' ' || problem || ': ' || detail FROM audit_log_verify()").await;
    assert_eq!(problems, Vec::<String>::new(), "the audit chain verifies");
    db.drop().await;
}
