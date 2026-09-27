//! `shadoucmdb restore`: checks a backup completely, then loads it in a single
//! transaction. The schema is rebuilt by this binary's migrations up to the
//! level the backup was taken at, the rows go in (system tables first, then
//! the tables of the types, built from the restored data model), and any newer
//! migrations run on top, so a backup from an older release restores into a
//! newer one.

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::Args;
use sqlx::Connection;
use sqlx::postgres::PgConnection;

use super::archive::{self, Header, Reader, TableEntry};
use super::{EXCLUDED_TABLES, TYPE_TABLES_MIGRATION, Table, app_tables, ident, stored_columns};
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
///
/// The system tables are rebuilt by the migrations up to the backup's level
/// and loaded first; the tables of the types are then built from the restored
/// data model by the DDL engine, checked against the backup and loaded.
pub async fn restore<R: Read>(
    conn: &mut PgConnection,
    source: R,
    header: &Header,
    wipe: bool,
    commit: bool,
) -> anyhow::Result<Report> {
    check_compatible(header)?;
    let level = header.migration_level().context("backup has no migrations")?;
    // The writer puts every system table before the first type table; loading relies on it.
    let split = header.tables.iter().position(|t| t.table().is_area()).unwrap_or(header.tables.len());
    let (system, types) = header.tables.split_at(split);
    if types.iter().any(|t| !t.table().is_area()) {
        bail!("backup is damaged: the system tables must come before the tables of the types");
    }
    if level < TYPE_TABLES_MIGRATION && !types.is_empty() {
        bail!("the backup has tables of types, which migration {level} does not have");
    }
    super::session_settings(conn).await?;
    let mut tx = conn.begin().await?;

    if wipe {
        super::drop_app_objects(&mut tx).await?;
    }
    // Each migration runs in a savepoint of this transaction.
    MIGRATOR.run_to(level, &mut *tx).await.context("rebuilding the schema of the backup failed")?;

    // The schema at that level must have exactly the backup's tables and columns.
    let tables: Vec<Table> = app_tables(&mut tx).await?.into_iter().filter(|t| !t.is_area()).collect();
    same_tables(&mut tx, &tables, system, &format!("migration {level}")).await?;

    // Migrations can leave deferred checks queued (0005 seeds ui_settings under a
    // deferred foreign key), and ALTER TABLE refuses tables with pending events:
    // run those checks now.
    exec(&mut tx, "SET CONSTRAINTS ALL IMMEDIATE".into()).await?;

    // Load like pg_restore does: foreign keys are dropped and re-created after
    // the data (which re-checks every reference), and the application's own
    // triggers are off, so rows go in exactly as they were backed up and no
    // audit entries or updated_at stamps are invented.
    let mut foreign_keys = drop_foreign_keys(&mut tx, &tables).await?;
    for t in &tables {
        exec(&mut tx, format!("ALTER TABLE {} DISABLE TRIGGER USER", t.sql())).await?;
        // Rows the migrations seeded (built-in profile, default settings) are replaced by the backup's.
        exec(&mut tx, format!("DELETE FROM {}", t.sql())).await?;
    }
    let mut reader = Reader::new(source)?;
    load(&mut tx, &mut reader, system).await?;
    let mut all_tables = tables;

    if level >= TYPE_TABLES_MIGRATION {
        // The areas, types and fields are restored: build their schemas and tables.
        build_type_tables(&mut tx).await.context("building the tables of the types failed")?;
        let tables: Vec<Table> = app_tables(&mut tx).await?.into_iter().filter(Table::is_area).collect();
        same_tables(&mut tx, &tables, types, "the restored data model").await?;
        foreign_keys.extend(drop_foreign_keys(&mut tx, &tables).await?);
        for t in &tables {
            exec(&mut tx, format!("ALTER TABLE {} DISABLE TRIGGER USER", t.sql())).await?;
            // A required field may still lack values on older assets (the engine
            // then left it nullable); the engine sets NOT NULL again after the load
            // wherever every row has a value.
            let not_null: Vec<String> = sqlx::query_scalar(
                "SELECT a.attname::text FROM pg_attribute a
                 WHERE a.attrelid = to_regclass(format('%I.%I', $1::text, $2::text))
                   AND a.attnum > 0 AND NOT a.attisdropped AND a.attnotnull
                   AND NOT EXISTS (SELECT 1 FROM pg_index i
                                   WHERE i.indrelid = a.attrelid AND i.indisprimary AND a.attnum = ANY (i.indkey))",
            )
            .bind(&t.schema)
            .bind(&t.name)
            .fetch_all(&mut *tx)
            .await?;
            for c in not_null {
                exec(&mut tx, format!("ALTER TABLE {} ALTER COLUMN {} DROP NOT NULL", t.sql(), ident(&c))).await?;
            }
        }
        load(&mut tx, &mut reader, types).await?;
        all_tables.extend(tables);
    }
    // The file is read a second time here; it must still be the one that was checked.
    reader.finish()?;

    for s in &header.sequences {
        let exists: bool = sqlx::query_scalar("SELECT to_regclass(format('%I.%I', $1::text, $2::text)) IS NOT NULL")
            .bind(&s.schema)
            .bind(&s.name)
            .fetch_one(&mut *tx)
            .await?;
        if !exists {
            bail!("the backup has sequence {}.{} which the restored schema does not have", s.schema, s.name);
        }
        match s.last_value {
            Some(v) => {
                sqlx::query("SELECT setval(to_regclass(format('%I.%I', $1::text, $2::text)), $3, true)")
                    .bind(&s.schema)
                    .bind(&s.name)
                    .bind(v)
                    .execute(&mut *tx)
                    .await?;
            }
            None => exec(&mut tx, format!("ALTER SEQUENCE {}.{} RESTART", ident(&s.schema), ident(&s.name))).await?,
        }
    }

    for (table, name, definition) in &foreign_keys {
        exec(&mut tx, format!("ALTER TABLE {} ADD CONSTRAINT {} {definition}", table.sql(), ident(name)))
            .await
            .with_context(|| format!("restored rows break the reference {name} on {table}"))?;
    }
    for t in &all_tables {
        exec(&mut tx, format!("ALTER TABLE {} ENABLE TRIGGER USER", t.sql())).await?;
    }

    for t in &header.tables {
        let n: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {}", t.table().sql())))
            .fetch_one(&mut *tx)
            .await?;
        if n as u64 != t.rows {
            bail!("table {} has {n} rows after the restore, the backup has {}", t.table(), t.rows);
        }
    }

    // Back to checking deferrable constraints at commit, which is never stricter
    // than their declared mode, so newer migrations run as they would anywhere else.
    exec(&mut tx, "SET CONSTRAINTS ALL DEFERRED".into()).await?;
    let applied = "SELECT count(*) FROM public._sqlx_migrations WHERE success";
    let before: i64 = sqlx::query_scalar(applied).fetch_one(&mut *tx).await?;
    MIGRATOR.run(&mut *tx).await.context("applying newer migrations to the restored data failed")?;
    let after: i64 = sqlx::query_scalar(applied).fetch_one(&mut *tx).await?;
    // As `shadoucmdb migrate` does afterwards: NOT NULL where every asset has a
    // value again, reporting views, and anything newer migrations expect.
    build_type_tables(&mut tx).await.context("reconciling the data model after the restore failed")?;

    let builtin: i64 = sqlx::query_scalar("SELECT count(*) FROM cmdb.permission_profiles WHERE is_builtin")
        .fetch_one(&mut *tx)
        .await?;
    if builtin != 1 {
        bail!("the restored data has no built-in Administrator profile");
    }
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM cmdb.users").fetch_one(&mut *tx).await?;

    if commit {
        tx.commit().await?;
    } else {
        tx.rollback().await?;
    }
    Ok(Report { rows: header.total_rows(), users, migrations_applied_after: (after - before) as usize })
}

/// `tables` (what the database has) must be exactly the backup's `entries`
/// (plus the excluded ones), with the same columns. `made_by` names what built them.
async fn same_tables(
    conn: &mut PgConnection,
    tables: &[Table],
    entries: &[TableEntry],
    made_by: &str,
) -> anyhow::Result<()> {
    for t in tables {
        let excluded = !t.is_area() && EXCLUDED_TABLES.contains(&t.name.as_str());
        if !excluded && !entries.iter().any(|e| e.table() == *t) {
            bail!("table {t} exists after {made_by} but is not in the backup");
        }
    }
    for e in entries {
        let t = e.table();
        if !tables.contains(&t) {
            bail!("the backup has table {t} which {made_by} does not create");
        }
        let mut columns = stored_columns(&mut *conn, &t).await?;
        if t.is_area() {
            columns.sort();
        }
        if columns != e.columns {
            bail!(
                "columns of {t} differ between the backup ({}) and {made_by} ({})",
                e.columns.join(", "),
                columns.join(", ")
            );
        }
    }
    Ok(())
}

/// Drops the foreign keys of `tables` and returns them for re-creation.
async fn drop_foreign_keys(conn: &mut PgConnection, tables: &[Table]) -> anyhow::Result<Vec<(Table, String, String)>> {
    let schemas: Vec<&str> = tables.iter().map(|t| t.schema.as_str()).collect();
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT n.nspname::text, cl.relname::text, co.conname::text, pg_get_constraintdef(co.oid)
         FROM pg_constraint co JOIN pg_class cl ON cl.oid = co.conrelid JOIN pg_namespace n ON n.oid = cl.relnamespace
         WHERE co.contype = 'f' AND n.nspname = ANY($1)
         ORDER BY 1, 2, 3",
    )
    .bind(&schemas)
    .fetch_all(&mut *conn)
    .await?;
    let mut out = Vec::new();
    for (schema, table, name, definition) in rows {
        let table = Table { schema, name: table };
        if !tables.contains(&table) {
            continue;
        }
        exec(conn, format!("ALTER TABLE {} DROP CONSTRAINT {}", table.sql(), ident(&name))).await?;
        out.push((table, name, definition));
    }
    Ok(out)
}

/// Inserts the rows of `entries`, which are next in the file.
async fn load<R: Read>(conn: &mut PgConnection, reader: &mut Reader<R>, entries: &[TableEntry]) -> anyhow::Result<()> {
    for t in entries {
        reader.section(t)?;
        let table = t.table();
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
                    .with_context(|| format!("loading rows into {table} failed"))?;
                batch = String::from("[");
                in_batch = 0;
            }
        }
    }
    Ok(())
}

/// The area schemas and type tables the restored data model describes, built
/// by the DDL engine as the API role would (three-role install), without a
/// schema change entry: the backup's history already records them.
async fn build_type_tables(conn: &mut PgConnection) -> anyhow::Result<()> {
    let switched = crate::db::act_as_api_role(&mut *conn).await?;
    crate::schema::rebuild_unrecorded(&mut *conn).await.map_err(|e| anyhow::anyhow!("{}", e.message))?;
    if switched {
        exec(conn, "RESET ROLE".into()).await?;
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
