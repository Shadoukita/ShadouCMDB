//! Notification actions of a workflow definition (v0.4.0 design SHAA-2725
//! §2.1, §3, §8, §11.1; slice S3).
//!
//! An action says what to tell whom when something happens on an instance:
//! a kind (`inbox`, `email`, `webhook`), a trigger (a transition, an approval
//! event, a cancel or force), and who receives it. Actions live on the
//! definition, like grants and approvers: they are mutable and audited, and
//! need no new version. They never change CMDB data (attribute actions are
//! part of the version graph, S2).
//!
//! Nothing is sent from a request. The enqueue trigger (migrations 0073,
//! 0075) writes a run per matching action in the event's transaction, and the
//! workers of [`outbox`] deliver it after commit.
//!
//! This slice delivers the in-app inbox to profiles, groups and named users.
//! The schema and its checks cover every kind and source; e-mail (S4),
//! webhooks (S5) and the CI-dependent and participant sources (S4) are
//! refused with `kind_unavailable` / `source_unavailable` until their slice
//! ships, so nothing is saved that would never be delivered.

#[cfg(test)]
mod edge_tests;
pub mod outbox;
#[cfg(test)]
mod tests;

use std::collections::{BTreeSet, HashMap, HashSet};

use axum::extract::RawPathParams;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::types::Json as SqlJson;
use sqlx::{PgConnection, PgPool};
use utoipa::openapi::Required;
use utoipa::openapi::path::{Parameter, ParameterBuilder, ParameterIn};
use utoipa::openapi::schema::{ObjectBuilder, Schema, Type};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::schemas::{WorkflowDefinition, WorkflowPrincipalRef, WorkflowProblem, WorkflowProblemSeverity};
use super::service;
use crate::api::context::RequestContext;
use crate::api::route::{Check, PathInput};
use crate::api::schemas::{self as api_schemas, key_schema};
use crate::api::validate;
use crate::auth::permissions::ClassOp;
use crate::data::auth as auth_data;
use crate::data::crud::{self, AuditAction, AuditEntry};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};

/// Actions per (trigger, transition) of one definition; also the database's limit (0073).
pub const MAX_PER_TRIGGER: usize = 10;
/// Users a preview lists.
const PREVIEW_USERS: usize = 500;

// ---------------------------------------------------------------------------
// API shapes
// ---------------------------------------------------------------------------

/// How an action delivers
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum WorkflowActionKind {
    /// An entry in each recipient's in-app notifications
    Inbox,
    /// An e-mail to each recipient (not available yet)
    Email,
    /// A signed HTTPS request to a registered endpoint (not available yet)
    Webhook,
}

/// What fires an action
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum WorkflowActionTrigger {
    /// The transition `transition` was applied (run directly, in bulk, or by a final approval)
    Transition,
    /// An approval request for `transition` was made
    ApprovalRequested,
    /// A step of an approval request for `transition` became active (the first one included)
    ApprovalStep,
    /// An approval request for `transition` closed (filter with `settings.statuses`)
    ApprovalClosed,
    /// A step of an approval request for `transition` became overdue
    ApprovalOverdue,
    /// An instance was cancelled
    InstanceCancelled,
    /// An instance was forced into a state
    InstanceForced,
}

impl WorkflowActionTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkflowActionTrigger::Transition => "transition",
            WorkflowActionTrigger::ApprovalRequested => "approval_requested",
            WorkflowActionTrigger::ApprovalStep => "approval_step",
            WorkflowActionTrigger::ApprovalClosed => "approval_closed",
            WorkflowActionTrigger::ApprovalOverdue => "approval_overdue",
            WorkflowActionTrigger::InstanceCancelled => "instance_cancelled",
            WorkflowActionTrigger::InstanceForced => "instance_forced",
        }
    }

    /// The instance triggers name no transition; every other one does.
    pub fn has_transition(self) -> bool {
        !matches!(self, WorkflowActionTrigger::InstanceCancelled | WorkflowActionTrigger::InstanceForced)
    }
}

/// Where an action's recipients come from
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum WorkflowActionRecipientSource {
    /// The active users holding a permission profile
    Profile,
    /// The active members of a user group
    Group,
    /// One named user
    User,
    /// The CI's owner (not available yet)
    CiOwner,
    /// The user linked to the Person a reference field of the CI points at (not available yet)
    CiAttribute,
    /// The owners of the business services the CI is a direct member of (not available yet)
    ServiceOwner,
    /// Someone taking part in the event (not available yet)
    Participant,
    /// A fixed e-mail address, e-mail only (not available yet)
    Address,
}

impl WorkflowActionRecipientSource {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkflowActionRecipientSource::Profile => "profile",
            WorkflowActionRecipientSource::Group => "group",
            WorkflowActionRecipientSource::User => "user",
            WorkflowActionRecipientSource::CiOwner => "ci_owner",
            WorkflowActionRecipientSource::CiAttribute => "ci_attribute",
            WorkflowActionRecipientSource::ServiceOwner => "service_owner",
            WorkflowActionRecipientSource::Participant => "participant",
            WorkflowActionRecipientSource::Address => "address",
        }
    }

    /// Sources this release resolves.
    fn available(self) -> bool {
        matches!(
            self,
            WorkflowActionRecipientSource::Profile
                | WorkflowActionRecipientSource::Group
                | WorkflowActionRecipientSource::User
        )
    }
}

/// Who takes part in the event
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowActionParticipant {
    /// Who ran the event
    Actor,
    /// Who started the instance
    Starter,
    /// Who made the approval request (approval triggers)
    Requester,
    /// The eligible deciders of the active step (approval triggers)
    Approvers,
}

/// How much an e-mail tells
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowActionContent {
    /// That something needs attention, the workflow and transition names and a link; no CI data
    Minimal,
    /// Also the CI, the transition, the states, the actor and the comment
    Standard,
    /// Also the type's summary fields, redacted per recipient
    Detailed,
}

/// How an approval request closed (trigger `approval_closed`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowApprovalClosedStatus {
    Approved,
    Rejected,
    Withdrawn,
    Cancelled,
}

fn text_schema(max: usize) -> Schema {
    ObjectBuilder::new().schema_type(Type::String).min_length(Some(1)).max_length(Some(max)).into()
}

fn subject_schema() -> Schema {
    text_schema(200)
}

fn intro_schema() -> Schema {
    text_schema(2000)
}

/// A subject in English and German
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowActionSubject {
    #[schema(schema_with = subject_schema)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub en: Option<String>,
    #[schema(schema_with = subject_schema)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub de: Option<String>,
}

/// An intro text in English and German
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowActionIntro {
    #[schema(schema_with = intro_schema)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub en: Option<String>,
    #[schema(schema_with = intro_schema)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub de: Option<String>,
}

/// Settings of an action; each applies only to the kinds and triggers it names
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowActionSettings {
    /// Inbox and e-mail: leave out who ran the event (default true), so nobody is told of their own action
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_actor: Option<bool>,
    /// Trigger `approval_closed`: only these outcomes (default all)
    #[schema(min_items = 1, max_items = 4)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statuses: Option<Vec<WorkflowApprovalClosedStatus>>,
    /// E-mail: how much the message tells (default standard)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<WorkflowActionContent>,
    /// E-mail: the subject; placeholders `{{ci.label}}`, `{{ci.ident}}`, `{{ci.class}}`, `{{workflow.name}}`,
    /// `{{transition.name}}`, `{{state.from}}`, `{{state.to}}`, `{{actor.name}}`, `{{approval.step}}`,
    /// `{{approval.dueAt}}`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<WorkflowActionSubject>,
    /// E-mail: a paragraph above the built-in text, with the placeholders of `subject`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intro: Option<WorkflowActionIntro>,
    /// Webhook: the CI fields (keys, own or inherited) the payload carries; none by default
    #[schema(max_items = 50)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_attributes: Option<Vec<String>>,
}

/// The placeholders of an e-mail's subject and intro.
pub const PLACEHOLDERS: &[&str] = &[
    "ci.label",
    "ci.ident",
    "ci.class",
    "workflow.name",
    "transition.name",
    "state.from",
    "state.to",
    "actor.name",
    "approval.step",
    "approval.dueAt",
];

/// Placeholders that render empty in `minimal` content.
const CI_PLACEHOLDERS: &[&str] = &["ci.label", "ci.ident", "ci.class"];

/// `{{name}}` occurrences in `text`.
fn placeholders(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else { break };
        out.push(after[..end].trim());
        rest = &after[end + 2..];
    }
    out
}

fn principal_input_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .min_length(Some(1))
        .max_length(Some(200))
        .description(Some("By id, or by name regardless of case (a user by username)"))
        .into()
}

fn address_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .min_length(Some(3))
        .max_length(Some(254))
        .pattern(Some(r"^[^@\s]+@[^@\s]+$"))
        .into()
}

fn name_schema() -> Schema {
    ObjectBuilder::new().schema_type(Type::String).min_length(Some(1)).max_length(Some(100)).into()
}

fn endpoint_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .pattern(Some("^[a-z][a-z0-9_-]{0,62}$"))
        .description(Some("Key of a registered webhook endpoint"))
        .into()
}

/// One source of an action's recipients; exactly the field `source` names is set (none for `ci_owner`)
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowActionRecipientInput {
    pub source: WorkflowActionRecipientSource,
    #[schema(schema_with = principal_input_schema)]
    #[serde(default)]
    pub profile: Option<String>,
    #[schema(schema_with = principal_input_schema)]
    #[serde(default)]
    pub group: Option<String>,
    #[schema(schema_with = principal_input_schema)]
    #[serde(default)]
    pub user: Option<String>,
    /// By id or by key: a reference field of the workflow's type to the Person type
    #[schema(schema_with = principal_input_schema)]
    #[serde(default)]
    pub attribute: Option<String>,
    #[serde(default)]
    pub service_owner_role: Option<super::schemas::WorkflowServiceOwnerRole>,
    #[serde(default)]
    pub participant: Option<WorkflowActionParticipant>,
    #[schema(schema_with = address_schema)]
    #[serde(default)]
    pub address: Option<String>,
}

impl WorkflowActionRecipientInput {
    /// The field `source` names must be set, and no other.
    fn problems(&self, path: &str) -> Vec<FieldError> {
        use WorkflowActionRecipientSource as S;
        let set = [
            (S::Profile, "profile", self.profile.is_some()),
            (S::Group, "group", self.group.is_some()),
            (S::User, "user", self.user.is_some()),
            (S::CiAttribute, "attribute", self.attribute.is_some()),
            (S::ServiceOwner, "serviceOwnerRole", self.service_owner_role.is_some()),
            (S::Participant, "participant", self.participant.is_some()),
            (S::Address, "address", self.address.is_some()),
        ];
        let mut out = Vec::new();
        for (source, field, present) in set {
            if source == self.source && !present {
                out.push(body_error(
                    format!("{path}.{field}"),
                    "required",
                    format!("Required for source {}", source.as_str()),
                ));
            } else if source != self.source && present {
                out.push(body_error(
                    format!("{path}.{field}"),
                    "source_mismatch",
                    format!("Only for source {}; this recipient is {}", source.as_str(), self.source.as_str()),
                ));
            }
        }
        out
    }
}

/// One notification action as sent
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowActionInput {
    /// Unique within the workflow; keeps the action's delivery history when it is renamed
    #[schema(schema_with = key_schema)]
    pub key: String,
    #[schema(schema_with = name_schema)]
    pub name: String,
    pub kind: WorkflowActionKind,
    pub trigger: WorkflowActionTrigger,
    /// The transition (the request's transition for the approval triggers); null for the instance triggers
    #[schema(schema_with = super::schemas::nullable_key_schema)]
    #[serde(default)]
    pub transition: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Inbox and e-mail: 1 to 20 sources, expanded and deduplicated when the action runs
    #[schema(max_items = 20)]
    #[serde(default)]
    pub recipients: Vec<WorkflowActionRecipientInput>,
    /// Webhook: the endpoint
    #[schema(schema_with = endpoint_schema)]
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub settings: WorkflowActionSettings,
}

fn yes() -> bool {
    true
}

fn body_error(field: String, code: &str, message: String) -> FieldError {
    FieldError { location: FieldLocation::Body, field, message, code: code.into() }
}

impl WorkflowActionInput {
    /// What the body alone can tell.
    fn problems(&self, path: &str) -> Vec<FieldError> {
        let mut out = Vec::new();
        let err = |out: &mut Vec<FieldError>, field: &str, code: &str, message: String| {
            out.push(body_error(format!("{path}.{field}"), code, message))
        };
        if self.name.trim().is_empty() {
            err(&mut out, "name", "required", "Required".into());
        }
        match (self.trigger.has_transition(), &self.transition) {
            (true, None) => {
                err(&mut out, "transition", "required", format!("Required for trigger {}", self.trigger.as_str()))
            }
            (false, Some(_)) => err(
                &mut out,
                "transition",
                "not_applicable",
                format!("Trigger {} fires for every transition key; leave transition out", self.trigger.as_str()),
            ),
            _ => {}
        }
        let notifies_people = self.kind != WorkflowActionKind::Webhook;
        if notifies_people && self.recipients.is_empty() {
            err(&mut out, "recipients", "required", "At least one recipient source is required".into());
        }
        if !notifies_people && !self.recipients.is_empty() {
            err(&mut out, "recipients", "not_applicable", "A webhook goes to its endpoint, not to recipients".into());
        }
        match (self.kind, &self.endpoint) {
            (WorkflowActionKind::Webhook, None) => {
                err(&mut out, "endpoint", "required", "Required for a webhook".into())
            }
            (WorkflowActionKind::Webhook, Some(_)) | (_, None) => {}
            (_, Some(_)) => err(&mut out, "endpoint", "not_applicable", "Only for a webhook".into()),
        }
        for (i, r) in self.recipients.iter().enumerate() {
            out.extend(r.problems(&format!("{path}.recipients[{i}]")));
            if r.source == WorkflowActionRecipientSource::Address && self.kind != WorkflowActionKind::Email {
                err(
                    &mut out,
                    &format!("recipients[{i}].source"),
                    "not_applicable",
                    "A fixed address has no inbox: e-mail only".into(),
                );
            }
            if r.participant.is_some_and(|p| {
                matches!(p, WorkflowActionParticipant::Requester | WorkflowActionParticipant::Approvers)
            }) && !matches!(
                self.trigger,
                WorkflowActionTrigger::ApprovalRequested
                    | WorkflowActionTrigger::ApprovalStep
                    | WorkflowActionTrigger::ApprovalClosed
                    | WorkflowActionTrigger::ApprovalOverdue
            ) {
                err(
                    &mut out,
                    &format!("recipients[{i}].participant"),
                    "not_applicable",
                    "The requester and the approvers exist only for the approval triggers".into(),
                );
            }
        }
        let s = &self.settings;
        let only = |out: &mut Vec<FieldError>, set: bool, applies: bool, field: &str, what: &str| {
            if set && !applies {
                out.push(body_error(format!("{path}.settings.{field}"), "not_applicable", format!("Only for {what}")));
            }
        };
        only(&mut out, s.exclude_actor.is_some(), notifies_people, "excludeActor", "inbox and e-mail actions");
        only(
            &mut out,
            s.statuses.is_some(),
            self.trigger == WorkflowActionTrigger::ApprovalClosed,
            "statuses",
            "trigger approval_closed",
        );
        let email = self.kind == WorkflowActionKind::Email;
        only(&mut out, s.content.is_some(), email, "content", "e-mail actions");
        only(&mut out, s.subject.is_some(), email, "subject", "e-mail actions");
        only(&mut out, s.intro.is_some(), email, "intro", "e-mail actions");
        only(
            &mut out,
            s.include_attributes.is_some(),
            self.kind == WorkflowActionKind::Webhook,
            "includeAttributes",
            "webhook actions",
        );
        let texts = [
            ("subject.en", s.subject.as_ref().and_then(|t| t.en.as_deref())),
            ("subject.de", s.subject.as_ref().and_then(|t| t.de.as_deref())),
            ("intro.en", s.intro.as_ref().and_then(|t| t.en.as_deref())),
            ("intro.de", s.intro.as_ref().and_then(|t| t.de.as_deref())),
        ];
        for (field, text) in texts {
            for p in placeholders(text.unwrap_or_default()) {
                if !PLACEHOLDERS.contains(&p) {
                    err(
                        &mut out,
                        &format!("settings.{field}"),
                        "unknown_placeholder",
                        format!("Unknown placeholder {{{{{p}}}}}; use one of {}", PLACEHOLDERS.join(", ")),
                    );
                }
            }
        }
        if let Some(keys) = &s.include_attributes {
            let mut seen = HashSet::new();
            for (i, k) in keys.iter().enumerate() {
                if !seen.insert(k) {
                    err(
                        &mut out,
                        &format!("settings.includeAttributes[{i}]"),
                        "duplicate",
                        "Listed more than once".into(),
                    );
                }
            }
        }
        out
    }
}

/// Replace every notification action of a workflow
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowActionsReplace {
    /// The version you loaded; if someone saved in between, the request fails with 409 VERSION_CONFLICT
    #[schema(minimum = 1)]
    pub version: i32,
    /// Every action of the workflow, in order (replaces the current set; an action keeps its history by key)
    #[schema(max_items = 200)]
    pub actions: Vec<WorkflowActionInput>,
}

impl Check for WorkflowActionsReplace {
    fn check(&self) -> Vec<FieldError> {
        let mut out: Vec<FieldError> =
            self.actions.iter().enumerate().flat_map(|(i, a)| a.problems(&format!("actions[{i}]"))).collect();
        let mut keys = HashSet::new();
        let mut per_trigger: HashMap<(WorkflowActionTrigger, Option<&str>), usize> = HashMap::new();
        for (i, a) in self.actions.iter().enumerate() {
            if !keys.insert(a.key.as_str()) {
                out.push(body_error(
                    format!("actions[{i}].key"),
                    "duplicate",
                    format!("Key {} is used more than once", a.key),
                ));
            }
            let n = per_trigger.entry((a.trigger, a.transition.as_deref())).or_default();
            *n += 1;
            if *n == MAX_PER_TRIGGER + 1 {
                out.push(body_error(
                    format!("actions[{i}]"),
                    "too_many_actions",
                    format!("At most {MAX_PER_TRIGGER} actions per trigger and transition"),
                ));
            }
        }
        out
    }
}

/// A webhook endpoint, by key and name
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowActionEndpointRef {
    pub id: Uuid,
    pub key: String,
    pub name: String,
}

/// A reference field of the workflow's type
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowActionAttributeRef {
    pub id: Uuid,
    pub key: String,
}

/// One source of an action's recipients
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowActionRecipient {
    pub source: WorkflowActionRecipientSource,
    pub profile: Option<WorkflowPrincipalRef>,
    pub group: Option<WorkflowPrincipalRef>,
    pub user: Option<WorkflowPrincipalRef>,
    pub attribute: Option<WorkflowActionAttributeRef>,
    pub service_owner_role: Option<super::schemas::WorkflowServiceOwnerRole>,
    pub participant: Option<WorkflowActionParticipant>,
    pub address: Option<String>,
}

/// One notification action of a workflow
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowAction {
    pub id: Uuid,
    pub key: String,
    pub name: String,
    pub kind: WorkflowActionKind,
    pub trigger: WorkflowActionTrigger,
    pub transition: Option<String>,
    pub enabled: bool,
    pub recipients: Vec<WorkflowActionRecipient>,
    pub endpoint: Option<WorkflowActionEndpointRef>,
    pub settings: WorkflowActionSettings,
}

/// The notification actions of a workflow, with what the lint finds in them
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowActions {
    /// The definition's version: send it back with a change
    pub version: i32,
    /// In the order they were saved
    pub actions: Vec<WorkflowAction>,
    /// Warnings: `unknown_transition` (the current version lacks it; kept for instances on older versions),
    /// `recipients_cannot_view` (no active user of a source may view the workflow's type), `too_many_recipients`
    /// (more than `WORKFLOW_ACTIONS_MAX_RECIPIENTS` users; the rest are left out), `missing_locale` and
    /// `minimal_placeholder` (e-mail)
    pub problems: Vec<WorkflowProblem>,
}

// ---------------------------------------------------------------------------
// Stored form
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct ActionRow {
    id: Uuid,
    key: String,
    name: String,
    kind: WorkflowActionKind,
    trigger: WorkflowActionTrigger,
    transition_key: Option<String>,
    enabled: bool,
    endpoint_id: Option<Uuid>,
    endpoint_key: Option<String>,
    endpoint_name: Option<String>,
    settings: SqlJson<WorkflowActionSettings>,
}

#[derive(sqlx::FromRow)]
struct RecipientRow {
    action_id: Uuid,
    source: WorkflowActionRecipientSource,
    profile_id: Option<Uuid>,
    profile_name: Option<String>,
    group_id: Option<Uuid>,
    group_name: Option<String>,
    user_id: Option<Uuid>,
    username: Option<String>,
    attribute_id: Option<Uuid>,
    attribute_key: Option<String>,
    service_owner_role: Option<super::schemas::WorkflowServiceOwnerRole>,
    participant: Option<String>,
    address: Option<String>,
}

fn participant_of(s: &str) -> Option<WorkflowActionParticipant> {
    serde_json::from_value(Value::String(s.to_owned())).ok()
}

fn participant_str(p: WorkflowActionParticipant) -> &'static str {
    match p {
        WorkflowActionParticipant::Actor => "actor",
        WorkflowActionParticipant::Starter => "starter",
        WorkflowActionParticipant::Requester => "requester",
        WorkflowActionParticipant::Approvers => "approvers",
    }
}

/// The actions of definition `id`, in order.
pub async fn load(conn: &mut PgConnection, id: Uuid) -> Result<Vec<WorkflowAction>, AppError> {
    let rows: Vec<ActionRow> = sqlx::query_as(
        "SELECT a.id, a.key, a.name, a.kind, a.trigger, a.transition_key, a.enabled, a.endpoint_id,
                e.key AS endpoint_key, e.name AS endpoint_name, a.settings
         FROM cmdb.workflow_actions a LEFT JOIN cmdb.webhook_endpoints e ON e.id = a.endpoint_id
         WHERE a.definition_id = $1 ORDER BY a.position, a.key",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let recipients: Vec<RecipientRow> = sqlx::query_as(
        "SELECT r.action_id, r.source, r.profile_id, p.name AS profile_name, r.group_id, g.name AS group_name,
                r.user_id, u.username, r.attribute_id, ad.key AS attribute_key, r.service_owner_role, r.participant,
                r.address
         FROM cmdb.workflow_action_recipients r
         LEFT JOIN cmdb.permission_profiles p ON p.id = r.profile_id
         LEFT JOIN cmdb.user_groups g ON g.id = r.group_id
         LEFT JOIN cmdb.users u ON u.id = r.user_id
         LEFT JOIN cmdb.ci_attribute_definitions ad ON ad.id = r.attribute_id
         WHERE r.action_id = ANY($1) ORDER BY r.action_id, r.position",
    )
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await?;
    let mut by_action: HashMap<Uuid, Vec<WorkflowActionRecipient>> = HashMap::new();
    for r in recipients {
        let principal = |id: Option<Uuid>, name: Option<String>| {
            id.map(|id| WorkflowPrincipalRef { id, name: name.unwrap_or_default() })
        };
        by_action.entry(r.action_id).or_default().push(WorkflowActionRecipient {
            source: r.source,
            profile: principal(r.profile_id, r.profile_name),
            group: principal(r.group_id, r.group_name),
            user: principal(r.user_id, r.username),
            attribute: r
                .attribute_id
                .map(|id| WorkflowActionAttributeRef { id, key: r.attribute_key.unwrap_or_default() }),
            service_owner_role: r.service_owner_role,
            participant: r.participant.as_deref().and_then(participant_of),
            address: r.address,
        });
    }
    Ok(rows
        .into_iter()
        .map(|r| WorkflowAction {
            recipients: by_action.remove(&r.id).unwrap_or_default(),
            endpoint: r.endpoint_id.map(|id| WorkflowActionEndpointRef {
                id,
                key: r.endpoint_key.unwrap_or_default(),
                name: r.endpoint_name.unwrap_or_default(),
            }),
            id: r.id,
            key: r.key,
            name: r.name,
            kind: r.kind,
            trigger: r.trigger,
            transition: r.transition_key,
            enabled: r.enabled,
            settings: r.settings.0,
        })
        .collect())
}

/// The actions in the audit log's form: by key, recipients and endpoint by name.
pub fn specs(actions: &[WorkflowAction]) -> Value {
    Value::Array(
        actions
            .iter()
            .map(|a| {
                let recipients: Vec<Value> = a
                    .recipients
                    .iter()
                    .map(|r| {
                        let named = r
                            .profile
                            .as_ref()
                            .or(r.group.as_ref())
                            .or(r.user.as_ref())
                            .map(|p| json!(p.name))
                            .or_else(|| r.attribute.as_ref().map(|f| json!(f.key)))
                            .or_else(|| r.service_owner_role.map(|s| json!(s.as_str())))
                            .or_else(|| r.participant.map(|p| json!(participant_str(p))))
                            .or_else(|| r.address.as_ref().map(|a| json!(a)))
                            .unwrap_or(json!(true));
                        let mut o = serde_json::Map::new();
                        o.insert(r.source.as_str().to_owned(), named);
                        Value::Object(o)
                    })
                    .collect();
                json!({
                    "key": a.key, "name": a.name, "kind": a.kind, "trigger": a.trigger, "transition": a.transition,
                    "enabled": a.enabled, "recipients": recipients,
                    "endpoint": a.endpoint.as_ref().map(|e| &e.key), "settings": a.settings,
                })
            })
            .collect(),
    )
}

// ---------------------------------------------------------------------------
// Lint
// ---------------------------------------------------------------------------

/// Transition keys of definition `id`: in any version or the draft, and in its current version.
async fn transition_keys(conn: &mut PgConnection, id: Uuid) -> Result<(HashSet<String>, HashSet<String>), AppError> {
    let rows: Vec<(String, bool)> = sqlx::query_as(
        "SELECT DISTINCT t.key, v.id = d.current_version_id FROM cmdb.workflow_transitions t
         JOIN cmdb.workflow_versions v ON v.id = t.version_id
         JOIN cmdb.workflow_definitions d ON d.id = v.definition_id
         WHERE v.definition_id = $1",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let any = rows.iter().map(|(k, _)| k.clone()).collect();
    let current = rows.into_iter().filter(|(_, c)| *c).map(|(k, _)| k).collect();
    Ok((any, current))
}

/// The active users each profile, group or named user source resolves to.
async fn static_members(conn: &mut PgConnection, actions: &[WorkflowAction]) -> Result<Vec<(Uuid, Uuid)>, AppError> {
    let pick = |f: fn(&WorkflowActionRecipient) -> Option<&WorkflowPrincipalRef>| -> Vec<Uuid> {
        actions.iter().flat_map(|a| a.recipients.iter()).filter_map(|r| f(r).map(|p| p.id)).collect()
    };
    Ok(sqlx::query_as(
        "SELECT up.profile_id, up.user_id FROM cmdb.user_permission_profiles up
         JOIN cmdb.users u ON u.id = up.user_id WHERE up.profile_id = ANY($1) AND u.is_active
         UNION ALL
         SELECT m.group_id, m.user_id FROM cmdb.user_group_members m
         JOIN cmdb.users u ON u.id = m.user_id WHERE m.group_id = ANY($2) AND u.is_active
         UNION ALL
         SELECT id, id FROM cmdb.users WHERE id = ANY($3) AND is_active",
    )
    .bind(pick(|r| r.profile.as_ref()))
    .bind(pick(|r| r.group.as_ref()))
    .bind(pick(|r| r.user.as_ref()))
    .fetch_all(&mut *conn)
    .await?)
}

fn warning(path: String, code: &str, message: String) -> WorkflowProblem {
    WorkflowProblem { path, code: code.into(), message, severity: WorkflowProblemSeverity::Warning }
}

/// The lint of definition `d`'s actions (warnings only; what is refused is refused on save).
async fn problems(
    conn: &mut PgConnection,
    d: &WorkflowDefinition,
    actions: &[WorkflowAction],
    max_recipients: usize,
) -> Result<Vec<WorkflowProblem>, AppError> {
    let (_, current) = transition_keys(&mut *conn, d.id).await?;
    let members = static_members(&mut *conn, actions).await?;
    let users: Vec<Uuid> = members.iter().map(|(_, u)| *u).collect::<BTreeSet<_>>().into_iter().collect();
    let permissions = auth_data::load_permissions_of(&mut *conn, &users).await?;
    let mut viewers: HashMap<Uuid, HashSet<Uuid>> = HashMap::new();
    for (source, user) in &members {
        let set = viewers.entry(*source).or_default();
        if permissions.get(user).is_some_and(|p| p.can(d.class_id, ClassOp::View)) {
            set.insert(*user);
        }
    }
    let mut out = Vec::new();
    for (i, a) in actions.iter().enumerate() {
        let path = format!("actions[{i}]");
        if let Some(t) = &a.transition
            && !current.contains(t)
        {
            out.push(warning(
                format!("{path}.transition"),
                "unknown_transition",
                format!(
                    "The current version has no transition {t}: action {} fires only for instances on a version \
                     that has it",
                    a.key
                ),
            ));
        }
        let mut reached: HashSet<Uuid> = HashSet::new();
        for (j, r) in a.recipients.iter().enumerate() {
            let Some(p) = r.profile.as_ref().or(r.group.as_ref()).or(r.user.as_ref()) else { continue };
            let seen = viewers.get(&p.id).cloned().unwrap_or_default();
            if seen.is_empty() {
                out.push(warning(
                    format!("{path}.recipients[{j}]"),
                    "recipients_cannot_view",
                    format!(
                        "No active user of {} {} may view type {}: they would receive nothing",
                        r.source.as_str(),
                        p.name,
                        d.class_key
                    ),
                ));
            }
            reached.extend(seen);
        }
        if reached.len() > max_recipients {
            out.push(warning(
                format!("{path}.recipients"),
                "too_many_recipients",
                format!(
                    "Action {} reaches {} users; only the first {max_recipients} (WORKFLOW_ACTIONS_MAX_RECIPIENTS) \
                     are notified",
                    a.key,
                    reached.len()
                ),
            ));
        }
        if a.kind == WorkflowActionKind::Email {
            let s = &a.settings;
            let texts = [
                ("subject", s.subject.as_ref().map(|t| (&t.en, &t.de))),
                ("intro", s.intro.as_ref().map(|t| (&t.en, &t.de))),
            ];
            for (field, text) in texts {
                if let Some((en, de)) = text
                    && en.is_some() != de.is_some()
                {
                    out.push(warning(
                        format!("{path}.settings.{field}"),
                        "missing_locale",
                        format!("The {field} is given in one language only; the other one falls back to it"),
                    ));
                }
                if s.content == Some(WorkflowActionContent::Minimal)
                    && let Some((en, de)) = text
                    && [en, de]
                        .iter()
                        .filter_map(|t| t.as_deref())
                        .flat_map(placeholders)
                        .any(|p| CI_PLACEHOLDERS.contains(&p))
                {
                    out.push(warning(
                        format!("{path}.settings.{field}"),
                        "minimal_placeholder",
                        "Minimal content names no CI: the CI placeholders render empty".into(),
                    ));
                }
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// GET and PUT
// ---------------------------------------------------------------------------

pub async fn get(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    max_recipients: usize,
) -> Result<WorkflowActions, AppError> {
    let mut conn = pool.acquire().await?;
    let d = service::load_for(&mut conn, ctx, id, false, service::Access::Read).await?;
    let actions = load(&mut conn, id).await?;
    let problems = problems(&mut conn, &d, &actions, max_recipients).await?;
    Ok(WorkflowActions { version: d.version, actions, problems })
}

/// A recipient source resolved to what it is stored by.
struct Resolved {
    source: WorkflowActionRecipientSource,
    profile: Option<Uuid>,
    group: Option<Uuid>,
    user: Option<Uuid>,
}

pub async fn replace(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    b: &WorkflowActionsReplace,
    max_recipients: usize,
) -> Result<WorkflowActions, AppError> {
    let mut tx = pool.begin().await?;
    let before = service::load_for(&mut tx, ctx, id, true, service::Access::Write).await?;
    service::check_version(b.version, before.version)?;
    let (known, _) = transition_keys(&mut tx, id).await?;
    let named = |f: fn(&WorkflowActionRecipientInput) -> &Option<String>| -> Vec<String> {
        b.actions
            .iter()
            .flat_map(|a| a.recipients.iter())
            .filter_map(|r| f(r).as_ref().map(|s| s.to_lowercase()))
            .collect()
    };
    let lookup = |sql: &'static str, names: Vec<String>| sqlx::query_as::<_, (Uuid, String)>(sql).bind(names);
    let profiles = lookup(
        "SELECT id, name FROM cmdb.permission_profiles WHERE id::text = ANY($1) OR lower(name) = ANY($1)",
        named(|r| &r.profile),
    )
    .fetch_all(&mut *tx)
    .await?;
    let groups = lookup(
        "SELECT id, name FROM cmdb.user_groups WHERE id::text = ANY($1) OR lower(name) = ANY($1)",
        named(|r| &r.group),
    )
    .fetch_all(&mut *tx)
    .await?;
    let users = lookup(
        "SELECT id, username FROM cmdb.users WHERE id::text = ANY($1) OR lower(username) = ANY($1)",
        named(|r| &r.user),
    )
    .fetch_all(&mut *tx)
    .await?;
    let find = |list: &[(Uuid, String)], given: &str| -> Option<Uuid> {
        let by_id = validate::is_uuid(given).then(|| given.parse::<Uuid>().ok()).flatten();
        list.iter().find(|(id, name)| Some(*id) == by_id || name.to_lowercase() == given.to_lowercase()).map(|r| r.0)
    };

    let mut errors = Vec::new();
    let mut resolved: Vec<Vec<Resolved>> = Vec::with_capacity(b.actions.len());
    for (i, a) in b.actions.iter().enumerate() {
        let path = format!("actions[{i}]");
        match a.kind {
            WorkflowActionKind::Inbox => {}
            WorkflowActionKind::Email => errors.push(body_error(
                format!("{path}.kind"),
                "kind_unavailable",
                "E-mail actions are not available yet: they need the outbound mail settings of a later release".into(),
            )),
            WorkflowActionKind::Webhook => errors.push(body_error(
                format!("{path}.kind"),
                "kind_unavailable",
                "Webhook actions are not available yet: they need the webhook endpoints of a later release".into(),
            )),
        }
        if let Some(t) = &a.transition
            && !known.contains(t)
        {
            errors.push(body_error(
                format!("{path}.transition"),
                "unknown_transition",
                format!("Workflow {} has no transition {t} in any version or in its draft", before.key),
            ));
        }
        let mut list = Vec::with_capacity(a.recipients.len());
        let mut seen = HashSet::new();
        for (j, r) in a.recipients.iter().enumerate() {
            let rpath = format!("{path}.recipients[{j}]");
            if !r.source.available() {
                errors.push(body_error(
                    format!("{rpath}.source"),
                    "source_unavailable",
                    format!(
                        "Recipient source {} is not available yet; use a profile, a group or a named user",
                        r.source.as_str()
                    ),
                ));
                continue;
            }
            let (list_of, given, what) = match r.source {
                WorkflowActionRecipientSource::Profile => (&profiles, r.profile.as_deref(), "permission profile"),
                WorkflowActionRecipientSource::Group => (&groups, r.group.as_deref(), "user group"),
                _ => (&users, r.user.as_deref(), "user"),
            };
            let given = given.unwrap_or_default();
            let Some(found) = find(list_of, given) else {
                let field = match r.source {
                    WorkflowActionRecipientSource::Profile => "profile",
                    WorkflowActionRecipientSource::Group => "group",
                    _ => "user",
                };
                errors.push(body_error(format!("{rpath}.{field}"), "not_found", format!("No {what} \"{given}\"")));
                continue;
            };
            if !seen.insert((r.source, found)) {
                errors.push(body_error(rpath, "duplicate", "The same recipient is listed more than once".into()));
                continue;
            }
            list.push(Resolved {
                source: r.source,
                profile: (r.source == WorkflowActionRecipientSource::Profile).then_some(found),
                group: (r.source == WorkflowActionRecipientSource::Group).then_some(found),
                user: (r.source == WorkflowActionRecipientSource::User).then_some(found),
            });
        }
        resolved.push(list);
    }
    if !errors.is_empty() {
        return Err(AppError::validation(errors));
    }

    let old = load(&mut tx, id).await?;
    let version = store(&mut tx, ctx, &before, &old, &b.actions, &resolved).await?;
    let actions = load(&mut tx, id).await?;
    let problems = problems(&mut tx, &before, &actions, max_recipients).await?;
    tx.commit().await?;
    Ok(WorkflowActions { version, actions, problems })
}

/// Writes the actions: rows kept by key (so their runs keep their action),
/// new ones inserted, missing ones deleted; recipients rewritten. Audited
/// when anything changed. Returns the definition's version now.
async fn store(
    tx: &mut PgConnection,
    ctx: &RequestContext,
    before: &WorkflowDefinition,
    old: &[WorkflowAction],
    actions: &[WorkflowActionInput],
    resolved: &[Vec<Resolved>],
) -> Result<i32, AppError> {
    let id = before.id;
    let keys: Vec<&str> = actions.iter().map(|a| a.key.as_str()).collect();
    sqlx::query("DELETE FROM cmdb.workflow_actions WHERE definition_id = $1 AND NOT (key = ANY($2))")
        .bind(id)
        .bind(&keys)
        .execute(&mut *tx)
        .await?;
    // Park every kept row on a transition key of its own first, so the
    // database's per-trigger limit, counted row by row, never sees a
    // half-moved set. Only this transaction sees the parked rows.
    sqlx::query(
        "UPDATE cmdb.workflow_actions SET trigger = 'transition', transition_key = 'parked_' || replace(id::text, '-', '')
         WHERE definition_id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    let mut action_ids = Vec::with_capacity(actions.len());
    for (n, a) in actions.iter().enumerate() {
        let settings = SqlJson(&a.settings);
        let row: Uuid = sqlx::query_scalar(
            "INSERT INTO cmdb.workflow_actions
               (definition_id, key, name, kind, trigger, transition_key, enabled, position, endpoint_id, settings)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NULL, $9)
             ON CONFLICT (definition_id, key) DO UPDATE SET
               name = EXCLUDED.name, kind = EXCLUDED.kind, trigger = EXCLUDED.trigger,
               transition_key = EXCLUDED.transition_key, enabled = EXCLUDED.enabled, position = EXCLUDED.position,
               endpoint_id = EXCLUDED.endpoint_id, settings = EXCLUDED.settings
             RETURNING id",
        )
        .bind(id)
        .bind(&a.key)
        .bind(a.name.trim())
        .bind(a.kind)
        .bind(a.trigger)
        .bind(&a.transition)
        .bind(a.enabled)
        .bind(i16::try_from(n + 1).unwrap_or(i16::MAX))
        .bind(settings)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| limit_error(e, n))?;
        action_ids.push(row);
    }
    sqlx::query("DELETE FROM cmdb.workflow_action_recipients WHERE action_id = ANY($1)")
        .bind(&action_ids)
        .execute(&mut *tx)
        .await?;
    let mut owners = Vec::new();
    let mut positions = Vec::new();
    let mut sources = Vec::new();
    let (mut profiles, mut groups, mut users) = (Vec::new(), Vec::new(), Vec::new());
    for (action, list) in action_ids.iter().zip(resolved) {
        for (j, r) in list.iter().enumerate() {
            owners.push(*action);
            positions.push(i16::try_from(j + 1).unwrap_or(i16::MAX));
            sources.push(r.source.as_str());
            profiles.push(r.profile);
            groups.push(r.group);
            users.push(r.user);
        }
    }
    sqlx::query(
        "INSERT INTO cmdb.workflow_action_recipients (action_id, position, source, profile_id, group_id, user_id)
         SELECT * FROM unnest($1::uuid[], $2::smallint[], $3::text[], $4::uuid[], $5::uuid[], $6::uuid[])",
    )
    .bind(&owners)
    .bind(&positions)
    .bind(&sources)
    .bind(&profiles)
    .bind(&groups)
    .bind(&users)
    .execute(&mut *tx)
    .await?;

    let new = load(&mut *tx, id).await?;
    let comparable = |list: &[WorkflowAction]| specs(list);
    if comparable(old) == comparable(&new) {
        return Ok(before.version);
    }
    let (user_id, user_name) = service::actor(ctx);
    let version: i32 = sqlx::query_scalar(
        "UPDATE cmdb.workflow_definitions SET version = version + 1, updated_by_id = $2, updated_by_name = $3
         WHERE id = $1 RETURNING version",
    )
    .bind(id)
    .bind(user_id)
    .bind(&user_name)
    .fetch_one(&mut *tx)
    .await?;
    let entry = AuditEntry {
        action: AuditAction::Update,
        entity_type: "workflow_definitions",
        entity_id: id,
        old_value: Some(json!({ "version": before.version, "actions": specs(old) })),
        new_value: Some(json!({ "version": version, "actions": specs(&new) })),
    };
    crud::write_audit(&mut *tx, ctx, vec![entry]).await?;
    Ok(version)
}

/// The per-trigger limit of the database as the API's 400.
fn limit_error(e: sqlx::Error, n: usize) -> AppError {
    if let sqlx::Error::Database(db) = &e
        && db.constraint() == Some("workflow_actions_per_trigger")
    {
        return AppError::validation(vec![body_error(
            format!("actions[{n}]"),
            "too_many_actions",
            format!("At most {MAX_PER_TRIGGER} actions per trigger and transition"),
        )]);
    }
    e.into()
}

// ---------------------------------------------------------------------------
// Preview
// ---------------------------------------------------------------------------

/// `{id}/actions/{key}`: a definition and one of its action keys.
pub struct ActionKeyPath(pub Uuid, pub String);

impl PathInput for ActionKeyPath {
    fn params() -> Vec<Parameter> {
        vec![
            ParameterBuilder::new()
                .name("id")
                .parameter_in(ParameterIn::Path)
                .required(Required::True)
                .schema(Some(api_schemas::uuid_builder()))
                .build(),
            ParameterBuilder::new()
                .name("key")
                .parameter_in(ParameterIn::Path)
                .required(Required::True)
                .description(Some("Action key"))
                .schema(Some(key_schema()))
                .build(),
        ]
    }
    fn parse(raw: &RawPathParams) -> Result<Self, AppError> {
        let get = |name: &str| raw.iter().find(|(k, _)| *k == name).map(|(_, v)| v).unwrap_or_default();
        let bad = |field: &str, message: &str| {
            AppError::validation(vec![FieldError {
                location: FieldLocation::Params,
                field: field.into(),
                message: message.into(),
                code: "invalid_format".into(),
            }])
        };
        let id = Some(get("id"))
            .filter(|v| validate::is_uuid(v))
            .and_then(|v| Uuid::parse_str(v).ok())
            .ok_or_else(|| bad("id", "Invalid UUID"))?;
        let key = get("key");
        let valid = key.len() <= 63
            && key.starts_with(|c: char| c.is_ascii_lowercase())
            && key.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if !valid {
            return Err(bad("key", "Expected an action key (lower_snake_case)"));
        }
        Ok(ActionKeyPath(id, key.to_owned()))
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct WorkflowActionPreviewQuery {
    /// Judge the view right on this CI's type; without it, on the workflow's type
    pub ci_id: Option<Uuid>,
}

/// Whether a user would be notified, and why not
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowActionPreviewReason {
    /// Would be notified
    Included,
    /// May not view the CI's type: gets nothing, and the CI is never named to them
    NoView,
    /// The account is disabled
    Inactive,
    /// Beyond `WORKFLOW_ACTIONS_MAX_RECIPIENTS`
    Truncated,
}

/// One user an action resolves to
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowActionPreviewUser {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
    pub reason: WorkflowActionPreviewReason,
    /// The sources that name them, as `group CAB`
    pub sources: Vec<String>,
}

/// Who an action would notify now
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowActionPreview {
    pub key: String,
    pub kind: WorkflowActionKind,
    /// The CI judged, if one was given
    pub ci_id: Option<Uuid>,
    /// Users who would be notified
    pub included: i64,
    /// Up to 500 users, those included first, then by username
    pub users: Vec<WorkflowActionPreviewUser>,
    /// More users than listed
    pub truncated: bool,
    /// Whoever runs the event is left out as well (`excludeActor`, the default)
    pub excludes_actor: bool,
}

pub async fn preview(
    pool: &PgPool,
    ctx: &RequestContext,
    path: &ActionKeyPath,
    q: &WorkflowActionPreviewQuery,
    max_recipients: usize,
) -> Result<WorkflowActionPreview, AppError> {
    let mut conn = pool.acquire().await?;
    let d = service::load_for(&mut conn, ctx, path.0, false, service::Access::Read).await?;
    let action =
        load(&mut conn, d.id).await?.into_iter().find(|a| a.key == path.1).ok_or_else(|| {
            AppError::new(ErrorCode::NotFound, format!("Workflow {} has no action {}", d.key, path.1))
        })?;
    let class_id = match q.ci_id {
        Some(ci) => {
            let class: Option<Uuid> = sqlx::query_scalar("SELECT class_id FROM cmdb.configuration_items WHERE id = $1")
                .bind(ci)
                .fetch_optional(&mut *conn)
                .await?;
            class.filter(|c| ctx.may_view_all(&[*c])).ok_or_else(|| AppError::missing("Configuration item", ci))?
        }
        None => d.class_id,
    };
    let resolved = outbox::resolve_static(&mut conn, &action.recipients).await?;
    let ids: Vec<Uuid> = resolved.keys().copied().collect();
    let permissions = auth_data::load_permissions_of(&mut conn, &ids).await?;
    let people: Vec<(Uuid, String, String, bool)> =
        sqlx::query_as("SELECT id, username, display_name, is_active FROM cmdb.users WHERE id = ANY($1) ORDER BY id")
            .bind(&ids)
            .fetch_all(&mut *conn)
            .await?;
    let mut users = Vec::with_capacity(people.len());
    let mut included = 0usize;
    for (id, username, display_name, active) in people {
        let reason = if !active {
            WorkflowActionPreviewReason::Inactive
        } else if !permissions.get(&id).is_some_and(|p| p.can(class_id, ClassOp::View)) {
            WorkflowActionPreviewReason::NoView
        } else if included >= max_recipients {
            WorkflowActionPreviewReason::Truncated
        } else {
            included += 1;
            WorkflowActionPreviewReason::Included
        };
        let sources = resolved.get(&id).cloned().unwrap_or_default().into_iter().collect();
        users.push(WorkflowActionPreviewUser { id, username, display_name, reason, sources });
    }
    users.sort_by(|a, b| a.reason.cmp(&b.reason).then_with(|| a.username.cmp(&b.username)));
    let truncated = users.len() > PREVIEW_USERS;
    users.truncate(PREVIEW_USERS);
    Ok(WorkflowActionPreview {
        key: action.key,
        kind: action.kind,
        ci_id: q.ci_id,
        included: i64::try_from(included).unwrap_or(i64::MAX),
        users,
        truncated,
        excludes_actor: action.settings.exclude_actor.unwrap_or(true),
    })
}

/// Sources named by a principal, for messages: `group CAB`.
pub(crate) fn source_label(r: &WorkflowActionRecipient) -> String {
    let name = r.profile.as_ref().or(r.group.as_ref()).or(r.user.as_ref()).map(|p| p.name.as_str()).unwrap_or_default();
    format!("{} {name}", r.source.as_str())
}
