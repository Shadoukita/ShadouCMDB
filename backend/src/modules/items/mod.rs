//! Configuration items: inventory list, detail, CRUD, relationship graph and global search.

pub mod facets;
pub mod plan;
pub mod schemas;
pub mod service;

pub use plan::value_schema;

use axum::http::{Method, StatusCode};

use crate::auth::permissions::GlobalPermission;

use crate::api::route::{CheckedBody, IdPath, In, Json, NoBody, NoContent, NoPath, NoQuery, Query, Route, route};
use crate::http::error::ErrorCode;
use schemas::{
    ChangeHistogramQuery, CreateItemBody, FacetsQuery, GraphQuery, ListItemsQuery, SearchQuery, UpdateItemBody,
};

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
        route(Method::GET, "/api/v1/configuration-items/change-histogram", "getConfigurationItemChangeHistogram")
            .tag(TAG)
            .summary("Changes per hour or day to the CIs of an inventory query")
            .description(
                "Takes the filters of `listConfigurationItems` and counts, per bucket, the audit log entries on the CIs that match them now: `created` (create), `statusChanged` (an update that changed the `status` attribute, with or without other fields) and `updated` (every other update, deletion and restore). Exports and other read events are not counted. Buckets are aligned to UTC hours or days and every bucket of the range is returned, empty ones included. The range is `from` (inclusive) to `to` (exclusive), at most 7 days with `bucket=hour` and 90 days with `bucket=day` (400 `range_too_large` on `from`; 400 `invalid_range` when `from` is not before `to`). Needs `audit.view`, like the audit log the counts come from; a caller whose profile limits the classes they may view counts only CIs of those classes, and only the entries `listAuditLog` would show them.",
            )
            .requires(GlobalPermission::AuditView)
            .handle(|api, In(NoPath, Query(q), NoBody): In<NoPath, Query<ChangeHistogramQuery>, NoBody>| async move {
                Ok(Json(service::change_histogram(&api.pool, &api.ctx, &q).await?))
            }),
        route(Method::GET, "/api/v1/configuration-items/facets", "getConfigurationItemFacets")
            .tag(TAG)
            .summary("Facet counts for the inventory list: CIs per class, criticality, lookup value and business service")
            .description(
                "Takes the filters of `listConfigurationItems` and returns, per facet, the values with how many CIs match. Each facet is counted with its own filter left out and every other filter applied (class counts ignore `classId`, a lookup list's counts ignore that list's values in `lookupValueId`), so the counts are what ticking one more value would add. `total` is the count with every filter. Facets: `class` (per exact class), `criticality`, one `lookup.<list key>` per lookup list stored in a lookup attribute (status, environment, location, ...), and `businessService` (direct members) when business services exist. Counts are exact and cover only CIs in classes the caller may view, like the list; a business service the caller may not view is not a facet value.",
            )
            .handle(|api, In(NoPath, Query(q), NoBody): In<NoPath, Query<FacetsQuery>, NoBody>| async move {
                Ok(Json(facets::facets(&api.pool, &api.ctx, &q).await?))
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
