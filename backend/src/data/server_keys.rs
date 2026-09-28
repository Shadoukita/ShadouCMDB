//! Secrets the server generates for itself (`server_keys`, migration 0021).

use sqlx::PgPool;

/// The key for `purpose`: its id and 32 secret bytes.
pub async fn get(pool: &PgPool, purpose: &str) -> sqlx::Result<Option<(i16, Vec<u8>)>> {
    sqlx::query_as("SELECT key_id, secret FROM server_keys WHERE purpose = $1").bind(purpose).fetch_optional(pool).await
}

/// Stores a key for `purpose` unless one exists already (another process may
/// have been first); read it back with [`get`] either way.
pub async fn insert_if_absent(pool: &PgPool, purpose: &str, key_id: i16, secret: &[u8]) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO server_keys (purpose, key_id, secret) VALUES ($1, $2, $3) ON CONFLICT (purpose) DO NOTHING",
    )
    .bind(purpose)
    .bind(key_id)
    .bind(secret)
    .execute(pool)
    .await?;
    Ok(())
}
