//! The backup file: gzip-compressed JSON Lines, readable with `zcat` and
//! independent of the PostgreSQL version and of pg_dump.
//!
//! ```text
//! {"format":"shadoucmdb-backup","format_version":2,...,"tables":[{"schema":"cmdb","name":"owners","columns":[...],"rows":3},...]}
//! {"table":"cmdb.owners","rows":3}     one section line per table, in header order
//! {"id":"...","name":"...",...}        exactly `rows` row lines (row_to_json of the table's columns)
//! ...
//! {"end":{"rows":1234,"sha256":"..."}} SHA-256 of every uncompressed byte above this line
//! ```
//!
//! Tables are named with their schema: the system tables (`cmdb`) come first,
//! then the table of every type in its area's schema (`infrastruktur.server`).
//!
//! Lines are interpreted by position (the header says how many rows follow
//! each section line), so a column that happens to be called `table` or `end`
//! cannot be mistaken for a control line. Row lines are passed to PostgreSQL
//! as raw text and never parsed into Rust numbers, so numeric values keep
//! their full precision.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;

use anyhow::{Context, bail};
use chrono::{DateTime, Utc};
use flate2::Compression;
use flate2::read::MultiGzDecoder;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::Table;

pub const FORMAT: &str = "shadoucmdb-backup";
/// 2: tables and sequences carry their schema, and the tables of the types
/// (area schemas) are included. Format 1 files lack those tables.
pub const FORMAT_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Header {
    pub format: String,
    pub format_version: u32,
    pub created_at: DateTime<Utc>,
    /// Version of the shadoucmdb binary that wrote the file.
    pub app_version: String,
    pub server_version: String,
    pub database: String,
    /// Migrations applied to the source database: the schema the rows fit.
    pub migrations: Vec<MigrationEntry>,
    pub tables: Vec<TableEntry>,
    pub sequences: Vec<SequenceEntry>,
    /// Tables deliberately left out (their rows are not worth restoring, e.g. sessions).
    pub excluded_tables: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationEntry {
    pub version: i64,
    pub description: String,
    /// Hex SHA-384 of the migration SQL, as sqlx records it.
    pub checksum: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableEntry {
    pub schema: String,
    pub name: String,
    /// Stored columns in table order (generated columns are recomputed on restore).
    pub columns: Vec<String>,
    pub rows: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequenceEntry {
    pub schema: String,
    pub name: String,
    /// `None`: the sequence was never used.
    pub last_value: Option<i64>,
}

impl TableEntry {
    pub fn table(&self) -> Table {
        Table { schema: self.schema.clone(), name: self.name.clone() }
    }
}

impl Header {
    pub fn total_rows(&self) -> u64 {
        self.tables.iter().map(|t| t.rows).sum()
    }

    /// Highest migration version the rows were written under.
    pub fn migration_level(&self) -> Option<i64> {
        self.migrations.iter().map(|m| m.version).max()
    }
}

#[derive(Serialize, Deserialize)]
struct Section {
    table: String,
    rows: u64,
}

#[derive(Serialize, Deserialize)]
struct Footer {
    end: FooterBody,
}

#[derive(Serialize, Deserialize)]
struct FooterBody {
    rows: u64,
    sha256: String,
}

pub struct Writer<W: Write> {
    out: GzEncoder<W>,
    hash: Sha256,
    rows: u64,
}

impl<W: Write> Writer<W> {
    pub fn new(out: W, header: &Header) -> anyhow::Result<Self> {
        let mut w = Writer { out: GzEncoder::new(out, Compression::default()), hash: Sha256::new(), rows: 0 };
        w.line(&serde_json::to_string(header)?)?;
        Ok(w)
    }

    fn line(&mut self, s: &str) -> std::io::Result<()> {
        self.hash.update(s.as_bytes());
        self.hash.update(b"\n");
        self.out.write_all(s.as_bytes())?;
        self.out.write_all(b"\n")
    }

    pub fn section(&mut self, table: &TableEntry) -> anyhow::Result<()> {
        let s = serde_json::to_string(&Section { table: table.table().to_string(), rows: table.rows })?;
        Ok(self.line(&s)?)
    }

    /// One row as PostgreSQL's JSON text. A raw line break in valid JSON can
    /// only be whitespace (inside strings it is escaped), so folding it into a
    /// space keeps one row per line without changing the value.
    pub fn row(&mut self, json: &str) -> anyhow::Result<()> {
        self.rows += 1;
        if json.contains(['\n', '\r']) {
            return Ok(self.line(&json.replace(['\n', '\r'], " "))?);
        }
        Ok(self.line(json)?)
    }

    pub fn finish(mut self) -> anyhow::Result<W> {
        let sha256 = hex::encode(self.hash.clone().finalize());
        let footer = serde_json::to_string(&Footer { end: FooterBody { rows: self.rows, sha256 } })?;
        self.out.write_all(footer.as_bytes())?;
        self.out.write_all(b"\n")?;
        Ok(self.out.finish()?)
    }
}

/// Reads a backup front to back, checking its structure as it goes. The
/// SHA-256 and the row totals are only confirmed by [`Reader::finish`].
pub struct Reader<R: Read> {
    lines: BufReader<MultiGzDecoder<BufReader<R>>>,
    hash: Sha256,
    line_no: u64,
    rows: u64,
    header: Header,
}

impl<R: Read> Reader<R> {
    pub fn new(source: R) -> anyhow::Result<Self> {
        let mut source = BufReader::new(source);
        let start = source.fill_buf().context("cannot read the backup")?;
        if start.is_empty() {
            bail!("not a ShadouCMDB backup (the file is empty)");
        }
        if !start.starts_with(&[0x1f, 0x8b]) {
            bail!("not a ShadouCMDB backup (not gzip-compressed)");
        }
        let decoder = MultiGzDecoder::new(source);
        let mut r = Reader {
            lines: BufReader::new(decoder),
            hash: Sha256::new(),
            line_no: 0,
            rows: 0,
            header: Header {
                format: String::new(),
                format_version: 0,
                created_at: DateTime::<Utc>::MIN_UTC,
                app_version: String::new(),
                server_version: String::new(),
                database: String::new(),
                migrations: vec![],
                tables: vec![],
                sequences: vec![],
                excluded_tables: vec![],
            },
        };
        let first = r.read_line(true)?;
        #[derive(Deserialize)]
        struct Probe {
            format: Option<String>,
            format_version: Option<u32>,
        }
        let probe: Probe = serde_json::from_str(&first).context("not a ShadouCMDB backup (bad header)")?;
        if probe.format.as_deref() != Some(FORMAT) {
            bail!("not a ShadouCMDB backup (format is not \"{FORMAT}\")");
        }
        match probe.format_version {
            Some(FORMAT_VERSION) => {}
            Some(1) => bail!(
                "backup format 1 was written by a ShadouCMDB build that left out the tables of the types (the \
                 attribute values); it cannot be restored completely. Take a new backup with this release"
            ),
            Some(v) if v > FORMAT_VERSION => {
                bail!("backup format {v} was written by a newer ShadouCMDB; this binary reads format {FORMAT_VERSION}")
            }
            other => bail!("unsupported backup format version {other:?}"),
        }
        r.header = serde_json::from_str(&first).context("backup header is malformed")?;
        Ok(r)
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    fn read_line(&mut self, hashed: bool) -> anyhow::Result<String> {
        let mut buf = String::new();
        let n = self
            .lines
            .read_line(&mut buf)
            .with_context(|| format!("backup is damaged: cannot read line {}", self.line_no + 1))?;
        if n == 0 || !buf.ends_with('\n') {
            bail!("backup is truncated after line {}", self.line_no);
        }
        self.line_no += 1;
        if hashed {
            self.hash.update(buf.as_bytes());
        }
        buf.pop();
        Ok(buf)
    }

    /// Reads the section line that must introduce `table`.
    pub fn section(&mut self, table: &TableEntry) -> anyhow::Result<()> {
        let line = self.read_line(true)?;
        let s: Section = serde_json::from_str(&line)
            .with_context(|| format!("backup is damaged: line {} is not a table section", self.line_no))?;
        let name = table.table().to_string();
        if s.table != name || s.rows != table.rows {
            bail!(
                "backup is damaged: line {} starts table \"{}\" ({} rows), the header expects \"{}\" ({} rows)",
                self.line_no,
                s.table,
                s.rows,
                name,
                table.rows
            );
        }
        Ok(())
    }

    /// Next row line, checked to be a JSON object.
    pub fn row(&mut self) -> anyhow::Result<String> {
        let line = self.read_line(true)?;
        if !line.starts_with('{') || serde_json::from_str::<serde::de::IgnoredAny>(&line).is_err() {
            bail!("backup is damaged: line {} is not a JSON row", self.line_no);
        }
        self.rows += 1;
        Ok(line)
    }

    /// Checks the footer against everything read, and that nothing follows it.
    pub fn finish(mut self) -> anyhow::Result<Header> {
        let line = self.read_line(false)?;
        let footer: Footer = serde_json::from_str(&line)
            .with_context(|| format!("backup is damaged: line {} is not the end marker", self.line_no))?;
        let sha256 = hex::encode(self.hash.clone().finalize());
        if footer.end.sha256 != sha256 {
            bail!("backup is damaged: SHA-256 mismatch (recorded {}, computed {sha256})", footer.end.sha256);
        }
        if footer.end.rows != self.rows || self.rows != self.header.total_rows() {
            bail!(
                "backup is damaged: {} rows read, the end marker records {} and the header {}",
                self.rows,
                footer.end.rows,
                self.header.total_rows()
            );
        }
        let mut rest = Vec::new();
        self.lines.read_to_end(&mut rest).context("backup is damaged after the end marker")?;
        if !rest.is_empty() {
            bail!("backup is damaged: {} unexpected bytes after the end marker", rest.len());
        }
        Ok(self.header)
    }
}

/// Reads a whole backup and checks every consistency rule without touching a database.
pub fn verify<R: Read>(source: R) -> anyhow::Result<Header> {
    let mut r = Reader::new(source)?;
    let tables = r.header().tables.clone();
    for t in &tables {
        r.section(t)?;
        for _ in 0..t.rows {
            r.row()?;
        }
    }
    r.finish()
}

pub fn verify_file(path: &Path) -> anyhow::Result<Header> {
    let file = std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    verify(file).with_context(|| format!("{} failed the consistency check", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> Header {
        Header {
            format: FORMAT.into(),
            format_version: FORMAT_VERSION,
            created_at: Utc::now(),
            app_version: "test".into(),
            server_version: "18".into(),
            database: "db".into(),
            migrations: vec![MigrationEntry { version: 1, description: "one".into(), checksum: "00".into() }],
            tables: vec![
                TableEntry { schema: "cmdb".into(), name: "a".into(), columns: vec!["id".into()], rows: 2 },
                TableEntry { schema: "infrastruktur".into(), name: "b".into(), columns: vec!["table".into()], rows: 1 },
            ],
            sequences: vec![],
            excluded_tables: vec!["sessions".into()],
        }
    }

    fn sample() -> Vec<u8> {
        let h = header();
        let mut w = Writer::new(Vec::new(), &h).unwrap();
        w.section(&h.tables[0]).unwrap();
        w.row(r#"{"id":1}"#).unwrap();
        w.row("{\"id\":\n2}").unwrap();
        w.section(&h.tables[1]).unwrap();
        // A row whose only column is called "table" is still a row.
        w.row(r#"{"table":"a","rows":2}"#).unwrap();
        w.finish().unwrap()
    }

    fn gunzip(bytes: &[u8]) -> String {
        let mut s = String::new();
        MultiGzDecoder::new(bytes).read_to_string(&mut s).unwrap();
        s
    }

    fn gzip(s: &str) -> Vec<u8> {
        let mut e = GzEncoder::new(Vec::new(), Compression::default());
        e.write_all(s.as_bytes()).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn a_written_backup_verifies_and_keeps_one_row_per_line() {
        let bytes = sample();
        let h = verify(bytes.as_slice()).unwrap();
        assert_eq!(h.total_rows(), 3);
        assert_eq!(gunzip(&bytes).lines().count(), 1 + 2 + 3 + 1);
    }

    #[test]
    fn a_changed_row_fails_the_checksum() {
        let text = gunzip(&sample()).replace(r#"{"id":1}"#, r#"{"id":7}"#);
        let err = verify(gzip(&text).as_slice()).unwrap_err().to_string();
        assert!(err.contains("SHA-256 mismatch"), "{err}");
    }

    #[test]
    fn a_missing_row_or_a_cut_file_is_reported() {
        let text = gunzip(&sample());
        let without_row: String = text.lines().filter(|l| *l != r#"{"id":1}"#).map(|l| format!("{l}\n")).collect();
        let err = verify(gzip(&without_row).as_slice()).unwrap_err().to_string();
        assert!(err.contains("starts table"), "{err}");

        let cut = &text[..text.len() - 20];
        let err = verify(gzip(cut).as_slice()).unwrap_err().to_string();
        assert!(err.contains("truncated") || err.contains("end marker"), "{err}");
    }

    #[test]
    fn trailing_data_and_foreign_files_are_rejected() {
        let text = gunzip(&sample()) + "{}\n";
        assert!(verify(gzip(&text).as_slice()).unwrap_err().to_string().contains("after the end marker"));
        assert!(verify(&b"PGDMP"[..]).unwrap_err().to_string().contains("not gzip"));
        assert!(verify(gzip("{\"format\":\"other\"}\n").as_slice()).unwrap_err().to_string().contains("format"));
        let newer = gzip(&format!("{{\"format\":\"{FORMAT}\",\"format_version\":99}}\n"));
        assert!(verify(newer.as_slice()).unwrap_err().to_string().contains("newer ShadouCMDB"));
        let v1 = gzip(&format!("{{\"format\":\"{FORMAT}\",\"format_version\":1}}\n"));
        assert!(verify(v1.as_slice()).unwrap_err().to_string().contains("new backup"));
    }
}
