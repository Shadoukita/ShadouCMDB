//! Administration > Workflows: the design-time API of the workflow engine
//! (v0.4.0 slice S2, SHAA-1423; design on SHAA-1411 §3.3, §3.4, §6.1, §6.3).
//!
//! A definition is attached to one type and holds the mutable settings and
//! the transition grants. Its graph lives in versions: one draft, edited as a
//! whole, then published as an immutable version (the database refuses any
//! change to a published graph). Everything here needs `workflows.manage`.
//! Running workflows on CIs is the runtime API (S3).

pub mod condition;
pub mod graph;
pub mod refs;
pub mod schemas;
pub mod service;
#[cfg(test)]
mod tests;

use axum::extract::RawPathParams;
use axum::http::{Method, StatusCode};
use utoipa::openapi::Required;
use utoipa::openapi::path::{Parameter, ParameterBuilder, ParameterIn};
use utoipa::openapi::schema::{ObjectBuilder, Type};
use uuid::Uuid;

use self::schemas::*;
use crate::api::route::{Body, IdPath, In, Json, NoBody, NoContent, NoPath, NoQuery, PathInput, Query, Route, route};
use crate::api::{schemas as api_schemas, validate};
use crate::auth::permissions::GlobalPermission;
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};

/// `{id}/versions/{no}`: a definition and one of its version numbers.
pub struct VersionNoPath(pub Uuid, pub i32);

fn param_error(field: &str, message: &str) -> AppError {
    AppError::validation(vec![FieldError {
        location: FieldLocation::Params,
        field: field.into(),
        message: message.into(),
        code: "invalid_format".into(),
    }])
}

impl PathInput for VersionNoPath {
    fn params() -> Vec<Parameter> {
        vec![
            ParameterBuilder::new()
                .name("id")
                .parameter_in(ParameterIn::Path)
                .required(Required::True)
                .schema(Some(api_schemas::uuid_builder()))
                .build(),
            ParameterBuilder::new()
                .name("no")
                .parameter_in(ParameterIn::Path)
                .required(Required::True)
                .description(Some("Version number"))
                .schema(Some(ObjectBuilder::new().schema_type(Type::Integer).minimum(Some(1)).maximum(Some(i32::MAX))))
                .build(),
        ]
    }
    fn parse(raw: &RawPathParams) -> Result<Self, AppError> {
        let get = |name: &str| raw.iter().find(|(k, _)| *k == name).map(|(_, v)| v).unwrap_or_default();
        let id = Some(get("id"))
            .filter(|v| validate::is_uuid(v))
            .and_then(|v| Uuid::parse_str(v).ok())
            .ok_or_else(|| param_error("id", "Invalid UUID"))?;
        let no = get("no")
            .parse::<i32>()
            .ok()
            .filter(|n| *n >= 1)
            .ok_or_else(|| param_error("no", "Expected a positive integer"))?;
        Ok(VersionNoPath(id, no))
    }
}

const TAG: &str = "Workflow definitions";
const BASE: &str = "/api/v1/admin/workflow-definitions";
const BY_ID: &str = "/api/v1/admin/workflow-definitions/{id}";
const VERSIONS: &str = "/api/v1/admin/workflow-definitions/{id}/versions";
const VERSION: &str = "/api/v1/admin/workflow-definitions/{id}/versions/{no}";
const RETIRE: &str = "/api/v1/admin/workflow-definitions/{id}/versions/{no}/retire";
const DRAFT: &str = "/api/v1/admin/workflow-definitions/{id}/draft";
const VALIDATE: &str = "/api/v1/admin/workflow-definitions/{id}/draft/validate";
const PUBLISH: &str = "/api/v1/admin/workflow-definitions/{id}/draft/publish";
const GRANTS: &str = "/api/v1/admin/workflow-definitions/{id}/grants";

pub fn routes() -> Vec<Route> {
    let manage = GlobalPermission::WorkflowsManage;
    vec![
        route(Method::GET, BASE, "listWorkflowDefinitions")
            .tag(TAG)
            .summary("List workflow definitions (paginated; filter by type key, active flag or text)")
            .requires(manage)
            .handle(
                |api, In(NoPath, Query(q), NoBody): In<NoPath, Query<WorkflowDefinitionList>, NoBody>| async move {
                    Ok(Json(service::list(&api.pool, &q).await?))
                },
            ),
        route(Method::POST, BASE, "createWorkflowDefinition")
            .tag(TAG)
            .summary("Create a workflow definition with an empty draft (version 1)")
            .description(
                "The key and the type never change. A new workflow is inactive unless `isActive: true` is sent. \
                 `stateAttributeId` names an active lookup field of the type (own or inherited) that the workflow \
                 keeps in step with its state; only one active workflow may drive a field (409 CONFLICT \
                 `state_attribute_driven`). An active workflow with a state field returns the `UNINSTANCED_CIS` \
                 warning, as for an update. 409 CONFLICT on `key` when it is taken (regardless of case).",
            )
            .status(StatusCode::CREATED)
            .requires(manage)
            .session_only()
            .errors(&[ErrorCode::Conflict])
            .handle(
                |api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<WorkflowDefinitionCreate>>| async move {
                    Ok(Json(service::create(&api.pool, &api.ctx, &b).await?))
                },
            ),
        route(Method::GET, BY_ID, "getWorkflowDefinition")
            .tag(TAG)
            .summary("Get a workflow definition, with its current version number and its draft's checksum")
            .requires(manage)
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(service::get(&api.pool, id).await?))
            }),
        route(Method::PATCH, BY_ID, "updateWorkflowDefinition")
            .tag(TAG)
            .summary("Change a workflow's name, description or flags")
            .description(
                "Send the `version` you loaded: 409 VERSION_CONFLICT if the workflow, its grants or its versions \
                 changed in between. `stateAttributeId` can change only until a version is published (409 \
                 CONFLICT `published`). Inactive workflows start no new instances; running ones continue. When the \
                 change makes the workflow an active driver of a state field (activating it, or giving an active \
                 one a state field), `warnings` carries `UNINSTANCED_CIS` with the number of live CIs it covers that \
                 have no running instance: their state field cannot be edited until instances are started on them \
                 (`count` is null when the caller may not view all the types it spans).",
            )
            .requires(manage)
            .session_only()
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict, ErrorCode::VersionConflict])
            .handle(
                |api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<WorkflowDefinitionUpdate>>| async move {
                    Ok(Json(service::update(&api.pool, &api.ctx, id, &b).await?))
                },
            ),
        route(Method::DELETE, BY_ID, "deleteWorkflowDefinition")
            .tag(TAG)
            .summary("Delete a workflow that never ran, with its versions and grants")
            .description(
                "409 IN_USE `has_instances` once the workflow has had an instance on any CI: its history keeps it. \
                 Deactivate it instead.",
            )
            .status(StatusCode::NO_CONTENT)
            .requires(manage)
            .session_only()
            .errors(&[ErrorCode::NotFound, ErrorCode::InUse])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                service::remove(&api.pool, &api.ctx, id).await?;
                Ok(NoContent)
            }),
        route(Method::GET, VERSIONS, "listWorkflowVersions")
            .tag(TAG)
            .summary("List a workflow's versions, newest first, without their graphs (the draft included)")
            .requires(manage)
            .errors(&[ErrorCode::NotFound])
            .handle(
                |api, In(IdPath(id), Query(q), NoBody): In<IdPath, Query<WorkflowVersionList>, NoBody>| async move {
                    Ok(Json(service::versions(&api.pool, &api.ctx, id, &q).await?))
                },
            ),
        route(Method::GET, VERSION, "getWorkflowVersion")
            .tag(TAG)
            .summary("Get one version of a workflow with its whole graph")
            .requires(manage)
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(VersionNoPath(id, no), NoQuery, NoBody): In<VersionNoPath, NoQuery, NoBody>| async move {
                Ok(Json(service::version(&api.pool, id, no).await?))
            }),
        route(Method::POST, RETIRE, "retireWorkflowVersion")
            .tag(TAG)
            .summary("Retire a published version: no new instance starts on it")
            .description(
                "Running instances stay on it until they are migrated. When it was the current version, the newest \
                 version still published becomes current (none: the workflow starts no instances until the next \
                 publish). 409 CONFLICT for a draft or an already retired version. Bumps the workflow's `version`.",
            )
            .requires(manage)
            .session_only()
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict])
            .handle(|api, In(VersionNoPath(id, no), NoQuery, NoBody): In<VersionNoPath, NoQuery, NoBody>| async move {
                Ok(Json(service::retire(&api.pool, &api.ctx, id, no).await?))
            }),
        route(Method::GET, DRAFT, "getWorkflowDraft")
            .tag(TAG)
            .summary("Get the draft graph (404 when there is none)")
            .requires(manage)
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(service::draft(&api.pool, id).await?))
            }),
        route(Method::PUT, DRAFT, "replaceWorkflowDraft")
            .tag(TAG)
            .summary("Replace the whole draft graph (creating the draft as the next version when there is none)")
            .description(
                "States, transitions and their fields are named by key; fields are fields of the workflow's type \
                 (own or inherited) and `stateValue` is the key of a value of the state field's list. Anything that \
                 cannot be resolved, duplicate keys, transitions to unknown states or to their own state, and \
                 malformed or ill-typed conditions are 400 VALIDATION_ERROR with the path of each problem. Whether \
                 the graph can be published is the lint's question (`draft/validate`). With `expectedChecksum`, \
                 409 VERSION_CONFLICT if the draft changed (or was published or deleted) since it was loaded. The \
                 response carries the new `checksum`. Drafts are not audited; publishing is.",
            )
            .requires(manage)
            .session_only()
            .body_limit(1024 * 1024)
            .errors(&[ErrorCode::NotFound, ErrorCode::VersionConflict])
            .handle(
                |api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<WorkflowDraftReplace>>| async move {
                    Ok(Json(service::replace_draft(&api.pool, id, &b).await?))
                },
            ),
        route(Method::DELETE, DRAFT, "deleteWorkflowDraft")
            .tag(TAG)
            .summary("Discard the draft")
            .status(StatusCode::NO_CONTENT)
            .requires(manage)
            .session_only()
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                service::delete_draft(&api.pool, id).await?;
                Ok(NoContent)
            }),
        route(Method::POST, VALIDATE, "validateWorkflowDraft")
            .tag(TAG)
            .summary("Lint the draft as publishing would, without publishing")
            .description(
                "Errors refuse publishing: no states, no initial state or a terminal one, unreachable states, \
                 non-terminal states without a way out, states from which no terminal state can be reached, terminal \
                 states with outgoing transitions, fields or conditions on fields that are not (or no longer) on the \
                 type, archived or of the wrong type, and state values outside the state field's list or retired. \
                 Warnings do not: a transition no profile is granted (only administrators could run it). Always 200; \
                 `valid` tells whether publishing would go ahead.",
            )
            .requires(manage)
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(service::validate_draft(&api.pool, id).await?))
            }),
        route(Method::POST, PUBLISH, "publishWorkflowDraft")
            .tag(TAG)
            .summary("Publish the draft as the workflow's current version")
            .description(
                "`expectedDraftChecksum` is the checksum of the draft you validated: 409 VERSION_CONFLICT if it \
                 changed in between. A draft the lint finds errors in is 400 VALIDATION_ERROR with one detail per \
                 problem (`field` is its path in the draft body, `code` the lint code). The published version can \
                 never change again; running instances stay on the version they started on. Every field it depends \
                 on is recorded, so archiving, retyping or purging those fields is refused (409 IN_USE). Audited as \
                 `workflow.publish` with the whole graph. Bumps the workflow's `version`.",
            )
            .status(StatusCode::CREATED)
            .requires(manage)
            .session_only()
            .errors(&[ErrorCode::NotFound, ErrorCode::VersionConflict])
            .handle(|api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<WorkflowPublish>>| async move {
                Ok(Json(service::publish(&api.pool, &api.ctx, id, &b).await?))
            }),
        route(Method::GET, GRANTS, "getWorkflowGrants")
            .tag(TAG)
            .summary("Who may run which transition of a workflow")
            .description(
                "Grants are per transition key and permission profile, for every version of the workflow. `_cancel` \
                 is the grant to cancel an instance. Running a transition also needs the edit right on the CI's type; \
                 administrators may run every transition.",
            )
            .requires(manage)
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(service::grants(&api.pool, id).await?))
            }),
        route(Method::PUT, GRANTS, "replaceWorkflowGrants")
            .tag(TAG)
            .summary("Replace who may run which transition of a workflow")
            .description(
                "`grants` is the complete new set; profiles are given by id or by name. Send the workflow's \
                 `version`: 409 VERSION_CONFLICT if it changed in between. An unknown profile is 400 \
                 VALIDATION_ERROR `not_found` on `grants[i].profiles[j]`. A change bumps the workflow's version and \
                 is audited with the grants before and after, by profile name.",
            )
            .requires(manage)
            .session_only()
            .errors(&[ErrorCode::NotFound, ErrorCode::VersionConflict])
            .handle(
                |api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<WorkflowGrantsReplace>>| async move {
                    Ok(Json(service::replace_grants(&api.pool, &api.ctx, id, &b).await?))
                },
            ),
    ]
}
