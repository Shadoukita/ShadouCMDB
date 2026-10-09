//! Request and response bodies of the run-time approvals API (approvals
//! design SHAA-1869 §2.2, §10.2): approval requests on gated transitions and
//! the decisions on them.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::openapi::schema::{ObjectBuilder, Schema, Type};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::runtime_schemas::{MAX_COMMENT, WorkflowInstance};
use super::schemas::{
    WorkflowApprovalDroppedSource, WorkflowApproverRole, WorkflowApproverSource, WorkflowFieldChange,
};
use crate::api::route::Check;
use crate::api::schemas::{self, QueryBool, Sort, key_schema, ts};
use crate::http::error::{FieldError, FieldLocation};
use crate::paged;

fn comment_schema() -> Schema {
    schemas::multiline_text_schema(MAX_COMMENT)
}

fn reason_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .min_length(Some(1))
        .max_length(Some(MAX_COMMENT))
        .pattern(Some(r"\S"))
        .description(Some("Why; recorded in the instance's history and the audit log"))
        .into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum WorkflowApprovalStatus {
    /// Waiting for decisions; the instance runs no other transition
    Pending,
    /// The last step reached its quorum and the transition was applied
    Approved,
    /// An approver rejected it; the instance stayed where it was
    Rejected,
    /// The requester withdrew it
    Withdrawn,
    /// Closed by a manager, or by a cancel, forced state, migration or CI deletion of the instance
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum WorkflowApprovalCloseReason {
    Approved,
    Rejected,
    Overdue,
    /// Withdrawn by the requester, or cancelled by a manager (status tells which)
    Withdrawn,
    InstanceCancelled,
    InstanceForced,
    InstanceMigrated,
    CiDeleted,
}

impl WorkflowApprovalCloseReason {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkflowApprovalCloseReason::Approved => "approved",
            WorkflowApprovalCloseReason::Rejected => "rejected",
            WorkflowApprovalCloseReason::Overdue => "overdue",
            WorkflowApprovalCloseReason::Withdrawn => "withdrawn",
            WorkflowApprovalCloseReason::InstanceCancelled => "instance_cancelled",
            WorkflowApprovalCloseReason::InstanceForced => "instance_forced",
            WorkflowApprovalCloseReason::InstanceMigrated => "instance_migrated",
            WorkflowApprovalCloseReason::CiDeleted => "ci_deleted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum WorkflowApprovalStepStatus {
    /// An earlier step is not approved yet
    Waiting,
    /// Open for decisions
    Active,
    Approved,
    Rejected,
    /// The request closed before the step was decided
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum WorkflowApprovalVerdict {
    Approve,
    Reject,
}

impl WorkflowApprovalVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkflowApprovalVerdict::Approve => "approve",
            WorkflowApprovalVerdict::Reject => "reject",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum WorkflowApprovalCredential {
    /// A signed-in session
    Session,
    /// An API token its owner minted for themselves, on a step that allows tokens
    Token,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum WorkflowApprovalPrincipalKind {
    User,
    Profile,
    Group,
}

/// The pending approval request of an instance: it stays in its state, runs no other transition, and moves along
/// the requested transition once the last step is approved
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowPendingApproval {
    pub request_id: Uuid,
    /// 1 for the instance's first request, 2 for the next one (after a rejection or withdrawal), …
    pub request_no: i32,
    pub transition_key: String,
    /// The state the instance moves to once approved
    pub to_state: String,
    /// The active step (1-based)
    pub step_no: i16,
    pub step_key: String,
    pub step_count: i16,
    /// Approvals the active step has
    pub approvals: i64,
    /// Approvals the active step needs
    pub required: i16,
    #[schema(required = true)]
    #[serde(serialize_with = "schemas::ts_opt::serialize")]
    pub due_at: Option<DateTime<Utc>>,
    pub overdue: bool,
    /// The request's `version`: send it as `expectedVersion` with a decision, withdrawal or cancellation
    pub version: i32,
}

/// One step of a transition's approval policy, as a transition offers it
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApprovalStepSummary {
    pub key: String,
    pub name: String,
    pub required_approvals: i16,
}

/// Who made the request, as they stand now (advisory: it does not block a decision)
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApprovalRequester {
    /// Null once the account was deleted
    #[schema(required = true)]
    pub id: Option<Uuid>,
    pub name: String,
    /// The account exists and is enabled
    pub active: bool,
    /// Active, and still holds the edit right on the CI's type and a grant of the transition: false means the change
    /// was staged by someone who could no longer make it
    pub still_authorized: bool,
}

/// One approve or reject
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApprovalDecision {
    pub id: i64,
    #[serde(skip)]
    pub step_no: i16,
    #[schema(inline)]
    pub decision: WorkflowApprovalVerdict,
    pub actor_name: String,
    #[schema(inline)]
    pub credential: WorkflowApprovalCredential,
    /// The principal a delegate decided for
    #[schema(required = true)]
    pub on_behalf_of_name: Option<String>,
    #[schema(required = true)]
    pub comment: Option<String>,
    #[serde(serialize_with = "ts::serialize")]
    pub decided_at: DateTime<Utc>,
}

/// One step of a request
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApprovalRequestStep {
    pub step_no: i16,
    pub key: String,
    pub name: String,
    pub required_approvals: i16,
    #[schema(inline)]
    pub status: WorkflowApprovalStepStatus,
    #[schema(required = true)]
    #[serde(serialize_with = "schemas::ts_opt::serialize")]
    pub activated_at: Option<DateTime<Utc>>,
    #[schema(required = true)]
    #[serde(serialize_with = "schemas::ts_opt::serialize")]
    pub due_at: Option<DateTime<Utc>>,
    pub overdue: bool,
    #[schema(required = true)]
    #[serde(serialize_with = "schemas::ts_opt::serialize")]
    pub completed_at: Option<DateTime<Utc>>,
    pub approvals: i64,
    /// Distinct active users who could decide it when it was last resolved (the requester never counts). Every
    /// step is resolved when the request is made; null only for a waiting step of a request made before that
    #[schema(required = true)]
    pub eligible_count: Option<i32>,
    /// Active, and fewer users could decide it than approvals are still needed
    pub understaffed: bool,
    /// Approver sources, or parts of one, not used at the last resolution, with why: a CI field naming the
    /// approvers that the requester set (GH#664), a business service owner the requester made an owner, or the
    /// owners of a service the requester added the CI to (GH#708)
    pub dropped_sources: Vec<WorkflowApprovalDroppedSource>,
    pub decisions: Vec<WorkflowApprovalDecision>,
}

/// A principal who may decide the active step
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApprovalPrincipal {
    #[schema(inline)]
    pub role: WorkflowApproverRole,
    #[schema(inline)]
    pub kind: WorkflowApprovalPrincipalKind,
    pub id: Uuid,
    #[schema(inline)]
    pub source: WorkflowApproverSource,
    /// The assignment that made it eligible, e.g. "group CAB"
    pub label: String,
    /// For a CI field source: the audited change that set the field when the step was resolved; null otherwise or
    /// when the field has no audit history
    #[schema(required = true)]
    pub field_last_changed: Option<WorkflowFieldChange>,
}

/// Whether the caller may decide the active step now, and why not
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApprovalEligibility {
    pub can_decide: bool,
    /// May decide in person (a decision without `onBehalfOf` is cast in person when this is true)
    pub in_person: bool,
    /// The principals the caller may decide for through a live delegation, by name
    pub on_behalf_of: Vec<WorkflowApprovalOnBehalfOf>,
    /// Why not: the `details[0].code` a decision would be refused with (`not_pending`, `not_eligible`,
    /// `requester`, `token_creator`, `on_behalf_of_requester`, `earlier_step`, `actor_of:<key>`,
    /// `session_required`, `token_not_self_minted`); null when `canDecide`
    #[schema(required = true)]
    pub reason: Option<String>,
    #[schema(required = true)]
    pub message: Option<String>,
}

/// A principal the caller may decide for, and the delegation that allows it
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApprovalOnBehalfOf {
    pub user_id: Uuid,
    pub name: String,
    pub delegation_id: Uuid,
}

/// An approval request with its steps and decisions
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApprovalRequest {
    pub id: Uuid,
    pub instance_id: Uuid,
    pub ci_id: Uuid,
    pub ci_ident: String,
    pub ci_label: String,
    pub class_key: String,
    pub definition_key: String,
    pub definition_name: String,
    /// The version of the workflow the instance is pinned to
    pub version_no: i32,
    pub transition_key: String,
    pub transition_name: String,
    pub from_state: String,
    pub to_state: String,
    pub request_no: i32,
    #[schema(inline)]
    pub status: WorkflowApprovalStatus,
    #[schema(inline, required = true)]
    pub close_reason: Option<WorkflowApprovalCloseReason>,
    pub current_step_no: i16,
    #[serde(serialize_with = "ts::serialize")]
    pub requested_at: DateTime<Utc>,
    pub requester: WorkflowApprovalRequester,
    #[schema(required = true)]
    pub comment: Option<String>,
    /// The transition's field values the request applies on final approval, by field key, in the form of the item
    /// endpoints
    #[schema(value_type = Object)]
    pub staged_fields: Value,
    pub steps: Vec<WorkflowApprovalRequestStep>,
    /// Who may decide the active step: shown to `workflows.manage` holders and to those who may decide it, null to
    /// everyone else (it would reveal profile and group membership)
    #[schema(required = true)]
    pub approvers: Option<Vec<WorkflowApprovalPrincipal>>,
    pub my_eligibility: WorkflowApprovalEligibility,
    #[schema(required = true)]
    #[serde(serialize_with = "schemas::ts_opt::serialize")]
    pub closed_at: Option<DateTime<Utc>>,
    #[schema(required = true)]
    pub closed_by_name: Option<String>,
    /// Send it as `expectedVersion`: 409 VERSION_CONFLICT if the request changed in between
    pub version: i32,
}

/// A request and its instance after a decision, withdrawal or cancellation
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApprovalOutcome {
    pub request: WorkflowApprovalRequest,
    /// Moved along the transition when the decision was the final approval
    pub instance: WorkflowInstance,
}

// ---------------------------------------------------------------------------
// Bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApprovalDecide {
    /// The active step: 409 CONFLICT `step_not_active` otherwise
    #[schema(schema_with = key_schema)]
    pub step_key: String,
    #[schema(inline)]
    pub decision: WorkflowApprovalVerdict,
    /// The request's `version` you loaded
    #[schema(minimum = 1)]
    pub expected_version: i32,
    /// Required to reject, optional to approve
    #[schema(schema_with = comment_schema)]
    #[serde(default)]
    pub comment: Option<String>,
    /// Decide for this principal through a live delegation to you (`myEligibility.onBehalfOf`). Left out: in
    /// person when you qualify in person, otherwise for your only principal (400 when there are several)
    #[serde(default)]
    pub on_behalf_of: Option<Uuid>,
}

impl Check for WorkflowApprovalDecide {
    fn check(&self) -> Vec<FieldError> {
        let blank = self.comment.as_deref().is_none_or(|c| c.trim().is_empty());
        if self.decision == WorkflowApprovalVerdict::Reject && blank {
            return vec![FieldError {
                location: FieldLocation::Body,
                field: "comment".into(),
                message: "Say why you reject the request".into(),
                code: "required".into(),
            }];
        }
        Vec::new()
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApprovalWithdraw {
    #[schema(minimum = 1)]
    pub expected_version: i32,
    #[schema(schema_with = comment_schema)]
    #[serde(default)]
    pub comment: Option<String>,
}

impl Check for WorkflowApprovalWithdraw {}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApprovalCancel {
    #[schema(minimum = 1)]
    pub expected_version: i32,
    #[schema(schema_with = reason_schema)]
    pub comment: String,
}

impl Check for WorkflowApprovalCancel {}

// ---------------------------------------------------------------------------
// Lists (slice A3b)
// ---------------------------------------------------------------------------

/// Which requests a list shows (default `actionable`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowApprovalView {
    /// Pending requests whose active step the caller may decide now (the inbox)
    Actionable,
    /// Requests the caller made
    Requested,
    /// Requests the caller approved or rejected a step of
    Decided,
    /// Every request on the CIs the caller may view
    All,
}

fn view_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(["actionable", "requested", "decided", "all"]))
        .default(Some("actionable".into()))
        .description(Some(
            "actionable (default): pending requests whose active step you may decide now, in person; requested: \
             the ones you made; decided: the ones you approved or rejected a step of; all: every request on the \
             CIs you may view",
        ))
        .into()
}

fn overdue_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(["true", "false"]))
        .description(Some("true: only requests whose active step is overdue; false: only the others"))
        .into()
}

fn request_sort() -> Schema {
    schemas::sort_schema(&["dueAt", "requestedAt", "closedAt"], "dueAt")
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct WorkflowApprovalRequestList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    pub limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    pub offset: i64,
    #[param(required = false, schema_with = view_schema)]
    pub view: WorkflowApprovalView,
    #[param(inline)]
    pub status: Option<WorkflowApprovalStatus>,
    /// Only requests on instances of this workflow
    #[param(schema_with = key_schema)]
    pub definition_key: Option<String>,
    /// Only requests on this CI
    pub ci_id: Option<Uuid>,
    /// Only requests this user made (the incident runbook: the pending requests of a disabled account)
    pub requested_by: Option<Uuid>,
    #[param(schema_with = overdue_schema)]
    pub overdue: Option<QueryBool>,
    /// `dueAt` is the active step's due date (the last step reached, for a closed request); requests without one
    /// come last either way
    #[param(required = false, schema_with = request_sort)]
    pub sort: Sort,
}
paged!(WorkflowApprovalRequestList);

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct WorkflowApprovalHistoryList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    pub limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    pub offset: i64,
}
paged!(WorkflowApprovalHistoryList);

/// Who made a request, as recorded then
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApprovalRequestedBy {
    /// Null once the account was deleted
    #[schema(required = true)]
    pub id: Option<Uuid>,
    pub name: String,
}

/// The step a request is at: the active one while pending, the last one reached once closed
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApprovalCurrentStep {
    pub step_no: i16,
    pub key: String,
    pub name: String,
    #[schema(inline)]
    pub status: WorkflowApprovalStepStatus,
    pub approvals: i64,
    pub required_approvals: i16,
    #[schema(required = true)]
    #[serde(serialize_with = "schemas::ts_opt::serialize")]
    pub due_at: Option<DateTime<Utc>>,
    pub overdue: bool,
}

/// An approval request in a list: no staged values, approvers or decisions (`GET /workflow-approval-requests/{id}`
/// has them)
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApprovalRequestItem {
    pub id: Uuid,
    pub instance_id: Uuid,
    pub ci_id: Uuid,
    pub ci_ident: String,
    pub ci_label: String,
    pub class_key: String,
    pub definition_key: String,
    pub definition_name: String,
    pub version_no: i32,
    pub transition_key: String,
    pub transition_name: String,
    pub from_state: String,
    pub to_state: String,
    pub request_no: i32,
    #[schema(inline)]
    pub status: WorkflowApprovalStatus,
    #[schema(inline, required = true)]
    pub close_reason: Option<WorkflowApprovalCloseReason>,
    #[serde(serialize_with = "ts::serialize")]
    pub requested_at: DateTime<Utc>,
    pub requested_by: WorkflowApprovalRequestedBy,
    pub current_step: WorkflowApprovalCurrentStep,
    pub step_count: i16,
    #[schema(required = true)]
    #[serde(serialize_with = "schemas::ts_opt::serialize")]
    pub closed_at: Option<DateTime<Utc>>,
    #[schema(required = true)]
    pub closed_by_name: Option<String>,
    /// Send it as `expectedVersion` with a decision, withdrawal or cancellation
    pub version: i32,
}

// ---------------------------------------------------------------------------
// Delegations (slice A4, §6.1)
// ---------------------------------------------------------------------------

/// Where a delegation stands now (judged at the database's clock)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowApprovalDelegationStatus {
    /// Starts later
    Scheduled,
    /// In its window and not revoked: the delegate may decide for the principal while both accounts are active
    Active,
    /// Its window is over
    Ended,
    /// Revoked before it ended
    Revoked,
}

/// A user on a delegation, as recorded (the id is null once the account was deleted)
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApprovalDelegationUser {
    #[schema(required = true)]
    pub id: Option<Uuid>,
    pub name: String,
}

/// A time-boxed delegation of one user's approvals to another (never deleted: revoked, so the history stays)
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApprovalDelegation {
    pub id: Uuid,
    /// Whose approvals are delegated
    pub principal: WorkflowApprovalDelegationUser,
    /// Who may decide for the principal
    pub delegate: WorkflowApprovalDelegationUser,
    /// Limited to one workflow; false: every workflow
    pub scoped: bool,
    /// The workflow it is limited to; null when it is not limited, or when the workflow's type is one you may not
    /// view
    #[schema(required = true)]
    pub definition_key: Option<String>,
    #[schema(required = true)]
    pub definition_name: Option<String>,
    #[serde(serialize_with = "ts::serialize")]
    pub starts_at: DateTime<Utc>,
    #[serde(serialize_with = "ts::serialize")]
    pub ends_at: DateTime<Utc>,
    #[schema(required = true)]
    pub reason: Option<String>,
    #[schema(inline)]
    pub status: WorkflowApprovalDelegationStatus,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    /// The principal, or the administrator who made it for them
    pub created_by: WorkflowApprovalDelegationUser,
    #[schema(required = true)]
    #[serde(serialize_with = "schemas::ts_opt::serialize")]
    pub revoked_at: Option<DateTime<Utc>>,
    #[schema(required = true)]
    pub revoked_by_name: Option<String>,
}

/// Longest delegation window (the database checks it too).
pub const MAX_DELEGATION_DAYS: i64 = 90;
/// Active (not revoked, not ended) delegations a principal may have at once.
pub const MAX_ACTIVE_DELEGATIONS: i64 = 5;
const MAX_DELEGATION_REASON: usize = 500;

fn delegation_reason_schema() -> Schema {
    schemas::multiline_text_schema(MAX_DELEGATION_REASON)
}

/// Delegate your own approvals
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApprovalDelegationCreate {
    /// Who may decide for you: an active user other than you
    pub delegate_user_id: Uuid,
    /// May be in the future (planned leave)
    pub starts_at: DateTime<Utc>,
    /// Required: at most 90 days after `startsAt`, and in the future
    pub ends_at: DateTime<Utc>,
    /// Limit it to one workflow; left out: every workflow
    #[schema(schema_with = key_schema)]
    #[serde(default)]
    pub definition_key: Option<String>,
    #[schema(schema_with = delegation_reason_schema)]
    #[serde(default)]
    pub reason: Option<String>,
}

fn check_delegation(starts: DateTime<Utc>, ends: DateTime<Utc>, reason: Option<&str>) -> Vec<FieldError> {
    let error = |field: &str, code: &str, message: &str| FieldError {
        location: FieldLocation::Body,
        field: field.into(),
        message: message.into(),
        code: code.into(),
    };
    let mut out = Vec::new();
    if ends <= starts {
        out.push(error("endsAt", "out_of_range", "The delegation must end after it starts"));
    } else if ends - starts > chrono::Duration::days(MAX_DELEGATION_DAYS) {
        out.push(error("endsAt", "out_of_range", "A delegation lasts at most 90 days"));
    }
    if reason.is_some_and(|r| r.chars().count() > MAX_DELEGATION_REASON) {
        out.push(error("reason", "too_long", "At most 500 characters"));
    }
    out
}

impl Check for WorkflowApprovalDelegationCreate {
    fn check(&self) -> Vec<FieldError> {
        check_delegation(self.starts_at, self.ends_at, self.reason.as_deref())
    }
}

/// Delegate someone else's approvals while they are absent (`users.manage`)
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApprovalDelegationAdminCreate {
    /// Whose approvals are delegated
    pub principal_user_id: Uuid,
    /// Who may decide for them: an active user other than the principal, and not you (SHAA-1872 C2)
    pub delegate_user_id: Uuid,
    pub starts_at: DateTime<Utc>,
    /// Required: at most 90 days after `startsAt`, and in the future
    pub ends_at: DateTime<Utc>,
    #[schema(schema_with = key_schema)]
    #[serde(default)]
    pub definition_key: Option<String>,
    #[schema(schema_with = delegation_reason_schema)]
    #[serde(default)]
    pub reason: Option<String>,
}

impl Check for WorkflowApprovalDelegationAdminCreate {
    fn check(&self) -> Vec<FieldError> {
        check_delegation(self.starts_at, self.ends_at, self.reason.as_deref())
    }
}

/// Your side of a delegation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowApprovalDelegationRole {
    /// Delegations of your approvals
    Principal,
    /// Delegations to you
    Delegate,
}

fn active_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(["true", "false"]))
        .description(Some(
            "true: only delegations that are not revoked and have not ended (scheduled or active); false: only the \
             others",
        ))
        .into()
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct WorkflowApprovalMyDelegationList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    pub limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    pub offset: i64,
    /// Left out: both
    #[param(inline)]
    pub role: Option<WorkflowApprovalDelegationRole>,
    #[param(schema_with = active_schema)]
    pub active: Option<QueryBool>,
}
paged!(WorkflowApprovalMyDelegationList);

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct WorkflowApprovalDelegationList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    pub limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    pub offset: i64,
    /// Only delegations of this user's approvals
    pub principal: Option<Uuid>,
    /// Only delegations to this user
    pub delegate: Option<Uuid>,
    #[param(schema_with = active_schema)]
    pub active: Option<QueryBool>,
}
paged!(WorkflowApprovalDelegationList);
