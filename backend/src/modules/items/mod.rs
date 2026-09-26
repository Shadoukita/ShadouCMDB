//! Configuration items: inventory list, detail, CRUD, relationship graph and global search.

pub mod schemas;
pub mod service;

use axum::http::{Method, StatusCode};

use crate::api::route::{Body, IdPath, In, Json, NoBody, NoContent, NoPath, NoQuery, Query, Route, route};
use crate::http::error::ErrorCode;
use schemas::{CreateItemBody, GraphQuery, ListItemsQuery, SearchQuery, UpdateItemBody};

const TAG: &str = "Configuration items";
const BASE: &str = "/api/v1/configuration-items";
const BY_ID: &str = "/api/v1/configuration-items/{id}";

pub fn routes() -> Vec<Route> {
    vec![
        route(Method::GET, BASE, "listConfigurationItems")
            .tag(TAG)
            .summary("Inventory list: paginated, searchable, filterable, sortable")
            .description(
                "Returns summaries (no attribute values) of CIs in classes the caller may view. Soft-deleted CIs are hidden unless `deleted=include|only`.",
            )
            .handle(|api, In(NoPath, Query(q), NoBody): In<NoPath, Query<ListItemsQuery>, NoBody>| async move {
                Ok(Json(service::list(&api.pool, &api.ctx, &q).await?))
            }),
        route(Method::GET, BY_ID, "getConfigurationItem")
            .tag(TAG)
            .summary("Get a CI with its attribute values")
            .description("Deleted CIs are still returned (with `deletedAt` set) so history and old links resolve. Needs view on the CI's class.")
            .errors(&[ErrorCode::NotFound])
            .class_checked()
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(service::get(&api.pool, &api.ctx, id).await?))
            }),
        route(Method::POST, BASE, "createConfigurationItem")
            .tag(TAG)
            .summary("Create a CI, including its attribute values")
            .description("Needs create on the class.")
            .status(StatusCode::CREATED)
            .class_checked()
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<CreateItemBody>>| async move {
                Ok(Json(service::create(&api.pool, &api.ctx, &b).await?))
            }),
        route(Method::PATCH, BY_ID, "updateConfigurationItem")
            .tag(TAG)
            .summary("Update a CI (partial); attributes are merged, null clears one")
            .description("Needs edit on the CI's class (and create on the new class when `classId` changes).")
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict, ErrorCode::VersionConflict])
            .class_checked()
            .handle(|api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<UpdateItemBody>>| async move {
                Ok(Json(service::update(&api.pool, &api.ctx, id, &b).await?))
            }),
        route(Method::DELETE, BY_ID, "deleteConfigurationItem")
            .tag(TAG)
            .summary("Delete a CI (soft delete)")
            .description(
                "Sets `deletedAt` on the CI and soft-deletes its live relationships in the same transaction. Both stay readable for history. Needs delete on the CI's class.",
            )
            .errors(&[ErrorCode::NotFound])
            .class_checked()
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                service::remove(&api.pool, &api.ctx, id).await?;
                Ok(NoContent)
            }),
        route(Method::GET, "/api/v1/configuration-items/{id}/graph", "getConfigurationItemGraph")
            .tag(TAG)
            .summary("Relationship graph around a CI in one call (nodes + edges)")
            .description(
                "Breadth-first traversal of live relationships up to `depth` hops. For a Server -> Application -> Database view, ask from the application with `direction=outgoing`, or from the server with `direction=both&depth=2`. Needs view on the root's class; CIs of classes the caller may not view are left out (and not traversed).",
            )
            .errors(&[ErrorCode::NotFound])
            .class_checked()
            .handle(|api, In(IdPath(id), Query(q), NoBody): In<IdPath, Query<GraphQuery>, NoBody>| async move {
                Ok(Json(service::graph(&api.pool, &api.ctx, id, &q).await?))
            }),
        route(Method::GET, "/api/v1/search", "searchConfigurationItems")
            .tag("Search")
            .summary("Global search across CIs, ranked, with the fields that matched")
            .description(
                "Matches name, hostname and serial number (substring), IP address (prefix, or containment when `q` is an IP or CIDR), notes (word prefix) and attribute values (text/enum substring, IP/CIDR prefix). Exact matches rank first, then name prefix, then trigram similarity. Only CIs in classes the caller may view.",
            )
            .handle(|api, In(NoPath, Query(q), NoBody): In<NoPath, Query<SearchQuery>, NoBody>| async move {
                Ok(Json(service::search(&api.pool, &api.ctx, &q).await?))
            }),
    ]
}
