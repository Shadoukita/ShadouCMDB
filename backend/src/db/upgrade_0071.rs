//! Migration 0071 (redesign metadata, SHAA-2357) on a populated three-role
//! install at 0070: the CIs and the audit log are untouched and the CI table
//! is not rewritten; every CI gets its newest create, update, delete or
//! restore entry as its last change (read events do not count, a CI whose
//! entries were pruned gets none); only the template's own located_in and
//! connected_to get a category, without stamping updated_at; the API role
//! reads the last changes, which follow its own audit entries, but never
//! writes them.

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

const FINGERPRINT: &str =
    "SELECT (SELECT count(*) FROM configuration_items),
            (SELECT md5(string_agg(id::text || ident || label || version::text || coalesce(deleted_at::text, ''), ',' ORDER BY id))
               FROM configuration_items),
            (SELECT pg_relation_filenode('cmdb.configuration_items')::bigint),
            (SELECT md5(string_agg(t::text, '|' ORDER BY t.id)) FROM audit_log t),
            (SELECT md5(string_agg((to_jsonb(t) - 'category')::text, '|' ORDER BY t.id)) FROM relationship_types t)";

type Fingerprint = (i64, String, i64, String, String);

#[tokio::test]
async fn the_upgrade_backfills_last_changes_and_categories_and_changes_no_data() {
    const TEST: &str = "the_upgrade_backfills_last_changes_and_categories_and_changes_no_data";
    let Some(roles) = scratch::Roles::create(TEST).await else { return };
    let db = roles.empty().await;
    let pool = &db.pool;
    let mut migrator = pool.acquire().await.unwrap();
    MIGRATOR.run_to(70, &mut *migrator).await.expect("migrations up to 0070");
    reconcile_and_link(pool).await.expect("reconcile at 0070");

    // 300 CIs with a create and an update each, as a running install has them.
    let class: Uuid = scalar(pool, "SELECT id FROM ci_classes WHERE system_role = 'person'").await;
    sqlx::query(
        "INSERT INTO configuration_items (class_id, ident, label)
         SELECT $1, 'P-' || g, 'Person ' || g FROM generate_series(1, 300) g",
    )
    .bind(class)
    .execute(pool)
    .await
    .unwrap();
    for (action, actor) in [("create", "creator"), ("update", "editor")] {
        sqlx::query(
            "INSERT INTO audit_log (actor_type, actor_id, actor_name, action, entity_type, entity_id, old_value, new_value)
             SELECT 'user', $1, $1, $2, 'configuration_items', id, CASE WHEN $2 = 'create' THEN NULL ELSE '{}'::jsonb END, '{}'
             FROM configuration_items
             WHERE ident LIKE 'P-%' AND ident <> 'P-300' ORDER BY ident",
        )
        .bind(actor)
        .bind(action)
        .execute(pool)
        .await
        .unwrap();
    }
    // P-1 was then deleted by an import; P-2 exported (a read, not a change)
    // and its workflow moved on; P-300's entries were pruned by retention; an
    // entry names a CI that no longer exists.
    let insert = |actor_type: &'static str, actor: &'static str, action: &'static str, ident: &'static str| {
        // An export is an event (no old value); a delete keeps the CI as it was.
        let sql = "INSERT INTO audit_log (actor_type, actor_name, action, entity_type, entity_id, old_value, new_value)
                   SELECT $1, $2, $3, 'configuration_items', id, CASE WHEN $3 = 'export' THEN NULL ELSE '{}'::jsonb END, '{}'
                   FROM configuration_items WHERE ident = $4";
        sqlx::query(sql).bind(actor_type).bind(actor).bind(action).bind(ident).execute(pool)
    };
    sqlx::query("UPDATE configuration_items SET deleted_at = now() WHERE ident = 'P-1'").execute(pool).await.unwrap();
    insert("import", "nightly import", "delete", "P-1").await.unwrap();
    insert("system", "exporter", "export", "P-2").await.unwrap();
    sqlx::query(
        "INSERT INTO audit_log (actor_type, actor_name, action, entity_type, entity_id, old_value, new_value)
         VALUES ('system', 'ghost', 'update', 'configuration_items', gen_random_uuid(), '{}', '{}')",
    )
    .execute(pool)
    .await
    .unwrap();

    // The template's located_in (labels as shipped), a connected_to an
    // administrator relabelled, and a type of the install's own.
    sqlx::query(
        "INSERT INTO relationship_types (key, name, forward_label, reverse_label, is_directional, updated_at)
         VALUES ('located_in', 'Located in', 'is located in', 'contains', true, '2026-01-01T00:00:00Z'),
                ('connected_to', 'Connected to', 'is patched to', 'is patched to', false, '2026-01-01T00:00:00Z'),
                ('backs_up', 'Backs up', 'backs up', 'is backed up by', true, '2026-01-01T00:00:00Z')",
    )
    .execute(pool)
    .await
    .unwrap();

    let before: Fingerprint = sqlx::query_as(FINGERPRINT).fetch_one(pool).await.unwrap();

    MIGRATOR.run(&mut *migrator).await.expect("upgrade to the latest version");
    drop(migrator);
    reconcile_and_link(pool).await.expect("reconcile after the upgrade");

    let after: Fingerprint = sqlx::query_as(FINGERPRINT).fetch_one(pool).await.unwrap();
    assert_eq!(after, before, "CIs, audit log and relationship types unchanged; CI table not rewritten");
    assert_eq!(scalar::<i64>(pool, "SELECT count(*) FROM ci_classes WHERE subtitle_attribute_id IS NOT NULL").await, 0);

    // One row per CI that still has a change in the log: the newest one.
    let last = |ident: &'static str| async move {
        sqlx::query_as::<_, (String, String, Option<String>)>(
            "SELECT l.action, l.actor_type, l.actor_name FROM ci_last_changes l
             JOIN configuration_items ci ON ci.id = l.ci_id WHERE ci.ident = $1",
        )
        .bind(ident)
        .fetch_optional(pool)
        .await
        .unwrap()
    };
    assert_eq!(scalar::<i64>(pool, "SELECT count(*) FROM ci_last_changes").await, 299);
    assert_eq!(last("P-1").await, Some(("delete".into(), "import".into(), Some("nightly import".into()))));
    assert_eq!(last("P-2").await, Some(("update".into(), "user".into(), Some("editor".into()))), "export is a read");
    assert_eq!(last("P-150").await, Some(("update".into(), "user".into(), Some("editor".into()))));
    assert_eq!(last("P-300").await, None, "pruned: no row until its next change");
    let stale: i64 = scalar(
        pool,
        "SELECT count(*) FROM ci_last_changes l
         WHERE l.audit_id <> (SELECT max(a.id) FROM audit_log a
                              WHERE a.entity_id = l.ci_id AND a.action IN ('create', 'update', 'delete', 'restore'))",
    )
    .await;
    assert_eq!(stale, 0, "every row copies the CI's newest change entry");

    let categories: Vec<(String, Option<String>, String)> = sqlx::query_as(
        "SELECT key, category, updated_at::text FROM relationship_types
         WHERE key IN ('located_in', 'connected_to', 'backs_up') ORDER BY key",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let stamp = "2026-01-01 00:00:00+00".to_owned();
    assert_eq!(
        categories,
        vec![
            ("backs_up".into(), None, stamp.clone()),
            ("connected_to".into(), None, stamp.clone()),
            ("located_in".into(), Some("Location".into()), stamp),
        ],
        "only the template's own labels get a category; updated_at is not stamped"
    );

    // The API role reads the table, and its audit entries move it on through
    // the trigger, but it cannot write a row itself.
    let api = roles.api_pool(&db).await;
    let ci: Uuid = scalar(&api, "SELECT id FROM configuration_items WHERE ident = 'P-300'").await;
    sqlx::query(
        "INSERT INTO audit_log (actor_type, actor_id, actor_name, action, entity_type, entity_id, old_value, new_value)
         VALUES ('user', 'u-1', 'alice', 'update', 'configuration_items', $1, '{}', '{}'),
                ('api_client', 'u-2', 'script', 'update', 'configuration_items', $1, '{}', '{}')",
    )
    .bind(ci)
    .execute(&api)
    .await
    .unwrap();
    let row: (String, Option<String>) =
        sqlx::query_as("SELECT actor_type, actor_name FROM ci_last_changes WHERE ci_id = $1")
            .bind(ci)
            .fetch_one(&api)
            .await
            .unwrap();
    assert_eq!(row, ("api_client".into(), Some("script".into())), "the newest entry of the statement wins");
    let err = sqlx::query("UPDATE ci_last_changes SET actor_name = 'forged' WHERE ci_id = $1")
        .bind(ci)
        .execute(&api)
        .await
        .unwrap_err();
    assert_eq!(sql_state(&err), "42501", "{err}");
    let err = sqlx::query("DELETE FROM ci_last_changes").execute(&api).await.unwrap_err();
    assert_eq!(sql_state(&err), "42501", "{err}");
    let err = sqlx::query("SELECT cmdb.audit_log_track_ci_changes()").execute(&api).await.unwrap_err();
    assert_eq!(sql_state(&err), "42501", "the trigger function is not callable: {err}");
    api.close().await;

    // The row goes with its CI.
    sqlx::query("DELETE FROM configuration_items WHERE id = $1").bind(ci).execute(pool).await.unwrap();
    assert_eq!(scalar::<i64>(pool, "SELECT count(*) FROM ci_last_changes").await, 299);

    db.drop().await;
    roles.drop().await;
}
