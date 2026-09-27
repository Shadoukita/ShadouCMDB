//! `shadoucmdb restore`: checks a backup completely, then loads it in a single
//! transaction. The schema is rebuilt by this binary's migrations up to the
//! level the backup was taken at, the rows go in, and any newer migrations run
//! on top, so a backup from an older release restores into a newer one.
//!
//! Type tables: the system tables are loaded first; the data model in them
//! (areas, types, fields) then drives the DDL engine, which rebuilds every area
//! schema, type table and reporting view before their rows are loaded.

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::Args;
use sqlx::Connection;
use sqlx::postgres::PgConnection;

use super::archive::{self, Header, Reader, TableEntry};
use super::{TYPE_TABLES_SINCE, Table, app_tables, ident, stored_columns};
use crate::config::DatabaseConfig;
use crate::db::MIGRATOR;

#[derive(Debug, Args)]
pub struct RestoreArgs {
    /// Backup file written by `shadoucmdb backup`.
    #[arg(value_name = "FILE")]
    pub file: PathBuf,
    /// Replace a database that already has ShadouCMDB tables (all its current
    /// data, users and settings are deleted). Without it, the target must be empty.
    #[arg(long)]
    pub replace: bool,
    /// Do the whole restore, including every check, then roll it back.
    #[arg(long)]
    pub dry_run: bool,
    /// Do not ask for confirmation before replacing a database (for scripts).
    #[arg(long)]
    pub yes: bool,
}

/// Rows per INSERT; also capped by [`BATCH_BYTES`].
const BATCH_ROWS: usize = 1000;
const BATCH_BYTES: usize = 8 << 20;

#[derive(Debug)]
pub struct Report {
    pub rows: u64,
    pub users: i64,
    pub migrations_applied_after: usize,
    /// Required fields left nullable because some assets have no value.
    pub warnings: Vec<String>,
}

pub async fn run(cfg: &DatabaseConfig, args: RestoreArgs) -> anyhow::Result<()> {
    println!("Checking {} ...", args.file.display());
    let header = archive::verify_file(&args.file)?;
    println!(
        "Backup of database \"{}\" taken {} by ShadouCMDB {}: {} rows in {} tables, migration {}",
        header.database,
        header.created_at.format("%Y-%m-%d %H:%M:%S UTC"),
        header.app_version,
        header.total_rows(),
        header.tables.len(),
        header.migration_level().unwrap_or_default()
    );
    check_compatible(&header)?;
    println!("File is intact (SHA-256 and row counts match) and fits this release");

    let mut conn = super::connect(cfg).await?;
    let (database, place) = super::describe(&mut conn).await?;
    super::ensure_no_other_clients(&mut conn).await?;
    let populated = super::app_object_count(&mut conn).await? > 0;
    if populated && !args.replace {
        bail!(
            "{place} already contains ShadouCMDB tables. Restore into an empty database, or pass --replace to \
             delete everything in it first (take a backup of it before you do)."
        );
    }
    if populated && !args.dry_run {
        super::confirm("delete all data, users and settings in", &database, &place, args.yes)?;
    }

    let file = std::fs::File::open(&args.file).with_context(|| format!("cannot open {}", args.file.display()))?;
    let report = restore(&mut conn, file, &header, populated, !args.dry_run).await;
    conn.close().await.ok();
    let report = report?;

    let verb = if args.dry_run { "Dry run: would restore" } else { "Restored" };
    println!("{verb} {} rows into {place}", report.rows);
    if report.migrations_applied_after > 0 {
        println!("Applied {} newer migration(s) on top of the backup", report.migrations_applied_after);
    }
    for w in &report.warnings {
        println!("  warning: {w}");
    }
    if args.dry_run {
        println!("Rolled back: nothing was changed");
    } else if report.users == 0 {
        println!("The backup has no users: the web UI will ask for first-run setup");
    } else {
        println!("{} user(s) restored; sessions are not part of a backup, so everyone signs in again", report.users);
    }
    Ok(())
}

/// The backup's migrations must be ones this binary ships, with the same SQL.
pub fn check_compatible(header: &Header) -> anyhow::Result<()> {
    let ours: HashMap<i64, String> = MIGRATOR
        .iter()
        .filter(|m| m.migration_type.is_up_migration())
        .map(|m| (m.version, hex::encode(&*m.checksum)))
        .collect();
    if header.migrations.is_empty() {
        bail!("the backup records no migrations; it cannot be matched to a schema");
    }
    for m in &header.migrations {
        match ours.get(&m.version) {
            None => bail!(
                "the backup was taken at migration {:04} ({}), which this binary does not know; restore it with \
                 ShadouCMDB {} or newer",
                m.version,
                m.description,
                header.app_version
            ),
            Some(sum) if *sum != m.checksum => bail!(
                "migration {:04} ({}) in the backup differs from the one in this binary; use the release the \
                 backup was taken with ({})",
                m.version,
                m.description,
                header.app_version
            ),
            Some(_) => {}
        }
    }
    Ok(())
}

/// Loads `source` (already checked with [`archive::verify`]) into the
/// connected database in one transaction; `wipe` drops the existing
/// ShadouCMDB objects first, `commit = false` rolls everything back.
pub async fn restore<R: Read>(
    conn: &mut PgConnection,
    source: R,
    header: &Header,
    wipe: bool,
    commit: bool,
) -> anyhow::Result<Report> {
    check_compatible(header)?;
    let level = header.migration_level().context("backup has no migrations")?;
    let (system, types): (Vec<&TableEntry>, Vec<&TableEntry>) =
        header.tables.iter().partition(|t| Table::new(&t.schema, &t.name).is_system());
    if header.tables.iter().skip(system.len()).any(|t| Table::new(&t.schema, &t.name).is_system()) {
        bail!("backup is damaged: its system tables do not all come before the type tables");
    }
    if level < TYPE_TABLES_SINCE && !types.is_empty() {
        bail!("backup is damaged: it has type tables, but migration {level} has none");
    }
    super::session_settings(conn).await?;
    let mut tx = conn.begin().await?;

    if wipe {
        super::drop_app_objects(&mut tx).await?;
    }
    // Each migration runs in a savepoint of this transaction.
    MIGRATOR.run_to(level, &mut *tx).await.context("rebuilding the schema of the backup failed")?;

    // The schema at that level must have exactly the backup's system tables.
    let system_tables = app_tables(&mut tx).await?;
    match_tables(&mut tx, &system_tables, &system, &format!("migration {level}")).await?;

    // Migrations can leave deferred checks queued (0005 seeds ui_settings under a
    // deferred foreign key), and ALTER TABLE refuses tables with pending events:
    // run those checks now.
    exec(&mut tx, "SET CONSTRAINTS ALL IMMEDIATE".into()).await?;

    let mut reader = Reader::new(source)?;
    let mut foreign_keys = prepare(&mut tx, &system_tables, false).await?;
    for t in &system {
        load(&mut tx, &mut reader, t).await?;
    }
    let mut tables = system_tables;

    let mut warnings = Vec::new();
    if level >= TYPE_TABLES_SINCE {
        // The data model is in; the area schemas, type tables (with their checks,
        // foreign keys and indexes) and reporting views follow from it. The
        // backup holds no DDL: the engine builds exactly what it would build for
        // this model, owned by the role that alters them at run time.
        rebuild(&mut tx).await.context("rebuilding the area schemas and type tables failed")?;
        let type_tables: Vec<Table> = app_tables(&mut tx).await?.into_iter().filter(|t| !t.is_system()).collect();
        match_tables(&mut tx, &type_tables, &types, "the data model in the backup").await?;
        foreign_keys.extend(prepare(&mut tx, &type_tables, true).await?);
        for t in &types {
            load(&mut tx, &mut reader, t).await?;
        }
        tables.extend(type_tables);
    }
    // The file is read a second time here; it must still be the one that was checked.
    reader.finish()?;

    for s in &header.sequences {
        let exists: bool = sqlx::query_scalar("SELECT to_regclass(format('%I.%I', $1, $2)) IS NOT NULL")
            .bind(&s.schema)
            .bind(&s.name)
            .fetch_one(&mut *tx)
            .await?;
        if !exists {
            bail!("the backup has sequence {}.{} which the restored schema does not have", s.schema, s.name);
        }
        match s.last_value {
            Some(v) => {
                sqlx::query("SELECT setval(to_regclass(format('%I.%I', $1, $2)), $3, true)")
                    .bind(&s.schema)
                    .bind(&s.name)
                    .bind(v)
                    .execute(&mut *tx)
                    .await?;
            }
            None => exec(&mut tx, format!("ALTER SEQUENCE {} RESTART", Table::new(&s.schema, &s.name).sql())).await?,
        }
    }

    for (table, name, definition) in &foreign_keys {
        exec(&mut tx, format!("ALTER TABLE {table} ADD CONSTRAINT {name} {definition}"))
            .await
            .with_context(|| format!("restored rows break the reference {name} on {table}"))?;
    }
    for t in &tables {
        exec(&mut tx, format!("ALTER TABLE {} ENABLE TRIGGER USER", t.sql())).await?;
    }

    for t in &header.tables {
        let table = Table::new(&t.schema, &t.name);
        let n: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {}", table.sql())))
            .fetch_one(&mut *tx)
            .await?;
        if n as u64 != t.rows {
            bail!("table {} has {n} rows after the restore, the backup has {}", table.display(), t.rows);
        }
    }

    if level >= TYPE_TABLES_SINCE {
        // NOT NULL was lifted for the load; the engine puts it back on every
        // required field that has a value on every asset, as `migrate` does.
        warnings = rebuild(&mut tx).await.context("restoring required fields failed")?;
    }

    // Back to checking deferrable constraints at commit, which is never stricter
    // than their declared mode, so newer migrations run as they would anywhere else.
    exec(&mut tx, "SET CONSTRAINTS ALL DEFERRED".into()).await?;
    let applied = "SELECT count(*) FROM public._sqlx_migrations WHERE success";
    let before: i64 = sqlx::query_scalar(applied).fetch_one(&mut *tx).await?;
    MIGRATOR.run(&mut *tx).await.context("applying newer migrations to the restored data failed")?;
    let after: i64 = sqlx::query_scalar(applied).fetch_one(&mut *tx).await?;
    if after > before {
        // As after `shadoucmdb migrate`: the newer migrations may have built type
        // tables (0009), whose reporting views and grants come from the engine.
        let ctx = crate::api::context::RequestContext::system("restore", "restore");
        let switched = crate::db::act_as_api_role(&mut tx).await?;
        let change = crate::schema::reconcile(&mut tx, &ctx, "Reconcile after restore")
            .await
            .map_err(|e| anyhow::anyhow!("reconciling the data model failed: {}", e.message))?;
        if switched {
            exec(&mut tx, "RESET ROLE".into()).await?;
        }
        if let Some(c) = change {
            warnings.extend(c.impact.0.into_iter().filter(|i| i.kind == "warning").map(|i| i.message));
        }
    }

    let builtin: i64 =
        sqlx::query_scalar("SELECT count(*) FROM permission_profiles WHERE is_builtin").fetch_one(&mut *tx).await?;
    if builtin != 1 {
        bail!("the restored data has no built-in Administrator profile");
    }
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users").fetch_one(&mut *tx).await?;

    if commit {
        tx.commit().await?;
    } else {
        tx.rollback().await?;
    }
    Ok(Report { rows: header.total_rows(), users, migrations_applied_after: (after - before) as usize, warnings })
}

/// Runs the DDL engine unrecorded, as the API role on a three-role install.
/// Returns its warnings.
async fn rebuild(conn: &mut PgConnection) -> anyhow::Result<Vec<String>> {
    let switched = crate::db::act_as_api_role(conn).await?;
    let (_, warnings) = crate::schema::rebuild_unrecorded(conn).await.map_err(|e| anyhow::anyhow!("{}", e.message))?;
    if switched {
        exec(conn, "RESET ROLE".into()).await?;
    }
    Ok(warnings)
}

/// `found` (what the schema has) must be exactly the backup's `expected`
/// tables, with the same columns; the order of the columns may differ (the
/// rows are loaded by column name).
async fn match_tables(
    conn: &mut PgConnection,
    found: &[Table],
    expected: &[&TableEntry],
    source: &str,
) -> anyhow::Result<()> {
    for t in found {
        if !t.is_excluded() && !expected.iter().any(|e| e.schema == t.schema && e.name == t.name) {
            bail!("table {} exists after rebuilding {source} but is not in the backup", t.display());
        }
    }
    for e in expected {
        let table = Table::new(&e.schema, &e.name);
        if !found.contains(&table) {
            bail!("the backup has table {} which {source} does not create", table.display());
        }
        let mut columns = stored_columns(conn, &table).await?;
        let mut backed_up = e.columns.clone();
        columns.sort();
        backed_up.sort();
        if columns != backed_up {
            bail!(
                "columns of {} differ between the backup ({}) and {source} ({})",
                table.display(),
                backed_up.join(", "),
                columns.join(", ")
            );
        }
    }
    Ok(())
}

/// Readies `tables` for loading, like pg_restore does: their foreign keys are
/// dropped (and returned, to be re-created after the data, which re-checks
/// every reference), the application's own triggers are off so rows go in
/// exactly as they were backed up and no audit entries or updated_at stamps are
/// invented, and rows the migrations seeded (built-in profile, default
/// settings) are deleted to make room for the backup's. `lift_not_null`: type
/// tables, where a required field may have no value on assets from before it
/// was required.
async fn prepare(
    conn: &mut PgConnection,
    tables: &[Table],
    lift_not_null: bool,
) -> anyhow::Result<Vec<(String, String, String)>> {
    let names: Vec<String> = tables.iter().map(Table::sql).collect();
    let foreign_keys: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT quote_ident(n.nspname) || '.' || quote_ident(cl.relname), quote_ident(co.conname),
                pg_get_constraintdef(co.oid)
         FROM pg_constraint co JOIN pg_class cl ON cl.oid = co.conrelid JOIN pg_namespace n ON n.oid = cl.relnamespace
         WHERE co.contype = 'f' AND cl.oid = ANY ($1::text[]::regclass[])
         ORDER BY 1, 2",
    )
    .bind(&names)
    .fetch_all(&mut *conn)
    .await?;
    for (table, name, _) in &foreign_keys {
        exec(conn, format!("ALTER TABLE {table} DROP CONSTRAINT {name}")).await?;
    }
    for t in tables {
        exec(conn, format!("ALTER TABLE {} DISABLE TRIGGER USER", t.sql())).await?;
        exec(conn, format!("DELETE FROM {}", t.sql())).await?;
        if lift_not_null {
            let columns: Vec<String> = sqlx::query_scalar(
                "SELECT quote_ident(a.attname) FROM pg_attribute a
                 WHERE a.attrelid = $1::text::regclass AND a.attnum > 0 AND NOT a.attisdropped AND a.attnotnull
                   AND NOT EXISTS (SELECT 1 FROM pg_index i
                                   WHERE i.indrelid = a.attrelid AND i.indisprimary AND a.attnum = ANY (i.indkey))",
            )
            .bind(t.sql())
            .fetch_all(&mut *conn)
            .await?;
            for c in columns {
                exec(conn, format!("ALTER TABLE {} ALTER COLUMN {c} DROP NOT NULL", t.sql())).await?;
            }
        }
    }
    Ok(foreign_keys)
}

/// Reads the section of `t` and inserts its rows, in batches.
async fn load<R: Read>(conn: &mut PgConnection, reader: &mut Reader<R>, t: &TableEntry) -> anyhow::Result<()> {
    let table = Table::new(&t.schema, &t.name);
    reader.section(t)?;
    let insert = format!(
        "INSERT INTO {table} ({cols}) OVERRIDING SYSTEM VALUE SELECT {cols} FROM json_populate_recordset(NULL::{table}, $1::json)",
        table = table.sql(),
        cols = t.columns.iter().map(|c| ident(c)).collect::<Vec<_>>().join(", ")
    );
    let mut batch = String::from("[");
    let mut in_batch = 0usize;
    for i in 0..t.rows {
        let row = reader.row()?;
        if in_batch > 0 {
            batch.push(',');
        }
        batch.push_str(&row);
        in_batch += 1;
        if in_batch == BATCH_ROWS || batch.len() >= BATCH_BYTES || i + 1 == t.rows {
            batch.push(']');
            sqlx::query(sqlx::AssertSqlSafe(insert.clone()))
                .bind(&batch)
                .execute(&mut *conn)
                .await
                .with_context(|| format!("loading rows into {} failed", table.display()))?;
            batch = String::from("[");
            in_batch = 0;
        }
    }
    Ok(())
}

async fn exec(conn: &mut PgConnection, sql: String) -> anyhow::Result<()> {
    sqlx::query(sqlx::AssertSqlSafe(sql.clone()))
        .execute(conn)
        .await
        .with_context(|| format!("could not run: {sql}"))?;
    Ok(())
}
