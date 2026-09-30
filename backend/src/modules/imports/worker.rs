//! Import workers (§3.6): each server process runs `IMPORT_WORKERS` of them
//! plus one cleanup task.
//!
//! **Claim (T13).** A worker takes a job that needs a phase, or one whose
//! lease ran out, with `FOR UPDATE SKIP LOCKED`, and in the same short
//! statement sets itself as the lease owner for 60 s and increments the
//! fencing token `lease_epoch`. The phase then runs without holding that row
//! lock; the lease is renewed every 20 s. Every write of the phase names the
//! owner and the epoch, so a worker that stalled past its lease cannot write
//! after another replica took the job over. Lease times come from the
//! database clock only.
//!
//! **Attempts (CR1).** Each claim counts. A job claimed a third time without
//! finishing its phase fails with `internal_error` and is not claimed again,
//! so one pathological file cannot crash-loop every replica.
//!
//! **Panics.** A phase runs in its own task: a panic fails the job with
//! `internal_error`, never the process.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::json;
use sqlx::PgPool;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::MAX_COLUMNS;
use super::analyse::{self, FileOptions};
use super::jobs::{self, FINAL_STATUSES, JobRow, RUNNING_STATUSES};
use super::parse::{Limits, ParseError};
use super::schemas::Phase;
use super::storage::DbFile;
use crate::config::ImportConfig;

pub const LEASE: Duration = Duration::from_secs(60);
pub const RENEW_EVERY: Duration = Duration::from_secs(20);
/// Claims of one phase before the job fails.
pub const MAX_ATTEMPTS: i32 = 3;
/// Longest analysis.
pub const ANALYSIS_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const POLL: Duration = Duration::from_secs(1);
const CLEANUP_EVERY: Duration = Duration::from_secs(10 * 60);

/// A claimed job: who holds it, with which fencing token.
#[derive(Debug, Clone)]
pub struct Lease {
    pub job: Uuid,
    pub owner: String,
    pub epoch: i32,
    pub phase: Phase,
    pub attempts: i32,
}

impl Lease {
    /// Extends the lease; false when another worker took the job or it left the phase.
    pub async fn renew(&self, pool: &PgPool) -> sqlx::Result<bool> {
        let n = sqlx::query(
            "UPDATE cmdb.import_jobs SET lease_until = now() + $4 * interval '1 second'
             WHERE id = $1 AND lease_owner = $2 AND lease_epoch = $3
               AND status IN ('analysing', 'validating', 'committing')",
        )
        .bind(self.job)
        .bind(&self.owner)
        .bind(self.epoch)
        .bind(LEASE.as_secs() as f64)
        .execute(pool)
        .await?
        .rows_affected();
        Ok(n == 1)
    }

    /// Gives the job back at shutdown, so another replica takes it at once;
    /// this claim does not count as an attempt.
    async fn release(&self, pool: &PgPool) {
        let _ = sqlx::query(
            "UPDATE cmdb.import_jobs SET lease_until = now(), attempts = greatest(attempts - 1, 0)
             WHERE id = $1 AND lease_owner = $2 AND lease_epoch = $3",
        )
        .bind(self.job)
        .bind(&self.owner)
        .bind(self.epoch)
        .execute(pool)
        .await;
    }
}

/// Takes the next job that needs a phase, or one whose lease ran out.
pub async fn claim(pool: &PgPool, owner: &str) -> sqlx::Result<Option<Lease>> {
    let row: Option<(Uuid, Phase, i32, i32)> = sqlx::query_as(
        "UPDATE cmdb.import_jobs j SET
           lease_owner = $1, lease_until = now() + $2 * interval '1 second', lease_epoch = j.lease_epoch + 1,
           attempts = j.attempts + 1,
           status = CASE j.phase WHEN 'analyse' THEN 'analysing' WHEN 'validate' THEN 'validating' ELSE 'committing' END
         WHERE j.id = (
           SELECT id FROM cmdb.import_jobs
           WHERE phase IS NOT NULL
             AND (status = 'queued' OR (status IN ('analysing', 'validating', 'committing') AND lease_until < now()))
           ORDER BY coalesce(queued_at, created_at), id
           FOR UPDATE SKIP LOCKED
           LIMIT 1)
         RETURNING j.id, j.phase, j.lease_epoch, j.attempts",
    )
    .bind(owner)
    .bind(LEASE.as_secs() as f64)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(job, phase, epoch, attempts)| Lease { job, owner: owner.to_owned(), epoch, phase, attempts }))
}

/// Ends the job with a job-level error, if this worker still holds it.
pub async fn fail(pool: &PgPool, lease: &Lease, code: &str, message: &str) -> sqlx::Result<bool> {
    let n = sqlx::query(
        "UPDATE cmdb.import_jobs SET status = 'failed', error = $4, finished_at = now(),
           expires_at = now() + interval '24 hours', lease_owner = NULL, lease_until = NULL
         WHERE id = $1 AND lease_owner = $2 AND lease_epoch = $3",
    )
    .bind(lease.job)
    .bind(&lease.owner)
    .bind(lease.epoch)
    .bind(sqlx::types::Json(json!({ "code": code, "message": message })))
    .execute(pool)
    .await?
    .rows_affected();
    Ok(n == 1)
}

pub struct Workers {
    stop: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
}

impl Workers {
    /// Stops claiming, gives running jobs back, and waits up to 10 s.
    pub async fn stop(self) {
        let _ = self.stop.send(true);
        for t in self.tasks {
            if tokio::time::timeout(Duration::from_secs(10), t).await.is_err() {
                tracing::warn!("an import worker did not stop within 10 s; its job is taken over when the lease ends");
            }
        }
    }
}

/// Starts the workers and the cleanup task of this process.
pub fn spawn(pool: PgPool, cfg: Arc<ImportConfig>) -> Workers {
    let (stop, rx) = watch::channel(false);
    let owner = Uuid::new_v4().to_string();
    let mut tasks: Vec<JoinHandle<()>> = (0..cfg.workers)
        .map(|i| tokio::spawn(run(pool.clone(), cfg.clone(), format!("{owner}/{i}"), rx.clone())))
        .collect();
    tasks.push(tokio::spawn(cleanup_loop(pool, rx)));
    Workers { stop, tasks }
}

async fn run(pool: PgPool, cfg: Arc<ImportConfig>, owner: String, mut stop: watch::Receiver<bool>) {
    loop {
        if *stop.borrow() {
            return;
        }
        match claim(&pool, &owner).await {
            Ok(Some(lease)) => work(&pool, &cfg, lease, &mut stop).await,
            Ok(None) => {
                tokio::select! {
                    _ = tokio::time::sleep(POLL) => {}
                    _ = stop.changed() => return,
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "import worker cannot claim a job; retrying");
                tokio::select! {
                    _ = tokio::time::sleep(POLL * 5) => {}
                    _ = stop.changed() => return,
                }
            }
        }
    }
}

/// Runs one claimed phase, renewing the lease; stops it when the lease is
/// lost or the process shuts down.
pub async fn work(pool: &PgPool, cfg: &Arc<ImportConfig>, lease: Lease, stop: &mut watch::Receiver<bool>) {
    if lease.attempts >= MAX_ATTEMPTS {
        tracing::warn!(job = %lease.job, attempts = lease.attempts, "import job failed repeatedly; giving up");
        let _ = fail(pool, &lease, "internal_error", "The import stopped repeatedly while processing this file.").await;
        return;
    }
    let lost = Arc::new(AtomicBool::new(false));
    let mut task = tokio::spawn(run_phase(pool.clone(), cfg.clone(), lease.clone(), lost.clone()));
    let mut renew = tokio::time::interval(RENEW_EVERY);
    renew.tick().await;
    loop {
        tokio::select! {
            done = &mut task => {
                if let Err(e) = done {
                    tracing::error!(job = %lease.job, panicked = e.is_panic(), "import phase ended abnormally");
                    let _ = fail(pool, &lease, "internal_error", "The import stopped because of an internal error.").await;
                }
                return;
            }
            _ = renew.tick() => {
                if !matches!(lease.renew(pool).await, Ok(true)) {
                    lost.store(true, Ordering::SeqCst);
                }
            }
            _ = stop.changed() => {
                lost.store(true, Ordering::SeqCst);
                let _ = tokio::time::timeout(Duration::from_secs(5), &mut task).await;
                lease.release(pool).await;
                return;
            }
        }
    }
}

async fn run_phase(pool: PgPool, cfg: Arc<ImportConfig>, lease: Lease, lost: Arc<AtomicBool>) {
    match lease.phase {
        Phase::Analyse => analyse_phase(&pool, &cfg, &lease, &lost).await,
        // The dry run and the commit arrive with the mapping (SHAA-799 part 4).
        Phase::Validate | Phase::Commit => {
            let _ = fail(&pool, &lease, "internal_error", "This server cannot run this step of the import.").await;
        }
    }
}

fn job_error(e: &ParseError) -> serde_json::Value {
    json!({ "code": e.code, "message": e.message, "row": e.row, "column": e.column })
}

/// Reads the file once and records what it holds (§1.2 step 1).
async fn analyse_phase(pool: &PgPool, cfg: &ImportConfig, lease: &Lease, lost: &Arc<AtomicBool>) {
    let job: JobRow = match jobs::fetch(
        &mut *match pool.acquire().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(job = %lease.job, error = %e, "import analysis cannot start");
                return;
            }
        },
        lease.job,
    )
    .await
    {
        Ok(Some(j)) => j,
        _ => return,
    };
    let started = Instant::now();
    let file = DbFile {
        pool: pool.clone(),
        job: job.id,
        len: job.file_size as u64,
        runtime: tokio::runtime::Handle::current(),
    };
    let format = job.format();
    let options = job.options();
    let limits = Limits { max_rows: cfg.max_rows, max_columns: MAX_COLUMNS };
    let (progress_pool, progress_lease, progress_lost) = (pool.clone(), lease.clone(), lost.clone());
    let runtime = tokio::runtime::Handle::current();
    let blocking_options = options.clone();
    let analysis = tokio::task::spawn_blocking(move || {
        let mut last = Instant::now();
        analyse::analyse(&file, format, &blocking_options, &limits, &mut |rows| {
            if progress_lost.load(Ordering::SeqCst) || started.elapsed() > ANALYSIS_TIMEOUT {
                return std::ops::ControlFlow::Break(());
            }
            if last.elapsed() >= Duration::from_secs(1) {
                last = Instant::now();
                let still_ours = runtime.block_on(write_progress(&progress_pool, &progress_lease, rows));
                if !matches!(still_ours, Ok(true)) {
                    return std::ops::ControlFlow::Break(());
                }
            }
            std::ops::ControlFlow::Continue(())
        })
    });
    let result = match analysis.await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(job = %job.id, panicked = e.is_panic(), "import analysis ended abnormally");
            let _ = fail(pool, lease, "internal_error", "The file could not be analysed because of an internal error.")
                .await;
            return;
        }
    };
    let outcome = match result {
        Ok(info) => {
            let resolved = FileOptions {
                sheet: info.sheet.clone(),
                encoding: info.encoding,
                delimiter: info.delimiter.clone(),
                has_header_row: Some(info.has_header_row),
            };
            let rows = info.row_count as i32;
            let done = sqlx::query(
                "UPDATE cmdb.import_jobs SET status = 'ready', phase = NULL, file_info = $4, file_options = $5,
                   progress_done = $6, progress_total = $6, attempts = 0, lease_owner = NULL, lease_until = NULL,
                   error = NULL, expires_at = now() + interval '24 hours'
                 WHERE id = $1 AND lease_owner = $2 AND lease_epoch = $3 AND status = 'analysing'",
            )
            .bind(job.id)
            .bind(&lease.owner)
            .bind(lease.epoch)
            .bind(sqlx::types::Json(&info))
            .bind(sqlx::types::Json(&resolved))
            .bind(rows)
            .execute(pool)
            .await;
            tracing::info!(
                job = %job.id, format = format.as_str(), size = job.file_size, sha256 = job.file_sha256.as_deref().unwrap_or(""),
                rows, columns = info.column_count, ms = started.elapsed().as_millis() as u64,
                recorded = matches!(done, Ok(ref r) if r.rows_affected() == 1), "import file analysed"
            );
            return;
        }
        Err(e) if e.code == "stopped" && started.elapsed() > ANALYSIS_TIMEOUT => {
            ParseError::new("timeout", "The file took longer than 15 minutes to analyse.")
        }
        Err(e) if e.code == "stopped" => return,
        Err(e) => e,
    };
    // Row and column only: never the cell or the file name (G4).
    tracing::info!(
        job = %job.id, format = format.as_str(), size = job.file_size, sha256 = job.file_sha256.as_deref().unwrap_or(""),
        code = outcome.code, row = outcome.row, column = outcome.column, "import file refused"
    );
    let _ = sqlx::query(
        "UPDATE cmdb.import_jobs SET status = 'failed', error = $4, finished_at = now(),
           expires_at = now() + interval '24 hours', lease_owner = NULL, lease_until = NULL
         WHERE id = $1 AND lease_owner = $2 AND lease_epoch = $3 AND status = 'analysing'",
    )
    .bind(job.id)
    .bind(&lease.owner)
    .bind(lease.epoch)
    .bind(sqlx::types::Json(job_error(&outcome)))
    .execute(pool)
    .await;
}

/// Records progress; false when the job is no longer this worker's (lease
/// lost, cancelled).
pub async fn write_progress(pool: &PgPool, lease: &Lease, done: u32) -> sqlx::Result<bool> {
    let n = sqlx::query(
        "UPDATE cmdb.import_jobs SET progress_done = $4, progress_total = greatest(progress_total, $4)
         WHERE id = $1 AND lease_owner = $2 AND lease_epoch = $3
           AND status IN ('analysing', 'validating', 'committing')",
    )
    .bind(lease.job)
    .bind(&lease.owner)
    .bind(lease.epoch)
    .bind(done as i32)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(n == 1)
}

async fn cleanup_loop(pool: PgPool, mut stop: watch::Receiver<bool>) {
    // Soon after start, then every 10 minutes.
    let mut wait = Duration::from_secs(30);
    loop {
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = stop.changed() => return,
        }
        wait = CLEANUP_EVERY;
        match cleanup(&pool, chrono::Duration::zero()).await {
            Ok(c) if c != Cleaned::default() => tracing::info!(?c, "import cleanup"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "import cleanup failed; retried in 10 minutes"),
        }
    }
}

/// What one cleanup run removed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Cleaned {
    pub stale_uploads: u64,
    pub files_of_jobs: u64,
    pub expired_jobs: u64,
    pub old_records: u64,
    pub idempotency_keys: u64,
}

/// The retention rules (§3.5, §5.5), judged at the database's `now()` plus
/// `shift` (tests move the clock forward):
/// - an upload that received nothing for 1 h is removed (CR7);
/// - 24 h after a job's last activity or end, its file and row problems are
///   deleted (T17); a job that had not ended becomes `expired`, an ended job
///   keeps its status and counts;
/// - job records go 90 days after the job ended;
/// - idempotency keys go after 24 h (T15).
pub async fn cleanup(pool: &PgPool, shift: chrono::Duration) -> sqlx::Result<Cleaned> {
    let at = |sql: &str| sql.replace("NOW", "(now() + $1)");
    let shift = sqlx::postgres::types::PgInterval::try_from(shift).unwrap_or_default();
    let mut tx = pool.begin().await?;
    let stale_uploads = sqlx::query(sqlx::AssertSqlSafe(at(
        "DELETE FROM cmdb.import_jobs WHERE status = 'uploading' AND updated_at < NOW - interval '1 hour'",
    )))
    .bind(shift)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let expiring =
        format!("SELECT id FROM cmdb.import_jobs WHERE expires_at < NOW AND status NOT IN {RUNNING_STATUSES}");
    let files_of_jobs = sqlx::query(sqlx::AssertSqlSafe(at(&format!(
        "DELETE FROM cmdb.import_job_files WHERE job_id IN ({expiring})"
    ))))
    .bind(shift)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    sqlx::query(sqlx::AssertSqlSafe(at(&format!("DELETE FROM cmdb.import_job_issues WHERE job_id IN ({expiring})"))))
        .bind(shift)
        .execute(&mut *tx)
        .await?;
    let expired_jobs = sqlx::query(sqlx::AssertSqlSafe(at(&format!(
        "UPDATE cmdb.import_jobs SET status = 'expired', phase = NULL, finished_at = coalesce(finished_at, NOW),
           lease_owner = NULL, lease_until = NULL
         WHERE expires_at < NOW AND status NOT IN {RUNNING_STATUSES} AND status NOT IN {FINAL_STATUSES}"
    ))))
    .bind(shift)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let old_records = sqlx::query(sqlx::AssertSqlSafe(at(&format!(
        "DELETE FROM cmdb.import_jobs WHERE status IN {FINAL_STATUSES} AND finished_at < NOW - interval '90 days'"
    ))))
    .bind(shift)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let idempotency_keys = sqlx::query(sqlx::AssertSqlSafe(at(
        "DELETE FROM cmdb.import_idempotency_keys WHERE created_at < NOW - interval '24 hours'",
    )))
    .bind(shift)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(Cleaned { stale_uploads, files_of_jobs, expired_jobs, old_records, idempotency_keys })
}
