//! `shadoucmdb backup`: every system table (schema `cmdb`) and the type table
//! of every area, read in one REPEATABLE READ snapshot, so the file is
//! consistent even while the server keeps writing. Needs no pg_dump and no
//! superuser: the application role reads its own tables. The structure of the
//! type tables is not stored: it follows from the data model (areas, types and
//! fields) in the system tables, and restore rebuilds it from there.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use clap::Args;
use futures_util::TryStreamExt;
use sqlx::Connection;
use sqlx::postgres::PgConnection;

use super::archive::{
    self, EncryptionKeyEntry, FORMAT, FORMAT_VERSION, Header, MigrationEntry, SequenceEntry, TableEntry,
};
use super::{Table, app_schemas, app_tables, ident, stored_columns};
use crate::config::DatabaseConfig;

#[derive(Debug, Args)]
pub struct BackupArgs {
    /// Write the backup to this file (default: shadoucmdb-backup-<UTC time>.jsonl.gz
    /// in the current directory). An existing file is never overwritten.
    #[arg(long, value_name = "PATH")]
    pub out: Option<PathBuf>,
}

pub async fn run(cfg: &DatabaseConfig, args: BackupArgs) -> anyhow::Result<()> {
    let path = args.out.unwrap_or_else(|| {
        PathBuf::from(format!("shadoucmdb-backup-{}.jsonl.gz", chrono::Utc::now().format("%Y%m%dT%H%M%SZ")))
    });
    if path.exists() {
        bail!("{} already exists; choose another --out", path.display());
    }
    let partial = PathBuf::from(format!("{}.partial", path.display()));

    let mut conn = super::connect(cfg).await?;
    let (_, place) = super::describe(&mut conn).await?;
    println!("Backing up {place}");

    let file = create_private(&partial)?;
    let written = async {
        let mut out = std::io::BufWriter::new(file);
        let header = write(&mut conn, &mut out).await?;
        let file = out.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()?;
        drop(file);
        // Read the file back before calling it a backup.
        let checked = archive::verify_file(&partial)?;
        anyhow::ensure!(checked.total_rows() == header.total_rows(), "backup changed while it was verified");
        std::fs::rename(&partial, &path)
            .with_context(|| format!("cannot rename {} to {}", partial.display(), path.display()))?;
        anyhow::Ok(header)
    }
    .await;
    conn.close().await.ok();
    let header = match written {
        Ok(h) => h,
        Err(e) => {
            std::fs::remove_file(&partial).ok();
            return Err(e);
        }
    };

    println!(
        "Wrote {} rows from {} tables at migration {} ({} bytes)",
        header.total_rows(),
        header.tables.len(),
        header.migration_level().unwrap_or_default(),
        std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
    );
    println!("Verified: SHA-256 and row counts match");
    println!("Backup: {}", path.display());
    println!(
        "The file contains password hashes and personal data (audit log): store it encrypted and access-controlled."
    );
    let keys: Vec<String> = header
        .encryption_keys
        .iter()
        .map(|k| k.key_id.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    if !keys.is_empty() {
        println!(
            "Authenticator secrets in it are encrypted with key {}, which is not in the file: keep that key (and its \
             copy in escrow) as long as you keep this backup.",
            keys.join(", ")
        );
    }
    Ok(())
}

/// Creates the file only if it does not exist, readable by its owner only.
fn create_private(path: &Path) -> anyhow::Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path).with_context(|| format!("cannot create {}", path.display()))
}

/// Writes the backup of the connected database to `out`.
pub async fn write<W: Write>(conn: &mut PgConnection, out: W) -> anyhow::Result<Header> {
    super::session_settings(conn).await?;
    let mut tx = conn.begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ, READ ONLY").await?;

    let exists: bool =
        sqlx::query_scalar("SELECT to_regclass('public._sqlx_migrations') IS NOT NULL").fetch_one(&mut *tx).await?;
    if !exists {
        bail!("this database has no ShadouCMDB schema (no migrations applied); nothing to back up");
    }
    let migrations: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT version, description, encode(checksum, 'hex') FROM public._sqlx_migrations WHERE success ORDER BY version",
    )
    .fetch_all(&mut *tx)
    .await?;
    let (database, server_version): (String, String) =
        sqlx::query_as("SELECT current_database(), current_setting('server_version')").fetch_one(&mut *tx).await?;

    let mut tables = Vec::new();
    let mut excluded = Vec::new();
    // System tables first: restore needs the data model before it can rebuild the type tables.
    for table in app_tables(&mut tx).await? {
        if table.is_excluded() {
            excluded.push(table.display());
            continue;
        }
        let columns = stored_columns(&mut tx, &table).await?;
        let rows: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {}", table.sql())))
            .fetch_one(&mut *tx)
            .await?;
        tables.push(TableEntry { schema: table.schema, name: table.name, columns, rows: rows as u64 });
    }
    let schemas = app_schemas(&mut tx).await?;
    let sequences: Vec<(String, String, Option<i64>)> = sqlx::query_as(
        "SELECT schemaname::text, sequencename::text, last_value FROM pg_sequences
         WHERE schemaname = ANY ($1) ORDER BY 1, 2",
    )
    .bind(&schemas)
    .fetch_all(&mut *tx)
    .await?;
    let encryption_keys = crate::secrets::sealed::key_counts(&mut tx)
        .await?
        .into_iter()
        .map(|c| EncryptionKeyEntry { key_id: c.key_id.to_string(), table: c.table.name().into(), rows: c.rows as u64 })
        .collect();

    let header = Header {
        format: FORMAT.into(),
        format_version: FORMAT_VERSION,
        created_at: chrono::Utc::now(),
        app_version: env!("CARGO_PKG_VERSION").into(),
        server_version,
        database,
        migrations: migrations
            .into_iter()
            .map(|(version, description, checksum)| MigrationEntry { version, description, checksum })
            .collect(),
        tables,
        sequences: sequences
            .into_iter()
            .map(|(schema, name, last_value)| SequenceEntry { schema, name, last_value })
            .collect(),
        excluded_tables: excluded,
        encryption_keys,
    };

    let mut w = archive::Writer::new(out, &header)?;
    for t in &header.tables {
        w.section(t)?;
        let cols = t.columns.iter().map(|c| ident(c)).collect::<Vec<_>>().join(", ");
        let table = Table::new(&t.schema, &t.name);
        let order = primary_key(&mut tx, &table).await?;
        let order = if order.is_empty() {
            String::new()
        } else {
            format!(" ORDER BY {}", order.iter().map(|c| ident(c)).collect::<Vec<_>>().join(", "))
        };
        let sql = format!("SELECT row_to_json(x)::text FROM (SELECT {cols} FROM {}{order}) x", table.sql());
        let mut written = 0u64;
        let mut rows = sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(sql)).fetch(&mut *tx);
        while let Some(row) = rows.try_next().await? {
            w.row(&row)?;
            written += 1;
        }
        // Same snapshot, so this cannot differ; checked because the file depends on it.
        anyhow::ensure!(written == t.rows, "table {} yielded {written} rows, counted {}", table.display(), t.rows);
    }
    w.finish()?.flush()?;
    tx.commit().await?;
    Ok(header)
}

async fn primary_key(conn: &mut PgConnection, table: &Table) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT a.attname::text FROM pg_index i
         CROSS JOIN LATERAL unnest(i.indkey) WITH ORDINALITY k(attnum, ord)
         JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.attnum
         WHERE i.indrelid = to_regclass(format('%I.%I', $1, $2)) AND i.indisprimary
         ORDER BY k.ord",
    )
    .bind(&table.schema)
    .bind(&table.name)
    .fetch_all(conn)
    .await
}
