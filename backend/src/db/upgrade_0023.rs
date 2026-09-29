//! Migration 0023 (OIDC MFA assurance) against an install with providers:
//! existing OIDC providers keep their behaviour (trust the provider), so the
//! upgrade locks nobody out; directories get nothing; sessions stay valid.

use sqlx::{Executor, PgPool};

use crate::db::{MIGRATOR, scratch};

const BEFORE: &str = "
INSERT INTO identity_providers (id, kind, name, issuer_url, client_id, scopes, username_claim, groups_claim) VALUES
  ('00000000-0000-4000-8000-0000000000a1', 'oidc', 'Entra ID', 'https://login.example.test/v2.0', 'cmdb',
   'profile email', 'preferred_username', 'groups');
INSERT INTO identity_providers (id, kind, name, ldap_url, start_tls, user_base_dn, user_filter, username_attribute,
  display_name_attribute, email_attribute, group_attribute) VALUES
  ('00000000-0000-4000-8000-0000000000a2', 'ldap', 'AD', 'ldaps://dc.example.test', false, 'DC=example,DC=test',
   '(uid={username})', 'uid', 'cn', 'mail', 'memberOf');
INSERT INTO users (id, username, display_name, identity_provider_id, external_id) VALUES
  ('00000000-0000-4000-8000-00000000000a', 'alice', 'Alice', '00000000-0000-4000-8000-0000000000a1', 'sub-alice');
INSERT INTO sessions (user_id, token_hash, csrf_token, expires_at) VALUES
  ('00000000-0000-4000-8000-00000000000a', decode(repeat('01', 32), 'hex'), 'csrf', now() + interval '1 hour');
";

async fn providers(pool: &PgPool) -> Vec<(String, Option<String>, Option<Vec<String>>)> {
    sqlx::query_as("SELECT name, mfa_assurance, required_acr FROM identity_providers ORDER BY name")
        .fetch_all(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn existing_oidc_providers_are_trusted_after_the_upgrade() {
    let Some(db) = scratch::empty("existing_oidc_providers_are_trusted_after_the_upgrade").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(22, pool).await.expect("migrations up to 0022");
    pool.execute(BEFORE).await.expect("data before the upgrade");
    MIGRATOR.run_to(23, pool).await.expect("migration 0023");

    assert_eq!(
        providers(pool).await,
        vec![
            ("AD".to_owned(), None, None),
            ("Entra ID".to_owned(), Some("trust_provider".to_owned()), Some(Vec::new())),
        ]
    );
    let sessions: Vec<bool> = sqlx::query_scalar("SELECT provider_mfa FROM sessions").fetch_all(pool).await.unwrap();
    assert_eq!(sessions, vec![false], "no backfill");

    // No default: an OIDC row must say how it treats MFA.
    let no_value = pool
        .execute(
            "INSERT INTO identity_providers (kind, name, issuer_url, client_id, scopes, username_claim, groups_claim)
             VALUES ('oidc', 'New', 'https://sso.example.test', 'cmdb', 'profile', 'preferred_username', 'groups')",
        )
        .await;
    assert!(no_value.is_err());
    // The checks: acr values only with verify, well-formed, never on a directory.
    for (assurance, acr) in [
        ("trust_provider", "{gold}"),
        ("verify", "{\"has space\"}"),
        ("verify", "{\"\"}"),
        ("verify", "{a,b,c,d,e,f,g,h,i,j,k}"),
        ("bogus", "{}"),
    ] {
        let r = sqlx::query(
            "UPDATE identity_providers SET mfa_assurance = $1, required_acr = $2::text[] WHERE name = 'Entra ID'",
        )
        .bind(assurance)
        .bind(acr)
        .execute(pool)
        .await;
        assert!(r.is_err(), "{assurance} {acr}");
    }
    sqlx::query("UPDATE identity_providers SET mfa_assurance = 'verify', required_acr = '{urn:x:gold,loa3}' WHERE name = 'Entra ID'")
        .execute(pool)
        .await
        .expect("verify with acr values");
    let ldap =
        sqlx::query("UPDATE identity_providers SET mfa_assurance = 'verify', required_acr = '{}' WHERE name = 'AD'")
            .execute(pool)
            .await;
    assert!(ldap.is_err(), "a directory has no MFA assurance");
    db.drop().await;
}
