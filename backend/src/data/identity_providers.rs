//! SQL for identity providers (OIDC, LDAP), their group mappings and the
//! accounts that belong to a provider.

use chrono::{DateTime, Utc};
use sqlx::PgConnection;
use uuid::Uuid;

pub const TABLE: &str = "identity_providers";

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ProviderRow {
    pub id: Uuid,
    pub kind: String,
    pub name: String,
    pub is_enabled: bool,
    pub sort_order: i32,
    pub ca_certificate: Option<String>,
    pub issuer_url: Option<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub scopes: Option<String>,
    pub username_claim: Option<String>,
    pub groups_claim: Option<String>,
    /// OIDC: `verify` or `trust_provider` (see [`crate::auth::sso::oidc::MfaPolicy`]).
    pub mfa_assurance: Option<String>,
    /// OIDC: the acr values that count as MFA under `verify` (empty: amr decides).
    pub required_acr: Option<Vec<String>>,
    pub ldap_url: Option<String>,
    pub start_tls: Option<bool>,
    pub bind_dn: Option<String>,
    pub bind_password: Option<String>,
    pub user_base_dn: Option<String>,
    pub user_filter: Option<String>,
    pub username_attribute: Option<String>,
    pub display_name_attribute: Option<String>,
    pub email_attribute: Option<String>,
    pub group_attribute: Option<String>,
    pub user_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub const COLUMNS: &str = "id, kind, name, is_enabled, sort_order, ca_certificate,
    issuer_url, client_id, client_secret, scopes, username_claim, groups_claim, mfa_assurance, required_acr,
    ldap_url, start_tls, bind_dn, bind_password, user_base_dn, user_filter,
    username_attribute, display_name_attribute, email_attribute, group_attribute,
    (SELECT count(*) FROM users u WHERE u.identity_provider_id = identity_providers.id) AS user_count,
    created_at, updated_at";

const ORDER: &str = "sort_order, lower(name), id";

pub async fn list(conn: &mut PgConnection) -> sqlx::Result<Vec<ProviderRow>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {COLUMNS} FROM identity_providers ORDER BY {ORDER}")))
        .fetch_all(conn)
        .await
}

/// Enabled providers of one kind, in sign-in order.
pub async fn enabled(conn: &mut PgConnection, kind: &str) -> sqlx::Result<Vec<ProviderRow>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM identity_providers WHERE is_enabled AND kind = $1 ORDER BY {ORDER}"
    )))
    .bind(kind)
    .fetch_all(conn)
    .await
}

pub async fn get(conn: &mut PgConnection, id: Uuid, for_update: bool) -> sqlx::Result<Option<ProviderRow>> {
    crate::data::crud::select_by_id(conn, TABLE, COLUMNS, id, for_update).await
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MappingRow {
    pub provider_id: Uuid,
    pub group_name: String,
    pub profile_id: Uuid,
    pub profile_name: String,
}

pub async fn mappings(conn: &mut PgConnection, provider_ids: &[Uuid]) -> sqlx::Result<Vec<MappingRow>> {
    sqlx::query_as(
        "SELECT m.provider_id, m.group_name, m.profile_id, p.name AS profile_name
         FROM identity_provider_group_mappings m JOIN permission_profiles p ON p.id = m.profile_id
         WHERE m.provider_id = ANY($1)
         ORDER BY lower(m.group_name), p.is_builtin DESC, lower(p.name)",
    )
    .bind(provider_ids)
    .fetch_all(conn)
    .await
}

/// Replaces the provider's mappings with exactly these (group, profile) pairs.
pub async fn set_mappings(conn: &mut PgConnection, provider_id: Uuid, pairs: &[(String, Uuid)]) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM identity_provider_group_mappings WHERE provider_id = $1")
        .bind(provider_id)
        .execute(&mut *conn)
        .await?;
    let (groups, profiles): (Vec<String>, Vec<Uuid>) = pairs.iter().cloned().unzip();
    sqlx::query(
        "INSERT INTO identity_provider_group_mappings (provider_id, group_name, profile_id)
         SELECT $1, g, p FROM unnest($2::text[], $3::uuid[]) AS u(g, p)
         ON CONFLICT DO NOTHING",
    )
    .bind(provider_id)
    .bind(groups)
    .bind(profiles)
    .execute(conn)
    .await?;
    Ok(())
}

/// The profiles these groups map to (groups compared case-insensitively).
pub async fn profiles_for_groups(
    conn: &mut PgConnection,
    provider_id: Uuid,
    groups: &[String],
) -> sqlx::Result<Vec<Uuid>> {
    let lower: Vec<String> = groups.iter().map(|g| g.to_lowercase()).collect();
    sqlx::query_scalar(
        "SELECT DISTINCT profile_id FROM identity_provider_group_mappings
         WHERE provider_id = $1 AND lower(group_name) = ANY($2) ORDER BY profile_id",
    )
    .bind(provider_id)
    .bind(lower)
    .fetch_all(conn)
    .await
}

pub async fn delete(conn: &mut PgConnection, id: Uuid) -> sqlx::Result<()> {
    crate::data::crud::delete_row(conn, TABLE, id).await
}

// ---------------------------------------------------------------------------
// Accounts that belong to a provider
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct LinkedUser {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
    pub email: Option<String>,
    pub is_active: bool,
}

pub async fn find_linked(
    conn: &mut PgConnection,
    provider_id: Uuid,
    external_id: &str,
    for_update: bool,
) -> sqlx::Result<Option<LinkedUser>> {
    let lock = if for_update { " FOR UPDATE" } else { "" };
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT id, username, display_name, email, is_active FROM users
         WHERE identity_provider_id = $1 AND external_id = $2{lock}"
    )))
    .bind(provider_id)
    .bind(external_id)
    .fetch_optional(conn)
    .await
}

pub struct NewLinkedUser<'a> {
    pub provider_id: Uuid,
    pub external_id: &'a str,
    pub username: &'a str,
    pub display_name: &'a str,
    pub email: Option<&'a str>,
}

pub async fn insert_linked(conn: &mut PgConnection, u: &NewLinkedUser<'_>) -> sqlx::Result<Uuid> {
    sqlx::query_scalar(
        "INSERT INTO users (username, display_name, email, identity_provider_id, external_id)
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(u.username)
    .bind(u.display_name)
    .bind(u.email)
    .bind(u.provider_id)
    .bind(u.external_id)
    .fetch_one(conn)
    .await
}

/// Takes over what the provider says about the user.
pub async fn refresh_linked(
    conn: &mut PgConnection,
    id: Uuid,
    username: &str,
    display_name: &str,
    email: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE users SET username = $2, display_name = $3, email = $4 WHERE id = $1")
        .bind(id)
        .bind(username)
        .bind(display_name)
        .bind(email)
        .execute(conn)
        .await?;
    Ok(())
}

/// Whether another account already has this username (case-insensitively).
pub async fn username_taken(conn: &mut PgConnection, username: &str, except: Option<Uuid>) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM users WHERE lower(username) = lower($1) AND id IS DISTINCT FROM $2)",
    )
    .bind(username)
    .bind(except)
    .fetch_one(conn)
    .await
}

pub async fn user_ids(conn: &mut PgConnection, provider_id: Uuid) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar("SELECT id FROM users WHERE identity_provider_id = $1").bind(provider_id).fetch_all(conn).await
}
