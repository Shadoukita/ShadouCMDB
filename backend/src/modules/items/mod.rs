//! Configuration items: inventory list, detail, CRUD, relationship graph and global search.

pub mod plan;
pub mod schemas;
pub mod service;

pub use plan::value_schema;

use axum::http::{Method, StatusCode};

use crate::api::route::{CheckedBody, IdPath, In, Json, NoBody, NoContent, NoPath, NoQuery, Query, Route, route};
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
                "Returns CIs in classes the caller may view, each with its attribute values (`attributes`, `attributeReferences`) as on `getConfigurationItem`. Only active CIs (inside their validity period) unless `active=false|all`; soft-deleted CIs are hidden unless `deleted=include|only`.",
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
            .description(
                "Needs create on the class. The ident is generated unless an administrator sends one (403 for anyone else, 409 when another CI has it). The label follows from the class's title attribute.",
            )
            .status(StatusCode::CREATED)
            .errors(&[ErrorCode::Conflict])
            .class_checked()
            .handle(|api, In(NoPath, NoQuery, CheckedBody(b)): In<NoPath, NoQuery, CheckedBody<CreateItemBody>>| async move {
                match b {
                    Ok(b) => Ok(Json(service::create(&api.pool, &api.ctx, &b).await?)),
                    Err(invalid) => Err(service::create_errors(&api.pool, &api.ctx, invalid).await),
                }
            }),
        route(Method::PATCH, BY_ID, "updateConfigurationItem")
            .tag(TAG)
            .summary("Update a CI (partial); attributes are merged, null clears one")
            .description(
                "Needs edit on the CI's class (and create on the new class when `classId` changes). A business service keeps its class and no CI moves into the business service class (400 `business_service_class` on `classId`, the same for every service). `validUntil` must be after `validFrom`, each taken from the body or else the stored value (400 on the field sent). Changing `ident` is for administrators only (403 for anyone else; resending the current value is allowed) and is recorded in the audit log like every change. Resending the value a reference attribute already holds is no change: it is accepted whether the referenced CI is live, deleted or in a class the caller may not view. A new reference must be a live CI the caller may view (else `not_found`, as for a missing CI) of the attribute's reference class.",
            )
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict, ErrorCode::VersionConflict])
            .class_checked()
            .handle(|api, In(IdPath(id), NoQuery, CheckedBody(b)): In<IdPath, NoQuery, CheckedBody<UpdateItemBody>>| async move {
                match b {
                    Ok(b) => Ok(Json(service::update(&api.pool, &api.ctx, id, &b).await?)),
                    Err(invalid) => Err(service::update_errors(&api.pool, &api.ctx, id, invalid).await),
                }
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
                "Matches label and ident (substring and word prefix) and attribute values (text/enum substring, IP/CIDR prefix, IP containment when `q` is an IP or CIDR). Exact label or ident matches rank first, then label prefix, then trigram similarity. Only CIs in classes the caller may view; only active CIs unless `active=false|all`.",
            )
            .handle(|api, In(NoPath, Query(q), NoBody): In<NoPath, Query<SearchQuery>, NoBody>| async move {
                Ok(Json(service::search(&api.pool, &api.ctx, &q).await?))
            }),
    ]
}
