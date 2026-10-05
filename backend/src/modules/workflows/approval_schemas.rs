//! Request and response bodies of the run-time approvals API (approvals
//! design SHAA-1869 §2.2, §10.2): approval requests on gated transitions and
//! the decisions on them.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use utoipa::openapi::schema::{ObjectBuilder, Schema, Type};
use uuid::Uuid;

use super::runtime_schemas::{MAX_COMMENT, WorkflowInstance};
use super::schemas::{WorkflowApproverRole, WorkflowApproverSource};
use crate::api::route::Check;
use crate::api::schemas::{self, key_schema, ts};
use crate::http::error::{FieldError, FieldLocation};

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
    /// Distinct active users who could decide it when it was last resolved (the requester never counts); null
    /// before the step is active
    #[schema(required = true)]
    pub eligible_count: Option<i32>,
    /// Active, and fewer users could decide it than approvals are still needed
    pub understaffed: bool,
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
}

/// Whether the caller may decide the active step now, and why not
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApprovalEligibility {
    pub can_decide: bool,
    /// Why not: the `details[0].code` a decision would be refused with (`not_pending`, `not_eligible`,
    /// `requester`, `token_creator`, `earlier_step`, `actor_of:<key>`, `session_required`,
    /// `token_not_self_minted`); null when `canDecide`
    #[schema(required = true)]
    pub reason: Option<String>,
    #[schema(required = true)]
    pub message: Option<String>,
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
