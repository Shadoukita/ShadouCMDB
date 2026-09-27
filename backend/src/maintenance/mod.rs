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

/// Schemas of the application's own tables: `cmdb` since migration 0008,
/// `public` before it (now only the migration history and pg_trgm). Every
/// other schema ShadouCMDB owns is an area, listed in `cmdb.areas`.
pub const SYSTEM_SCHEMAS: &[&str] = &["public", "cmdb"];

/// The first migration that keeps attribute values in per-type tables in the
/// area schemas. From here on those tables are part of a backup.
pub const TYPE_TABLES_MIGRATION: i64 = 9;

/// A table of the application: a system table or the table of a type.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Table {
    pub schema: String,
    pub name: String,
}

impl Table {
    /// `"schema"."name"`
    pub fn sql(&self) -> String {
        format!("{}.{}", ident(&self.schema), ident(&self.name))
    }

    /// In an area schema (the table of a type), rather than a system table.
    pub fn is_area(&self) -> bool {
        !SYSTEM_SCHEMAS.contains(&self.schema.as_str())
    }
}

impl std::fmt::Display for Table {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.schema, self.name)
    }
}

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

/// Schemas of the areas that exist in the database, from `cmdb.areas`.
pub async fn area_schemas(conn: &mut PgConnection) -> sqlx::Result<Vec<String>> {
    let has_areas: bool =
        sqlx::query_scalar("SELECT to_regclass('cmdb.areas') IS NOT NULL").fetch_one(&mut *conn).await?;
    if !has_areas {
        return Ok(Vec::new());
    }
    sqlx::query_scalar(
        "SELECT a.key FROM cmdb.areas a WHERE EXISTS (SELECT 1 FROM pg_namespace n WHERE n.nspname = a.key)
         ORDER BY a.key",
    )
    .fetch_all(conn)
    .await
}

/// The system schemas followed by the area schemas.
pub async fn app_schemas(conn: &mut PgConnection) -> sqlx::Result<Vec<String>> {
    let mut schemas: Vec<String> = SYSTEM_SCHEMAS.iter().map(|s| (*s).to_owned()).collect();
    schemas.extend(area_schemas(conn).await?);
    Ok(schemas)
}

/// Base tables of the application, excluding extension members and the
/// migration bookkeeping table: system tables first, then the tables of the
/// types, each sorted by schema and name.
pub async fn app_tables(conn: &mut PgConnection) -> sqlx::Result<Vec<Table>> {
    let schemas = app_schemas(conn).await?;
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT n.nspname::text, c.relname::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = ANY($1) AND c.relkind = 'r' AND NOT c.relispartition
           AND NOT (n.nspname = 'public' AND c.relname = '_sqlx_migrations')
           AND NOT EXISTS (SELECT 1 FROM pg_depend d
                           WHERE d.classid = 'pg_class'::regclass AND d.objid = c.oid AND d.deptype = 'e')",
    )
    .bind(&schemas)
    .fetch_all(conn)
    .await?;
    let mut tables: Vec<Table> = rows.into_iter().map(|(schema, name)| Table { schema, name }).collect();
    tables.sort_by(|a, b| (a.is_area(), a).cmp(&(b.is_area(), b)));
    Ok(tables)
}

/// Stored (non-generated) columns of a table, in table order.
pub async fn stored_columns(conn: &mut PgConnection, table: &Table) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT attname::text FROM pg_attribute
         WHERE attrelid = to_regclass(format('%I.%I', $1::text, $2::text))
           AND attnum > 0 AND NOT attisdropped AND attgenerated = ''
         ORDER BY attnum",
    )
    .bind(&table.schema)
    .bind(&table.name)
    .fetch_all(conn)
    .await
}

/// Relations, functions and types in `schemas` that are not part of an extension.
async fn object_count(conn: &mut PgConnection, schemas: &[String]) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                 WHERE n.nspname = ANY($1) AND c.relkind IN ('r', 'p', 'v', 'm', 'S', 'f')
                   AND NOT (c.relkind = 'r' AND c.relispartition)
                   AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_class'::regclass
                                   AND d.objid = c.oid AND d.deptype IN ('e', 'a', 'i')))
              + (SELECT count(*) FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
                 WHERE n.nspname = ANY($1)
                   AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_proc'::regclass
                                   AND d.objid = p.oid AND d.deptype = 'e'))",
    )
    .bind(schemas)
    .fetch_one(conn)
    .await
}

/// Objects ShadouCMDB left in the database: everything in `public` that is not
/// part of an extension (the migration history, or the tables of an install
/// from before migration 0008), plus the `cmdb`, area and old Node/Drizzle
/// schemas themselves. Zero means the database is empty as far as ShadouCMDB
/// is concerned.
pub async fn app_object_count(conn: &mut PgConnection) -> sqlx::Result<i64> {
    let mut schemas = vec!["cmdb".to_owned(), "drizzle".to_owned()];
    schemas.extend(area_schemas(conn).await?);
    let in_public = object_count(conn, &["public".to_owned()]).await?;
    let own_schemas: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_namespace WHERE nspname = ANY($1)")
        .bind(&schemas)
        .fetch_one(conn)
        .await?;
    Ok(in_public + own_schemas)
}

/// Drops every ShadouCMDB object: the area schemas (the tables of the types
/// and their reporting views), the `cmdb` schema (all system tables with their
/// rows, indexes, triggers and sequences, functions and types), everything
/// else in `public` (the migration history, and the tables of an install from
/// before migration 0008) and the old Node/Drizzle bookkeeping schema.
/// Extensions (pg_trgm) stay: they may be shared and are harmless.
/// Returns the number of objects dropped. Run inside a transaction.
pub async fn drop_app_objects(conn: &mut PgConnection) -> anyhow::Result<usize> {
    let areas = area_schemas(&mut *conn).await?;
    let mut owned = areas.clone();
    owned.push("cmdb".into());
    let in_schemas = object_count(&mut *conn, &owned).await?;
    let mut statements: Vec<String> = Vec::new();
    // Areas first: their tables reference cmdb.configuration_items.
    for schema in areas.iter().map(String::as_str).chain(["cmdb", "drizzle"]) {
        statements.push(format!("DROP SCHEMA IF EXISTS {} CASCADE", ident(schema)));
    }

    let relations: Vec<(String, String)> = sqlx::query_as(
        "SELECT c.relkind::text, quote_ident(c.relname) FROM pg_class c
         WHERE c.relnamespace = 'public'::regnamespace AND c.relkind IN ('r', 'p', 'v', 'm', 'S', 'f')
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
         WHERE p.pronamespace = 'public'::regnamespace
           AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_proc'::regclass AND d.objid = p.oid
                           AND d.deptype = 'e')",
    )
    .fetch_all(&mut *conn)
    .await?;
    let types: Vec<(String, String)> = sqlx::query_as(
        "SELECT t.typtype::text, format_type(t.oid, NULL) FROM pg_type t
         WHERE t.typnamespace = 'public'::regnamespace AND t.typtype IN ('e', 'd', 'r', 'c')
           AND (t.typtype <> 'c' OR (SELECT relkind FROM pg_class WHERE oid = t.typrelid) = 'c')
           AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_type'::regclass AND d.objid = t.oid
                           AND d.deptype = 'e')",
    )
    .fetch_all(&mut *conn)
    .await?;

    let of = |kinds: &[&str]| -> Vec<String> {
        relations.iter().filter(|(k, _)| kinds.contains(&k.as_str())).map(|(_, n)| format!("public.{n}")).collect()
    };
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
    // Names are qualified: regprocedure and format_type omit `public.` when it is on the search_path.
    for (kind, signature) in &functions {
        let keyword = match kind.as_str() {
            "p" => "PROCEDURE",
            "a" => "AGGREGATE",
            _ => "FUNCTION",
        };
        let signature =
            if signature.starts_with("public.") { signature.clone() } else { format!("public.{signature}") };
        statements.push(format!("DROP {keyword} IF EXISTS {signature} CASCADE"));
    }
    for (kind, name) in &types {
        let keyword = if kind == "d" { "DOMAIN" } else { "TYPE" };
        let name = if name.starts_with("public.") { name.clone() } else { format!("public.{name}") };
        statements.push(format!("DROP {keyword} IF EXISTS {name} CASCADE"));
    }

    for s in &statements {
        sqlx::query(sqlx::AssertSqlSafe(s.clone()))
            .execute(&mut *conn)
            .await
            .with_context(|| format!("could not run: {s}"))?;
    }
    Ok(in_schemas as usize + relations.len() + functions.len() + types.len())
}

#[cfg(test)]
mod tests;
