//! CI notes (SHAA-2355): the note stream of a configuration item (author,
//! time, text), shown on the Notes tab of the CI detail page, and the
//! instance's note policy (edit window, retention period).

pub mod service;
#[cfg(test)]
mod tests;

use axum::http::header::{self, HeaderValue};
use axum::http::{Method, StatusCode};
use utoipa::openapi::Required;
use utoipa::openapi::path::{Parameter, ParameterBuilder, ParameterIn};
use uuid::Uuid;

use crate::api::route::{
    Body, IdPath, In, Json, NoBody, NoContent, NoPath, NoQuery, PathInput, Query, Route, WithHeaders, route,
};
use crate::api::{schemas as api_schemas, validate};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use service::{CiNote, CreateCiNote, DeleteCiNoteQuery, ListCiNotesQuery, UpdateCiNote, UpdateCiNoteSettings};

const TAG: &str = "CI notes";
const NOTES: &str = "/api/v1/configuration-items/{id}/notes";
const NOTE: &str = "/api/v1/configuration-items/{id}/notes/{noteId}";

/// `{id}` (the CI) and `{noteId}`.
pub struct NotePath(pub Uuid, pub Uuid);

fn uuid_param(name: &str) -> Parameter {
    ParameterBuilder::new()
        .name(name)
        .parameter_in(ParameterIn::Path)
        .required(Required::True)
        .schema(Some(api_schemas::uuid_builder()))
        .build()
}

impl PathInput for NotePath {
    fn params() -> Vec<Parameter> {
        vec![uuid_param("id"), uuid_param("noteId")]
    }
    fn parse(raw: &axum::extract::RawPathParams) -> Result<Self, AppError> {
        let get = |name: &str| -> Result<Uuid, FieldError> {
            let value = raw.iter().find(|(k, _)| *k == name).map(|(_, v)| v).unwrap_or_default();
            validate::is_uuid(value).then(|| Uuid::parse_str(value).ok()).flatten().ok_or_else(|| FieldError {
                location: FieldLocation::Params,
                field: name.into(),
                message: "Invalid UUID".into(),
                code: "invalid_format".into(),
            })
        };
        match (get("id"), get("noteId")) {
            (Ok(ci), Ok(note)) => Ok(NotePath(ci, note)),
            (a, b) => Err(AppError::validation([a.err(), b.err()].into_iter().flatten().collect())),
        }
    }
}

/// `201 Created` with the new note's URL.
fn created(note: CiNote) -> Result<WithHeaders<Json<CiNote>>, AppError> {
    let location = format!("/api/v1/configuration-items/{}/notes/{}", note.ci_id, note.id);
    let location = HeaderValue::from_str(&location).map_err(|_| AppError::internal())?;
    Ok(WithHeaders(Json(note), vec![(header::LOCATION, location)]))
}

pub fn routes() -> Vec<Route> {
    vec![
        route(Method::GET, NOTES, "listCiNotes")
            .tag(TAG)
            .summary("The notes of a CI, newest first (paginated)")
            .description(
                "Needs view on the CI's class; a CI that is missing or that the caller may not view is `404`, alike. \
                 A deleted CI keeps its notes, readable. Each note says whether the caller may change (`canEdit`) \
                 or delete (`canDelete`) it under the current policy.",
            )
            .errors(&[ErrorCode::NotFound])
            .class_checked()
            .handle(|api, In(IdPath(id), Query(q), NoBody): In<IdPath, Query<ListCiNotesQuery>, NoBody>| async move {
                Ok(Json(service::list(&api.pool, &api.ctx, id, &q).await?))
            }),
        route(Method::POST, NOTES, "createCiNote")
            .tag(TAG)
            .summary("Add a note to a CI")
            .description(
                "Needs edit on the CI's class (403); a CI that is missing, deleted or not viewable is `404`. The \
                 caller is the author (an API token writes as its owner). Audited as a `create` of `ci_notes` with \
                 the note, its CI in `ciId`.",
            )
            .status(StatusCode::CREATED)
            .errors(&[ErrorCode::NotFound])
            .class_checked()
            .handle(|api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<CreateCiNote>>| async move {
                created(service::create(&api.pool, &api.ctx, id, &b).await?)
            }),
        route(Method::PATCH, NOTE, "updateCiNote")
            .tag(TAG)
            .summary("Change the text of your note")
            .description(
                "Only the author, with edit on the CI's class, inside the edit window of the note policy, while the \
                 CI is not deleted: `403` with `details[0].code` `not_author` or `edit_window_closed` otherwise. \
                 Send the `version` you loaded (`409 VERSION_CONFLICT` if it changed). Sending the current text \
                 changes nothing. Audited as an `update` of `ci_notes` with the note before and after.",
            )
            .errors(&[ErrorCode::NotFound, ErrorCode::VersionConflict])
            .class_checked()
            .handle(
                |api, In(NotePath(ci, note), NoQuery, Body(b)): In<NotePath, NoQuery, Body<UpdateCiNote>>| async move {
                    Ok(Json(service::update(&api.pool, &api.ctx, ci, note, &b).await?))
                },
            ),
        route(Method::DELETE, NOTE, "deleteCiNote")
            .tag(TAG)
            .summary("Delete a note")
            .description(
                "The author, with edit on the CI's class and inside the edit window of the note policy (also on a \
                 deleted CI), or an administrator at any time; `403` with `details[0].code` `not_author` or \
                 `edit_window_closed` otherwise. `?version=` is the version you loaded (`409 VERSION_CONFLICT`). The \
                 note is removed; the audit log keeps it as a `delete` of `ci_notes`.",
            )
            .errors(&[ErrorCode::NotFound, ErrorCode::VersionConflict])
            .class_checked()
            .handle(
                |api, In(NotePath(ci, note), Query(q), NoBody): In<NotePath, Query<DeleteCiNoteQuery>, NoBody>| async move {
                    service::delete(&api.pool, &api.ctx, ci, note, q.version).await?;
                    Ok(NoContent)
                },
            ),
        route(Method::GET, "/api/v1/ci-note-settings", "getCiNoteSettings")
            .tag(TAG)
            .summary("The note policy: edit window and retention period")
            .description("Any signed-in user.")
            .handle(|api, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| async move {
                Ok(Json(service::get_settings(&api.pool).await?))
            }),
        route(Method::PUT, "/api/v1/ci-note-settings", "updateCiNoteSettings")
            .tag(TAG)
            .summary("Change the note policy (Administrator)")
            .description(
                "Administrator only (session only). `editWindowMinutes` applies to every note at once, older ones \
                 included. With `retentionDays` set, the server deletes notes older than that within the hour, and \
                 every hour after; each is audited as a `delete` of `ci_notes` by the system actor `note retention`, \
                 without its text. A change of the policy is audited as an `update` of `ci_note_settings`.",
            )
            .session_only()
            .errors(&[ErrorCode::Forbidden])
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<UpdateCiNoteSettings>>| async move {
                Ok(Json(service::update_settings(&api.pool, &api.ctx, &b).await?))
            }),
    ]
}
