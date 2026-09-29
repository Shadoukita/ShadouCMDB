//! Migration 0022 (API token creator) against an install with tokens: the
//! creating user comes from the token's `create` audit row, and only from there.

use sqlx::{Executor, PgPool};
use uuid::Uuid;

use crate::db::{MIGRATOR, scratch};

/// Two users; four tokens owned by bob: minted by alice, by the CLI, by a user
/// deleted since, and one whose create row was purged. Bob's own name is the
/// display creator of the last one, which must not be taken for evidence.
const BEFORE: &str = "
INSERT INTO users (id, username, display_name, password_hash) VALUES
  ('00000000-0000-4000-8000-00000000000a', 'alice', 'Alice', '$argon2id$v=19$test'),
  ('00000000-0000-4000-8000-00000000000b', 'bob', 'Bob', '$argon2id$v=19$test');
INSERT INTO api_tokens (id, name, user_id, token_hash, token_prefix, expires_at, created_by) VALUES
  ('00000000-0000-4000-8000-0000000000f1', 'minted by alice', '00000000-0000-4000-8000-00000000000b',
   decode(repeat('01', 32), 'hex'), 'scmdb_1', now() + interval '30 days', 'alice'),
  ('00000000-0000-4000-8000-0000000000f2', 'from the cli', '00000000-0000-4000-8000-00000000000b',
   decode(repeat('02', 32), 'hex'), 'scmdb_2', now() + interval '30 days', 'create-token'),
  ('00000000-0000-4000-8000-0000000000f3', 'creator deleted', '00000000-0000-4000-8000-00000000000b',
   decode(repeat('03', 32), 'hex'), 'scmdb_3', now() + interval '30 days', 'carol'),
  ('00000000-0000-4000-8000-0000000000f4', 'row purged', '00000000-0000-4000-8000-00000000000b',
   decode(repeat('04', 32), 'hex'), 'scmdb_4', now() + interval '30 days', 'bob');
INSERT INTO audit_log (actor_type, actor_id, actor_name, action, entity_type, entity_id, old_value, new_value)
VALUES
  ('user', '00000000-0000-4000-8000-00000000000a', 'alice', 'create', 'api_tokens',
   '00000000-0000-4000-8000-0000000000f1', NULL, '{}'),
  ('system', NULL, 'create-token', 'create', 'api_tokens', '00000000-0000-4000-8000-0000000000f2', NULL, '{}'),
  ('user', '00000000-0000-4000-8000-00000000000c', 'carol', 'create', 'api_tokens',
   '00000000-0000-4000-8000-0000000000f3', NULL, '{}'),
  -- alice later revoked bob's purged-create token: an update, not a creation.
  ('user', '00000000-0000-4000-8000-00000000000a', 'alice', 'update', 'api_tokens',
   '00000000-0000-4000-8000-0000000000f4', '{}', '{}');
";

async fn creators(pool: &PgPool) -> Vec<(String, Option<Uuid>)> {
    sqlx::query_as("SELECT name, created_by_user_id FROM api_tokens ORDER BY name").fetch_all(pool).await.unwrap()
}

#[tokio::test]
async fn token_creators_are_taken_from_the_audit_log() {
    let Some(db) = scratch::empty("token_creators_are_taken_from_the_audit_log").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(21, pool).await.expect("migrations up to 0021");
    pool.execute(BEFORE).await.expect("data before the upgrade");
    MIGRATOR.run(pool).await.expect("migration 0022");

    let alice: Uuid = "00000000-0000-4000-8000-00000000000a".parse().unwrap();
    assert_eq!(
        creators(pool).await,
        vec![
            ("creator deleted".to_owned(), None),
            ("from the cli".to_owned(), None),
            ("minted by alice".to_owned(), Some(alice)),
            ("row purged".to_owned(), None),
        ]
    );

    // Deleting the creator keeps the token (it belongs to bob) and forgets the creator.
    sqlx::query("DELETE FROM users WHERE id = $1").bind(alice).execute(pool).await.unwrap();
    let left: Vec<(String, Option<Uuid>)> = creators(pool).await.into_iter().filter(|(_, c)| c.is_some()).collect();
    assert_eq!((creators(pool).await.len(), left), (4, vec![]));
    db.drop().await;
}
