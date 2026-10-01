//! The commit (§2.6): the rows of a validated job written in file order.
//!
//! - **Chunks.** [`CHUNK_ROWS`] rows per transaction, without savepoints
//!   (T2). Each chunk re-resolves its own keys and locks the CIs they match in
//!   id order (T3, T4), then plans its rows again against the locked state
//!   through the same plan step as the CI API (D10), so drift since the dry
//!   run is caught.
//! - **Replay.** When a statement fails, the chunk rolls back and its rows are
//!   written again one transaction per row. A row that fails again is
//!   `failed`; the others are applied. A deadlock or serialization failure
//!   runs the chunk (or row) again, at most [`RETRIES`] times, then the job
//!   fails.
//! - **Cursor and fencing (T13).** Every transaction ends by moving
//!   `committed_through_row` and the counts, naming the lease; when that
//!   updates nothing, the transaction rolls back and the worker lets the job
//!   go. A worker that takes the job over resumes after the cursor, so no row
//!   is written twice.
//! - **Rows with dry-run errors** are never attempted (`skipped`).
//! - **Cancel** stops after the current chunk; the rows written so far stay.
//!   The cancel only requests the stop (`cancel_requested_at`); the job stays
//!   `committing` until [`finish`] ends it as `cancelled` with its final
//!   counts, so a `cancelled` job never changes again (GH#359).
//! - **Audit (§4.3).** Per-CI and per-relationship entries in the chunk's
//!   transaction, as the owner with `actor_type = import` and
//!   `request_id = import:<jobId>`; nothing for unchanged rows; one
//!   `import.commit` event when the commit ends, with the job's final status.
//!   Only the owner can start a commit ([`jobs::check_own_job`]), so the
//!   owner the entries name is who asked for it (GH#389).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::http::HeaderMap;
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::dry_run::{MAX_STORED_ISSUES, model_fingerprint, owner_context, stream_rows};
use super::jobs::{self, COLUMNS, JobRow, check_own_job, fetch_for_update, require_enabled};
use super::parse::{Limits, Row};
use super::planner::{self, Context, Issue, JobData, Matches, Pending, Severity};
use super::schemas::{CommitCounts, CommitImport, ImportJob, ImportSummary, JobStatus, RowOutcome};
use super::storage::DbFile;
use super::worker::Lease;
use super::{MAX_COLUMNS, coded, mapping, upload};
use crate::api::context::RequestContext;
use crate::auth::permissions::Permissions;
use crate::auth::{Credential, Principal};
use crate::config::ImportConfig;
use crate::data::crud::{self, AuditAction, AuditEntry};
use crate::data::items as items_data;
use crate::data::relationships as rel_data;
use crate::http::error::{AppError, ErrorCode};
use crate::modules::items::plan::{self, Registry};
use crate::modules::items::service::details;
use crate::modules::relationships;

/// Runs of one chunk (or replayed row) after a deadlock or serialization failure.
pub const RETRIES: u32 = 3;

// ---------------------------------------------------------------------------
// POST /imports/{id}/commit
// ---------------------------------------------------------------------------

fn refused(code: &str, message: &str) -> AppError {
    coded(ErrorCode::Conflict, message, code)
}

/// Queues the commit of a validated job (`POST /imports/{id}/commit`).
///
/// Refused with `409` and `details[0].code`: `dry_run_required` (no dry run
/// since the mapping was set), `dry_run_stale` (the data model changed or the
/// dry run is older than 24 h, T14), `has_error_rows` (without
/// `skipErrorRows`) and `invalid_state`. `429 import_busy` while another job
/// of the owner runs (T19). A repeated `Idempotency-Key` returns the job as it
/// is now and starts nothing (T15).
pub async fn start(
    pool: &PgPool,
    ctx: &RequestContext,
    cfg: &ImportConfig,
    headers: &HeaderMap,
    id: Uuid,
    input: &CommitImport,
) -> Result<ImportJob, AppError> {
    let caller = jobs::ctx_user(ctx).ok_or_else(AppError::internal)?;
    let key = upload::idempotency_key(headers)?;
    if let Some(key) = &key
        && upload::replay(pool, caller, key, "commit", Some(id)).await?.is_some()
    {
        return jobs::get(pool, ctx, id).await;
    }
    let mut tx = pool.begin().await?;
    require_enabled(&mut tx, cfg).await?;
    let job = check_own_job(ctx, fetch_for_update(&mut tx, id).await?, id)?;
    match job.status {
        JobStatus::Validated => {}
        JobStatus::Ready if job.mapping.is_some() => {
            return Err(refused("dry_run_required", "Run the dry run before the commit."));
        }
        _ => return Err(refused("invalid_state", "Only a job whose dry run finished can be committed.")),
    }
    let Some(finished) = job.dry_run_finished_at else {
        return Err(refused("dry_run_required", "Run the dry run before the commit."));
    };
    let fingerprint = model_fingerprint(&mut tx).await?;
    if job.model_fingerprint.as_deref() != Some(fingerprint.as_str()) {
        return Err(refused(
            "dry_run_stale",
            "The data model changed since the dry run. Run the dry run again before the commit.",
        ));
    }
    if Utc::now() - finished > chrono::Duration::hours(24) {
        return Err(refused("dry_run_stale", "The dry run is older than 24 hours. Run it again before the commit."));
    }
    let summary = job.summary().unwrap_or_default();
    if summary.error_rows > 0 && !input.skip_error_rows {
        return Err(refused(
            "has_error_rows",
            &format!(
                "{} rows have errors. Fix the file, or commit with skipErrorRows to import the other rows.",
                summary.error_rows
            ),
        ));
    }
    let owner = job.created_by_id.ok_or_else(AppError::internal)?;
    upload::lock_user(&mut tx, owner).await?;
    let summary = ImportSummary { committed: Some(CommitCounts::default()), ..summary };
    let row: JobRow = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "UPDATE cmdb.import_jobs SET status = 'queued', phase = 'commit', summary = $2, committed_through_row = 0,
           error = NULL, attempts = 0, progress_done = 0,
           progress_total = coalesce((file_info->>'rowCount')::int, 0), queued_at = now(),
           expires_at = now() + interval '24 hours'
         WHERE id = $1 RETURNING {COLUMNS}"
    )))
    .bind(id)
    .bind(sqlx::types::Json(&summary))
    .fetch_one(&mut *tx)
    .await?;
    if let Some(key) = &key {
        sqlx::query(
            "INSERT INTO cmdb.import_idempotency_keys (user_id, key, operation, job_id) VALUES ($1, $2, 'commit', $3)
             ON CONFLICT (user_id, operation, key) DO UPDATE SET job_id = EXCLUDED.job_id, created_at = now()",
        )
        .bind(caller)
        .bind(key)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    let position = jobs::queue_position(&mut tx, &row).await?;
    tx.commit().await?;
    Ok(row.dto(position))
}

// ---------------------------------------------------------------------------
// The worker phase
// ---------------------------------------------------------------------------

/// A job-level stop: `(code, message)`.
type Stop = (&'static str, String);

fn internal() -> Stop {
    ("internal_error", "The commit stopped because of an internal error.".into())
}

fn stopped() -> Stop {
    ("stopped", String::new())
}

/// What the commit keeps between chunks.
struct State {
    job: JobRow,
    data: JobData,
    /// Rows with errors in the dry run: never attempted.
    dry_errors: HashSet<u32>,
    /// When the dry run stored only part of its problems: the last row it
    /// stored one for. A later row whose plan has errors was an error row of
    /// the dry run too, as far as anyone can tell, so it is skipped.
    stored_through: Option<u32>,
    summary: ImportSummary,
    next_seq: i32,
    stored_issues: usize,
    done: i32,
}

impl State {
    fn counts(&mut self) -> &mut CommitCounts {
        self.summary.committed.get_or_insert_with(CommitCounts::default)
    }

    fn was_dry_error(&self, row: u32) -> bool {
        self.dry_errors.contains(&row) || self.stored_through.is_some_and(|last| row > last)
    }
}

/// How a transaction of the commit ended.
enum Written {
    Ok,
    /// Another worker holds the job now.
    Lost,
}

/// What [`record`] wrote, for the state once the transaction committed.
struct Recorded {
    stored: usize,
    found: u32,
}

pub async fn run(pool: &PgPool, cfg: &ImportConfig, lease: &Lease, lost: &Arc<AtomicBool>) {
    let result = commit(pool, cfg, lease, lost).await;
    let (status, error) = match result {
        Ok(Some(status)) => (status, None),
        Ok(None) | Err(("stopped", _)) => return,
        Err((code, message)) => {
            tracing::info!(job = %lease.job, code, "import commit stopped");
            ("failed", Some(json!({ "code": code, "message": message })))
        }
    };
    if let Err(e) = finish(pool, lease, status, error).await {
        tracing::warn!(job = %lease.job, error = %e, "import commit: end not recorded; the next worker records it");
    }
}

/// Ends a commit that cannot go on (attempts used up, a panic) with its
/// `import.commit` event.
pub async fn fail(pool: &PgPool, lease: &Lease, code: &str, message: &str) -> sqlx::Result<bool> {
    finish(pool, lease, "failed", Some(json!({ "code": code, "message": message }))).await
}

/// Writes rows chunk by chunk. `Ok(Some(status))` is how the job ends,
/// `Ok(None)` that it is no longer this worker's.
async fn commit(
    pool: &PgPool,
    cfg: &ImportConfig,
    lease: &Lease,
    lost: &Arc<AtomicBool>,
) -> Result<Option<&'static str>, Stop> {
    let started = std::time::Instant::now();
    let mut conn = pool.acquire().await.map_err(|_| internal())?;
    let job = jobs::fetch(&mut conn, lease.job).await.map_err(|_| internal())?.ok_or_else(stopped)?;
    let mapping = job.mapping().ok_or_else(internal)?;
    let info = job.info().ok_or_else(internal)?;
    let headers: Vec<String> = info.columns.iter().map(|c| c.header.clone()).collect();
    let ctx = owner_context(&mut conn, cfg, job.created_by_id, job.id).await?;
    let resolved = mapping::resolve(&mut conn, &ctx, &mapping, &headers).await.map_err(|_| {
        ("dry_run_stale", "The mapping no longer fits the data model. Run the dry run again.".to_owned())
    })?;
    let data = JobData::load(&mut conn, resolved).await.map_err(|_| internal())?;

    let issue_rows: Vec<(i32, String, String)> =
        sqlx::query_as("SELECT row_no, severity, phase FROM cmdb.import_job_issues WHERE job_id = $1")
            .bind(job.id)
            .fetch_all(&mut *conn)
            .await
            .map_err(|_| internal())?;
    let next_seq: Option<i32> = sqlx::query_scalar("SELECT max(seq) + 1 FROM cmdb.import_job_issues WHERE job_id = $1")
        .bind(job.id)
        .fetch_one(&mut *conn)
        .await
        .map_err(|_| internal())?;
    drop(conn);
    let summary = job.summary().unwrap_or_default();
    let dry_errors: HashSet<u32> = issue_rows
        .iter()
        .filter(|(_, sev, phase)| sev == "error" && phase == "validate")
        .map(|(r, _, _)| *r as u32)
        .collect();
    let stored_validate = issue_rows.iter().filter(|(_, _, phase)| phase == "validate").count();
    let truncated = (summary.issues_total as usize) > stored_validate && stored_validate >= MAX_STORED_ISSUES;
    let stored_through = truncated.then(|| issue_rows.iter().map(|(r, _, _)| *r as u32).max().unwrap_or(0));
    let cursor = job.committed_through_row as u32;
    let mut s = State {
        dry_errors,
        stored_through,
        summary,
        next_seq: next_seq.unwrap_or(0),
        stored_issues: issue_rows.len(),
        done: 0,
        job,
        data,
    };

    // The reader stops on its own flag; the commit sees a cancel at the next
    // chunk boundary.
    let reading = Arc::new(AtomicBool::new(false));
    let file = DbFile {
        pool: pool.clone(),
        job: s.job.id,
        len: s.job.file_size as u64,
        runtime: tokio::runtime::Handle::current(),
    };
    let limits = Limits::new(cfg.max_rows, MAX_COLUMNS, cfg.max_file_bytes);
    let (mut rx, reader) = stream_rows(file, s.job.format(), s.job.options(), limits, reading.clone());
    let end = |r: Result<Option<&'static str>, Stop>| {
        reading.store(true, Ordering::SeqCst);
        r
    };
    while let Some(rows) = rx.recv().await {
        s.done += rows.iter().filter(|r| r.number <= cursor).count() as i32;
        let rows: Vec<Row> = rows.into_iter().filter(|r| r.number > cursor).collect();
        if rows.is_empty() {
            continue;
        }
        // The owner's rights as they are now, at every chunk (§3.6, T22).
        let ctx = {
            let mut conn = pool.acquire().await.map_err(|_| internal())?;
            match cancel_requested(&mut conn, lease).await.map_err(|_| internal())? {
                None => return end(Ok(None)),
                Some(true) => return end(Ok(Some("cancelled"))),
                Some(false) => {}
            }
            match owner_context(&mut conn, cfg, s.job.created_by_id, s.job.id).await {
                Ok(ctx) => ctx,
                Err(stop) => return end(Err(stop)),
            }
        };
        #[cfg(test)]
        test_hooks::pause(s.job.id).await;
        match chunk(pool, &mut s, &ctx, lease, &rows).await {
            Ok(Written::Ok) => {}
            Ok(Written::Lost) => return end(Ok(None)),
            Err(stop) => return end(Err(stop)),
        }
        #[cfg(test)]
        if test_hooks::dies_now(s.job.id) {
            return end(Ok(None));
        }
    }
    reader.await.map_err(|_| internal())?.map_err(|e| {
        if e.code == "stopped" {
            stopped()
        } else {
            ("file_changed", format!("The file could not be read again: {}", e.message))
        }
    })?;
    let mut conn = pool.acquire().await.map_err(|_| internal())?;
    let status = match cancel_requested(&mut conn, lease).await.map_err(|_| internal())? {
        None => return Ok(None),
        Some(true) => "cancelled",
        Some(false) => {
            let c = s.counts();
            if c.failed > 0 || c.skipped > 0 { "completed_with_errors" } else { "completed" }
        }
    };
    let c = s.counts().clone();
    tracing::info!(
        job = %s.job.id, created = c.created, updated = c.updated, unchanged = c.unchanged, skipped = c.skipped,
        failed = c.failed, relationships = c.relationships_added, ms = started.elapsed().as_millis() as u64,
        lost = lost.load(Ordering::SeqCst), "import commit finished"
    );
    Ok(Some(status))
}

/// Whether a stop was requested, if this worker still holds the job.
async fn cancel_requested(conn: &mut PgConnection, lease: &Lease) -> sqlx::Result<Option<bool>> {
    sqlx::query_scalar(
        "SELECT cancel_requested_at IS NOT NULL FROM cmdb.import_jobs
         WHERE id = $1 AND lease_owner = $2 AND lease_epoch = $3 AND status = 'committing'",
    )
    .bind(lease.job)
    .bind(&lease.owner)
    .bind(lease.epoch)
    .fetch_optional(conn)
    .await
}

/// One chunk: all rows in one transaction, else row by row.
async fn chunk(
    pool: &PgPool,
    s: &mut State,
    ctx: &RequestContext,
    lease: &Lease,
    rows: &[Row],
) -> Result<Written, Stop> {
    match attempt(pool, s, ctx, lease, rows).await? {
        Ok(w) => return Ok(w),
        Err(e) => {
            tracing::info!(job = %s.job.id, first_row = rows[0].number, error = %e.message,
                "import commit: a chunk failed; writing its rows one by one");
        }
    }
    for row in rows {
        match attempt(pool, s, ctx, lease, std::slice::from_ref(row)).await? {
            Ok(Written::Ok) => {}
            Ok(Written::Lost) => return Ok(Written::Lost),
            Err(e) => {
                // The row failed on its own: record it and move the cursor past it.
                let issues = failure_issues(&s.data, row, &e);
                let mut delta = CommitCounts::default();
                if s.was_dry_error(row.number) {
                    delta.skipped = 1;
                } else {
                    delta.failed = 1;
                }
                let mut tx = pool.begin().await.map_err(|_| internal())?;
                let Some(recorded) =
                    record(&mut tx, s, lease, row.number, 1, &delta, issues).await.map_err(|_| internal())?
                else {
                    return Ok(Written::Lost);
                };
                tx.commit().await.map_err(|_| internal())?;
                s.apply(&delta, 1, &recorded);
            }
        }
    }
    Ok(Written::Ok)
}

impl State {
    /// Follows a committed transaction.
    fn apply(&mut self, delta: &CommitCounts, rows: i32, recorded: &Recorded) {
        self.next_seq += recorded.stored as i32;
        self.stored_issues += recorded.stored;
        self.summary.issues_total += recorded.found;
        let c = self.counts();
        c.created += delta.created;
        c.updated += delta.updated;
        c.unchanged += delta.unchanged;
        c.skipped += delta.skipped;
        c.failed += delta.failed;
        c.relationships_added += delta.relationships_added;
        self.done += rows;
    }
}

/// Writes `rows` in one transaction, retrying deadlocks. `Ok(Err(e))`: a
/// statement failed and nothing was written.
async fn attempt(
    pool: &PgPool,
    s: &mut State,
    ctx: &RequestContext,
    lease: &Lease,
    rows: &[Row],
) -> Result<Result<Written, AppError>, Stop> {
    let mut tries = 0;
    loop {
        let mut tx = pool.begin().await.map_err(|_| internal())?;
        let result = write_rows(&mut tx, s, ctx, rows).await;
        let (delta, issues) = match result {
            Ok(r) => r,
            Err(e) if e.code == ErrorCode::ServerBusy && tries + 1 < RETRIES => {
                drop(tx);
                tries += 1;
                tracing::info!(job = %s.job.id, tries, "import commit: deadlock; running the rows again");
                continue;
            }
            Err(e) if matches!(e.code, ErrorCode::ServerBusy | ErrorCode::DatabaseUnavailable) => {
                tracing::warn!(job = %s.job.id, error = %e.message, "import commit: giving up after retries");
                return Err(internal());
            }
            Err(e) => return Ok(Err(e)),
        };
        let last = rows.last().map(|r| r.number).unwrap_or(0);
        let recorded = match record(&mut tx, s, lease, last, rows.len() as i32, &delta, issues).await {
            Ok(Some(r)) => r,
            Ok(None) => return Ok(Ok(Written::Lost)),
            Err(e) => {
                let e = AppError::from(e);
                if e.code == ErrorCode::ServerBusy && tries + 1 < RETRIES {
                    tries += 1;
                    continue;
                }
                return Err(internal());
            }
        };
        match tx.commit().await {
            Ok(()) => {}
            Err(e) => {
                let e = AppError::from(e);
                if e.code == ErrorCode::ServerBusy && tries + 1 < RETRIES {
                    tries += 1;
                    continue;
                }
                return Ok(Err(e));
            }
        }
        s.apply(&delta, rows.len() as i32, &recorded);
        return Ok(Ok(Written::Ok));
    }
}

/// Plans `rows` against the locked state and writes them, with their audit
/// entries. Returns the counts and the rows' problems.
async fn write_rows(
    conn: &mut PgConnection,
    s: &State,
    ctx: &RequestContext,
    rows: &[Row],
) -> Result<(CommitCounts, Vec<Issue>), AppError> {
    let data = &s.data;
    let model = &data.model;
    let mut delta = CommitCounts::default();
    let mut issues = Vec::new();
    let todo: Vec<Row> = rows.iter().filter(|r| !s.dry_errors.contains(&r.number)).cloned().collect();
    delta.skipped = (rows.len() - todo.len()) as u32;
    if todo.is_empty() {
        return Ok((delta, issues));
    }
    let mut pending = Pending::default();
    let mut c = Context {
        job: data,
        ctx,
        matches: Matches::Chunk,
        pending: &mut pending,
        duplicates: None,
        new_ids: None,
        lock: true,
        grow_pending: true,
    };
    let planned = planner::plan_chunk(conn, &mut c, &todo).await?;

    let mut written: Vec<(Uuid, Uuid, Option<Value>)> = Vec::new();
    let mut edges: Vec<Uuid> = Vec::new();
    for p in &planned.rows {
        match p.outcome {
            RowOutcome::Error => {
                if s.was_dry_error(p.number) {
                    delta.skipped += 1;
                } else {
                    // Drift since the dry run: the row is no longer valid.
                    delta.failed += 1;
                    issues.extend(p.issues.iter().cloned());
                }
                continue;
            }
            RowOutcome::Create => {
                let body = p.create.as_ref().ok_or_else(AppError::internal)?;
                let defs = planned.defs.get(&body.class_id).map(Vec::as_slice).unwrap_or_default();
                let mut plan = plan::plan_create(ctx, model, defs, body, &planned.resolver)?;
                if let Registry::Create { id, .. } = &mut plan.registry {
                    *id = p.ci_id;
                }
                let id = plan::apply_rows(conn, model, &plan).await?;
                written.push((id, body.class_id, None));
                delta.created += 1;
            }
            RowOutcome::Update => {
                let (before, body) = p.update.as_ref().ok_or_else(AppError::internal)?;
                let class = before.summary.class_id;
                let defs = planned.defs.get(&class).map(Vec::as_slice).unwrap_or_default();
                let plan = plan::plan_update(ctx, model, defs, before.clone(), body, &planned.resolver, None)?;
                let id = plan::apply_rows(conn, model, &plan).await?;
                written.push((id, class, Some(crud::json(before))));
                delta.updated += 1;
            }
            RowOutcome::Unchanged => delta.unchanged += 1,
        }
        for (type_id, source, target, _) in &p.edges {
            edges.push(rel_data::insert(conn, *type_id, *source, *target, None).await?);
        }
        delta.relationships_added += p.edges.len() as u32;
    }

    // Labels once for the chunk, then the audit entries from what was stored.
    let ids: Vec<Uuid> = written.iter().map(|(id, _, _)| *id).collect();
    let classes: Vec<Uuid> = written.iter().map(|(_, class, _)| *class).collect::<HashSet<_>>().into_iter().collect();
    if !ids.is_empty() {
        items_data::refresh_labels(conn, model, &classes, Some(&ids)).await?;
    }
    let after: HashMap<Uuid, Value> =
        details(conn, model, &ids).await?.into_iter().map(|ci| (ci.summary.id, crud::json(&ci))).collect();
    let mut entries: Vec<AuditEntry> = written
        .into_iter()
        .map(|(id, _, before)| AuditEntry {
            action: if before.is_some() { AuditAction::Update } else { AuditAction::Create },
            entity_type: "configuration_items",
            entity_id: id,
            old_value: before,
            new_value: after.get(&id).cloned(),
        })
        .collect();
    for id in edges {
        let dto = relationships::load(conn, id).await?;
        entries.push(AuditEntry {
            action: AuditAction::Create,
            entity_type: "ci_relationships",
            entity_id: id,
            old_value: None,
            new_value: Some(crud::json(&dto)),
        });
    }
    crud::write_audit(conn, ctx, entries).await?;
    Ok((delta, issues))
}

/// The problems of a row that failed at commit.
fn failure_issues(data: &JobData, row: &Row, e: &AppError) -> Vec<Issue> {
    let column_of = |field: &str| planner::column_of_field(&data.resolved, field).map(|c| c.index);
    let base = |column: Option<u32>, field: Option<String>, code: &str, message: &str| Issue {
        row: row.number,
        column,
        field,
        value: None,
        severity: Severity::Error,
        code: code.into(),
        message: message.into(),
    };
    match &e.details {
        Some(details) if !details.is_empty() => {
            details.iter().map(|f| base(column_of(&f.field), Some(f.field.clone()), &f.code, &f.message)).collect()
        }
        _ => vec![base(None, None, "commit_failed", "The row could not be written.")],
    }
}

/// Stores the problems and moves the cursor, fenced by the lease (T13). The
/// counts written are the state's plus `delta`.
async fn record(
    conn: &mut PgConnection,
    s: &State,
    lease: &Lease,
    through: u32,
    rows: i32,
    delta: &CommitCounts,
    issues: Vec<Issue>,
) -> sqlx::Result<Option<Recorded>> {
    let mut summary = s.summary.clone();
    let c = summary.committed.get_or_insert_with(CommitCounts::default);
    c.created += delta.created;
    c.updated += delta.updated;
    c.unchanged += delta.unchanged;
    c.skipped += delta.skipped;
    c.failed += delta.failed;
    c.relationships_added += delta.relationships_added;
    summary.issues_total += issues.len() as u32;
    let n = sqlx::query(
        "UPDATE cmdb.import_jobs SET committed_through_row = $4, progress_done = least($5, progress_total),
           summary = $6, attempts = 0, expires_at = now() + interval '24 hours'
         WHERE id = $1 AND lease_owner = $2 AND lease_epoch = $3 AND status = 'committing'",
    )
    .bind(lease.job)
    .bind(&lease.owner)
    .bind(lease.epoch)
    .bind(through as i32)
    .bind(s.done + rows)
    .bind(sqlx::types::Json(&summary))
    .execute(&mut *conn)
    .await?
    .rows_affected();
    if n != 1 {
        return Ok(None);
    }
    let room = MAX_STORED_ISSUES.saturating_sub(s.stored_issues);
    let kept: Vec<&Issue> = issues.iter().take(room).collect();
    if !kept.is_empty() {
        let seq: Vec<i32> = (0..kept.len() as i32).map(|i| s.next_seq + i).collect();
        let row: Vec<i32> = kept.iter().map(|i| i.row as i32).collect();
        let col: Vec<Option<i32>> = kept.iter().map(|i| i.column.map(|c| c as i32)).collect();
        let field: Vec<Option<String>> = kept.iter().map(|i| i.field.clone()).collect();
        let value: Vec<Option<String>> = kept.iter().map(|i| i.value.clone()).collect();
        let code: Vec<&str> = kept.iter().map(|i| i.code.as_str()).collect();
        let message: Vec<&str> = kept.iter().map(|i| i.message.as_str()).collect();
        sqlx::query(
            "INSERT INTO cmdb.import_job_issues (job_id, seq, row_no, col_index, field, value, severity, code, message, phase)
             SELECT $1, u.seq, u.row_no, u.col, u.field, u.value, 'error', c.code, c.message, 'commit'
             FROM unnest($2::int[], $3::int[], $4::int[], $5::text[], $6::text[]) WITH ORDINALITY AS u(seq, row_no, col, field, value, n)
             JOIN unnest($7::text[], $8::text[]) WITH ORDINALITY AS c(code, message, n) USING (n)",
        )
        .bind(lease.job)
        .bind(&seq)
        .bind(&row)
        .bind(&col)
        .bind(&field)
        .bind(&value)
        .bind(&code)
        .bind(&message)
        .execute(&mut *conn)
        .await?;
    }
    Ok(Some(Recorded { stored: kept.len(), found: issues.len() as u32 }))
}

/// Ends the commit with `status` and writes its `import.commit` event, in one
/// transaction fenced by the lease: the job reaches its final status with
/// the counts of the last committed chunk. A commit that fails after a stop
/// was requested (a worker that kept dying, say) ends `cancelled`: nothing more
/// is written either way, and the stop is what the owner asked for.
async fn finish(pool: &PgPool, lease: &Lease, status: &str, error: Option<Value>) -> sqlx::Result<bool> {
    let mut tx = pool.begin().await?;
    let row: Option<JobRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "UPDATE cmdb.import_jobs SET
           status = CASE WHEN cancel_requested_at IS NOT NULL AND $4::text = 'failed' THEN 'cancelled' ELSE $4 END,
           error = CASE WHEN cancel_requested_at IS NOT NULL AND $4::text = 'failed' THEN NULL ELSE $5 END,
           finished_at = now(), expires_at = now() + interval '24 hours',
           lease_owner = NULL, lease_until = NULL
         WHERE id = $1 AND lease_owner = $2 AND lease_epoch = $3 AND status = 'committing'
         RETURNING {COLUMNS}"
    )))
    .bind(lease.job)
    .bind(&lease.owner)
    .bind(lease.epoch)
    .bind(status)
    .bind(error.map(sqlx::types::Json))
    .fetch_optional(&mut *tx)
    .await?;
    let Some(job) = row else { return Ok(false) };
    let entry = commit_event(&job);
    crud::write_audit(&mut tx, &audit_context(&job), vec![entry]).await?;
    tx.commit().await?;
    Ok(true)
}

/// Writes the `import.commit` event of a commit cancelled while still
/// `queued`: no worker holds it, so no worker would end it (§4.3). `job` is
/// the row as the cancel left it.
pub(super) async fn record_queued_cancel(conn: &mut PgConnection, job: &JobRow) -> sqlx::Result<()> {
    crud::write_audit(conn, &audit_context(job), vec![commit_event(job)]).await
}

/// The `import.commit` event (§4.3).
fn commit_event(job: &JobRow) -> AuditEntry {
    let mapping = job.mapping();
    let c = job.summary().and_then(|s| s.committed).unwrap_or_default();
    let rows = job.info().map(|i| i.row_count).unwrap_or(0);
    AuditEntry {
        action: AuditAction::ImportCommit,
        entity_type: "import_jobs",
        entity_id: job.id,
        old_value: None,
        new_value: Some(json!({
            "fileName": job.file_name,
            "fileSha256": job.file_sha256,
            "format": job.file_format,
            "classKey": job.class_key,
            "mode": mapping.as_ref().map(|m| m.mode),
            "key": mapping.as_ref().and_then(|m| m.key.clone()),
            "emptyCells": mapping.as_ref().map(|m| m.empty_cells),
            "mappingId": job.mapping_id,
            "rows": rows,
            "created": c.created,
            "updated": c.updated,
            "unchanged": c.unchanged,
            "skipped": c.skipped,
            "failed": c.failed,
            "relationshipsAdded": c.relationships_added,
            "outcome": job.status.as_str(),
            "startedAt": job.queued_at,
            "finishedAt": job.finished_at,
        })),
    }
}

/// Who the `import.commit` event names: the job's owner, via bulk import. It
/// only writes the event, so it carries no permissions; it also works when
/// the owner lost their rights or was removed.
fn audit_context(job: &JobRow) -> RequestContext {
    let principal = Principal {
        user_id: job.created_by_id.unwrap_or_default(),
        username: job.created_by_name.clone(),
        credential: Credential::Token,
        permissions: Permissions::default(),
    };
    let mut ctx = RequestContext::import_for_user(Arc::new(principal), job.id);
    ctx.actor.id = job.created_by_id.map(|id| id.to_string());
    ctx
}

/// Lets a test stop a commit as if its process died after a number of chunks
/// (nothing more is written and the lease stays until it runs out), or hold it
/// inside a chunk.
#[cfg(test)]
pub mod test_hooks {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use tokio::sync::oneshot;
    use uuid::Uuid;

    static DIE_AFTER: Mutex<Option<HashMap<Uuid, usize>>> = Mutex::new(None);

    type Pause = (usize, oneshot::Sender<()>, oneshot::Receiver<()>);
    static PAUSES: Mutex<Option<HashMap<Uuid, Pause>>> = Mutex::new(None);

    /// Holds the commit of `job` when its chunk number `chunk` (from 1) has
    /// passed the cancel check and is about to be written. The first receiver
    /// fires when it is held; sending on the second lets it go on.
    pub fn pause_at(job: Uuid, chunk: usize) -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
        let (reached_tx, reached_rx) = oneshot::channel();
        let (go_tx, go_rx) = oneshot::channel();
        PAUSES.lock().unwrap().get_or_insert_with(HashMap::new).insert(job, (chunk, reached_tx, go_rx));
        (reached_rx, go_tx)
    }

    pub(super) async fn pause(job: Uuid) {
        let held = {
            let mut guard = PAUSES.lock().unwrap();
            let Some(map) = guard.as_mut() else { return };
            let Some((left, _, _)) = map.get_mut(&job) else { return };
            *left -= 1;
            if *left > 0 {
                return;
            }
            map.remove(&job)
        };
        if let Some((_, reached, go)) = held {
            let _ = reached.send(());
            let _ = go.await;
        }
    }

    pub fn die_after(job: Uuid, chunks: usize) {
        DIE_AFTER.lock().unwrap().get_or_insert_with(HashMap::new).insert(job, chunks);
    }

    pub(super) fn dies_now(job: Uuid) -> bool {
        let mut guard = DIE_AFTER.lock().unwrap();
        let Some(map) = guard.as_mut() else { return false };
        let Some(left) = map.get_mut(&job) else { return false };
        *left -= 1;
        if *left == 0 {
            map.remove(&job);
            return true;
        }
        false
    }
}
