//! Saved views (SHAA-578, SHAA-616): named states of the inventory list and of
//! the search page, personal or shared with every user.
//!
//! A view stores a query by key, never data or rights (D2, D3): running it is
//! an ordinary list or search request, which applies the caller's class rights
//! as always, so a view never widens what a user sees. Every route needs a
//! browser session (D8): a view belongs to a person, and an API token has none.

pub mod definition;
pub mod resolve;
pub mod service;
#[cfg(test)]
mod tests;

use axum::http::{HeaderValue, Method, StatusCode, header};

use crate::api::route::{Body, IdPath, In, Json, NoBody, NoContent, NoPath, NoQuery, Query, Route, WithHeaders, route};
use crate::http::error::{AppError, ErrorCode};
use service::{
    CopySavedView, CreateSavedView, DeleteSavedViewQuery, ListSavedViewsQuery, SavedView, SetSavedViewDefault,
    UpdateSavedView,
};

pub const TAG: &str = "Saved views";

const VIEWS: &str = "/api/v1/saved-views";
const VIEW: &str = "/api/v1/saved-views/{id}";

/// `201 Created` with the new view's URL.
fn created(view: SavedView) -> Result<WithHeaders<Json<SavedView>>, AppError> {
    let location = HeaderValue::from_str(&format!("{VIEWS}/{}", view.id)).map_err(|_| AppError::internal())?;
    Ok(WithHeaders(Json(view), vec![(header::LOCATION, location)]))
}

pub fn routes() -> Vec<Route> {
    vec![
        route(Method::GET, VIEWS, "listSavedViews")
            .tag(TAG)
            .summary("The caller's views and the shared views available to them")
            .description(
                "Session only. Personal views first, then shared ones, each by name; not paged (at most 200 personal \
                 views per user and 500 shared views per instance, both contexts together, reported in `limits`). \
                 A shared view whose classes the caller may view none of is left out; one they may view some of \
                 comes without the others' keys, which `resolved.issues` only counts (`not_available`). Each view \
                 comes resolved against today's data model: `resolved.state` is `ok`, `degraded` (something that \
                 only narrows or presents results was dropped) or `unavailable` (a filter would disappear and widen \
                 the result, so the view has no `query` and is never applied). The stored definition is never \
                 rewritten.",
            )
            .session_only()
            .handle(|api, In(NoPath, Query(q), NoBody): In<NoPath, Query<ListSavedViewsQuery>, NoBody>| async move {
                Ok(Json(service::list(&api.pool, &api.ctx, &q).await?))
            }),
        route(Method::POST, VIEWS, "createSavedView")
            .tag(TAG)
            .summary("Save a view")
            .description(
                "Session only. `visibility: shared` needs `views.share` (403). The definition is checked like a list \
                 request: a class the caller cannot view is `400 unknown_class`, the same as an unknown key; an \
                 attribute sort needs a class (`class_required`) and an attribute every class has \
                 (`unknown_attribute`, `ambiguous_attribute`) that is not a reference (`not_sortable`); lookup \
                 lists and values must exist and be active (`unknown_lookup`); a search view needs `filters.q` and \
                 takes neither `sort` nor `columns` (`not_allowed`); at most 16 KiB (`too_large`). Names are unique \
                 per owner and context ignoring case (shared: per context), `409 CONFLICT duplicate_name`; at most \
                 200 personal views per user and 500 shared views (`409 CONFLICT limit_reached`). A shared view is \
                 audited as a `create` of `saved_views`; personal views are not audited.",
            )
            .session_only()
            .status(StatusCode::CREATED)
            .errors(&[ErrorCode::Forbidden, ErrorCode::Conflict])
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<CreateSavedView>>| async move {
                created(service::create(&api.pool, &api.ctx, &b).await?)
            }),
        route(Method::PUT, "/api/v1/saved-views/defaults", "setSavedViewDefault")
            .tag(TAG)
            .summary("Set or clear the caller's default view for an inventory list")
            .description(
                "Session only. `classKey` names the class list (null: the unscoped inventory); `viewId` null clears \
                 the default. The view must be readable by the caller (`404` otherwise), an inventory view \
                 (`400 not_inventory`), belong to that list (`400 home_mismatch`: its home is its class when it has \
                 exactly one, else the unscoped inventory) and not be `unavailable` (`400 unavailable`). One \
                 default per list; deleting the view removes it. Not audited.",
            )
            .session_only()
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<SetSavedViewDefault>>| async move {
                Ok(Json(service::set_default(&api.pool, &api.ctx, &b).await?))
            }),
        route(Method::GET, VIEW, "getSavedView")
            .tag(TAG)
            .summary("A saved view")
            .description(
                "Session only. Another user's personal view, and a shared view whose classes the caller may view \
                 none of, are `404`, the same as a view that does not exist.",
            )
            .session_only()
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(service::get(&api.pool, &api.ctx, id).await?))
            }),
        route(Method::PATCH, VIEW, "updateSavedView")
            .tag(TAG)
            .summary("Rename a view or change its definition")
            .description(
                "Session only. Send the `version` you loaded: `409 VERSION_CONFLICT` if someone saved in between. \
                 A personal view is changed by its owner, a shared view with `views.share` (403); context and \
                 visibility cannot change (copy instead). The definition is checked as on create; class keys of \
                 the stored definition the caller may not view are kept. When the view's home changes, defaults \
                 for its old home are removed. A shared view is audited as an `update` of `saved_views` with the \
                 view before and after.",
            )
            .session_only()
            .errors(&[ErrorCode::NotFound, ErrorCode::Forbidden, ErrorCode::Conflict, ErrorCode::VersionConflict])
            .handle(|api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<UpdateSavedView>>| async move {
                Ok(Json(service::update(&api.pool, &api.ctx, id, &b).await?))
            }),
        route(Method::DELETE, VIEW, "deleteSavedView")
            .tag(TAG)
            .summary("Delete a view")
            .description(
                "Session only. `?version=` is the version you loaded (`409 VERSION_CONFLICT` otherwise). A personal \
                 view is deleted by its owner, a shared view with `views.share` (403). Users who had it as their \
                 default get the standard list again. No CI is touched. A shared view is audited as a `delete` of \
                 `saved_views`.",
            )
            .session_only()
            .errors(&[ErrorCode::NotFound, ErrorCode::Forbidden, ErrorCode::VersionConflict])
            .handle(
                |api, In(IdPath(id), Query(q), NoBody): In<IdPath, Query<DeleteSavedViewQuery>, NoBody>| async move {
                    service::delete(&api.pool, &api.ctx, id, q.version).await?;
                    Ok(NoContent)
                },
            ),
        route(Method::POST, "/api/v1/saved-views/{id}/copy", "copySavedView")
            .tag(TAG)
            .summary("Copy a view (save as, copy to my views, share a copy)")
            .description(
                "Session only. Any view the caller can read; a shared copy needs `views.share` (403). The copy has \
                 the source's context and description and the part of its definition the caller may see; the \
                 source stays. Names and limits as on create. A shared copy is audited as a `create` of \
                 `saved_views` whose new value names the source in `copiedFrom`.",
            )
            .session_only()
            .status(StatusCode::CREATED)
            .errors(&[ErrorCode::NotFound, ErrorCode::Forbidden, ErrorCode::Conflict])
            .handle(|api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<CopySavedView>>| async move {
                created(service::copy(&api.pool, &api.ctx, id, &b).await?)
            }),
    ]
}
