//! Migration 0043 (users and Person CIs, SHAA-1505/SHAA-1508) on a populated
//! v0.4 database: the Person type is created next to a customer's own
//! "person" type, `migrate` builds its table and links every account with an
//! e-mail (audited), accounts without one are left for their next sign-in,
//! and a second run changes nothing. Shared e-mails stop the upgrade with
//! the list, and nothing changes.

use sqlx::{Executor, PgPool};
use uuid::Uuid;

use crate::db::{MIGRATOR, reconcile_and_link, scratch};

/// A v0.4 install (migration 0042) with a type keyed "person" in an area keyed
/// "people", and accounts with and without e-mails.
const V04: &str = "
INSERT INTO areas (id, key, name) VALUES ('00000000-0000-4000-8000-0000000000a1', 'people', 'Staff');
INSERT INTO ci_classes (id, key, name, area_id) VALUES
  ('00000000-0000-4000-8000-0000000000c1', 'person', 'Contact', '00000000-0000-4000-8000-0000000000a1');
INSERT INTO ci_attribute_definitions (class_id, key, label, data_type, is_required, sort_order)
  VALUES ('00000000-0000-4000-8000-0000000000c1', 'name', 'Name', 'text', true, 0);
INSERT INTO users (id, username, display_name, email, password_hash) VALUES
  ('00000000-0000-4000-8000-000000000001', 'alice', 'Alice Admin', 'Alice@Example.test', '$argon2id$x'),
  ('00000000-0000-4000-8000-000000000002', 'bob', 'Bob', NULL, '$argon2id$x'),
  ('00000000-0000-4000-8000-000000000003', 'carol', 'Carol', 'carol@example.test', '$argon2id$x');
";

async fn scalar<T>(pool: &PgPool, sql: &str) -> T
where
    T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin,
{
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

#[tokio::test]
async fn upgrade_links_every_account_with_an_email_to_a_person() {
    let Some(db) = scratch::empty("upgrade_0043_links_accounts").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(42, pool).await.expect("migrations up to 0042");
    pool.execute(V04).await.expect("v0.4 data");

    MIGRATOR.run(pool).await.expect("migration 0043");
    // The customer's "person" type and "people" area keep their keys; the built-in ones step aside.
    let (key, area, name): (String, String, String) = sqlx::query_as(
        "SELECT c.key, a.key, c.name FROM ci_classes c JOIN areas a ON a.id = c.area_id WHERE c.system_role = 'person'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!((key.as_str(), area.as_str(), name.as_str()), ("person_2", "people_2", "Person"));
    let roles: Vec<(String, String)> = sqlx::query_as(
        "SELECT key, system_role FROM ci_attribute_definitions WHERE system_role IS NOT NULL ORDER BY key",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(roles, [("email".into(), "person_email".into()), ("name".into(), "person_name".into())]);

    // What `migrate` does next: build the table, link the accounts.
    let (change, linked) = reconcile_and_link(pool).await.expect("reconcile and link");
    assert_eq!(linked, 2, "alice and carol");
    let statements = change.unwrap().statements;
    assert!(statements.iter().any(|s| s.starts_with("CREATE UNIQUE INDEX \"uq_")), "{statements:?}");

    let linked: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT u.username, ci.label, p.email FROM users u
         LEFT JOIN configuration_items ci ON ci.id = u.person_ci_id
         LEFT JOIN people_2.person_2 p ON p.id = u.person_ci_id ORDER BY u.username",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(
        linked,
        [
            ("alice".into(), Some("Alice Admin".into()), Some("Alice@Example.test".into())),
            ("bob".into(), None, None),
            ("carol".into(), Some("Carol".into()), Some("carol@example.test".into())),
        ]
    );
    // Audited: a Person created and an account updated (with its link) for each.
    let audited: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT entity_type, action, count(*) FROM audit_log
         WHERE actor_name = 'migrate' AND entity_type IN ('users', 'configuration_items')
         GROUP BY 1, 2 ORDER BY 1, 2",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(audited, [("configuration_items".into(), "create".into(), 2), ("users".into(), "update".into(), 2)]);
    let person_in_log: i64 = scalar(
        pool,
        "SELECT count(*) FROM audit_log WHERE entity_type = 'users' AND action = 'update'
           AND old_value -> 'person' = 'null' AND new_value -> 'person' ->> 'label' IS NOT NULL",
    )
    .await;
    assert_eq!(person_in_log, 2, "the link shows in the account's audit row");

    // A second run finds nothing to do.
    let (change, linked) = reconcile_and_link(pool).await.expect("again");
    assert!(change.is_none());
    assert_eq!(linked, 0);
    let persons: i64 = scalar(pool, "SELECT count(*) FROM people_2.person_2").await;
    assert_eq!(persons, 2);

    // Bob enters his e-mail at his next sign-in; until then he is listed as such.
    let bob: Option<Uuid> = scalar(pool, "SELECT person_ci_id FROM users WHERE username = 'bob'").await;
    assert_eq!(bob, None);

    db.drop().await;
}

#[tokio::test]
async fn shared_emails_stop_the_upgrade_with_the_list() {
    let Some(db) = scratch::empty("upgrade_0043_shared_emails").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(42, pool).await.expect("migrations up to 0042");
    pool.execute(
        "INSERT INTO users (username, display_name, email, password_hash) VALUES
           ('ops', 'Ops', 'ops@example.test', '$argon2id$x'),
           ('Ops2', 'Ops 2', 'OPS@example.test', '$argon2id$x'),
           ('backup', 'Backup', 'svc@example.test', '$argon2id$x'),
           ('monitor', 'Monitor', 'svc@example.test', '$argon2id$x'),
           ('dana', 'Dana', 'dana@example.test', '$argon2id$x')",
    )
    .await
    .unwrap();

    let err = MIGRATOR.run(pool).await.expect_err("shared e-mails stop the upgrade").to_string();
    assert!(err.contains("ops@example.test (users: ops, Ops2); svc@example.test (users: backup, monitor)"), "{err}");
    assert!(!err.contains("dana"), "{err}");
    assert!(err.contains("Nothing was changed"), "{err}");

    // Nothing changed: 0043 is not recorded and none of its objects exist.
    let applied: i64 = scalar(pool, "SELECT max(version) FROM _sqlx_migrations WHERE success").await;
    assert_eq!(applied, 42);
    let index: Option<String> = scalar(pool, "SELECT to_regclass('cmdb.users_email_uq')::text").await;
    assert_eq!(index, None);
    let column: bool = scalar(
        pool,
        "SELECT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_name = 'users' AND column_name = 'person_ci_id')",
    )
    .await;
    assert!(!column);

    // Once each account has its own address, the upgrade goes through.
    pool.execute("UPDATE users SET email = 'ops2@example.test' WHERE username = 'Ops2'").await.unwrap();
    pool.execute("UPDATE users SET email = 'monitor@example.test' WHERE username = 'monitor'").await.unwrap();
    MIGRATOR.run(pool).await.expect("migration 0043");
    let (_, linked) = reconcile_and_link(pool).await.unwrap();
    assert_eq!(linked, 5);

    db.drop().await;
}
