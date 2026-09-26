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
                "Returns summaries (no attribute values). Soft-deleted CIs are hidden unless `deleted=include|only`.",
            )
            .handle(|api, In(NoPath, Query(q), NoBody): In<NoPath, Query<ListItemsQuery>, NoBody>| async move {
                Ok(Json(service::list(&api.pool, &q).await?))
            }),
        route(Method::GET, BY_ID, "getConfigurationItem")
            .tag(TAG)
            .summary("Get a CI with its attribute values")
            .description("Deleted CIs are still returned (with `deletedAt` set) so history and old links resolve.")
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(service::get(&api.pool, id).await?))
            }),
        route(Method::POST, BASE, "createConfigurationItem")
            .tag(TAG)
            .summary("Create a CI, including its attribute values")
            .status(StatusCode::CREATED)
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<CreateItemBody>>| async move {
                Ok(Json(service::create(&api.pool, &api.ctx, &b).await?))
            }),
        route(Method::PATCH, BY_ID, "updateConfigurationItem")
            .tag(TAG)
            .summary("Update a CI (partial); attributes are merged, null clears one")
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict, ErrorCode::VersionConflict])
            .handle(|api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<UpdateItemBody>>| async move {
                Ok(Json(service::update(&api.pool, &api.ctx, id, &b).await?))
            }),
        route(Method::DELETE, BY_ID, "deleteConfigurationItem")
            .tag(TAG)
            .summary("Delete a CI (soft delete)")
            .description(
                "Sets `deletedAt` on the CI and soft-deletes its live relationships in the same transaction. Both stay readable for history.",
            )
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                service::remove(&api.pool, &api.ctx, id).await?;
                Ok(NoContent)
            }),
        route(Method::GET, "/api/v1/configuration-items/{id}/graph", "getConfigurationItemGraph")
            .tag(TAG)
            .summary("Relationship graph around a CI in one call (nodes + edges)")
            .description(
                "Breadth-first traversal of live relationships up to `depth` hops. For a Server -> Application -> Database view, ask from the application with `direction=outgoing`, or from the server with `direction=both&depth=2`.",
            )
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), Query(q), NoBody): In<IdPath, Query<GraphQuery>, NoBody>| async move {
                Ok(Json(service::graph(&api.pool, id, &q).await?))
            }),
        route(Method::GET, "/api/v1/search", "searchConfigurationItems")
            .tag("Search")
            .summary("Global search across CIs, ranked, with the fields that matched")
            .description(
                "Matches name, hostname and serial number (substring), IP address (prefix, or containment when `q` is an IP or CIDR), notes (word prefix) and attribute values (text/enum substring, IP/CIDR prefix). Exact matches rank first, then name prefix, then trigram similarity.",
            )
            .handle(|api, In(NoPath, Query(q), NoBody): In<NoPath, Query<SearchQuery>, NoBody>| async move {
                Ok(Json(service::search(&api.pool, &q).await?))
            }),
    ]
}
