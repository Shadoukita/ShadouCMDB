//! Request and response bodies of the workflow runtime API (instances on CIs).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use utoipa::openapi::schema::{ObjectBuilder, Schema, Type};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::schemas::{WorkflowState, WorkflowStateCategory, WorkflowTransition};
use crate::api::route::Check;
use crate::api::schemas::{self, Sort, key_schema, ts};
use crate::http::error::{FieldError, FieldLocation};
use crate::modules::classes::AttributeDataType;
use crate::paged;

/// Longest comment or reason a step takes (the events table's limit).
pub const MAX_COMMENT: usize = 4000;

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
pub enum WorkflowInstanceStatus {
    /// Running: transitions can move it on
    Active,
    /// It reached a terminal state
    Completed,
    /// Cancelled by a user, or because its CI was deleted
    Cancelled,
}

/// A state as an instance is in it, or a transition leads to it
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowStateRef {
    pub key: String,
    pub name: String,
    #[schema(inline)]
    pub category: WorkflowStateCategory,
    pub terminal: bool,
}

/// One run of a workflow on a CI
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowInstance {
    pub id: Uuid,
    pub definition_id: Uuid,
    pub definition_key: String,
    pub definition_name: String,
    /// The version of the workflow the instance is pinned to
    pub version_no: i32,
    pub ci_id: Uuid,
    pub ci_ident: String,
    pub ci_label: String,
    /// The CI's type
    pub class_key: String,
    #[schema(inline)]
    pub status: WorkflowInstanceStatus,
    pub state: WorkflowStateRef,
    #[serde(serialize_with = "ts::serialize")]
    pub started_at: DateTime<Utc>,
    pub started_by_name: String,
    #[serde(serialize_with = "ts::serialize")]
    pub last_transition_at: DateTime<Utc>,
    /// When it completed or was cancelled; null while active
    #[schema(required = true)]
    #[serde(serialize_with = "schemas::ts_opt::serialize")]
    pub ended_at: Option<DateTime<Utc>>,
    /// Send it back as `expectedVersion`; a stale one fails with 409 VERSION_CONFLICT
    pub version: i32,
    /// The CI's own `version` after the step (its fields and state field may have changed)
    pub ci_version: i32,
}

/// A field a transition shows
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowTransitionFieldView {
    pub key: String,
    pub label: String,
    #[schema(inline)]
    pub data_type: AttributeDataType,
    /// It must have a value (the CI's, or one sent with the transition)
    pub required: bool,
    /// The CI's value, in the form of the item endpoints; null when it has none
    #[schema(value_type = Option<Object>, required = true)]
    pub current_value: Option<Value>,
}

/// Why a transition cannot run as the CI stands
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowBlockedReason {
    /// `fields.<key>` of the field the condition reads
    pub field: String,
    /// `condition`
    pub code: String,
    pub message: String,
}

/// A transition out of the instance's state that the caller may run
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowAvailableTransition {
    pub key: String,
    pub name: String,
    pub to_state: WorkflowStateRef,
    pub requires_comment: bool,
    pub fields: Vec<WorkflowTransitionFieldView>,
    /// The conditions that fail on the CI's current values: empty when it can run (given its required fields and
    /// comment). Values sent with the transition count too, so a condition on one of its fields can still be met.
    pub blocked_by: Vec<WorkflowBlockedReason>,
}

/// The graph of the version an instance is pinned to (no grants)
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowPinnedGraph {
    pub initial_state: String,
    pub states: Vec<WorkflowState>,
    pub transitions: Vec<WorkflowTransition>,
}

/// An instance with what the caller can do with it
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowInstanceView {
    pub instance: WorkflowInstance,
    /// The transitions out of the current state the caller is granted and may run (the edit right on the CI's type
    /// included); empty when the instance has ended. Transitions the caller is not granted are left out.
    pub available_transitions: Vec<WorkflowAvailableTransition>,
    /// Whether the caller may cancel it (`workflows.manage`, or the `_cancel` grant with the edit right)
    pub can_cancel: bool,
}

/// An instance with its pinned graph and what the caller can do with it
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowInstanceDetail {
    pub instance: WorkflowInstance,
    pub graph: WorkflowPinnedGraph,
    pub available_transitions: Vec<WorkflowAvailableTransition>,
    pub can_cancel: bool,
}

/// A workflow that can be started on a CI
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowStartable {
    pub definition_id: Uuid,
    pub definition_key: String,
    pub definition_name: String,
    pub version_no: i32,
}

/// The workflows of one CI
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CiWorkflows {
    /// Running instances first, then the 20 that ended last
    pub data: Vec<WorkflowInstanceView>,
    /// Active workflows of the CI's type that are not running on it and that the caller may start (the edit right
    /// on the type); empty for a deleted CI
    pub startable: Vec<WorkflowStartable>,
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowInstanceStart {
    /// The workflow, by id; or give `definitionKey`
    #[serde(default)]
    pub definition_id: Option<Uuid>,
    /// The workflow, by key; or give `definitionId`
    #[schema(schema_with = key_schema)]
    #[serde(default)]
    pub definition_key: Option<String>,
    pub ci_id: Uuid,
    #[schema(schema_with = comment_schema)]
    #[serde(default)]
    pub comment: Option<String>,
}

impl Check for WorkflowInstanceStart {
    fn check(&self) -> Vec<FieldError> {
        if self.definition_id.is_some() == self.definition_key.is_some() {
            return vec![FieldError {
                location: FieldLocation::Body,
                field: "definitionId".into(),
                message: "Give the workflow by definitionId or by definitionKey (one of them)".into(),
                code: "one_of".into(),
            }];
        }
        Vec::new()
    }
}

fn fields_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .description(Some(
            "Values of the transition's fields by field key, in the form of PATCH /configuration-items/{id} \
             (null clears one). Only the fields the transition lists.",
        ))
        .into()
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowTransitionRun {
    #[schema(schema_with = key_schema)]
    pub transition_key: String,
    /// The instance's `version` you loaded: 409 VERSION_CONFLICT if it moved on in between
    #[schema(minimum = 1)]
    pub expected_version: i32,
    #[schema(schema_with = fields_schema)]
    #[serde(default)]
    pub fields: Map<String, Value>,
    #[schema(schema_with = comment_schema)]
    #[serde(default)]
    pub comment: Option<String>,
}

impl Check for WorkflowTransitionRun {
    fn check(&self) -> Vec<FieldError> {
        if self.fields.len() > 50 {
            return vec![FieldError {
                location: FieldLocation::Body,
                field: "fields".into(),
                message: "At most 50 fields".into(),
                code: "too_big".into(),
            }];
        }
        Vec::new()
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowInstanceCancel {
    #[schema(minimum = 1)]
    pub expected_version: i32,
    #[schema(schema_with = reason_schema)]
    pub reason: String,
}

impl Check for WorkflowInstanceCancel {}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowInstanceForce {
    #[schema(minimum = 1)]
    pub expected_version: i32,
    /// A state of the version the instance is pinned to
    #[schema(schema_with = key_schema)]
    pub state_key: String,
    #[schema(schema_with = reason_schema)]
    pub reason: String,
}

impl Check for WorkflowInstanceForce {}

// ---------------------------------------------------------------------------
// Lists
// ---------------------------------------------------------------------------

fn instance_sort() -> Schema {
    schemas::sort_schema(&["lastTransitionAt", "startedAt"], "-lastTransitionAt")
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct WorkflowInstanceList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    pub limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    pub offset: i64,
    /// Only instances of this workflow
    #[param(schema_with = key_schema)]
    pub definition_key: Option<String>,
    /// Only instances in a state of this key (in whichever version)
    #[param(schema_with = key_schema)]
    pub state_key: Option<String>,
    #[param(inline)]
    pub status: Option<WorkflowInstanceStatus>,
    /// Only instances on CIs of this type (by key; the types below it are not included)
    #[param(schema_with = key_schema)]
    pub class_key: Option<String>,
    /// Only instances on this CI
    pub ci_id: Option<Uuid>,
    #[param(required = false, schema_with = instance_sort)]
    pub sort: Sort,
}
paged!(WorkflowInstanceList);

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct WorkflowInstanceSummaryQuery {
    /// Only this workflow
    #[param(schema_with = key_schema)]
    pub definition_key: Option<String>,
}

/// Running instances of one workflow in one state
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowStateCount {
    pub definition_id: Uuid,
    pub definition_key: String,
    pub state_key: String,
    /// The state's name in the newest version that has it
    pub state_name: String,
    #[schema(inline)]
    pub category: WorkflowStateCategory,
    pub count: i64,
}

/// Running instances per workflow and state, on the CIs the caller may view
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowInstanceSummary {
    pub data: Vec<WorkflowStateCount>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct WorkflowEventList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    pub limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    pub offset: i64,
}
paged!(WorkflowEventList);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum WorkflowEventKind {
    Start,
    Transition,
    Cancel,
    Migrate,
    Force,
}

/// One step in an instance's history, oldest first
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowEvent {
    pub id: i64,
    #[schema(inline)]
    pub kind: WorkflowEventKind,
    /// The transition run (`transition` events only)
    #[schema(required = true)]
    pub transition_key: Option<String>,
    #[schema(required = true)]
    pub from_state_key: Option<String>,
    pub to_state_key: String,
    /// The version before a migration (`migrate` events only)
    #[schema(required = true)]
    pub from_version_no: Option<i32>,
    pub to_version_no: i32,
    #[serde(serialize_with = "ts::serialize")]
    pub occurred_at: DateTime<Utc>,
    /// user, api_client, import or system (a CI deletion cancelling its instances)
    pub actor_type: String,
    #[schema(required = true)]
    pub actor_name: Option<String>,
    /// The comment of a step, or the reason of a cancel or forced state
    #[schema(required = true)]
    pub comment: Option<String>,
    /// `{fieldKey: {old, new}}`: the CI fields the step wrote (transition fields and the state field)
    #[schema(value_type = Option<Object>, required = true)]
    pub field_changes: Option<sqlx::types::Json<Value>>,
    /// Joins to the audit log's `requestId`
    #[schema(required = true)]
    pub request_id: Option<String>,
}
