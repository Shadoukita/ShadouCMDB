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

/// System tables whose rows are never backed up. Restoring sessions would sign
/// people back in with tokens from the past; after a restore everyone signs in again.
pub const EXCLUDED_TABLES: &[&str] = &["sessions"];

/// Schemas of the application's own tables: `cmdb` since migration 0008,
/// `public` before (and still for the migration bookkeeping table). Areas are
/// schemas too, created at run time; [`area_schemas`] lists them.
pub const SYSTEM_SCHEMAS: &[&str] = &["cmdb", "public"];

/// Where sqlx records applied migrations; part of the schema, not of the data.
const MIGRATIONS_TABLE: (&str, &str) = ("public", "_sqlx_migrations");

/// First migration with per-type tables in area schemas (SHAA-56).
pub const TYPE_TABLES_SINCE: i64 = 9;

/// A table, by schema and name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, sqlx::FromRow)]
pub struct Table {
    pub schema: String,
    pub name: String,
}

impl Table {
    pub fn new(schema: &str, name: &str) -> Self {
        Table { schema: schema.into(), name: name.into() }
    }

    /// `"schema"."name"`, for SQL.
    pub fn sql(&self) -> String {
        format!("{}.{}", ident(&self.schema), ident(&self.name))
    }

    /// `schema.name`, for people and the backup file.
    pub fn display(&self) -> String {
        format!("{}.{}", self.schema, self.name)
    }

    pub fn is_system(&self) -> bool {
        SYSTEM_SCHEMAS.contains(&self.schema.as_str())
    }

    /// Left out of a backup (see [`EXCLUDED_TABLES`]); a type table may be called `sessions`.
    pub fn is_excluded(&self) -> bool {
        self.is_system() && EXCLUDED_TABLES.contains(&self.name.as_str())
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

/// Schemas of the areas (`cmdb.areas`) that exist in the database. Empty
/// before migration 0008, and on a database without ShadouCMDB.
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

/// Every schema that holds ShadouCMDB data: the system schemas, then the areas.
pub async fn app_schemas(conn: &mut PgConnection) -> sqlx::Result<Vec<String>> {
    let mut schemas: Vec<String> = SYSTEM_SCHEMAS.iter().map(|s| s.to_string()).collect();
    schemas.extend(area_schemas(conn).await?);
    Ok(schemas)
}

/// Base tables of the application: the system tables (sorted by schema and
/// name), then the type tables of every area. Extension members and the
/// migration bookkeeping table are left out.
pub async fn app_tables(conn: &mut PgConnection) -> sqlx::Result<Vec<Table>> {
    let areas = area_schemas(conn).await?;
    let mut tables: Vec<Table> = sqlx::query_as(
        "SELECT n.nspname::text AS schema, c.relname::text AS name
         FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE (n.nspname = ANY ($1) OR n.nspname = ANY ($2)) AND c.relkind = 'r' AND NOT c.relispartition
           AND (n.nspname, c.relname) <> ($3, $4)
           AND NOT EXISTS (SELECT 1 FROM pg_depend d
                           WHERE d.classid = 'pg_class'::regclass AND d.objid = c.oid AND d.deptype = 'e')",
    )
    .bind(SYSTEM_SCHEMAS)
    .bind(&areas)
    .bind(MIGRATIONS_TABLE.0)
    .bind(MIGRATIONS_TABLE.1)
    .fetch_all(conn)
    .await?;
    tables.sort_by(|a, b| (!a.is_system(), a).cmp(&(!b.is_system(), b)));
    Ok(tables)
}

/// Stored (non-generated) columns of a table, in table order.
pub async fn stored_columns(conn: &mut PgConnection, table: &Table) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT attname::text FROM pg_attribute
         WHERE attrelid = to_regclass(format('%I.%I', $1, $2)) AND attnum > 0 AND NOT attisdropped
           AND attgenerated = ''
         ORDER BY attnum",
    )
    .bind(&table.schema)
    .bind(&table.name)
    .fetch_all(conn)
    .await
}

/// ShadouCMDB objects in the database: the `cmdb`, area and old Drizzle
/// schemas, plus what is in `public` and not part of an extension. Zero means
/// the database is empty as far as ShadouCMDB is concerned.
pub async fn app_object_count(conn: &mut PgConnection) -> sqlx::Result<i64> {
    let areas = area_schemas(conn).await?;
    let public = objects(conn, &["public".to_owned()]).await?;
    let schemas: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_namespace WHERE nspname IN ('cmdb', 'drizzle') OR nspname = ANY ($1)",
    )
    .bind(&areas)
    .fetch_one(conn)
    .await?;
    Ok(public.len() as i64 + schemas)
}

/// One droppable object: (SQL keyword, qualified name or signature).
type Object = (&'static str, String);

/// Tables, views, sequences, functions and types in `schemas` that are not
/// part of an extension (pg_trgm), and not sequences owned by a column (they
/// go with their table).
async fn objects(conn: &mut PgConnection, schemas: &[String]) -> sqlx::Result<Vec<Object>> {
    let relations: Vec<(String, String)> = sqlx::query_as(
        "SELECT c.relkind::text, quote_ident(n.nspname) || '.' || quote_ident(c.relname)
         FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = ANY ($1) AND c.relkind IN ('r', 'p', 'v', 'm', 'S', 'f')
           AND NOT (c.relkind = 'r' AND c.relispartition)
           AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_class'::regclass AND d.objid = c.oid
                           AND d.deptype IN ('e', 'a', 'i'))
         ORDER BY 2",
    )
    .bind(schemas)
    .fetch_all(&mut *conn)
    .await?;
    let functions: Vec<(String, String)> = sqlx::query_as(
        "SELECT p.prokind::text, quote_ident(n.nspname) || '.' || quote_ident(p.proname)
                                 || '(' || pg_get_function_identity_arguments(p.oid) || ')'
         FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
         WHERE n.nspname = ANY ($1)
           AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_proc'::regclass AND d.objid = p.oid
                           AND d.deptype = 'e')
         ORDER BY 2",
    )
    .bind(schemas)
    .fetch_all(&mut *conn)
    .await?;
    let types: Vec<(String, String)> = sqlx::query_as(
        "SELECT t.typtype::text, quote_ident(n.nspname) || '.' || quote_ident(t.typname)
         FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace
         WHERE n.nspname = ANY ($1) AND t.typtype IN ('e', 'd', 'r', 'c')
           AND (t.typtype <> 'c' OR (SELECT relkind FROM pg_class WHERE oid = t.typrelid) = 'c')
           AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_type'::regclass AND d.objid = t.oid
                           AND d.deptype = 'e')
         ORDER BY 2",
    )
    .bind(schemas)
    .fetch_all(&mut *conn)
    .await?;

    let mut out = Vec::new();
    for (kind, name) in relations {
        let keyword = match kind.as_str() {
            "v" => "VIEW",
            "m" => "MATERIALIZED VIEW",
            "f" => "FOREIGN TABLE",
            "S" => "SEQUENCE",
            _ => "TABLE",
        };
        out.push((keyword, name));
    }
    for (kind, name) in functions {
        let keyword = match kind.as_str() {
            "p" => "PROCEDURE",
            "a" => "AGGREGATE",
            _ => "FUNCTION",
        };
        out.push((keyword, name));
    }
    for (kind, name) in types {
        out.push((if kind == "d" { "DOMAIN" } else { "TYPE" }, name));
    }
    Ok(out)
}

/// Drops every ShadouCMDB object: the schema of every area (with its type
/// tables and reporting views), the `cmdb` schema (system tables, functions,
/// the schema change history), what migrations before 0008 left in `public`,
/// the migration history, and the old Node/Drizzle bookkeeping schema.
/// Extensions (pg_trgm) stay: they may be shared and are harmless.
/// Returns the number of objects dropped. Run inside a transaction.
pub async fn drop_app_objects(conn: &mut PgConnection) -> anyhow::Result<usize> {
    let areas = area_schemas(conn).await?;
    let mut schemas: Vec<String> = SYSTEM_SCHEMAS.iter().map(|s| s.to_string()).collect();
    schemas.extend(areas.iter().cloned());
    let dropped = objects(conn, &schemas).await?.len();

    let mut statements = Vec::new();
    for area in &areas {
        statements.push(format!("DROP SCHEMA IF EXISTS {} CASCADE", ident(area)));
    }
    statements.push("DROP SCHEMA IF EXISTS cmdb CASCADE".into());
    statements.push("DROP SCHEMA IF EXISTS drizzle CASCADE".into());
    // `public` is shared with pg_trgm (and possibly more), so object by object.
    for (keyword, name) in objects(conn, &["public".to_owned()]).await? {
        statements.push(format!("DROP {keyword} IF EXISTS {name} CASCADE"));
    }

    for s in &statements {
        sqlx::query(sqlx::AssertSqlSafe(s.clone()))
            .execute(&mut *conn)
            .await
            .with_context(|| format!("could not run: {s}"))?;
    }
    Ok(dropped)
}

#[cfg(test)]
mod tests;
