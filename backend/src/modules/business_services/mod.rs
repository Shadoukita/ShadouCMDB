//! Business services (v0.3.0, SHAA-927): a business service is a CI of the
//! built-in class `system_role = 'business_service'`; its members are live
//! relationships of the built-in type `system_role = 'business_service_member'`
//! (service = source, member = target); its owners are users and user groups
//! (`business_service_owners`). The service's own fields are created, edited
//! and deleted with the CI endpoints; these routes add the list with member
//! counts and owners, membership, owners, the "part of" view, the owner picker
//! and the settings the UI needs.

#[cfg(test)]
mod perf;
pub mod schemas;
pub mod service;
#[cfg(test)]
mod tests;

use axum::http::header::{self, HeaderValue};
use axum::http::{Method, StatusCode};
use utoipa::openapi::Required;
use utoipa::openapi::path::{Parameter, ParameterBuilder, ParameterIn};
use uuid::Uuid;

use crate::api::route::{
    Body, Csv, IdPath, In, Json, NoBody, NoContent, NoPath, NoQuery, PathInput, Query, Route, WithHeaders, route,
};
use crate::api::{schemas as api_schemas, validate};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use schemas::{
    BusinessServiceMembersAdd, BusinessServiceMembersRemove, BusinessServiceOwnersReplace, BusinessServiceQuery,
    MemberExportQuery, MemberQuery, PrincipalQuery,
};

/// CIs one add or remove request may name (fixed, SHAA-927 §1.3).
pub const MAX_BATCH: usize = 500;
/// Owners per role (fixed).
pub const MAX_OWNERS_PER_ROLE: usize = 10;

const TAG: &str = "Business services";

/// `{id}` (the service) and `{ciId}` (the member).
pub struct MemberPath(pub Uuid, pub Uuid);

fn uuid_param(name: &str) -> Parameter {
    ParameterBuilder::new()
        .name(name)
        .parameter_in(ParameterIn::Path)
        .required(Required::True)
        .schema(Some(api_schemas::uuid_builder()))
        .build()
}

impl PathInput for MemberPath {
    fn params() -> Vec<Parameter> {
        vec![uuid_param("id"), uuid_param("ciId")]
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
        match (get("id"), get("ciId")) {
            (Ok(id), Ok(ci)) => Ok(MemberPath(id, ci)),
            (a, b) => Err(AppError::validation([a.err(), b.err()].into_iter().flatten().collect())),
        }
    }
}

const VISIBILITY: &str = "Members of classes the caller may not view are neither listed nor counted, and an id of \
    such a CI answers exactly like an id that does not exist; `visibility` says `restricted` whenever the caller's \
    profile limits the classes they may view, whether or not anything is left out.";

pub fn routes() -> Vec<Route> {
    vec![
        route(Method::GET, "/api/v1/business-services", "listBusinessServices")
            .tag(TAG)
            .summary("List business services with criticality, owners and member counts")
            .description(format!(
                "Live business services (CIs of the built-in business service class; create, edit and delete them \
                 with the CI endpoints). `memberCount` counts the direct members the caller may view, \
                 `serviceMemberCount` those of them that are business services. Owners are shown by display name \
                 only. Needs view on the business service class (403 otherwise). {VISIBILITY}"
            ))
            .class_checked()
            .handle(|api, In(NoPath, Query(q), NoBody): In<NoPath, Query<BusinessServiceQuery>, NoBody>| async move {
                Ok(Json(service::list(&api.pool, &api.ctx, &q).await?))
            }),
        route(Method::GET, "/api/v1/business-services/{id}", "getBusinessService")
            .tag(TAG)
            .summary("One business service with its owners, member counts and limits")
            .description(
                "The service's fields (attributes, validity) come from GET /api/v1/configuration-items/{id}. 404 for \
                 a CI that is missing, deleted or not a business service, alike. Needs view on the business service \
                 class (403 otherwise).",
            )
            .errors(&[ErrorCode::NotFound])
            .class_checked()
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(service::get(&api.pool, &api.ctx, api.business_services, id).await?))
            }),
        route(Method::GET, "/api/v1/business-services/{id}/members", "listBusinessServiceMembers")
            .tag(TAG)
            .summary("The members of a business service (paginated)")
            .description(format!(
                "`ciId` answers \"which of these are members\" (the member picker). A `classId` that does not exist \
                 or that the caller may not view is refused with 400 VALIDATION_ERROR, the same for both. {VISIBILITY}"
            ))
            .errors(&[ErrorCode::NotFound])
            .class_checked()
            .handle(|api, In(IdPath(id), Query(q), NoBody): In<IdPath, Query<MemberQuery>, NoBody>| async move {
                Ok(Json(service::members(&api.pool, &api.ctx, id, &q).await?))
            }),
        route(Method::POST, "/api/v1/business-services/{id}/members", "addBusinessServiceMembers")
            .tag(TAG)
            .summary("Add members to a business service (all or nothing)")
            .description(format!(
                "Adds up to 500 CIs of any class. Any invalid id fails the whole request with 400 VALIDATION_ERROR and one detail \
                 per bad entry (`memberIds[<index>]`): `not_found` (missing, deleted or hidden CI, identical text), \
                 `membership_self`, `membership_cycle` (the CI is a business service that already includes this one, \
                 directly or nested), `membership_nesting_depth` (the chain of services including services would \
                 exceed BUSINESS_SERVICE_MAX_NESTING); and once on `memberIds`, `member_limit` (the members the \
                 caller may view plus the new ones exceed BUSINESS_SERVICE_MAX_MEMBERS). CIs that already are \
                 members are reported in `alreadyMembers`, not as an error. Recorded in the audit log as one \
                 relationship `create` per member plus one `update` on the service. Needs edit on the business \
                 service class and view on each member's class. {VISIBILITY}"
            ))
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict])
            .class_checked()
            .handle(
                |api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<BusinessServiceMembersAdd>>| async move {
                    Ok(Json(service::add_members(&api.pool, &api.ctx, api.business_services, id, &b).await?))
                },
            ),
        route(Method::POST, "/api/v1/business-services/{id}/members/remove", "removeBusinessServiceMembers")
            .tag(TAG)
            .summary("Remove members from a business service (all or nothing)")
            .description(
                "Up to 500 CIs; an id that is not a member the caller may view fails the whole request with 400 \
                 `not_found` on `memberIds[<index>]`. The CIs themselves are not changed. A POST because a DELETE \
                 with a body is poorly supported by proxies. Needs edit on the business service class.",
            )
            .status(StatusCode::NO_CONTENT)
            .errors(&[ErrorCode::NotFound])
            .class_checked()
            .handle(
                |api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<BusinessServiceMembersRemove>>| async move {
                    service::remove_members(&api.pool, &api.ctx, id, &b).await?;
                    Ok(NoContent)
                },
            ),
        route(Method::DELETE, "/api/v1/business-services/{id}/members/{ciId}", "removeBusinessServiceMember")
            .tag(TAG)
            .summary("Remove one member from a business service")
            .description(
                "404 when the CI is not a member, does not exist or is in a class the caller may not view, alike. \
                 Needs edit on the business service class.",
            )
            .errors(&[ErrorCode::NotFound])
            .class_checked()
            .handle(|api, In(MemberPath(id, ci), NoQuery, NoBody): In<MemberPath, NoQuery, NoBody>| async move {
                service::remove_member(&api.pool, &api.ctx, id, ci).await?;
                Ok(NoContent)
            }),
        route(Method::GET, "/api/v1/business-services/{id}/members/export", "exportBusinessServiceMembers")
            .tag(TAG)
            .summary("The members of a business service as CSV")
            .description(format!(
                "The member list's filters and sort without paging, at most BUSINESS_SERVICE_MAX_MEMBERS rows, as a \
                 CSV file (`Content-Disposition: attachment`). Every field is quoted; a value starting with =, +, -, \
                 @, a tab or a line break is prefixed with ' so spreadsheets do not run it as a formula. The first \
                 row is a comment with the service, the filters and the visibility note; then the columns ci_id, \
                 ident, name, class, criticality, is_service, active, added_at. Each export is recorded in the audit \
                 log (action `export` on the service, with the row count, never the rows). Needs view on the \
                 business service class. {VISIBILITY}"
            ))
            .errors(&[ErrorCode::NotFound])
            .class_checked()
            .handle(|api, In(IdPath(id), Query(q), NoBody): In<IdPath, Query<MemberExportQuery>, NoBody>| async move {
                let (name, body) = service::export(&api.pool, &api.ctx, api.business_services, id, q.into()).await?;
                let disposition = HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
                    .map_err(|_| AppError::internal())?;
                Ok(WithHeaders(
                    Csv(body),
                    vec![
                        (header::CONTENT_DISPOSITION, disposition),
                        (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
                    ],
                ))
            }),
        route(Method::PUT, "/api/v1/business-services/{id}/owners", "replaceBusinessServiceOwners")
            .tag(TAG)
            .summary("Replace the owners of a business service")
            .description(
                "Replaces both roles at once; the order in each array is the display order. Owners are users or \
                 user groups, at most 10 per role. A duplicate within a role is refused with 400 `duplicate`, an \
                 unknown or deleted user or group with 400 `not_found` (`technical[<index>]` / \
                 `business[<index>]`); a disabled user is accepted. `version` must be the service's current version \
                 (409 VERSION_CONFLICT otherwise); the change bumps it and is recorded as one `update` on the \
                 service. Needs edit on the business service class.",
            )
            .errors(&[ErrorCode::NotFound, ErrorCode::VersionConflict])
            .class_checked()
            .handle(
                |api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<BusinessServiceOwnersReplace>>| async move {
                    Ok(Json(service::replace_owners(&api.pool, &api.ctx, id, &b).await?))
                },
            ),
        route(Method::GET, "/api/v1/configuration-items/{id}/business-services", "listConfigurationItemServices")
            .tag(TAG)
            .summary("The business services a CI is part of, directly or through nested services")
            .description(
                "Walks membership only (the member relationship type), from the CI out to the services that include \
                 it and the services that include those, up to BUSINESS_SERVICE_MAX_NESTING + 1 levels. At most 200 \
                 services (`truncated` beyond, or when the walk stops at an impact analysis bound); not paginated. \
                 404 when the CI is missing, deleted or in a class the caller may not view. A caller without view on \
                 the business service class gets `data: []`. Runs under the impact analysis limits: 429 \
                 RATE_LIMITED or 503 SERVER_BUSY as getConfigurationItemImpact.",
            )
            .errors(&[ErrorCode::NotFound, ErrorCode::RateLimited, ErrorCode::ServerBusy])
            .class_checked()
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(service::part_of(&api.pool, &api.ctx, &api.impact, api.business_services, id).await?))
            }),
        route(Method::GET, "/api/v1/principals", "searchPrincipals")
            .tag(TAG)
            .summary("Look up users and groups to assign as owners")
            .description(
                "At most 20 users (by display name or username) and groups (by name) matching `q`, best matches \
                 first; a lookup, not a directory, so there is no paging. Users come with their username, so people \
                 with the same name can be told apart. Needs edit on the business service class or users.manage.",
            )
            .class_checked()
            .handle(|api, In(NoPath, Query(q), NoBody): In<NoPath, Query<PrincipalQuery>, NoBody>| async move {
                Ok(Json(service::principals(&api.pool, &api.ctx, &q).await?))
            }),
        route(Method::GET, "/api/v1/settings/business-services", "getBusinessServiceSettings")
            .tag(TAG)
            .summary("The business service class and member type, the caller's rights on them, and the limits")
            .description(
                "For any signed-in user. `classId` is the built-in business service class (also in the class list), \
                 `memberRelationshipTypeId` the built-in member type.",
            )
            .handle(|api, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| async move {
                Ok(Json(service::settings(&api.pool, &api.ctx, api.business_services).await?))
            }),
    ]
}
