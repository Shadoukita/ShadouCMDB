//! Bulk import of configuration items from CSV and Excel files (SHAA-714).
//!
//! Every import route needs a browser session (D8). Import is off until an
//! administrator turns it on (D4), and needs the global right `cis.import`
//! (D3) on top of the class rights, which every row is still checked against.

pub mod analyse;
pub mod commit;
pub mod convert;
pub mod csv_safe;
pub mod dry_run;
pub mod jobs;
pub mod mapping;
pub mod parse;
pub mod planner;
pub mod report;
pub mod saved;
pub mod schemas;
pub mod settings;
pub mod storage;
pub mod suggest;
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
const MAPPINGS: &str = "/api/v1/import-mappings";
const MAPPING: &str = "/api/v1/import-mappings/{id}";

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
        route(Method::GET, "/api/v1/imports/{id}/mapping-suggestion", "suggestImportMapping")
            .tag(TAG)
            .summary("A first mapping for the file, from its headers")
            .description(
                "Headers are compared ignoring case, surrounding spaces and runs of spaces, `_`, `-` and `.`. Per \
                 column: the saved mapping's target for the header (with `mappingId`, or when exactly one saved \
                 mapping of the class has the same set of headers as the file), else an attribute key, `ident`, \
                 `valid from` or `valid until`, else the label of exactly one attribute or relationship type (its \
                 forward label for outgoing, reverse label for incoming). A target goes to the first column only. \
                 References and relationships match by label. `ident` is never suggested for new CIs to \
                 non-administrators. Nothing is stored: send the mapping with `PUT /imports/{id}/mapping`. A class \
                 the caller cannot import into is `400 unknown_class`, the same as an unknown key.",
            )
            .requires(GlobalPermission::CisImport)
            .session_only()
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict])
            .handle(|api, In(IdPath(id), Query(q), NoBody): In<IdPath, Query<suggest::SuggestQuery>, NoBody>| async move {
                Ok(Json(suggest::suggest(&api.pool, &api.ctx, &api.imports, id, &q).await?))
            }),
        route(Method::PUT, "/api/v1/imports/{id}/mapping", "setImportMapping")
            .tag(TAG)
            .summary("Set how the file's columns map to the class")
            .description(
                "Checked against the file's columns and the data model; every problem is reported at once in \
                 `details`, with `field` such as `columns[3].target.key`. Any dry run is dropped and the job is \
                 `ready`. In `ready` or `validated`; `409` while a step runs.",
            )
            .requires(GlobalPermission::CisImport)
            .session_only()
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict])
            .handle(|api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<schemas::ImportMapping>>| async move {
                Ok(Json(jobs::set_mapping(&api.pool, &api.ctx, &api.imports, id, &b).await?))
            }),
        route(Method::POST, "/api/v1/imports/{id}/dry-run", "startImportDryRun")
            .tag(TAG)
            .summary("Check every row without writing anything")
            .description(
                "Runs the rows through the same validation as the CI API and records what each would do, and every \
                 problem. `409` without a mapping (`mapping_required`) or while a step runs (`invalid_state`); \
                 `429 import_busy` while another import of the job's owner runs.",
            )
            .requires(GlobalPermission::CisImport)
            .session_only()
            .status(StatusCode::ACCEPTED)
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict, ErrorCode::RateLimited])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(jobs::start_dry_run(&api.pool, &api.ctx, &api.imports, id).await?))
            }),
        route(Method::GET, "/api/v1/imports/{id}/issues", "listImportIssues")
            .tag(TAG)
            .summary("The problems the dry run and the commit found, by row")
            .description(
                "In row order. `value` is the cell, cut to 200 characters. At most 10,000 problems are stored per \
                 job; `summary.issuesTotal` counts them all. Empty once the file was deleted (24 h after the last \
                 activity). Also while bulk import is off.",
            )
            .requires(GlobalPermission::CisImport)
            .session_only()
            .errors(&[ErrorCode::NotFound])
            .handle(
                |api, In(IdPath(id), Query(q), NoBody): In<IdPath, Query<schemas::ListImportIssuesQuery>, NoBody>| async move {
                    Ok(Json(jobs::issues(&api.pool, &api.ctx, id, &q).await?))
                },
            ),
        route(Method::GET, "/api/v1/imports/{id}/error-report", "downloadImportErrorReport")
            .tag(TAG)
            .summary("The problems as a CSV to fix and upload again")
            .description(
                "Columns `Row`, `Severity`, `Column`, `Problem`, `Code`, then every original column of the row \
                 under its original header; one line per problem, in row order. UTF-8 with a byte order mark, CRLF, \
                 the file's delimiter (`,` for workbooks). Every field is quoted, and a field a spreadsheet could \
                 read as a formula (starting with `=` `+` `-` `@`, a tab or a line break) or starting with `'` gets \
                 a leading `'`; a file uploaded again is recognised as a report by its first five headers and the \
                 `'` is taken off. At most the 10,000 stored problems. `404` when the job has no problems or its file \
                 expired. A download by anyone other than the job's owner is audited as `import.report_read`. \
                 `429` when the caller already downloads 2 reports, `503` when the server sends 8; a client that \
                 reads nothing for 30 s, or takes more than 15 minutes, is cut off. Also while bulk import is off, \
                 so the report can be kept before the job is deleted.",
            )
            .requires(GlobalPermission::CisImport)
            .session_only()
            .errors(&[ErrorCode::NotFound, ErrorCode::RateLimited, ErrorCode::ServerBusy])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                report::download(&api.pool, &api.ctx, &api.imports, id).await
            }),
        route(Method::POST, "/api/v1/imports/{id}/commit", "commitImport")
            .tag(TAG)
            .summary("Write the rows the dry run checked")
            .description(
                "Queues the commit (`202`, then `committing`). Rows are written in file order, 500 per transaction, \
                 and planned again against the current data, so changes since the dry run are caught; each row is \
                 applied completely or not at all. Rows the dry run found errors in are never written. `409` with \
                 `details[0].code`: `dry_run_required`, `dry_run_stale` (the data model changed, or the dry run is \
                 older than 24 hours), `has_error_rows` (send `skipErrorRows: true` to import the other rows) or \
                 `invalid_state`. `429 import_busy` while another import of the job's owner runs. An optional \
                 `Idempotency-Key` returns the job as it is now instead of starting a second commit.",
            )
            .requires(GlobalPermission::CisImport)
            .session_only()
            .status(StatusCode::ACCEPTED)
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict, ErrorCode::RateLimited, ErrorCode::IdempotencyKeyReused])
            .handle(|api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<schemas::CommitImport>>| async move {
                Ok(Json(commit::start(&api.pool, &api.ctx, &api.imports, &api.headers, id, &b).await?))
            }),
        route(Method::POST, "/api/v1/imports/{id}/cancel", "cancelImport")
            .tag(TAG)
            .summary("Stop an import")
            .description(
                "Analysis, dry run and a queued commit stop at once. A running commit stops after its current \
                 batch of at most 500 rows, and the rows committed so far stay: the job stays `committing` with \
                 `cancelRequestedAt` set until that batch is written, then ends as `cancelled` with its final \
                 counts. `409` once the job has ended. Also while bulk import is off.",
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
        route(Method::GET, MAPPINGS, "listImportMappings")
            .tag(TAG)
            .summary("Saved mappings, by name")
            .description(
                "Every saved mapping of a class the caller can view (at most 500 per instance, so not paged). \
                 Saved mappings are shared with everyone who has `cis.import`.",
            )
            .requires(GlobalPermission::CisImport)
            .session_only()
            .handle(
                |api, In(NoPath, Query(q), NoBody): In<NoPath, Query<saved::ListSavedMappingsQuery>, NoBody>| async move {
                    Ok(Json(saved::list(&api.pool, &api.ctx, &q).await?))
                },
            ),
        route(Method::POST, MAPPINGS, "createImportMapping")
            .tag(TAG)
            .summary("Save a mapping for files with the same layout")
            .description(
                "Needs view on the class; a class the caller cannot view is `400 unknown_class`, the same as an \
                 unknown key. Names are unique per class (`409 duplicate_name`). At most 500 per instance \
                 (`409 limit_reached`) and 64 KiB per definition (`400 too_large`). The definition's targets are \
                 checked when it is applied to a file. Audited as a `create` of `import_mappings`.",
            )
            .requires(GlobalPermission::CisImport)
            .session_only()
            .status(StatusCode::CREATED)
            .errors(&[ErrorCode::Conflict])
            .handle(
                |api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<saved::CreateSavedImportMapping>>| async move {
                    Ok(Json(saved::create(&api.pool, &api.ctx, &api.imports, &b).await?))
                },
            ),
        route(Method::GET, MAPPING, "getImportMapping")
            .tag(TAG)
            .summary("A saved mapping")
            .description("`404` when its class is hidden from the caller, the same as for a mapping that does not exist.")
            .requires(GlobalPermission::CisImport)
            .session_only()
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(saved::get(&api.pool, &api.ctx, id).await?))
            }),
        route(Method::PATCH, MAPPING, "updateImportMapping")
            .tag(TAG)
            .summary("Rename or change a saved mapping (creator or Administrator)")
            .description(
                "Send the `version` you loaded; `409 VERSION_CONFLICT` if someone saved in between. The class cannot \
                 change. Only the user who saved it and administrators may change it (`403`). Audited as an \
                 `update` of `import_mappings` with the old and new definition.",
            )
            .requires(GlobalPermission::CisImport)
            .session_only()
            .errors(&[ErrorCode::NotFound, ErrorCode::Forbidden, ErrorCode::Conflict, ErrorCode::VersionConflict])
            .handle(
                |api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<saved::UpdateSavedImportMapping>>| async move {
                    Ok(Json(saved::update(&api.pool, &api.ctx, &api.imports, id, &b).await?))
                },
            ),
        route(Method::DELETE, MAPPING, "deleteImportMapping")
            .tag(TAG)
            .summary("Delete a saved mapping (creator or Administrator)")
            .description(
                "`?version=` is the version you loaded (`409 VERSION_CONFLICT` otherwise). Jobs that used the \
                 mapping keep their own copy. Audited as a `delete` of `import_mappings`.",
            )
            .requires(GlobalPermission::CisImport)
            .session_only()
            .errors(&[ErrorCode::NotFound, ErrorCode::Forbidden, ErrorCode::VersionConflict])
            .handle(
                |api, In(IdPath(id), Query(q), NoBody): In<IdPath, Query<saved::DeleteSavedMappingQuery>, NoBody>| async move {
                    saved::delete(&api.pool, &api.ctx, id, q.version).await?;
                    Ok(NoContent)
                },
            ),
    ]
}
