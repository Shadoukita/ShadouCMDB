//! A workflow version's graph: resolving a draft body against the data model,
//! storing it, reading it back in the API form, its canonical checksum, and
//! the publish-time lint (design SHAA-1411 §3.4).
//!
//! The draft body names everything by key (states, fields, lookup values);
//! the tables hold ids, so a version keeps pointing at the same field whatever
//! is renamed later. Conditions are stored with field ids (see
//! [`super::condition`]).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use sqlx::types::Json as SqlJson;
use uuid::Uuid;

use super::approvers;
use super::condition::{self, Scope};
use super::schemas::{
    DueAfter, WorkflowApproval, WorkflowApprovalOverdue, WorkflowApprovalStep, WorkflowDraftReplace, WorkflowProblem,
    WorkflowProblemSeverity, WorkflowSetAttribute, WorkflowState, WorkflowStateCategory, WorkflowTransition,
    WorkflowTransitionField, WorkflowValueFrom, WorkflowVersion, WorkflowVersionStatus,
};
use super::state_field::Driver;
use crate::data::classes::{self as class_data, EffectiveAttributeRow};
use crate::http::error::{AppError, FieldError, FieldLocation};
use crate::modules::classes::AttributeDataType;
use crate::modules::items::plan;
use crate::schema::model::{Field, Model};

// ---------------------------------------------------------------------------
// What a graph may refer to
// ---------------------------------------------------------------------------

/// A lookup list value, as conditions and states see it.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct LookupValue {
    pub id: Uuid,
    pub list_id: Uuid,
    pub key: String,
    pub is_active: bool,
}

/// The fields a workflow on one type may use (the type's own and inherited)
/// and the values of their lookup lists.
pub struct Fields {
    pub class_key: String,
    /// Every field of the model, for naming one that is no longer on the type.
    pub model: Model,
    pub lineage: Vec<Uuid>,
    pub values: Vec<LookupValue>,
    /// The type's fields (own and inherited) with their rules, for the attribute actions.
    pub defs: Vec<EffectiveAttributeRow>,
    /// The built-in Person type: the only type an attribute action may reference.
    pub person_class: Option<Uuid>,
}

impl Fields {
    pub async fn load(conn: &mut PgConnection, class_id: Uuid) -> Result<Fields, AppError> {
        let model = Model::load(conn).await?;
        let lineage: Vec<Uuid> = model.lineage(class_id).iter().map(|c| c.id).collect();
        let class_key = model.class(class_id).map(|c| c.key.clone()).unwrap_or_default();
        let lists: Vec<Uuid> =
            model.fields.iter().filter(|f| lineage.contains(&f.class_id)).filter_map(|f| f.lookup_list_id).collect();
        let values = sqlx::query_as::<_, LookupValue>(
            "SELECT id, list_id, key, is_active FROM cmdb.lookup_list_values WHERE list_id = ANY($1)",
        )
        .bind(&lists)
        .fetch_all(&mut *conn)
        .await?;
        let defs = class_data::effective_attributes(conn, class_id).await?;
        let person_class = sqlx::query_scalar("SELECT id FROM cmdb.ci_classes WHERE system_role = 'person'")
            .fetch_optional(&mut *conn)
            .await?;
        Ok(Fields { class_key, model, lineage, values, defs, person_class })
    }

    /// A field of the type with its rules, by id.
    pub fn def(&self, id: Uuid) -> Option<&EffectiveAttributeRow> {
        self.defs.iter().find(|d| d.id == id)
    }

    /// A field of the type (own or inherited) by key.
    pub fn by_key(&self, key: &str) -> Option<&Field> {
        self.model.fields.iter().find(|f| f.key == key && self.lineage.contains(&f.class_id))
    }

    pub fn on_type(&self, f: &Field) -> bool {
        self.lineage.contains(&f.class_id)
    }

    pub fn value(&self, list: Uuid, key: &str) -> Option<&LookupValue> {
        self.values.iter().find(|v| v.list_id == list && v.key == key)
    }
}

/// Resolves the fields of a condition by key (the API form).
struct ByKey<'a>(&'a Fields);

impl Scope for ByKey<'_> {
    fn field(&self, name: &str) -> Result<&Field, String> {
        self.0.by_key(name).ok_or_else(|| format!("Type {} has no field {name} (own or inherited)", self.0.class_key))
    }
    fn lookup_value(&self, list: Uuid, key: &str) -> bool {
        self.0.value(list, key).is_some()
    }
}

/// Resolves the fields of a stored condition by id.
struct ById<'a>(&'a Fields);

impl Scope for ById<'_> {
    fn field(&self, name: &str) -> Result<&Field, String> {
        let field = name.parse::<Uuid>().ok().and_then(|id| self.0.model.field(id));
        match field {
            Some(f) if self.0.on_type(f) => Ok(f),
            Some(f) => Err(format!("Field {} is no longer a field of type {}", f.key, self.0.class_key)),
            None => Err("The field no longer exists".into()),
        }
    }
    fn lookup_value(&self, list: Uuid, key: &str) -> bool {
        self.0.value(list, key).is_some()
    }
}

fn body_error(field: String, message: impl Into<String>, code: &str) -> FieldError {
    FieldError { location: FieldLocation::Body, field, message: message.into(), code: code.into() }
}

// ---------------------------------------------------------------------------
// Storing a draft
// ---------------------------------------------------------------------------

struct ResolvedTransition {
    fields: Vec<(Uuid, bool)>,
    conditions: Option<Value>,
    /// Attribute actions: field, `value_from` column, literal.
    set_attributes: Vec<(Uuid, &'static str, Option<Value>)>,
}

/// A draft body with its field keys, lookup value keys and conditions resolved to ids.
struct Resolved {
    state_values: Vec<Option<Uuid>>,
    transitions: Vec<ResolvedTransition>,
}

/// Resolves the keys of `body` (already checked for shape, duplicate keys and
/// dangling state keys): what cannot be resolved is a 400.
fn resolve(fields: &Fields, state_attribute: Option<Uuid>, body: &WorkflowDraftReplace) -> Result<Resolved, AppError> {
    let mut errors = Vec::new();
    let state_list = state_attribute.and_then(|id| fields.model.field(id)).and_then(|f| f.lookup_list_id);
    let mut state_values: Vec<Option<Uuid>> = Vec::with_capacity(body.states.len());
    for (i, s) in body.states.iter().enumerate() {
        let path = format!("states[{i}].stateValue");
        state_values.push(match (&s.state_value, state_list) {
            (None, _) => None,
            (Some(_), None) => {
                errors.push(body_error(
                    path,
                    "The workflow has no state field (stateAttributeId); set one before mapping states to its values",
                    "no_state_attribute",
                ));
                None
            }
            (Some(key), Some(list)) => match fields.value(list, key) {
                Some(v) => Some(v.id),
                None => {
                    errors.push(body_error(
                        path,
                        format!("No value \"{key}\" in the state field's list"),
                        "unknown_value",
                    ));
                    None
                }
            },
        });
    }
    let mut resolved = Vec::with_capacity(body.transitions.len());
    for (i, t) in body.transitions.iter().enumerate() {
        let mut ids = Vec::with_capacity(t.fields.len());
        for (j, f) in t.fields.iter().enumerate() {
            match fields.by_key(&f.attribute) {
                Some(field) => ids.push((field.id, f.required)),
                None => errors.push(body_error(
                    format!("transitions[{i}].fields[{j}].attribute"),
                    format!("Type {} has no field {} (own or inherited)", fields.class_key, f.attribute),
                    "unknown_attribute",
                )),
            }
        }
        let conditions = match &t.conditions {
            None => None,
            Some(c) => match condition::parse(c, &format!("transitions[{i}].conditions"), &ByKey(fields)) {
                Ok(parsed) => Some(parsed.to_json(&|id| id.to_string())),
                Err(e) => {
                    errors.extend(e);
                    None
                }
            },
        };
        let mut set_attributes = Vec::with_capacity(t.set_attributes.len());
        for (j, a) in t.set_attributes.iter().enumerate() {
            let path = format!("transitions[{i}].setAttributes[{j}]");
            let from = match (&a.value, a.value_from) {
                (Some(_), None) => "literal",
                (None, Some(from)) => from.as_str(),
                _ => {
                    errors.push(body_error(
                        path.clone(),
                        "Give either value or valueFrom (now, today, actor or clear)",
                        "value_or_value_from",
                    ));
                    continue;
                }
            };
            match fields.by_key(&a.attribute) {
                Some(field) if set_attributes.iter().any(|(id, _, _)| *id == field.id) => errors.push(body_error(
                    format!("{path}.attribute"),
                    format!("Transition {} sets field {} twice", t.key, a.attribute),
                    "duplicate",
                )),
                Some(field) => set_attributes.push((field.id, from, a.value.clone())),
                None => errors.push(body_error(
                    format!("{path}.attribute"),
                    format!("Type {} has no field {} (own or inherited)", fields.class_key, a.attribute),
                    "unknown_attribute",
                )),
            }
        }
        resolved.push(ResolvedTransition { fields: ids, conditions, set_attributes });
    }
    if let Some(layout) = &body.layout
        && serde_json::to_vec(layout).map_or(usize::MAX, |b| b.len()) > 60 * 1024
    {
        errors.push(body_error("layout".into(), "The layout is at most 60 KiB", "too_large"));
    }
    if !errors.is_empty() {
        return Err(AppError::validation(errors));
    }
    Ok(Resolved { state_values, transitions: resolved })
}

/// The checksum `body` would have once stored as a draft, without storing it
/// (a configuration import compares it with the current published version).
pub fn draft_checksum(
    fields: &Fields,
    state_attribute: Option<Uuid>,
    body: &WorkflowDraftReplace,
) -> Result<String, AppError> {
    let r = resolve(fields, state_attribute, body)?;
    let key = |id: Uuid| fields.model.field(id).map(|f| f.key.clone()).unwrap_or_else(|| id.to_string());
    let states: Vec<WorkflowState> = body
        .states
        .iter()
        .zip(&r.state_values)
        .map(|(s, v)| WorkflowState {
            state_value: v.and_then(|id| fields.values.iter().find(|x| x.id == id)).map(|x| x.key.clone()),
            ..s.clone()
        })
        .collect();
    let transitions: Vec<WorkflowTransition> = body
        .transitions
        .iter()
        .zip(&r.transitions)
        .map(|(t, r)| WorkflowTransition {
            fields: r
                .fields
                .iter()
                .map(|(id, required)| WorkflowTransitionField { attribute: key(*id), required: *required })
                .collect(),
            conditions: r.conditions.as_ref().map(|c| {
                condition::rename_fields(c, &|s| {
                    s.parse::<Uuid>().ok().and_then(|id| fields.model.field(id)).map(|f| f.key.clone())
                })
            }),
            ..t.clone()
        })
        .collect();
    Ok(checksum(&body.initial_state, &states, &transitions))
}

/// Replaces the graph of draft `version_id` with `body` (already checked for
/// shape, duplicate keys and dangling state keys). Field keys, lookup value
/// keys and conditions are resolved here: what cannot be resolved is a 400.
pub async fn store_draft(
    conn: &mut PgConnection,
    version_id: Uuid,
    fields: &Fields,
    state_attribute: Option<Uuid>,
    body: &WorkflowDraftReplace,
) -> Result<(), AppError> {
    let Resolved { state_values, transitions: resolved } = resolve(fields, state_attribute, body)?;

    // The initial state goes first: the states it points at are about to go.
    sqlx::query("UPDATE cmdb.workflow_versions SET initial_state_id = NULL, layout = $2 WHERE id = $1")
        .bind(version_id)
        .bind(body.layout.clone().map(SqlJson))
        .execute(&mut *conn)
        .await?;
    // Transitions and their fields go with their states.
    sqlx::query("DELETE FROM cmdb.workflow_states WHERE version_id = $1").bind(version_id).execute(&mut *conn).await?;

    let keys: Vec<&str> = body.states.iter().map(|s| s.key.as_str()).collect();
    let names: Vec<&str> = body.states.iter().map(|s| s.name.as_str()).collect();
    let categories: Vec<&str> = body.states.iter().map(|s| s.category.as_str()).collect();
    let terminal: Vec<bool> = body.states.iter().map(|s| s.terminal).collect();
    let state_ids: Vec<(Uuid, String)> = sqlx::query_as(
        "INSERT INTO cmdb.workflow_states (version_id, key, name, category, is_terminal, state_value_id, sort_order)
         SELECT $1, u.key, u.name, u.category, u.terminal, u.value, (u.n - 1)::int
         FROM unnest($2::text[], $3::text[], $4::text[], $5::bool[], $6::uuid[]) WITH ORDINALITY
              AS u(key, name, category, terminal, value, n)
         RETURNING id, key",
    )
    .bind(version_id)
    .bind(&keys)
    .bind(&names)
    .bind(&categories)
    .bind(&terminal)
    .bind(&state_values)
    .fetch_all(&mut *conn)
    .await?;
    let state_id: HashMap<&str, Uuid> = state_ids.iter().map(|(id, k)| (k.as_str(), *id)).collect();
    let initial = body.initial_state.as_deref().and_then(|k| state_id.get(k).copied());
    sqlx::query("UPDATE cmdb.workflow_versions SET initial_state_id = $2 WHERE id = $1")
        .bind(version_id)
        .bind(initial)
        .execute(&mut *conn)
        .await?;

    let t = &body.transitions;
    let keys: Vec<&str> = t.iter().map(|t| t.key.as_str()).collect();
    let names: Vec<&str> = t.iter().map(|t| t.name.as_str()).collect();
    let from: Vec<Uuid> = t.iter().map(|t| state_id[t.from.as_str()]).collect();
    let to: Vec<Uuid> = t.iter().map(|t| state_id[t.to.as_str()]).collect();
    let comment: Vec<bool> = t.iter().map(|t| t.requires_comment).collect();
    let conditions: Vec<Option<Value>> = resolved.iter().map(|r| r.conditions.clone()).collect();
    let transition_ids: Vec<(Uuid, String)> = sqlx::query_as(
        "INSERT INTO cmdb.workflow_transitions
           (version_id, key, name, from_state_id, to_state_id, requires_comment, conditions, sort_order)
         SELECT $1, u.key, u.name, u.from_id, u.to_id, u.comment, u.conditions, (u.n - 1)::int
         FROM unnest($2::text[], $3::text[], $4::uuid[], $5::uuid[], $6::bool[], $7::jsonb[]) WITH ORDINALITY
              AS u(key, name, from_id, to_id, comment, conditions, n)
         RETURNING id, key",
    )
    .bind(version_id)
    .bind(&keys)
    .bind(&names)
    .bind(&from)
    .bind(&to)
    .bind(&comment)
    .bind(&conditions)
    .fetch_all(&mut *conn)
    .await?;
    let transition_id: HashMap<&str, Uuid> = transition_ids.iter().map(|(id, k)| (k.as_str(), *id)).collect();

    let (mut tids, mut aids, mut required, mut order) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (t, r) in body.transitions.iter().zip(&resolved) {
        for (n, (attribute, is_required)) in r.fields.iter().enumerate() {
            tids.push(transition_id[t.key.as_str()]);
            aids.push(*attribute);
            required.push(*is_required);
            order.push(n as i32);
        }
    }
    sqlx::query(
        "INSERT INTO cmdb.workflow_transition_fields (transition_id, attribute_id, is_required, sort_order)
         SELECT * FROM unnest($1::uuid[], $2::uuid[], $3::bool[], $4::int[])",
    )
    .bind(&tids)
    .bind(&aids)
    .bind(&required)
    .bind(&order)
    .execute(&mut *conn)
    .await?;
    let (mut tids, mut positions, mut aids, mut from, mut values) =
        (Vec::new(), Vec::<i16>::new(), Vec::new(), Vec::new(), Vec::<Option<Value>>::new());
    for (t, r) in body.transitions.iter().zip(&resolved) {
        for (n, (attribute, value_from, value)) in r.set_attributes.iter().enumerate() {
            tids.push(transition_id[t.key.as_str()]);
            positions.push(n as i16 + 1);
            aids.push(*attribute);
            from.push(*value_from);
            values.push(value.clone());
        }
    }
    if !tids.is_empty() {
        sqlx::query(
            "INSERT INTO cmdb.workflow_transition_set_attributes (transition_id, position, attribute_id, value_from, value)
             SELECT * FROM unnest($1::uuid[], $2::smallint[], $3::uuid[], $4::text[], $5::jsonb[])",
        )
        .bind(&tids)
        .bind(&positions)
        .bind(&aids)
        .bind(&from)
        .bind(&values)
        .execute(&mut *conn)
        .await?;
    }
    store_steps(conn, &body.transitions, &transition_id).await
}

/// The approval steps of the draft's transitions, numbered 1..n in their order.
async fn store_steps(
    conn: &mut PgConnection,
    transitions: &[WorkflowTransition],
    transition_id: &HashMap<&str, Uuid>,
) -> Result<(), AppError> {
    let mut tids = Vec::new();
    let mut nos: Vec<i16> = Vec::new();
    let (mut keys, mut names, mut on_overdue) = (Vec::new(), Vec::new(), Vec::new());
    let (mut required, mut due, mut distinct, mut exclude, mut tokens): (Vec<i16>, Vec<Option<i32>>, _, _, _) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for t in transitions {
        for (n, s) in t.approval.iter().flat_map(|a| a.steps.iter()).enumerate() {
            tids.push(transition_id[t.key.as_str()]);
            nos.push(n as i16 + 1);
            keys.push(s.key.as_str());
            names.push(s.name.as_str());
            required.push(s.required_approvals);
            due.push(s.due_after.map(DueAfter::minutes));
            on_overdue.push(match s.on_overdue {
                WorkflowApprovalOverdue::Flag => "flag",
                WorkflowApprovalOverdue::Reject => "reject",
            });
            distinct.push(s.distinct_from_earlier);
            exclude.push(json!(s.exclude_actors_of));
            tokens.push(s.allow_api_tokens);
        }
    }
    if tids.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO cmdb.workflow_transition_approval_steps
           (transition_id, step_no, key, name, required_approvals, due_after, on_overdue, distinct_from_earlier,
            exclude_actors_of, allow_api_tokens)
         SELECT u.t, u.n, u.key, u.name, u.required, make_interval(mins => u.due), u.overdue, u.distinct_,
                ARRAY(SELECT jsonb_array_elements_text(u.exclude)), u.tokens
         FROM unnest($1::uuid[], $2::smallint[], $3::text[], $4::text[], $5::smallint[], $6::int[], $7::text[],
                     $8::bool[], $9::jsonb[], $10::bool[])
              AS u(t, n, key, name, required, due, overdue, distinct_, exclude, tokens)",
    )
    .bind(&tids)
    .bind(&nos)
    .bind(&keys)
    .bind(&names)
    .bind(&required)
    .bind(&due)
    .bind(&on_overdue)
    .bind(&distinct)
    .bind(&exclude)
    .bind(&tokens)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Reading a version
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct VersionRow {
    pub id: Uuid,
    pub version_no: i32,
    pub status: WorkflowVersionStatus,
    pub initial_state_id: Option<Uuid>,
    pub layout: Option<SqlJson<Value>>,
    pub change_note: Option<String>,
    pub checksum: Option<Vec<u8>>,
    pub published_at: Option<DateTime<Utc>>,
    pub published_by_name: Option<String>,
    pub created_at: DateTime<Utc>,
}

pub const VERSION_COLUMNS: &str = "id, version_no, status, initial_state_id, layout, change_note, checksum, \
     published_at, published_by_name, created_at";

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct StateRow {
    pub id: Uuid,
    pub key: String,
    pub name: String,
    pub category: WorkflowStateCategory,
    pub is_terminal: bool,
    pub value_key: Option<String>,
    pub value_list_id: Option<Uuid>,
    pub value_active: Option<bool>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TransitionRow {
    pub id: Uuid,
    pub key: String,
    pub name: String,
    pub from_state_id: Uuid,
    pub to_state_id: Uuid,
    pub requires_comment: bool,
    pub conditions: Option<SqlJson<Value>>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct FieldRow {
    pub transition_id: Uuid,
    pub attribute_id: Uuid,
    pub is_required: bool,
}

/// An attribute action of a transition.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SetAttributeRow {
    pub transition_id: Uuid,
    pub attribute_id: Uuid,
    pub value_from: String,
    pub value: Option<SqlJson<Value>>,
}

impl SetAttributeRow {
    /// The API form, the field named by key.
    pub fn render(&self, model: &Model) -> WorkflowSetAttribute {
        WorkflowSetAttribute {
            attribute: Stored::attribute_key(model, self.attribute_id),
            value: self.value.as_ref().map(|v| v.0.clone()),
            value_from: WorkflowValueFrom::parse(&self.value_from),
        }
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct StepRow {
    pub transition_id: Uuid,
    pub key: String,
    pub name: String,
    pub required_approvals: i16,
    pub due_minutes: Option<i32>,
    pub on_overdue: WorkflowApprovalOverdue,
    pub distinct_from_earlier: bool,
    pub exclude_actors_of: Vec<String>,
    pub allow_api_tokens: bool,
}

/// A version as stored.
pub struct Stored {
    pub version: VersionRow,
    pub states: Vec<StateRow>,
    pub transitions: Vec<TransitionRow>,
    pub fields: Vec<FieldRow>,
    /// Approval steps, in step order per transition.
    pub steps: Vec<StepRow>,
    /// Attribute actions, in their order per transition.
    pub set_attributes: Vec<SetAttributeRow>,
}

pub async fn load(conn: &mut PgConnection, version: VersionRow) -> Result<Stored, AppError> {
    let states = sqlx::query_as::<_, StateRow>(
        "SELECT s.id, s.key, s.name, s.category, s.is_terminal,
                v.key AS value_key, v.list_id AS value_list_id, v.is_active AS value_active
         FROM cmdb.workflow_states s LEFT JOIN cmdb.lookup_list_values v ON v.id = s.state_value_id
         WHERE s.version_id = $1 ORDER BY s.sort_order, s.key",
    )
    .bind(version.id)
    .fetch_all(&mut *conn)
    .await?;
    let transitions = sqlx::query_as::<_, TransitionRow>(
        "SELECT id, key, name, from_state_id, to_state_id, requires_comment, conditions
         FROM cmdb.workflow_transitions WHERE version_id = $1 ORDER BY sort_order, key",
    )
    .bind(version.id)
    .fetch_all(&mut *conn)
    .await?;
    let fields = sqlx::query_as::<_, FieldRow>(
        "SELECT f.transition_id, f.attribute_id, f.is_required
         FROM cmdb.workflow_transition_fields f JOIN cmdb.workflow_transitions t ON t.id = f.transition_id
         WHERE t.version_id = $1 ORDER BY f.sort_order, f.attribute_id",
    )
    .bind(version.id)
    .fetch_all(&mut *conn)
    .await?;
    let steps = sqlx::query_as::<_, StepRow>(
        "SELECT s.transition_id, s.key, s.name, s.required_approvals,
                (extract(epoch FROM s.due_after) / 60)::int AS due_minutes, s.on_overdue, s.distinct_from_earlier,
                s.exclude_actors_of, s.allow_api_tokens
         FROM cmdb.workflow_transition_approval_steps s JOIN cmdb.workflow_transitions t ON t.id = s.transition_id
         WHERE t.version_id = $1 ORDER BY s.transition_id, s.step_no",
    )
    .bind(version.id)
    .fetch_all(&mut *conn)
    .await?;
    let set_attributes = load_set_attributes(conn, version.id).await?;
    Ok(Stored { version, states, transitions, fields, steps, set_attributes })
}

/// The attribute actions of version `version_id`, in their order per transition.
pub async fn load_set_attributes(conn: &mut PgConnection, version_id: Uuid) -> Result<Vec<SetAttributeRow>, AppError> {
    Ok(sqlx::query_as::<_, SetAttributeRow>(
        "SELECT a.transition_id, a.attribute_id, a.value_from, a.value
         FROM cmdb.workflow_transition_set_attributes a JOIN cmdb.workflow_transitions t ON t.id = a.transition_id
         WHERE t.version_id = $1 ORDER BY a.transition_id, a.position",
    )
    .bind(version_id)
    .fetch_all(&mut *conn)
    .await?)
}

impl Stored {
    fn state_key(&self, id: Uuid) -> String {
        self.states.iter().find(|s| s.id == id).map(|s| s.key.clone()).unwrap_or_default()
    }

    /// The approval steps of transition `id`, in order.
    pub fn steps_of(&self, id: Uuid) -> impl Iterator<Item = &StepRow> {
        self.steps.iter().filter(move |s| s.transition_id == id)
    }

    /// The attribute actions of transition `id`, in order.
    pub fn set_attributes_of(&self, id: Uuid) -> impl Iterator<Item = &SetAttributeRow> {
        self.set_attributes.iter().filter(move |s| s.transition_id == id)
    }

    fn approval(&self, id: Uuid) -> Option<WorkflowApproval> {
        let steps: Vec<WorkflowApprovalStep> = self
            .steps_of(id)
            .map(|s| WorkflowApprovalStep {
                key: s.key.clone(),
                name: s.name.clone(),
                required_approvals: s.required_approvals,
                due_after: s.due_minutes.map(DueAfter),
                on_overdue: s.on_overdue,
                distinct_from_earlier: s.distinct_from_earlier,
                exclude_actors_of: s.exclude_actors_of.clone(),
                allow_api_tokens: s.allow_api_tokens,
            })
            .collect();
        (!steps.is_empty()).then_some(WorkflowApproval { steps })
    }

    fn attribute_key(model: &Model, id: Uuid) -> String {
        model.field(id).map(|f| f.key.clone()).unwrap_or_else(|| id.to_string())
    }

    /// The graph in the API form (keys), as the checksum covers it.
    pub fn graph(&self, model: &Model) -> (Option<String>, Vec<WorkflowState>, Vec<WorkflowTransition>) {
        let initial = self.version.initial_state_id.map(|id| self.state_key(id));
        let states = self
            .states
            .iter()
            .map(|s| WorkflowState {
                key: s.key.clone(),
                name: s.name.clone(),
                category: s.category,
                terminal: s.is_terminal,
                state_value: s.value_key.clone(),
            })
            .collect();
        let transitions = self
            .transitions
            .iter()
            .map(|t| WorkflowTransition {
                key: t.key.clone(),
                name: t.name.clone(),
                from: self.state_key(t.from_state_id),
                to: self.state_key(t.to_state_id),
                requires_comment: t.requires_comment,
                fields: self
                    .fields
                    .iter()
                    .filter(|f| f.transition_id == t.id)
                    .map(|f| WorkflowTransitionField {
                        attribute: Self::attribute_key(model, f.attribute_id),
                        required: f.is_required,
                    })
                    .collect(),
                conditions: t.conditions.as_ref().map(|c| {
                    condition::rename_fields(&c.0, &|s| {
                        s.parse::<Uuid>().ok().and_then(|id| model.field(id)).map(|f| f.key.clone())
                    })
                }),
                approval: self.approval(t.id),
                set_attributes: self.set_attributes_of(t.id).map(|a| a.render(model)).collect(),
            })
            .collect();
        (initial, states, transitions)
    }

    pub fn checksum(&self, model: &Model) -> String {
        let (initial, states, transitions) = self.graph(model);
        checksum(&initial, &states, &transitions)
    }

    pub fn render(&self, model: &Model, is_current: bool) -> WorkflowVersion {
        let (initial_state, states, transitions) = self.graph(model);
        let v = &self.version;
        WorkflowVersion {
            version_no: v.version_no,
            status: v.status,
            is_current,
            initial_state,
            states,
            transitions,
            layout: v.layout.as_ref().map(|l| l.0.clone()),
            checksum: v.checksum.as_ref().map(hex::encode),
            change_note: v.change_note.clone(),
            published_at: v.published_at,
            published_by_name: v.published_by_name.clone(),
            created_at: v.created_at,
        }
    }

    /// Every field the version depends on: transition fields, conditions and
    /// attribute actions (`workflow_version_attribute_refs`); the definition
    /// adds its state field.
    pub fn attributes(&self) -> Vec<Uuid> {
        let mut out: Vec<Uuid> = self.fields.iter().map(|f| f.attribute_id).collect();
        out.extend(self.set_attributes.iter().map(|a| a.attribute_id));
        for t in &self.transitions {
            if let Some(c) = &t.conditions {
                collect_fields(&c.0, &mut out);
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

fn collect_fields(v: &Value, out: &mut Vec<Uuid>) {
    match v {
        Value::Object(o) => {
            if let Some(id) = o.get("field").and_then(Value::as_str).and_then(|s| s.parse().ok()) {
                out.push(id);
            }
            o.iter().filter(|(k, _)| *k != "value").for_each(|(_, v)| collect_fields(v, out));
        }
        Value::Array(a) => a.iter().for_each(|v| collect_fields(v, out)),
        _ => {}
    }
}

/// JSON with every object's keys sorted, so equal graphs serialise equally.
fn canonical(v: Value) -> Value {
    match v {
        Value::Object(o) => {
            let mut entries: Vec<(String, Value)> = o.into_iter().map(|(k, v)| (k, canonical(v))).collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(entries.into_iter().collect::<Map<String, Value>>())
        }
        Value::Array(a) => Value::Array(a.into_iter().map(canonical).collect()),
        other => other,
    }
}

/// sha256 (hex) of the canonical graph: initial state, states and
/// transitions in their order, by key. The layout is not part of it.
pub fn checksum(initial: &Option<String>, states: &[WorkflowState], transitions: &[WorkflowTransition]) -> String {
    let graph = canonical(json!({ "initialState": initial, "states": states, "transitions": transitions }));
    hex::encode(Sha256::digest(serde_json::to_vec(&graph).unwrap_or_default()))
}

// ---------------------------------------------------------------------------
// Lint
// ---------------------------------------------------------------------------

/// What the lint needs to know about the definition.
pub struct LintContext<'a> {
    pub fields: &'a Fields,
    pub state_attribute: Option<Uuid>,
    /// Transition keys granted to at least one profile.
    pub granted: &'a HashSet<String>,
    /// The approver assignments the approval steps are linted against.
    pub approvers: &'a approvers::Facts,
    /// The other active workflows driving a state field on CIs this one may
    /// cover ([`StateFields::overlapping`](super::state_field::StateFields::overlapping)).
    pub other_drivers: &'a [Driver],
}

fn error(path: impl Into<String>, code: &str, message: impl Into<String>) -> WorkflowProblem {
    WorkflowProblem::new(path, WorkflowProblemSeverity::Error, code, message)
}

/// The problems that refuse publishing (errors) or are worth knowing (warnings).
pub fn lint(g: &Stored, cx: &LintContext<'_>) -> Vec<WorkflowProblem> {
    let mut out = Vec::new();
    let index: HashMap<Uuid, usize> = g.states.iter().enumerate().map(|(i, s)| (s.id, i)).collect();
    let n = g.states.len();
    if n == 0 {
        out.push(error("states", "no_states", "A workflow needs at least one state"));
    }

    // Graph shape.
    let mut outgoing = vec![Vec::new(); n];
    let mut incoming = vec![Vec::new(); n];
    for t in &g.transitions {
        if let (Some(&a), Some(&b)) = (index.get(&t.from_state_id), index.get(&t.to_state_id)) {
            outgoing[a].push(b);
            incoming[b].push(a);
        }
    }
    let initial = g.version.initial_state_id.and_then(|id| index.get(&id).copied());
    match initial {
        None if n > 0 => out.push(error("initialState", "no_initial_state", "Choose the state an instance starts in")),
        Some(i) if g.states[i].is_terminal => out.push(
            error(
                "initialState",
                "initial_state_terminal",
                format!("The initial state {} is terminal: an instance would end as it starts", g.states[i].key),
            )
            .with("state", g.states[i].key.as_str()),
        ),
        _ => {}
    }
    let reach = |starts: Vec<usize>, edges: &Vec<Vec<usize>>| {
        let mut seen = vec![false; n];
        let mut queue: VecDeque<usize> = starts.into_iter().collect();
        while let Some(i) = queue.pop_front() {
            if std::mem::replace(&mut seen[i], true) {
                continue;
            }
            queue.extend(edges[i].iter().copied().filter(|j| !seen[*j]));
        }
        seen
    };
    let reachable = initial.map(|i| reach(vec![i], &outgoing));
    let terminals: Vec<usize> = (0..n).filter(|i| g.states[*i].is_terminal).collect();
    if n > 0 && terminals.is_empty() {
        out.push(error(
            "states",
            "no_terminal_state",
            "Mark at least one state terminal: instances must be able to end",
        ));
    }
    let finishes = reach(terminals.clone(), &incoming);
    for (i, s) in g.states.iter().enumerate() {
        let path = format!("states[{i}]");
        if reachable.as_ref().is_some_and(|r| !r[i]) {
            out.push(
                error(&path, "unreachable_state", format!("State {} cannot be reached from the initial state", s.key))
                    .with("state", s.key.as_str()),
            );
        }
        if s.is_terminal && !outgoing[i].is_empty() {
            out.push(
                error(
                    &path,
                    "terminal_has_transitions",
                    format!("State {} is terminal but has outgoing transitions", s.key),
                )
                .with("state", s.key.as_str()),
            );
        } else if !s.is_terminal && outgoing[i].is_empty() {
            out.push(
                error(&path, "dead_end", format!("State {} is not terminal and has no outgoing transition", s.key))
                    .with("state", s.key.as_str()),
            );
        } else if !s.is_terminal && !terminals.is_empty() && !finishes[i] {
            out.push(
                error(&path, "no_terminal_reachable", format!("No terminal state can be reached from state {}", s.key))
                    .with("state", s.key.as_str()),
            );
        }
    }

    // The state field and the values states map to.
    let fields = cx.fields;
    let state_field = cx.state_attribute.and_then(|id| fields.model.field(id));
    if let Some(id) = cx.state_attribute {
        match state_field {
            Some(f) if !fields.on_type(f) => out.push(
                error(
                    "stateAttributeId",
                    "unknown_attribute",
                    format!("The state field {} is not a field of type {}", f.key, fields.class_key),
                )
                .with("attribute", f.key.as_str())
                .with("class", fields.class_key.as_str()),
            ),
            Some(f) if f.data_type != AttributeDataType::Lookup => out.push(
                error(
                    "stateAttributeId",
                    "attribute_type",
                    format!("The state field {} is a {} field, not a lookup field", f.key, f.data_type.as_str()),
                )
                .with("attribute", f.key.as_str())
                .with("dataType", f.data_type.as_str()),
            ),
            Some(f) if !f.is_active => out.push(
                error("stateAttributeId", "inactive_attribute", format!("The state field {} is archived", f.key))
                    .with("attribute", f.key.as_str()),
            ),
            Some(_) => {}
            None => out.push(error("stateAttributeId", "unknown_attribute", format!("Field {id} no longer exists"))),
        }
    }
    let state_list = state_field.and_then(|f| f.lookup_list_id);
    for (i, s) in g.states.iter().enumerate() {
        let Some(key) = &s.value_key else { continue };
        let path = format!("states[{i}].stateValue");
        if cx.state_attribute.is_none() {
            out.push(
                error(
                    path,
                    "no_state_attribute",
                    format!("State {} maps to value {key}, but the workflow has no state field", s.key),
                )
                .with("state", s.key.as_str())
                .with("value", key.as_str()),
            );
        } else if s.value_list_id != state_list {
            out.push(
                error(path, "state_value_list", format!("{key} is not a value of the state field's list"))
                    .with("value", key.as_str()),
            );
        } else if s.value_active == Some(false) {
            out.push(
                error(path, "state_value_inactive", format!("The value {key} is retired")).with("value", key.as_str()),
            );
        }
    }

    // Transition fields and conditions.
    for (i, t) in g.transitions.iter().enumerate() {
        for (j, f) in g.fields.iter().filter(|f| f.transition_id == t.id).enumerate() {
            let path = format!("transitions[{i}].fields[{j}].attribute");
            match fields.model.field(f.attribute_id) {
                Some(a) if !fields.on_type(a) => out.push(
                    error(path, "unknown_attribute", format!("{} is not a field of type {}", a.key, fields.class_key))
                        .with("attribute", a.key.as_str())
                        .with("class", fields.class_key.as_str()),
                ),
                Some(a) if !a.is_active => out.push(
                    error(path, "inactive_attribute", format!("Field {} is archived", a.key))
                        .with("attribute", a.key.as_str()),
                ),
                // A workflow's state field moves only with its states: taking it
                // as a transition field would let the actor set any value (GH#668).
                Some(a) if cx.state_attribute == Some(a.id) => out.push(
                    error(
                        path,
                        "state_field",
                        format!("{} is this workflow's state field: its states set it, not transition fields", a.key),
                    )
                    .with("attribute", a.key.as_str()),
                ),
                Some(a) => {
                    if let Some(d) = cx.other_drivers.iter().find(|d| d.attribute_id == a.id) {
                        out.push(
                            error(
                                path,
                                "state_field",
                                format!("{} is the state field of {}: a transition cannot set it", a.key, d.named()),
                            )
                            .with("attribute", a.key.as_str())
                            .with(d.param().0, d.param().1),
                        );
                    }
                }
                None => out.push(error(path, "unknown_attribute", "The field no longer exists")),
            }
        }
        if let Some(c) = &t.conditions {
            let path = format!("transitions[{i}].conditions");
            match condition::parse(&c.0, &path, &ById(fields)) {
                Ok(parsed) => {
                    let mut used = Vec::new();
                    parsed.attributes(&mut used);
                    for a in used.iter().filter_map(|id| fields.model.field(*id)).filter(|a| !a.is_active) {
                        out.push(
                            error(&path, "inactive_attribute", format!("Field {} is archived", a.key))
                                .with("attribute", a.key.as_str()),
                        );
                    }
                }
                Err(problems) => out.extend(problems.into_iter().map(|p| {
                    let params = condition_params(&c.0, &path, &p.field, fields);
                    WorkflowProblem { params, ..error(p.field, &p.code, p.message) }
                })),
            }
        }
        out.extend(lint_set_attributes(g, t, i, cx));
        if !cx.granted.contains(&t.key) {
            out.push(
                WorkflowProblem::new(
                    format!("transitions[{i}]"),
                    WorkflowProblemSeverity::Warning,
                    "ungranted_transition",
                    format!("No profile is granted transition {}: only administrators could run it", t.key),
                )
                .with("transition", t.key.as_str()),
            );
        }
        for (j, s) in g.steps_of(t.id).enumerate() {
            for (k, key) in s.exclude_actors_of.iter().enumerate() {
                if !g.transitions.iter().any(|x| &x.key == key) {
                    out.push(
                        error(
                            format!("transitions[{i}].approval.steps[{j}].excludeActorsOf[{k}]"),
                            "unknown_transition",
                            format!(
                                "Step {} excludes the actors of transition {key}, which this version does not have",
                                s.key
                            ),
                        )
                        .with("step", s.key.as_str())
                        .with("excluded", key.as_str()),
                    );
                }
            }
        }
    }
    out.extend(cx.approvers.lint(g, &cx.fields.class_key, &|i, j| format!("transitions[{i}].approval.steps[{j}]")));
    out
}

/// The attribute actions of transition `t` (the `i`th), actions design
/// SHAA-2725 §3.4. Errors, never warnings, for a target that is (a) a
/// workflow-managed state field, (b) an identifying field, (c) a field of the
/// same transition, (d) a reference to another type than Person, (e) a
/// read-only field (the key fields of the built-in Person type), or (f) not
/// (or no longer) an active field of the type; then the value is checked
/// against the field: a literal as a CI write would check it, `valueFrom`
/// against the field's type.
fn lint_set_attributes(g: &Stored, t: &TransitionRow, i: usize, cx: &LintContext<'_>) -> Vec<WorkflowProblem> {
    let fields = cx.fields;
    let mut out = Vec::new();
    for (j, s) in g.set_attributes_of(t.id).enumerate() {
        let path = format!("transitions[{i}].setAttributes[{j}]");
        let attribute = format!("{path}.attribute");
        let (a, def) = match (fields.model.field(s.attribute_id), fields.def(s.attribute_id)) {
            (Some(a), Some(def)) if fields.on_type(a) => (a, def),
            (Some(a), _) => {
                out.push(
                    error(
                        attribute,
                        "unknown_attribute",
                        format!("{} is not a field of type {}", a.key, fields.class_key),
                    )
                    .with("attribute", a.key.as_str())
                    .with("class", fields.class_key.as_str()),
                );
                continue;
            }
            (None, _) => {
                out.push(error(attribute, "unknown_attribute", "The field no longer exists"));
                continue;
            }
        };
        let refused = if !a.is_active {
            Some(("inactive_attribute", format!("Field {} is archived", a.key), None))
        } else if cx.state_attribute == Some(a.id) {
            Some((
                "workflow_managed_attribute",
                format!("{} is this workflow's state field: its states set it, not attribute actions", a.key),
                None,
            ))
        } else if let Some(d) = cx.other_drivers.iter().find(|d| d.attribute_id == a.id) {
            Some((
                "workflow_managed_attribute",
                format!("{} is the state field of {}: an attribute action cannot set it", a.key, d.named()),
                Some(d.param()),
            ))
        } else if def.is_identifying {
            Some((
                "identifying_attribute",
                format!("{} is an identifying field: only a person may change what identifies a CI", a.key),
                None,
            ))
        } else if g.fields.iter().any(|f| f.transition_id == t.id && f.attribute_id == a.id) {
            Some((
                "transition_field",
                format!(
                    "{} is also a field of transition {}: either the user enters it or the action sets it",
                    a.key, t.key
                ),
                Some(("transition", t.key.clone())),
            ))
        } else if a.system_role.is_some() {
            Some((
                "read_only_attribute",
                format!("{} is a key field of the Person type and cannot be set", a.key),
                None,
            ))
        } else if a.data_type == AttributeDataType::Reference && def.reference_class_id != fields.person_class {
            Some((
                "reference_not_person",
                format!("{} references another type than Person: an attribute action can only name a Person", a.key),
                None,
            ))
        } else {
            None
        };
        if let Some((code, message, extra)) = refused {
            let p = error(attribute, code, message).with("attribute", a.key.as_str());
            out.push(match extra {
                Some((name, value)) => p.with(name, value),
                None => p,
            });
            continue;
        }
        let from = WorkflowValueFrom::parse(&s.value_from);
        let type_name = a.data_type.as_str();
        let wrong_type = |what: &str, expected: &str| {
            error(
                format!("{path}.valueFrom"),
                "value_from_type",
                format!("{} is a {type_name} field: valueFrom {} needs {what}", a.key, s.value_from),
            )
            .with("attribute", a.key.as_str())
            .with("dataType", type_name)
            .with("valueFrom", s.value_from.as_str())
            .with("expected", expected)
        };
        match (from, &s.value) {
            (Some(WorkflowValueFrom::Now), _)
                if !matches!(a.data_type, AttributeDataType::Date | AttributeDataType::Datetime) =>
            {
                out.push(wrong_type("a date or datetime field", "date_or_datetime"))
            }
            (Some(WorkflowValueFrom::Today), _) if a.data_type != AttributeDataType::Date => {
                out.push(wrong_type("a date field", "date"))
            }
            (Some(WorkflowValueFrom::Actor), _) if a.data_type != AttributeDataType::Reference => {
                out.push(wrong_type("a field that references Person", "person_reference"))
            }
            (Some(WorkflowValueFrom::Clear), _) if def.is_required => out.push(
                error(
                    format!("{path}.valueFrom"),
                    "required_attribute",
                    format!("{} is required and cannot be cleared", a.key),
                )
                .with("attribute", a.key.as_str()),
            ),
            (Some(_), _) => {}
            (None, Some(value)) => out.extend(literal(&path, a, def, &value.0, fields)),
            (None, None) => out.push(error(path, "value_or_value_from", "The action has no value")),
        }
    }
    out
}

/// The problems of a literal `value` for field `a`, as a CI write would find them.
fn literal(path: &str, a: &Field, def: &EffectiveAttributeRow, value: &Value, fields: &Fields) -> Vec<WorkflowProblem> {
    let path = format!("{path}.value");
    match a.data_type {
        AttributeDataType::Reference => vec![
            error(
                path,
                "reference_literal",
                format!(
                    "{} is a reference: set it to the actor (valueFrom actor) or clear it; a CI id does not travel \
                 between installs",
                    a.key
                ),
            )
            .with("attribute", a.key.as_str()),
        ],
        AttributeDataType::Lookup => {
            let found = value.as_str().and_then(|key| fields.value(a.lookup_list_id.unwrap_or_default(), key));
            match found {
                Some(v) if v.is_active => Vec::new(),
                Some(v) => vec![
                    error(path, "value_inactive", format!("The value {} is retired", v.key))
                        .with("attribute", a.key.as_str())
                        .with("value", v.key.as_str()),
                ],
                None => vec![
                    error(path, "unknown_value", format!("{value} is not the key of a value of {}'s list", a.key))
                        .with("attribute", a.key.as_str())
                        .with("value", shown(value)),
                ],
            }
        }
        _ => plan::literal_problems(def, value, &path)
            .into_iter()
            .map(|p| literal_params(error(p.field, &p.code, format!("{}: {}", a.key, p.message)), a, def, value))
            .collect(),
    }
}

/// A value as a message shows it: a string as is, anything else as JSON.
fn shown(value: &Value) -> String {
    value.as_str().map_or_else(|| value.to_string(), str::to_owned)
}

/// The params of a problem of literal `value` for field `a`: the field, its
/// type and the value, and the rule the value breaks where the field has one.
fn literal_params(p: WorkflowProblem, a: &Field, def: &EffectiveAttributeRow, value: &Value) -> WorkflowProblem {
    let rule = |name: &str| def.validation.as_ref().and_then(|v| v.0.get(name)).filter(|v| v.is_number()).cloned();
    let limit = match (p.code.as_str(), value.is_string()) {
        ("too_big", true) => rule("maxLength"),
        ("too_big", false) => rule("maximum"),
        ("too_small", true) => rule("minLength"),
        ("too_small", false) => rule("minimum"),
        _ => None,
    };
    let options = (p.code == "invalid_value").then(|| def.enum_values.as_ref().map(|v| v.0.join(", "))).flatten();
    let pattern = (p.code == "invalid_format")
        .then(|| def.validation.as_ref().and_then(|v| v.0.get("pattern")).and_then(Value::as_str))
        .flatten();
    let mut p = p.with("attribute", a.key.as_str()).with("dataType", a.data_type.as_str()).with("value", shown(value));
    if let Some(limit) = limit {
        p = p.with("limit", limit);
    }
    if let Some(options) = options {
        p = p.with("options", options);
    }
    if let Some(pattern) = pattern {
        p = p.with("pattern", pattern);
    }
    p
}

/// The leaf of stored condition `c` (at `base`) that a problem at `at` is
/// about, and the value at `at` when it is (part of) the leaf's value.
fn condition_leaf<'c>(c: &'c Value, base: &str, at: &str) -> Option<(&'c Value, Option<&'c Value>)> {
    let mut node = c;
    let mut leaf = c.get("field").is_some().then_some(c);
    let rest = at.strip_prefix(base)?;
    for part in rest.split(['.', '[']).filter(|s| !s.is_empty()) {
        let next = match part.strip_suffix(']').and_then(|n| n.parse::<usize>().ok()) {
            Some(n) => node.get(n),
            None => node.get(part),
        };
        let Some(next) = next else { break };
        node = next;
        if node.get("field").is_some() {
            leaf = Some(node);
        }
    }
    let value = (rest.contains(".value") && !std::ptr::eq(node, leaf?)).then_some(node);
    Some((leaf?, value))
}

/// The params of a problem at `at` inside the stored condition `c` (at
/// `base`): the field of its leaf (by key, while it exists, with its type and
/// the workflow type's key), the leaf's op, and the value at `at`.
fn condition_params(c: &Value, base: &str, at: &str, fields: &Fields) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    let Some((leaf, value)) = condition_leaf(c, base, at) else { return out };
    let field = leaf.get("field").and_then(Value::as_str).and_then(|id| id.parse::<Uuid>().ok());
    if let Some(f) = field.and_then(|id| fields.model.field(id)) {
        out.insert("attribute".into(), f.key.as_str().into());
        out.insert("dataType".into(), f.data_type.as_str().into());
        out.insert("class".into(), fields.class_key.as_str().into());
    }
    if let Some(op) = leaf.get("op").and_then(Value::as_str) {
        out.insert("op".into(), op.into());
    }
    if let Some(v) = value {
        out.insert("value".into(), shown(v).into());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_condition_problem_finds_its_leaf_and_value() {
        let c = json!({ "all": [
            { "field": "f1", "op": "eq", "value": "x" },
            { "any": [ { "field": "f2", "op": "in", "value": ["a", 2] }, { "field": "f3", "op": "isSet" } ] }
        ] });
        let at = |path: &str| condition_leaf(&c, "t[0].conditions", &format!("t[0].conditions{path}"));
        let (leaf, value) = at(".all[1].any[0].value[1]").unwrap();
        assert_eq!((&leaf["field"], value), (&json!("f2"), Some(&json!(2))));
        let (leaf, value) = at(".all[0].op").unwrap();
        assert_eq!((&leaf["field"], value), (&json!("f1"), None));
        let (leaf, value) = at(".all[1].any[1].field").unwrap();
        assert_eq!((&leaf["field"], value), (&json!("f3"), None));
        let (leaf, value) = at(".all[0].value").unwrap();
        assert_eq!((&leaf["field"], value), (&json!("f1"), Some(&json!("x"))));
        assert!(at(".all[1]").is_none(), "a group is no leaf");
        assert!(condition_leaf(&c, "t[1].conditions", "t[0].conditions.all[0]").is_none());
        assert_eq!(shown(&json!(["a", 2])), r#"["a",2]"#);
    }

    /// A graph with states, fields and conditions but no approval policy.
    fn pre_approvals_graph() -> (Vec<WorkflowState>, Vec<WorkflowTransition>) {
        let g = json!({
            "states": [
                { "key": "planned", "name": "Planned", "category": "open", "terminal": false, "stateValue": "planned" },
                { "key": "approved", "name": "Approved", "category": "active", "terminal": false, "stateValue": null },
                { "key": "done", "name": "In production", "category": "done", "terminal": true, "stateValue": "live" }
            ],
            "transitions": [
                { "key": "approve", "name": "Approve", "from": "planned", "to": "approved", "requiresComment": true,
                  "fields": [ { "attribute": "owner_team", "required": true }, { "attribute": "notes", "required": false } ],
                  "conditions": { "all": [ { "field": "environment", "op": "in", "value": ["prod"] },
                                           { "field": "risk", "op": "lte", "value": 2 } ] } },
                { "key": "go_live", "name": "Go live", "from": "approved", "to": "done", "requiresComment": false, "fields": [] }
            ]
        });
        (
            serde_json::from_value(g["states"].clone()).unwrap(),
            serde_json::from_value(g["transitions"].clone()).unwrap(),
        )
    }

    /// Approvals design §3.1 and acceptance of slice A2: the checksum of a
    /// version without approvals is byte-identical to what the code before
    /// approvals computed (both values were computed on `main` @ a5b3aac,
    /// before the `approval` block existed). No checksum rewrite is needed.
    #[test]
    fn a_graph_without_approvals_keeps_its_pre_approvals_checksum() {
        let (states, transitions) = pre_approvals_graph();
        assert_eq!(
            checksum(&Some("planned".into()), &states, &transitions),
            "bf4fde411e1565c940affe9e828edf589d39b83d7de53121d857941a80adc9c9"
        );
        assert_eq!(checksum(&None, &[], &[]), "33783a6339d9a0615157f90b3ff3f7b4b833dea2e78e1ffbb61a4bb2b56e9839");
    }

    /// Actions design SHAA-2725 §10.1 and acceptance of slice S2: a version
    /// without attribute actions keeps the checksum the code before
    /// `setAttributes` computed (computed on `main` @ e2f3f5c9, with an
    /// approval policy so every optional block before S2 is covered).
    #[test]
    fn a_graph_without_attribute_actions_keeps_its_pre_actions_checksum() {
        let (states, mut transitions) = pre_approvals_graph();
        transitions[0].approval = Some(
            serde_json::from_value(json!({ "steps": [
                { "key": "tech", "name": "Tech", "requiredApprovals": 1 },
                { "key": "cab", "name": "CAB", "requiredApprovals": 2, "dueAfter": "P2D", "onOverdue": "reject",
                  "excludeActorsOf": ["go_live"], "allowApiTokens": true } ] }))
            .unwrap(),
        );
        assert_eq!(
            checksum(&Some("planned".into()), &states, &transitions),
            "dbbbc8b4c114ca84a3d8d4951f55b60e8a31248f8270338bd8802fde3397089a"
        );
    }

    /// A policy is part of the checksum, and equal intervals checksum equally
    /// however they are written.
    #[test]
    fn an_approval_policy_changes_the_checksum_and_intervals_are_canonical() {
        let (states, mut transitions) = pre_approvals_graph();
        let before = checksum(&None, &states, &transitions);
        let policy = |due: &str| -> WorkflowApproval {
            serde_json::from_value(json!({ "steps": [ { "key": "cab", "name": "CAB", "dueAfter": due } ] })).unwrap()
        };
        transitions[0].approval = Some(policy("PT48H"));
        let hours = checksum(&None, &states, &transitions);
        transitions[0].approval = Some(policy("P2D"));
        assert_eq!(checksum(&None, &states, &transitions), hours);
        assert_ne!(hours, before);
        assert_eq!(serde_json::to_value(&transitions[0].approval).unwrap()["steps"][0]["dueAfter"], "P2D");
    }

    #[test]
    fn due_intervals_parse_and_print_canonically() {
        for (input, minutes, canonical) in [
            ("PT15M", 15, "PT15M"),
            ("PT90M", 90, "PT1H30M"),
            ("PT4H", 240, "PT4H"),
            ("P1DT12H", 2160, "P1DT12H"),
            ("P1W", 10080, "P7D"),
            ("P1W2DT3H4M", 13144, "P9DT3H4M"),
            ("P90D", 129_600, "P90D"),
        ] {
            let d = DueAfter::parse(input).unwrap_or_else(|| panic!("{input}"));
            assert_eq!((d.minutes(), d.to_string().as_str()), (minutes, canonical), "{input}");
        }
        for bad in ["", "P", "PT", "P1H", "PT1D", "P1M", "P1Y", "PT1S", "P1D2W", "PT-1H", "p1d", "P1.5D", "P99999999D"]
        {
            assert_eq!(DueAfter::parse(bad), None, "{bad}");
        }
        let range = serde_json::from_value::<DueAfter>(json!("PT14M")).unwrap_err().to_string();
        assert!(range.starts_with("range|"), "{range}");
        assert!(serde_json::from_value::<DueAfter>(json!("P91D")).is_err());
    }
}
