//! Import jobs: the stored record, what callers see of it, and the operations
//! that need no worker (read, list, cancel, delete, file options).
//!
//! A job is visible to its owner and to administrators only; anyone else gets
//! the same `404` as for a job that does not exist (§3 guard 4).

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::analyse::{FileFormat, FileInfo, FileOptions};
use super::schemas::{
    DryRunInfo, ImportFile, ImportIssue, ImportJob, ImportJobSummary, ImportMapping, ImportSummary, IssueSeverity,
    JobError, JobOwner, JobStatus, ListImportIssuesQuery, ListImportsQuery, Phase, PlannedRow, Progress, StaleReason,
};
use super::{coded, mapping, settings};
use crate::api::context::RequestContext;
use crate::api::schemas::{Page, Paged};
use crate::config::ImportConfig;
use crate::http::error::{AppError, ErrorCode};

/// A job row as stored.
#[derive(Debug, Clone, sqlx::FromRow)]
// The lease columns are read only by the workers' own queries.
#[allow(dead_code)]
pub struct JobRow {
    pub id: Uuid,
    pub created_by_id: Option<Uuid>,
    pub created_by_name: String,
    pub status: JobStatus,
    pub phase: Option<Phase>,
    pub file_name: String,
    pub file_format: String,
    pub file_size: i64,
    pub file_sha256: Option<String>,
    pub file_options: sqlx::types::Json<Value>,
    pub file_info: Option<sqlx::types::Json<Value>>,
    pub class_key: Option<String>,
    pub mapping: Option<sqlx::types::Json<Value>>,
    pub mapping_id: Option<Uuid>,
    pub summary: Option<sqlx::types::Json<Value>>,
    pub preview: Option<sqlx::types::Json<Value>>,
    pub model_fingerprint: Option<String>,
    pub dry_run_finished_at: Option<DateTime<Utc>>,
    pub committed_through_row: i32,
    pub attempts: i32,
    pub progress_done: i32,
    pub progress_total: i32,
    pub error: Option<sqlx::types::Json<Value>>,
    pub queued_at: Option<DateTime<Utc>>,
    pub lease_owner: Option<String>,
    pub lease_until: Option<DateTime<Utc>>,
    pub lease_epoch: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub expires_at: DateTime<Utc>,
}

pub const COLUMNS: &str = "id, created_by_id, created_by_name, status, phase, file_name, file_format, file_size, \
    file_sha256, file_options, file_info, class_key, mapping, mapping_id, summary, preview, model_fingerprint, \
    dry_run_finished_at, committed_through_row, attempts, progress_done, progress_total, error, queued_at, \
    lease_owner, lease_until, lease_epoch, created_at, updated_at, finished_at, expires_at";

/// SQL list of the statuses in which nothing happens any more.
pub const FINAL_STATUSES: &str = "('completed', 'completed_with_errors', 'failed', 'cancelled', 'expired')";
/// SQL list of the statuses in which a worker phase runs or waits, or the file arrives.
pub const RUNNING_STATUSES: &str = "('uploading', 'queued', 'analysing', 'validating', 'committing')";

impl JobRow {
    pub fn format(&self) -> FileFormat {
        if self.file_format == "xlsx" { FileFormat::Xlsx } else { FileFormat::Csv }
    }

    pub fn options(&self) -> FileOptions {
        serde_json::from_value(self.file_options.0.clone()).unwrap_or_default()
    }

    pub fn info(&self) -> Option<FileInfo> {
        self.file_info.as_ref().and_then(|v| serde_json::from_value(v.0.clone()).ok())
    }

    pub fn mapping(&self) -> Option<ImportMapping> {
        self.mapping.as_ref().and_then(|v| serde_json::from_value(v.0.clone()).ok())
    }

    pub fn summary(&self) -> Option<ImportSummary> {
        self.summary.as_ref().and_then(|v| serde_json::from_value(v.0.clone()).ok())
    }

    pub fn error(&self) -> Option<JobError> {
        self.error.as_ref().and_then(|v| serde_json::from_value(v.0.clone()).ok())
    }

    fn progress(&self, queue_position: Option<i64>) -> Progress {
        Progress {
            done: self.progress_done,
            total: self.progress_total,
            queue_position,
            started_at: self.queued_at,
            updated_at: self.updated_at,
        }
    }

    fn owner(&self) -> JobOwner {
        JobOwner { id: self.created_by_id, name: self.created_by_name.clone() }
    }

    /// The dry run's state, stale after 24 h (the model fingerprint is judged at commit).
    pub fn dry_run(&self, model_changed: bool) -> Option<DryRunInfo> {
        let finished_at = self.dry_run_finished_at?;
        let reason = if model_changed {
            Some(StaleReason::ModelChanged)
        } else if Utc::now() - finished_at > chrono::Duration::hours(24) {
            Some(StaleReason::Expired)
        } else {
            None
        };
        Some(DryRunInfo { finished_at, stale: reason.is_some(), stale_reason: reason })
    }

    pub fn dto(&self, queue_position: Option<i64>) -> ImportJob {
        self.dto_with(queue_position, false)
    }

    /// The job as callers see it; `model_changed` marks its dry run stale (T14).
    pub fn dto_with(&self, queue_position: Option<i64>, model_changed: bool) -> ImportJob {
        let info = self.info();
        let options = self.options();
        let file = ImportFile {
            name: self.file_name.clone(),
            format: self.format(),
            size: self.file_size,
            sha256: self.file_sha256.clone(),
            sheets: info.as_ref().map(|i| i.sheets.clone()).unwrap_or_default(),
            hidden_sheets: info.as_ref().map(|i| i.hidden_sheets.clone()).unwrap_or_default(),
            sheet: info.as_ref().and_then(|i| i.sheet.clone()),
            encoding: info.as_ref().and_then(|i| i.encoding),
            delimiter: info.as_ref().and_then(|i| i.delimiter.clone()),
            has_header_row: info.as_ref().map(|i| i.has_header_row).unwrap_or(options.has_header_row()),
            row_count: info.as_ref().map(|i| i.row_count),
            column_count: info.as_ref().map(|i| i.column_count),
            preview_rows: info.as_ref().map(|i| i.preview_rows.clone()).unwrap_or_default(),
        };
        let preview: Vec<PlannedRow> =
            self.preview.as_ref().and_then(|v| serde_json::from_value(v.0.clone()).ok()).unwrap_or_default();
        ImportJob {
            id: self.id,
            status: self.status,
            phase: self.phase,
            file,
            columns: info.map(|i| i.columns).unwrap_or_default(),
            class_key: self.class_key.clone(),
            mapping: self.mapping(),
            mapping_id: self.mapping_id,
            progress: self.progress(queue_position),
            summary: self.summary(),
            preview,
            dry_run: self.dry_run(model_changed),
            error: self.error(),
            created_at: self.created_at,
            created_by: self.owner(),
            expires_at: self.expires_at,
            finished_at: self.finished_at,
        }
    }

    pub fn summary_dto(&self) -> ImportJobSummary {
        ImportJobSummary {
            id: self.id,
            status: self.status,
            phase: self.phase,
            file_name: self.file_name.clone(),
            file_format: self.format(),
            file_size: self.file_size,
            row_count: self.info().map(|i| i.row_count),
            class_key: self.class_key.clone(),
            progress: self.progress(None),
            summary: self.summary(),
            error: self.error(),
            created_at: self.created_at,
            created_by: self.owner(),
            expires_at: self.expires_at,
            finished_at: self.finished_at,
        }
    }
}

pub async fn fetch(conn: &mut PgConnection, id: Uuid) -> sqlx::Result<Option<JobRow>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {COLUMNS} FROM cmdb.import_jobs WHERE id = $1")))
        .bind(id)
        .fetch_optional(conn)
        .await
}

pub async fn fetch_for_update(conn: &mut PgConnection, id: Uuid) -> sqlx::Result<Option<JobRow>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {COLUMNS} FROM cmdb.import_jobs WHERE id = $1 FOR UPDATE")))
        .bind(id)
        .fetch_optional(conn)
        .await
}

fn not_found(id: Uuid) -> AppError {
    AppError::not_found(format!("Import {id} does not exist or belongs to another user"))
}

fn is_administrator(ctx: &RequestContext) -> bool {
    ctx.principal().is_some_and(|p| p.permissions.administrator)
}

/// The caller's own job, or any job for an administrator; `404` otherwise.
pub fn check_owner(ctx: &RequestContext, job: Option<JobRow>, id: Uuid) -> Result<JobRow, AppError> {
    match job {
        Some(j) if is_administrator(ctx) || (j.created_by_id.is_some() && j.created_by_id == ctx_user(ctx)) => Ok(j),
        _ => Err(not_found(id)),
    }
}

pub fn ctx_user(ctx: &RequestContext) -> Option<Uuid> {
    ctx.principal().map(|p| p.user_id)
}

/// `403 import_disabled` unless the switch is on and the server allows import.
pub async fn require_enabled(conn: &mut PgConnection, cfg: &ImportConfig) -> Result<(), AppError> {
    if cfg.allowed && settings::stored_enabled(conn).await? {
        Ok(())
    } else {
        Err(coded(ErrorCode::Forbidden, "Bulk import is turned off for this instance.", "import_disabled"))
    }
}

/// Jobs waiting ahead of a queued one.
pub async fn queue_position(conn: &mut PgConnection, job: &JobRow) -> sqlx::Result<Option<i64>> {
    if job.status != JobStatus::Queued {
        return Ok(None);
    }
    let ahead: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM cmdb.import_jobs WHERE status = 'queued' AND queued_at < $1 AND id <> $2",
    )
    .bind(job.queued_at)
    .bind(job.id)
    .fetch_one(conn)
    .await?;
    Ok(Some(ahead))
}

pub async fn get(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<ImportJob, AppError> {
    let mut conn = pool.acquire().await?;
    let job = check_owner(ctx, fetch(&mut conn, id).await?, id)?;
    let position = queue_position(&mut conn, &job).await?;
    // A validated job's dry run goes stale when the data model changes: the
    // fingerprint takes milliseconds, so the poll can say so (T14).
    let model_changed = job.status == JobStatus::Validated
        && job.model_fingerprint.as_deref() != Some(super::dry_run::model_fingerprint(&mut conn).await?.as_str());
    Ok(job.dto_with(position, model_changed))
}

pub async fn list(
    pool: &PgPool,
    ctx: &RequestContext,
    q: &ListImportsQuery,
) -> Result<Page<ImportJobSummary>, AppError> {
    let all = q.all.is_some_and(bool::from);
    if all {
        ctx.require_administrator("list every user's imports")?;
    }
    let owner = if all { None } else { ctx_user(ctx) };
    let mut conn = pool.acquire().await?;
    let filter = "($1::uuid IS NULL OR created_by_id = $1) AND ($2::text IS NULL OR status = $2) AND ($3 OR $1::uuid IS NOT NULL)";
    let status = q.status.map(|s| s.as_str());
    let total: i64 =
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM cmdb.import_jobs WHERE {filter}")))
            .bind(owner)
            .bind(status)
            .bind(all)
            .fetch_one(&mut *conn)
            .await?;
    let rows: Vec<JobRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM cmdb.import_jobs WHERE {filter} ORDER BY created_at DESC, id LIMIT $4 OFFSET $5"
    )))
    .bind(owner)
    .bind(status)
    .bind(all)
    .bind(q.limit)
    .bind(q.offset)
    .fetch_all(&mut *conn)
    .await?;
    Ok(Page { data: rows.iter().map(JobRow::summary_dto).collect(), page: q.page_meta(total) })
}

pub(crate) fn invalid_state(message: &str) -> AppError {
    coded(ErrorCode::Conflict, message, "invalid_state")
}

/// Stops a job: analysis and dry run at once, a commit after its current
/// chunk (the worker sees the status at the chunk boundary). Allowed while the
/// switch is off, so running jobs can be stopped (W3).
pub async fn cancel(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<ImportJob, AppError> {
    let mut tx = pool.begin().await?;
    let job = check_owner(ctx, fetch_for_update(&mut tx, id).await?, id)?;
    if job.status.is_final() {
        return Err(invalid_state("This import has already ended."));
    }
    if job.status == JobStatus::Uploading {
        return Err(invalid_state("The file is still uploading; stop the upload instead."));
    }
    let row: JobRow = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "UPDATE cmdb.import_jobs SET status = 'cancelled', finished_at = now(), expires_at = now() + interval '24 hours',
           lease_owner = CASE WHEN status = 'committing' THEN lease_owner END,
           lease_until = CASE WHEN status = 'committing' THEN lease_until END
         WHERE id = $1 RETURNING {COLUMNS}"
    )))
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if job.status == JobStatus::Queued && job.phase == Some(Phase::Commit) {
        super::commit::record_queued_cancel(&mut tx, &row).await?;
    }
    tx.commit().await?;
    Ok(row.dto(None))
}

/// Removes a job with its file and row problems. CIs it imported stay. Allowed
/// while the switch is off, so uploaded files can be removed (W3).
pub async fn delete(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    let job = check_owner(ctx, fetch_for_update(&mut tx, id).await?, id)?;
    if job.status.is_running() {
        return Err(invalid_state("This import is still running. Cancel it first."));
    }
    sqlx::query("DELETE FROM cmdb.import_jobs WHERE id = $1").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Changes how the file is read and analyses it again. The mapping and any
/// dry run are dropped, because the columns may change. In `ready`, or after
/// an analysis that failed (for example with the wrong sheet or encoding).
pub async fn update_file_options(
    pool: &PgPool,
    ctx: &RequestContext,
    cfg: &ImportConfig,
    id: Uuid,
    input: &FileOptions,
) -> Result<ImportJob, AppError> {
    let mut tx = pool.begin().await?;
    require_enabled(&mut tx, cfg).await?;
    let job = check_owner(ctx, fetch_for_update(&mut tx, id).await?, id)?;
    let analysis_failed = job.status == JobStatus::Failed && job.phase == Some(Phase::Analyse);
    if job.status != JobStatus::Ready && !analysis_failed {
        return Err(invalid_state("The file options can be changed only after the file was analysed."));
    }
    let mut errors = Vec::new();
    if job.format() == FileFormat::Xlsx && (input.encoding.is_some() || input.delimiter.is_some()) {
        errors.push(super::body_field("encoding", "Only CSV files have an encoding and a delimiter", "not_applicable"));
    }
    if job.format() == FileFormat::Csv && input.sheet.is_some() {
        errors.push(super::body_field("sheet", "Only workbooks have sheets", "not_applicable"));
    }
    if input.delimiter.is_some() && input.delimiter_byte().is_none() {
        errors.push(super::body_field("delimiter", "Must be one of , ; tab |", "invalid_enum"));
    }
    if let (Some(sheet), Some(info)) = (&input.sheet, job.info())
        && !info.sheets.contains(sheet)
    {
        errors.push(super::body_field("sheet", "The workbook has no worksheet of this name", "not_found"));
    }
    if !errors.is_empty() {
        return Err(AppError::validation(errors));
    }
    let mut options = job.options();
    if input.sheet.is_some() {
        options.sheet = input.sheet.clone();
    }
    if input.encoding.is_some() {
        options.encoding = input.encoding;
    }
    if input.delimiter.is_some() {
        options.delimiter = input.delimiter.clone();
    }
    if input.has_header_row.is_some() {
        options.has_header_row = input.has_header_row;
    }
    let row: JobRow = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "UPDATE cmdb.import_jobs SET status = 'queued', phase = 'analyse', file_options = $2, file_info = NULL,
           class_key = NULL, mapping = NULL, mapping_id = NULL, summary = NULL, preview = NULL, model_fingerprint = NULL,
           dry_run_finished_at = NULL, error = NULL, attempts = 0, progress_done = 0, progress_total = 0,
           queued_at = now(), finished_at = NULL, expires_at = now() + interval '24 hours'
         WHERE id = $1 RETURNING {COLUMNS}"
    )))
    .bind(id)
    .bind(sqlx::types::Json(&options))
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM cmdb.import_job_issues WHERE job_id = $1").bind(id).execute(&mut *tx).await?;
    let position = queue_position(&mut tx, &row).await?;
    tx.commit().await?;
    Ok(row.dto(position))
}

/// Sets the mapping after checking it against the file's columns and the
/// data model (`PUT /imports/{id}/mapping`). Any dry run is dropped and the
/// job is `ready` again. In `ready` or `validated`; `409` while a phase runs.
pub async fn set_mapping(
    pool: &PgPool,
    ctx: &RequestContext,
    cfg: &ImportConfig,
    id: Uuid,
    mapping: &ImportMapping,
) -> Result<ImportJob, AppError> {
    let mut tx = pool.begin().await?;
    require_enabled(&mut tx, cfg).await?;
    let job = check_owner(ctx, fetch_for_update(&mut tx, id).await?, id)?;
    if !matches!(job.status, JobStatus::Ready | JobStatus::Validated) {
        return Err(invalid_state("The mapping can be set only after the file was analysed and while no step runs."));
    }
    let headers: Vec<String> =
        job.info().map(|i| i.columns.into_iter().map(|c| c.header).collect()).unwrap_or_default();
    mapping::resolve(&mut tx, ctx, mapping, &headers).await?;
    let row: JobRow = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "UPDATE cmdb.import_jobs SET status = 'ready', phase = NULL, class_key = $2, mapping = $3, summary = NULL,
           preview = NULL, model_fingerprint = NULL, dry_run_finished_at = NULL, error = NULL,
           expires_at = now() + interval '24 hours'
         WHERE id = $1 RETURNING {COLUMNS}"
    )))
    .bind(id)
    .bind(&mapping.class_key)
    .bind(sqlx::types::Json(mapping))
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM cmdb.import_job_issues WHERE job_id = $1").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(row.dto(None))
}

/// Queues the dry run (`POST /imports/{id}/dry-run`): `409` without a mapping
/// or while a phase runs, `429 import_busy` while another job of the user
/// runs (T19). The mapping is checked again, since the model may have changed.
pub async fn start_dry_run(
    pool: &PgPool,
    ctx: &RequestContext,
    cfg: &ImportConfig,
    id: Uuid,
) -> Result<ImportJob, AppError> {
    let mut tx = pool.begin().await?;
    require_enabled(&mut tx, cfg).await?;
    let job = check_owner(ctx, fetch_for_update(&mut tx, id).await?, id)?;
    if !matches!(job.status, JobStatus::Ready | JobStatus::Validated) {
        return Err(invalid_state("A dry run can start only after the file was analysed and while no step runs."));
    }
    let Some(mapping) = job.mapping() else {
        return Err(coded(ErrorCode::Conflict, "Set the mapping before the dry run.", "mapping_required"));
    };
    let owner = job.created_by_id.ok_or_else(AppError::internal)?;
    super::upload::lock_user(&mut tx, owner).await?;
    let headers: Vec<String> =
        job.info().map(|i| i.columns.into_iter().map(|c| c.header).collect()).unwrap_or_default();
    mapping::resolve(&mut tx, ctx, &mapping, &headers).await?;
    let row: JobRow = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "UPDATE cmdb.import_jobs SET status = 'queued', phase = 'validate', summary = NULL, preview = NULL,
           model_fingerprint = NULL, dry_run_finished_at = NULL, error = NULL, attempts = 0, progress_done = 0,
           progress_total = coalesce((file_info->>'rowCount')::int, 0), queued_at = now(),
           expires_at = now() + interval '24 hours'
         WHERE id = $1 RETURNING {COLUMNS}"
    )))
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM cmdb.import_job_issues WHERE job_id = $1").bind(id).execute(&mut *tx).await?;
    let position = queue_position(&mut tx, &row).await?;
    tx.commit().await?;
    Ok(row.dto(position))
}

/// The problems of the last dry run and of the commit, in row order
/// (`GET /imports/{id}/issues`). Empty once the file expired (T17). Readable while
/// import is off, like the job itself (W3).
pub async fn issues(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    q: &ListImportIssuesQuery,
) -> Result<Page<ImportIssue>, AppError> {
    let mut conn = pool.acquire().await?;
    let job = check_owner(ctx, fetch(&mut conn, id).await?, id)?;
    let headers: Vec<String> =
        job.info().map(|i| i.columns.into_iter().map(|c| c.header).collect()).unwrap_or_default();
    let filter = "job_id = $1 AND ($2::text IS NULL OR severity = $2) AND ($3::text IS NULL OR code = $3)
                  AND ($4::int IS NULL OR col_index = $4)";
    let severity = q.severity.map(|s| match s {
        IssueSeverity::Error => "error",
        IssueSeverity::Warning => "warning",
    });
    let column = q.column.map(|c| c as i32);
    let total: i64 =
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM cmdb.import_job_issues WHERE {filter}")))
            .bind(id)
            .bind(severity)
            .bind(&q.code)
            .bind(column)
            .fetch_one(&mut *conn)
            .await?;
    type IssueRow = (i32, Option<i32>, Option<String>, Option<String>, String, String, String);
    let rows: Vec<IssueRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT row_no, col_index, field, value, severity, code, message FROM cmdb.import_job_issues
         WHERE {filter} ORDER BY row_no, seq LIMIT $5 OFFSET $6"
    )))
    .bind(id)
    .bind(severity)
    .bind(&q.code)
    .bind(column)
    .bind(q.limit)
    .bind(q.offset)
    .fetch_all(&mut *conn)
    .await?;
    let data = rows
        .into_iter()
        .map(|(row, col, field, value, severity, code, message)| ImportIssue {
            row: row as u32,
            column: col.map(|c| c as u32),
            header: col.and_then(|c| headers.get(c as usize).cloned()),
            field,
            value,
            severity: if severity == "warning" { IssueSeverity::Warning } else { IssueSeverity::Error },
            code,
            message,
        })
        .collect();
    Ok(Page { data, page: q.page_meta(total) })
}
