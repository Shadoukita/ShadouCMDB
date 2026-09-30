//! A job's mapping, checked against the data model and the caller's rights
//! (SHAA-714 §3.2, §4.1, §4.2), and resolved into what the planner needs.
//!
//! Classes, attributes and relationship types are named by key. A class the
//! caller may not import into is refused exactly like an unknown key
//! (`unknown_class`); rules whose other end holds only classes the caller may
//! not view count as not allowed.

use std::collections::HashSet;

use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use super::schemas::{
    ColumnTarget, DateFormat, EmptyCells, ImportMapping, ImportMode, MappingOptions, MatchBy, RelationshipDirection,
    TargetMatch,
};
use super::template::importable_class;
use crate::api::context::RequestContext;
use crate::auth::permissions::ClassOp;
use crate::data::classes::{self as class_data, EffectiveAttributeRow};
use crate::http::error::{AppError, FieldError, FieldLocation};
use crate::modules::classes::AttributeDataType;
use crate::schema::model::{Field, Model};

/// Data types a CI can be matched by (§2.2, §2.5).
pub fn matchable(t: AttributeDataType) -> bool {
    matches!(t, AttributeDataType::Text | AttributeDataType::Integer | AttributeDataType::Ip | AttributeDataType::Cidr)
}

/// How the other CI of a reference or relationship is found.
#[derive(Debug, Clone)]
pub struct Lookup {
    pub by: MatchBy,
    /// `by: attribute`: the fields with that key on the candidate classes.
    pub fields: Vec<Field>,
    /// The classes the other CI may be in (subclasses included), those the caller may view only.
    pub candidates: Vec<Uuid>,
    /// The CIs this import creates can be found this way (the import's class is a candidate).
    pub pending: bool,
}

#[derive(Debug, Clone)]
pub enum Target {
    /// An attribute of the class, by index into [`Resolved::defs`].
    Attribute {
        def: usize,
        reference: Option<Lookup>,
    },
    Relationship {
        type_id: Uuid,
        type_key: String,
        direction: RelationshipDirection,
        lookup: Lookup,
    },
    Ident,
    ValidFrom,
    ValidUntil,
}

#[derive(Debug, Clone)]
pub struct Column {
    pub index: u32,
    // Read by the commit (the next step of SHAA-799 part 4).
    #[allow(dead_code)]
    pub header: String,
    pub target: Target,
    pub empty_cells: EmptyCells,
    pub decimal_separator: char,
    pub date_format: DateFormat,
    pub time_zone: String,
}

/// How rows find existing CIs.
#[derive(Debug, Clone)]
pub enum Key {
    Ident,
    /// Index into [`Resolved::defs`].
    Attribute(usize),
}

/// A mapping ready for the planner.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub class_id: Uuid,
    // Read by the commit (the next step of SHAA-799 part 4).
    #[allow(dead_code)]
    pub class_key: String,
    pub mode: ImportMode,
    pub key: Option<Key>,
    /// The column the key is read from.
    pub key_column: Option<usize>,
    pub options: MappingOptions,
    pub columns: Vec<Column>,
    /// The class's effective attribute definitions.
    pub defs: Vec<EffectiveAttributeRow>,
    /// The fields of the class's lineage, by definition id order of `defs`.
    pub title_field: Option<Field>,
}

impl Resolved {
    pub fn creates(&self) -> bool {
        self.mode != ImportMode::UpdateOnly
    }
    pub fn updates(&self) -> bool {
        self.mode != ImportMode::CreateOnly
    }
}

fn error(field: String, message: impl Into<String>, code: &str) -> FieldError {
    FieldError { location: FieldLocation::Body, field, message: message.into(), code: code.into() }
}

/// The class and every class below it.
pub fn subtree(model: &Model, class_id: Uuid) -> Vec<Uuid> {
    model.classes.iter().filter(|c| model.lineage(c.id).iter().any(|a| a.id == class_id)).map(|c| c.id).collect()
}

/// `a` is `b` or below it.
pub fn is_a(model: &Model, a: Uuid, b: Uuid) -> bool {
    model.lineage(a).iter().any(|c| c.id == b)
}

fn visible(ctx: &RequestContext, classes: Vec<Uuid>) -> Vec<Uuid> {
    classes.into_iter().filter(|c| ctx.require_class(*c, ClassOp::View).is_ok()).collect()
}

/// Fields called `key` that CIs of `classes` have (own or inherited), matchable ones only.
fn fields_by_key(model: &Model, classes: &[Uuid], key: &str) -> Vec<Field> {
    let mut out: Vec<Field> = Vec::new();
    for c in classes {
        for a in model.lineage(*c) {
            for f in model.own_fields(a.id).filter(|f| f.key == key && matchable(f.data_type) && f.is_active) {
                if !out.iter().any(|o| o.id == f.id) {
                    out.push(f.clone());
                }
            }
        }
    }
    out
}

/// (source class, target class) of every rule of the type.
async fn rules_of(conn: &mut PgConnection, type_key: &str) -> sqlx::Result<Vec<(Uuid, Uuid)>> {
    sqlx::query_as(
        "SELECT r.source_class_id, r.target_class_id
         FROM cmdb.relationship_types t JOIN cmdb.relationship_type_rules r ON r.relationship_type_id = t.id
         WHERE t.key = $1",
    )
    .bind(type_key)
    .fetch_all(conn)
    .await
}

/// Checks a mapping for a file with `columns` columns (headers given) and
/// resolves it. All problems are reported at once, with the field they concern.
pub async fn resolve(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    mapping: &ImportMapping,
    headers: &[String],
) -> Result<Resolved, AppError> {
    let unknown = || {
        AppError::validation(vec![error(
            "classKey".into(),
            format!("CI class \"{}\" does not exist or cannot be imported into", mapping.class_key),
            "unknown_class",
        )])
    };
    let class_id = importable_class(conn, ctx, &mapping.class_key).await?.ok_or_else(unknown)?;
    let model = Model::load(conn).await?;
    let defs = class_data::effective_attributes(conn, class_id).await?;
    let is_admin = ctx.principal().is_some_and(|p| p.permissions.administrator);
    let mut errors: Vec<FieldError> = Vec::new();
    let options = &mapping.options;

    if options.list_separator.chars().count() != 1 {
        errors.push(error("options.listSeparator".into(), "Must be one character", "invalid_length"));
    }
    let mut zones: Vec<(String, String)> = vec![("options.timeZone".into(), options.time_zone.clone())];
    for (i, c) in mapping.columns.iter().enumerate() {
        if let Some(tz) = c.options.as_ref().and_then(|o| o.time_zone.clone()) {
            zones.push((format!("columns[{i}].options.timeZone"), tz));
        }
    }
    let names: Vec<String> = zones.iter().map(|(_, z)| z.clone()).collect();
    let known: Vec<String> = sqlx::query_scalar("SELECT name FROM pg_timezone_names WHERE name = ANY($1)")
        .bind(&names)
        .fetch_all(&mut *conn)
        .await?;
    for (field, zone) in &zones {
        if zone != "UTC" && !known.contains(zone) {
            errors.push(error(field.clone(), "Not an IANA time zone name", "invalid_time_zone"));
        }
    }

    let mut columns: Vec<Column> = Vec::new();
    let mut targets_seen: Vec<(String, usize)> = Vec::new();
    let mut seen_index: HashSet<u32> = HashSet::new();
    for (i, c) in mapping.columns.iter().enumerate() {
        let at = |f: &str| format!("columns[{i}].{f}");
        if c.index as usize >= headers.len() {
            errors.push(error(at("index"), "The file has no such column", "unknown_column"));
            continue;
        }
        if !seen_index.insert(c.index) {
            errors.push(error(at("index"), "This column is mapped twice", "duplicate_column"));
            continue;
        }
        let o = c.options.clone().unwrap_or_default();
        let decimal = o.decimal_separator.as_deref().unwrap_or(&options.decimal_separator);
        if decimal != "." && decimal != "," {
            errors.push(error(at("options.decimalSeparator"), "Must be . or ,", "invalid_enum"));
        }
        let target_id: String;
        let target = match &c.target {
            ColumnTarget::Ignore => continue,
            ColumnTarget::Ident => {
                target_id = "ident".into();
                if !is_admin && mapping.mode != ImportMode::UpdateOnly {
                    errors.push(error(
                        at("target.kind"),
                        "Only administrators can set the ident of new CIs. Map it only to match existing CIs.",
                        "ident_admin_only",
                    ));
                }
                Target::Ident
            }
            ColumnTarget::ValidFrom => {
                target_id = "validFrom".into();
                Target::ValidFrom
            }
            ColumnTarget::ValidUntil => {
                target_id = "validUntil".into();
                Target::ValidUntil
            }
            ColumnTarget::Attribute { key, match_ } => {
                target_id = format!("attributes.{key}");
                let Some(def) = defs.iter().position(|d| &d.key == key) else {
                    errors.push(error(
                        at("target.key"),
                        format!("Class \"{}\" has no attribute \"{key}\"", mapping.class_key),
                        "unknown_attribute",
                    ));
                    continue;
                };
                if !defs[def].is_active {
                    errors.push(error(at("target.key"), "This attribute is retired", "attribute_inactive"));
                    continue;
                }
                let reference = if defs[def].data_type == AttributeDataType::Reference {
                    let Some(m) = match_ else {
                        errors.push(error(at("target.match"), "Say how the referenced CI is found", "required"));
                        continue;
                    };
                    let class = defs[def].reference_class_id.unwrap_or_default();
                    let candidates = visible(ctx, subtree(&model, class));
                    match lookup(&model, m, candidates, class_id, &at("target.match")) {
                        Ok(l) => Some(l),
                        Err(e) => {
                            errors.push(e);
                            continue;
                        }
                    }
                } else {
                    None
                };
                Target::Attribute { def, reference }
            }
            ColumnTarget::Relationship { type_key, direction, match_ } => {
                target_id = format!("relationships.{type_key}.{direction:?}");
                let rules = rules_of(conn, type_key).await?;
                let type_row = sqlx::query_as::<_, (Uuid, bool)>(
                    "SELECT id, is_active FROM cmdb.relationship_types WHERE key = $1",
                )
                .bind(type_key)
                .fetch_optional(&mut *conn)
                .await?;
                let Some((type_id, active)) = type_row else {
                    errors.push(error(
                        at("target.typeKey"),
                        format!("Relationship type \"{type_key}\" does not exist"),
                        "unknown_relationship_type",
                    ));
                    continue;
                };
                // The other end: every class a rule allows opposite this class, and those below it.
                let mut other: Vec<Uuid> = Vec::new();
                for (source, target) in &rules {
                    let (mine, theirs) = match direction {
                        RelationshipDirection::Outgoing => (*source, *target),
                        RelationshipDirection::Incoming => (*target, *source),
                    };
                    if is_a(&model, class_id, mine) {
                        other.extend(subtree(&model, theirs));
                    }
                }
                other.sort();
                other.dedup();
                let candidates = visible(ctx, other);
                if !active || candidates.is_empty() {
                    errors.push(error(
                        at("target.typeKey"),
                        "This relationship type cannot connect this class in this direction",
                        "relationship_not_allowed",
                    ));
                    continue;
                }
                match lookup(&model, match_, candidates, class_id, &at("target.match")) {
                    Ok(lookup) => {
                        Target::Relationship { type_id, type_key: type_key.clone(), direction: *direction, lookup }
                    }
                    Err(e) => {
                        errors.push(e);
                        continue;
                    }
                }
            }
        };
        if let Some((_, first)) = targets_seen.iter().find(|(t, _)| *t == target_id) {
            errors.push(error(
                at("target"),
                format!("Column {} is already mapped to this target", first + 1),
                "duplicate_target",
            ));
            continue;
        }
        targets_seen.push((target_id, i));
        columns.push(Column {
            index: c.index,
            header: headers[c.index as usize].clone(),
            target,
            empty_cells: c.empty_cells.unwrap_or(mapping.empty_cells),
            decimal_separator: decimal.chars().next().unwrap_or('.'),
            date_format: o.date_format.unwrap_or(options.date_format),
            time_zone: o.time_zone.unwrap_or_else(|| options.time_zone.clone()),
        });
    }

    // The key (§2.2).
    let mut key = None;
    let mut key_column = None;
    // Only-create imports may name a key too: a row whose key exists is then `exists`.
    if mapping.mode != ImportMode::CreateOnly || mapping.key.is_some() {
        match mapping.key.as_ref().map(|k| k.field.as_str()) {
            None => errors.push(error("key".into(), "Say how rows find existing CIs", "key_unmapped")),
            Some("ident") => {
                key = Some(Key::Ident);
                key_column = columns.iter().position(|c| matches!(c.target, Target::Ident));
            }
            Some(field) => match field.strip_prefix("attributes.").and_then(|k| defs.iter().position(|d| d.key == k)) {
                None => errors.push(error("key.field".into(), "Not an attribute of the class", "unknown_attribute")),
                Some(def) if !matchable(defs[def].data_type) => errors.push(error(
                    "key.field".into(),
                    "CIs can be matched only by a text, integer, IP or CIDR attribute",
                    "key_not_matchable",
                )),
                Some(def) => {
                    key = Some(Key::Attribute(def));
                    key_column =
                        columns.iter().position(|c| matches!(c.target, Target::Attribute { def: d, .. } if d == def));
                }
            },
        }
        if key.is_some() && key_column.is_none() {
            errors.push(error("key.field".into(), "The key's column is not mapped", "key_unmapped"));
        }
    }

    // Required attributes, in the modes that create (§1.2).
    if mapping.mode != ImportMode::UpdateOnly {
        for (i, d) in defs.iter().enumerate() {
            let mapped = columns.iter().any(|c| matches!(c.target, Target::Attribute { def, .. } if def == i));
            if d.is_required && d.is_active && d.default_value.is_none() && !mapped {
                errors.push(error(
                    format!("attributes.{}", d.key),
                    format!("{} is required; map a column to it", d.label),
                    "required_unmapped",
                ));
            }
        }
    }

    if !errors.is_empty() {
        return Err(AppError::validation(errors));
    }
    Ok(Resolved {
        class_id,
        class_key: mapping.class_key.clone(),
        mode: mapping.mode,
        key,
        key_column,
        options: options.clone(),
        columns,
        title_field: model.title_field(class_id).cloned(),
        defs,
    })
}

fn lookup(
    model: &Model,
    m: &TargetMatch,
    candidates: Vec<Uuid>,
    import_class: Uuid,
    at: &str,
) -> Result<Lookup, FieldError> {
    let pending = candidates.contains(&import_class);
    let fields = match m.by {
        MatchBy::Attribute => {
            let Some(key) = m.attribute_key.as_deref() else {
                return Err(error(format!("{at}.attributeKey"), "Name the attribute to match by", "required"));
            };
            let fields = fields_by_key(model, &candidates, key);
            if fields.is_empty() {
                return Err(error(
                    format!("{at}.attributeKey"),
                    "No text, integer, IP or CIDR attribute of this key on the classes the other CI may be in",
                    "unknown_attribute",
                ));
            }
            fields
        }
        _ => Vec::new(),
    };
    Ok(Lookup { by: m.by, fields, candidates, pending })
}

/// The definition a saved mapping stores: the mapping without the class and
/// with columns named by header, so it applies to any file with those headers.
// Used by saved mappings (the next step of SHAA-799 part 4).
#[allow(dead_code)]
pub fn to_definition(mapping: &ImportMapping, headers: &[String]) -> Value {
    let mut v = serde_json::to_value(mapping).unwrap_or(Value::Null);
    if let Some(obj) = v.as_object_mut() {
        obj.remove("classKey");
        if let Some(Value::Array(cols)) = obj.get_mut("columns") {
            for c in cols.iter_mut() {
                if let Some(o) = c.as_object_mut() {
                    let index = o.remove("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                    o.insert("header".into(), Value::String(headers.get(index).cloned().unwrap_or_default()));
                }
            }
        }
    }
    v
}

/// A saved definition applied to a file: columns found by header (first
/// match); columns whose header the file lacks are left out.
#[allow(dead_code)]
pub fn from_definition(definition: &Value, class_key: &str, headers: &[String]) -> Option<ImportMapping> {
    let mut v = definition.clone();
    let obj = v.as_object_mut()?;
    obj.insert("classKey".into(), Value::String(class_key.to_owned()));
    if let Some(Value::Array(cols)) = obj.get_mut("columns") {
        cols.retain_mut(|c| {
            let Some(o) = c.as_object_mut() else { return false };
            let header = o.remove("header").and_then(|h| h.as_str().map(str::to_owned)).unwrap_or_default();
            match headers.iter().position(|h| *h == header) {
                Some(i) => {
                    o.insert("index".into(), Value::from(i as u64));
                    true
                }
                None => false,
            }
        });
    }
    serde_json::from_value(v).ok()
}
