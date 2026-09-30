//! The dry run (§1.2 step 3, §2.1): every row through the same pipeline as
//! the commit, read-only.
//!
//! 1. The first pass reads the file and keeps only each row's keys (no
//!    database).
//! 2. [`planner::whole`] loads the CIs those keys match, the rows that share
//!    a key, and the CIs the file creates, so later rows can refer to them.
//! 3. The second pass plans the rows in chunks of [`CHUNK_ROWS`], with the
//!    owner's permissions as their profiles give them at each chunk (§3.6).
//!
//! It stores the counts, a preview of at most [`PREVIEW_ROWS`] rows, the
//! first [`MAX_STORED_ISSUES`] problems and the data-model fingerprint that
//! the commit compares (T14). Every write names the lease (T13).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::analyse::{self, FileFormat, FileOptions};
use super::jobs::{self, JobRow};
use super::mapping;
use super::parse::{Limits, ParseError, Row};
use super::planner::{self, Context, Issue, JobData, Matches, Severity};
use super::schemas::{ImportSummary, PlannedRow, RowOutcome};
use super::storage::DbFile;
use super::worker::{Lease, fail, write_progress};
use super::{MAX_COLUMNS, settings};
use crate::api::context::RequestContext;
use crate::auth::permissions::GlobalPermission;
use crate::auth::{Credential, Principal};
use crate::config::ImportConfig;
use crate::data::auth as auth_data;

/// Rows planned together (the commit's chunk size, §2.6).
pub const CHUNK_ROWS: usize = 500;
/// Planned rows kept as the preview (§3.1).
pub const PREVIEW_ROWS: usize = 50;
/// Problems stored per job; more are counted (§3.5).
pub const MAX_STORED_ISSUES: usize = 10_000;

/// A job-level stop: `(code, message)` for `import_jobs.error`.
type Stop = (&'static str, String);

fn internal() -> Stop {
    ("internal_error", "The dry run stopped because of an internal error.".into())
}

/// The owner's context as their profiles give it now (§3.6, T22), or why the
/// job must stop: owner removed or deactivated, `cis.import` withdrawn, or
/// the switch turned off.
pub async fn owner_context(
    conn: &mut PgConnection,
    cfg: &ImportConfig,
    owner: Option<Uuid>,
    job: Uuid,
) -> Result<RequestContext, Stop> {
    let revoked = || ("permission_revoked", "The owner of this import may no longer import CIs.".to_owned());
    let owner = owner.ok_or_else(revoked)?;
    let user: Option<(String, bool)> = sqlx::query_as("SELECT username, is_active FROM cmdb.users WHERE id = $1")
        .bind(owner)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|_| internal())?;
    let Some((username, true)) = user else { return Err(revoked()) };
    let permissions = auth_data::load_permissions(conn, owner).await.map_err(|_| internal())?;
    if !permissions.has(GlobalPermission::CisImport) {
        return Err(revoked());
    }
    if !(cfg.allowed && settings::stored_enabled(conn).await.map_err(|_| internal())?) {
        return Err(("import_disabled", "Bulk import was turned off for this instance.".into()));
    }
    let principal = Principal { user_id: owner, username, credential: Credential::Token, permissions };
    Ok(RequestContext::import_for_user(Arc::new(principal), job))
}

/// SHA-256 over `(table, id, updated_at)` of every row that defines the data
/// model (T14). Each of these tables has an `updated_at` trigger (0002, 0004).
pub async fn model_fingerprint(conn: &mut PgConnection) -> sqlx::Result<String> {
    let rows: Vec<(String, Uuid, String)> = sqlx::query_as(
        "SELECT t, id, updated_at::text FROM (
           SELECT 'ci_classes' AS t, id, updated_at FROM cmdb.ci_classes
           UNION ALL SELECT 'ci_attribute_definitions', id, updated_at FROM cmdb.ci_attribute_definitions
           UNION ALL SELECT 'relationship_types', id, updated_at FROM cmdb.relationship_types
           UNION ALL SELECT 'relationship_type_rules', id, updated_at FROM cmdb.relationship_type_rules
           UNION ALL SELECT 'lookup_lists', id, updated_at FROM cmdb.lookup_lists
           UNION ALL SELECT 'lookup_list_values', id, updated_at FROM cmdb.lookup_list_values
         ) m ORDER BY t, id",
    )
    .fetch_all(conn)
    .await?;
    let mut h = Sha256::new();
    for (t, id, at) in rows {
        h.update(t.as_bytes());
        h.update([0]);
        h.update(id.as_bytes());
        h.update(at.as_bytes());
        h.update([0]);
    }
    Ok(hex::encode(h.finalize()))
}

/// Reads the stored file on a blocking thread and hands its rows over in
/// chunks. Stops when `lost` is set or the receiver is gone.
pub fn stream_rows(
    file: DbFile,
    format: FileFormat,
    options: FileOptions,
    limits: Limits,
    lost: Arc<AtomicBool>,
) -> (mpsc::Receiver<Vec<Row>>, tokio::task::JoinHandle<Result<(), ParseError>>) {
    let (tx, rx) = mpsc::channel::<Vec<Row>>(2);
    let reader = tokio::task::spawn_blocking(move || {
        let mut chunk: Vec<Row> = Vec::with_capacity(CHUNK_ROWS);
        let mut closed = false;
        analyse::read_file(&file, format, &options, &limits, &mut |row| {
            if lost.load(Ordering::SeqCst) {
                return std::ops::ControlFlow::Break(());
            }
            chunk.push(row);
            if chunk.len() == CHUNK_ROWS && tx.blocking_send(std::mem::take(&mut chunk)).is_err() {
                closed = true;
                return std::ops::ControlFlow::Break(());
            }
            std::ops::ControlFlow::Continue(())
        })?;
        if !closed && !chunk.is_empty() {
            let _ = tx.blocking_send(chunk);
        }
        Ok(())
    });
    (rx, reader)
}

/// What the second pass adds up.
#[derive(Default)]
struct Tally {
    summary: ImportSummary,
    preview: Vec<PlannedRow>,
    issues: Vec<Issue>,
}

impl Tally {
    fn issue(&mut self, i: Issue) {
        match i.severity {
            Severity::Warning => self.summary.warnings += 1,
            Severity::Error => {}
        }
        self.summary.issues_total += 1;
        if self.issues.len() < MAX_STORED_ISSUES {
            self.issues.push(i);
        }
    }
}

pub async fn run(pool: &PgPool, cfg: &ImportConfig, lease: &Lease, lost: &Arc<AtomicBool>) {
    match validate(pool, cfg, lease, lost).await {
        Ok(()) => {}
        Err((code @ "mapping_invalid", message)) => {
            // The model changed since the mapping was set: back to `ready`, so it can be fixed.
            let _ = sqlx::query(
                "UPDATE cmdb.import_jobs SET status = 'ready', phase = NULL, error = $4, lease_owner = NULL,
                   lease_until = NULL, attempts = 0
                 WHERE id = $1 AND lease_owner = $2 AND lease_epoch = $3 AND status = 'validating'",
            )
            .bind(lease.job)
            .bind(&lease.owner)
            .bind(lease.epoch)
            .bind(sqlx::types::Json(serde_json::json!({ "code": code, "message": message })))
            .execute(pool)
            .await;
        }
        Err(("stopped", _)) => {}
        Err((code, message)) => {
            tracing::info!(job = %lease.job, code, "import dry run stopped");
            let _ = fail(pool, lease, code, &message).await;
        }
    }
}

async fn validate(pool: &PgPool, cfg: &ImportConfig, lease: &Lease, lost: &Arc<AtomicBool>) -> Result<(), Stop> {
    let started = std::time::Instant::now();
    let mut conn = pool.acquire().await.map_err(|_| internal())?;
    let job: JobRow =
        jobs::fetch(&mut conn, lease.job).await.map_err(|_| internal())?.ok_or(("stopped", String::new()))?;
    let mapping = job.mapping().ok_or(("mapping_invalid", "Set the mapping before the dry run.".to_owned()))?;
    let info = job.info().ok_or_else(internal)?;
    let headers: Vec<String> = info.columns.iter().map(|c| c.header.clone()).collect();

    let ctx = owner_context(&mut conn, cfg, job.created_by_id, job.id).await?;
    let resolved = mapping::resolve(&mut conn, &ctx, &mapping, &headers).await.map_err(|e| {
        let detail = e
            .details
            .as_ref()
            .and_then(|d| d.first())
            .map(|d| format!(" ({}: {})", d.field, d.message))
            .unwrap_or_default();
        ("mapping_invalid", format!("The mapping no longer fits the data model{detail}. Check it and save it again."))
    })?;
    let fingerprint = model_fingerprint(&mut conn).await.map_err(|_| internal())?;
    let data = JobData::load(&mut conn, resolved).await.map_err(|_| internal())?;
    drop(conn);

    let file = || DbFile {
        pool: pool.clone(),
        job: job.id,
        len: job.file_size as u64,
        runtime: tokio::runtime::Handle::current(),
    };
    let limits = || Limits { max_rows: cfg.max_rows, max_columns: MAX_COLUMNS };
    let stopped = || -> Stop { ("stopped", String::new()) };
    let parse_failed = |e: ParseError| -> Stop {
        if e.code == "stopped" {
            stopped()
        } else {
            ("file_changed", format!("The file could not be read again: {}", e.message))
        }
    };

    // Pass 1: the keys of every row.
    let (mut rx, reader) = stream_rows(file(), job.format(), job.options(), limits(), lost.clone());
    let mut keys = Vec::new();
    while let Some(rows) = rx.recv().await {
        keys.extend(planner::keys_of(&data, &rows));
    }
    reader.await.map_err(|_| internal())?.map_err(parse_failed)?;
    if lost.load(Ordering::SeqCst) {
        return Err(stopped());
    }

    let mut conn = pool.acquire().await.map_err(|_| internal())?;
    let whole = planner::whole(&mut conn, &data, &ctx, &keys).await.map_err(|_| internal())?;
    drop(keys);
    let planner::Whole { index, duplicates, new_ids, mut pending, matches_deleted } = whole;

    // Pass 2: plan every row.
    let mut tally = Tally::default();
    let mut done = 0u32;
    let (mut rx, reader) = stream_rows(file(), job.format(), job.options(), limits(), lost.clone());
    while let Some(rows) = rx.recv().await {
        let ctx = owner_context(&mut conn, cfg, job.created_by_id, job.id).await?;
        let mut c = Context {
            job: &data,
            ctx: &ctx,
            matches: Matches::Index(&index),
            pending: &mut pending,
            duplicates: Some(&duplicates),
            new_ids: Some(&new_ids),
            lock: false,
            grow_pending: false,
        };
        let plan = planner::plan_chunk(&mut conn, &mut c, &rows).await.map_err(|e| {
            tracing::warn!(job = %job.id, error = %e.message, "import dry run: planning failed");
            internal()
        })?;
        for p in plan.rows {
            match p.outcome {
                RowOutcome::Create => tally.summary.create += 1,
                RowOutcome::Update => tally.summary.update += 1,
                RowOutcome::Unchanged => tally.summary.unchanged += 1,
                RowOutcome::Error => tally.summary.error_rows += 1,
            }
            if p.outcome != RowOutcome::Error {
                tally.summary.relationships_to_add += p.edges.len() as u32;
            }
            if matches!(p.outcome, RowOutcome::Create | RowOutcome::Update) && tally.preview.len() < PREVIEW_ROWS {
                tally.preview.push(p.preview());
            }
            if matches_deleted.contains(&p.number) {
                tally.issue(Issue {
                    row: p.number,
                    column: None,
                    field: None,
                    value: None,
                    severity: Severity::Warning,
                    code: "matches_deleted".into(),
                    message: "A deleted CI has this key. Restore it instead if it is the same asset.".into(),
                });
            }
            for i in p.issues {
                tally.issue(i);
            }
        }
        done += rows.len() as u32;
        if !matches!(write_progress(pool, lease, done).await, Ok(true)) {
            lost.store(true, Ordering::SeqCst);
            return Err(stopped());
        }
    }
    reader.await.map_err(|_| internal())?.map_err(parse_failed)?;
    if lost.load(Ordering::SeqCst) {
        return Err(stopped());
    }
    if let Some(i) = duplicate_file(&mut conn, &job, &mapping.class_key).await.map_err(|_| internal())? {
        tally.issue(i);
    }
    let recorded = finish(&mut conn, lease, &tally, &fingerprint).await.map_err(|e| {
        tracing::warn!(job = %job.id, error = %e, "import dry run: result not stored");
        internal()
    })?;
    tracing::info!(
        job = %job.id, rows = done, create = tally.summary.create, update = tally.summary.update,
        unchanged = tally.summary.unchanged, error_rows = tally.summary.error_rows,
        issues = tally.summary.issues_total, ms = started.elapsed().as_millis() as u64, recorded, "import dry run finished"
    );
    Ok(())
}

/// `duplicate_file`: the same user imported the same file into the same
/// class in the last 24 h (§3.6).
async fn duplicate_file(conn: &mut PgConnection, job: &JobRow, class_key: &str) -> sqlx::Result<Option<Issue>> {
    let Some(sha) = &job.file_sha256 else { return Ok(None) };
    let hit: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM cmdb.import_jobs
           WHERE created_by_id = $1 AND id <> $2 AND file_sha256 = $3 AND class_key = $4
             AND status IN ('completed', 'completed_with_errors') AND finished_at > now() - interval '24 hours')",
    )
    .bind(job.created_by_id)
    .bind(job.id)
    .bind(sha)
    .bind(class_key)
    .fetch_one(conn)
    .await?;
    Ok(hit.then(|| Issue {
        row: 1,
        column: None,
        field: None,
        value: None,
        severity: Severity::Warning,
        code: "duplicate_file".into(),
        message: "You imported this file into this class in the last 24 hours.".into(),
    }))
}

/// Stores the result in one transaction, fenced by the lease; false when the
/// job is no longer this worker's.
async fn finish(conn: &mut PgConnection, lease: &Lease, tally: &Tally, fingerprint: &str) -> sqlx::Result<bool> {
    let mut tx = sqlx::Connection::begin(&mut *conn).await?;
    let n = sqlx::query(
        "UPDATE cmdb.import_jobs SET status = 'validated', phase = NULL, summary = $4, preview = $5,
           model_fingerprint = $6, dry_run_finished_at = now(), error = NULL, attempts = 0,
           lease_owner = NULL, lease_until = NULL, expires_at = now() + interval '24 hours'
         WHERE id = $1 AND lease_owner = $2 AND lease_epoch = $3 AND status = 'validating'",
    )
    .bind(lease.job)
    .bind(&lease.owner)
    .bind(lease.epoch)
    .bind(sqlx::types::Json(&tally.summary))
    .bind(sqlx::types::Json(&tally.preview))
    .bind(fingerprint)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if n != 1 {
        return Ok(false);
    }
    sqlx::query("DELETE FROM cmdb.import_job_issues WHERE job_id = $1").bind(lease.job).execute(&mut *tx).await?;
    for (start, batch) in tally.issues.chunks(1_000).enumerate().map(|(i, b)| (i * 1_000, b)) {
        let seq: Vec<i32> = (0..batch.len()).map(|i| (start + i) as i32).collect();
        let row: Vec<i32> = batch.iter().map(|i| i.row as i32).collect();
        let col: Vec<Option<i32>> = batch.iter().map(|i| i.column.map(|c| c as i32)).collect();
        let field: Vec<Option<String>> = batch.iter().map(|i| i.field.clone()).collect();
        let value: Vec<Option<String>> = batch.iter().map(|i| i.value.clone()).collect();
        let severity: Vec<&str> = batch.iter().map(|i| i.severity.as_str()).collect();
        let code: Vec<&str> = batch.iter().map(|i| i.code.as_str()).collect();
        let message: Vec<&str> = batch.iter().map(|i| i.message.as_str()).collect();
        sqlx::query(
            "INSERT INTO cmdb.import_job_issues (job_id, seq, row_no, col_index, field, value, severity, code, message, phase)
             SELECT $1, * , 'validate' FROM unnest($2::int[], $3::int[], $4::int[], $5::text[], $6::text[], $7::text[], $8::text[], $9::text[])",
        )
        .bind(lease.job)
        .bind(&seq)
        .bind(&row)
        .bind(&col)
        .bind(&field)
        .bind(&value)
        .bind(&severity)
        .bind(&code)
        .bind(&message)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(true)
}
