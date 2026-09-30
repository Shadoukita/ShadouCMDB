//! `GET /imports/{id}/error-report`: the stored problems next to the rows
//! they are about, as a CSV the user can fix and upload again (§3.4, §5.1).
//!
//! - Columns `Row`, `Severity`, `Column`, `Problem`, `Code`, then every
//!   original column of the row under its original header. One line per
//!   problem, in row order.
//! - Every field goes through [`csv_safe`]: quoted and neutralised.
//! - The rows come from the stored file, the messages from the stored
//!   problems; nothing else is read from the CMDB.
//! - Streamed: the file is read on a blocking thread and the reading stops
//!   once the last row with a problem was written.
//! - A download by anyone but the job's owner is audited as
//!   `import.report_read` (W7).

use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::path::Path;

use axum::body::Bytes;
use serde_json::json;
use sqlx::PgPool;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::analyse::{self, FileFormat, FileOptions, REPORT_HEADERS};
use super::jobs::{JobRow, check_owner, ctx_user, fetch, require_enabled};
use super::parse::{Limits, Row};
use super::schemas::JobStatus;
use super::storage::DbFile;
use super::{MAX_COLUMNS, csv_safe};
use crate::api::context::RequestContext;
use crate::api::route::CsvDownload;
use crate::config::ImportConfig;
use crate::data::crud::{self, AuditAction, AuditEntry};
use crate::http::error::AppError;

/// Bytes collected before a piece of the report is sent.
const FLUSH_BYTES: usize = 64 * 1024;

/// One stored problem, as the report shows it.
struct Line {
    column: Option<usize>,
    severity: String,
    code: String,
    message: String,
}

/// How the report is laid out: the delimiter, the original headers, and how
/// many leading columns of the file to leave out.
struct Layout {
    delimiter: char,
    headers: Vec<String>,
    /// The file is itself an error report: its first five columns are the old
    /// report's, not data, so they are not repeated.
    skip: usize,
}

impl Layout {
    fn of(job: &JobRow) -> Self {
        let info = job.info();
        let headers: Vec<String> =
            info.as_ref().map(|i| i.columns.iter().map(|c| c.header.clone()).collect()).unwrap_or_default();
        let delimiter = match (job.format(), info.as_ref().and_then(|i| i.delimiter.as_deref())) {
            (FileFormat::Csv, Some(d)) if d.chars().count() == 1 => d.chars().next().unwrap_or(','),
            _ => ',',
        };
        Layout::new(delimiter, headers)
    }

    fn new(delimiter: char, headers: Vec<String>) -> Self {
        let is_report =
            headers.len() >= REPORT_HEADERS.len() && REPORT_HEADERS.iter().zip(&headers).all(|(a, b)| a == b);
        let skip = if is_report { REPORT_HEADERS.len() } else { 0 };
        Layout { delimiter, headers, skip }
    }

    fn header_record(&self, out: &mut String) {
        let fields = REPORT_HEADERS.iter().copied().chain(self.headers.iter().skip(self.skip).map(String::as_str));
        csv_safe::write_record(out, self.delimiter, fields);
    }

    /// The lines of one row; `cells` is empty for a row that is not a data row
    /// (the header row, for problems with the whole file).
    fn row_records(&self, out: &mut String, row: u32, lines: &[Line], cells: &[String]) {
        let number = row.to_string();
        for l in lines {
            let column = l.column.and_then(|c| self.headers.get(c)).map(String::as_str).unwrap_or("");
            let head = [number.as_str(), l.severity.as_str(), column, l.message.as_str(), l.code.as_str()];
            let data = (self.skip..self.headers.len()).map(|i| cells.get(i).map(String::as_str).unwrap_or(""));
            csv_safe::write_record(out, self.delimiter, head.into_iter().chain(data));
        }
    }
}

/// `<name>-errors.csv`, from the uploaded file's name without its extension.
fn report_name(file_name: &str) -> String {
    let stem = Path::new(file_name).file_stem().and_then(|s| s.to_str()).unwrap_or("import");
    format!("{stem}-errors.csv")
}

fn no_report(id: Uuid) -> AppError {
    AppError::not_found(format!("Import {id} has no error report"))
}

pub async fn download(
    pool: &PgPool,
    ctx: &RequestContext,
    cfg: &ImportConfig,
    id: Uuid,
) -> Result<CsvDownload, AppError> {
    let mut conn = pool.acquire().await?;
    require_enabled(&mut conn, cfg).await?;
    let job = check_owner(ctx, fetch(&mut conn, id).await?, id)?;
    if job.status == JobStatus::Expired {
        return Err(no_report(id));
    }
    let has_file: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM cmdb.import_job_files WHERE job_id = $1)")
        .bind(id)
        .fetch_one(&mut *conn)
        .await?;
    type IssueRow = (i32, Option<i32>, String, String, String);
    let rows: Vec<IssueRow> = sqlx::query_as(
        "SELECT row_no, col_index, severity, code, message FROM cmdb.import_job_issues
         WHERE job_id = $1 ORDER BY row_no, seq",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    if !has_file || rows.is_empty() {
        return Err(no_report(id));
    }
    let mut pending: BTreeMap<u32, Vec<Line>> = BTreeMap::new();
    for (row, col, severity, code, message) in rows {
        pending.entry(row.max(0) as u32).or_default().push(Line {
            column: col.and_then(|c| usize::try_from(c).ok()),
            severity,
            code,
            message,
        });
    }

    drop(conn);

    if job.created_by_id.is_none() || job.created_by_id != ctx_user(ctx) {
        let mut tx = pool.begin().await?;
        let entry = AuditEntry {
            action: AuditAction::ImportReportRead,
            entity_type: "import_jobs",
            entity_id: id,
            old_value: None,
            new_value: Some(json!({
                "fileName": job.file_name,
                "classKey": job.class_key,
                "ownerId": job.created_by_id,
                "ownerName": job.created_by_name,
            })),
        };
        crud::write_audit(&mut tx, ctx, vec![entry]).await?;
        tx.commit().await?;
    }

    let layout = Layout::of(&job);
    let file =
        DbFile { pool: pool.clone(), job: id, len: job.file_size as u64, runtime: tokio::runtime::Handle::current() };
    let limits = Limits { max_rows: cfg.max_rows, max_columns: MAX_COLUMNS };
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(4);
    tokio::task::spawn_blocking({
        let (format, options) = (job.format(), job.options());
        move || write(&file, format, &options, &limits, &layout, pending, &tx)
    });
    let mut rx = rx;
    let body = axum::body::Body::from_stream(futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx)));
    Ok(CsvDownload { file_name: report_name(&job.file_name), body })
}

/// Writes the report into `tx` piece by piece. A read failure (the file was
/// read by the dry run, so only a lost database) ends the response early.
fn write(
    file: &DbFile,
    format: FileFormat,
    options: &FileOptions,
    limits: &Limits,
    layout: &Layout,
    mut pending: BTreeMap<u32, Vec<Line>>,
    tx: &mpsc::Sender<Result<Bytes, std::io::Error>>,
) {
    let mut out = String::from(csv_safe::BOM);
    layout.header_record(&mut out);
    let mut gone = false;
    let mut flush = |out: &mut String, force: bool| {
        if (force || out.len() >= FLUSH_BYTES) && !out.is_empty() {
            gone |= tx.blocking_send(Ok(Bytes::from(std::mem::take(out)))).is_err();
        }
        !gone
    };
    // Problems of rows before `before` that are not data rows.
    let flush_before = |out: &mut String, pending: &mut BTreeMap<u32, Vec<Line>>, before: u32| {
        while let Some(entry) = pending.first_entry() {
            if *entry.key() >= before {
                break;
            }
            let (row, lines) = entry.remove_entry();
            layout.row_records(out, row, &lines, &[]);
        }
    };
    let result = analyse::read_file(file, format, options, limits, &mut |row: Row| {
        flush_before(&mut out, &mut pending, row.number);
        if let Some(lines) = pending.remove(&row.number) {
            let cells: Vec<String> = row.cells.iter().map(|c| c.display()).collect();
            layout.row_records(&mut out, row.number, &lines, &cells);
        }
        if !flush(&mut out, false) || pending.is_empty() {
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    });
    if let Err(e) = result {
        tracing::warn!(job = %file.job, code = e.code, "import error report: the stored file could not be read");
        let _ = tx.blocking_send(Err(std::io::Error::other("the stored file could not be read")));
        return;
    }
    flush_before(&mut out, &mut pending, u32::MAX);
    flush(&mut out, true);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(headers: &[&str]) -> Layout {
        Layout::new(';', headers.iter().map(|h| h.to_string()).collect())
    }

    fn line(column: Option<usize>, code: &str, message: &str) -> Line {
        Line { column, severity: "error".into(), code: code.into(), message: message.into() }
    }

    #[test]
    fn a_row_with_two_problems_appears_twice_with_its_cells() {
        let l = layout(&["Hostname", "Cores"]);
        let mut out = String::new();
        l.header_record(&mut out);
        let lines = [line(Some(1), "invalid_type", "Not a whole number"), line(None, "required", "=Missing")];
        l.row_records(&mut out, 3, &lines, &["=cmd|' /C calc'!A0".into(), "-5".into()]);
        assert_eq!(
            out,
            "\"Row\";\"Severity\";\"Column\";\"Problem\";\"Code\";\"Hostname\";\"Cores\"\r\n\
             \"3\";\"error\";\"Cores\";\"Not a whole number\";\"invalid_type\";\"'=cmd|' /C calc'!A0\";\"'-5\"\r\n\
             \"3\";\"error\";\"\";\"'=Missing\";\"required\";\"'=cmd|' /C calc'!A0\";\"'-5\"\r\n"
        );
    }

    #[test]
    fn a_report_of_a_report_does_not_repeat_the_old_problem_columns() {
        let l = layout(&["Row", "Severity", "Column", "Problem", "Code", "Hostname"]);
        let mut out = String::new();
        l.header_record(&mut out);
        let cells: Vec<String> = ["2", "error", "", "old", "x", "web01"].iter().map(|s| s.to_string()).collect();
        l.row_records(&mut out, 2, &[line(Some(5), "exists", "new")], &cells);
        assert_eq!(
            out,
            "\"Row\";\"Severity\";\"Column\";\"Problem\";\"Code\";\"Hostname\"\r\n\
             \"2\";\"error\";\"Hostname\";\"new\";\"exists\";\"web01\"\r\n"
        );
    }

    #[test]
    fn the_file_name_ends_in_errors_csv() {
        assert_eq!(report_name("servers.xlsx"), "servers-errors.csv");
        assert_eq!(report_name("a.b.csv"), "a.b-errors.csv");
        assert_eq!(report_name("noext"), "noext-errors.csv");
    }
}
