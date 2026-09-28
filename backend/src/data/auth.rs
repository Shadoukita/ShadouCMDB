//! SQL for users, sessions and permission profiles.

use std::net::IpAddr;
use std::time::Duration;

use chrono::{DateTime, Utc};
use ipnetwork::IpNetwork;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::auth::permissions::{ClassRights, GlobalPermission, Permissions};

fn interval(d: Duration) -> String {
    format!("{} seconds", d.as_secs())
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

pub struct LiveSession {
    pub session_id: Uuid,
    pub user_id: Uuid,
    pub username: String,
    pub csrf_token: String,
    /// last_seen_at is more than a minute old: worth an UPDATE.
    pub needs_touch: bool,
    /// A profile the user holds requires MFA and they have not set it up.
    /// Local and directory (LDAP) accounts; never OIDC accounts, whose
    /// provider enforces MFA (see [`MFA_REQUIRED`]).
    pub mfa_enrolment_required: bool,
}

/// Whether a profile the user `u` holds requires MFA here. OIDC accounts are
/// exempt: their provider runs its own second factor. Local and directory
/// (LDAP) accounts are covered: a directory password alone is one factor.
/// Shared by the per-request gate and `/auth/me`, so the two cannot disagree.
pub const MFA_REQUIRED: &str = "(NOT EXISTS (SELECT 1 FROM identity_providers ip
                  WHERE ip.id = u.identity_provider_id AND ip.kind = 'oidc')
         AND EXISTS (SELECT 1 FROM user_permission_profiles up JOIN permission_profiles p ON p.id = up.profile_id
                  WHERE up.user_id = u.id AND p.require_mfa))";

pub async fn create_session(
    conn: &mut PgConnection,
    user_id: Uuid,
    token_hash: &[u8],
    csrf_token: &str,
    max_age: Duration,
    user_agent: Option<&str>,
    ip_address: Option<IpAddr>,
) -> sqlx::Result<Uuid> {
    sqlx::query_scalar(
        "INSERT INTO sessions (user_id, token_hash, csrf_token, expires_at, user_agent, ip_address)
         VALUES ($1, $2, $3, now() + $4::interval, $5, $6) RETURNING id",
    )
    .bind(user_id)
    .bind(token_hash)
    .bind(csrf_token)
    .bind(interval(max_age))
    .bind(user_agent)
    .bind(ip_address.map(IpNetwork::from))
    .fetch_one(conn)
    .await
}

pub async fn resolve_session(pool: &PgPool, token_hash: &[u8], idle: Duration) -> sqlx::Result<Option<LiveSession>> {
    let row: Option<(Uuid, Uuid, String, String, bool, bool)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT s.id, u.id, u.username, s.csrf_token, s.last_seen_at < now() - interval '1 minute',
                {MFA_REQUIRED}
                AND NOT EXISTS (SELECT 1 FROM user_totp t WHERE t.user_id = u.id AND t.confirmed_at IS NOT NULL)
         FROM sessions s JOIN users u ON u.id = s.user_id
         WHERE s.token_hash = $1 AND s.expires_at > now() AND s.last_seen_at > now() - $2::interval AND u.is_active"
    )))
    .bind(token_hash)
    .bind(interval(idle))
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(session_id, user_id, username, csrf_token, needs_touch, mfa_enrolment_required)| LiveSession {
        session_id,
        user_id,
        username,
        csrf_token,
        needs_touch,
        mfa_enrolment_required,
    }))
}

pub async fn touch_session(pool: &PgPool, id: Uuid) -> sqlx::Result<()> {
    sqlx::query("UPDATE sessions SET last_seen_at = now() WHERE id = $1").bind(id).execute(pool).await?;
    Ok(())
}

/// A session that was just ended, for its audit row (never the token or its hash).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct EndedSession {
    pub id: Uuid,
    pub user_id: Uuid,
    pub username: String,
    pub ip_address: Option<IpNetwork>,
    pub user_agent: Option<String>,
    pub created_at: DateTime<Utc>,
}

const ENDED: &str = "DELETE FROM sessions s USING users u WHERE u.id = s.user_id AND";
const ENDED_COLUMNS: &str = "RETURNING s.id, s.user_id, u.username, s.ip_address, s.user_agent, s.created_at";

pub async fn delete_session(conn: &mut PgConnection, id: Uuid) -> sqlx::Result<Option<EndedSession>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!("{ENDED} s.id = $1 {ENDED_COLUMNS}")))
        .bind(id)
        .fetch_optional(conn)
        .await
}

pub async fn delete_session_by_token(conn: &mut PgConnection, token_hash: &[u8]) -> sqlx::Result<Option<EndedSession>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!("{ENDED} s.token_hash = $1 {ENDED_COLUMNS}")))
        .bind(token_hash)
        .fetch_optional(conn)
        .await
}

/// Signs a user out everywhere, optionally keeping one session (the caller's own).
pub async fn delete_user_sessions(
    conn: &mut PgConnection,
    user_id: Uuid,
    keep: Option<Uuid>,
) -> sqlx::Result<Vec<EndedSession>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!("{ENDED} s.user_id = $1 AND s.id IS DISTINCT FROM $2 {ENDED_COLUMNS}")))
        .bind(user_id)
        .bind(keep)
        .fetch_all(conn)
        .await
}

/// Expired and idle sessions; called on login so the table stays small.
pub async fn purge_sessions(pool: &PgPool, idle: Duration) -> sqlx::Result<u64> {
    let done = sqlx::query("DELETE FROM sessions WHERE expires_at <= now() OR last_seen_at <= now() - $1::interval")
        .bind(interval(idle))
        .execute(pool)
        .await?;
    Ok(done.rows_affected())
}

// ---------------------------------------------------------------------------
// Effective permissions
// ---------------------------------------------------------------------------

/// The union of every profile the user holds, in one round trip.
pub async fn load_permissions(conn: &mut PgConnection, user_id: Uuid) -> sqlx::Result<Permissions> {
    type Row = (String, Option<String>, Option<Uuid>, bool, bool, bool, bool);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT 'admin', NULL::text, NULL::uuid, true, true, true, true
         FROM user_permission_profiles up JOIN permission_profiles p ON p.id = up.profile_id
         WHERE up.user_id = $1 AND p.is_builtin
         UNION ALL
         SELECT 'global', g.permission, NULL::uuid, false, false, false, false
         FROM user_permission_profiles up JOIN permission_profile_global_permissions g ON g.profile_id = up.profile_id
         WHERE up.user_id = $1
         UNION ALL
         SELECT 'class', NULL::text, c.class_id, c.can_view, c.can_create, c.can_edit, c.can_delete
         FROM user_permission_profiles up JOIN permission_profile_class_permissions c ON c.profile_id = up.profile_id
         WHERE up.user_id = $1",
    )
    .bind(user_id)
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

// ---------------------------------------------------------------------------
// Users
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UserRow {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
    pub email: Option<String>,
    pub is_active: bool,
    pub password_changed_at: DateTime<Utc>,
    pub last_login_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Set for an account that signs in through an identity provider.
    pub identity_provider_id: Option<Uuid>,
}

pub const USER_COLUMNS: &str = "id, username, display_name, email, is_active, password_changed_at, last_login_at, \
     created_at, updated_at, identity_provider_id";

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UserProfileRow {
    pub user_id: Uuid,
    pub id: Uuid,
    pub name: String,
    pub is_builtin: bool,
}

/// The profiles each of the given users holds, by name.
pub async fn profiles_of_users(conn: &mut PgConnection, user_ids: &[Uuid]) -> sqlx::Result<Vec<UserProfileRow>> {
    sqlx::query_as(
        "SELECT up.user_id, p.id, p.name, p.is_builtin
         FROM user_permission_profiles up JOIN permission_profiles p ON p.id = up.profile_id
         WHERE up.user_id = ANY($1)
         ORDER BY p.is_builtin DESC, lower(p.name), p.id",
    )
    .bind(user_ids)
    .fetch_all(conn)
    .await
}

pub async fn get_user(conn: &mut PgConnection, id: Uuid, for_update: bool) -> sqlx::Result<Option<UserRow>> {
    let lock = if for_update { " FOR UPDATE" } else { "" };
    sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {USER_COLUMNS} FROM users WHERE id = $1{lock}")))
        .bind(id)
        .fetch_optional(conn)
        .await
}

pub struct LoginRow {
    pub id: Uuid,
    pub username: String,
    /// None for an account that signs in through an identity provider.
    pub password_hash: Option<String>,
    pub is_active: bool,
    /// The provider the account belongs to, and its kind (`oidc`, `ldap`).
    pub provider: Option<(Uuid, String)>,
}

pub async fn find_for_login(pool: &PgPool, username: &str) -> sqlx::Result<Option<LoginRow>> {
    type Row = (Uuid, String, Option<String>, bool, Option<Uuid>, Option<String>);
    let row: Option<Row> = sqlx::query_as(
        "SELECT u.id, u.username, u.password_hash, u.is_active, u.identity_provider_id, p.kind
         FROM users u LEFT JOIN identity_providers p ON p.id = u.identity_provider_id
         WHERE lower(u.username) = lower($1)",
    )
    .bind(username)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(id, username, password_hash, is_active, provider_id, kind)| LoginRow {
        id,
        username,
        password_hash,
        is_active,
        provider: provider_id.zip(kind),
    }))
}

/// What confirming a user's own password needs to know about their account.
pub struct PasswordCheck {
    pub username: String,
    pub password_hash: Option<String>,
    /// The provider the account belongs to, and its kind (`oidc`, `ldap`).
    pub provider: Option<(Uuid, String)>,
    pub external_id: Option<String>,
}

pub async fn password_check(conn: &mut PgConnection, id: Uuid) -> sqlx::Result<Option<PasswordCheck>> {
    type Row = (String, Option<String>, Option<Uuid>, Option<String>, Option<String>);
    let row: Option<Row> = sqlx::query_as(
        "SELECT u.username, u.password_hash, u.identity_provider_id, p.kind, u.external_id
         FROM users u LEFT JOIN identity_providers p ON p.id = u.identity_provider_id
         WHERE u.id = $1",
    )
    .bind(id)
    .fetch_optional(conn)
    .await?;
    Ok(row.map(|(username, password_hash, provider_id, kind, external_id)| PasswordCheck {
        username,
        password_hash,
        provider: provider_id.zip(kind),
        external_id,
    }))
}

/// None: no such user; Some(None): the user signs in through an identity provider.
pub async fn password_hash(conn: &mut PgConnection, id: Uuid) -> sqlx::Result<Option<Option<String>>> {
    sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1").bind(id).fetch_optional(conn).await
}

pub async fn record_login(pool: &PgPool, id: Uuid) -> sqlx::Result<()> {
    sqlx::query("UPDATE users SET last_login_at = now() WHERE id = $1").bind(id).execute(pool).await?;
    Ok(())
}

pub struct NewUser<'a> {
    pub username: &'a str,
    pub display_name: &'a str,
    pub email: Option<&'a str>,
    pub password_hash: &'a str,
    pub is_active: bool,
}

pub async fn insert_user(conn: &mut PgConnection, u: &NewUser<'_>) -> sqlx::Result<Uuid> {
    sqlx::query_scalar(
        "INSERT INTO users (username, display_name, email, password_hash, is_active)
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(u.username)
    .bind(u.display_name)
    .bind(u.email)
    .bind(u.password_hash)
    .bind(u.is_active)
    .fetch_one(conn)
    .await
}

pub async fn set_password(conn: &mut PgConnection, id: Uuid, hash: &str) -> sqlx::Result<()> {
    sqlx::query("UPDATE users SET password_hash = $2, password_changed_at = now() WHERE id = $1")
        .bind(id)
        .bind(hash)
        .execute(conn)
        .await?;
    Ok(())
}

pub async fn delete_user(conn: &mut PgConnection, id: Uuid) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM users WHERE id = $1").bind(id).execute(conn).await?;
    Ok(())
}

/// Replaces the user's profiles with exactly these.
pub async fn set_user_profiles(conn: &mut PgConnection, user_id: Uuid, profile_ids: &[Uuid]) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM user_permission_profiles WHERE user_id = $1 AND NOT (profile_id = ANY($2))")
        .bind(user_id)
        .bind(profile_ids)
        .execute(&mut *conn)
        .await?;
    sqlx::query(
        "INSERT INTO user_permission_profiles (user_id, profile_id)
         SELECT $1, unnest($2::uuid[]) ON CONFLICT DO NOTHING",
    )
    .bind(user_id)
    .bind(profile_ids)
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn count_users(conn: &mut PgConnection) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT count(*) FROM users").fetch_one(conn).await
}

/// Serialises first-run setup: two concurrent requests cannot both see zero
/// users. A transaction-scoped advisory lock, so it blocks nothing but another
/// setup (a table lock would block every write to `users`, sign-in included).
pub async fn lock_setup(conn: &mut PgConnection) -> sqlx::Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('shadoucmdb.setup'))").execute(conn).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Permission profiles
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ProfileRow {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub is_builtin: bool,
    pub require_mfa: bool,
    pub user_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub const PROFILE_COLUMNS: &str = "p.id, p.name, p.description, p.is_builtin, p.require_mfa,
    (SELECT count(*) FROM user_permission_profiles up WHERE up.profile_id = p.id) AS user_count,
    p.created_at, p.updated_at";

pub async fn get_profile(conn: &mut PgConnection, id: Uuid, for_update: bool) -> sqlx::Result<Option<ProfileRow>> {
    let lock = if for_update { " FOR UPDATE OF p" } else { "" };
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {PROFILE_COLUMNS} FROM permission_profiles p WHERE p.id = $1{lock}"
    )))
    .bind(id)
    .fetch_optional(conn)
    .await
}

/// Every profile except the built-in one, by name.
pub async fn editable_profiles(conn: &mut PgConnection) -> sqlx::Result<Vec<ProfileRow>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {PROFILE_COLUMNS} FROM permission_profiles p WHERE NOT p.is_builtin ORDER BY lower(p.name)"
    )))
    .fetch_all(conn)
    .await
}

pub async fn builtin_profile_id(conn: &mut PgConnection) -> sqlx::Result<Uuid> {
    sqlx::query_scalar("SELECT id FROM permission_profiles WHERE is_builtin").fetch_one(conn).await
}

pub async fn existing_profiles(conn: &mut PgConnection, ids: &[Uuid]) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar("SELECT id FROM permission_profiles WHERE id = ANY($1)").bind(ids).fetch_all(conn).await
}

pub async fn existing_classes(conn: &mut PgConnection, ids: &[Uuid]) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar("SELECT id FROM ci_classes WHERE id = ANY($1)").bind(ids).fetch_all(conn).await
}

pub async fn insert_profile(
    conn: &mut PgConnection,
    name: &str,
    description: Option<&str>,
    require_mfa: bool,
) -> sqlx::Result<Uuid> {
    sqlx::query_scalar(
        "INSERT INTO permission_profiles (name, description, require_mfa) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(name)
    .bind(description)
    .bind(require_mfa)
    .fetch_one(conn)
    .await
}

pub async fn update_profile(
    conn: &mut PgConnection,
    id: Uuid,
    name: Option<&str>,
    description: Option<Option<&str>>,
    require_mfa: Option<bool>,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE permission_profiles
         SET name = COALESCE($2, name),
             description = CASE WHEN $3 THEN $4 ELSE description END,
             require_mfa = COALESCE($5, require_mfa),
             updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(name)
    .bind(description.is_some())
    .bind(description.flatten())
    .bind(require_mfa)
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn delete_profile(conn: &mut PgConnection, id: Uuid) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM permission_profiles WHERE id = $1").bind(id).execute(conn).await?;
    Ok(())
}

pub async fn profile_global_permissions(conn: &mut PgConnection, ids: &[Uuid]) -> sqlx::Result<Vec<(Uuid, String)>> {
    sqlx::query_as(
        "SELECT profile_id, permission FROM permission_profile_global_permissions
         WHERE profile_id = ANY($1) ORDER BY permission",
    )
    .bind(ids)
    .fetch_all(conn)
    .await
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ClassPermissionRow {
    pub profile_id: Uuid,
    pub class_id: Option<Uuid>,
    pub can_view: bool,
    pub can_create: bool,
    pub can_edit: bool,
    pub can_delete: bool,
}

pub async fn profile_class_permissions(conn: &mut PgConnection, ids: &[Uuid]) -> sqlx::Result<Vec<ClassPermissionRow>> {
    sqlx::query_as(
        "SELECT c.profile_id, c.class_id, c.can_view, c.can_create, c.can_edit, c.can_delete
         FROM permission_profile_class_permissions c LEFT JOIN ci_classes cls ON cls.id = c.class_id
         WHERE c.profile_id = ANY($1)
         ORDER BY c.class_id IS NOT NULL, lower(cls.name), c.class_id",
    )
    .bind(ids)
    .fetch_all(conn)
    .await
}

/// Replaces the profile's global permissions.
pub async fn set_global_permissions(conn: &mut PgConnection, id: Uuid, perms: &[GlobalPermission]) -> sqlx::Result<()> {
    let names: Vec<&str> = perms.iter().map(|p| p.as_str()).collect();
    sqlx::query("DELETE FROM permission_profile_global_permissions WHERE profile_id = $1")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    sqlx::query(
        "INSERT INTO permission_profile_global_permissions (profile_id, permission)
         SELECT $1, unnest($2::text[]) ON CONFLICT DO NOTHING",
    )
    .bind(id)
    .bind(names)
    .execute(conn)
    .await?;
    Ok(())
}

/// Replaces the profile's class permissions; entries granting nothing are dropped.
pub async fn set_class_permissions(
    conn: &mut PgConnection,
    id: Uuid,
    grants: &[(Option<Uuid>, ClassRights)],
) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM permission_profile_class_permissions WHERE profile_id = $1")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    for (class_id, rights) in grants {
        let r = rights.normalised();
        if r.is_empty() {
            continue;
        }
        sqlx::query(
            "INSERT INTO permission_profile_class_permissions
               (profile_id, class_id, can_view, can_create, can_edit, can_delete)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(id)
        .bind(class_id)
        .bind(r.view)
        .bind(r.create)
        .bind(r.edit)
        .bind(r.delete)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}
