//! Migration 0072 (in-app notifications, SHAA-2356) on a populated three-role
//! install at 0071: the file is additive, so the CIs, accounts and import jobs
//! are untouched and nothing is backfilled for what happened before the
//! upgrade; an import that was running when the server was upgraded notifies
//! its creator once it ends, but only an active creator, and not when it is
//! cancelled or expires; the API role reads, marks and prunes notifications;
//! they go with their user.

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

const FINGERPRINT: &str = "SELECT (SELECT count(*) FROM configuration_items),
            (SELECT pg_relation_filenode('cmdb.configuration_items')::bigint),
            (SELECT md5(string_agg(t::text, '|' ORDER BY t.id)) FROM users t),
            (SELECT md5(string_agg(t::text, '|' ORDER BY t.id)) FROM import_jobs t)";

type Fingerprint = (i64, i64, String, String);

#[tokio::test]
async fn the_upgrade_adds_notifications_without_a_backfill_and_changes_no_data() {
    const TEST: &str = "the_upgrade_adds_notifications_without_a_backfill_and_changes_no_data";
    let Some(roles) = scratch::Roles::create(TEST).await else { return };
    let db = roles.empty().await;
    let pool = &db.pool;
    let mut migrator = pool.acquire().await.unwrap();
    MIGRATOR.run_to(71, &mut *migrator).await.expect("migrations up to 0071");
    reconcile_and_link(pool).await.expect("reconcile at 0071");

    let class: Uuid = scalar(pool, "SELECT id FROM ci_classes WHERE system_role = 'person'").await;
    sqlx::query(
        "INSERT INTO configuration_items (class_id, ident, label)
         SELECT $1, 'P-' || g, 'Person ' || g FROM generate_series(1, 300) g",
    )
    .bind(class)
    .execute(pool)
    .await
    .unwrap();
    let user = |name: &'static str, active: bool| async move {
        sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO users (username, display_name, password_hash, is_active)
             VALUES ($1, $1, '$argon2id$v=19$test', $2) RETURNING id",
        )
        .bind(name)
        .bind(active)
        .fetch_one(pool)
        .await
        .unwrap()
    };
    let alice = user("alice", true).await;
    let bob = user("bob", false).await;
    // Import jobs as the install has them: one finished long ago, four still
    // committing when the server stops for the upgrade.
    let job = |by: Option<Uuid>, status: &'static str, file: &'static str| async move {
        sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO import_jobs (created_by_id, created_by_name, status, file_name, file_format, file_size,
                                      file_sha256, class_key, expires_at)
             VALUES ($1, 'someone', $2, $3, 'csv', 10, repeat('a', 64), 'person', now() + interval '1 day')
             RETURNING id",
        )
        .bind(by)
        .bind(status)
        .bind(file)
        .fetch_one(pool)
        .await
        .unwrap()
    };
    job(Some(alice), "completed", "old.csv").await;
    let running = job(Some(alice), "committing", "people.csv").await;
    let inactive = job(Some(bob), "committing", "bob.csv").await;
    let orphan = job(None, "committing", "orphan.csv").await;
    let cancelled = job(Some(alice), "committing", "cancel.csv").await;

    let before: Fingerprint = sqlx::query_as(FINGERPRINT).fetch_one(pool).await.unwrap();

    // To 0072 only: 0073 adds users.locale, which the row hashes would see.
    MIGRATOR.run_to(72, &mut *migrator).await.expect("upgrade to 0072");
    drop(migrator);
    reconcile_and_link(pool).await.expect("reconcile after the upgrade");

    let after: Fingerprint = sqlx::query_as(FINGERPRINT).fetch_one(pool).await.unwrap();
    assert_eq!(after, before, "CIs, accounts and import jobs unchanged; CI table not rewritten");
    assert_eq!(scalar::<i64>(pool, "SELECT count(*) FROM notifications").await, 0, "nothing backfilled");

    // The jobs end after the upgrade, through the API role as the worker runs.
    let api = roles.api_pool(&db).await;
    for (id, status) in
        [(running, "completed"), (inactive, "failed"), (orphan, "completed_with_errors"), (cancelled, "cancelled")]
    {
        sqlx::query("UPDATE import_jobs SET status = $2, finished_at = now() WHERE id = $1")
            .bind(id)
            .bind(status)
            .execute(&api)
            .await
            .unwrap_or_else(|e| panic!("{status}: {e}"));
    }
    let rows: Vec<(Uuid, String, Uuid, String, String)> = sqlx::query_as(
        "SELECT user_id, kind, entity_id, data->>'fileName', data->>'status' FROM notifications ORDER BY created_at",
    )
    .fetch_all(&api)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![(alice, "import_finished".into(), running, "people.csv".into(), "completed".into())],
        "only the active creator of a job that ended hears of it"
    );
    // The job expiring later does not notify again.
    sqlx::query("UPDATE import_jobs SET status = 'expired' WHERE id = $1").bind(running).execute(&api).await.unwrap();
    assert_eq!(scalar::<i64>(&api, "SELECT count(*) FROM notifications").await, 1);

    // The API role marks it read and prunes it by retention.
    let marked = sqlx::query("UPDATE notifications SET read_at = now() WHERE user_id = $1")
        .bind(alice)
        .execute(&api)
        .await
        .unwrap();
    assert_eq!(marked.rows_affected(), 1);
    sqlx::query("DELETE FROM notifications WHERE created_at < now() - interval '30 days'").execute(&api).await.unwrap();
    api.close().await;

    // The notification goes with its user.
    sqlx::query("DELETE FROM users WHERE id = $1").bind(alice).execute(pool).await.unwrap();
    assert_eq!(scalar::<i64>(pool, "SELECT count(*) FROM notifications").await, 0);

    db.drop().await;
    roles.drop().await;
}
