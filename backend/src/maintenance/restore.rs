//! `shadoucmdb restore`: checks a backup completely, then loads it in a single
//! transaction. The schema is rebuilt by this binary's migrations up to the
//! level the backup was taken at, the rows go in, and any newer migrations run
//! on top, so a backup from an older release restores into a newer one.

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::Args;
use sqlx::Connection;
use sqlx::postgres::PgConnection;

use super::archive::{self, Header, Reader};
use super::{EXCLUDED_TABLES, app_tables, ident, stored_columns};
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
    super::session_settings(conn).await?;
    let mut tx = conn.begin().await?;

    if wipe {
        super::drop_app_objects(&mut tx).await?;
    }
    // Each migration runs in a savepoint of this transaction.
    MIGRATOR.run_to(level, &mut *tx).await.context("rebuilding the schema of the backup failed")?;

    // The schema at that level must have exactly the backup's tables and columns.
    let tables = app_tables(&mut tx).await?;
    let backed_up: Vec<&str> = header.tables.iter().map(|t| t.name.as_str()).collect();
    for t in &tables {
        if !backed_up.contains(&t.as_str()) && !EXCLUDED_TABLES.contains(&t.as_str()) {
            bail!("table {t} exists at migration {level} but is not in the backup");
        }
    }
    for t in &header.tables {
        if !tables.contains(&t.name) {
            bail!("the backup has table {} which migration {level} does not create", t.name);
        }
        let columns = stored_columns(&mut tx, &t.name).await?;
        if columns != t.columns {
            bail!(
                "columns of {} differ between the backup ({}) and migration {level} ({})",
                t.name,
                t.columns.join(", "),
                columns.join(", ")
            );
        }
    }

    // Migrations can leave deferred checks queued (0005 seeds ui_settings under a
    // deferred foreign key), and ALTER TABLE refuses tables with pending events:
    // run those checks now.
    exec(&mut tx, "SET CONSTRAINTS ALL IMMEDIATE".into()).await?;

    // Load like pg_restore does: foreign keys are dropped and re-created after
    // the data (which re-checks every reference), and the application's own
    // triggers are off, so rows go in exactly as they were backed up and no
    // audit entries or updated_at stamps are invented.
    let foreign_keys: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT quote_ident(cl.relname), quote_ident(co.conname), pg_get_constraintdef(co.oid)
         FROM pg_constraint co JOIN pg_class cl ON cl.oid = co.conrelid
         WHERE co.contype = 'f' AND cl.relnamespace = current_schema()::regnamespace
         ORDER BY cl.relname, co.conname",
    )
    .fetch_all(&mut *tx)
    .await?;
    for (table, name, _) in &foreign_keys {
        exec(&mut tx, format!("ALTER TABLE {table} DROP CONSTRAINT {name}")).await?;
    }
    for t in &tables {
        exec(&mut tx, format!("ALTER TABLE {} DISABLE TRIGGER USER", ident(t))).await?;
        // Rows the migrations seeded (built-in profile, default settings) are replaced by the backup's.
        exec(&mut tx, format!("DELETE FROM {}", ident(t))).await?;
    }

    let mut reader = Reader::new(source)?;
    for t in &header.tables {
        reader.section(t)?;
        let insert = format!(
            "INSERT INTO {table} ({cols}) OVERRIDING SYSTEM VALUE SELECT {cols} FROM json_populate_recordset(NULL::{table}, $1::json)",
            table = ident(&t.name),
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
                    .execute(&mut *tx)
                    .await
                    .with_context(|| format!("loading rows into {} failed", t.name))?;
                batch = String::from("[");
                in_batch = 0;
            }
        }
    }
    // The file is read a second time here; it must still be the one that was checked.
    reader.finish()?;

    for s in &header.sequences {
        let exists: bool = sqlx::query_scalar("SELECT to_regclass(quote_ident($1)) IS NOT NULL")
            .bind(&s.name)
            .fetch_one(&mut *tx)
            .await?;
        if !exists {
            bail!("the backup has sequence {} which migration {level} does not create", s.name);
        }
        match s.last_value {
            Some(v) => {
                sqlx::query("SELECT setval(to_regclass(quote_ident($1)), $2, true)")
                    .bind(&s.name)
                    .bind(v)
                    .execute(&mut *tx)
                    .await?;
            }
            None => exec(&mut tx, format!("ALTER SEQUENCE {} RESTART", ident(&s.name))).await?,
        }
    }

    for (table, name, definition) in &foreign_keys {
        exec(&mut tx, format!("ALTER TABLE {table} ADD CONSTRAINT {name} {definition}"))
            .await
            .with_context(|| format!("restored rows break the reference {name} on {table}"))?;
    }
    for t in &tables {
        exec(&mut tx, format!("ALTER TABLE {} ENABLE TRIGGER USER", ident(t))).await?;
    }

    for t in &header.tables {
        let n: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {}", ident(&t.name))))
            .fetch_one(&mut *tx)
            .await?;
        if n as u64 != t.rows {
            bail!("table {} has {n} rows after the restore, the backup has {}", t.name, t.rows);
        }
    }

    // Back to checking deferrable constraints at commit, which is never stricter
    // than their declared mode, so newer migrations run as they would anywhere else.
    exec(&mut tx, "SET CONSTRAINTS ALL DEFERRED".into()).await?;
    let applied = "SELECT count(*) FROM _sqlx_migrations WHERE success";
    let before: i64 = sqlx::query_scalar(applied).fetch_one(&mut *tx).await?;
    MIGRATOR.run(&mut *tx).await.context("applying newer migrations to the restored data failed")?;
    let after: i64 = sqlx::query_scalar(applied).fetch_one(&mut *tx).await?;

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
    Ok(Report { rows: header.total_rows(), users, migrations_applied_after: (after - before) as usize })
}

async fn exec(conn: &mut PgConnection, sql: String) -> anyhow::Result<()> {
    sqlx::query(sqlx::AssertSqlSafe(sql.clone()))
        .execute(conn)
        .await
        .with_context(|| format!("could not run: {sql}"))?;
    Ok(())
}
