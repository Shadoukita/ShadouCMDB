//! SQL for API tokens. Never selects `token_hash` back out: it is only a lookup key.

use std::net::IpAddr;
use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use ipnetwork::IpNetwork;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::auth::permissions::{ClassRights, GlobalPermission, Permissions};
use crate::data::auth::MFA_REQUIRED;

/// Lets [`MFA_REQUIRED`] read the token's `mfa_verified` as the session's.
const TOKEN_AS_SESSION: &str = "CROSS JOIN LATERAL (SELECT t.mfa_verified) AS s(mfa_verified)";

/// [`MFA_REQUIRED`] with the token `t` standing in for the session: whether
/// the owner `u` must use two-factor authentication and the token was not
/// created from a session that proved it (GH#200). Needs [`TOKEN_AS_SESSION`]
/// in the FROM clause.
fn refused_for_mfa() -> String {
    format!("(({MFA_REQUIRED}) AND NOT t.mfa_verified)")
}

/// The token a request presented, whatever its state; the caller decides whether it is usable.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PresentedToken {
    pub id: Uuid,
    pub name: String,
    pub token_prefix: String,
    pub user_id: Uuid,
    pub username: String,
    pub user_active: bool,
    /// The owner signs in locally, or through an identity provider that is
    /// enabled: disabling the provider stops the token until it is enabled
    /// again, as disabling the owner does (GH#257).
    pub provider_enabled: bool,
    pub profile_id: Option<Uuid>,
    /// Who minted it; null for the CLI, a deleted creator or an unknown one
    pub created_by_user_id: Option<Uuid>,
    /// Whether that creator's account is enabled (false when there is none)
    pub creator_active: bool,
    pub revoked: bool,
    pub expired: bool,
    /// The owner must use two-factor authentication and the token was not
    /// created from a session that proved it.
    pub mfa_required: bool,
}

pub async fn find_by_hash(pool: &PgPool, token_hash: &[u8]) -> sqlx::Result<Option<PresentedToken>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT t.id, t.name, t.token_prefix, u.id AS user_id, u.username, u.is_active AS user_active,
                coalesce(ip.is_enabled, true) AS provider_enabled, t.profile_id,
                t.created_by_user_id, coalesce(c.is_active, false) AS creator_active,
                t.revoked_at IS NOT NULL AS revoked, t.expires_at <= now() AS expired,
                {} AS mfa_required
         FROM api_tokens t JOIN users u ON u.id = t.user_id LEFT JOIN users c ON c.id = t.created_by_user_id
              LEFT JOIN identity_providers ip ON ip.id = u.identity_provider_id
              {TOKEN_AS_SESSION}
         WHERE t.token_hash = $1",
        refused_for_mfa()
    )))
    .bind(token_hash)
    .fetch_optional(pool)
    .await
}

pub async fn record_use(conn: &mut PgConnection, id: Uuid, ip: Option<IpAddr>) -> sqlx::Result<()> {
    sqlx::query("UPDATE api_tokens SET last_used_at = now(), last_used_ip = $2 WHERE id = $1")
        .bind(id)
        .bind(ip.map(IpNetwork::from))
        .execute(conn)
        .await?;
    Ok(())
}

/// One profile's grants (the built-in profile: everything).
pub async fn profile_permissions(conn: &mut PgConnection, profile_id: Uuid) -> sqlx::Result<Permissions> {
    type Row = (String, Option<String>, Option<Uuid>, bool, bool, bool, bool);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT 'admin', NULL::text, NULL::uuid, true, true, true, true
         FROM permission_profiles WHERE id = $1 AND is_builtin
         UNION ALL
         SELECT 'global', permission, NULL::uuid, false, false, false, false
         FROM permission_profile_global_permissions WHERE profile_id = $1
         UNION ALL
         SELECT 'class', NULL::text, class_id, can_view, can_create, can_edit, can_delete
         FROM permission_profile_class_permissions WHERE profile_id = $1",
    )
    .bind(profile_id)
    .fetch_all(conn)
    .await?;
    let mut p = Permissions::default();
    for (kind, permission, class_id, view, create, edit, delete) in rows {
        match kind.as_str() {
            "admin" => p.administrator = true,
            "global" => {
                if let Some(g) = permission.as_deref().and_then(GlobalPermission::parse) {
                    p.merge_global(g);
                }
            }
            _ => p.merge_class(class_id, ClassRights { view, create, edit, delete }),
        }
    }
    Ok(p)
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TokenRow {
    pub id: Uuid,
    pub name: String,
    pub user_id: Uuid,
    pub username: String,
    pub user_active: bool,
    pub profile_id: Option<Uuid>,
    pub profile_name: Option<String>,
    pub profile_is_builtin: Option<bool>,
    pub token_prefix: String,
    pub expires_at: DateTime<Utc>,
    pub expired: bool,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_by: Option<String>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub last_used_ip: Option<IpNetwork>,
    pub created_by: Option<String>,
    pub created_by_user_id: Option<Uuid>,
    pub mfa_verified: bool,
    pub refused_for_mfa: bool,
    pub created_at: DateTime<Utc>,
}

pub static FROM: LazyLock<String> = LazyLock::new(|| {
    format!(
        "api_tokens t JOIN users u ON u.id = t.user_id LEFT JOIN permission_profiles p ON p.id = t.profile_id
         {TOKEN_AS_SESSION}"
    )
});

/// The owner `u` signs in locally, or through an identity provider that is
/// enabled (GH#257).
const OWNER_PROVIDER_ENABLED: &str = "(u.identity_provider_id IS NULL OR EXISTS (SELECT 1 FROM identity_providers ip
     WHERE ip.id = u.identity_provider_id AND ip.is_enabled))";

/// A working token (not revoked or expired, owner active, owner's identity
/// provider enabled) that is refused because of [`refused_for_mfa`]: what a
/// request with it would be answered (`mfa_required`). Also the
/// `refusedForMfa` filter and the count `shadoucmdb migrate` prints.
pub static REFUSED_WORKING: LazyLock<String> = LazyLock::new(|| {
    format!(
        "(t.revoked_at IS NULL AND t.expires_at > now() AND u.is_active AND {OWNER_PROVIDER_ENABLED} AND {})",
        refused_for_mfa()
    )
});

pub static COLUMNS: LazyLock<String> = LazyLock::new(|| {
    format!(
        "t.id, t.name, t.user_id, u.username, u.is_active AS user_active, t.profile_id,
         p.name AS profile_name, p.is_builtin AS profile_is_builtin, t.token_prefix, t.expires_at,
         t.expires_at <= now() AS expired, t.revoked_at, t.revoked_by, t.last_used_at, t.last_used_ip,
         t.created_by, t.created_by_user_id, t.mfa_verified, {} AS refused_for_mfa, t.created_at",
        *REFUSED_WORKING
    )
});

pub async fn get(conn: &mut PgConnection, id: Uuid, for_update: bool) -> sqlx::Result<Option<TokenRow>> {
    let lock = if for_update { " FOR UPDATE OF t" } else { "" };
    sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {} FROM {} WHERE t.id = $1{lock}", *COLUMNS, *FROM)))
        .bind(id)
        .fetch_optional(conn)
        .await
}

pub struct NewToken<'a> {
    pub name: &'a str,
    pub user_id: Uuid,
    pub profile_id: Uuid,
    pub token_hash: &'a [u8],
    pub token_prefix: &'a str,
    pub expires_at: DateTime<Utc>,
    pub created_by: Option<&'a str>,
    pub created_by_user_id: Option<Uuid>,
    /// The creating session proved a second factor (or no user: an operator command).
    pub mfa_verified: bool,
}

pub async fn insert(conn: &mut PgConnection, t: &NewToken<'_>) -> sqlx::Result<Uuid> {
    sqlx::query_scalar(
        "INSERT INTO api_tokens (name, user_id, profile_id, token_hash, token_prefix, expires_at, created_by,
                                 created_by_user_id, mfa_verified)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
    )
    .bind(t.name)
    .bind(t.user_id)
    .bind(t.profile_id)
    .bind(t.token_hash)
    .bind(t.token_prefix)
    .bind(t.expires_at)
    .bind(t.created_by)
    .bind(t.created_by_user_id)
    .bind(t.mfa_verified)
    .fetch_one(conn)
    .await
}

/// Whether a token for `owner_id` created from a session with this
/// `mfa_verified` would be refused (the creation guard, GH#200).
pub async fn refused_for_mfa_if_created(
    conn: &mut PgConnection,
    owner_id: Uuid,
    mfa_verified: bool,
) -> sqlx::Result<bool> {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT {} FROM users u CROSS JOIN (SELECT $2::boolean) AS t(mfa_verified) {TOKEN_AS_SESSION} WHERE u.id = $1",
        refused_for_mfa()
    )))
    .bind(owner_id)
    .bind(mfa_verified)
    .fetch_one(conn)
    .await
}

/// What `shadoucmdb migrate` and `verify` print when working tokens are
/// refused for MFA (GH#200); None when there are none.
pub async fn second_factor_refusal_notice(conn: &mut PgConnection) -> sqlx::Result<Option<String>> {
    let (refused, accounts) = count_second_factor_refusals(conn).await?;
    Ok((refused > 0).then(|| {
        format!(
            "{refused} API token{} of {accounts} account{} {} refused: their owners must use two-factor \
             authentication and the tokens were not created from a session signed in with a second factor. List \
             them in the web UI under Administration › API tokens with the \"Refused for two-factor only\" \
             filter (/admin/api-tokens?refusedForMfa=true), or with \
             GET /api/v1/admin/api-tokens?refusedForMfa=true, and create new tokens for the affected integrations.",
            if refused == 1 { "" } else { "s" },
            if accounts == 1 { "" } else { "s" },
            if refused == 1 { "is" } else { "are" },
        )
    }))
}

/// Working tokens [`REFUSED_WORKING`] refuses, and how many owners they have.
pub async fn count_second_factor_refusals(conn: &mut PgConnection) -> sqlx::Result<(i64, i64)> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT count(*), count(DISTINCT t.user_id) FROM {} WHERE {}",
        *FROM, *REFUSED_WORKING
    )))
    .fetch_one(conn)
    .await
}

pub async fn revoke(conn: &mut PgConnection, id: Uuid, by: &str) -> sqlx::Result<()> {
    sqlx::query("UPDATE api_tokens SET revoked_at = now(), revoked_by = $2 WHERE id = $1 AND revoked_at IS NULL")
        .bind(id)
        .bind(by)
        .execute(conn)
        .await?;
    Ok(())
}

/// The tokens that still authenticate (neither revoked nor expired) and are
/// the user's own or, with `created_for_others`, that the user created for
/// another owner, locked. One statement locking in id order: two resets of
/// users who minted tokens for each other take the same rows in the same
/// order, and cannot deadlock (GH#166).
pub async fn active_of_user(
    conn: &mut PgConnection,
    user_id: Uuid,
    created_for_others: bool,
) -> sqlx::Result<Vec<TokenRow>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {} FROM {}
         WHERE (t.user_id = $1 OR ($2 AND t.created_by_user_id = $1)) AND t.revoked_at IS NULL AND t.expires_at > now()
         ORDER BY t.id FOR UPDATE OF t",
        *COLUMNS, *FROM
    )))
    .bind(user_id)
    .bind(created_for_others)
    .fetch_all(conn)
    .await
}

/// The user's tokens, locked, before the user is deleted (their rows cascade away).
pub async fn of_user(conn: &mut PgConnection, user_id: Uuid) -> sqlx::Result<Vec<TokenRow>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {} FROM {} WHERE t.user_id = $1 ORDER BY t.created_at, t.id FOR UPDATE OF t",
        *COLUMNS, *FROM
    )))
    .bind(user_id)
    .fetch_all(conn)
    .await
}
