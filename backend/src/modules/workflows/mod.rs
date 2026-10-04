//! Administration > Workflows: the design-time API of the workflow engine
//! (v0.4.0 slice S2, SHAA-1423; design on SHAA-1411 §3.3, §3.4, §6.1, §6.3).
//!
//! A definition is attached to one type and holds the mutable settings and
//! the transition grants. Its graph lives in versions: one draft, edited as a
//! whole, then published as an immutable version (the database refuses any
//! change to a published graph). Everything here needs `workflows.manage`.
//! Running workflows on CIs is the runtime API ([`runtime`], S3), whose
//! routes are [`runtime_routes`].

pub mod adopt;
#[cfg(test)]
mod adopt_tests;
pub mod archive;
pub mod condition;
pub mod eval;
pub mod graph;
pub mod migration;
#[cfg(test)]
mod perf;
pub mod refs;
pub mod runtime;
pub mod runtime_schemas;
#[cfg(test)]
mod runtime_tests;
#[cfg(test)]
mod s6_tests;
pub mod schemas;
pub mod service;
pub mod state_field;
#[cfg(test)]
mod tests;

use axum::extract::RawPathParams;
use axum::http::{Method, StatusCode};
use utoipa::openapi::Required;
use utoipa::openapi::path::{Parameter, ParameterBuilder, ParameterIn};
use utoipa::openapi::schema::{ObjectBuilder, Type};
use uuid::Uuid;

use self::runtime_schemas::*;
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
const BOOTSTRAP: &str = "/api/v1/admin/workflow-definitions/{id}/bootstrap";
const MIGRATIONS: &str = "/api/v1/admin/workflow-definitions/{id}/instance-migrations";

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
        route(Method::POST, BOOTSTRAP, "bootstrapWorkflowInstances")
            .tag(TAG)
            .summary("Start the workflow on the existing CIs it covers, each in the state of its state field value")
            .description(
                "For adopting a workflow on an inventory that already exists (an active workflow makes its state \
                 field read-only on every CI it covers, Q3). Every live CI of the covered types without a running \
                 instance of the workflow gets one, on the current published version, in the state whose state \
                 field value is the CI's current one (a value several states map is the first non-terminal one's). \
                 CIs whose value no state maps, or none, are reported in `unmapped` and skipped; CIs in a terminal \
                 state are reported and skipped. Runs in batches of 1,000 CIs, each committed on its own; a run \
                 that stops part way is finished by running it again, since CIs that run the workflow are left \
                 alone (a second run starts nothing). Each start is a `start` event and a `workflow.start` audit \
                 row on the CI with `actor_type = system`, naming the caller in `requestedBy`. `dryRun: true` only \
                 counts. `stateFromAttribute` must be true. 409 CONFLICT `no_state_field`, `unpublished`, \
                 `inactive`, or `changed_during_bootstrap` when the workflow is deactivated or published again while \
                 it runs. 403 when the workflow covers types the caller may not view.",
            )
            .requires(manage)
            .session_only()
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict])
            .handle(
                |api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<WorkflowBootstrap>>| async move {
                    Ok(Json(adopt::bootstrap(&api.pool, &api.ctx, id, &b).await?))
                },
            ),
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
                 VALIDATION_ERROR `not_found` on `grants[i].profiles[j]`, and a transition key that is neither `_cancel` \
                 nor a transition of any version or the draft is 400 `unknown_transition` on \
                 `grants[i].transitionKey`. A change bumps the workflow's version and \
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
        route(Method::POST, MIGRATIONS, "migrateWorkflowInstances")
            .tag(TAG)
            .summary("Move the running instances of one version to a newer version (or report what would move)")
            .description(
                "Every running instance on `fromVersionNo` moves to `toVersionNo` (a newer, published version), into \
                 the state `stateMap` names for its current state; a state left out of `stateMap` moves to the state \
                 of the same key, when the target version has one that is not terminal. With `dryRun: true` nothing \
                 is written and the response tells how many instances each state holds and where they would go. \
                 400 VALIDATION_ERROR: `unknown_version` or `not_newer` on the version numbers; `unknown_state`, \
                 `terminal_source`, `unknown_target_state` or `terminal_target` on `stateMap.<key>`; `unmapped` on \
                 `stateMap.<key>` for a state with running instances and nowhere to go (in a dry run too). 409 \
                 CONFLICT `not_published` when the target version is a draft or retired. 403 FORBIDDEN unless the \
                 caller may view and edit every type the workflow runs on. A real run moves up to 1,000 instances \
                 per transaction, locking each CI before its instance: a run cut short leaves the moved batches \
                 moved, and running it again moves the rest. Each moved instance keeps its CI and its history, gets \
                 a `migrate` event, and is audited on its CI as `workflow.migrate` (version and state before and \
                 after); when the workflow drives a state field and the new state maps to another value, the CI's \
                 field is written (a CI `update` audit row).",
            )
            .requires(manage)
            .session_only()
            .class_checked()
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict])
            .handle(
                |api,
                 In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<WorkflowInstanceMigration>>| async move {
                    Ok(Json(migration::migrate(&api.pool, &api.ctx, id, &b).await?))
                },
            ),
        route(Method::GET, "/api/v1/admin/workflow-archive", "listArchivedWorkflowInstances")
            .tag(TAG)
            .summary("The workflow history of CIs deleted for good, newest first (paginated)")
            .description(
                "When a CI is deleted for good (its type is purged), its workflow instances move here with all of \
                 their events, in the same transaction; `requestId` joins the CI's `delete` audit row. The archive \
                 is never changed or deleted, and audit log retention does not touch it. Entries are listed only to \
                 a caller whose permission profile does not limit the types they may view (the CIs' types may no \
                 longer exist to judge by); to anyone else the list is empty.",
            )
            .requires(manage)
            .handle(
                |api, In(NoPath, Query(q), NoBody): In<NoPath, Query<WorkflowArchiveList>, NoBody>| async move {
                    Ok(Json(archive::list(&api.pool, &api.ctx, &q).await?))
                },
            ),
    ]
}

const RUN_TAG: &str = "Workflow instances";
const INSTANCES: &str = "/api/v1/workflow-instances";
const INSTANCE: &str = "/api/v1/workflow-instances/{id}";

/// The runtime API: workflow instances on CIs. Open to API tokens; every
/// operation answers 404 for a CI the caller may not view.
pub fn runtime_routes() -> Vec<Route> {
    vec![
        route(Method::GET, INSTANCES, "listWorkflowInstances")
            .tag(RUN_TAG)
            .summary("List workflow instances on the CIs you may view (paginated, filterable)")
            .description(
                "Instances on CIs of types the caller may not view are left out of the page and of `page.total`. \
                 Sorted by `lastTransitionAt`, newest first, unless `sort` says otherwise.",
            )
            .handle(
                |api, In(NoPath, Query(q), NoBody): In<NoPath, Query<WorkflowInstanceList>, NoBody>| async move {
                    Ok(Json(runtime::list(&api.pool, &api.ctx, &q).await?))
                },
            ),
        route(Method::GET, "/api/v1/workflow-instances/summary", "getWorkflowInstanceSummary")
            .tag(RUN_TAG)
            .summary("Count running instances per workflow and state, on the CIs you may view (for dashboards)")
            .handle(
                |api,
                 In(NoPath, Query(q), NoBody): In<NoPath, Query<WorkflowInstanceSummaryQuery>, NoBody>| async move {
                    Ok(Json(runtime::summary(&api.pool, &api.ctx, &q).await?))
                },
            ),
        route(Method::POST, INSTANCES, "startWorkflowInstance")
            .tag(RUN_TAG)
            .summary("Start a workflow on a CI")
            .description(
                "Needs the edit right on the CI's type. The instance starts in the initial state of the workflow's \
                 current version and stays on that version. When the workflow drives a state field and the initial \
                 state maps to one of its values, the CI's field is set (a CI `update` audit row). 404 when the CI \
                 or the workflow does not exist or is of a type the caller may not view. 400 VALIDATION_ERROR \
                 `not_covered` when the workflow does not run on the CI's type. 409 CONFLICT `deleted` (the CI is \
                 deleted), `unpublished`, `inactive` or `already_running` (one running instance per workflow and \
                 CI). Audited on the CI as `workflow.start`.",
            )
            .status(StatusCode::CREATED)
            .class_checked()
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict])
            .handle(
                |api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<WorkflowInstanceStart>>| async move {
                    Ok(Json(runtime::start(&api.pool, &api.ctx, &b).await?))
                },
            ),
        route(Method::POST, "/api/v1/workflow-instances/bulk-transitions", "runWorkflowTransitionsInBulk")
            .tag(RUN_TAG)
            .summary("Run up to 500 transitions in one request, with a result per item")
            .description(
                "Each item is checked and run exactly as `POST /workflow-instances/{id}/transitions` would run it, \
                 with the same rights, grants, validation and audit rows. All items run in one transaction, each in \
                 a savepoint: an item that is refused is rolled back alone and reported with the error the single \
                 endpoint would have answered (`code`, `message`, `details`); the items that ran are committed \
                 together. Always 200 for a well-formed body: `succeeded` and `failed` count the items, `results` \
                 has one entry per item in the request's order. Items run in the order of their CIs, so concurrent \
                 bulk runs over the same CIs queue instead of deadlocking; items on the same instance run in the \
                 request's order, each against the `expectedVersion` it names. A server fault rolls back every \
                 item (500).",
            )
            .handle(
                |api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<WorkflowBulkTransitions>>| async move {
                    Ok(Json(runtime::bulk_transitions(&api.pool, &api.ctx, &b).await?))
                },
            ),
        route(Method::GET, INSTANCE, "getWorkflowInstance")
            .tag(RUN_TAG)
            .summary("Get a workflow instance with the graph of its version and the transitions you may run")
            .description(
                "`availableTransitions` lists the transitions out of the current state that the caller is granted \
                 and may run (the edit right on the CI's type), each with its fields and the conditions that fail on \
                 the CI's current values (`blockedBy`). Transitions the caller is not granted are left out. 404 for \
                 an instance on a CI of a type the caller may not view.",
            )
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(runtime::get(&api.pool, &api.ctx, id).await?))
            }),
        route(Method::GET, "/api/v1/workflow-instances/{id}/events", "listWorkflowInstanceEvents")
            .tag(RUN_TAG)
            .summary("The history of a workflow instance, oldest step first (paginated)")
            .description(
                "Events are kept for the life of the CI; audit log retention does not remove them. `requestId` joins \
                 an event to the audit rows of the same request.",
            )
            .errors(&[ErrorCode::NotFound])
            .handle(
                |api, In(IdPath(id), Query(q), NoBody): In<IdPath, Query<WorkflowEventList>, NoBody>| async move {
                    Ok(Json(runtime::events(&api.pool, &api.ctx, id, &q).await?))
                },
            ),
        route(Method::POST, "/api/v1/workflow-instances/{id}/transitions", "runWorkflowTransition")
            .tag(RUN_TAG)
            .summary("Move a workflow instance along a transition")
            .description(
                "One transaction: the fields sent are validated as PATCH /configuration-items/{id} validates them \
                 (400 VALIDATION_ERROR on `fields.<key>`, and `not_a_transition_field` for a field the transition \
                 does not list); required fields, the comment and the conditions are then checked on the CI's \
                 values with the ones sent (422 WORKFLOW_CONDITION_FAILED, one detail each: `required`, \
                 `comment_required`, `condition`). The fields and the state field are written to the CI (a CI \
                 `update` audit row), the instance moves on (and completes on a terminal state), and the step is \
                 audited on the CI as `workflow.transition`. Needs the edit right on the CI's type and a grant of \
                 the transition to one of the caller's profiles; with an API token, to the token's profile as \
                 well (403 FORBIDDEN). 400 `unknown_transition` for a key the version does not have; 409 CONFLICT \
                 `not_from_current_state` or `not_active`; 409 VERSION_CONFLICT on a stale `expectedVersion`.",
            )
            .class_checked()
            .errors(&[
                ErrorCode::NotFound,
                ErrorCode::Conflict,
                ErrorCode::VersionConflict,
                ErrorCode::WorkflowConditionFailed,
            ])
            .handle(
                |api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<WorkflowTransitionRun>>| async move {
                    Ok(Json(runtime::transition(&api.pool, &api.ctx, id, &b).await?))
                },
            ),
        route(Method::POST, "/api/v1/workflow-instances/{id}/cancel", "cancelWorkflowInstance")
            .tag(RUN_TAG)
            .summary("Cancel a running workflow instance")
            .description(
                "Needs `workflows.manage`, or the edit right on the CI's type and the workflow's `_cancel` grant. \
                 The CI's fields stay as they are. Audited on the CI as `workflow.cancel` with the reason. 409 \
                 CONFLICT `not_active`; 409 VERSION_CONFLICT on a stale `expectedVersion`.",
            )
            .class_checked()
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict, ErrorCode::VersionConflict])
            .handle(
                |api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<WorkflowInstanceCancel>>| async move {
                    Ok(Json(runtime::cancel(&api.pool, &api.ctx, id, &b).await?))
                },
            ),
        route(Method::POST, "/api/v1/workflow-instances/{id}/force", "forceWorkflowInstanceState")
            .tag(RUN_TAG)
            .summary("Put a running workflow instance into another state of its version, bypassing transitions")
            .description(
                "For administrators repairing an instance: needs `workflows.manage` and the edit right on the CI's \
                 type. No condition, field or grant is checked; the state field is written as a transition would. \
                 A terminal state completes the instance. Audited on the CI as `workflow.force` with the mandatory \
                 reason. 400 `unknown_state`; 409 CONFLICT `same_state` or `not_active`; 409 VERSION_CONFLICT on a \
                 stale `expectedVersion`.",
            )
            .requires(GlobalPermission::WorkflowsManage)
            .class_checked()
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict, ErrorCode::VersionConflict])
            .handle(
                |api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<WorkflowInstanceForce>>| async move {
                    Ok(Json(runtime::force(&api.pool, &api.ctx, id, &b).await?))
                },
            ),
        route(Method::GET, "/api/v1/configuration-items/{id}/workflows", "getConfigurationItemWorkflows")
            .tag(RUN_TAG)
            .summary("The workflows of one CI: running and recent instances, and the workflows you may start")
            .description(
                "Running instances first, then the 20 that ended last, each with the transitions the caller may \
                 run. `controlledFields` lists the CI's fields an active workflow drives: they change only \
                 through the workflow (409 WORKFLOW_CONTROLLED_FIELD on a direct write). 404 for a CI of a type the \
                 caller may not view.",
            )
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(runtime::of_ci(&api.pool, &api.ctx, id).await?))
            }),
    ]
}
