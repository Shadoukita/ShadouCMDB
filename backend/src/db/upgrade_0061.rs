//! Migration 0061 (sent backup.restore entries) against an install with
//! restores: an entry counts as sent only when a user or API client wrote a row
//! after it, so one a CLI command buried (GH#677) is still sent.

use sqlx::{Executor, PgPool};

use crate::db::{MIGRATOR, scratch};

/// Restore 1, then a sign-in (a server ran); restore 2, then `mfa
/// reset-undecryptable` (system rows only); restore 3 at the end.
const BEFORE: &str = "
INSERT INTO audit_log (actor_type, actor_name, action, entity_type, entity_id, new_value) VALUES
  ('system', 'owner', 'backup.restore', 'audit_log', gen_random_uuid(), '{}'),
  ('user', 'alice', 'login.success', 'sessions', gen_random_uuid(), '{}'),
  ('system', 'owner', 'backup.restore', 'audit_log', gen_random_uuid(), '{}'),
  ('system', 'cli: mfa reset-undecryptable', 'mfa.disable', 'users', gen_random_uuid(), '{}'),
  ('system', 'owner', 'backup.restore', 'audit_log', gen_random_uuid(), '{}');
";

async fn restores(pool: &PgPool, sql: &'static str) -> Vec<i64> {
    sqlx::query_scalar(sql).fetch_all(pool).await.unwrap()
}

#[tokio::test]
async fn only_restores_a_user_row_follows_count_as_sent() {
    let Some(db) = scratch::empty("only_restores_a_user_row_follows_count_as_sent").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(60, pool).await.expect("migrations up to 0060");
    pool.execute(BEFORE).await.expect("data before the upgrade");
    MIGRATOR.run(pool).await.expect("migration 0061");

    let all = restores(pool, "SELECT chain_seq FROM audit_log WHERE action = 'backup.restore' ORDER BY 1").await;
    assert_eq!(all.len(), 3);
    let sent = restores(pool, "SELECT chain_seq FROM audit_export_restores ORDER BY 1").await;
    assert_eq!(sent, all[..1]);
    db.drop().await;
}
