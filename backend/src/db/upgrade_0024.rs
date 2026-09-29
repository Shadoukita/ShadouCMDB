//! Migration 0024 (API tokens follow requireMfa, GH#200) against an install
//! with tokens and sessions: the backfill trusts a token whose creator had a
//! confirmed authenticator when it was created, and a session opened after
//! its user confirmed one; nothing else. OIDC sessions keep `provider_mfa`
//! under its new name.

use sqlx::{Executor, PgPool};

use crate::db::{MIGRATOR, scratch};

const BEFORE: &str = "
INSERT INTO users (id, username, display_name, password_hash) VALUES
  ('00000000-0000-4000-8000-00000000000a', 'alice', 'Alice', '$argon2id$v=19$upgrade-test'),
  ('00000000-0000-4000-8000-00000000000b', 'bob', 'Bob', '$argon2id$v=19$upgrade-test'),
  ('00000000-0000-4000-8000-00000000000c', 'svc', 'Service', '$argon2id$v=19$upgrade-test');
-- Alice confirmed an authenticator a week ago; Bob has one he never confirmed.
INSERT INTO user_totp (user_id, secret, confirmed_at) VALUES
  ('00000000-0000-4000-8000-00000000000a', decode(repeat('00', 20), 'hex'), now() - interval '7 days'),
  ('00000000-0000-4000-8000-00000000000b', decode(repeat('00', 20), 'hex'), NULL);
INSERT INTO api_tokens (name, user_id, token_hash, token_prefix, expires_at, created_at, created_by_user_id) VALUES
  ('alice before', '00000000-0000-4000-8000-00000000000a', decode(repeat('01', 32), 'hex'), 'scmdb_1',
   now() + interval '30 days', now() - interval '8 days', '00000000-0000-4000-8000-00000000000a'),
  ('alice after', '00000000-0000-4000-8000-00000000000a', decode(repeat('02', 32), 'hex'), 'scmdb_2',
   now() + interval '30 days', now() - interval '1 day', '00000000-0000-4000-8000-00000000000a'),
  ('svc by alice', '00000000-0000-4000-8000-00000000000c', decode(repeat('03', 32), 'hex'), 'scmdb_3',
   now() + interval '30 days', now() - interval '1 day', '00000000-0000-4000-8000-00000000000a'),
  ('svc by bob', '00000000-0000-4000-8000-00000000000c', decode(repeat('04', 32), 'hex'), 'scmdb_4',
   now() + interval '30 days', now() - interval '1 day', '00000000-0000-4000-8000-00000000000b'),
  ('alice unknown creator', '00000000-0000-4000-8000-00000000000a', decode(repeat('05', 32), 'hex'), 'scmdb_5',
   now() + interval '30 days', now() - interval '1 day', NULL);
INSERT INTO sessions (user_id, token_hash, csrf_token, expires_at, created_at, provider_mfa) VALUES
  ('00000000-0000-4000-8000-00000000000a', decode(repeat('11', 32), 'hex'), 'c1', now() + interval '1 hour',
   now() - interval '8 days', false),
  ('00000000-0000-4000-8000-00000000000a', decode(repeat('12', 32), 'hex'), 'c2', now() + interval '1 hour',
   now() - interval '1 hour', false),
  ('00000000-0000-4000-8000-00000000000b', decode(repeat('13', 32), 'hex'), 'c3', now() + interval '1 hour',
   now() - interval '1 hour', true),
  ('00000000-0000-4000-8000-00000000000b', decode(repeat('14', 32), 'hex'), 'c4', now() + interval '1 hour',
   now() - interval '1 hour', false);
";

async fn tokens(pool: &PgPool) -> Vec<(String, bool)> {
    sqlx::query_as("SELECT name, mfa_verified FROM api_tokens ORDER BY name").fetch_all(pool).await.unwrap()
}

async fn sessions(pool: &PgPool) -> Vec<(String, bool)> {
    sqlx::query_as("SELECT csrf_token, mfa_verified FROM sessions ORDER BY csrf_token").fetch_all(pool).await.unwrap()
}

#[tokio::test]
async fn the_backfill_trusts_only_tokens_created_after_a_confirmed_authenticator() {
    let Some(db) = scratch::empty("the_backfill_trusts_only_tokens_created_after_a_confirmed_authenticator").await
    else {
        return;
    };
    let pool = &db.pool;
    MIGRATOR.run_to(23, pool).await.expect("migrations up to 0023");
    pool.execute(BEFORE).await.expect("data before the upgrade");
    MIGRATOR.run_to(24, pool).await.expect("migration 0024");

    let owned = |rows: &[(&str, bool)]| rows.iter().map(|(n, v)| ((*n).to_owned(), *v)).collect::<Vec<_>>();
    assert_eq!(
        tokens(pool).await,
        owned(&[
            ("alice after", true),
            ("alice before", false),
            ("alice unknown creator", true),
            ("svc by alice", true),
            ("svc by bob", false),
        ])
    );
    // c1 predates Alice's enrolment, c2 follows it; c3 is an OIDC session
    // that proved MFA (the rename keeps it), c4 a password-only one.
    assert_eq!(sessions(pool).await, owned(&[("c1", false), ("c2", true), ("c3", true), ("c4", false)]));

    let default: bool = sqlx::query_scalar(
        "INSERT INTO api_tokens (name, user_id, token_hash, token_prefix, expires_at)
         VALUES ('new', '00000000-0000-4000-8000-00000000000a', decode(repeat('06', 32), 'hex'), 'scmdb_6',
                 now() + interval '1 day') RETURNING mfa_verified",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert!(!default, "a new token is not trusted unless its creation says so");
    db.drop().await;
}
