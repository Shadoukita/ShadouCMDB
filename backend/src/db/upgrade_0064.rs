//! Migration 0064 (the API role marks a sent backup.restore entry only through
//! audit_export_mark_restore_sent()): what the export listed before stays,
//! and the API role can no longer insert rows itself.

use sqlx::PgPool;

use crate::db::{MIGRATOR, scratch};

async fn listed(pool: &PgPool) -> Vec<i64> {
    sqlx::query_scalar("SELECT chain_seq FROM cmdb.audit_export_restores ORDER BY 1").fetch_all(pool).await.unwrap()
}

fn sql_state(err: &sqlx::Error) -> String {
    err.as_database_error().and_then(|d| d.code()).unwrap_or_default().into_owned()
}

#[tokio::test]
async fn listed_entries_stay_and_the_api_role_marks_through_the_function() {
    let Some(roles) = scratch::Roles::create("listed_entries_stay_and_the_api_role_marks_through_the_function").await
    else {
        return;
    };
    let db = roles.empty().await;
    let pool = &db.pool;
    MIGRATOR.run_to(63, pool).await.expect("migrations up to 0063");
    let restores: Vec<i64> = sqlx::query_scalar(
        "INSERT INTO cmdb.audit_log (actor_type, actor_name, action, entity_type, entity_id, new_value)
         SELECT 'system', 'owner', 'backup.restore', 'audit_log', gen_random_uuid(), '{}' FROM generate_series(1, 2)
         RETURNING chain_seq",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let api = roles.api_pool(&db).await;
    // The export on 0063 listed the first entry itself.
    sqlx::query("INSERT INTO cmdb.audit_export_restores (chain_seq) VALUES ($1)")
        .bind(restores[0])
        .execute(&api)
        .await
        .unwrap();
    api.close().await;

    MIGRATOR.run(pool).await.expect("migration 0064");
    assert_eq!(listed(pool).await, [restores[0]]);
    let api = roles.api_pool(&db).await;
    let err = sqlx::query("INSERT INTO cmdb.audit_export_restores (chain_seq) VALUES ($1)")
        .bind(restores[1])
        .execute(&api)
        .await
        .unwrap_err();
    assert_eq!(sql_state(&err), "42501", "{err}");
    let marked: bool = sqlx::query_scalar("SELECT cmdb.audit_export_mark_restore_sent($1)")
        .bind(restores[1])
        .fetch_one(&api)
        .await
        .unwrap();
    assert!(marked);
    assert_eq!(listed(&api).await, restores);
    api.close().await;
    db.drop().await;
    roles.drop().await;
}
