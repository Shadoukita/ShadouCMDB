//! What each row of a file would do (SHAA-714 §2): the row pipeline shared
//! by the dry run and the commit.
//!
//! A chunk of rows is planned in three steps:
//! 1. every mapped cell is converted (§2.4) without the database;
//! 2. everything the chunk needs from the database is read in batches:
//!    date-times in their time zone, key and lookup values in the form the
//!    database compares (`lower(btrim(…))`, `::inet`, …), the CIs the keys
//!    match (§2.2), the CIs references and relationships point at (§2.5),
//!    the labels new CIs will get, the matched CIs themselves and their
//!    class definitions;
//! 3. each row, in file order, goes through the CI API's own plan step
//!    (`items::plan`, D10) with an [`ImportResolver`] over what step 2 read,
//!    and its relationships are checked against the type's rules.
//!
//! Only CIs the importer may view are ever found (§4.2).

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, NaiveDateTime, Utc};
use serde_json::{Map, Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use super::convert::{self, Converted, Reading};
use super::mapping::{Column, Key, Lookup, Resolved, Target, is_a, subtree};
use super::parse::{CellValue, Row};
use super::schemas::{EmptyCells, FieldChange, MatchBy, PlannedRow, RelationshipDirection, RowOutcome};
use crate::api::context::RequestContext;
use crate::auth::permissions::ClassOp;
use crate::data::classes::{self as class_data, EffectiveAttributeRow};
use crate::data::items::{self as items_data, StoredValue};
use crate::http::error::{AppError, ErrorCode};
use crate::modules::classes::AttributeDataType;
use crate::modules::items::plan::{self, Plan, Registry, Resolver};
use crate::modules::items::schemas::{ConfigurationItem, CreateItemBody, UpdateItemBody};
use crate::modules::items::service::details;
use crate::schema::model::{Field, Model};

/// Values looked up in one query.
pub const BATCH: usize = 5_000;
/// Targets in one relationship cell (§3.5).
pub const MAX_TARGETS: usize = 50;
/// Characters of a cell kept with its issue (T17).
pub const ISSUE_VALUE_CHARS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

/// A problem with a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub row: u32,
    pub column: Option<u32>,
    pub field: Option<String>,
    pub value: Option<String>,
    pub severity: Severity,
    pub code: String,
    pub message: String,
}

/// What the whole job needs, loaded once.
pub struct JobData {
    pub resolved: Resolved,
    pub model: Model,
    /// Every value of the lists the class's lookup fields use: id → (list, active, parent).
    pub values: HashMap<Uuid, (Uuid, bool, Option<Uuid>)>,
    /// list → its values as (id, key, name).
    pub by_list: HashMap<Uuid, Vec<(Uuid, String, String)>>,
    /// relationship type → (source class, target class) of every rule, and whether it is directional.
    pub rules: HashMap<Uuid, (bool, Vec<(Uuid, Uuid)>)>,
}

impl JobData {
    pub async fn load(conn: &mut PgConnection, resolved: Resolved) -> sqlx::Result<JobData> {
        let model = Model::load(conn).await?;
        let lists: Vec<Uuid> = resolved.defs.iter().filter_map(|d| d.lookup_list_id).collect();
        let rows: Vec<(Uuid, Uuid, String, String, bool, Option<Uuid>)> = sqlx::query_as(
            "SELECT id, list_id, key, name, is_active, parent_value_id FROM cmdb.lookup_list_values
             WHERE list_id = ANY($1) ORDER BY sort_order, key",
        )
        .bind(&lists)
        .fetch_all(&mut *conn)
        .await?;
        let mut values = HashMap::new();
        let mut by_list: HashMap<Uuid, Vec<(Uuid, String, String)>> = HashMap::new();
        for (id, list, key, name, active, parent) in rows {
            values.insert(id, (list, active, parent));
            by_list.entry(list).or_default().push((id, key, name));
        }
        let types: Vec<Uuid> = resolved
            .columns
            .iter()
            .filter_map(|c| match &c.target {
                Target::Relationship { type_id, .. } => Some(*type_id),
                _ => None,
            })
            .collect();
        let rule_rows: Vec<(Uuid, bool, Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
            "SELECT t.id, t.is_directional, r.source_class_id, r.target_class_id
             FROM cmdb.relationship_types t LEFT JOIN cmdb.relationship_type_rules r ON r.relationship_type_id = t.id
             WHERE t.id = ANY($1)",
        )
        .bind(&types)
        .fetch_all(&mut *conn)
        .await?;
        let mut rules: HashMap<Uuid, (bool, Vec<(Uuid, Uuid)>)> = HashMap::new();
        for (t, directional, s, g) in rule_rows {
            let e = rules.entry(t).or_insert((directional, Vec::new()));
            if let (Some(s), Some(g)) = (s, g) {
                e.1.push((s, g));
            }
        }
        Ok(JobData { resolved, model, values, by_list, rules })
    }

    /// The type allows an edge from a CI of class `source` to one of `target`.
    fn allows(&self, type_id: Uuid, source: Uuid, target: Uuid) -> bool {
        let Some((directional, rules)) = self.rules.get(&type_id) else { return false };
        rules.iter().any(|(s, t)| {
            (is_a(&self.model, source, *s) && is_a(&self.model, target, *t))
                || (!directional && is_a(&self.model, source, *t) && is_a(&self.model, target, *s))
        })
    }
}

// ---------------------------------------------------------------------------
// Comparing values the way the database does
// ---------------------------------------------------------------------------

/// Something a CI can be found by.
#[derive(Debug, Clone)]
pub enum Dim {
    Ident,
    Label,
    Field(Field),
}

/// A key of the pending index: the dimension (the field by id) and the canonical value.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DimKey {
    Ident,
    Label,
    Field(Uuid),
}

impl Dim {
    pub fn key(&self) -> DimKey {
        match self {
            Dim::Ident => DimKey::Ident,
            Dim::Label => DimKey::Label,
            Dim::Field(f) => DimKey::Field(f.id),
        }
    }

    fn data_type(&self) -> AttributeDataType {
        match self {
            Dim::Field(f) => f.data_type,
            _ => AttributeDataType::Text,
        }
    }

    /// Whether the database can cast `raw` for this dimension (checked here, so
    /// a bad cell never fails a whole batch).
    pub fn valid(&self, raw: &str) -> bool {
        match self.data_type() {
            AttributeDataType::Integer => raw.trim().parse::<i64>().is_ok(),
            AttributeDataType::Ip => raw.trim().parse::<ipnetwork::IpNetwork>().is_ok(),
            AttributeDataType::Cidr => raw.trim().parse::<ipnetwork::IpNetwork>().is_ok_and(|n| n.network() == n.ip()),
            _ => true,
        }
    }

    /// The comparison form of `expr` (T10).
    fn canon(&self, expr: &str) -> String {
        match self.data_type() {
            AttributeDataType::Integer => format!("(btrim({expr}::text))::bigint::text"),
            AttributeDataType::Ip => format!("(btrim({expr}::text))::inet::text"),
            AttributeDataType::Cidr => format!("(btrim({expr}::text))::cidr::text"),
            _ => format!("lower(btrim({expr}))"),
        }
    }

    fn column(&self, model: &Model) -> Option<(String, String)> {
        match self {
            Dim::Ident => Some((String::new(), "ci.ident".into())),
            Dim::Label => Some((String::new(), "ci.label".into())),
            Dim::Field(f) => {
                let table = model.table(f.class_id)?;
                Some((format!(" JOIN {} t ON t.id = ci.id", table.sql()), format!("t.{}", f.column())))
            }
        }
    }
}

/// raw → canonical form, for the valid values.
pub async fn canonical(conn: &mut PgConnection, dim: &Dim, raws: &[String]) -> sqlx::Result<HashMap<String, String>> {
    let mut out = HashMap::new();
    let valid: Vec<String> =
        raws.iter().filter(|r| dim.valid(r)).cloned().collect::<HashSet<_>>().into_iter().collect();
    for batch in valid.chunks(BATCH) {
        let sql = format!("SELECT x, {} FROM unnest($1::text[]) AS u(x)", dim.canon("x"));
        let rows: Vec<(String, String)> =
            sqlx::query_as(sqlx::AssertSqlSafe(sql)).bind(batch).fetch_all(&mut *conn).await?;
        out.extend(rows);
    }
    Ok(out)
}

/// CIs of `classes` whose `dim` has one of these canonical values:
/// (canonical value, CI id, class id). `deleted` looks at soft-deleted CIs instead.
pub async fn find(
    conn: &mut PgConnection,
    model: &Model,
    dim: &Dim,
    canons: &[String],
    classes: &[Uuid],
    deleted: bool,
) -> sqlx::Result<Vec<(String, Uuid, Uuid)>> {
    let Some((join, column)) = dim.column(model) else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    let unique: Vec<String> = canons.iter().cloned().collect::<HashSet<_>>().into_iter().collect();
    for batch in unique.chunks(BATCH) {
        let sql = format!(
            "SELECT {c}, ci.id, ci.class_id FROM cmdb.configuration_items ci{join}
             WHERE {c} = ANY($1) AND ci.class_id = ANY($2) AND ci.deleted_at IS {} NULL",
            if deleted { "NOT" } else { "" },
            c = dim.canon(&column),
        );
        let rows: Vec<(String, Uuid, Uuid)> =
            sqlx::query_as(sqlx::AssertSqlSafe(sql)).bind(batch).bind(classes).fetch_all(&mut *conn).await?;
        out.extend(rows);
    }
    Ok(out)
}

/// The dimensions a lookup compares.
pub fn dims(lookup: &Lookup) -> Vec<Dim> {
    match lookup.by {
        MatchBy::Ident => vec![Dim::Ident],
        MatchBy::Label => vec![Dim::Label],
        MatchBy::Attribute => lookup.fields.iter().cloned().map(Dim::Field).collect(),
    }
}

/// The dimension of the key.
pub fn key_dim(resolved: &Resolved, model: &Model) -> Option<Dim> {
    match resolved.key.as_ref()? {
        Key::Ident => Some(Dim::Ident),
        Key::Attribute(i) => model.fields.iter().find(|f| f.id == resolved.defs[*i].id).cloned().map(Dim::Field),
    }
}

// ---------------------------------------------------------------------------
// CIs created by the file (§2.5 "pending CIs")
// ---------------------------------------------------------------------------

/// The CIs rows of the file create, by what they can be found by: row number and the id they get.
#[derive(Debug, Default)]
pub struct Pending {
    pub entries: HashMap<(DimKey, String), Vec<(u32, Uuid)>>,
    /// Rows creating a CI, by their key's canonical value (for `reference_to_pending_unresolvable`).
    pub by_key: HashMap<String, u32>,
}

impl Pending {
    pub fn add(&mut self, dim: DimKey, canon: String, row: u32, id: Uuid) {
        self.entries.entry((dim, canon)).or_default().push((row, id));
    }
}

// ---------------------------------------------------------------------------
// The resolver: what the CI API's plan step asks, answered from memory
// ---------------------------------------------------------------------------

/// [`Resolver`] over the job's lookup values and the CIs a chunk refers to,
/// including the CIs earlier rows create (T1).
pub struct ImportResolver<'j> {
    values: &'j HashMap<Uuid, (Uuid, bool, Option<Uuid>)>,
    extra_values: HashMap<Uuid, (Uuid, bool, Option<Uuid>)>,
    cis: HashMap<Uuid, Uuid>,
}

impl Resolver for ImportResolver<'_> {
    fn lookup_state(&self, list_id: Uuid, value_id: Uuid) -> Option<bool> {
        self.values
            .get(&value_id)
            .or(self.extra_values.get(&value_id))
            .filter(|(list, _, _)| *list == list_id)
            .map(|(_, active, _)| *active)
    }
    fn lookup_parent(&self, value_id: Uuid) -> Option<Uuid> {
        self.values.get(&value_id).or(self.extra_values.get(&value_id)).and_then(|(_, _, p)| *p)
    }
    fn visible_target(&self, ci_id: Uuid) -> Option<Uuid> {
        self.cis.get(&ci_id).copied()
    }
}

// ---------------------------------------------------------------------------
// Step 1: cells
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Cell {
    Value(Value),
    Local(NaiveDateTime, String),
    Empty(EmptyCells),
    /// A reference, to resolve: the cell's text.
    Ref(String),
}

#[derive(Debug, Clone)]
struct Draft {
    key_raw: Option<String>,
    ident: Option<String>,
    valid_from: Option<Cell>,
    valid_until: Option<Cell>,
    /// (definition index, column index, cell)
    attrs: Vec<(usize, usize, Cell)>,
    /// (column index, targets' texts)
    rels: Vec<(usize, Vec<String>)>,
    issues: Vec<Issue>,
    failed: bool,
}

fn cell_text(row: &Row, index: u32) -> Option<String> {
    let c = row.cells.get(index as usize)?;
    if c.is_blank() {
        return None;
    }
    let t = c.display();
    Some(if t.chars().count() > ISSUE_VALUE_CHARS { t.chars().take(ISSUE_VALUE_CHARS).collect() } else { t })
}

fn issue(row: &Row, col: Option<&Column>, field: Option<String>, sev: Severity, code: &str, message: &str) -> Issue {
    Issue {
        row: row.number,
        column: col.map(|c| c.index),
        field,
        value: col.and_then(|c| cell_text(row, c.index)),
        severity: sev,
        code: code.into(),
        message: message.into(),
    }
}

fn value_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn draft(job: &JobData, row: &Row) -> Draft {
    let r = &job.resolved;
    let mut d = Draft {
        key_raw: None,
        ident: None,
        valid_from: None,
        valid_until: None,
        attrs: Vec::new(),
        rels: Vec::new(),
        issues: Vec::new(),
        failed: false,
    };
    for (ci, col) in r.columns.iter().enumerate() {
        let cell = row.cells.get(col.index as usize).unwrap_or(&CellValue::Empty);
        let reading = |enums: &'static [String]| Reading {
            trim: r.options.trim,
            decimal_separator: col.decimal_separator,
            date_format: col.date_format,
            enum_values: enums,
        };
        let fail = |d: &mut Draft, field: Option<String>, code: &str, message: &str| {
            d.issues.push(issue(row, Some(col), field, Severity::Error, code, message));
            d.failed = true;
        };
        match &col.target {
            Target::Ident => {
                d.ident = convert::text(cell, true);
            }
            Target::ValidFrom | Target::ValidUntil => {
                let field = if matches!(col.target, Target::ValidFrom) { "validFrom" } else { "validUntil" };
                let v = match convert::convert(cell, AttributeDataType::Datetime, &reading(&[])) {
                    Ok(Converted::Empty) => Cell::Empty(col.empty_cells),
                    Ok(Converted::Value(v)) => Cell::Value(v),
                    Ok(Converted::Local(t)) => Cell::Local(t, col.time_zone.clone()),
                    Err(p) => {
                        fail(&mut d, Some(field.into()), p.code, &p.message);
                        continue;
                    }
                };
                if field == "validFrom" {
                    d.valid_from = Some(v);
                } else {
                    d.valid_until = Some(v);
                }
            }
            Target::Attribute { def, reference } => {
                let def_row = &r.defs[*def];
                let field = format!("attributes.{}", def_row.key);
                let enums: Vec<String> = def_row.enum_values.as_ref().map(|e| e.0.clone()).unwrap_or_default();
                let reading = Reading {
                    trim: r.options.trim,
                    decimal_separator: col.decimal_separator,
                    date_format: col.date_format,
                    enum_values: &enums,
                };
                let converted = match convert::convert(cell, def_row.data_type, &reading) {
                    Ok(c) => c,
                    Err(p) => {
                        fail(&mut d, Some(field), p.code, &p.message);
                        continue;
                    }
                };
                let c = match converted {
                    Converted::Empty => Cell::Empty(col.empty_cells),
                    Converted::Local(t) => Cell::Local(t, col.time_zone.clone()),
                    Converted::Value(v) if reference.is_some() => Cell::Ref(value_text(&v).unwrap_or_default()),
                    Converted::Value(v) if def_row.data_type == AttributeDataType::Lookup => {
                        let text = value_text(&v).unwrap_or_default();
                        match lookup_value(job, def_row.lookup_list_id.unwrap_or_default(), &text) {
                            Ok(id) => Cell::Value(json!(id.to_string())),
                            Err((code, message)) => {
                                fail(&mut d, Some(field), code, message);
                                continue;
                            }
                        }
                    }
                    Converted::Value(v) => Cell::Value(v),
                };
                if matches!(r.key, Some(Key::Attribute(k)) if k == *def)
                    && let Cell::Value(v) = &c
                {
                    d.key_raw = value_text(v);
                }
                d.attrs.push((*def, ci, c));
            }
            Target::Relationship { .. } => {
                let Some(text) = convert::text(cell, true) else { continue };
                let sep = r.options.list_separator.chars().next().unwrap_or(';');
                let targets: Vec<String> =
                    text.split(sep).map(|t| t.trim().to_owned()).filter(|t| !t.is_empty()).collect();
                if targets.len() > MAX_TARGETS {
                    fail(&mut d, None, "too_many_values", &format!("At most {MAX_TARGETS} targets per cell"));
                    continue;
                }
                d.rels.push((ci, targets));
            }
        }
    }
    if matches!(r.key, Some(Key::Ident)) {
        d.key_raw = d.ident.clone();
    }
    d
}

/// A lookup cell's value: its key within the list, else its name ignoring case (§2.4).
fn lookup_value(job: &JobData, list: Uuid, text: &str) -> Result<Uuid, (&'static str, &'static str)> {
    let values = job.by_list.get(&list).map(Vec::as_slice).unwrap_or_default();
    if let Some((id, _, _)) = values.iter().find(|(_, k, _)| k == text) {
        return Ok(*id);
    }
    let lower = text.to_lowercase();
    let named: Vec<&(Uuid, String, String)> = values.iter().filter(|(_, _, n)| n.to_lowercase() == lower).collect();
    match named.as_slice() {
        [one] => Ok(one.0),
        [] => Err(("not_found", "Not a value of this attribute's list")),
        _ => Err(("ambiguous_value", "Several values of the list have this name")),
    }
}

// ---------------------------------------------------------------------------
// Matching rows to existing CIs
// ---------------------------------------------------------------------------

/// Where a row's key finds existing CIs.
pub enum Matches<'a> {
    /// The dry run's job-wide index: canonical key → (CI, class).
    Index(&'a HashMap<String, Vec<(Uuid, Uuid)>>),
    /// The commit re-resolves each chunk's keys (T4) and locks the CIs (T3).
    Chunk,
}

/// What planning a chunk needs besides the rows.
pub struct Context<'a> {
    pub job: &'a JobData,
    pub ctx: &'a RequestContext,
    pub matches: Matches<'a>,
    /// CIs the file creates: the dry run knows the whole file's (forward
    /// references are errors), the commit only the chunk's earlier rows.
    pub pending: &'a mut Pending,
    /// Rows with duplicate keys (dry run): canonical key → rows.
    pub duplicates: Option<&'a HashMap<String, Vec<u32>>>,
    /// The ids new CIs get: the dry run chose them for the whole file.
    pub new_ids: Option<&'a HashMap<u32, Uuid>>,
    /// Lock matched CIs (commit).
    pub lock: bool,
    /// Add each created CI to `pending` as it is planned (commit); the dry
    /// run filled it for the whole file first ([`collect`]).
    pub grow_pending: bool,
}

/// A planned row.
#[derive(Debug)]
pub struct Planned {
    pub number: u32,
    pub outcome: RowOutcome,
    pub ci_id: Option<Uuid>,
    pub class_id: Option<Uuid>,
    pub label: Option<String>,
    pub changes: Vec<FieldChange>,
    pub issues: Vec<Issue>,
    pub create: Option<CreateItemBody>,
    pub update: Option<(ConfigurationItem, UpdateItemBody)>,
    /// Edges to add: (type, source, target, column).
    pub edges: Vec<(Uuid, Uuid, Uuid, usize)>,
    pub edges_existing: u32,
}

impl Planned {
    pub fn has_errors(&self) -> bool {
        self.issues.iter().any(|i| i.severity == Severity::Error)
    }

    pub fn preview(&self) -> PlannedRow {
        PlannedRow {
            row: self.number,
            outcome: self.outcome,
            ci_id: self.ci_id,
            ci_label: self.label.clone(),
            changes: self.changes.clone(),
        }
    }
}

/// The chunk's plans, with what applying them needs.
pub struct ChunkPlan<'j> {
    pub rows: Vec<Planned>,
    pub resolver: ImportResolver<'j>,
    pub defs: HashMap<Uuid, Vec<EffectiveAttributeRow>>,
}

fn stored_equals(before: &Value, new: &StoredValue) -> bool {
    match new {
        StoredValue::Text(s) | StoredValue::Date(s) => before.as_str() == Some(s.as_str()),
        StoredValue::Number(n) => before.as_f64() == Some(*n),
        StoredValue::Boolean(b) => before.as_bool() == Some(*b),
        StoredValue::Datetime(s) => {
            let a = before.as_str().and_then(|b| DateTime::parse_from_rfc3339(b).ok());
            a.is_some() && a == DateTime::parse_from_rfc3339(s).ok()
        }
        StoredValue::Ip(s) | StoredValue::Cidr(s) => {
            let a = before.as_str().and_then(|b| b.parse::<ipnetwork::IpNetwork>().ok());
            a.is_some() && a == s.parse::<ipnetwork::IpNetwork>().ok()
        }
        StoredValue::Reference(id) | StoredValue::Lookup(id) => before.as_str() == Some(id.to_string().as_str()),
    }
}

fn stored_json(v: &StoredValue) -> Value {
    match v {
        StoredValue::Number(n) => items_data::number_json(*n),
        StoredValue::Boolean(b) => json!(b),
        other => json!(other.as_text()),
    }
}

/// The changes a plan makes to `before`, and whether it makes any.
fn changes(plan: &Plan<'_>, before: &ConfigurationItem) -> Vec<FieldChange> {
    let mut out = Vec::new();
    for (def, v) in &plan.set {
        let old = before.attributes.get(&def.key).cloned().unwrap_or(Value::Null);
        if !stored_equals(&old, v) {
            out.push(FieldChange { field: format!("attributes.{}", def.key), old, new: stored_json(v) });
        }
    }
    if let Registry::Update { ident, valid_from, valid_until, .. } = &plan.registry {
        if let Some(i) = ident {
            out.push(FieldChange { field: "ident".into(), old: json!(before.summary.ident), new: json!(i) });
        }
        if let Some(v) = valid_from
            && *v != before.summary.valid_from
        {
            out.push(FieldChange { field: "validFrom".into(), old: json!(before.summary.valid_from), new: json!(v) });
        }
        if let Some(v) = valid_until
            && *v != before.summary.valid_until
        {
            out.push(FieldChange { field: "validUntil".into(), old: json!(before.summary.valid_until), new: json!(v) });
        }
    }
    out
}

/// Plans a chunk of rows (see the module docs).
pub async fn plan_chunk<'j>(
    conn: &mut PgConnection,
    c: &mut Context<'j>,
    rows: &[Row],
) -> Result<ChunkPlan<'j>, AppError> {
    let job = c.job;
    let r = &job.resolved;
    let model = &job.model;
    let visible_classes = |classes: Vec<Uuid>| -> Vec<Uuid> {
        classes.into_iter().filter(|cl| c.ctx.require_class(*cl, ClassOp::View).is_ok()).collect()
    };
    let import_classes = visible_classes(subtree(model, r.class_id));

    // Step 1: cells.
    let mut drafts: Vec<Draft> = rows.iter().map(|row| draft(job, row)).collect();

    // Step 2a: date-times without an offset, in their zone.
    let mut locals: Vec<(String, String)> = Vec::new();
    for d in &drafts {
        for cell in d.attrs.iter().map(|(_, _, c)| c).chain(d.valid_from.iter()).chain(d.valid_until.iter()) {
            if let Cell::Local(t, tz) = cell {
                locals.push((t.format("%Y-%m-%dT%H:%M:%S%.f").to_string(), tz.clone()));
            }
        }
    }
    let instants = local_instants(conn, &locals).await?;
    let fix = |cell: &mut Cell| {
        if let Cell::Local(t, tz) = cell {
            let k = (t.format("%Y-%m-%dT%H:%M:%S%.f").to_string(), tz.clone());
            if let Some(v) = instants.get(&k) {
                *cell = Cell::Value(json!(v));
            }
        }
    };
    for d in &mut drafts {
        for (_, _, cell) in &mut d.attrs {
            fix(cell);
        }
        if let Some(cell) = &mut d.valid_from {
            fix(cell);
        }
        if let Some(cell) = &mut d.valid_until {
            fix(cell);
        }
    }

    // Step 2b: the rows' keys, and the CIs they match.
    let key_dim = key_dim(r, model);
    let key_canon: HashMap<String, String> = match &key_dim {
        Some(dim) => {
            let raws: Vec<String> = drafts.iter().filter_map(|d| d.key_raw.clone()).collect();
            canonical(conn, dim, &raws).await?
        }
        None => HashMap::new(),
    };
    let mut chunk_matches: HashMap<String, Vec<(Uuid, Uuid)>> = HashMap::new();
    if let (Matches::Chunk, Some(dim)) = (&c.matches, &key_dim) {
        let canons: Vec<String> = key_canon.values().cloned().collect();
        for (canon, id, class) in find(conn, model, dim, &canons, &import_classes, false).await? {
            chunk_matches.entry(canon).or_default().push((id, class));
        }
    }
    let matches_of = |canon: &str| -> Vec<(Uuid, Uuid)> {
        match &c.matches {
            Matches::Index(index) => index.get(canon).cloned().unwrap_or_default(),
            Matches::Chunk => chunk_matches.get(canon).cloned().unwrap_or_default(),
        }
    };
    let mut matched_ids: Vec<Uuid> = Vec::new();
    for d in &drafts {
        if let Some(canon) = d.key_raw.as_ref().and_then(|k| key_canon.get(k))
            && let [(id, _)] = matches_of(canon).as_slice()
        {
            matched_ids.push(*id);
        }
    }
    matched_ids.sort();
    matched_ids.dedup();
    if c.lock && !matched_ids.is_empty() {
        // One statement, in id order, so concurrent chunks cannot deadlock on each other (T3).
        sqlx::query("SELECT id FROM cmdb.configuration_items WHERE id = ANY($1) ORDER BY id FOR UPDATE")
            .bind(&matched_ids)
            .execute(&mut *conn)
            .await?;
    }
    let befores: HashMap<Uuid, ConfigurationItem> =
        details(conn, model, &matched_ids).await?.into_iter().map(|ci| (ci.summary.id, ci)).collect();

    // Step 2c: references and relationship targets.
    let mut targets: HashMap<(usize, String), Vec<(Uuid, Uuid)>> = HashMap::new();
    let mut target_canon: HashMap<(usize, DimKey, String), String> = HashMap::new();
    for (ci, col) in r.columns.iter().enumerate() {
        let lookup = match &col.target {
            Target::Attribute { reference: Some(l), .. } => l,
            Target::Relationship { lookup, .. } => lookup,
            _ => continue,
        };
        let raws: Vec<String> = drafts
            .iter()
            .flat_map(|d| {
                let mut v: Vec<String> = d
                    .attrs
                    .iter()
                    .filter(|(_, c2, _)| *c2 == ci)
                    .filter_map(|(_, _, cell)| if let Cell::Ref(t) = cell { Some(t.clone()) } else { None })
                    .collect();
                v.extend(d.rels.iter().filter(|(c2, _)| *c2 == ci).flat_map(|(_, t)| t.clone()));
                v
            })
            .collect();
        if raws.is_empty() {
            continue;
        }
        for dim in dims(lookup) {
            let canon = canonical(conn, &dim, &raws).await?;
            let found =
                find(conn, model, &dim, &canon.values().cloned().collect::<Vec<_>>(), &lookup.candidates, false)
                    .await?;
            let mut by_canon: HashMap<String, Vec<(Uuid, Uuid)>> = HashMap::new();
            for (cv, id, class) in found {
                by_canon.entry(cv).or_default().push((id, class));
            }
            for (raw, cv) in canon {
                let hits = by_canon.get(&cv).cloned().unwrap_or_default();
                let e = targets.entry((ci, raw.clone())).or_default();
                for h in hits {
                    if !e.contains(&h) {
                        e.push(h);
                    }
                }
                target_canon.insert((ci, dim.key(), raw), cv);
            }
        }
    }

    // Step 2d: the labels new CIs will get (T9), when the title field is mapped.
    let title = r.title_field.clone();
    let title_def = title.as_ref().and_then(|t| r.defs.iter().position(|d| d.id == t.id));
    let mut title_values: Vec<String> = Vec::new();
    for d in &drafts {
        if let Some(i) = title_def
            && let Some((_, _, Cell::Value(v))) = d.attrs.iter().find(|(def, _, _)| *def == i)
            && let Some(t) = value_text(v)
        {
            title_values.push(t);
        }
    }
    let labels: HashMap<String, Option<String>> = match &title {
        Some(f) if !title_values.is_empty() => {
            let unique: Vec<String> = title_values.iter().cloned().collect::<HashSet<_>>().into_iter().collect();
            let computed = items_data::labels_of(conn, f, &unique).await?;
            unique.into_iter().zip(computed).collect()
        }
        _ => HashMap::new(),
    };
    let label_canon: HashMap<String, String> = canonical(
        conn,
        &Dim::Label,
        &labels.values().flatten().cloned().chain(drafts.iter().filter_map(|d| d.ident.clone())).collect::<Vec<_>>(),
    )
    .await?;
    // Canonical values of the class's own lookup fields, for the CIs rows create.
    let own_dims: Vec<(usize, Dim)> = r
        .columns
        .iter()
        .filter_map(|col| match &col.target {
            Target::Attribute { def, reference: None } => {
                let f = model.fields.iter().find(|f| f.id == r.defs[*def].id)?;
                super::mapping::matchable(f.data_type).then(|| (*def, Dim::Field(f.clone())))
            }
            _ => None,
        })
        .collect();
    let mut own_canon: HashMap<(usize, String), String> = HashMap::new();
    for (def, dim) in &own_dims {
        let raws: Vec<String> = drafts
            .iter()
            .filter_map(|d| d.attrs.iter().find(|(i, _, _)| i == def))
            .filter_map(|(_, _, cell)| if let Cell::Value(v) = cell { value_text(v) } else { None })
            .collect();
        for (raw, cv) in canonical(conn, dim, &raws).await? {
            own_canon.insert((*def, raw), cv);
        }
    }

    // Class definitions of every class a row touches.
    let mut defs: HashMap<Uuid, Vec<EffectiveAttributeRow>> = HashMap::new();
    defs.insert(r.class_id, r.defs.clone());
    for b in befores.values() {
        if let std::collections::hash_map::Entry::Vacant(e) = defs.entry(b.summary.class_id) {
            e.insert(class_data::effective_attributes(conn, b.summary.class_id).await?);
        }
    }

    // Existing edges of the matched CIs.
    let mut existing: HashSet<(Uuid, Uuid, Uuid)> = HashSet::new();
    let has_rels = r.columns.iter().any(|col| matches!(col.target, Target::Relationship { .. }));
    if has_rels && !matched_ids.is_empty() {
        let rows: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
            "SELECT relationship_type_id, source_ci_id, target_ci_id FROM cmdb.ci_relationships
             WHERE deleted_at IS NULL AND (source_ci_id = ANY($1) OR target_ci_id = ANY($1))",
        )
        .bind(&matched_ids)
        .fetch_all(&mut *conn)
        .await?;
        existing.extend(rows);
    }

    // The resolver: every CI a reference may point at, found above or created by the file.
    let mut cis: HashMap<Uuid, Uuid> = HashMap::new();
    for hits in targets.values() {
        for (id, class) in hits {
            cis.insert(*id, *class);
        }
    }
    for ((_, _), entries) in c.pending.entries.iter() {
        for (_, id) in entries {
            cis.insert(*id, r.class_id);
        }
    }
    let mut resolver = ImportResolver { values: &job.values, extra_values: HashMap::new(), cis };
    // Current lookup values of matched CIs may be of lists the mapping does not use.
    let mut extra: Vec<Uuid> = Vec::new();
    for b in befores.values() {
        for v in b.attributes.values() {
            if let Some(id) = v.as_str().and_then(|s| Uuid::parse_str(s).ok())
                && !job.values.contains_key(&id)
            {
                extra.push(id);
            }
        }
    }
    if !extra.is_empty() {
        let rows: Vec<(Uuid, Uuid, bool, Option<Uuid>)> = sqlx::query_as(
            "SELECT id, list_id, is_active, parent_value_id FROM cmdb.lookup_list_values WHERE id = ANY($1)",
        )
        .bind(&extra)
        .fetch_all(&mut *conn)
        .await?;
        resolver.extra_values = rows.into_iter().map(|(id, l, a, p)| (id, (l, a, p))).collect();
    }

    // Each row's matches, before the rows are planned (they add to `pending`).
    let row_hits: Vec<Vec<(Uuid, Uuid)>> = drafts
        .iter()
        .map(|d| d.key_raw.as_ref().and_then(|k| key_canon.get(k)).map(|k| matches_of(k)).unwrap_or_default())
        .collect();

    // Step 3: row by row.
    let mut out: Vec<Planned> = Vec::with_capacity(rows.len());
    let mut added: HashSet<(Uuid, Uuid, Uuid)> = HashSet::new();
    for ((row, d), hits) in rows.iter().zip(drafts).zip(row_hits) {
        let planned = plan_row(
            c,
            row,
            d,
            &befores,
            &key_canon,
            hits,
            &targets,
            &target_canon,
            &labels,
            &label_canon,
            &own_canon,
            &defs,
            &mut resolver,
            &existing,
            &mut added,
        );
        out.push(planned);
    }
    Ok(ChunkPlan { rows: out, resolver, defs })
}

/// Date-times without an offset → RFC 3339 instants, per their zone.
async fn local_instants(
    conn: &mut PgConnection,
    locals: &[(String, String)],
) -> sqlx::Result<HashMap<(String, String), String>> {
    let mut out = HashMap::new();
    let unique: Vec<(String, String)> = locals.iter().cloned().collect::<HashSet<_>>().into_iter().collect();
    for batch in unique.chunks(BATCH) {
        let (times, zones): (Vec<String>, Vec<String>) = batch.iter().cloned().unzip();
        let rows: Vec<(String, String, DateTime<Utc>)> =
            sqlx::query_as("SELECT t, z, (t::timestamp AT TIME ZONE z) FROM unnest($1::text[], $2::text[]) AS u(t, z)")
                .bind(&times)
                .bind(&zones)
                .fetch_all(&mut *conn)
                .await?;
        for (t, z, instant) in rows {
            out.insert((t, z), instant.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true));
        }
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn plan_row(
    c: &mut Context<'_>,
    row: &Row,
    mut d: Draft,
    befores: &HashMap<Uuid, ConfigurationItem>,
    key_canon: &HashMap<String, String>,
    hits: Vec<(Uuid, Uuid)>,
    targets: &HashMap<(usize, String), Vec<(Uuid, Uuid)>>,
    target_canon: &HashMap<(usize, DimKey, String), String>,
    labels: &HashMap<String, Option<String>>,
    label_canon: &HashMap<String, String>,
    own_canon: &HashMap<(usize, String), String>,
    defs: &HashMap<Uuid, Vec<EffectiveAttributeRow>>,
    resolver: &mut ImportResolver<'_>,
    existing: &HashSet<(Uuid, Uuid, Uuid)>,
    added: &mut HashSet<(Uuid, Uuid, Uuid)>,
) -> Planned {
    let job = c.job;
    let r = &job.resolved;
    let model = &job.model;
    let key_col = r.key_column.map(|i| &r.columns[i]);
    let mut p = Planned {
        number: row.number,
        outcome: RowOutcome::Error,
        ci_id: None,
        class_id: None,
        label: None,
        changes: Vec::new(),
        issues: std::mem::take(&mut d.issues),
        create: None,
        update: None,
        edges: Vec::new(),
        edges_existing: 0,
    };
    let key_field = key_col.map(|col| match &col.target {
        Target::Attribute { def, .. } => format!("attributes.{}", r.defs[*def].key),
        _ => "ident".into(),
    });
    let fail = |p: &mut Planned, col: Option<&Column>, field: Option<String>, code: &str, message: &str| {
        p.issues.push(issue(row, col, field, Severity::Error, code, message));
    };

    // Matching (§2.2).
    let canon = d.key_raw.as_ref().and_then(|k| key_canon.get(k)).cloned();
    enum Outcome {
        Create,
        Update(Uuid),
    }
    let outcome = if !r.updates() {
        Some(Outcome::Create)
    } else if d.key_raw.is_none() {
        if r.creates() && r.key.is_none() {
            Some(Outcome::Create)
        } else {
            fail(&mut p, key_col, key_field.clone(), "key_empty", "The key is empty, so the row cannot be matched");
            None
        }
    } else if let Some(rows) = canon.as_ref().and_then(|k| c.duplicates.and_then(|dup| dup.get(k))) {
        let others: Vec<String> = rows.iter().filter(|n| **n != row.number).map(u32::to_string).collect();
        fail(
            &mut p,
            key_col,
            key_field.clone(),
            "duplicate_key_in_file",
            &format!("The same key is also on rows {}", others.join(", ")),
        );
        None
    } else {
        match hits.as_slice() {
            [] if r.creates() => Some(Outcome::Create),
            [] => {
                fail(&mut p, key_col, key_field.clone(), "no_match", "No CI has this key");
                None
            }
            [(id, _)] if r.updates() && r.mode != super::schemas::ImportMode::CreateOnly => Some(Outcome::Update(*id)),
            [(id, _)] => {
                let label = befores.get(id).map(|b| b.summary.label.clone()).unwrap_or_default();
                fail(
                    &mut p,
                    key_col,
                    key_field.clone(),
                    "exists",
                    &format!("A CI with this key already exists: {label}"),
                );
                None
            }
            many => {
                fail(
                    &mut p,
                    key_col,
                    key_field.clone(),
                    "ambiguous_match",
                    &format!("{} CIs have this value. Use a more specific key.", many.len()),
                );
                None
            }
        }
    };
    let create_only_key_exists = r.mode == super::schemas::ImportMode::CreateOnly && !hits.is_empty();
    if create_only_key_exists && p.issues.iter().all(|i| i.code != "exists") {
        let label =
            hits.first().and_then(|(id, _)| befores.get(id).map(|b| b.summary.label.clone())).unwrap_or_default();
        fail(&mut p, key_col, key_field.clone(), "exists", &format!("A CI with this key already exists: {label}"));
    }

    // References (§2.5).
    let mut attributes: Map<String, Value> = Map::new();
    let before = match &outcome {
        Some(Outcome::Update(id)) => befores.get(id),
        _ => None,
    };
    for (def, col_i, cell) in &d.attrs {
        let col = &r.columns[*col_i];
        let def_row = &r.defs[*def];
        let field = format!("attributes.{}", def_row.key);
        match cell {
            Cell::Value(v) => {
                attributes.insert(def_row.key.clone(), v.clone());
            }
            Cell::Local(..) => {
                fail(&mut p, Some(col), Some(field), "invalid_format", "Not a valid date-time in this time zone")
            }
            Cell::Empty(EmptyCells::Clear) => {
                // Clearing a value that is not set is no change (T11); a new CI gets the default.
                if before.is_some_and(|b| b.attributes.contains_key(&def_row.key)) {
                    attributes.insert(def_row.key.clone(), Value::Null);
                }
            }
            Cell::Empty(EmptyCells::Ignore) => {}
            Cell::Ref(text) => {
                let Target::Attribute { reference: Some(lookup), .. } = &col.target else { continue };
                match resolve_target(c, row.number, *col_i, lookup, text, targets, target_canon) {
                    Ok((id, class)) => {
                        resolver.cis.insert(id, class);
                        attributes.insert(def_row.key.clone(), json!(id.to_string()));
                    }
                    Err((code, message)) => fail(&mut p, Some(col), Some(field), code, &message),
                }
            }
        }
    }

    let registry_time = |cell: &Option<Cell>| -> Result<Option<Option<DateTime<Utc>>>, ()> {
        match cell {
            None | Some(Cell::Empty(EmptyCells::Ignore)) => Ok(None),
            Some(Cell::Empty(EmptyCells::Clear)) => Ok(Some(None)),
            Some(Cell::Value(v)) => {
                v.as_str().and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map(|t| Some(Some(t.to_utc()))).ok_or(())
            }
            _ => Err(()),
        }
    };
    let valid_from = registry_time(&d.valid_from);
    let valid_until = registry_time(&d.valid_until);
    if valid_from.is_err() || valid_until.is_err() {
        fail(&mut p, None, None, "invalid_format", "Not a valid date-time");
    }
    let valid_from = valid_from.unwrap_or(None).flatten();
    let valid_until = valid_until.unwrap_or(None);

    if p.has_errors() || d.failed {
        p.outcome = RowOutcome::Error;
        return p;
    }

    // The CI: the same plan step as the API (D10).
    let row_ci: Uuid;
    let row_class: Uuid;
    match outcome {
        Some(Outcome::Create) => {
            let id = c.new_ids.and_then(|m| m.get(&row.number).copied()).unwrap_or_else(Uuid::new_v4);
            let body = CreateItemBody {
                class_id: r.class_id,
                ident: d.ident.clone(),
                valid_from,
                valid_until: valid_until.flatten(),
                attributes: Some(attributes),
            };
            let class_defs = defs.get(&r.class_id).map(Vec::as_slice).unwrap_or_default();
            match plan::plan_create(c.ctx, model, class_defs, &body, &*resolver) {
                Ok(planned) => {
                    p.changes = planned
                        .set
                        .iter()
                        .map(|(def, v)| FieldChange {
                            field: format!("attributes.{}", def.key),
                            old: Value::Null,
                            new: stored_json(v),
                        })
                        .collect();
                    p.outcome = RowOutcome::Create;
                    p.create = Some(body);
                }
                Err(e) => {
                    plan_errors(&mut p, row, r, e);
                    return p;
                }
            }
            let title_label = title_value(r, &d).and_then(|t| labels.get(&t).cloned().flatten());
            p.label = title_label.clone().or_else(|| d.ident.clone());
            // Later rows may refer to this CI (T9). The dry run knows them all already.
            if !c.grow_pending {
            } else if let Some(l) = &title_label
                && let Some(cv) = label_canon.get(l)
            {
                c.pending.add(DimKey::Label, cv.clone(), row.number, id);
            } else if let Some(i) = &d.ident
                && let Some(cv) = label_canon.get(i)
            {
                c.pending.add(DimKey::Label, cv.clone(), row.number, id);
            }
            if c.grow_pending
                && let Some(i) = &d.ident
                && let Some(cv) = label_canon.get(i)
            {
                c.pending.add(DimKey::Ident, cv.clone(), row.number, id);
            }
            for (def, _, cell) in d.attrs.iter().filter(|_| c.grow_pending) {
                if let Cell::Value(v) = cell
                    && let Some(raw) = value_text(v)
                    && let Some(cv) = own_canon.get(&(*def, raw))
                {
                    c.pending.add(DimKey::Field(r.defs[*def].id), cv.clone(), row.number, id);
                }
            }
            if c.grow_pending
                && let Some(k) = &canon
            {
                c.pending.by_key.entry(k.clone()).or_insert(row.number);
            }
            resolver.cis.insert(id, r.class_id);
            row_ci = id;
            row_class = r.class_id;
            p.class_id = Some(r.class_id);
            p.ci_id = Some(id);
        }
        Some(Outcome::Update(id)) => {
            let Some(before) = befores.get(&id).cloned() else {
                fail(&mut p, key_col, key_field, "not_found", "The matched CI does not exist any more");
                return p;
            };
            let class = before.summary.class_id;
            let body = UpdateItemBody {
                class_id: None,
                ident: d.ident.clone().filter(|i| i.to_lowercase() != before.summary.ident.to_lowercase()),
                valid_from,
                valid_until,
                attributes: if attributes.is_empty() { None } else { Some(attributes) },
                version: None,
            };
            let class_defs = defs.get(&class).map(Vec::as_slice).unwrap_or_default();
            let registry_only = body.attributes.is_none()
                && body.ident.is_none()
                && body.valid_from.is_none()
                && body.valid_until.is_none();
            if registry_only {
                // Nothing mapped to change: the row only (maybe) adds relationships.
                if let Err(e) = c
                    .ctx
                    .require_class_visible(class, "Configuration item", id)
                    .and_then(|_| c.ctx.require_class(class, ClassOp::Edit))
                {
                    plan_errors(&mut p, row, r, e);
                    return p;
                }
                p.outcome = RowOutcome::Unchanged;
            } else {
                match plan::plan_update(c.ctx, model, class_defs, before.clone(), &body, &*resolver) {
                    Ok(planned) => {
                        p.changes = changes(&planned, &before);
                        if !planned.clear.is_empty() {
                            for cleared in &planned.clear {
                                if let Some(def) = class_defs.iter().find(|d| d.id == *cleared) {
                                    p.changes.push(FieldChange {
                                        field: format!("attributes.{}", def.key),
                                        old: before.attributes.get(&def.key).cloned().unwrap_or(Value::Null),
                                        new: Value::Null,
                                    });
                                }
                            }
                        }
                        p.outcome = if p.changes.is_empty() { RowOutcome::Unchanged } else { RowOutcome::Update };
                    }
                    Err(e) => {
                        plan_errors(&mut p, row, r, e);
                        return p;
                    }
                }
            }
            p.label = Some(before.summary.label.clone());
            p.ci_id = Some(id);
            p.class_id = Some(class);
            p.update = Some((before, body));
            row_ci = id;
            row_class = class;
        }
        None => return p,
    }

    // Relationships (§2.5): add what is missing, never remove.
    for (col_i, texts) in &d.rels {
        let col = &r.columns[*col_i];
        let Target::Relationship { type_id, direction, lookup, type_key } = &col.target else { continue };
        let field = Some(format!("relationships.{type_key}"));
        for text in texts {
            let (other, other_class) = match resolve_target(c, row.number, *col_i, lookup, text, targets, target_canon)
            {
                Ok(t) => t,
                Err((code, message)) => {
                    p.issues.push(Issue {
                        value: Some(text.clone()),
                        ..issue(row, Some(col), field.clone(), Severity::Error, code, &message)
                    });
                    continue;
                }
            };
            let (source, target, source_class, target_class) = match direction {
                RelationshipDirection::Outgoing => (row_ci, other, row_class, other_class),
                RelationshipDirection::Incoming => (other, row_ci, other_class, row_class),
            };
            if source == target {
                fail(&mut p, Some(col), field.clone(), "reference_self", "A CI cannot be related to itself");
                continue;
            }
            if !job.allows(*type_id, source_class, target_class) {
                fail(
                    &mut p,
                    Some(col),
                    field.clone(),
                    "relationship_not_allowed",
                    "This relationship type cannot connect these CIs",
                );
                continue;
            }
            if c.ctx.require_class(source_class, ClassOp::Edit).is_err() {
                fail(
                    &mut p,
                    Some(col),
                    field.clone(),
                    "forbidden",
                    "You do not have the edit permission on the source CI's class",
                );
                continue;
            }
            let directional = job.rules.get(type_id).is_none_or(|(d, _)| *d);
            let present = |s: &HashSet<(Uuid, Uuid, Uuid)>| {
                s.contains(&(*type_id, source, target)) || (!directional && s.contains(&(*type_id, target, source)))
            };
            if present(existing) || present(added) {
                p.edges_existing += 1;
            } else {
                added.insert((*type_id, source, target));
                p.edges.push((*type_id, source, target, *col_i));
            }
        }
    }
    if p.has_errors() {
        p.outcome = RowOutcome::Error;
        p.edges.clear();
    }
    p
}

fn title_value(r: &Resolved, d: &Draft) -> Option<String> {
    let t = r.title_field.as_ref()?;
    let i = r.defs.iter().position(|def| def.id == t.id)?;
    d.attrs.iter().find(|(def, _, _)| *def == i).and_then(|(_, _, cell)| match cell {
        Cell::Value(v) => value_text(v),
        _ => None,
    })
}

/// The CI a reference or relationship cell names: found in the database,
/// or created by an earlier row. Hidden and missing CIs look the same.
fn resolve_target(
    c: &Context<'_>,
    row: u32,
    col: usize,
    lookup: &Lookup,
    text: &str,
    targets: &HashMap<(usize, String), Vec<(Uuid, Uuid)>>,
    target_canon: &HashMap<(usize, DimKey, String), String>,
) -> Result<(Uuid, Uuid), (&'static str, String)> {
    let mut hits: Vec<(Uuid, Uuid)> = targets.get(&(col, text.to_owned())).cloned().unwrap_or_default();
    let mut later: Option<u32> = None;
    if lookup.pending {
        for dim in dims(lookup) {
            let Some(cv) = target_canon.get(&(col, dim.key(), text.to_owned())) else { continue };
            for (r, id) in c.pending.entries.get(&(dim.key(), cv.clone())).into_iter().flatten() {
                if *r < row {
                    if !hits.iter().any(|(h, _)| h == id) {
                        hits.push((*id, c.job.resolved.class_id));
                    }
                } else if *r > row {
                    later = Some(later.map_or(*r, |l| l.min(*r)));
                }
            }
        }
    }
    match hits.as_slice() {
        [one] => Ok(*one),
        [] => {
            if let Some(r) = later {
                return Err((
                    "reference_to_later_row",
                    format!("The CI on row {r} is created after this row. Move it up, or import it first."),
                ));
            }
            if lookup.pending && matches!(lookup.by, MatchBy::Ident | MatchBy::Label) {
                let canon = text.trim().to_lowercase();
                if let Some(r) = c.pending.by_key.get(&canon).filter(|r| **r < row) {
                    return Err((
                        "reference_to_pending_unresolvable",
                        format!(
                            "The CI on row {r} has no ident or label until it is imported. Match it by an attribute, or import it first."
                        ),
                    ));
                }
            }
            Err(("not_found", "Referenced CI does not exist or is deleted".into()))
        }
        many => Err((
            "ambiguous_reference",
            format!("{} CIs match this value. Match by a more specific value.", many.len()),
        )),
    }
}

/// Issues from the CI API's plan step: each field error on its column.
/// The column mapped to a field of the CI API (`attributes.<key>`, `ident`, …).
pub fn column_of_field<'r>(r: &'r Resolved, field: &str) -> Option<&'r Column> {
    r.columns.iter().find(|c| match &c.target {
        Target::Attribute { def, .. } => field == format!("attributes.{}", r.defs[*def].key),
        Target::Ident => field == "ident",
        Target::ValidFrom => field == "validFrom",
        Target::ValidUntil => field == "validUntil",
        Target::Relationship { .. } => false,
    })
}

fn plan_errors(p: &mut Planned, row: &Row, r: &Resolved, e: AppError) {
    let column_of = |field: &str| column_of_field(r, field);
    match (e.code, e.details) {
        (ErrorCode::ValidationError, Some(details)) if !details.is_empty() => {
            for f in details {
                p.issues.push(issue(
                    row,
                    column_of(&f.field),
                    Some(f.field.clone()),
                    Severity::Error,
                    &f.code,
                    &f.message,
                ));
            }
        }
        (ErrorCode::Forbidden, _) => {
            p.issues.push(issue(row, None, None, Severity::Error, "forbidden", &e.message));
        }
        (ErrorCode::Conflict | ErrorCode::NotFound, _) => {
            p.issues.push(issue(row, None, None, Severity::Error, "conflict", &e.message));
        }
        _ => p.issues.push(issue(row, None, None, Severity::Error, "invalid", &e.message)),
    }
    p.outcome = RowOutcome::Error;
}

// ---------------------------------------------------------------------------
// The dry run's first pass: keys and the CIs the file creates
// ---------------------------------------------------------------------------

/// What the first pass keeps of a row.
#[derive(Debug, Clone)]
pub struct RowKeys {
    pub number: u32,
    key_raw: Option<String>,
    ident: Option<String>,
    title: Option<String>,
    /// (definition index, raw value) of the class's matchable attributes.
    own: Vec<(usize, String)>,
}

/// The first pass over one chunk: no database, only the cells.
pub fn keys_of(job: &JobData, rows: &[Row]) -> Vec<RowKeys> {
    let r = &job.resolved;
    rows.iter()
        .map(|row| {
            let d = draft(job, row);
            let own = d
                .attrs
                .iter()
                .filter(|(def, _, _)| super::mapping::matchable(r.defs[*def].data_type))
                .filter_map(
                    |(def, _, cell)| if let Cell::Value(v) = cell { Some((*def, value_text(v)?)) } else { None },
                )
                .collect();
            RowKeys {
                number: row.number,
                key_raw: d.key_raw.clone(),
                ident: d.ident.clone(),
                title: title_value(r, &d),
                own,
            }
        })
        .collect()
}

/// The job-wide state of a dry run (§2.2, §2.5): the key index, the rows
/// sharing a key, the ids of the CIs rows create and how later rows find them,
/// and which rows match a deleted CI.
#[derive(Debug, Default)]
pub struct Whole {
    pub index: HashMap<String, Vec<(Uuid, Uuid)>>,
    pub duplicates: HashMap<String, Vec<u32>>,
    pub new_ids: HashMap<u32, Uuid>,
    pub pending: Pending,
    pub matches_deleted: HashSet<u32>,
}

/// Builds [`Whole`] from the first pass. Reads only the CIs whose keys occur
/// in the file, in batches (CR2).
pub async fn whole(
    conn: &mut PgConnection,
    job: &JobData,
    ctx: &RequestContext,
    keys: &[RowKeys],
) -> sqlx::Result<Whole> {
    let r = &job.resolved;
    let model = &job.model;
    let mut w = Whole::default();
    let import_classes: Vec<Uuid> =
        subtree(model, r.class_id).into_iter().filter(|cl| ctx.require_class(*cl, ClassOp::View).is_ok()).collect();

    let key_canon: HashMap<String, String> = match key_dim(r, model) {
        Some(dim) => {
            let raws: Vec<String> = keys.iter().filter_map(|k| k.key_raw.clone()).collect();
            let canon = canonical(conn, &dim, &raws).await?;
            let values: Vec<String> = canon.values().cloned().collect();
            for (cv, id, class) in find(conn, model, &dim, &values, &import_classes, false).await? {
                w.index.entry(cv).or_default().push((id, class));
            }
            // A deleted CI with this key, visible to the importer: a warning (§2.2, T12).
            let deleted: HashSet<String> = find(conn, model, &dim, &values, &import_classes, true)
                .await?
                .into_iter()
                .map(|(cv, _, _)| cv)
                .collect();
            for k in keys {
                if let Some(cv) = k.key_raw.as_ref().and_then(|raw| canon.get(raw))
                    && deleted.contains(cv)
                    && !w.index.contains_key(cv)
                {
                    w.matches_deleted.insert(k.number);
                }
            }
            canon
        }
        None => HashMap::new(),
    };
    let mut rows_of: HashMap<String, Vec<u32>> = HashMap::new();
    for k in keys {
        if let Some(cv) = k.key_raw.as_ref().and_then(|raw| key_canon.get(raw)) {
            rows_of.entry(cv.clone()).or_default().push(k.number);
        }
    }
    w.duplicates = rows_of.into_iter().filter(|(_, rows)| rows.len() > 1).collect();

    // The rows that create a CI, and how later rows can find it (T9).
    let creates: Vec<&RowKeys> = keys
        .iter()
        .filter(|k| {
            let cv = k.key_raw.as_ref().and_then(|raw| key_canon.get(raw));
            let dup = cv.is_some_and(|c| w.duplicates.contains_key(c));
            let matched = cv.is_some_and(|c| w.index.get(c).is_some_and(|h| !h.is_empty()));
            r.creates() && !dup && !matched && (k.key_raw.is_some() || !r.updates() || r.key.is_none())
        })
        .collect();
    let titles: Vec<String> =
        creates.iter().filter_map(|k| k.title.clone()).collect::<HashSet<_>>().into_iter().collect();
    let labels: HashMap<String, Option<String>> = match &r.title_field {
        Some(f) if !titles.is_empty() => {
            let mut out = HashMap::new();
            for batch in titles.chunks(BATCH) {
                let computed = items_data::labels_of(conn, f, batch).await?;
                out.extend(batch.iter().cloned().zip(computed));
            }
            out
        }
        _ => HashMap::new(),
    };
    let label_raws: Vec<String> =
        labels.values().flatten().cloned().chain(creates.iter().filter_map(|k| k.ident.clone())).collect();
    let label_canon = canonical(conn, &Dim::Label, &label_raws).await?;
    let mut own_canon: HashMap<(usize, String), String> = HashMap::new();
    let defs_used: HashSet<usize> = creates.iter().flat_map(|k| k.own.iter().map(|(d, _)| *d)).collect();
    for def in defs_used {
        let Some(f) = model.fields.iter().find(|f| f.id == r.defs[def].id) else { continue };
        let raws: Vec<String> =
            creates.iter().flat_map(|k| k.own.iter().filter(|(d, _)| *d == def).map(|(_, v)| v.clone())).collect();
        for (raw, cv) in canonical(conn, &Dim::Field(f.clone()), &raws).await? {
            own_canon.insert((def, raw), cv);
        }
    }
    for k in creates {
        let id = Uuid::new_v4();
        w.new_ids.insert(k.number, id);
        let label = k.title.as_ref().and_then(|t| labels.get(t).cloned().flatten()).or_else(|| k.ident.clone());
        if let Some(cv) = label.as_ref().and_then(|l| label_canon.get(l)) {
            w.pending.add(DimKey::Label, cv.clone(), k.number, id);
        }
        if let Some(cv) = k.ident.as_ref().and_then(|i| label_canon.get(i)) {
            w.pending.add(DimKey::Ident, cv.clone(), k.number, id);
        }
        for (def, raw) in &k.own {
            if let Some(cv) = own_canon.get(&(*def, raw.clone())) {
                w.pending.add(DimKey::Field(r.defs[*def].id), cv.clone(), k.number, id);
            }
        }
        if let Some(cv) = k.key_raw.as_ref().and_then(|raw| key_canon.get(raw)) {
            w.pending.by_key.entry(cv.clone()).or_insert(k.number);
        }
    }
    Ok(w)
}
