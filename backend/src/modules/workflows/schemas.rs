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
    /// no running instance of it (0 on read once every one has). Their state field cannot be edited (it is driven
    /// by the workflow) until an instance is started on them.
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
    /// `UNINSTANCED_CIS` when the workflow is an active state-field driver: on read always (count 0 included), on
    /// create and update only when the change makes it one and some CIs lack an instance; empty otherwise
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

// ---------------------------------------------------------------------------
// Approval policies (approvals design SHAA-1869 §3.1, §10.1)
// ---------------------------------------------------------------------------

/// Bounds of a step's due interval, in minutes (the column's check).
pub const MIN_DUE_MINUTES: i32 = 15;
pub const MAX_DUE_MINUTES: i32 = 90 * 24 * 60;

/// A step's due interval in whole minutes, written as an ISO 8601 duration
/// of weeks, days, hours and minutes (`P2D`, `PT4H`, `P1DT12H`). It is
/// serialised in one canonical form (`P2D` for `PT48H`), so equal intervals
/// checksum equally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DueAfter(pub i32);

impl DueAfter {
    pub fn parse(s: &str) -> Option<DueAfter> {
        let rest = s.strip_prefix('P')?;
        let (date, time) = match rest.split_once('T') {
            Some((d, t)) if !t.is_empty() => (d, Some(t)),
            Some(_) => return None,
            None => (rest, None),
        };
        let mut minutes: i64 = 0;
        let mut any = false;
        let mut take = |part: &str, units: &[(char, i64)]| -> Option<()> {
            let mut part = part;
            let mut last = 0;
            while !part.is_empty() {
                let end = part.find(|c: char| !c.is_ascii_digit())?;
                let (digits, tail) = part.split_at(end);
                let unit = tail.chars().next()?;
                let pos = units.iter().position(|(u, _)| *u == unit)?;
                if digits.is_empty() || digits.len() > 7 || pos < last {
                    return None;
                }
                last = pos + 1;
                minutes += digits.parse::<i64>().ok()? * units[pos].1;
                any = true;
                part = &tail[1..];
            }
            Some(())
        };
        take(date, &[('W', 7 * 24 * 60), ('D', 24 * 60)])?;
        if let Some(t) = time {
            take(t, &[('H', 60), ('M', 1)])?;
        }
        (any && minutes <= i64::from(i32::MAX)).then_some(DueAfter(minutes as i32))
    }

    pub fn minutes(self) -> i32 {
        self.0
    }
}

impl std::fmt::Display for DueAfter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (days, rest) = (self.0 / (24 * 60), self.0 % (24 * 60));
        let (hours, minutes) = (rest / 60, rest % 60);
        write!(f, "P")?;
        if days > 0 {
            write!(f, "{days}D")?;
        }
        if hours > 0 || minutes > 0 || days == 0 {
            write!(f, "T")?;
            if hours > 0 {
                write!(f, "{hours}H")?;
            }
            if minutes > 0 || (hours == 0 && days == 0) {
                write!(f, "{minutes}M")?;
            }
        }
        Ok(())
    }
}

impl Serialize for DueAfter {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for DueAfter {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        let due = DueAfter::parse(&s).ok_or_else(|| {
            serde::de::Error::custom(
                "invalid_format|An ISO 8601 duration of weeks, days, hours and minutes, e.g. P2D, PT4H or P1DT12H",
            )
        })?;
        if !(MIN_DUE_MINUTES..=MAX_DUE_MINUTES).contains(&due.0) {
            return Err(serde::de::Error::custom("range|Between 15 minutes (PT15M) and 90 days (P90D)"));
        }
        Ok(due)
    }
}

fn due_after_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .pattern(Some(r"^P(\d+W)?(\d+D)?(T(\d+H)?(\d+M)?)?$"))
        .max_length(Some(40))
        .description(Some(
            "The step is overdue this long after it became active: an ISO 8601 duration of weeks, days, hours and \
             minutes (`P2D`, `PT4H`, `P1DT12H`), from 15 minutes to 90 days, wall-clock. Returned in canonical form \
             (`PT48H` is returned as `P2D`). Left out: no due date.",
        ))
        .into()
}

fn exclude_actors_schema() -> Schema {
    ArrayBuilder::new()
        .items(utoipa::openapi::RefOr::T(key_schema()))
        .max_items(Some(20))
        .description(Some(
            "Transition keys of this version: whoever ran one of them on the instance (and whoever requested it) may \
             not approve this step",
        ))
        .into()
}

/// What happens when a step is overdue (default `flag`): `flag` marks it overdue and adds the escalation approvers, `reject`
/// rejects the request (needs `dueAfter`)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum WorkflowApprovalOverdue {
    #[default]
    Flag,
    Reject,
}

fn one() -> i16 {
    1
}

/// One step of an approval policy
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApprovalStep {
    /// Unique in the transition; approver assignments refer to it
    #[schema(schema_with = key_schema)]
    pub key: String,
    #[schema(schema_with = workflow_name_schema)]
    #[serde(deserialize_with = "trimmed")]
    pub name: String,
    /// Approvals the step needs (1-20, default 1). One rejection rejects the request.
    #[schema(minimum = 1, maximum = 20)]
    #[serde(default = "one")]
    pub required_approvals: i16,
    #[schema(schema_with = due_after_schema, required = false)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_after: Option<DueAfter>,
    #[schema(inline)]
    #[serde(default)]
    pub on_overdue: WorkflowApprovalOverdue,
    /// Whoever approved an earlier step of the same request may not approve this one (default true)
    #[serde(default = "yes")]
    pub distinct_from_earlier: bool,
    #[schema(schema_with = exclude_actors_schema)]
    #[serde(default)]
    pub exclude_actors_of: Vec<String>,
    /// Decisions may come through an API token the approver minted for themselves (default false: a signed-in
    /// session only)
    #[serde(default)]
    pub allow_api_tokens: bool,
}

/// The approval policy of a transition: running it creates an approval request, and the instance moves only once
/// every step is approved, in order. The requester can never approve their own request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApproval {
    #[schema(min_items = 1, max_items = 5)]
    pub steps: Vec<WorkflowApprovalStep>,
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
    /// Left out: the transition runs without approval. Part of the checksum only when present, so a version
    /// without approvals keeps the checksum it had before approvals existed.
    #[schema(required = false)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<WorkflowApproval>,
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

/// What the schema cannot say about a policy: unique step keys, a due date
/// for `onOverdue: reject`, and no transition listed twice in `excludeActorsOf`.
/// Whether those transitions exist is the lint's question (publishing).
fn approval_problems(path: &str, a: &WorkflowApproval) -> Vec<FieldError> {
    let mut errors = duplicates(&format!("{path}.steps"), a.steps.iter().map(|s| s.key.as_str()));
    for (j, s) in a.steps.iter().enumerate() {
        if s.on_overdue == WorkflowApprovalOverdue::Reject && s.due_after.is_none() {
            errors.push(FieldError {
                location: FieldLocation::Body,
                field: format!("{path}.steps[{j}].dueAfter"),
                message: "onOverdue: reject needs a due interval".into(),
                code: "required".into(),
            });
        }
        let mut seen = HashSet::new();
        for (k, key) in s.exclude_actors_of.iter().enumerate() {
            if !seen.insert(key.as_str()) {
                errors.push(FieldError {
                    location: FieldLocation::Body,
                    field: format!("{path}.steps[{j}].excludeActorsOf[{k}]"),
                    message: "Listed more than once".into(),
                    code: "duplicate".into(),
                });
            }
        }
    }
    errors
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
            if let Some(a) = &t.approval {
                errors.extend(approval_problems(&format!("transitions[{i}].approval"), a));
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

pub(crate) fn grant_key_schema() -> Schema {
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

// ---------------------------------------------------------------------------
// Approvers (approvals design SHAA-1869 §3.1, §6.2, §10.1)
// ---------------------------------------------------------------------------

/// Approver assignments of one workflow.
pub const MAX_APPROVERS: usize = 500;

/// When an assignment applies
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema, sqlx::Type,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum WorkflowApproverRole {
    /// May decide the step while it is active
    #[default]
    Approver,
    /// May decide the step only once it is overdue
    Escalation,
}

impl WorkflowApproverRole {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkflowApproverRole::Approver => "approver",
            WorkflowApproverRole::Escalation => "escalation",
        }
    }
}

/// Where the approvers of an assignment come from: the holders of a permission `profile`, the members of a user
/// `group`, one named `user`, the user linked to the Person a reference field of the CI points at (`ci_attribute`,
/// for example the CI's owner), or the owners in one role of the business services the CI is a direct member of
/// (`service_owner`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum WorkflowApproverSource {
    Profile,
    Group,
    User,
    CiAttribute,
    ServiceOwner,
}

impl WorkflowApproverSource {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkflowApproverSource::Profile => "profile",
            WorkflowApproverSource::Group => "group",
            WorkflowApproverSource::User => "user",
            WorkflowApproverSource::CiAttribute => "ci_attribute",
            WorkflowApproverSource::ServiceOwner => "service_owner",
        }
    }
}

/// An owner role of a business service
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum WorkflowServiceOwnerRole {
    Technical,
    Business,
}

impl WorkflowServiceOwnerRole {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkflowServiceOwnerRole::Technical => "technical",
            WorkflowServiceOwnerRole::Business => "business",
        }
    }
}

/// A profile, group or user, by id and name (a user's name is their username)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowPrincipalRef {
    pub id: Uuid,
    pub name: String,
}

/// A reference field of the workflow's type (own or inherited)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowAttributeRef {
    pub id: Uuid,
    pub key: String,
    /// Key of the type that defines the field
    pub class_key: String,
    pub label: String,
}

/// Who may decide one step of a transition's approval policy
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApprover {
    pub transition_key: String,
    pub step_key: String,
    #[schema(inline)]
    pub role: WorkflowApproverRole,
    #[schema(inline)]
    pub source: WorkflowApproverSource,
    /// Set for `source: profile`
    #[schema(required = true)]
    pub profile: Option<WorkflowPrincipalRef>,
    /// Set for `source: group`
    #[schema(required = true)]
    pub group: Option<WorkflowPrincipalRef>,
    /// Set for `source: user`
    #[schema(required = true)]
    pub user: Option<WorkflowPrincipalRef>,
    /// Set for `source: ci_attribute`
    #[schema(required = true)]
    pub attribute: Option<WorkflowAttributeRef>,
    /// Set for `source: service_owner`
    #[schema(inline, required = true)]
    pub service_owner_role: Option<WorkflowServiceOwnerRole>,
}

/// The approver assignments of a workflow, with what the lint finds in them
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApprovers {
    /// The definition's version: send it back with a change
    pub version: i32,
    /// By transition, step, role and source
    pub approvers: Vec<WorkflowApprover>,
    /// Warnings about the assignments against the current version and the draft (an approval step nobody may
    /// approve, approvers who cannot view the type, too few approvers for a step's quorum, an assignment for a
    /// step neither has)
    pub problems: Vec<WorkflowProblem>,
}

fn principal_input_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .min_length(Some(1))
        .max_length(Some(200))
        .description(Some("By id, or by name regardless of case (a user by username)"))
        .into()
}

fn attribute_input_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .min_length(Some(1))
        .max_length(Some(200))
        .description(Some(
            "By id or by key: a reference field of the workflow's type (own or inherited) that points at the Person \
             type",
        ))
        .into()
}

/// One approver assignment; exactly the field named by `source` is set
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApproverInput {
    /// A transition with an approval policy in some version of the workflow, or in its draft
    #[schema(schema_with = key_schema)]
    pub transition_key: String,
    /// A step of that transition's policy
    #[schema(schema_with = key_schema)]
    pub step_key: String,
    #[schema(inline)]
    #[serde(default)]
    pub role: WorkflowApproverRole,
    #[schema(inline)]
    pub source: WorkflowApproverSource,
    #[schema(schema_with = principal_input_schema)]
    #[serde(default)]
    pub profile: Option<String>,
    #[schema(schema_with = principal_input_schema)]
    #[serde(default)]
    pub group: Option<String>,
    #[schema(schema_with = principal_input_schema)]
    #[serde(default)]
    pub user: Option<String>,
    #[schema(schema_with = attribute_input_schema)]
    #[serde(default)]
    pub attribute: Option<String>,
    #[schema(inline)]
    #[serde(default)]
    pub service_owner_role: Option<WorkflowServiceOwnerRole>,
}

impl WorkflowApproverInput {
    /// The field `source` names must be set, and no other.
    fn problems(&self, path: &str) -> Vec<FieldError> {
        let set = [
            (WorkflowApproverSource::Profile, "profile", self.profile.is_some()),
            (WorkflowApproverSource::Group, "group", self.group.is_some()),
            (WorkflowApproverSource::User, "user", self.user.is_some()),
            (WorkflowApproverSource::CiAttribute, "attribute", self.attribute.is_some()),
            (WorkflowApproverSource::ServiceOwner, "serviceOwnerRole", self.service_owner_role.is_some()),
        ];
        let mut out = Vec::new();
        for (source, field, present) in set {
            if source == self.source && !present {
                out.push(FieldError {
                    location: FieldLocation::Body,
                    field: format!("{path}.{field}"),
                    message: format!("Required for source {}", source.as_str()),
                    code: "required".into(),
                });
            } else if source != self.source && present {
                out.push(FieldError {
                    location: FieldLocation::Body,
                    field: format!("{path}.{field}"),
                    message: format!(
                        "Only for source {}; this assignment is {}",
                        source.as_str(),
                        self.source.as_str()
                    ),
                    code: "source_mismatch".into(),
                });
            }
        }
        out
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApproversReplace {
    /// The version you loaded; if someone saved in between, the request fails with 409 VERSION_CONFLICT
    #[schema(minimum = 1)]
    pub version: i32,
    /// Every assignment of the workflow (replaces the current set)
    #[schema(inline, max_items = 500)]
    pub approvers: Vec<WorkflowApproverInput>,
}

impl Check for WorkflowApproversReplace {
    fn check(&self) -> Vec<FieldError> {
        self.approvers.iter().enumerate().flat_map(|(i, a)| a.problems(&format!("approvers[{i}]"))).collect()
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct WorkflowApproverPreviewQuery {
    /// Transition key
    #[param(schema_with = key_schema)]
    pub transition: String,
    /// Step key
    #[param(schema_with = key_schema)]
    pub step: String,
    /// Resolve the CI-dependent sources (reference field, service owners) on this CI, and judge the view right on
    /// its type. Without it, those sources are not resolved and the view right is judged on the workflow's type.
    pub ci_id: Option<Uuid>,
    /// Treat this user as the requester: four-eyes excludes them
    pub requested_by: Option<Uuid>,
}

/// Why a user may or may not decide the step
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowApproverPreviewReason {
    /// May decide the step
    Eligible,
    /// Assigned only for escalation: may decide once the step is overdue
    EscalationOnly,
    /// The requester (four-eyes)
    Excluded,
    /// No permission profile of theirs lets them view the CI's type; they would never see the request
    NoViewRight,
    /// The account is disabled
    Inactive,
}

/// One user an assignment resolves to
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApproverPreviewUser {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
    /// True when the user may decide the step (`reason: eligible`)
    pub eligible: bool,
    #[schema(inline)]
    pub reason: WorkflowApproverPreviewReason,
    /// The reason, in words
    pub message: String,
    /// The assignments the user is reached through, e.g. `approver: group CAB`
    pub via: Vec<String>,
}

/// One assignment of the step and what it resolved to
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApproverPreviewSource {
    #[schema(inline)]
    pub role: WorkflowApproverRole,
    #[schema(inline)]
    pub source: WorkflowApproverSource,
    /// e.g. `group CAB`, `field server.owner`, `business service owners`
    pub label: String,
    /// Users it resolved to (active or not)
    pub user_count: i64,
    /// Why it resolved to nobody, or that it is resolved per CI; null otherwise
    #[schema(required = true)]
    pub note: Option<String>,
}

/// Who could decide one step, and why each user is in or out
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApproverPreview {
    pub transition_key: String,
    pub step_key: String,
    #[schema(required = true)]
    pub ci_id: Option<Uuid>,
    /// From the draft, else the current version; null when neither has the step
    #[schema(required = true)]
    pub required_approvals: Option<i32>,
    /// Distinct users who may decide the step now (`reason: eligible`)
    pub eligible_count: i64,
    /// Eligible users first, then by username; at most 500
    pub users: Vec<WorkflowApproverPreviewUser>,
    /// More users were resolved than are listed
    pub truncated: bool,
    pub sources: Vec<WorkflowApproverPreviewSource>,
}

/// One approver assignment in a configuration file; exactly one of `profile`, `group`, `user`, `attribute` and
/// `serviceOwner` is set
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApproverSpec {
    /// Transition key
    #[schema(schema_with = key_schema)]
    pub transition: String,
    /// Step key
    #[schema(schema_with = key_schema)]
    pub step: String,
    #[schema(inline)]
    #[serde(default)]
    pub role: WorkflowApproverRole,
    /// Permission profile name (case-insensitive), in the file or already here
    #[schema(schema_with = principal_input_schema)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// User group name (case-insensitive); groups are not part of a file and must exist here
    #[schema(schema_with = principal_input_schema)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Username (case-insensitive); users are not part of a file and must exist here
    #[schema(schema_with = principal_input_schema)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// `<type key>.<field key>`: a reference field to the Person type, of the workflow's type or an ancestor
    #[schema(schema_with = attribute_input_schema)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribute: Option<String>,
    #[schema(inline)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_owner: Option<WorkflowServiceOwnerRole>,
}

impl WorkflowApproverSpec {
    pub fn source_count(&self) -> usize {
        [self.profile.is_some(), self.group.is_some(), self.user.is_some(), self.attribute.is_some()]
            .into_iter()
            .filter(|b| *b)
            .count()
            + usize::from(self.service_owner.is_some())
    }
}

/// Start the workflow on the live CIs it covers that have no running instance of it
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowBootstrap {
    /// Must be true: each CI starts in the state whose state field value is the CI's current one
    pub state_from_attribute: bool,
    /// Count only; nothing is written
    #[serde(default)]
    pub dry_run: bool,
}

impl Check for WorkflowBootstrap {
    fn check(&self) -> Vec<FieldError> {
        if self.state_from_attribute {
            return Vec::new();
        }
        vec![FieldError {
            location: FieldLocation::Body,
            field: "stateFromAttribute".into(),
            message: "Only stateFromAttribute: true is supported: each CI starts in the state of its current value"
                .into(),
            code: "invalid_value".into(),
        }]
    }
}

/// The CIs of one state a bootstrap starts (or, for a terminal state, leaves alone)
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowBootstrapState {
    pub state_key: String,
    pub state_name: String,
    /// Key of the state field value that maps to this state
    pub value_key: String,
    /// A terminal state: these CIs are skipped (their lifecycle is over)
    pub terminal: bool,
    pub count: i64,
}

/// CIs a bootstrap skips because no state maps their state field value
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowBootstrapUnmapped {
    /// Null: the CIs have no value
    pub value_id: Option<Uuid>,
    pub value_key: Option<String>,
    pub value_name: Option<String>,
    pub count: i64,
}

/// What a bootstrap started (or, on a dry run, would start)
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowBootstrapResult {
    pub dry_run: bool,
    pub definition_key: String,
    /// The published version the instances run
    pub version_no: i32,
    /// Instances started; on a dry run, the instances a run would start now
    pub started: i64,
    /// Covered live CIs that already have a running instance (left alone)
    pub already_running: i64,
    /// Started and terminal CIs per state, in the version's state order
    pub states: Vec<WorkflowBootstrapState>,
    /// CIs in a terminal state, skipped
    pub skipped_terminal: i64,
    /// CIs whose value no state maps, skipped; most frequent first
    pub unmapped: Vec<WorkflowBootstrapUnmapped>,
    pub skipped_unmapped: i64,
}
