//! `POST /imports`: the upload (§3.2, §3.5, §3.6, §5.3–5.5).
//!
//! 1. The per-user limits are checked under a per-user advisory lock in one
//!    short transaction that also creates the job as `uploading` (T19).
//! 2. The body is read as a stream and stored in 1 MiB chunks. A pool
//!    connection is taken only to insert a complete chunk, never while
//!    waiting for the client (CR7). The size is counted, the SHA-256
//!    computed, and at most about 2 MiB is buffered.
//! 3. The first bytes decide the format (magic bytes), whatever the
//!    extension or the declared type says (§5.4).
//! 4. After the last byte the job becomes `queued` for analysis, and the
//!    answer is sent. If the client disconnects, the upload times out or
//!    anything fails, the `uploading` job and its chunks are deleted at once.

use std::time::Duration;

use axum::http::HeaderMap;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

use super::analyse::FileFormat;
use super::jobs::{self, COLUMNS, FINAL_STATUSES, JobRow, RUNNING_STATUSES};
use super::parse::xlsx::{Sniffed, ole_error, sniff};
use super::schemas::ImportJob;
use super::storage::{CHUNK, insert_chunk};
use super::{coded, header_field};
use crate::api::context::RequestContext;
use crate::api::route::RawBody;
use crate::config::ImportConfig;
use crate::http::error::{AppError, ErrorCode};

pub const CSV_TYPE: &str = "text/csv";
pub const XLSX_TYPE: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
pub const MEDIA: &[&str] = &[CSV_TYPE, XLSX_TYPE];

/// Jobs of one user in a running phase (uploads included).
pub const MAX_RUNNING_PER_USER: i64 = 1;
/// Jobs of one user that have not ended.
pub const MAX_UNFINISHED_PER_USER: i64 = 20;
/// Uploads of one user per hour.
pub const MAX_UPLOADS_PER_HOUR: i64 = 30;
/// An upload that sends nothing for this long is dropped (T5).
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

pub const FILE_NAME_HEADER: &str = "x-file-name";
pub const IDEMPOTENCY_HEADER: &str = "idempotency-key";

fn limit(code: &str, message: &str) -> AppError {
    let mut e = coded(ErrorCode::RateLimited, message, code);
    e.retry_after = Some(60);
    e
}

/// Unicode format characters (General_Category Cf, Unicode 16): bidi
/// overrides and isolates, zero-width characters, the BOM, tags. Invisible,
/// and some make a name display differently from what it is
/// (`report<U+202E>xslx.exe`).
fn is_format(c: char) -> bool {
    matches!(
        c,
        '\u{AD}'
            | '\u{600}'..='\u{605}'
            | '\u{61C}'
            | '\u{6DD}'
            | '\u{70F}'
            | '\u{890}'..='\u{891}'
            | '\u{8E2}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{110BD}'
            | '\u{110CD}'
            | '\u{13430}'..='\u{1343F}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0001}'
            | '\u{E0020}'..='\u{E007F}'
    )
}

/// The file name from `X-File-Name` (percent-encoded UTF-8, T16): NFC, 1–255
/// characters, no control or format characters, no path.
pub fn file_name(headers: &HeaderMap) -> Result<String, AppError> {
    let invalid = |m: &str, code: &str| AppError::validation(vec![header_field("X-File-Name", m, code)]);
    let raw = headers
        .get(FILE_NAME_HEADER)
        .ok_or_else(|| invalid("Send the file name in the X-File-Name header", "required"))?
        .to_str()
        .map_err(|_| invalid("Must be percent-encoded UTF-8", "invalid_format"))?;
    let decoded = percent_encoding::percent_decode_str(raw)
        .decode_utf8()
        .map_err(|_| invalid("Must be percent-encoded UTF-8", "invalid_format"))?;
    let name: String = decoded.nfc().collect();
    let chars = name.chars().count();
    if chars == 0 || chars > 255 {
        return Err(invalid("Must be 1 to 255 characters", "invalid_length"));
    }
    if name.chars().any(|c| c.is_control() || is_format(c)) {
        return Err(invalid("Must not contain control or invisible formatting characters", "invalid_character"));
    }
    if name.contains('/') || name.contains('\\') {
        return Err(invalid("Must be a file name without a path", "invalid_character"));
    }
    Ok(name)
}

/// `Idempotency-Key`: 1–128 visible ASCII characters.
pub fn idempotency_key(headers: &HeaderMap) -> Result<Option<String>, AppError> {
    let Some(v) = headers.get(IDEMPOTENCY_HEADER) else { return Ok(None) };
    match v.to_str() {
        Ok(k) if (1..=128).contains(&k.len()) && k.bytes().all(|b| (0x21..=0x7e).contains(&b)) => {
            Ok(Some(k.to_owned()))
        }
        _ => Err(AppError::validation(vec![header_field(
            "Idempotency-Key",
            "Must be 1 to 128 visible ASCII characters",
            "invalid_format",
        )])),
    }
}

/// The job an idempotency key already stands for; `422` when the key was used
/// for another operation or another job (T15).
pub async fn replay(
    pool: &PgPool,
    user: Uuid,
    key: &str,
    operation: &str,
    job: Option<Uuid>,
) -> Result<Option<Uuid>, AppError> {
    let hit: Option<(String, Uuid)> = sqlx::query_as(
        "SELECT operation, job_id FROM cmdb.import_idempotency_keys
         WHERE user_id = $1 AND key = $2 AND created_at > now() - interval '24 hours'
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(user)
    .bind(key)
    .fetch_optional(pool)
    .await?;
    match hit {
        None => Ok(None),
        Some((op, id)) if op == operation && job.is_none_or(|j| j == id) => Ok(Some(id)),
        Some(_) => Err(AppError::new(
            ErrorCode::IdempotencyKeyReused,
            "This Idempotency-Key was already used for another request.",
        )
        .with_details(vec![header_field(
            "Idempotency-Key",
            "This key was already used for another request",
            "idempotency_key_reused",
        )])),
    }
}

/// Deletes the `uploading` job if the upload does not finish.
struct Cleanup {
    pool: PgPool,
    job: Uuid,
    armed: bool,
}

impl Cleanup {
    async fn now(mut self) {
        self.armed = false;
        delete_upload(&self.pool, self.job).await;
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if self.armed {
            let (pool, job) = (self.pool.clone(), self.job);
            tokio::spawn(async move { delete_upload(&pool, job).await });
        }
    }
}

async fn delete_upload(pool: &PgPool, job: Uuid) {
    if let Err(e) =
        sqlx::query("DELETE FROM cmdb.import_jobs WHERE id = $1 AND status = 'uploading'").bind(job).execute(pool).await
    {
        tracing::warn!(%job, error = %e, "could not remove an unfinished upload; the cleanup task will");
    }
}

/// Stored upload bytes of the whole instance.
async fn stored_bytes(pool: &PgPool) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT coalesce(sum(octet_length(data)), 0)::bigint FROM cmdb.import_job_files")
        .fetch_one(pool)
        .await
}

fn storage_full() -> AppError {
    limit(
        "import_storage_full",
        "The server has no room for more uploaded files. Delete finished imports or try again later.",
    )
}

/// Takes the user's import lock for the transaction and refuses with
/// `429 import_busy` while it is held or another of their jobs is running
/// (T19). Used by the upload, the dry run and the commit.
pub async fn lock_user(tx: &mut sqlx::PgConnection, user: Uuid) -> Result<(), AppError> {
    let busy = || limit("import_busy", "You already have an import running. Wait for it to finish or cancel it.");
    let locked: bool =
        sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended('cmdb.import:' || $1::text, 0))")
            .bind(user)
            .fetch_one(&mut *tx)
            .await?;
    if !locked {
        return Err(busy());
    }
    let running: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT count(*) FROM cmdb.import_jobs WHERE created_by_id = $1 AND status IN {RUNNING_STATUSES}"
    )))
    .bind(user)
    .fetch_one(&mut *tx)
    .await?;
    if running >= MAX_RUNNING_PER_USER {
        return Err(busy());
    }
    Ok(())
}

/// Creates the `uploading` job after the per-user limits (T19).
async fn start(
    pool: &PgPool,
    ctx: &RequestContext,
    cfg: &ImportConfig,
    name: &str,
    format: FileFormat,
    declared: Option<u64>,
    key: Option<&str>,
) -> Result<Uuid, AppError> {
    let principal = ctx.principal().ok_or_else(AppError::internal)?;
    let mut tx = pool.begin().await?;
    jobs::require_enabled(&mut tx, cfg).await?;
    lock_user(&mut tx, principal.user_id).await?;
    let (unfinished, recent): (i64, i64) = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT count(*) FILTER (WHERE status NOT IN {FINAL_STATUSES}),
                count(*) FILTER (WHERE created_at > now() - interval '1 hour')
         FROM cmdb.import_jobs WHERE created_by_id = $1"
    )))
    .bind(principal.user_id)
    .fetch_one(&mut *tx)
    .await?;
    if unfinished >= MAX_UNFINISHED_PER_USER {
        return Err(limit(
            "import_limit",
            "You have 20 imports that have not ended. Finish, cancel or delete some of them first.",
        ));
    }
    if recent >= MAX_UPLOADS_PER_HOUR {
        return Err(limit("import_rate", "You uploaded 30 files in the last hour. Try again later."));
    }
    let stored: i64 =
        sqlx::query_scalar("SELECT coalesce(sum(octet_length(data)), 0)::bigint FROM cmdb.import_job_files")
            .fetch_one(&mut *tx)
            .await?;
    if stored as u64 + declared.unwrap_or(0) > cfg.max_stored_bytes {
        return Err(storage_full());
    }
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO cmdb.import_jobs (created_by_id, created_by_name, status, file_name, file_format, file_size, expires_at)
         VALUES ($1, $2, 'uploading', $3, $4, 0, now() + interval '24 hours') RETURNING id",
    )
    .bind(principal.user_id)
    .bind(&principal.username)
    .bind(name)
    .bind(format.as_str())
    .fetch_one(&mut *tx)
    .await?;
    if let Some(key) = key {
        sqlx::query(
            "INSERT INTO cmdb.import_idempotency_keys (user_id, key, operation, job_id) VALUES ($1, $2, 'create', $3)",
        )
        .bind(principal.user_id)
        .bind(key)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(id)
}

fn format_error(sniffed: Sniffed, declared: FileFormat) -> Option<AppError> {
    let refuse = |code: &str, message: &str| coded(ErrorCode::UnsupportedMediaType, message, code);
    match (sniffed, declared) {
        (Sniffed::Ole, _) => {
            let e = ole_error();
            Some(refuse(e.code, &e.message))
        }
        (Sniffed::Zip, FileFormat::Xlsx) | (Sniffed::Other, FileFormat::Csv) => None,
        (Sniffed::Zip, FileFormat::Csv) => {
            Some(refuse("unsupported_format", "The file is a ZIP archive or workbook, not a CSV file."))
        }
        (Sniffed::Other, FileFormat::Xlsx) => {
            Some(refuse("unsupported_format", "The file is not an .xlsx workbook. Save it as .xlsx or CSV."))
        }
    }
}

/// Stores the upload and queues its analysis. Returns the job.
pub async fn create(
    pool: &PgPool,
    ctx: &RequestContext,
    cfg: &ImportConfig,
    headers: &HeaderMap,
    body: RawBody,
) -> Result<ImportJob, AppError> {
    let principal = ctx.principal().ok_or_else(AppError::internal)?;
    let key = idempotency_key(headers)?;
    if let Some(key) = &key
        && let Some(id) = replay(pool, principal.user_id, key, "create", None).await?
    {
        return jobs::get(pool, ctx, id).await;
    }
    let name = file_name(headers)?;
    let format = if body.content_type == XLSX_TYPE { FileFormat::Xlsx } else { FileFormat::Csv };
    let id = start(pool, ctx, cfg, &name, format, body.declared_length, key.as_deref()).await?;
    let cleanup = Cleanup { pool: pool.clone(), job: id, armed: true };
    let limit_bytes = (body.limit as u64).min(cfg.max_file_bytes);
    let received =
        tokio::time::timeout(cfg.upload_timeout, receive(pool, cfg, id, format, body.body, limit_bytes)).await;
    let (size, sha256) = match received {
        Ok(Ok(done)) => done,
        Ok(Err(e)) => {
            cleanup.now().await;
            return Err(e);
        }
        Err(_) => {
            cleanup.now().await;
            return Err(upload_timeout(&format!(
                "The upload did not finish within {} s.",
                cfg.upload_timeout.as_secs()
            )));
        }
    };
    let row: Option<JobRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "UPDATE cmdb.import_jobs SET status = 'queued', phase = 'analyse', file_size = $2, file_sha256 = $3,
           queued_at = now(), expires_at = now() + interval '24 hours'
         WHERE id = $1 AND status = 'uploading' RETURNING {COLUMNS}"
    )))
    .bind(id)
    .bind(size as i64)
    .bind(&sha256)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        cleanup.now().await;
        return Err(AppError::conflict("The upload was removed before it finished."));
    };
    let mut cleanup = cleanup;
    cleanup.armed = false;
    tracing::info!(job = %id, size, format = format.as_str(), sha256 = %sha256, "import file uploaded");
    let mut conn = pool.acquire().await?;
    let position = jobs::queue_position(&mut conn, &row).await?;
    Ok(row.dto(position))
}

fn upload_timeout(message: &str) -> AppError {
    coded(ErrorCode::RequestTimeout, message, "upload_timeout")
}

fn too_large(limit: u64) -> AppError {
    AppError::new(
        ErrorCode::PayloadTooLarge,
        format!("The file is larger than {} MB, the limit of this server.", limit / (1024 * 1024)),
    )
}

/// Reads the body into chunks. Returns the size and the SHA-256 (hex).
async fn receive(
    pool: &PgPool,
    cfg: &ImportConfig,
    job: Uuid,
    format: FileFormat,
    body: axum::body::Body,
    limit_bytes: u64,
) -> Result<(u64, String), AppError> {
    let mut stream = body.into_data_stream();
    let mut buffer: Vec<u8> = Vec::with_capacity(CHUNK);
    let mut hasher = Sha256::new();
    let (mut total, mut seq, mut sniffed) = (0u64, 0i32, false);
    loop {
        let next = match tokio::time::timeout(IDLE_TIMEOUT, stream.next()).await {
            Ok(n) => n,
            Err(_) => return Err(upload_timeout("The upload sent no data for 60 s.")),
        };
        let Some(frame) = next else { break };
        let bytes =
            frame.map_err(|_| AppError::field("(root)", "The upload was interrupted.", "upload_interrupted"))?;
        total += bytes.len() as u64;
        if total > limit_bytes {
            return Err(too_large(limit_bytes));
        }
        hasher.update(&bytes);
        buffer.extend_from_slice(&bytes);
        if !sniffed && buffer.len() >= 4 {
            sniffed = true;
            if let Some(e) = format_error(sniff(&buffer), format) {
                return Err(e);
            }
        }
        while buffer.len() >= CHUNK {
            store(pool, cfg, job, seq, &buffer[..CHUNK], total).await?;
            buffer.drain(..CHUNK);
            seq += 1;
        }
    }
    if total == 0 {
        return Err(AppError::field("(root)", "The file is empty.", "empty_file"));
    }
    if !sniffed && let Some(e) = format_error(sniff(&buffer), format) {
        return Err(e);
    }
    if !buffer.is_empty() {
        store(pool, cfg, job, seq, &buffer, total).await?;
    }
    Ok((total, hex::encode(hasher.finalize())))
}

/// Inserts one chunk (one short statement each) and keeps the job's size
/// current, within the instance's stored-bytes cap.
async fn store(
    pool: &PgPool,
    cfg: &ImportConfig,
    job: Uuid,
    seq: i32,
    data: &[u8],
    total: u64,
) -> Result<(), AppError> {
    if stored_bytes(pool).await? as u64 + data.len() as u64 > cfg.max_stored_bytes {
        return Err(storage_full());
    }
    insert_chunk(pool, job, seq, data).await?;
    sqlx::query("UPDATE cmdb.import_jobs SET file_size = $2 WHERE id = $1 AND status = 'uploading'")
        .bind(job)
        .bind(total as i64)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(raw: &str) -> Result<String, AppError> {
        let mut headers = HeaderMap::new();
        headers.insert(FILE_NAME_HEADER, raw.parse().unwrap());
        file_name(&headers)
    }

    #[test]
    fn file_names_refuse_invisible_and_bidi_characters() {
        assert_eq!(name("Server%20list%20%C3%BC.xlsx").unwrap(), "Server list ü.xlsx");
        for raw in ["report%E2%80%AEvsc.xlsx", "a%E2%80%8Bb.csv", "a%E2%81%A6b.csv", "%EF%BB%BFa.csv", "a%C2%ADb.csv"] {
            assert!(name(raw).is_err(), "{raw}");
        }
    }
}
