//! Migration 0044 (users and Person CIs, SHAA-1505/SHAA-1508) on a populated
//! v0.4 database: the Person type is created next to a customer's own
//! "person" type, `migrate` builds its table and links every account with an
//! e-mail (audited), accounts without one are left for their next sign-in,
//! and a second run changes nothing. Shared e-mails, and e-mails or display
//! names the Person type refuses, stop the upgrade with the list, and nothing
//! changes. An account refused after 0044 is reported, the others are linked.

use sqlx::{Executor, PgPool};
use uuid::Uuid;

use crate::db::{MIGRATOR, reconcile_and_link, refuse_accounts_person_refuses, scratch};

/// A v0.4 install (migration 0043) with a type keyed "person" in an area keyed
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
    let Some(db) = scratch::empty("upgrade_0044_links_accounts").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(43, pool).await.expect("migrations up to 0043");
    pool.execute(V04).await.expect("v0.4 data");

    MIGRATOR.run(pool).await.expect("migration 0044");
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
    let (change, links) = reconcile_and_link(pool).await.expect("reconcile and link");
    assert_eq!((links.linked, links.refused.len()), (2, 0), "alice and carol");
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
    let (change, links) = reconcile_and_link(pool).await.expect("again");
    assert!(change.is_none());
    assert_eq!(links.linked, 0);
    let persons: i64 = scalar(pool, "SELECT count(*) FROM people_2.person_2").await;
    assert_eq!(persons, 2);

    // Bob enters his e-mail at his next sign-in; until then he is listed as such.
    let bob: Option<Uuid> = scalar(pool, "SELECT person_ci_id FROM users WHERE username = 'bob'").await;
    assert_eq!(bob, None);

    db.drop().await;
}

#[tokio::test]
async fn shared_emails_stop_the_upgrade_with_the_list() {
    let Some(db) = scratch::empty("upgrade_0044_shared_emails").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(43, pool).await.expect("migrations up to 0043");
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

    // A failed run returns without releasing sqlx's session-level migration
    // lock. `migrate` exits, which ends the session; here the connection is
    // closed, or the next run would wait on the lock from another one.
    let mut conn = pool.acquire().await.unwrap();
    let err = MIGRATOR.run(&mut *conn).await.expect_err("shared e-mails stop the upgrade").to_string();
    conn.close().await.unwrap();
    assert!(err.contains("ops@example.test (users: ops, Ops2); svc@example.test (users: backup, monitor)"), "{err}");
    assert!(!err.contains("dana"), "{err}");
    assert!(err.contains("Nothing was changed"), "{err}");

    // Nothing changed: 0044 is not recorded and none of its objects exist.
    let applied: i64 = scalar(pool, "SELECT max(version) FROM _sqlx_migrations WHERE success").await;
    assert_eq!(applied, 43);
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
    MIGRATOR.run(pool).await.expect("migration 0044");
    let (_, links) = reconcile_and_link(pool).await.unwrap();
    assert_eq!(links.linked, 5);

    db.drop().await;
}

/// An e-mail of 255-320 characters (accepted before 0044) or a display name
/// the Person's Name refuses (GH#543).
fn long_email() -> String {
    format!("{}@example.test", "a".repeat(247))
}

#[tokio::test]
async fn emails_the_person_type_refuses_stop_the_upgrade_with_the_list() {
    let Some(db) = scratch::empty("upgrade_0044_refused_emails").await else { return };
    let pool = &db.pool;
    // The oldest release (v0.1.0-rc.1, migration 0006): the accounts are still
    // in `public`, the `cmdb` schema comes with 0008.
    MIGRATOR.run_to(6, pool).await.expect("migrations up to 0006");
    sqlx::query(
        "INSERT INTO users (username, display_name, email, password_hash) VALUES
           ('admin', 'Ada Admin', 'Ada.Admin@Acme.test', '$argon2id$x'),
           ('longmail', 'Long Mail', $1, '$argon2id$x'),
           ('longname', $2, 'longname@example.test', '$argon2id$x'),
           ('noemail', $2, NULL, '$argon2id$x')",
    )
    .bind(long_email())
    .bind("n".repeat(201))
    .execute(pool)
    .await
    .unwrap();

    let err = refuse_accounts_person_refuses(pool).await.expect_err("refused").to_string();
    assert!(err.contains("  longmail: e-mail has 260 characters, at most 254\n"), "{err}");
    assert!(err.contains("  longname: display name has 201 characters, at most 200\n"), "{err}");
    assert!(!err.contains("admin:") && !err.contains("noemail"), "{err}");
    assert!(err.contains("Nothing was changed"), "{err}");

    pool.execute("UPDATE users SET email = 'long@example.test' WHERE username = 'longmail'").await.unwrap();
    pool.execute("UPDATE users SET display_name = 'Long Name' WHERE username = 'longname'").await.unwrap();
    refuse_accounts_person_refuses(pool).await.expect("every account fits");

    db.drop().await;
}

#[tokio::test]
async fn an_account_the_person_type_refuses_does_not_lock_out_the_others() {
    let Some(db) = scratch::empty("upgrade_0044_refused_link").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(43, pool).await.expect("migrations up to 0043");
    sqlx::query(
        "INSERT INTO users (username, display_name, email, password_hash) VALUES
           ('admin', 'Ada Admin', 'Ada.Admin@Acme.test', '$argon2id$x'),
           ('longmail', 'Long Mail', $1, '$argon2id$x'),
           ('carol', 'Carol', 'carol@example.test', '$argon2id$x')",
    )
    .bind(long_email())
    .execute(pool)
    .await
    .unwrap();
    // 0044 applied without the pre-check, as a build before GH#543 did.
    MIGRATOR.run(pool).await.expect("migration 0044");

    let (_, links) = reconcile_and_link(pool).await.expect("reconcile and link");
    assert_eq!(links.linked, 2, "admin and carol");
    assert_eq!(links.refused.len(), 1, "{:?}", links.refused);
    assert!(links.refused[0].starts_with("longmail (email: "), "{:?}", links.refused);
    let linked: Vec<(String, bool)> =
        sqlx::query_as("SELECT username, person_ci_id IS NOT NULL FROM users ORDER BY username")
            .fetch_all(pool)
            .await
            .unwrap();
    assert_eq!(linked, [("admin".into(), true), ("carol".into(), true), ("longmail".into(), false)]);
    // Only the linked accounts are audited.
    let audited: i64 =
        scalar(pool, "SELECT count(*) FROM audit_log WHERE actor_name = 'migrate' AND entity_type = 'users'").await;
    assert_eq!(audited, 2);

    // Running it again reports the account again and changes nothing else.
    let (_, links) = reconcile_and_link(pool).await.expect("again");
    assert_eq!((links.linked, links.refused.len()), (0, 1));

    db.drop().await;
}
