//! Encrypted TOTP seeds against a real PostgreSQL (see `db::scratch`):
//! the upgrade of plaintext rows, key rotation, the refusal on an unknown key,
//! a row that does not decrypt, and `mfa reset-undecryptable` (GH#189).

use sqlx::{Executor, PgPool};
use uuid::Uuid;

use super::sealed::{self, PrepareError, SealedTable};
use super::{KeyId, Keyring, new_key};
use crate::api::context::RequestContext;
use crate::auth::events::LoginMethod;
use crate::auth::totp;
use crate::db::{MIGRATOR, scratch};
use crate::modules::mfa::{Verdict, verify_second_factor};

const ALICE: Uuid = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_000a);
const BOB: Uuid = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_000b);
const CAROL: Uuid = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_000c);

async fn add_user(pool: &PgPool, id: Uuid, name: &str) {
    sqlx::query("INSERT INTO users (id, username, display_name, password_hash) VALUES ($1, $2, $2, '$argon2id$x')")
        .bind(id)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
}

/// What is stored for the user: (secret, key_id, confirmed, last_used_step).
async fn row(pool: &PgPool, user: Uuid) -> (Vec<u8>, Option<i32>, bool, Option<i64>) {
    sqlx::query_as("SELECT secret, key_id, confirmed_at IS NOT NULL, last_used_step FROM user_totp WHERE user_id = $1")
        .bind(user)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn check(pool: &PgPool, ring: &Keyring, user: Uuid, code: &str) -> Verdict {
    let mut tx = pool.begin().await.unwrap();
    let v = verify_second_factor(&mut tx, ring, user, code).await.unwrap();
    tx.commit().await.unwrap();
    v
}

fn accepted(v: &Verdict) -> bool {
    matches!(v, Verdict::Accepted(LoginMethod::Totp))
}

/// Writes a sealed, confirmed row directly (as `enrol` + `confirm` would).
async fn put_sealed(pool: &PgPool, ring: &Keyring, user: Uuid, seed: &[u8], step: i64) {
    let s = sealed::seal_totp_secret(ring, user, seed);
    sqlx::query(
        "INSERT INTO user_totp (user_id, secret, key_id, confirmed_at, last_used_step) VALUES ($1, $2, $3, now(), $4)",
    )
    .bind(user)
    .bind(&s.bytes)
    .bind(s.key_id.0)
    .bind(step)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn plaintext_seeds_are_encrypted_at_start_up_without_re_enrolment() {
    let Some(db) = scratch::empty("plaintext_seeds_are_encrypted_at_start_up").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(23, pool).await.expect("migrations up to 0023");
    add_user(pool, ALICE, "alice").await;
    add_user(pool, BOB, "bob").await;
    let (seed_a, seed_b) = (totp::new_secret(), totp::new_secret());
    let step = totp::current_step();
    // Alice has MFA on and signed in with the previous code; Bob has a set-up in progress.
    sqlx::query("INSERT INTO user_totp (user_id, secret, confirmed_at, last_used_step) VALUES ($1, $2, now(), $3)")
        .bind(ALICE)
        .bind(&seed_a[..])
        .bind(step - 1)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO user_totp (user_id, secret) VALUES ($1, $2)")
        .bind(BOB)
        .bind(&seed_b[..])
        .execute(pool)
        .await
        .unwrap();
    MIGRATOR.run(pool).await.expect("migration 0024");
    assert_eq!(row(pool, ALICE).await.1, None, "the migration itself does not need the key");

    let ring = Keyring::random();
    let done = sealed::prepare(pool, &ring).await.unwrap();
    assert_eq!(
        (done[0].table, done[0].unencrypted, done[0].from_previous, done[0].failed),
        (SealedTable::UserTotp, 2, 0, 0)
    );
    for (user, seed) in [(ALICE, &seed_a), (BOB, &seed_b)] {
        let (stored, key_id, ..) = row(pool, user).await;
        assert_eq!((stored.len(), key_id), (48, Some(ring.active_id().0)));
        assert!(!stored.windows(20).any(|w| w == seed), "no plaintext seed left");
    }
    let (_, _, confirmed, last) = row(pool, ALICE).await;
    assert_eq!((confirmed, last), (true, Some(step - 1)), "enrolment and replay state kept");
    assert!(!row(pool, BOB).await.2, "a set-up in progress stays one");

    // The code already used is still refused (replay); the next one signs in.
    assert!(matches!(check(pool, &ring, ALICE, &totp::code_at(&seed_a, step - 1)).await, Verdict::Wrong));
    assert!(accepted(&check(pool, &ring, ALICE, &totp::code_at(&seed_a, step)).await));
    // A second start has nothing to do.
    let again = sealed::prepare(pool, &ring).await.unwrap();
    assert_eq!(again[0].unencrypted + again[0].from_previous, 0);

    // Only the two formats are accepted: 20 plaintext bytes without a key id, 48 with one.
    for (secret, key) in [(vec![0u8; 20], Some(1)), (vec![0u8; 48], None), (vec![0u8; 21], None)] {
        let err = sqlx::query("UPDATE user_totp SET secret = $1, key_id = $2 WHERE user_id = $3")
            .bind(&secret)
            .bind(key)
            .bind(BOB)
            .execute(pool)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("user_totp_secret_format"), "{err}");
    }
    db.drop().await;
}

#[tokio::test]
async fn rotation_re_encrypts_and_an_unknown_key_refuses_to_start() {
    let Some(db) = scratch::database("rotation_re_encrypts").await else { return };
    let pool = &db.pool;
    add_user(pool, ALICE, "alice").await;
    add_user(pool, BOB, "bob").await;
    let (a, b) = (new_key(), new_key());
    let old = Keyring::from_keys(&a, None);
    let seed = totp::new_secret();
    let step = totp::current_step();
    put_sealed(pool, &old, ALICE, &seed, step - 1).await;
    put_sealed(pool, &old, BOB, &seed, step - 1).await;

    // Only the new key: refused, naming both keys and the way out.
    let only_new = Keyring::from_keys(&b, None);
    let Err(PrepareError::Refused(msg)) = sealed::prepare(pool, &only_new).await else { panic!("not refused") };
    assert_eq!(
        msg,
        format!(
            "2 authenticator secrets are encrypted with key {}, but ENCRYPTION_KEY_FILE holds key {} (and \
             ENCRYPTION_KEY_PREVIOUS_FILE is not set). Configure the key this database was encrypted with. If that \
             key is lost, run \"shadoucmdb mfa reset-undecryptable\" (turns off two-factor sign-in for those users \
             so they can enrol again).",
            old.active_id(),
            only_new.active_id()
        )
    );
    assert_eq!(row(pool, ALICE).await.1, Some(old.active_id().0), "nothing changed");

    // New key active, old one previous: every row moves to the new key.
    let rotating = Keyring::from_keys(&b, Some(&a));
    let done = sealed::prepare(pool, &rotating).await.unwrap();
    assert_eq!((done[0].unencrypted, done[0].from_previous), (0, 2));
    let counts = sealed::key_counts(&mut pool.acquire().await.unwrap()).await.unwrap();
    assert_eq!(counts, vec![sealed::KeyCount { table: SealedTable::UserTotp, key_id: only_new.active_id(), rows: 2 }]);
    // After the rotation the old key is no longer needed.
    assert!(sealed::prepare(pool, &only_new).await.is_ok());
    assert!(accepted(&check(pool, &only_new, ALICE, &totp::code_at(&seed, step)).await));
    assert!(matches!(check(pool, &only_new, BOB, &totp::code_at(&seed, step - 1)).await, Verdict::Wrong), "replay");
    db.drop().await;
}

#[tokio::test]
async fn a_secret_moved_to_another_user_fails_closed() {
    let Some(db) = scratch::database("a_secret_moved_to_another_user").await else { return };
    let pool = &db.pool;
    add_user(pool, ALICE, "alice").await;
    add_user(pool, BOB, "bob").await;
    let ring = Keyring::random();
    let (seed_a, seed_b) = (totp::new_secret(), totp::new_secret());
    let step = totp::current_step();
    put_sealed(pool, &ring, ALICE, &seed_a, step - 5).await;
    put_sealed(pool, &ring, BOB, &seed_b, step - 5).await;
    // Someone with write access copies Alice's ciphertext onto Bob's row to use her codes for him.
    pool.execute(
        "UPDATE user_totp SET secret = (SELECT secret FROM user_totp WHERE user_id = '00000000-0000-4000-8000-00000000000a')
         WHERE user_id = '00000000-0000-4000-8000-00000000000b'",
    )
    .await
    .unwrap();
    let v = check(pool, &ring, BOB, &totp::code_at(&seed_a, step)).await;
    assert!(matches!(v, Verdict::Undecryptable));
    assert_eq!(v.failure_reason(), Some("secret_undecryptable"));
    assert!(matches!(check(pool, &ring, BOB, &totp::code_at(&seed_b, step)).await, Verdict::Undecryptable));
    // Alice is unaffected; start-up leaves Bob's row alone rather than guessing.
    assert!(accepted(&check(pool, &ring, ALICE, &totp::code_at(&seed_a, step)).await));
    assert_eq!(sealed::prepare(pool, &ring).await.unwrap()[0].failed, 0, "rows under the active key are not re-read");
    db.drop().await;
}

#[tokio::test]
async fn reset_undecryptable_turns_off_mfa_under_lost_keys_only() {
    let Some(db) = scratch::database("reset_undecryptable").await else { return };
    let pool = &db.pool;
    add_user(pool, ALICE, "alice").await;
    add_user(pool, BOB, "bob").await;
    add_user(pool, CAROL, "carol").await;
    let (lost, current) = (Keyring::random(), Keyring::random());
    let step = totp::current_step();
    put_sealed(pool, &lost, ALICE, &totp::new_secret(), step).await;
    put_sealed(pool, &lost, BOB, &totp::new_secret(), step).await;
    put_sealed(pool, &current, CAROL, &totp::new_secret(), step).await;
    for user in [ALICE, CAROL] {
        sqlx::query(
            "INSERT INTO user_recovery_codes (user_id, code_hash) VALUES ($1, sha256(gen_random_uuid()::text::bytea))",
        )
        .bind(user)
        .execute(pool)
        .await
        .unwrap();
    }

    let known = [current.active_id()];
    let mut tx = pool.begin().await.unwrap();
    let users = sealed::undecryptable_totp(&mut tx, &known).await.unwrap();
    let names: Vec<(&str, KeyId)> = users.iter().map(|u| (u.username.as_str(), u.key_id)).collect();
    assert_eq!(names, vec![("alice", lost.active_id()), ("bob", lost.active_id())]);
    let ctx = RequestContext::system("cli: mfa reset-undecryptable (test)", "cli");
    super::cli::reset_users(&mut tx, &ctx, &users).await.unwrap();
    tx.commit().await.unwrap();

    let left: Vec<Uuid> = sqlx::query_scalar("SELECT user_id FROM user_totp").fetch_all(pool).await.unwrap();
    assert_eq!(left, vec![CAROL]);
    let codes: Vec<Uuid> = sqlx::query_scalar("SELECT user_id FROM user_recovery_codes").fetch_all(pool).await.unwrap();
    assert_eq!(codes, vec![CAROL]);
    let events: Vec<(String, String, Option<String>, String, String)> = sqlx::query_as(
        "SELECT action, actor_type, actor_name, new_value->>'reason', new_value->>'keyId' FROM audit_log
         WHERE action = 'mfa.disable' ORDER BY new_value->>'username'",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let expected = |_: &str| {
        (
            "mfa.disable".to_owned(),
            "system".to_owned(),
            Some("cli: mfa reset-undecryptable (test)".to_owned()),
            "key_lost".to_owned(),
            lost.active_id().to_string(),
        )
    };
    assert_eq!(events, vec![expected("alice"), expected("bob")]);
    // The server starts with the current key again.
    assert!(sealed::prepare(pool, &current).await.is_ok());
    db.drop().await;
}
