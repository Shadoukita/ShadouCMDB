//! SQL for API tokens. Never selects `token_hash` back out: it is only a lookup key.

use std::net::IpAddr;

use chrono::{DateTime, Utc};
use ipnetwork::IpNetwork;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::auth::permissions::{ClassRights, GlobalPermission, Permissions};

/// The token a request presented, whatever its state; the caller decides whether it is usable.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PresentedToken {
    pub id: Uuid,
    pub name: String,
    pub token_prefix: String,
    pub user_id: Uuid,
    pub username: String,
    pub user_active: bool,
    pub profile_id: Option<Uuid>,
    pub revoked: bool,
    pub expired: bool,
}

pub async fn find_by_hash(pool: &PgPool, token_hash: &[u8]) -> sqlx::Result<Option<PresentedToken>> {
    sqlx::query_as(
        "SELECT t.id, t.name, t.token_prefix, u.id AS user_id, u.username, u.is_active AS user_active, t.profile_id,
                t.revoked_at IS NOT NULL AS revoked, t.expires_at <= now() AS expired
         FROM api_tokens t JOIN users u ON u.id = t.user_id
         WHERE t.token_hash = $1",
    )
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
    pub created_at: DateTime<Utc>,
}

pub const FROM: &str =
    "api_tokens t JOIN users u ON u.id = t.user_id LEFT JOIN permission_profiles p ON p.id = t.profile_id";

pub const COLUMNS: &str = "t.id, t.name, t.user_id, u.username, u.is_active AS user_active, t.profile_id,
    p.name AS profile_name, p.is_builtin AS profile_is_builtin, t.token_prefix, t.expires_at,
    t.expires_at <= now() AS expired, t.revoked_at, t.revoked_by, t.last_used_at, t.last_used_ip,
    t.created_by, t.created_at";

pub async fn get(conn: &mut PgConnection, id: Uuid, for_update: bool) -> sqlx::Result<Option<TokenRow>> {
    let lock = if for_update { " FOR UPDATE OF t" } else { "" };
    sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {COLUMNS} FROM {FROM} WHERE t.id = $1{lock}")))
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
}

pub async fn insert(conn: &mut PgConnection, t: &NewToken<'_>) -> sqlx::Result<Uuid> {
    sqlx::query_scalar(
        "INSERT INTO api_tokens (name, user_id, profile_id, token_hash, token_prefix, expires_at, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(t.name)
    .bind(t.user_id)
    .bind(t.profile_id)
    .bind(t.token_hash)
    .bind(t.token_prefix)
    .bind(t.expires_at)
    .bind(t.created_by)
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

/// The user's tokens, locked, before the user is deleted (their rows cascade away).
pub async fn of_user(conn: &mut PgConnection, user_id: Uuid) -> sqlx::Result<Vec<TokenRow>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM {FROM} WHERE t.user_id = $1 ORDER BY t.created_at, t.id FOR UPDATE OF t"
    )))
    .bind(user_id)
    .fetch_all(conn)
    .await
}
