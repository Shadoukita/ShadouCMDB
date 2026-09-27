//! Backup, restore, factory reset and decommission (SHAA-87).
//!
//! Command line only, on purpose: none of this is reachable over HTTP, so a
//! stolen session or API credential cannot wipe or replace the database.
//! Everything that changes data runs in one transaction; a failure (or a
//! `--dry-run`) leaves the database exactly as it was.

pub mod archive;
pub mod backup;
pub mod reset;
pub mod restore;

use std::io::{BufRead, IsTerminal, Write};

use anyhow::{Context, bail};
use clap::Args;
use sqlx::Connection;
use sqlx::postgres::PgConnection;

use crate::config::DatabaseConfig;

/// Tables whose rows are never backed up. Restoring sessions would sign
/// people back in with tokens from the past; after a restore everyone signs in again.
pub const EXCLUDED_TABLES: &[&str] = &["sessions"];

/// Where sqlx records applied migrations; part of the schema, not of the data.
const MIGRATIONS_TABLE: &str = "_sqlx_migrations";

#[derive(Debug, Args)]
pub struct ConfirmArgs {
    /// Do not ask for confirmation (for scripts). Without it the command asks
    /// you to type the database name, and refuses when there is no terminal.
    #[arg(long)]
    pub yes: bool,
}

/// `"name"`: an identifier quoted for SQL.
pub fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// A dedicated connection (not a pool) with the session settings that make
/// the row text round-trip exactly and no statement timeout for long copies.
pub async fn connect(cfg: &DatabaseConfig) -> anyhow::Result<PgConnection> {
    let mut conn = PgConnection::connect_with(&crate::db::connect_options(cfg)?)
        .await
        .context("could not connect to PostgreSQL")?;
    session_settings(&mut conn).await?;
    Ok(conn)
}

pub async fn session_settings(conn: &mut PgConnection) -> sqlx::Result<()> {
    for (name, value) in [
        ("statement_timeout", "0"),
        ("TimeZone", "UTC"),
        ("bytea_output", "hex"),
        ("extra_float_digits", "3"),
        ("IntervalStyle", "postgres"),
        ("DateStyle", "ISO, YMD"),
    ] {
        sqlx::query("SELECT set_config($1, $2, false)").bind(name).bind(value).execute(&mut *conn).await?;
    }
    Ok(())
}

/// `database "x" on host:port`, for messages and the confirmation prompt.
pub async fn describe(conn: &mut PgConnection) -> anyhow::Result<(String, String)> {
    let (db, addr, port): (String, Option<String>, Option<i32>) =
        sqlx::query_as("SELECT current_database(), host(inet_server_addr()), inet_server_port()")
            .fetch_one(&mut *conn)
            .await?;
    let place = match (addr, port) {
        (Some(a), Some(p)) => format!("database \"{db}\" on {a}:{p}"),
        _ => format!("database \"{db}\" (local socket)"),
    };
    Ok((db, place))
}

/// Refuses to change the database while a ShadouCMDB server (or another admin
/// command) is connected: its pooled connections would see tables vanish
/// under running requests.
pub async fn ensure_no_other_clients(conn: &mut PgConnection) -> anyhow::Result<()> {
    let others: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_stat_activity
         WHERE datname = current_database() AND pid <> pg_backend_pid() AND application_name = 'shadoucmdb'",
    )
    .fetch_one(&mut *conn)
    .await?;
    if others > 0 {
        bail!(
            "{others} other ShadouCMDB connection(s) are open to this database. Stop the server (and any other \
             shadoucmdb command) first, then run this again."
        );
    }
    Ok(())
}

/// Asks the operator to type the database name; `--yes` skips the question.
pub fn confirm(action: &str, database: &str, place: &str, yes: bool) -> anyhow::Result<()> {
    if yes {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        bail!("no terminal to confirm on; pass --yes to {action} {place} without asking");
    }
    eprint!("This will {action} {place}. This cannot be undone.\nType the database name ({database}) to continue: ");
    std::io::stderr().flush().ok();
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer).context("cannot read the confirmation")?;
    if answer.trim() != database {
        bail!("confirmation did not match; nothing was changed");
    }
    Ok(())
}

/// Base tables of the application schema, excluding extension members and the
/// migration bookkeeping table, sorted by name.
pub async fn app_tables(conn: &mut PgConnection) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT c.relname::text FROM pg_class c
         WHERE c.relnamespace = current_schema()::regnamespace AND c.relkind = 'r' AND NOT c.relispartition
           AND c.relname <> $1
           AND NOT EXISTS (SELECT 1 FROM pg_depend d
                           WHERE d.classid = 'pg_class'::regclass AND d.objid = c.oid AND d.deptype = 'e')
         ORDER BY c.relname",
    )
    .bind(MIGRATIONS_TABLE)
    .fetch_all(conn)
    .await
}

/// Stored (non-generated) columns of a table, in table order.
pub async fn stored_columns(conn: &mut PgConnection, table: &str) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT attname::text FROM pg_attribute
         WHERE attrelid = to_regclass(quote_ident($1)) AND attnum > 0 AND NOT attisdropped AND attgenerated = ''
         ORDER BY attnum",
    )
    .bind(table)
    .fetch_all(conn)
    .await
}

/// Objects in the application schema that are not part of an extension. Zero
/// means the database is empty as far as ShadouCMDB is concerned.
pub async fn app_object_count(conn: &mut PgConnection) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM pg_class c
                 WHERE c.relnamespace = current_schema()::regnamespace AND c.relkind IN ('r', 'p', 'v', 'm', 'S', 'f')
                   AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_class'::regclass
                                   AND d.objid = c.oid AND d.deptype = 'e'))
              + (SELECT count(*) FROM pg_proc p
                 WHERE p.pronamespace = current_schema()::regnamespace
                   AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_proc'::regclass
                                   AND d.objid = p.oid AND d.deptype = 'e'))
              + (SELECT count(*) FROM pg_namespace WHERE nspname = 'drizzle')",
    )
    .fetch_one(conn)
    .await
}

/// Drops every ShadouCMDB object: all tables (with their rows, indexes,
/// triggers and sequences), views, functions and types in the application
/// schema, the migration history, and the old Node/Drizzle bookkeeping schema.
/// Extensions (pg_trgm) stay: they may be shared and are harmless.
/// Returns the number of objects dropped. Run inside a transaction.
pub async fn drop_app_objects(conn: &mut PgConnection) -> anyhow::Result<usize> {
    let relations: Vec<(String, String)> = sqlx::query_as(
        "SELECT c.relkind::text, quote_ident(c.relname) FROM pg_class c
         WHERE c.relnamespace = current_schema()::regnamespace AND c.relkind IN ('r', 'p', 'v', 'm', 'S', 'f')
           AND NOT (c.relkind = 'r' AND c.relispartition)
           -- extension members, and sequences owned by a column (they go with their table)
           AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_class'::regclass AND d.objid = c.oid
                           AND d.deptype IN ('e', 'a', 'i'))
         ORDER BY c.relname",
    )
    .fetch_all(&mut *conn)
    .await?;
    let functions: Vec<(String, String)> = sqlx::query_as(
        "SELECT p.prokind::text, p.oid::regprocedure::text FROM pg_proc p
         WHERE p.pronamespace = current_schema()::regnamespace
           AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_proc'::regclass AND d.objid = p.oid
                           AND d.deptype = 'e')",
    )
    .fetch_all(&mut *conn)
    .await?;
    let types: Vec<(String, String)> = sqlx::query_as(
        "SELECT t.typtype::text, format_type(t.oid, NULL) FROM pg_type t
         WHERE t.typnamespace = current_schema()::regnamespace AND t.typtype IN ('e', 'd', 'r', 'c')
           AND (t.typtype <> 'c' OR (SELECT relkind FROM pg_class WHERE oid = t.typrelid) = 'c')
           AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_type'::regclass AND d.objid = t.oid
                           AND d.deptype = 'e')",
    )
    .fetch_all(&mut *conn)
    .await?;

    let of = |kinds: &[&str]| -> Vec<String> {
        relations.iter().filter(|(k, _)| kinds.contains(&k.as_str())).map(|(_, n)| n.clone()).collect()
    };
    let mut statements = Vec::new();
    for (kinds, keyword) in [
        (&["r", "p"][..], "TABLE"),
        (&["v"][..], "VIEW"),
        (&["m"][..], "MATERIALIZED VIEW"),
        (&["f"][..], "FOREIGN TABLE"),
        (&["S"][..], "SEQUENCE"),
    ] {
        let names = of(kinds);
        if !names.is_empty() {
            statements.push(format!("DROP {keyword} IF EXISTS {} CASCADE", names.join(", ")));
        }
    }
    for (kind, signature) in &functions {
        let keyword = match kind.as_str() {
            "p" => "PROCEDURE",
            "a" => "AGGREGATE",
            _ => "FUNCTION",
        };
        statements.push(format!("DROP {keyword} IF EXISTS {signature} CASCADE"));
    }
    for (kind, name) in &types {
        let keyword = if kind == "d" { "DOMAIN" } else { "TYPE" };
        statements.push(format!("DROP {keyword} IF EXISTS {name} CASCADE"));
    }
    statements.push("DROP SCHEMA IF EXISTS drizzle CASCADE".into());

    for s in &statements {
        sqlx::query(sqlx::AssertSqlSafe(s.clone()))
            .execute(&mut *conn)
            .await
            .with_context(|| format!("could not run: {s}"))?;
    }
    Ok(relations.len() + functions.len() + types.len())
}

#[cfg(test)]
mod tests;
