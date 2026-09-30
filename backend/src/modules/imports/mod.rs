//! Bulk import of configuration items from CSV and Excel files (SHAA-714).
//!
//! Every import route needs a browser session (D8). Import is off until an
//! administrator turns it on (D4), and needs the global right `cis.import`
//! (D3) on top of the class rights, which every row is still checked against.

pub mod analyse;
pub mod csv_safe;
pub mod jobs;
pub mod parse;
pub mod schemas;
pub mod settings;
pub mod storage;
pub mod template;
#[cfg(test)]
mod tests;
pub mod upload;
pub mod worker;

use axum::http::{HeaderValue, Method, StatusCode, header};

use crate::api::route::{
    Body, CsvDownload, IdPath, In, Json, NoBody, NoContent, NoPath, NoQuery, Query, RawBody, Route, WithHeaders, route,
};
use crate::auth::permissions::GlobalPermission;
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use schemas::{ListImportsQuery, TemplateQuery, UpdateFileOptions};
use settings::UpdateImportSettings;

const TAG: &str = "Bulk import";

/// Most columns in one file (§3.5).
pub const MAX_COLUMNS: u32 = 200;
/// Most characters in one cell (§3.5; the text attributes' default maxLength).
pub const MAX_CELL_CHARS: u32 = 10_000;

/// An error whose `details[0].code` says why, e.g. `import_disabled`.
pub(crate) fn coded(code: ErrorCode, message: &str, detail: &str) -> AppError {
    AppError::new(code, message).with_details(vec![FieldError {
        location: FieldLocation::Body,
        field: "(root)".into(),
        message: message.into(),
        code: detail.into(),
    }])
}

/// One problem with one body field.
pub(crate) fn body_field(field: &str, message: &str, code: &str) -> FieldError {
    FieldError { location: FieldLocation::Body, field: field.into(), message: message.into(), code: code.into() }
}

/// One problem with one request header.
pub(crate) fn header_field(field: &str, message: &str, code: &str) -> FieldError {
    FieldError { location: FieldLocation::Header, field: field.into(), message: message.into(), code: code.into() }
}

/// The largest upload any configuration allows (`IMPORT_MAX_FILE_MB` ≤ 200);
/// the handler holds each upload to the configured limit.
const UPLOAD_ROUTE_LIMIT: usize = 200 * 1024 * 1024;

const JOBS: &str = "/api/v1/imports";
const JOB: &str = "/api/v1/imports/{id}";

pub fn routes() -> Vec<Route> {
    vec![
        route(Method::GET, "/api/v1/imports/settings", "getImportSettings")
            .tag(TAG)
            .summary("Whether bulk import is on, and its limits")
            .description(
                "Any signed-in user (session only). `enabled` is true when an administrator switched import on and \
                 the server configuration allows it; `locked` is true when the server configuration \
                 (`IMPORT_ALLOWED=false`) keeps it off. `limits` are the effective upload limits.",
            )
            .session_only()
            .handle(|api, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| async move {
                Ok(Json(settings::get(&api.pool, &api.imports).await?))
            }),
        route(Method::PUT, "/api/v1/imports/settings", "updateImportSettings")
            .tag(TAG)
            .summary("Turn bulk import on or off (Administrator)")
            .description(
                "Administrator only (session only). The change is audited as an `update` of `import_settings`. \
                 Turning import on while the server configuration forbids it (`IMPORT_ALLOWED=false`) is \
                 `409 CONFLICT` with `details[0].code = import_locked`; turning it off always works.",
            )
            .session_only()
            .errors(&[ErrorCode::Forbidden, ErrorCode::Conflict])
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<UpdateImportSettings>>| async move {
                Ok(Json(settings::update(&api.pool, &api.ctx, &api.imports, &b).await?))
            }),
        route(Method::POST, JOBS, "createImport")
            .tag(TAG)
            .summary("Upload a CSV or XLSX file for import")
            .description(
                "The body is the file itself (not JSON, not multipart), with `Content-Type: text/csv` or \
                 `application/vnd.openxmlformats-officedocument.spreadsheetml.sheet`; any other type is `415`. The \
                 format is decided by the file's first bytes: a password-protected workbook or an `.xls` is `415` \
                 with `workbook_encrypted_or_xls`, a file that does not match the declared type `415` with \
                 `unsupported_format`. The file name goes in the `X-File-Name` header, percent-encoded UTF-8 \
                 (1–255 characters, no control or invisible formatting characters, no path), never in the URL. An optional \
                 `Idempotency-Key` (1–128 visible ASCII characters, kept 24 h) returns the job it created before. \
                 Answers `202` with the job, `queued` for analysis, once the last byte is stored. Limits: \
                 `IMPORT_MAX_FILE_MB` (`413`), `IMPORT_UPLOAD_TIMEOUT_SECS` for the whole upload and 60 s without \
                 data (`408`, `upload_timeout`), and `429` with `import_busy` (an import of yours is running), \
                 `import_limit` (20 unfinished imports), `import_rate` (30 uploads an hour) or \
                 `import_storage_full` (the server's stored-upload cap). `403` with `import_disabled` while bulk \
                 import is off.",
            )
            .requires(GlobalPermission::CisImport)
            .session_only()
            .raw_body(upload::MEDIA, UPLOAD_ROUTE_LIMIT)
            .status(StatusCode::ACCEPTED)
            .errors(&[ErrorCode::RateLimited, ErrorCode::IdempotencyKeyReused, ErrorCode::Conflict])
            .handle(|api, In(NoPath, NoQuery, body): In<NoPath, NoQuery, RawBody>| async move {
                let job = upload::create(&api.pool, &api.ctx, &api.imports, &api.headers, body).await?;
                let location =
                    HeaderValue::from_str(&format!("{JOBS}/{}", job.id)).map_err(|_| AppError::internal())?;
                Ok(WithHeaders(Json(job), vec![(header::LOCATION, location)]))
            }),
        route(Method::GET, JOBS, "listImports")
            .tag(TAG)
            .summary("Your imports, newest first")
            .description(
                "The caller's jobs; administrators may pass `all=true` for every user's. Also while bulk import is \
                 off, so remaining jobs can be seen and removed.",
            )
            .requires(GlobalPermission::CisImport)
            .session_only()
            .handle(|api, In(NoPath, Query(q), NoBody): In<NoPath, Query<ListImportsQuery>, NoBody>| async move {
                Ok(Json(jobs::list(&api.pool, &api.ctx, &q).await?))
            }),
        route(Method::GET, "/api/v1/imports/template", "downloadImportTemplate")
            .tag(TAG)
            .summary("An empty CSV with the column names of a class")
            .description(
                "One header row: `Ident` and the labels of the class's active attributes, every field quoted and \
                 neutralised against formula injection. `404` for a class the caller cannot import into, the same \
                 as for an unknown key.",
            )
            .requires(GlobalPermission::CisImport)
            .session_only()
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(NoPath, Query(q), NoBody): In<NoPath, Query<TemplateQuery>, NoBody>| async move {
                let (file_name, csv) = template::template(&api.pool, &api.ctx, &api.imports, &q.class_key).await?;
                Ok(CsvDownload { file_name, body: csv.into() })
            }),
        route(Method::GET, JOB, "getImport")
            .tag(TAG)
            .summary("An import job, for polling")
            .description(
                "The caller's job, or any job for an administrator; `404` otherwise, the same as for a job that \
                 does not exist. Poll every 1 s for the first 10 s, then every 2 s, then every 5 s after a minute.",
            )
            .requires(GlobalPermission::CisImport)
            .session_only()
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(jobs::get(&api.pool, &api.ctx, id).await?))
            }),
        route(Method::PATCH, "/api/v1/imports/{id}/file-options", "updateImportFileOptions")
            .tag(TAG)
            .summary("Change how the file is read (sheet, encoding, delimiter, header row)")
            .description(
                "The analysis runs again and the mapping and any dry run are dropped. In `ready`, or after an \
                 analysis that failed. `sheet` is for workbooks, `encoding` and `delimiter` for CSV files.",
            )
            .requires(GlobalPermission::CisImport)
            .session_only()
            .status(StatusCode::ACCEPTED)
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict])
            .handle(|api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<UpdateFileOptions>>| async move {
                Ok(Json(jobs::update_file_options(&api.pool, &api.ctx, &api.imports, id, &b).await?))
            }),
        route(Method::POST, "/api/v1/imports/{id}/cancel", "cancelImport")
            .tag(TAG)
            .summary("Stop an import")
            .description(
                "Analysis and dry run stop at once; a commit stops after its current batch of at most 500 rows, \
                 and the rows committed so far stay. `409` once the job has ended. Also while bulk import is off.",
            )
            .requires(GlobalPermission::CisImport)
            .session_only()
            .status(StatusCode::ACCEPTED)
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(jobs::cancel(&api.pool, &api.ctx, id).await?))
            }),
        route(Method::DELETE, JOB, "deleteImport")
            .tag(TAG)
            .summary("Delete an import, its file and its row problems")
            .description(
                "Never touches CIs. `409` while the job is running (cancel it first). Also while bulk import is off, \
                 so uploaded files can be removed.",
            )
            .requires(GlobalPermission::CisImport)
            .session_only()
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                jobs::delete(&api.pool, &api.ctx, id).await?;
                Ok(NoContent)
            }),
    ]
}
