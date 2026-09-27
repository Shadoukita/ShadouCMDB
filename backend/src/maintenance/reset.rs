//! `shadoucmdb factory-reset` and `shadoucmdb decommission`.
//!
//! Factory reset: every table is dropped and the schema rebuilt by the
//! migrations, which is exactly a fresh install. No users exist afterwards, so
//! the web UI forces first-run setup (or use `create-admin`).
//!
//! Decommission: every ShadouCMDB table, row, setting and the migration history
//! are dropped and nothing is rebuilt. What is left outside the database
//! (the database and role themselves, the env file, backups, logs) is listed
//! for the operator; see docs/backup-and-reset.md.

use anyhow::{Context, bail};
use sqlx::Connection;
use sqlx::postgres::PgConnection;

use super::ConfirmArgs;
use crate::config::DatabaseConfig;
use crate::db::MIGRATOR;

pub async fn factory_reset_cmd(cfg: &DatabaseConfig, args: ConfirmArgs) -> anyhow::Result<()> {
    let mut conn = super::connect(cfg).await?;
    let (database, place) = super::describe(&mut conn).await?;
    super::ensure_no_other_clients(&mut conn).await?;
    super::confirm(
        "delete all CIs, the data model, users, settings and the audit log in",
        &database,
        &place,
        args.yes,
    )?;
    let dropped = factory_reset(&mut conn).await;
    conn.close().await.ok();
    let dropped = dropped?;
    println!("Factory reset of {place}: {dropped} objects dropped, schema rebuilt at migration {}", level());
    println!("No users exist: open the web UI to run first-run setup, or use `shadoucmdb create-admin`.");
    Ok(())
}

pub async fn decommission_cmd(cfg: &DatabaseConfig, args: ConfirmArgs) -> anyhow::Result<()> {
    let mut conn = super::connect(cfg).await?;
    let (database, place) = super::describe(&mut conn).await?;
    super::ensure_no_other_clients(&mut conn).await?;
    super::confirm("permanently remove every ShadouCMDB table, row and setting from", &database, &place, args.yes)?;
    let dropped = decommission(&mut conn).await;
    conn.close().await.ok();
    let dropped = dropped?;
    println!("Decommissioned {place}: {dropped} objects dropped, no ShadouCMDB data or settings remain in it.");
    println!("To finish (see docs/backup-and-reset.md):");
    println!("  1. As a PostgreSQL administrator: DROP DATABASE \"{database}\"; DROP ROLE <the application role>;");
    println!("  2. Remove the service (systemd unit, `shadoucmdb service uninstall`, or the container).");
    println!("  3. Delete the env file (it holds the database password), the log files and the binary.");
    println!("  4. Destroy or retire the backups according to your retention policy.");
    Ok(())
}

fn level() -> i64 {
    MIGRATOR.iter().filter(|m| m.migration_type.is_up_migration()).map(|m| m.version).max().unwrap_or(0)
}

/// Drops everything and migrates from scratch, in one transaction.
pub async fn factory_reset(conn: &mut PgConnection) -> anyhow::Result<usize> {
    super::session_settings(conn).await?;
    let mut tx = conn.begin().await?;
    let dropped = super::drop_app_objects(&mut tx).await?;
    MIGRATOR.run(&mut *tx).await.context("rebuilding the schema failed")?;
    let (builtin, users): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM permission_profiles WHERE is_builtin), (SELECT count(*) FROM users)",
    )
    .fetch_one(&mut *tx)
    .await?;
    if builtin != 1 || users != 0 {
        bail!("after the reset expected the built-in profile and no users, found {builtin} and {users}");
    }
    tx.commit().await?;
    Ok(dropped)
}

/// Drops everything, in one transaction, and checks nothing is left.
pub async fn decommission(conn: &mut PgConnection) -> anyhow::Result<usize> {
    let mut tx = conn.begin().await?;
    let dropped = super::drop_app_objects(&mut tx).await?;
    let left = super::app_object_count(&mut tx).await?;
    if left != 0 {
        bail!("{left} objects are still in the schema after dropping; nothing was changed");
    }
    tx.commit().await?;
    Ok(dropped)
}
