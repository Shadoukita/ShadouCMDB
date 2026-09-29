//! Encrypted identity provider secrets against a real PostgreSQL (see
//! `db::scratch`): the upgrade of plaintext rows, key rotation, the refusal on
//! an unknown key, the check constraints of migration 0026, and
//! `identity-providers reset-undecryptable` (GH#199), which without a key
//! refuses unless `--no-key` is given (GH#239).

use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use super::sealed::{self, PrepareError, ProviderSecret, SealedTable, seal_provider_secret};
use super::{Keyring, new_key};
use crate::api::context::RequestContext;
use crate::data::identity_providers as data;
use crate::db::{MIGRATOR, scratch};
use crate::modules::sso;

const OIDC: Uuid = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0001);
const LDAP: Uuid = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0002);
const ANONYMOUS: Uuid = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0003);

const CLIENT_SECRET: &str = "oidc client sécret";
const BIND_PASSWORD: &str = "ldap-bind-password";
const BIND_DN: &str = "cn=svc-cmdb,ou=services,dc=example,dc=com";

async fn add_oidc(pool: &PgPool, id: Uuid, name: &str) {
    sqlx::query(
        "INSERT INTO identity_providers (id, kind, name, issuer_url, client_id, scopes, username_claim, groups_claim,
           mfa_assurance, required_acr)
         VALUES ($1, 'oidc', $2, 'https://idp.example.test', 'cmdb', 'profile', 'preferred_username', 'groups',
           'verify', '{}')",
    )
    .bind(id)
    .bind(name)
    .execute(pool)
    .await
    .unwrap();
}

async fn add_ldap(pool: &PgPool, id: Uuid, name: &str) {
    sqlx::query(
        "INSERT INTO identity_providers (id, kind, name, ldap_url, start_tls, user_base_dn, user_filter,
           username_attribute, display_name_attribute, email_attribute, group_attribute)
         VALUES ($1, 'ldap', $2, 'ldaps://dc1.example.test', false, 'dc=example,dc=com', '(uid={username})',
           'uid', 'cn', 'mail', 'memberOf')",
    )
    .bind(id)
    .bind(name)
    .execute(pool)
    .await
    .unwrap();
}

/// What is stored for a provider: (client_secret, bind_password,
/// client_secret_enc, bind_password_enc, secrets_key_id).
type Stored = (Option<String>, Option<String>, Option<Vec<u8>>, Option<Vec<u8>>, Option<i32>);

async fn stored(pool: &PgPool, id: Uuid) -> Stored {
    sqlx::query_as(
        "SELECT client_secret, bind_password, client_secret_enc, bind_password_enc, secrets_key_id
         FROM identity_providers WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn row(pool: &PgPool, id: Uuid) -> data::ProviderRow {
    data::get(&mut pool.acquire().await.unwrap(), id, false).await.unwrap().unwrap()
}

/// Writes an encrypted secret directly, as the admin API would.
async fn put_sealed(pool: &PgPool, ring: &Keyring, id: Uuid, column: ProviderSecret, secret: &str) {
    let s = seal_provider_secret(ring, id, column, secret);
    let sql = match column {
        ProviderSecret::ClientSecret => {
            "UPDATE identity_providers SET client_secret_enc = $1, secrets_key_id = $2 WHERE id = $3"
        }
        ProviderSecret::BindPassword => {
            "UPDATE identity_providers SET bind_dn = $4, bind_password_enc = $1, secrets_key_id = $2 WHERE id = $3"
        }
    };
    sqlx::query(sql).bind(&s.bytes).bind(s.key_id.0).bind(id).bind(BIND_DN).execute(pool).await.unwrap();
}

fn idp_rewrap(done: &[sealed::Rewrapped]) -> &sealed::Rewrapped {
    done.iter().find(|d| d.table == SealedTable::IdentityProviders).expect("identity_providers is a sealed table")
}

#[tokio::test]
async fn plaintext_provider_secrets_are_encrypted_at_start_up() {
    let Some(db) = scratch::empty("plaintext_provider_secrets_are_encrypted_at_start_up").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(25, pool).await.expect("migrations up to 0025");
    add_oidc(pool, OIDC, "Entra ID").await;
    add_ldap(pool, LDAP, "Corporate AD").await;
    add_ldap(pool, ANONYMOUS, "Anonymous directory").await;
    sqlx::query("UPDATE identity_providers SET client_secret = $1 WHERE id = $2")
        .bind(CLIENT_SECRET)
        .bind(OIDC)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("UPDATE identity_providers SET bind_dn = $1, bind_password = $2 WHERE id = $3")
        .bind(BIND_DN)
        .bind(BIND_PASSWORD)
        .bind(LDAP)
        .execute(pool)
        .await
        .unwrap();
    let mut conn = pool.acquire().await.unwrap();
    let before = sealed::unencrypted_counts(&mut conn).await.unwrap();
    assert!(before.contains(&(SealedTable::IdentityProviders, 2)), "{before:?}: counted before 0026 too");
    MIGRATOR.run(pool).await.expect("migration 0026");
    let after = sealed::unencrypted_counts(&mut conn).await.unwrap();
    assert!(after.contains(&(SealedTable::IdentityProviders, 2)), "{after:?}: the migration needs no key");
    drop(conn);

    let ring = Keyring::random();
    let done = sealed::prepare(pool, &ring).await.unwrap();
    let d = idp_rewrap(&done);
    assert_eq!((d.unencrypted, d.from_previous, d.failed), (2, 0, 0));
    let (cs, bp, cs_enc, bp_enc, key) = stored(pool, OIDC).await;
    let cs_enc = cs_enc.unwrap();
    assert_eq!((cs, bp, bp_enc, key), (None, None, None, Some(ring.active_id().0)));
    assert_eq!(cs_enc.len(), CLIENT_SECRET.len() + super::OVERHEAD);
    assert!(!cs_enc.windows(8).any(|w| CLIENT_SECRET.as_bytes().windows(8).any(|p| p == w)), "no plaintext left");
    let (cs, bp, cs_enc, bp_enc, key) = stored(pool, LDAP).await;
    assert_eq!((cs, bp, cs_enc, key), (None, None, None, Some(ring.active_id().0)));
    assert_eq!(bp_enc.unwrap().len(), BIND_PASSWORD.len() + super::OVERHEAD);
    assert_eq!(stored(pool, ANONYMOUS).await, (None, None, None, None, None), "nothing to encrypt, no key id");

    // Only the settings builders decrypt, and they give the original values.
    let settings = sso::oidc_settings(&row(pool, OIDC).await, &ring).unwrap();
    assert_eq!(settings.client_secret.as_deref(), Some(CLIENT_SECRET));
    let settings = sso::ldap_settings(&row(pool, LDAP).await, &ring).unwrap();
    assert_eq!((settings.bind_dn.as_deref(), settings.bind_password.as_deref()), (Some(BIND_DN), Some(BIND_PASSWORD)));
    let settings = sso::ldap_settings(&row(pool, ANONYMOUS).await, &ring).unwrap();
    assert_eq!((settings.bind_dn, settings.bind_password.is_none()), (None, true));

    // A second start has nothing to do; the counts are per table.
    let again = sealed::prepare(pool, &ring).await.unwrap();
    assert_eq!(idp_rewrap(&again).unencrypted + idp_rewrap(&again).from_previous, 0);
    let counts = sealed::key_counts(&mut pool.acquire().await.unwrap()).await.unwrap();
    assert_eq!(
        counts,
        vec![sealed::KeyCount { table: SealedTable::IdentityProviders, key_id: ring.active_id(), rows: 2 }]
    );
    db.drop().await;
}

#[tokio::test]
async fn provider_secrets_follow_a_rotation_and_an_unknown_key_refuses_to_start() {
    let Some(db) = scratch::database("provider_secrets_follow_a_rotation").await else { return };
    let pool = &db.pool;
    add_oidc(pool, OIDC, "Entra ID").await;
    add_ldap(pool, LDAP, "Corporate AD").await;
    let (a, b) = (new_key(), new_key());
    let old = Keyring::from_keys(&a, None);
    put_sealed(pool, &old, OIDC, ProviderSecret::ClientSecret, CLIENT_SECRET).await;
    put_sealed(pool, &old, LDAP, ProviderSecret::BindPassword, BIND_PASSWORD).await;

    let only_new = Keyring::from_keys(&b, None);
    let Err(PrepareError::Refused(msg)) = sealed::prepare(pool, &only_new).await else { panic!("not refused") };
    assert_eq!(
        msg,
        format!(
            "2 identity provider secrets are encrypted with key {}, but ENCRYPTION_KEY_FILE holds key {} (and \
             ENCRYPTION_KEY_PREVIOUS_FILE is not set). Configure the key this database was encrypted with. If that \
             key is lost, run \"shadoucmdb identity-providers reset-undecryptable\" (disables those providers and \
             clears their secrets until an administrator enters them again).",
            old.active_id(),
            only_new.active_id()
        )
    );
    assert_eq!(stored(pool, OIDC).await.4, Some(old.active_id().0), "nothing changed");
    // The settings under the lost key are not built.
    assert!(sso::oidc_settings(&row(pool, OIDC).await, &only_new).is_err());

    let rotating = Keyring::from_keys(&b, Some(&a));
    let done = sealed::prepare(pool, &rotating).await.unwrap();
    assert_eq!((idp_rewrap(&done).unencrypted, idp_rewrap(&done).from_previous), (0, 2));
    assert!(sealed::prepare(pool, &only_new).await.is_ok(), "the old key is no longer needed");
    let settings = sso::oidc_settings(&row(pool, OIDC).await, &only_new).unwrap();
    assert_eq!(settings.client_secret.as_deref(), Some(CLIENT_SECRET));
    let settings = sso::ldap_settings(&row(pool, LDAP).await, &only_new).unwrap();
    assert_eq!(settings.bind_password.as_deref(), Some(BIND_PASSWORD));
    db.drop().await;
}

/// A ciphertext moved to another provider or to the other column does not
/// open; start-up leaves such a row alone (it is under the active key).
#[tokio::test]
async fn a_provider_secret_moved_elsewhere_does_not_decrypt() {
    let Some(db) = scratch::database("a_provider_secret_moved_elsewhere").await else { return };
    let pool = &db.pool;
    let ring = Keyring::random();
    add_oidc(pool, OIDC, "Entra ID").await;
    let other = Uuid::new_v4();
    add_oidc(pool, other, "Other IdP").await;
    put_sealed(pool, &ring, OIDC, ProviderSecret::ClientSecret, CLIENT_SECRET).await;
    sqlx::query(
        "UPDATE identity_providers SET (client_secret_enc, secrets_key_id) =
           (SELECT client_secret_enc, secrets_key_id FROM identity_providers WHERE id = $1)
         WHERE id = $2",
    )
    .bind(OIDC)
    .bind(other)
    .execute(pool)
    .await
    .unwrap();
    assert!(sso::oidc_settings(&row(pool, other).await, &ring).is_err());
    assert!(sso::oidc_settings(&row(pool, OIDC).await, &ring).is_ok());
    // Sealed for bind_password of the same provider, read as its client secret.
    let s = seal_provider_secret(&ring, OIDC, ProviderSecret::BindPassword, CLIENT_SECRET);
    let moved = sealed::StoredSecret::Encrypted { key_id: s.key_id, bytes: s.bytes };
    assert!(sealed::open_provider_secret(&ring, OIDC, ProviderSecret::ClientSecret, &moved).is_err());
    assert_eq!(idp_rewrap(&sealed::prepare(pool, &ring).await.unwrap()).failed, 0, "rows under the active key");
    db.drop().await;
}

#[tokio::test]
async fn the_0026_constraints_reject_mixed_or_malformed_secrets() {
    let Some(db) = scratch::database("the_0026_constraints").await else { return };
    let pool = &db.pool;
    add_oidc(pool, OIDC, "Entra ID").await;
    add_ldap(pool, LDAP, "Corporate AD").await;
    let bytes = vec![7u8; 40];
    let cases: [(&str, Uuid, &str); 7] = [
        ("plaintext and ciphertext at once", OIDC, "client_secret = 'x', client_secret_enc = $1, secrets_key_id = 1"),
        ("a ciphertext without a key id", OIDC, "client_secret_enc = $1"),
        ("a key id without a ciphertext", OIDC, "secrets_key_id = 1"),
        ("a 28-byte value", OIDC, "client_secret_enc = substring($1 from 1 for 28), secrets_key_id = 1"),
        ("client_secret_enc on an LDAP row", LDAP, "client_secret_enc = $1, secrets_key_id = 1"),
        ("bind_password_enc on an OIDC row", OIDC, "bind_password_enc = $1, secrets_key_id = 1"),
        ("bind_dn without any password", LDAP, "bind_dn = 'cn=x'"),
    ];
    for (what, id, set) in cases {
        let err = sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE identity_providers SET {set} WHERE id = $2")))
            .bind(&bytes)
            .bind(id)
            .execute(pool)
            .await
            .expect_err(what);
        assert!(err.to_string().contains("check constraint"), "{what}: {err}");
    }
    // The accepted forms.
    for set in [
        "client_secret_enc = $1, secrets_key_id = 1",
        "client_secret = 'x', client_secret_enc = NULL, secrets_key_id = NULL",
        "client_secret = NULL",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE identity_providers SET {set} WHERE id = $2")))
            .bind(&bytes)
            .bind(OIDC)
            .execute(pool)
            .await
            .unwrap_or_else(|e| panic!("{set}: {e}"));
    }
    sqlx::query(
        "UPDATE identity_providers SET bind_dn = 'cn=x', bind_password_enc = $1, secrets_key_id = 1 WHERE id = $2",
    )
    .bind(&bytes)
    .bind(LDAP)
    .execute(pool)
    .await
    .unwrap();
    db.drop().await;
}

#[tokio::test]
async fn reset_undecryptable_disables_providers_under_lost_keys_only() {
    let Some(db) = scratch::database("reset_undecryptable_providers").await else { return };
    let pool = &db.pool;
    let (lost, current) = (Keyring::random(), Keyring::random());
    add_oidc(pool, OIDC, "Entra ID").await;
    add_ldap(pool, LDAP, "Corporate AD").await;
    let kept = Uuid::new_v4();
    add_oidc(pool, kept, "Current IdP").await;
    put_sealed(pool, &lost, OIDC, ProviderSecret::ClientSecret, CLIENT_SECRET).await;
    put_sealed(pool, &lost, LDAP, ProviderSecret::BindPassword, BIND_PASSWORD).await;
    put_sealed(pool, &current, kept, ProviderSecret::ClientSecret, CLIENT_SECRET).await;
    // A directory account with an open session: it ends with the provider.
    let user: Uuid = sqlx::query_scalar(
        "INSERT INTO users (username, display_name, identity_provider_id, external_id)
         VALUES ('dora', 'Dora', $1, 'uid=dora') RETURNING id",
    )
    .bind(LDAP)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO sessions (token_hash, user_id, csrf_token, expires_at)
         VALUES (sha256('t'::bytea), $1, 'c', now() + interval '1 hour')",
    )
    .bind(user)
    .execute(pool)
    .await
    .unwrap();

    // What --dry-run lists: the two under the lost key.
    let known = [current.active_id()];
    let mut tx = pool.begin().await.unwrap();
    let providers = sealed::undecryptable_providers(&mut tx, &known).await.unwrap();
    let names: Vec<(&str, &str, bool)> =
        providers.iter().map(|p| (p.name.as_str(), p.kind.as_str(), p.is_enabled)).collect();
    assert_eq!(names, vec![("Corporate AD", "ldap", true), ("Entra ID", "oidc", true)]);
    assert!(providers.iter().all(|p| p.key_id == lost.active_id()));
    tx.rollback().await.unwrap();
    assert_eq!(stored(pool, OIDC).await.4, Some(lost.active_id().0), "a dry run changes nothing");

    let mut tx = pool.begin().await.unwrap();
    let providers = sealed::undecryptable_providers(&mut tx, &known).await.unwrap();
    let ctx = RequestContext::system("cli: identity-providers reset-undecryptable (test)", "cli");
    crate::modules::identity_providers::reset_undecryptable(&mut tx, &ctx, None, &providers).await.unwrap();
    tx.commit().await.unwrap();

    for id in [OIDC, LDAP] {
        assert_eq!(stored(pool, id).await, (None, None, None, None, None));
        let r = row(pool, id).await;
        assert!(!r.is_enabled);
        assert_eq!(r.bind_dn, None);
    }
    let r = row(pool, kept).await;
    assert!(r.is_enabled && r.secrets.client_secret.is_some(), "the provider under the configured key is untouched");
    let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id = $1")
        .bind(user)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(sessions, 0);

    let audit: Vec<(String, String, Option<String>, Value, Value)> = sqlx::query_as(
        "SELECT action, actor_type, actor_name, old_value, new_value FROM audit_log
         WHERE entity_type = 'identity_providers' ORDER BY new_value->>'name'",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(audit.len(), 2);
    for (action, actor_type, actor_name, old, new) in &audit {
        assert_eq!((action.as_str(), actor_type.as_str()), ("update", "system"));
        assert_eq!(actor_name.as_deref(), Some("cli: identity-providers reset-undecryptable (test)"));
        assert_eq!(
            (new["reason"].as_str(), new["keyId"].as_str()),
            (Some("key_lost"), Some(&*lost.active_id().to_string()))
        );
        assert_eq!((old["isEnabled"].as_bool(), new["isEnabled"].as_bool()), (Some(true), Some(false)));
        for v in [old, new] {
            let text = v.to_string();
            assert!(!text.contains(CLIENT_SECRET) && !text.contains(BIND_PASSWORD), "{text}");
        }
    }
    let ldap = &audit[0];
    assert_eq!(ldap.3["ldap"]["bindDn"].as_str(), Some(BIND_DN), "the old bind DN stays on record");
    assert_eq!(
        (ldap.3["ldap"]["bindPasswordSet"].as_bool(), ldap.4["ldap"]["bindPasswordSet"].as_bool()),
        (Some(true), Some(false))
    );
    assert_eq!(ldap.4["ldap"]["bindDn"], Value::Null);
    let revoked: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'session.revoke'")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(revoked, 1);
    // The server starts with the current key again.
    assert!(sealed::prepare(pool, &current).await.is_ok());
    db.drop().await;
}

/// GH#239: without ENCRYPTION_KEY_FILE, `--yes` alone disables nothing; only
/// `--no-key` treats every encrypted provider secret as undecryptable.
#[tokio::test]
async fn reset_undecryptable_without_a_key_needs_no_key() {
    let Some(db) = scratch::database("reset_undecryptable_providers_no_key").await else { return };
    let pool = &db.pool;
    let ring = Keyring::random();
    add_oidc(pool, OIDC, "Entra ID").await;
    add_ldap(pool, LDAP, "Corporate AD").await;
    put_sealed(pool, &ring, OIDC, ProviderSecret::ClientSecret, CLIENT_SECRET).await;
    put_sealed(pool, &ring, LDAP, ProviderSecret::BindPassword, BIND_PASSWORD).await;
    let args = |no_key| super::cli::ResetUndecryptableArgs {
        dry_run: false,
        no_key,
        confirm: crate::maintenance::ConfirmArgs { yes: true },
    };
    let state = || async {
        let enabled: i64 = sqlx::query_scalar("SELECT count(*) FROM identity_providers WHERE is_enabled")
            .fetch_one(pool)
            .await
            .unwrap();
        let sealed: i64 =
            sqlx::query_scalar("SELECT count(*) FROM identity_providers WHERE secrets_key_id IS NOT NULL")
                .fetch_one(pool)
                .await
                .unwrap();
        let events: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE entity_type = 'identity_providers'")
            .fetch_one(pool)
            .await
            .unwrap();
        (enabled, sealed, events)
    };

    let err = super::cli::reset_idps(pool, None, None, &args(false)).await.unwrap_err().to_string();
    assert!(err.starts_with("ENCRYPTION_KEY_FILE is not set") && err.contains("--no-key"), "{err}");
    assert_eq!(state().await, (2, 2, 0), "nothing changed without --no-key");
    let configured = Some((ring.active_id(), None));
    let err = super::cli::reset_idps(pool, None, configured, &args(true)).await.unwrap_err().to_string();
    assert!(err.starts_with("--no-key was given, but ENCRYPTION_KEY_FILE is set"), "{err}");
    assert_eq!(state().await, (2, 2, 0), "nothing changed with --no-key and a key");

    super::cli::reset_idps(pool, None, None, &args(true)).await.unwrap();
    assert_eq!(state().await, (0, 0, 2), "--no-key disables every provider with a secret, audited");
    db.drop().await;
}
