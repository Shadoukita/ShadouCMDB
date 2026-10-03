//! Request and response bodies of the workflow definitions API.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::openapi::schema::{ArrayBuilder, ObjectBuilder, Schema, Type};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::api::context::Count;
use crate::api::route::Check;
use crate::api::schemas::{self, QueryBool, Sort, description_schema, key_schema, trimmed, ts};
use crate::data::crud::ColumnSet;
use crate::http::error::{FieldError, FieldLocation};
use crate::modules::simple_resource::non_empty;
use crate::paged;

/// Profiles granted one transition.
pub const MAX_GRANT_PROFILES: usize = 100;

fn workflow_name_schema() -> Schema {
    ObjectBuilder::new().schema_type(Type::String).min_length(Some(1)).max_length(Some(100)).pattern(Some(r"\S")).into()
}

fn change_note_schema() -> Schema {
    schemas::multiline_text_schema(2000)
}

fn checksum_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .pattern(Some("^[0-9a-f]{64}$"))
        .description(Some(
            "sha256 of the canonical graph (hex): states, transitions and the initial state, not the layout",
        ))
        .into()
}

fn conditions_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .description(Some(
            "Must hold for the transition to run. A group `{\"all\": [...]}` or `{\"any\": [...]}` of conditions, or a \
             leaf `{\"field\": <field key>, \"op\": <op>, \"value\": <value>}`. Ops: eq, ne, in, notIn (value: array of \
             1-100), isSet, isNotSet (no value), gt, gte, lt, lte (number, integer, date and datetime fields), contains \
             (text fields). The value has the field's type; enum and lookup values are given by key. At most 4 levels \
             deep, 32 leaves and 16 KiB. The field is one of the workflow type's own or inherited fields.",
        ))
        .into()
}

fn layout_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .description(Some("Designer node positions, free-form (at most 64 KiB). Not part of the checksum."))
        .into()
}

// ---------------------------------------------------------------------------
// Definitions
// ---------------------------------------------------------------------------

/// A workflow definition: the identity of a workflow and its mutable settings
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowDefinition {
    pub id: Uuid,
    /// Export identity; never changes
    pub key: String,
    pub name: String,
    #[schema(required = true)]
    pub description: Option<String>,
    /// The type the workflow runs on; never changes
    pub class_id: Uuid,
    pub class_key: String,
    /// Whether CIs of the types below `classId` run it too
    pub include_subclasses: bool,
    /// The lookup field of the type that the workflow keeps in step with its state
    #[schema(required = true)]
    pub state_attribute_id: Option<Uuid>,
    #[schema(required = true)]
    pub state_attribute_key: Option<String>,
    /// Start an instance when a CI of the type is created
    pub auto_start: bool,
    /// Inactive: no new instances; running ones continue
    pub is_active: bool,
    /// The newest published version that is not retired; null before the first publish
    #[schema(required = true)]
    pub current_version_no: Option<i32>,
    /// The version number of the draft; null when there is none
    #[schema(required = true)]
    pub draft_version_no: Option<i32>,
    /// Send it back with a change; a stale one fails with 409 VERSION_CONFLICT
    pub version: i32,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    pub created_by_name: String,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
    pub updated_by_name: String,
}

/// Something the caller should know about a change that was made anyway
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowWarning {
    /// `UNINSTANCED_CIS`: the workflow drives a state field and is active, and this many live CIs it covers have
    /// no running instance of it. Their state field cannot be edited (it is driven by the workflow) until an
    /// instance is started on them.
    #[schema(inline)]
    pub code: WorkflowWarningCode,
    /// Null when withheld: the count spans CIs of types the caller may not view
    #[schema(value_type = Option<i64>, required = true)]
    pub count: Count,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WorkflowWarningCode {
    UninstancedCis,
}

/// A workflow definition with its draft's checksum and the warnings of the change that returned it
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowDefinitionDetail {
    pub id: Uuid,
    pub key: String,
    pub name: String,
    #[schema(required = true)]
    pub description: Option<String>,
    pub class_id: Uuid,
    pub class_key: String,
    pub include_subclasses: bool,
    #[schema(required = true)]
    pub state_attribute_id: Option<Uuid>,
    #[schema(required = true)]
    pub state_attribute_key: Option<String>,
    pub auto_start: bool,
    pub is_active: bool,
    #[schema(required = true)]
    pub current_version_no: Option<i32>,
    #[schema(required = true)]
    pub draft_version_no: Option<i32>,
    /// The draft's checksum: send it as `expectedDraftChecksum` to publish. Null when there is no draft.
    #[schema(required = true, pattern = "^[0-9a-f]{64}$")]
    pub draft_checksum: Option<String>,
    pub version: i32,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    pub created_by_name: String,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
    pub updated_by_name: String,
    /// Filled by create and update only (e.g. `UNINSTANCED_CIS` when the workflow is activated); empty otherwise
    pub warnings: Vec<WorkflowWarning>,
}

impl WorkflowDefinitionDetail {
    pub fn new(d: WorkflowDefinition, draft_checksum: Option<String>, warnings: Vec<WorkflowWarning>) -> Self {
        WorkflowDefinitionDetail {
            id: d.id,
            key: d.key,
            name: d.name,
            description: d.description,
            class_id: d.class_id,
            class_key: d.class_key,
            include_subclasses: d.include_subclasses,
            state_attribute_id: d.state_attribute_id,
            state_attribute_key: d.state_attribute_key,
            auto_start: d.auto_start,
            is_active: d.is_active,
            current_version_no: d.current_version_no,
            draft_version_no: d.draft_version_no,
            draft_checksum,
            version: d.version,
            created_at: d.created_at,
            created_by_name: d.created_by_name,
            updated_at: d.updated_at,
            updated_by_name: d.updated_by_name,
            warnings,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowDefinitionCreate {
    /// Export identity, lower_snake_case, unique regardless of case; never changes
    #[schema(schema_with = key_schema)]
    pub key: String,
    #[schema(schema_with = workflow_name_schema)]
    #[serde(deserialize_with = "trimmed")]
    pub name: String,
    #[schema(schema_with = description_schema)]
    #[serde(default)]
    pub description: Option<String>,
    /// The type the workflow runs on; never changes
    pub class_id: Uuid,
    /// Default true
    #[schema(nullable = false)]
    pub include_subclasses: Option<bool>,
    /// A lookup field of the type (its own or inherited) that the workflow keeps in step with its state
    #[serde(default)]
    pub state_attribute_id: Option<Uuid>,
    /// Default false
    #[schema(nullable = false)]
    pub auto_start: Option<bool>,
    /// Default false: a new workflow starts inactive
    #[schema(nullable = false)]
    pub is_active: Option<bool>,
}

impl Check for WorkflowDefinitionCreate {}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowDefinitionUpdate {
    /// The version you loaded; if someone saved in between, the request fails with 409 VERSION_CONFLICT
    #[schema(minimum = 1)]
    pub version: i32,
    #[schema(schema_with = workflow_name_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    pub name: Option<String>,
    #[schema(schema_with = description_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    pub description: Option<Option<String>>,
    #[schema(nullable = false)]
    pub include_subclasses: Option<bool>,
    /// Changeable only until the first version is published (409 CONFLICT afterwards)
    #[schema(value_type = Option<Uuid>)]
    #[serde(default, deserialize_with = "schemas::patch")]
    pub state_attribute_id: Option<Option<Uuid>>,
    #[schema(nullable = false)]
    pub auto_start: Option<bool>,
    #[schema(nullable = false)]
    pub is_active: Option<bool>,
}

impl WorkflowDefinitionUpdate {
    pub fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("name", self.name.clone())
            .opt("description", self.description.clone())
            .opt("include_subclasses", self.include_subclasses)
            .opt("state_attribute_id", self.state_attribute_id)
            .opt("auto_start", self.auto_start)
            .opt("is_active", self.is_active);
        c
    }
}

impl Check for WorkflowDefinitionUpdate {
    fn check(&self) -> Vec<FieldError> {
        non_empty(&self.columns())
    }
}

fn definition_sort() -> Schema {
    schemas::sort_schema(&["name", "key", "createdAt", "updatedAt"], "name")
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct WorkflowDefinitionList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    pub limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    pub offset: i64,
    /// Matches the key, name and description
    #[param(schema_with = schemas::search_schema)]
    pub q: Option<String>,
    /// Only workflows of this type (by key; the types below it are not included)
    #[param(schema_with = key_schema)]
    pub class_key: Option<String>,
    /// Only active (true) or inactive (false) workflows
    #[param(inline)]
    pub active: Option<QueryBool>,
    #[param(required = false, schema_with = definition_sort)]
    pub sort: Sort,
}
paged!(WorkflowDefinitionList);

// ---------------------------------------------------------------------------
// Graphs
// ---------------------------------------------------------------------------

/// What reaching a state means
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum WorkflowStateCategory {
    Open,
    Active,
    Done,
    Cancelled,
}

impl WorkflowStateCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkflowStateCategory::Open => "open",
            WorkflowStateCategory::Active => "active",
            WorkflowStateCategory::Done => "done",
            WorkflowStateCategory::Cancelled => "cancelled",
        }
    }
}

/// A state of a workflow version
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowState {
    /// Unique in the version
    #[schema(schema_with = key_schema)]
    pub key: String,
    #[schema(schema_with = workflow_name_schema)]
    #[serde(deserialize_with = "trimmed")]
    pub name: String,
    #[schema(inline)]
    pub category: WorkflowStateCategory,
    /// Reaching it completes the instance (default false)
    #[serde(default)]
    pub terminal: bool,
    /// The key of the value of the definition's state field (a lookup list value) set while an instance is in this
    /// state; null leaves the field as it is
    #[schema(schema_with = nullable_key_schema)]
    #[serde(default)]
    pub state_value: Option<String>,
}

fn nullable_key_schema() -> Schema {
    utoipa::openapi::schema::AnyOfBuilder::new()
        .item(key_schema())
        .item(ObjectBuilder::new().schema_type(Type::Null))
        .into()
}

/// A field a transition shows, and whether it must be filled in
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowTransitionField {
    /// The key of a field of the workflow's type (its own or inherited)
    #[schema(schema_with = key_schema)]
    pub attribute: String,
    /// Default true
    #[serde(default = "yes")]
    pub required: bool,
}

fn yes() -> bool {
    true
}

/// A transition of a workflow version: a directed edge between two states
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowTransition {
    /// Unique in the version; grants refer to it
    #[schema(schema_with = key_schema)]
    pub key: String,
    #[schema(schema_with = workflow_name_schema)]
    #[serde(deserialize_with = "trimmed")]
    pub name: String,
    /// Key of the state it leaves
    #[schema(schema_with = key_schema)]
    pub from: String,
    /// Key of the state it enters (not `from`)
    #[schema(schema_with = key_schema)]
    pub to: String,
    /// A comment must be given to run it (default false)
    #[serde(default)]
    pub requires_comment: bool,
    #[schema(max_items = 50)]
    #[serde(default)]
    pub fields: Vec<WorkflowTransitionField>,
    #[schema(schema_with = conditions_schema, required = false)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Value>,
}

/// The whole draft graph; it replaces the draft (or creates one)
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowDraftReplace {
    /// Key of the state an instance starts in
    #[schema(schema_with = nullable_key_schema)]
    #[serde(default)]
    pub initial_state: Option<String>,
    #[schema(max_items = 100)]
    pub states: Vec<WorkflowState>,
    #[schema(max_items = 300)]
    pub transitions: Vec<WorkflowTransition>,
    #[schema(schema_with = layout_schema)]
    #[serde(default)]
    pub layout: Option<Value>,
    /// The `checksum` of the draft you loaded: 409 VERSION_CONFLICT if the draft changed (or was published or
    /// deleted) in between. Leave it out to replace whatever is there.
    #[schema(schema_with = checksum_schema)]
    #[serde(default)]
    pub expected_checksum: Option<String>,
}

fn duplicates<'a>(prefix: &str, keys: impl Iterator<Item = &'a str>) -> Vec<FieldError> {
    let mut seen = HashSet::new();
    keys.enumerate()
        .filter(|(_, k)| !seen.insert(*k))
        .map(|(i, k)| FieldError {
            location: FieldLocation::Body,
            field: format!("{prefix}[{i}].key"),
            message: format!("\"{k}\" is used more than once"),
            code: "duplicate".into(),
        })
        .collect()
}

impl Check for WorkflowDraftReplace {
    fn check(&self) -> Vec<FieldError> {
        let mut errors = duplicates("states", self.states.iter().map(|s| s.key.as_str()));
        errors.extend(duplicates("transitions", self.transitions.iter().map(|t| t.key.as_str())));
        let states: HashSet<&str> = self.states.iter().map(|s| s.key.as_str()).collect();
        let unknown = |field: String, key: &str| FieldError {
            location: FieldLocation::Body,
            field,
            message: format!("No state \"{key}\" in this graph"),
            code: "unknown_state".into(),
        };
        if let Some(initial) = &self.initial_state
            && !states.contains(initial.as_str())
        {
            errors.push(unknown("initialState".into(), initial));
        }
        for (i, t) in self.transitions.iter().enumerate() {
            for (end, key) in [("from", &t.from), ("to", &t.to)] {
                if !states.contains(key.as_str()) {
                    errors.push(unknown(format!("transitions[{i}].{end}"), key));
                }
            }
            if t.from == t.to {
                errors.push(FieldError {
                    location: FieldLocation::Body,
                    field: format!("transitions[{i}].to"),
                    message: "A transition leads to another state".into(),
                    code: "loop".into(),
                });
            }
            let mut seen = HashSet::new();
            for (j, f) in t.fields.iter().enumerate() {
                if !seen.insert(f.attribute.as_str()) {
                    errors.push(FieldError {
                        location: FieldLocation::Body,
                        field: format!("transitions[{i}].fields[{j}].attribute"),
                        message: "Listed more than once".into(),
                        code: "duplicate".into(),
                    });
                }
            }
        }
        if self.layout.as_ref().is_some_and(|l| !l.is_object()) {
            errors.push(FieldError {
                location: FieldLocation::Body,
                field: "layout".into(),
                message: "Expected an object".into(),
                code: "type".into(),
            });
        }
        errors
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum WorkflowVersionStatus {
    Draft,
    Published,
    Retired,
}

/// One version of a workflow with its whole graph
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowVersion {
    pub version_no: i32,
    #[schema(inline)]
    pub status: WorkflowVersionStatus,
    /// The version new instances start on
    pub is_current: bool,
    /// Key of the state an instance starts in; null while a draft has none
    #[schema(required = true)]
    pub initial_state: Option<String>,
    pub states: Vec<WorkflowState>,
    pub transitions: Vec<WorkflowTransition>,
    #[schema(value_type = Option<Object>, required = true)]
    pub layout: Option<Value>,
    #[schema(required = true, pattern = "^[0-9a-f]{64}$")]
    pub checksum: Option<String>,
    #[schema(required = true)]
    pub change_note: Option<String>,
    #[schema(required = true)]
    #[serde(serialize_with = "schemas::ts_opt::serialize")]
    pub published_at: Option<DateTime<Utc>>,
    #[schema(required = true)]
    pub published_by_name: Option<String>,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
}

/// One version of a workflow, without its graph
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowVersionSummary {
    pub version_no: i32,
    #[schema(inline)]
    pub status: WorkflowVersionStatus,
    pub is_current: bool,
    pub state_count: i64,
    pub transition_count: i64,
    /// Running instances pinned to this version. Null when withheld: the caller may not view every type the
    /// workflow runs on.
    #[schema(value_type = Option<i64>, required = true)]
    pub active_instance_count: Count,
    #[schema(required = true, pattern = "^[0-9a-f]{64}$")]
    pub checksum: Option<String>,
    #[schema(required = true)]
    pub change_note: Option<String>,
    #[schema(required = true)]
    #[serde(serialize_with = "schemas::ts_opt::serialize")]
    pub published_at: Option<DateTime<Utc>>,
    #[schema(required = true)]
    pub published_by_name: Option<String>,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct WorkflowVersionList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    pub limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    pub offset: i64,
}
paged!(WorkflowVersionList);

// ---------------------------------------------------------------------------
// Lint and publish
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowProblemSeverity {
    /// Publishing is refused while it stands
    Error,
    /// Publishing goes ahead
    Warning,
}

/// One finding of the graph lint
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowProblem {
    /// Where in the draft body, e.g. `states[2]`, `transitions[0].fields[1].attribute`
    pub path: String,
    /// Machine-readable: no_states, no_initial_state, initial_state_terminal, unreachable_state, dead_end,
    /// no_terminal_reachable, terminal_has_transitions, unknown_attribute, inactive_attribute, attribute_type,
    /// unknown_value, op_type, no_state_attribute, state_value_list, state_value_inactive, ungranted_transition, ...
    pub code: String,
    pub message: String,
    #[schema(inline)]
    pub severity: WorkflowProblemSeverity,
}

/// The lint of the draft
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowValidation {
    /// True when no problem is an error: publishing would go ahead
    pub valid: bool,
    pub checksum: String,
    pub problems: Vec<WorkflowProblem>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowPublish {
    /// What changed, for the version list and the audit log
    #[schema(schema_with = change_note_schema)]
    #[serde(default)]
    pub change_note: Option<String>,
    /// The `checksum` of the draft you validated: 409 VERSION_CONFLICT if the draft changed in between
    #[schema(schema_with = checksum_schema)]
    pub expected_draft_checksum: String,
}

impl Check for WorkflowPublish {}

// ---------------------------------------------------------------------------
// Grants
// ---------------------------------------------------------------------------

/// A permission profile, by id and name
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowGrantProfile {
    pub id: Uuid,
    pub name: String,
}

/// The profiles that may run one transition
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowGrant {
    /// A transition key, or `_cancel` for cancelling an instance
    pub transition_key: String,
    pub profiles: Vec<WorkflowGrantProfile>,
}

/// Who may run which transition of a workflow
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowGrants {
    /// The definition's version: send it back with a change
    pub version: i32,
    pub grants: Vec<WorkflowGrant>,
}

fn grant_key_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .pattern(Some("^(_cancel|[a-z][a-z0-9_]{0,62})$"))
        .description(Some("A transition key (of any version), or `_cancel` for cancelling an instance"))
        .into()
}

fn grant_profiles_schema() -> Schema {
    ArrayBuilder::new()
        .items(ObjectBuilder::new().schema_type(Type::String).min_length(Some(1)).max_length(Some(200)))
        .max_items(Some(MAX_GRANT_PROFILES))
        .description(Some("Permission profiles by id or by name (regardless of case)"))
        .into()
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowGrantInput {
    #[schema(schema_with = grant_key_schema)]
    pub transition_key: String,
    #[schema(schema_with = grant_profiles_schema)]
    pub profiles: Vec<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowGrantsReplace {
    /// The version you loaded; if someone saved in between, the request fails with 409 VERSION_CONFLICT
    #[schema(minimum = 1)]
    pub version: i32,
    /// Every grant of the workflow (replaces the current set)
    #[schema(inline, max_items = 301)]
    pub grants: Vec<WorkflowGrantInput>,
}

impl Check for WorkflowGrantsReplace {
    fn check(&self) -> Vec<FieldError> {
        let mut seen = HashSet::new();
        self.grants
            .iter()
            .enumerate()
            .filter(|(_, g)| !seen.insert(g.transition_key.as_str()))
            .map(|(i, _)| FieldError {
                location: FieldLocation::Body,
                field: format!("grants[{i}].transitionKey"),
                message: "Listed more than once".into(),
                code: "duplicate".into(),
            })
            .collect()
    }
}
