//! A workflow version's graph: resolving a draft body against the data model,
//! storing it, reading it back in the API form, its canonical checksum, and
//! the publish-time lint (design SHAA-1411 §3.4).
//!
//! The draft body names everything by key (states, fields, lookup values);
//! the tables hold ids, so a version keeps pointing at the same field whatever
//! is renamed later. Conditions are stored with field ids (see
//! [`super::condition`]).

use std::collections::{HashMap, HashSet, VecDeque};

use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use sqlx::types::Json as SqlJson;
use uuid::Uuid;

use super::condition::{self, Scope};
use super::schemas::{
    WorkflowDraftReplace, WorkflowProblem, WorkflowProblemSeverity, WorkflowState, WorkflowStateCategory,
    WorkflowTransition, WorkflowTransitionField, WorkflowVersion, WorkflowVersionStatus,
};
use crate::http::error::{AppError, FieldError, FieldLocation};
use crate::modules::classes::AttributeDataType;
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
}

/// The fields a workflow on one type may use (the type's own and inherited)
/// and the values of their lookup lists.
pub struct Fields {
    pub class_key: String,
    /// Every field of the model, for naming one that is no longer on the type.
    pub model: Model,
    pub lineage: Vec<Uuid>,
    pub values: Vec<LookupValue>,
}

impl Fields {
    pub async fn load(conn: &mut PgConnection, class_id: Uuid) -> Result<Fields, AppError> {
        let model = Model::load(conn).await?;
        let lineage: Vec<Uuid> = model.lineage(class_id).iter().map(|c| c.id).collect();
        let class_key = model.class(class_id).map(|c| c.key.clone()).unwrap_or_default();
        let lists: Vec<Uuid> =
            model.fields.iter().filter(|f| lineage.contains(&f.class_id)).filter_map(|f| f.lookup_list_id).collect();
        let values = sqlx::query_as::<_, LookupValue>(
            "SELECT id, list_id, key FROM cmdb.lookup_list_values WHERE list_id = ANY($1)",
        )
        .bind(&lists)
        .fetch_all(&mut *conn)
        .await?;
        Ok(Fields { class_key, model, lineage, values })
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
        resolved.push(ResolvedTransition { fields: ids, conditions });
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

/// A version as stored.
pub struct Stored {
    pub version: VersionRow,
    pub states: Vec<StateRow>,
    pub transitions: Vec<TransitionRow>,
    pub fields: Vec<FieldRow>,
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
    Ok(Stored { version, states, transitions, fields })
}

impl Stored {
    fn state_key(&self, id: Uuid) -> String {
        self.states.iter().find(|s| s.id == id).map(|s| s.key.clone()).unwrap_or_default()
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

    /// Every field the version depends on: transition fields and conditions
    /// (`workflow_version_attribute_refs`); the definition adds its state field.
    pub fn attributes(&self) -> Vec<Uuid> {
        let mut out: Vec<Uuid> = self.fields.iter().map(|f| f.attribute_id).collect();
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
}

fn error(path: impl Into<String>, code: &str, message: impl Into<String>) -> WorkflowProblem {
    WorkflowProblem {
        path: path.into(),
        code: code.into(),
        message: message.into(),
        severity: WorkflowProblemSeverity::Error,
    }
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
        Some(i) if g.states[i].is_terminal => out.push(error(
            "initialState",
            "initial_state_terminal",
            format!("The initial state {} is terminal: an instance would end as it starts", g.states[i].key),
        )),
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
            out.push(error(
                &path,
                "unreachable_state",
                format!("State {} cannot be reached from the initial state", s.key),
            ));
        }
        if s.is_terminal && !outgoing[i].is_empty() {
            out.push(error(
                &path,
                "terminal_has_transitions",
                format!("State {} is terminal but has outgoing transitions", s.key),
            ));
        } else if !s.is_terminal && outgoing[i].is_empty() {
            out.push(error(
                &path,
                "dead_end",
                format!("State {} is not terminal and has no outgoing transition", s.key),
            ));
        } else if !s.is_terminal && !terminals.is_empty() && !finishes[i] {
            out.push(error(
                &path,
                "no_terminal_reachable",
                format!("No terminal state can be reached from state {}", s.key),
            ));
        }
    }

    // The state field and the values states map to.
    let fields = cx.fields;
    let state_field = cx.state_attribute.and_then(|id| fields.model.field(id));
    if let Some(id) = cx.state_attribute {
        match state_field {
            Some(f) if !fields.on_type(f) => out.push(error(
                "stateAttributeId",
                "unknown_attribute",
                format!("The state field {} is not a field of type {}", f.key, fields.class_key),
            )),
            Some(f) if f.data_type != AttributeDataType::Lookup => out.push(error(
                "stateAttributeId",
                "attribute_type",
                format!("The state field {} is a {} field, not a lookup field", f.key, f.data_type.as_str()),
            )),
            Some(f) if !f.is_active => out.push(error(
                "stateAttributeId",
                "inactive_attribute",
                format!("The state field {} is archived", f.key),
            )),
            Some(_) => {}
            None => out.push(error("stateAttributeId", "unknown_attribute", format!("Field {id} no longer exists"))),
        }
    }
    let state_list = state_field.and_then(|f| f.lookup_list_id);
    for (i, s) in g.states.iter().enumerate() {
        let Some(key) = &s.value_key else { continue };
        let path = format!("states[{i}].stateValue");
        if cx.state_attribute.is_none() {
            out.push(error(
                path,
                "no_state_attribute",
                format!("State {} maps to value {key}, but the workflow has no state field", s.key),
            ));
        } else if s.value_list_id != state_list {
            out.push(error(path, "state_value_list", format!("{key} is not a value of the state field's list")));
        } else if s.value_active == Some(false) {
            out.push(error(path, "state_value_inactive", format!("The value {key} is retired")));
        }
    }

    // Transition fields and conditions.
    for (i, t) in g.transitions.iter().enumerate() {
        for (j, f) in g.fields.iter().filter(|f| f.transition_id == t.id).enumerate() {
            let path = format!("transitions[{i}].fields[{j}].attribute");
            match fields.model.field(f.attribute_id) {
                Some(a) if !fields.on_type(a) => out.push(error(
                    path,
                    "unknown_attribute",
                    format!("{} is not a field of type {}", a.key, fields.class_key),
                )),
                Some(a) if !a.is_active => {
                    out.push(error(path, "inactive_attribute", format!("Field {} is archived", a.key)))
                }
                Some(_) => {}
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
                        out.push(error(&path, "inactive_attribute", format!("Field {} is archived", a.key)));
                    }
                }
                Err(problems) => out.extend(problems.into_iter().map(|p| error(p.field, &p.code, p.message))),
            }
        }
        if !cx.granted.contains(&t.key) {
            out.push(WorkflowProblem {
                path: format!("transitions[{i}]"),
                code: "ungranted_transition".into(),
                message: format!("No profile is granted transition {}: only administrators could run it", t.key),
                severity: WorkflowProblemSeverity::Warning,
            });
        }
    }
    out
}
