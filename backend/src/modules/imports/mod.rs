//! Bulk import of configuration items from CSV and Excel files (SHAA-714).
//!
//! Every import route needs a browser session (D8). Import is off until an
//! administrator turns it on (D4), and needs the global right `cis.import`
//! (D3) on top of the class rights, which every row is still checked against.

pub mod settings;

use axum::http::Method;

use crate::api::route::{Body, In, Json, NoBody, NoPath, NoQuery, Route, route};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
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
    ]
}
