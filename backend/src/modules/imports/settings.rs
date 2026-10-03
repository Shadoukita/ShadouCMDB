//! The bulk import switch (D4) and the limits the UI shows (§3.5).
//!
//! The switch is one row in `import_settings`, off after install and upgrade.
//! `IMPORT_ALLOWED=false` is the operator's ceiling: import then stays off
//! whatever an administrator sets (W3).

use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{MAX_CELL_CHARS, MAX_COLUMNS, coded};
use crate::api::context::RequestContext;
use crate::api::route::Check;
use crate::config::ImportConfig;
use crate::data::crud::{self, AuditAction, AuditEntry};
use crate::http::error::{AppError, ErrorCode};

/// The audit entity id of the one settings row (it has no uuid of its own).
pub const SETTINGS_ENTITY_ID: Uuid = Uuid::nil();

/// Whether bulk import may be used on this instance, and its limits.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportSettings {
    /// Import is switched on and not forbidden by the server configuration.
    pub enabled: bool,
    /// The server configuration (`IMPORT_ALLOWED=false`) keeps import off; the switch cannot be turned on.
    pub locked: bool,
    pub limits: ImportLimits,
}

/// The effective limits of an upload.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportLimits {
    /// Largest file accepted, in bytes (`IMPORT_MAX_FILE_MB`)
    pub max_file_bytes: u64,
    /// Most data rows per file (`IMPORT_MAX_ROWS`)
    pub max_rows: u32,
    /// Most columns per file
    pub max_columns: u32,
    /// Most characters per cell
    pub max_cell_chars: u32,
}

/// Turns bulk import on or off for the whole instance.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateImportSettings {
    pub enabled: bool,
}
impl Check for UpdateImportSettings {}

/// The switch as stored (ignoring `IMPORT_ALLOWED`).
pub async fn stored_enabled(conn: &mut PgConnection) -> sqlx::Result<bool> {
    sqlx::query_scalar("SELECT enabled FROM cmdb.import_settings").fetch_one(conn).await
}

fn dto(stored: bool, cfg: &ImportConfig) -> ImportSettings {
    ImportSettings {
        enabled: cfg.allowed && stored,
        locked: !cfg.allowed,
        limits: ImportLimits {
            max_file_bytes: cfg.max_file_bytes,
            max_rows: cfg.max_rows,
            max_columns: MAX_COLUMNS,
            max_cell_chars: MAX_CELL_CHARS,
        },
    }
}

pub async fn get(pool: &PgPool, cfg: &ImportConfig) -> Result<ImportSettings, AppError> {
    let mut conn = pool.acquire().await?;
    Ok(dto(stored_enabled(&mut conn).await?, cfg))
}

/// Administrator only. Turning import on while `IMPORT_ALLOWED=false` is
/// `409 import_locked`; turning it off is always possible. A change is audited.
pub async fn update(
    pool: &PgPool,
    ctx: &RequestContext,
    cfg: &ImportConfig,
    input: &UpdateImportSettings,
) -> Result<ImportSettings, AppError> {
    ctx.require_administrator("turn bulk import on or off")?;
    if input.enabled && !cfg.allowed {
        return Err(coded(
            ErrorCode::Conflict,
            "Bulk import is disabled by the server configuration (IMPORT_ALLOWED=false).",
            "import_locked",
        ));
    }
    let mut tx = pool.begin().await?;
    let before: bool =
        sqlx::query_scalar("SELECT enabled FROM cmdb.import_settings FOR UPDATE").fetch_one(&mut *tx).await?;
    if before != input.enabled {
        let user = ctx.principal().map(|p| (p.user_id, p.username.clone()));
        sqlx::query("UPDATE cmdb.import_settings SET enabled = $1, updated_by_id = $2, updated_by_name = $3")
            .bind(input.enabled)
            .bind(user.as_ref().map(|u| u.0))
            .bind(user.as_ref().map(|u| u.1.as_str()).or(ctx.actor.name.as_deref()))
            .execute(&mut *tx)
            .await?;
        let entry = AuditEntry {
            action: AuditAction::Update,
            entity_type: "import_settings",
            entity_id: SETTINGS_ENTITY_ID,
            old_value: Some(json!({ "enabled": before })),
            new_value: Some(json!({ "enabled": input.enabled })),
        };
        crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    }
    tx.commit().await?;
    Ok(dto(input.enabled, cfg))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::permissions::{GlobalPermission, Permissions};
    use crate::db::scratch;

    const ADMIN: Uuid = Uuid::from_u128(0xa);
    const STEWARD: Uuid = Uuid::from_u128(0xb);

    fn user(administrator: bool) -> RequestContext {
        let permissions =
            Permissions { administrator, global: [GlobalPermission::CisImport].into(), ..Default::default() };
        let principal = crate::auth::Principal {
            user_id: if administrator { ADMIN } else { STEWARD },
            username: if administrator { "admin" } else { "steward" }.into(),
            credential: crate::auth::Credential::Session {
                id: Uuid::new_v4(),
                csrf_token: String::new(),
                mfa_enrolment_required: false,
                email_required: false,
                recently_confirmed: true,
            },
            permissions,
        };
        RequestContext::user(std::sync::Arc::new(principal), "test".into())
    }

    fn code(err: &AppError) -> (ErrorCode, Option<&str>) {
        (err.code, err.details.iter().flatten().next().map(|d| d.code.as_str()))
    }

    async fn audit_rows(pool: &PgPool) -> Vec<(serde_json::Value, serde_json::Value)> {
        sqlx::query_as("SELECT old_value, new_value FROM audit_log WHERE entity_type = 'import_settings' ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    /// G1/D4/W3: off by default, only an administrator switches it, every change
    /// is audited, and IMPORT_ALLOWED=false keeps it off.
    #[tokio::test]
    async fn the_switch_is_off_by_default_admin_only_audited_and_capped_by_the_server() {
        let Some(db) = scratch::database("import_switch").await else { return };
        let pool = &db.pool;
        sqlx::query(
            "INSERT INTO users (id, username, display_name, password_hash)
             VALUES ($1, 'admin', 'Admin', '$argon2id$v=19$test'), ($2, 'steward', 'Steward', '$argon2id$v=19$test')",
        )
        .bind(ADMIN)
        .bind(STEWARD)
        .execute(pool)
        .await
        .unwrap();
        let cfg = ImportConfig::default();
        let s = get(pool, &cfg).await.unwrap();
        assert!(!s.enabled && !s.locked);
        assert_eq!((s.limits.max_file_bytes, s.limits.max_rows), (50 * 1024 * 1024, 100_000));
        assert_eq!((s.limits.max_columns, s.limits.max_cell_chars), (200, 10_000));

        let on = UpdateImportSettings { enabled: true };
        let off = UpdateImportSettings { enabled: false };
        let err = update(pool, &user(false), &cfg, &on).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::Forbidden);
        assert!(update(pool, &user(true), &cfg, &on).await.unwrap().enabled);
        assert!(get(pool, &cfg).await.unwrap().enabled);
        // Saving the same value again is no change and no audit row.
        update(pool, &user(true), &cfg, &on).await.unwrap();
        assert_eq!(
            audit_rows(pool).await,
            [(serde_json::json!({ "enabled": false }), serde_json::json!({ "enabled": true }))]
        );
        let who: (Option<String>,) =
            sqlx::query_as("SELECT updated_by_name FROM import_settings").fetch_one(pool).await.unwrap();
        assert_eq!(who.0.as_deref(), Some("admin"));

        // The server configuration forbids import: shown off and locked, cannot be turned on,
        // can always be turned off.
        let locked = ImportConfig { allowed: false, ..ImportConfig::default() };
        let s = get(pool, &locked).await.unwrap();
        assert!(!s.enabled && s.locked);
        let err = update(pool, &user(true), &locked, &on).await.unwrap_err();
        assert_eq!(code(&err), (ErrorCode::Conflict, Some("import_locked")));
        assert!(!update(pool, &user(true), &locked, &off).await.unwrap().enabled);
        assert_eq!(audit_rows(pool).await.len(), 2);
        db.drop().await;
    }
}
