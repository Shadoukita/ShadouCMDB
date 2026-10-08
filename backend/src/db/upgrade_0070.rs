//! Migration 0070 (CI notes, SHAA-2355) on a populated three-role install at
//! 0069: the file is additive, so the CIs and accounts are untouched and the
//! CI table is not rewritten; the policy starts as its one default row; the
//! API role writes notes but never removes the policy; a note goes with its CI
//! and keeps its author's name when the account is deleted.

use sqlx::PgPool;
use uuid::Uuid;

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

fn sql_state(err: &sqlx::Error) -> String {
    err.as_database_error().and_then(|d| d.code()).unwrap_or_default().into_owned()
}

#[tokio::test]
async fn the_upgrade_adds_notes_to_a_populated_install_and_changes_no_data() {
    const TEST: &str = "the_upgrade_adds_notes_to_a_populated_install_and_changes_no_data";
    let Some(roles) = scratch::Roles::create(TEST).await else { return };
    let db = roles.empty().await;
    let pool = &db.pool;
    let mut migrator = pool.acquire().await.unwrap();
    MIGRATOR.run_to(69, &mut *migrator).await.expect("migrations up to 0069");
    reconcile_and_link(pool).await.expect("reconcile at 0069");

    // 500 CIs and an account, as a running install has them.
    let class: Uuid = scalar(pool, "SELECT id FROM ci_classes WHERE system_role = 'person'").await;
    sqlx::query(
        "INSERT INTO configuration_items (class_id, ident, label)
         SELECT $1, 'P-' || g, 'Person ' || g FROM generate_series(1, 500) g",
    )
    .bind(class)
    .execute(pool)
    .await
    .unwrap();
    let author: Uuid = scalar(
        pool,
        "INSERT INTO users (username, display_name, password_hash)
         VALUES ('noter', 'Noter', '$argon2id$v=19$test') RETURNING id",
    )
    .await;
    let before: (i64, String, i64) = sqlx::query_as(
        "SELECT count(*), md5(string_agg(id::text || ident || label || version::text, ',' ORDER BY id)),
                pg_relation_filenode('cmdb.configuration_items')::bigint
         FROM configuration_items",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    let users: i64 = scalar(pool, "SELECT count(*) FROM users").await;

    MIGRATOR.run(&mut *migrator).await.expect("upgrade to the latest version");
    drop(migrator);
    reconcile_and_link(pool).await.expect("reconcile after the upgrade");

    let after: (i64, String, i64) = sqlx::query_as(
        "SELECT count(*), md5(string_agg(id::text || ident || label || version::text, ',' ORDER BY id)),
                pg_relation_filenode('cmdb.configuration_items')::bigint
         FROM configuration_items",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(after, before, "the CIs are unchanged and their table not rewritten");
    assert_eq!(scalar::<i64>(pool, "SELECT count(*) FROM users").await, users);
    assert_eq!(scalar::<i64>(pool, "SELECT count(*) FROM ci_notes").await, 0);
    let policy: (Option<i32>, Option<i32>) =
        sqlx::query_as("SELECT edit_window_minutes, retention_days FROM ci_note_settings")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(policy, (Some(1440), None), "one row: 24 hours, kept until deleted");

    // The API role writes notes on the existing CIs and changes the policy,
    // but never removes it, and there is never a second row.
    let api = roles.api_pool(&db).await;
    let ci: Uuid = scalar(&api, "SELECT id FROM configuration_items WHERE ident = 'P-1'").await;
    let note: Uuid = sqlx::query_scalar(
        "INSERT INTO ci_notes (ci_id, body, author_id, author_name) VALUES ($1, 'Badge renewed', $2, 'Noter')
         RETURNING id",
    )
    .bind(ci)
    .bind(author)
    .fetch_one(&api)
    .await
    .unwrap();
    sqlx::query("UPDATE ci_notes SET body = 'Badge renewed until 2027', edited_at = now(), version = 2 WHERE id = $1")
        .bind(note)
        .execute(&api)
        .await
        .unwrap();
    sqlx::query("UPDATE ci_note_settings SET retention_days = 90").execute(&api).await.unwrap();
    let err = sqlx::query("DELETE FROM ci_note_settings").execute(&api).await.unwrap_err();
    assert_eq!(sql_state(&err), "42501", "{err}");
    let err = sqlx::query("INSERT INTO ci_note_settings (id) VALUES (false)").execute(&api).await.unwrap_err();
    assert_eq!(sql_state(&err), "23514", "{err}");
    let err = sqlx::query("INSERT INTO ci_note_settings DEFAULT VALUES").execute(&api).await.unwrap_err();
    assert_eq!(sql_state(&err), "23505", "{err}");
    let err = sqlx::query("INSERT INTO ci_notes (ci_id, body, author_name) VALUES ($1, '   ', 'Noter')")
        .bind(ci)
        .execute(&api)
        .await
        .unwrap_err();
    assert_eq!(sql_state(&err), "23514", "a blank note: {err}");
    api.close().await;

    // Deleting the account keeps the note and its author's name; removing the
    // CI row removes its notes.
    sqlx::query("DELETE FROM users WHERE id = $1").bind(author).execute(pool).await.unwrap();
    let kept: (Option<Uuid>, String) = sqlx::query_as("SELECT author_id, author_name FROM ci_notes WHERE id = $1")
        .bind(note)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(kept, (None, "Noter".to_owned()));
    sqlx::query("DELETE FROM configuration_items WHERE id = $1").bind(ci).execute(pool).await.unwrap();
    assert_eq!(scalar::<i64>(pool, "SELECT count(*) FROM ci_notes").await, 0);

    db.drop().await;
    roles.drop().await;
}
