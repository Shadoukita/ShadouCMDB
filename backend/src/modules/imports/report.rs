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
//! - Each stream holds a blocking thread, so they are capped per process and
//!   per user, and a client that stops reading is cut off (GH#351).

use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::task::Poll;
use std::time::{Duration, Instant};

use axum::body::Bytes;
use sqlx::PgPool;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::MAX_COLUMNS;
use super::analyse::{self, FileFormat, FileOptions, REPORT_HEADERS};
use super::jobs::{JobRow, check_owner, ctx_user, fetch, foreign_read_entry};
use super::parse::{Limits, Row};
use super::schemas::JobStatus;
use super::storage::DbFile;
use crate::api::context::RequestContext;
use crate::api::route::CsvDownload;
use crate::config::ImportConfig;
use crate::data::crud;
use crate::http::error::AppError;
use crate::modules::csv_safe;
use crate::modules::download_slots::Slots;

/// Bytes collected before a piece of the report is sent.
const FLUSH_BYTES: usize = 64 * 1024;
/// Reports streaming at once in this process; more are answered 503 SERVER_BUSY.
const MAX_STREAMS: usize = 8;
/// Reports one user streams at once; more are answered 429 RATE_LIMITED.
const MAX_STREAMS_PER_USER: usize = 2;
/// How long a piece may wait for the client to read before the download is cut off.
const SEND_TIMEOUT: Duration = Duration::from_secs(30);
/// How long one download may take in all.
const STREAM_DEADLINE: Duration = Duration::from_secs(15 * 60);

static STREAMS: LazyLock<Slots> =
    LazyLock::new(|| Slots::new(MAX_STREAMS, MAX_STREAMS_PER_USER, "error report downloads"));

/// Where the writer sends the pieces: each send waits at most `timeout`, and
/// all of them together at most until `deadline`.
struct Sink {
    tx: mpsc::Sender<Result<Bytes, std::io::Error>>,
    runtime: tokio::runtime::Handle,
    timeout: Duration,
    deadline: Instant,
    /// Set when the report was not written to the end; the body then ends with
    /// an error instead of looking complete.
    failed: Arc<AtomicBool>,
}

impl Sink {
    /// `false` once the client is gone or too slow; the writer stops then.
    fn send(&self, piece: Bytes) -> bool {
        let wait = self.deadline.saturating_duration_since(Instant::now()).min(self.timeout);
        match self.runtime.block_on(tokio::time::timeout(wait, self.tx.send(Ok(piece)))) {
            Ok(Ok(())) => true,
            Ok(Err(_)) => false,
            Err(_) => {
                tracing::warn!("import error report: the client did not read the download in time; stopped");
                self.fail();
                false
            }
        }
    }

    fn fail(&self) {
        self.failed.store(true, Ordering::SeqCst);
    }
}

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
    // No `require_enabled`: the owner keeps the report while import is off (W3).
    let mut conn = pool.acquire().await?;
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

    // Before the audit event: a refused download was not read.
    let permit = STREAMS.acquire(ctx_user(ctx))?;
    if job.created_by_id.is_none() || job.created_by_id != ctx_user(ctx) {
        let mut tx = pool.begin().await?;
        crud::write_audit(&mut tx, ctx, vec![foreign_read_entry(&job, "report")]).await?;
        tx.commit().await?;
    }

    let layout = Layout::of(&job);
    let file =
        DbFile { pool: pool.clone(), job: id, len: job.file_size as u64, runtime: tokio::runtime::Handle::current() };
    let limits = Limits::new(cfg.max_rows, MAX_COLUMNS, cfg.max_file_bytes);
    let (sink, mut rx) = Sink::new(SEND_TIMEOUT, STREAM_DEADLINE);
    let failed = sink.failed.clone();
    tokio::task::spawn_blocking({
        let (format, options) = (job.format(), job.options());
        move || {
            let _permit = permit;
            write(&file, format, &options, &limits, &layout, pending, &sink)
        }
    });
    let mut ended = false;
    let body = axum::body::Body::from_stream(futures_util::stream::poll_fn(move |cx| {
        match rx.poll_recv(cx) {
            // A report cut short ends with an error, so the client does not
            // take it for the whole report.
            Poll::Ready(None) if !ended && failed.load(Ordering::SeqCst) => {
                ended = true;
                Poll::Ready(Some(Err(std::io::Error::other("the error report was not written to the end"))))
            }
            other => other,
        }
    }));
    Ok(CsvDownload { file_name: report_name(&job.file_name), body })
}

impl Sink {
    fn new(timeout: Duration, total: Duration) -> (Self, mpsc::Receiver<Result<Bytes, std::io::Error>>) {
        let (tx, rx) = mpsc::channel(4);
        let sink = Sink {
            tx,
            runtime: tokio::runtime::Handle::current(),
            timeout,
            deadline: Instant::now() + total,
            failed: Arc::default(),
        };
        (sink, rx)
    }
}

/// Writes the report into `sink` piece by piece. A read failure (the file was
/// read by the dry run, so only a lost database) ends the response early.
fn write(
    file: &DbFile,
    format: FileFormat,
    options: &FileOptions,
    limits: &Limits,
    layout: &Layout,
    mut pending: BTreeMap<u32, Vec<Line>>,
    sink: &Sink,
) {
    let mut out = String::from(csv_safe::BOM);
    layout.header_record(&mut out);
    let mut gone = false;
    let mut flush = |out: &mut String, force: bool| {
        if !gone && (force || out.len() >= FLUSH_BYTES) && !out.is_empty() {
            gone = !sink.send(Bytes::from(std::mem::take(out)));
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
        sink.fail();
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

    /// Sends until the sink gives up; the number of pieces sent.
    fn fill(sink: Sink) -> usize {
        let mut sent = 0;
        while sink.send(Bytes::from_static(b"x")) {
            sent += 1;
        }
        sent
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_download_nobody_reads_gives_its_place_back_within_the_timeout() {
        let streams = Slots::new(1, 1, "downloads");
        let permit = streams.acquire(Some(Uuid::new_v4())).unwrap();
        let (sink, _rx) = Sink::new(Duration::from_millis(200), Duration::from_secs(600));
        let failed = sink.failed.clone();
        let writer = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            fill(sink)
        });
        let sent = tokio::time::timeout(Duration::from_secs(10), writer).await.expect("the writer stops").unwrap();
        assert_eq!(sent, 4, "the channel's capacity, then the timeout");
        assert!(failed.load(Ordering::SeqCst), "the body ends with an error");
        assert!(streams.acquire(Some(Uuid::new_v4())).is_ok(), "the place was given back");

        // A client that reads, but too slowly, is cut off at the deadline.
        let (sink, mut rx) = Sink::new(Duration::from_secs(600), Duration::from_millis(300));
        let reader = tokio::spawn(async move {
            while rx.recv().await.is_some() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });
        let started = Instant::now();
        tokio::task::spawn_blocking(move || fill(sink)).await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(10));
        reader.await.unwrap();
    }

    #[test]
    fn the_file_name_ends_in_errors_csv() {
        assert_eq!(report_name("servers.xlsx"), "servers-errors.csv");
        assert_eq!(report_name("a.b.csv"), "a.b-errors.csv");
        assert_eq!(report_name("noext"), "noext-errors.csv");
    }
}
