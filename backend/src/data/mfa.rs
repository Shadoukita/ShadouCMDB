//! SQL for two-factor sign-in: TOTP authenticators, recovery codes and the
//! pending second-factor step of a sign-in.

use std::time::Duration;

use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::secrets::Sealed;

// ---------------------------------------------------------------------------
// Authenticator
// ---------------------------------------------------------------------------

pub struct Totp {
    /// Sealed with the keyring (see [`crate::secrets::sealed::open_totp_secret`]).
    pub secret: Vec<u8>,
    /// The key that sealed `secret`; NULL for a seed stored before encryption.
    pub key_id: Option<i32>,
    pub confirmed: bool,
    pub last_used_step: Option<i64>,
}

/// secret, key_id, confirmed, last_used_step
type TotpRow = (Vec<u8>, Option<i32>, bool, Option<i64>);

pub async fn get_totp(conn: &mut PgConnection, user_id: Uuid, for_update: bool) -> sqlx::Result<Option<Totp>> {
    let lock = if for_update { " FOR UPDATE" } else { "" };
    let row: Option<TotpRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT secret, key_id, confirmed_at IS NOT NULL, last_used_step FROM user_totp WHERE user_id = $1{lock}"
    )))
    .bind(user_id)
    .fetch_optional(conn)
    .await?;
    Ok(row.map(|(secret, key_id, confirmed, last_used_step)| Totp { secret, key_id, confirmed, last_used_step }))
}

/// Starts (or restarts) an enrolment: a new, unconfirmed secret, already sealed.
pub async fn put_pending_totp(conn: &mut PgConnection, user_id: Uuid, secret: &Sealed) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO user_totp (user_id, secret, key_id) VALUES ($1, $2, $3)
         ON CONFLICT (user_id) DO UPDATE SET secret = EXCLUDED.secret, key_id = EXCLUDED.key_id,
           confirmed_at = NULL, last_used_step = NULL, created_at = now()",
    )
    .bind(user_id)
    .bind(&secret.bytes)
    .bind(secret.key_id.0)
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn confirm_totp(conn: &mut PgConnection, user_id: Uuid, step: i64) -> sqlx::Result<()> {
    sqlx::query("UPDATE user_totp SET confirmed_at = now(), last_used_step = $2 WHERE user_id = $1")
        .bind(user_id)
        .bind(step)
        .execute(conn)
        .await?;
    Ok(())
}

/// Records `step` as used; false when it (or a later one) already was, i.e. a replay.
pub async fn use_step(conn: &mut PgConnection, user_id: Uuid, step: i64) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "UPDATE user_totp SET last_used_step = $2
         WHERE user_id = $1 AND confirmed_at IS NOT NULL AND (last_used_step IS NULL OR last_used_step < $2)",
    )
    .bind(user_id)
    .bind(step)
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Removes the authenticator and the recovery codes; whether a confirmed one existed.
pub async fn delete_mfa(conn: &mut PgConnection, user_id: Uuid) -> sqlx::Result<bool> {
    sqlx::query("DELETE FROM user_recovery_codes WHERE user_id = $1").bind(user_id).execute(&mut *conn).await?;
    sqlx::query("DELETE FROM mfa_challenges WHERE user_id = $1").bind(user_id).execute(&mut *conn).await?;
    let confirmed: Option<bool> =
        sqlx::query_scalar("DELETE FROM user_totp WHERE user_id = $1 RETURNING confirmed_at IS NOT NULL")
            .bind(user_id)
            .fetch_optional(conn)
            .await?;
    Ok(confirmed == Some(true))
}

// ---------------------------------------------------------------------------
// Recovery codes
// ---------------------------------------------------------------------------

/// Replaces the user's recovery codes with these hashes.
pub async fn set_recovery_codes(conn: &mut PgConnection, user_id: Uuid, hashes: &[Vec<u8>]) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM user_recovery_codes WHERE user_id = $1").bind(user_id).execute(&mut *conn).await?;
    sqlx::query("INSERT INTO user_recovery_codes (user_id, code_hash) SELECT $1, unnest($2::bytea[])")
        .bind(user_id)
        .bind(hashes)
        .execute(conn)
        .await?;
    Ok(())
}

/// Marks an unused code as used; false if there is no such unused code.
pub async fn use_recovery_code(conn: &mut PgConnection, user_id: Uuid, hash: &[u8]) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "UPDATE user_recovery_codes SET used_at = now() WHERE user_id = $1 AND code_hash = $2 AND used_at IS NULL",
    )
    .bind(user_id)
    .bind(hash)
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

pub struct Status {
    pub totp_enabled: bool,
    /// A profile the user holds requires MFA here (the same rule as the
    /// per-request gate, for the session given).
    pub required: bool,
    pub recovery_codes_remaining: i64,
}

/// `session`: the caller's session, whose sign-in may have proven MFA (OIDC).
pub async fn status(conn: &mut PgConnection, user_id: Uuid, session: Option<Uuid>) -> sqlx::Result<Status> {
    let (totp_enabled, required, recovery_codes_remaining): (bool, bool, i64) =
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT EXISTS (SELECT 1 FROM user_totp t WHERE t.user_id = u.id AND t.confirmed_at IS NOT NULL),
                    {},
                    (SELECT count(*) FROM user_recovery_codes r WHERE r.user_id = u.id AND r.used_at IS NULL)
             FROM users u LEFT JOIN sessions s ON s.id = $2 AND s.user_id = u.id WHERE u.id = $1",
            crate::data::auth::MFA_REQUIRED
        )))
        .bind(user_id)
        .bind(session)
        .fetch_one(conn)
        .await?;
    Ok(Status { totp_enabled, required, recovery_codes_remaining })
}

/// Which of these users have a confirmed authenticator.
pub async fn enabled_among(conn: &mut PgConnection, user_ids: &[Uuid]) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar("SELECT user_id FROM user_totp WHERE user_id = ANY($1) AND confirmed_at IS NOT NULL")
        .bind(user_ids)
        .fetch_all(conn)
        .await
}

// ---------------------------------------------------------------------------
// Pending second factor of a sign-in
// ---------------------------------------------------------------------------

pub async fn create_challenge(
    conn: &mut PgConnection,
    user_id: Uuid,
    token_hash: &[u8],
    ttl: Duration,
) -> sqlx::Result<Uuid> {
    sqlx::query_scalar(
        "INSERT INTO mfa_challenges (user_id, token_hash, expires_at)
         VALUES ($1, $2, now() + make_interval(secs => $3)) RETURNING id",
    )
    .bind(user_id)
    .bind(token_hash)
    .bind(ttl.as_secs() as f64)
    .fetch_one(conn)
    .await
}

pub struct Challenge {
    pub id: Uuid,
    pub user_id: Uuid,
    pub username: String,
}

/// The live challenge behind this token, locked, if its user is still active.
pub async fn take_challenge(conn: &mut PgConnection, token_hash: &[u8]) -> sqlx::Result<Option<Challenge>> {
    let row: Option<(Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT c.id, u.id, u.username FROM mfa_challenges c JOIN users u ON u.id = c.user_id
         WHERE c.token_hash = $1 AND c.expires_at > now() AND u.is_active
         FOR UPDATE OF c",
    )
    .bind(token_hash)
    .fetch_optional(conn)
    .await?;
    Ok(row.map(|(id, user_id, username)| Challenge { id, user_id, username }))
}

/// Counts a wrong code; the challenge is dropped once it reaches `max` failures.
pub async fn challenge_failed(conn: &mut PgConnection, id: Uuid, max: i32) -> sqlx::Result<()> {
    sqlx::query("UPDATE mfa_challenges SET failed_attempts = failed_attempts + 1 WHERE id = $1")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    sqlx::query("DELETE FROM mfa_challenges WHERE id = $1 AND failed_attempts >= $2")
        .bind(id)
        .bind(max)
        .execute(conn)
        .await?;
    Ok(())
}

pub async fn delete_challenge(conn: &mut PgConnection, id: Uuid) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM mfa_challenges WHERE id = $1").bind(id).execute(conn).await?;
    Ok(())
}

/// Expired challenges; called on sign-in so the table stays small.
pub async fn purge_challenges(pool: &PgPool) -> sqlx::Result<u64> {
    Ok(sqlx::query("DELETE FROM mfa_challenges WHERE expires_at <= now()").execute(pool).await?.rows_affected())
}
