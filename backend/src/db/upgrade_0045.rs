//! Migration 0045 (e-mails unique ignoring Unicode form, GH#531/SHAA-1523) on
//! a database at 0044 whose Person table is built: addresses that differ only
//! in Unicode form stop the upgrade with the list, accounts and Persons alike;
//! once each has its own address both unique indexes compare
//! `cmdb.email_key` (NFKC, then lower case).

use sqlx::{Executor, PgPool};

use crate::db::{MIGRATOR, reconcile_and_link, scratch};

async fn scalar<T>(pool: &PgPool, sql: &str) -> T
where
    T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin,
{
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

/// The definition of an index, by its name.
async fn indexdef(pool: &PgPool, name: &str) -> String {
    scalar(pool, &format!("SELECT indexdef FROM pg_indexes WHERE indexname = '{name}'")).await
}

#[tokio::test]
async fn look_alike_emails_stop_the_upgrade_then_both_indexes_use_the_key() {
    let Some(db) = scratch::empty("upgrade_0045_email_key").await else { return };
    let pool = &db.pool;
    // A failed run leaves its connection holding the migration lock, and its
    // transaction open until the connection is used again: every run and
    // every fix in between uses this one.
    let mut migrator = pool.acquire().await.unwrap();
    MIGRATOR.run_to(44, &mut *migrator).await.expect("migrations up to 0044");
    // The state 0044 and its reconcile left: the Person table with its index
    // on lower(email), as the engine builds it before cmdb.email_key exists.
    reconcile_and_link(pool).await.expect("reconcile at 0044");
    let (table, index): (String, String) = sqlx::query_as(
        "SELECT format('%I.%I', a.key, c.key), 'uq_' || replace(d.id::text, '-', '')
         FROM ci_attribute_definitions d JOIN ci_classes c ON c.id = d.class_id JOIN areas a ON a.id = c.area_id
         WHERE d.system_role = 'person_email'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert!(indexdef(pool, &index).await.contains("lower(email)"));
    assert!(indexdef(pool, "users_email_uq").await.contains("lower(email)"));

    // José written composed (NFC) and decomposed (NFD); Ａｎｎａ full-width.
    let class: uuid::Uuid = scalar(pool, "SELECT id FROM ci_classes WHERE system_role = 'person'").await;
    let data = format!(
        "ALTER TABLE users DISABLE TRIGGER users_person_link;
         INSERT INTO users (username, display_name, email, password_hash) VALUES
           ('jose', 'José', 'jos\u{e9}@example.test', '$argon2id$x'),
           ('jose2', 'José 2', 'JOSE\u{301}@example.test', '$argon2id$x'),
           ('erin', 'Erin', 'erin@example.test', '$argon2id$x');
         INSERT INTO configuration_items (id, class_id, ident, label) VALUES
           ('00000000-0000-4000-8000-0000000000e1', '{class}', 'P-1', 'Anna'),
           ('00000000-0000-4000-8000-0000000000e2', '{class}', 'P-2', 'Anna (copy)');
         INSERT INTO {table} (id, name, email) VALUES
           ('00000000-0000-4000-8000-0000000000e1', 'Anna', 'anna@example.test'),
           ('00000000-0000-4000-8000-0000000000e2', 'Anna (copy)', '\u{ff21}\u{ff4e}\u{ff4e}\u{ff41}@example.test');
         ALTER TABLE users ENABLE TRIGGER users_person_link;"
    );
    pool.execute(sqlx::AssertSqlSafe(data)).await.expect("0044 data");

    // Accounts first: the list names them, nothing changes.
    let err = MIGRATOR.run(&mut *migrator).await.expect_err("look-alike accounts stop the upgrade").to_string();
    assert!(err.contains("(users: jose, jose2)"), "{err}");
    assert!(!err.contains("erin"), "{err}");
    let applied: i64 = scalar(pool, "SELECT max(version) FROM _sqlx_migrations WHERE success").await;
    assert_eq!(applied, 44);
    assert!(indexdef(pool, "users_email_uq").await.contains("lower(email)"));
    let key: Option<String> = scalar(pool, "SELECT to_regprocedure('cmdb.email_key(text)')::text").await;
    assert_eq!(key, None, "nothing was changed");

    // Then the Persons, by CI ident.
    migrator.execute("DELETE FROM users WHERE username = 'jose2'").await.unwrap();
    let err = MIGRATOR.run(&mut *migrator).await.expect_err("look-alike Persons stop the upgrade").to_string();
    assert!(err.contains("(people: P-1, P-2)"), "{err}");
    assert!(err.contains("Nothing was changed"), "{err}");
    assert!(indexdef(pool, &index).await.contains("lower(email)"));

    // Once each has its own address, the upgrade goes through.
    let fix = format!(
        "UPDATE {table} SET email = 'anna.copy@example.test' WHERE id = '00000000-0000-4000-8000-0000000000e2'"
    );
    migrator.execute(sqlx::AssertSqlSafe(fix)).await.unwrap();
    MIGRATOR.run(&mut *migrator).await.expect("migration 0045");
    assert!(indexdef(pool, "users_email_uq").await.contains("email_key(email)"));
    assert!(indexdef(pool, &index).await.contains("email_key(email)"));
    let (change, _) = reconcile_and_link(pool).await.expect("reconcile after 0045");
    assert!(change.is_none(), "the engine's index matches the migrated one");

    // Look-alikes are now one address in the database, for any write path.
    let err = pool
        .execute("INSERT INTO users (username, display_name, email, password_hash) VALUES ('j3', 'J', 'JOSE\u{301}@EXAMPLE.TEST', '$argon2id$x')")
        .await
        .unwrap_err();
    assert_eq!(err.as_database_error().unwrap().constraint(), Some("users_email_uq"));
    let dup =
        format!("UPDATE {table} SET email = 'ANNA@example.test' WHERE id = '00000000-0000-4000-8000-0000000000e2'");
    let err = pool.execute(sqlx::AssertSqlSafe(dup)).await.unwrap_err();
    assert_eq!(err.as_database_error().unwrap().constraint(), Some(index.as_str()));
    let full_width = format!(
        "UPDATE {table} SET email = '\u{ff41}\u{ff4e}\u{ff4e}\u{ff41}@example.test' WHERE id = '00000000-0000-4000-8000-0000000000e2'"
    );
    let err = pool.execute(sqlx::AssertSqlSafe(full_width)).await.unwrap_err();
    assert_eq!(err.as_database_error().unwrap().constraint(), Some(index.as_str()));

    // The pool closes, and the database drops, only once every connection is back.
    drop(migrator);
    db.drop().await;
}
