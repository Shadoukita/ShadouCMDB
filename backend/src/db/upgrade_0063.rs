//! Migration 0063 (only backup.restore entries in audit_export_restores):
//! rows listed before it that match no entry go, the export's own stay.

use sqlx::{Executor, PgPool};

use crate::db::{MIGRATOR, scratch};

async fn listed(pool: &PgPool) -> Vec<i64> {
    sqlx::query_scalar("SELECT chain_seq FROM audit_export_restores ORDER BY 1").fetch_all(pool).await.unwrap()
}

#[tokio::test]
async fn rows_that_match_no_restore_entry_are_removed() {
    let Some(db) = scratch::empty("rows_that_match_no_restore_entry_are_removed").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(62, pool).await.expect("migrations up to 0062");
    let (restore, other): (i64, i64) = sqlx::query_as(
        "WITH r AS (
           INSERT INTO audit_log (actor_type, actor_name, action, entity_type, entity_id, new_value) VALUES
             ('system', 'owner', 'backup.restore', 'audit_log', gen_random_uuid(), '{}'),
             ('user', 'alice', 'login.success', 'sessions', gen_random_uuid(), '{}')
           RETURNING chain_seq, action)
         SELECT (SELECT chain_seq FROM r WHERE action = 'backup.restore'),
                (SELECT chain_seq FROM r WHERE action = 'login.success')",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    // The export's row, one for another row, and numbers past the head (GH#696).
    sqlx::query("INSERT INTO audit_export_restores (chain_seq) VALUES ($1), ($2), ($2 + 1), ($2 + 250)")
        .bind(restore)
        .bind(other)
        .execute(pool)
        .await
        .unwrap();
    assert_eq!(listed(pool).await.len(), 4);
    MIGRATOR.run(pool).await.expect("migration 0063");
    assert_eq!(listed(pool).await, [restore]);
    let err = pool.execute("INSERT INTO audit_export_restores (chain_seq) VALUES (1000)").await.unwrap_err();
    assert_eq!(err.as_database_error().and_then(|d| d.code()).as_deref(), Some("23503"), "{err}");
    db.drop().await;
}
