//! Configuration-item rules: typed attribute values, class changes, optimistic
//! locking, soft delete with cascading edge removal, search and the graph.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value, json};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use ipnetwork::IpNetwork;

use super::schemas::{
    ActiveQuery, AttributeReference, ConfigurationItem, ConfigurationItemSummary, CreateItemBody, Graph,
    GraphDirection, GraphEdge, GraphEdgeType, GraphNode, GraphQuery, ItemFilterQuery, ListItemsQuery, SearchHit,
    SearchMatch, SearchQuery, SearchResults, UpdateItemBody,
};
use crate::api::context::RequestContext;
use crate::api::route::InvalidBody;
use crate::api::schemas::{LookupRef, Page, Paged};
use crate::api::{pg_error, validate};
use crate::auth::permissions::ClassOp;
use crate::data::classes::{self as class_data, EffectiveAttributeRow};
use crate::data::crud::{self, AuditAction, AuditEntry};
use crate::data::items::{
    self as data, ATTRIBUTE_SORT_PREFIX, ActiveFilter, Direction, ItemFilters, ListSort, SORT_FIELDS, StoredValue,
    SummaryRow,
};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::modules::classes::AttributeDataType;
use crate::schema::model::{Field, Model};

pub fn summary_dto(r: SummaryRow) -> ConfigurationItemSummary {
    ConfigurationItemSummary {
        id: r.id,
        ident: r.ident,
        label: r.label,
        class_id: r.class_id,
        class: LookupRef { id: r.class_id, key: r.class_key, name: r.class_name },
        valid_from: r.valid_from,
        valid_until: r.valid_until,
        active: r.active,
        version: r.version,
        created_at: r.created_at,
        updated_at: r.updated_at,
        deleted_at: r.deleted_at,
    }
}

// ---------------------------------------------------------------------------
// Attribute validation
// ---------------------------------------------------------------------------

/// The JSON Schema a value of an attribute must satisfy (same rules the
/// SHAA-3 API enforced), including the definition's extra validation. Also
/// checks attribute defaults.
pub fn value_schema(
    data_type: AttributeDataType,
    enum_values: Option<&[String]>,
    validation: Option<&Map<String, Value>>,
) -> Value {
    let rules = validation.cloned().unwrap_or_default();
    let mut range = Map::new();
    for (rule, keyword) in [("min", "minimum"), ("max", "maximum")] {
        if let Some(n) = rules.get(rule).filter(|n| n.is_number()) {
            range.insert(keyword.into(), n.clone());
        }
    }
    let with_range = |ty: &str| {
        let mut s = range.clone();
        s.insert("type".into(), ty.into());
        Value::Object(s)
    };
    match data_type {
        AttributeDataType::Text => {
            let mut s = json!({ "type": "string", "maxLength": rules.get("maxLength").and_then(Value::as_u64).unwrap_or(10_000) });
            if let Some(p) = rules.get("pattern").and_then(Value::as_str) {
                s["pattern"] = p.into();
            }
            s
        }
        AttributeDataType::Enum => {
            json!({ "enum": enum_values.unwrap_or_default() })
        }
        AttributeDataType::Number => with_range("number"),
        AttributeDataType::Integer => with_range("integer"),
        AttributeDataType::Boolean => json!({ "type": "boolean" }),
        AttributeDataType::Date => json!({ "type": "string", "format": "date" }),
        AttributeDataType::Datetime => json!({ "type": "string", "format": "date-time" }),
        AttributeDataType::Ip => json!({ "anyOf": [
            { "type": "string", "format": "ipv4" }, { "type": "string", "format": "ipv6" } ] }),
        AttributeDataType::Cidr => json!({ "anyOf": [
            { "type": "string", "format": "cidrv4" }, { "type": "string", "format": "cidrv6" } ] }),
        AttributeDataType::Reference | AttributeDataType::Lookup => json!({ "type": "string", "format": "uuid" }),
    }
}

fn to_stored(def: &EffectiveAttributeRow, v: &Value) -> Option<StoredValue> {
    use AttributeDataType as T;
    Some(match def.data_type {
        T::Text | T::Enum => StoredValue::Text(v.as_str()?.to_owned()),
        T::Number | T::Integer => StoredValue::Number(v.as_f64()?),
        T::Boolean => StoredValue::Boolean(v.as_bool()?),
        T::Date => StoredValue::Date(v.as_str()?.to_owned()),
        T::Datetime => StoredValue::Datetime(v.as_str()?.to_owned()),
        T::Ip => StoredValue::Ip(v.as_str()?.to_owned()),
        T::Cidr => StoredValue::Cidr(v.as_str()?.to_owned()),
        T::Reference => StoredValue::Reference(Uuid::parse_str(v.as_str()?).ok()?),
        T::Lookup => StoredValue::Lookup(Uuid::parse_str(v.as_str()?).ok()?),
    })
}

/// Which CIs a reference attribute may point at: live ones in `visible`
/// classes (the caller's view scope; `None` is every class). The value an
/// attribute definition already has in `current` (definition id -> target) is
/// no change: it is accepted as is, without the live and class checks, so
/// resending it tells nothing about a target the caller may not view (GH#269).
struct RefAccess<'a> {
    visible: Option<&'a [Uuid]>,
    current: Option<&'a HashMap<Uuid, Uuid>>,
}

/// The CI's reference values by attribute definition id, from its `attributes`.
fn current_refs(model: &Model, class_id: Uuid, attributes: &Map<String, Value>) -> HashMap<Uuid, Uuid> {
    model
        .lineage(class_id)
        .into_iter()
        .flat_map(|c| model.own_fields(c.id))
        .filter(|f| f.data_type == AttributeDataType::Reference)
        .filter_map(|f| Some((f.id, attributes.get(&f.key)?.as_str().and_then(|v| Uuid::parse_str(v).ok())?)))
        .collect()
}

struct Prepared<'d> {
    set: Vec<(&'d EffectiveAttributeRow, StoredValue)>,
    clear: Vec<Uuid>,
}

fn body_error(field: String, message: impl Into<String>, code: &str) -> FieldError {
    FieldError { location: FieldLocation::Body, field, message: message.into(), code: code.into() }
}

/// Validates attribute input against the class's effective definitions.
/// `lenient_clear` accepts `null` for keys the class does not define (they are
/// being cleared as part of a class change). A reference the caller may not
/// make (see [`RefAccess`]) fails exactly like one to a missing CI, so writes
/// are no existence oracle. A reference the attribute already has is left out
/// of the result: resending it changes nothing (see [`RefAccess`]).
async fn prepare_attributes<'d>(
    conn: &mut PgConnection,
    defs: &'d [EffectiveAttributeRow],
    input: Option<&Map<String, Value>>,
    class_key: &str,
    self_id: Option<Uuid>,
    lenient_clear: bool,
    access: RefAccess<'_>,
) -> Result<Prepared<'d>, AppError> {
    let by_key: HashMap<&str, &EffectiveAttributeRow> = defs.iter().map(|d| (d.key.as_str(), d)).collect();
    let mut errors = Vec::new();
    let mut prepared = Prepared { set: Vec::new(), clear: Vec::new() };
    let mut refs: Vec<(&EffectiveAttributeRow, Uuid)> = Vec::new();
    let mut lookups: Vec<(&EffectiveAttributeRow, Uuid)> = Vec::new();

    for (key, value) in input.into_iter().flatten() {
        let field = format!("attributes.{key}");
        let Some(def) = by_key.get(key.as_str()).copied() else {
            if !(lenient_clear && value.is_null()) {
                errors.push(body_error(
                    field,
                    format!("Class \"{class_key}\" has no attribute \"{key}\""),
                    "unknown_attribute",
                ));
            }
            continue;
        };
        if value.is_null() {
            prepared.clear.push(def.id);
            continue;
        }
        if !def.is_active {
            errors.push(body_error(
                field,
                "This attribute is retired and cannot receive new values",
                "attribute_inactive",
            ));
            continue;
        }
        let schema = value_schema(
            def.data_type,
            def.enum_values.as_ref().map(|v| v.0.as_slice()),
            def.validation.as_ref().map(|v| &v.0),
        );
        let problems = validate::check(&schema, value, FieldLocation::Body, None);
        if !problems.is_empty() {
            let pattern = def.validation.as_ref().and_then(|v| v.0.get("pattern")).and_then(Value::as_str);
            for mut p in problems {
                p.field = field.clone();
                if let (Some(pattern), "invalid_format") = (pattern, p.code.as_str()) {
                    p.message = validate::pattern_message(pattern)
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("Must match {pattern}"));
                }
                errors.push(p);
            }
            continue;
        }
        let Some(stored) = to_stored(def, value) else {
            errors.push(body_error(field, "Invalid input", "invalid_type"));
            continue;
        };
        match stored {
            StoredValue::Reference(id) if access.current.and_then(|c| c.get(&def.id)) == Some(&id) => continue,
            StoredValue::Reference(id) => refs.push((def, id)),
            StoredValue::Lookup(id) => lookups.push((def, id)),
            _ => {}
        }
        prepared.set.push((def, stored));
    }

    for (def, id) in lookups {
        let field = format!("attributes.{}", def.key);
        let list_id = def.lookup_list_id.unwrap_or_default();
        match class_data::lookup_value_state(conn, list_id, id).await? {
            Some(true) => {}
            Some(false) => errors.push(body_error(field, "This list value is retired", "lookup_value_inactive")),
            None => errors.push(body_error(field, "Not a value of this attribute's list", "not_found")),
        }
    }

    if !refs.is_empty() {
        let ids: Vec<Uuid> = refs.iter().map(|(_, id)| *id).collect();
        let live: HashMap<Uuid, Uuid> = data::live_items(conn, &ids).await?.into_iter().collect();
        for (def, id) in refs {
            let field = format!("attributes.{}", def.key);
            let allowed = live.get(&id).is_some_and(|class_id| is_visible(access.visible, *class_id));
            if Some(id) == self_id {
                errors.push(body_error(field, "A CI cannot reference itself", "reference_self"));
            } else if !allowed {
                errors.push(body_error(field, "Referenced CI does not exist or is deleted", "not_found"));
            }
        }
    }

    if errors.is_empty() { Ok(prepared) } else { Err(AppError::validation(errors)) }
}

fn is_visible(visible: Option<&[Uuid]>, class_id: Uuid) -> bool {
    visible.is_none_or(|classes| classes.contains(&class_id))
}

/// A reference must point at a CI of the field's class (or a subclass); the
/// foreign key only guarantees that the CI exists.
async fn check_reference_classes(conn: &mut PgConnection, prepared: &Prepared<'_>) -> Result<(), AppError> {
    let mut errors = Vec::new();
    for (def, value) in &prepared.set {
        let (StoredValue::Reference(target), Some(class)) = (value, def.reference_class_id) else { continue };
        let fits: Option<bool> =
            sqlx::query_scalar("SELECT cmdb.ci_class_is_a(class_id, $2) FROM cmdb.configuration_items WHERE id = $1")
                .bind(target)
                .bind(class)
                .fetch_optional(&mut *conn)
                .await?;
        if fits == Some(false) {
            errors.push(body_error(
                format!("attributes.{}", def.key),
                "Referenced CI is not of the class this field points at",
                "reference_class",
            ));
        }
    }
    if errors.is_empty() { Ok(()) } else { Err(AppError::validation(errors)) }
}

/// Dependent dropdowns: a value of a field with a parent field must belong to
/// the CI's value of that field (`current` holds the CI's values before this
/// write). Checked for the fields this write sets or clears and for those
/// whose parent field it sets or clears, so an unrelated edit of a CI whose
/// values predate the rule is not refused.
async fn check_parent_values(
    conn: &mut PgConnection,
    defs: &[EffectiveAttributeRow],
    prepared: &Prepared<'_>,
    current: Option<&Map<String, Value>>,
) -> Result<(), AppError> {
    let dependent: Vec<(&EffectiveAttributeRow, &EffectiveAttributeRow)> =
        defs.iter().filter_map(|d| Some((d, defs.iter().find(|p| Some(p.id) == d.parent_attribute_id)?))).collect();
    if dependent.is_empty() {
        return Ok(());
    }
    let mut value: HashMap<Uuid, Option<Uuid>> = HashMap::new();
    let mut touched: HashSet<Uuid> = prepared.clear.iter().copied().collect();
    for (def, stored) in &prepared.set {
        touched.insert(def.id);
        value.insert(def.id, if let StoredValue::Lookup(id) = stored { Some(*id) } else { None });
    }
    for id in &prepared.clear {
        value.insert(*id, None);
    }
    let value_of = |def: &EffectiveAttributeRow| -> Option<Uuid> {
        match value.get(&def.id) {
            Some(v) => *v,
            None => current?.get(&def.key)?.as_str().and_then(|s| Uuid::parse_str(s).ok()),
        }
    };
    let checks: Vec<(&EffectiveAttributeRow, &EffectiveAttributeRow, Uuid)> = dependent
        .into_iter()
        .filter(|(d, p)| touched.contains(&d.id) || touched.contains(&p.id))
        .filter_map(|(d, p)| Some((d, p, value_of(d)?)))
        .collect();
    if checks.is_empty() {
        return Ok(());
    }
    let ids: Vec<Uuid> = checks.iter().map(|(_, _, v)| *v).collect();
    let parents: HashMap<Uuid, Option<Uuid>> =
        sqlx::query_as("SELECT id, parent_value_id FROM lookup_list_values WHERE id = ANY($1)")
            .bind(&ids)
            .fetch_all(&mut *conn)
            .await?
            .into_iter()
            .collect();
    let mut errors = Vec::new();
    for (def, parent_def, child) in checks {
        let field = format!("attributes.{}", def.key);
        match value_of(parent_def) {
            None => errors.push(body_error(
                field,
                format!("Choose {} first; this value depends on it", parent_def.label),
                "lookup_parent_missing",
            )),
            Some(parent) if parents.get(&child).copied().flatten() != Some(parent) => errors.push(body_error(
                field,
                format!("Not a value of the chosen {}", parent_def.label),
                "lookup_parent_mismatch",
            )),
            Some(_) => {}
        }
    }
    if errors.is_empty() { Ok(()) } else { Err(AppError::validation(errors)) }
}

/// Attribute checks for a new CI: the values (defaults filled in), the classes
/// they reference, and that every required attribute has one.
async fn prepare_new<'d>(
    conn: &mut PgConnection,
    defs: &'d [EffectiveAttributeRow],
    input: Option<&Map<String, Value>>,
    class_key: &str,
    access: RefAccess<'_>,
) -> Result<Prepared<'d>, AppError> {
    let attributes = with_defaults(defs, input);
    let prepared = prepare_attributes(conn, defs, Some(&attributes), class_key, None, false, access).await?;
    check_reference_classes(conn, &prepared).await?;
    check_parent_values(conn, defs, &prepared, None).await?;
    let given: HashSet<Uuid> = prepared.set.iter().map(|(d, _)| d.id).collect();
    let missing: Vec<FieldError> = defs
        .iter()
        .filter(|d| d.is_required && d.is_active && !given.contains(&d.id))
        .map(|d| body_error(format!("attributes.{}", d.key), format!("{} is required", d.label), "required"))
        .collect();
    if missing.is_empty() { Ok(prepared) } else { Err(AppError::validation(missing)) }
}

// ---------------------------------------------------------------------------
// Bodies that failed validation
//
// A body that fails its schema never reaches create/update, but its attribute
// values can still be checked against the class. Reporting both at once saves
// the user a round trip per stage (GH#45). The attribute pass is best effort:
// without a usable class id, an existing CI, or the right to use the class,
// the body's own errors are the whole answer.
// ---------------------------------------------------------------------------

/// A uuid field of the raw body, unless the body's validation already rejected it.
fn raw_uuid(invalid: &InvalidBody, field: &str) -> Option<Uuid> {
    if invalid.errors.iter().any(|e| e.field == field) {
        return None;
    }
    invalid.raw.get(field)?.as_str().and_then(|s| Uuid::parse_str(s).ok())
}

/// The body's errors plus the attribute errors not already reported for the same field.
fn merged(mut errors: Vec<FieldError>, attributes: Result<(), AppError>) -> AppError {
    if let Err(AppError { code: ErrorCode::ValidationError, details: Some(more), .. }) = attributes {
        let seen: HashSet<String> = errors.iter().map(|e| e.field.clone()).collect();
        errors.extend(more.into_iter().filter(|e| !seen.contains(&e.field)));
    }
    AppError::validation(errors)
}

/// The 400 for a create body that failed validation.
pub async fn create_errors(pool: &PgPool, ctx: &RequestContext, invalid: InvalidBody) -> AppError {
    let attributes = invalid.raw.get("attributes").map(Value::as_object);
    let checked = match (raw_uuid(&invalid, "classId"), attributes) {
        // An "attributes" that is not an object is already reported.
        (Some(class_id), None | Some(Some(_))) if ctx.require_class(class_id, ClassOp::Create).is_ok() => {
            check_new_attributes(pool, ctx, class_id, attributes.flatten()).await
        }
        _ => Ok(()),
    };
    merged(invalid.errors, checked)
}

async fn check_new_attributes(
    pool: &PgPool,
    ctx: &RequestContext,
    class_id: Uuid,
    input: Option<&Map<String, Value>>,
) -> Result<(), AppError> {
    let mut conn = pool.acquire().await?;
    let key = class_key(&mut conn, class_id).await?;
    let defs = class_data::effective_attributes(&mut conn, class_id).await?;
    let visible = ctx.class_scope(ClassOp::View);
    let access = RefAccess { visible: visible.as_deref(), current: None };
    prepare_new(&mut conn, &defs, input, &key, access).await.map(drop)
}

/// The 400 for an update body that failed validation.
pub async fn update_errors(pool: &PgPool, ctx: &RequestContext, id: Uuid, invalid: InvalidBody) -> AppError {
    let checked = match invalid.raw.get("attributes").and_then(Value::as_object) {
        Some(attributes) => check_changed_attributes(pool, ctx, id, raw_uuid(&invalid, "classId"), attributes).await,
        None => Ok(()),
    };
    merged(invalid.errors, checked)
}

async fn check_changed_attributes(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    new_class: Option<Uuid>,
    input: &Map<String, Value>,
) -> Result<(), AppError> {
    let mut conn = pool.acquire().await?;
    // Outside a transaction the row lock ends with the statement.
    let Some(before) = data::lock(&mut conn, id).await? else { return Ok(()) };
    if before.deleted_at.is_some() || ctx.require_class(before.class_id, ClassOp::Edit).is_err() {
        return Ok(());
    }
    let class_id = new_class.unwrap_or(before.class_id);
    let class_changes = class_id != before.class_id;
    if class_changes && ctx.require_class(class_id, ClassOp::Create).is_err() {
        return Ok(());
    }
    let key = class_key(&mut conn, class_id).await?;
    let defs = class_data::effective_attributes(&mut conn, class_id).await?;
    // Same reference access as update(), so this 400 is no existence oracle either.
    let model = Model::load(&mut conn).await?;
    let current = must_detail(&mut conn, &model, id, None).await?.attributes;
    let refs = current_refs(&model, before.class_id, &current);
    let visible = ctx.class_scope(ClassOp::View);
    let access = RefAccess { visible: visible.as_deref(), current: Some(&refs) };
    let prepared = prepare_attributes(&mut conn, &defs, Some(input), &key, Some(id), class_changes, access).await?;
    check_reference_classes(&mut conn, &prepared).await?;
    check_parent_values(&mut conn, &defs, &prepared, Some(&current)).await
}

/// The input plus the default of every active attribute it leaves out.
fn with_defaults(defs: &[EffectiveAttributeRow], input: Option<&Map<String, Value>>) -> Map<String, Value> {
    let mut out = input.cloned().unwrap_or_default();
    for d in defs.iter().filter(|d| d.is_active) {
        if let Some(default) = &d.default_value
            && !out.contains_key(&d.key)
        {
            out.insert(d.key.clone(), default.0.clone());
        }
    }
    out
}

/// A database error while writing type rows, attributed to the field it concerns.
fn field_write_error(err: sqlx::Error, model: &Model) -> AppError {
    let Some(db) = err.as_database_error() else { return err.into() };
    let pg = db.try_downcast_ref::<sqlx::postgres::PgDatabaseError>();
    let column = pg.and_then(|p| p.column()).map(str::to_owned);
    let constraint = db.constraint().map(str::to_owned);
    // ck_<field id hex>_<hash>, fk_<field id hex>
    let by_constraint = constraint.as_deref().and_then(|c| {
        let hex = c.strip_prefix("ck_").or_else(|| c.strip_prefix("fk_"))?.get(..32)?;
        model.fields.iter().find(|f| f.hex() == hex).map(|f| f.key.clone())
    });
    match by_constraint.or(column) {
        Some(key) => pg_error::map(&err, Some(&format!("attributes.{key}"))).unwrap_or_else(|| err.into()),
        None => pg_error::map(&err, None).unwrap_or_else(|| err.into()),
    }
}

/// A database error while writing the registry row (ident taken, validity order).
fn registry_write_error(err: sqlx::Error) -> AppError {
    pg_error::map(&err, None).unwrap_or_else(|| err.into())
}

/// Writes the CI's rows in its type tables: `insert` for tables it has no row
/// in yet, `update` (only the columns that change) for the others.
async fn write_type_rows(
    conn: &mut PgConnection,
    model: &Model,
    ci_id: Uuid,
    class_id: Uuid,
    set: &[(&EffectiveAttributeRow, StoredValue)],
    clear: &[Uuid],
    new_tables: &[Uuid],
) -> Result<(), AppError> {
    for class in model.lineage(class_id) {
        let Some(table) = model.table(class.id) else { continue };
        let mut values: Vec<(&Field, Option<String>)> = Vec::new();
        for f in model.own_fields(class.id) {
            if let Some((_, v)) = set.iter().find(|(d, _)| d.id == f.id) {
                values.push((f, Some(v.as_text())));
            } else if clear.contains(&f.id) {
                values.push((f, None));
            }
        }
        let result = if new_tables.contains(&class.id) {
            data::insert_type_row(conn, &table, ci_id, &values).await
        } else {
            data::update_type_row(conn, &table, ci_id, &values).await
        };
        result.map_err(|e| field_write_error(e, model))?;
    }
    Ok(())
}

async fn check_required(
    conn: &mut PgConnection,
    model: &Model,
    ci_id: Uuid,
    defs: &[EffectiveAttributeRow],
) -> Result<(), AppError> {
    let present: HashSet<String> = data::values(conn, model, &[ci_id]).await?.into_iter().map(|v| v.key).collect();
    let missing: Vec<FieldError> = defs
        .iter()
        .filter(|d| d.is_required && d.is_active && !present.contains(&d.key))
        .map(|d| body_error(format!("attributes.{}", d.key), format!("{} is required", d.label), "required"))
        .collect();
    if missing.is_empty() { Ok(()) } else { Err(AppError::validation(missing)) }
}

async fn class_key(conn: &mut PgConnection, class_id: Uuid) -> Result<String, AppError> {
    match class_data::class_info(conn, class_id).await? {
        Some(c) => Ok(c.key),
        None => Err(AppError::field("classId", "CI class does not exist", "not_found")),
    }
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

async fn filters(conn: &mut PgConnection, model: &Model, q: &impl ItemFilterQuery) -> Result<ItemFilters, AppError> {
    let class_ids = match q.class_id() {
        Some(ids) if q.include_subclasses() => Some(class_data::with_descendant_classes(conn, &ids.0).await?),
        Some(ids) => Some(ids.0.clone()),
        None => None,
    };
    // Values grouped by list: any value of a list, and every list.
    let mut lookups: Vec<(Vec<Uuid>, data::LookupColumns)> = Vec::new();
    if let Some(ids) = q.lookup_value_id() {
        let known = data::lookup_value_lists(conn, &ids.0).await?;
        let mut by_list: Vec<(Uuid, Vec<Uuid>)> = Vec::new();
        for id in &ids.0 {
            match known.iter().find(|(v, _)| v == id) {
                Some((_, list)) => match by_list.iter_mut().find(|(l, _)| l == list) {
                    Some((_, values)) => values.push(*id),
                    None => by_list.push((*list, vec![*id])),
                },
                // An unknown value matches no CI.
                None => lookups.push((vec![*id], Vec::new())),
            }
        }
        lookups.extend(by_list.into_iter().map(|(list, values)| (values, model.lookup_columns(list))));
    }
    Ok(ItemFilters {
        q: None,
        class_ids,
        active: match q.active() {
            ActiveQuery::True => ActiveFilter::Active,
            ActiveQuery::False => ActiveFilter::Inactive,
            ActiveQuery::All => ActiveFilter::Any,
        },
        lookups,
        ip_within: q.ip_within().map(str::to_owned),
        deleted: Some(q.deleted()),
        visible_class_ids: None,
        // Also where ipWithin looks.
        search_tables: data::search_tables(model),
    })
}

fn sort_error(message: String, code: &str) -> AppError {
    AppError::validation(vec![FieldError {
        location: FieldLocation::Query,
        field: "sort".into(),
        message,
        code: code.into(),
    }])
}

/// `attributes.<key>` sorts on the field of that key that every requested
/// class has (its own or inherited), so it needs `classId`. CIs of
/// subclasses have a row in that field's table too.
fn list_sort<'m>(model: &'m Model, q: &ListItemsQuery) -> Result<ListSort<'m>, AppError> {
    let Some(key) = q.sort.field.strip_prefix(ATTRIBUTE_SORT_PREFIX) else {
        return Ok(ListSort::Core(SORT_FIELDS.iter().find(|f| **f == q.sort.field).copied().unwrap_or("label")));
    };
    let Some(class_ids) = q.class_id() else {
        return Err(sort_error("Sorting by an attribute needs classId".into(), "class_required"));
    };
    let mut found: Option<&Field> = None;
    for class_id in &class_ids.0 {
        let field = model.lineage(*class_id).into_iter().find_map(|c| model.own_fields(c.id).find(|f| f.key == key));
        match (field, found) {
            (None, _) => {
                return Err(sort_error(
                    format!("Attribute \"{key}\" is not defined on every class in classId"),
                    "unknown_attribute",
                ));
            }
            (Some(a), Some(b)) if a.id != b.id => {
                return Err(sort_error(
                    format!("Attribute \"{key}\" is a different attribute in the classes of classId"),
                    "ambiguous_attribute",
                ));
            }
            (Some(a), _) => found = Some(a),
        }
    }
    let Some(field) = found else { return Err(AppError::internal()) };
    if !data::is_sortable(field.data_type) {
        return Err(sort_error(format!("Attribute \"{key}\" is a reference and cannot be sorted on"), "not_sortable"));
    }
    match model.table(field.class_id) {
        Some(table) => Ok(ListSort::Attribute(table, field)),
        None => Err(AppError::internal()),
    }
}

/// Only CIs of classes the caller may view.
pub async fn list(
    pool: &PgPool,
    ctx: &RequestContext,
    q: &ListItemsQuery,
) -> Result<Page<ConfigurationItem>, AppError> {
    let mut conn = pool.acquire().await?;
    let model = Model::load(&mut conn).await?;
    let sort = list_sort(&model, q)?;
    let f = ItemFilters {
        q: q.q.clone(),
        visible_class_ids: ctx.class_scope(ClassOp::View),
        ..filters(&mut conn, &model, q).await?
    };
    let (rows, total) = data::list(&mut conn, &f, sort, q.sort.desc, q.limit, q.offset).await?;
    let data = with_attributes(&mut conn, &model, rows, f.visible_class_ids.as_deref()).await?;
    Ok(Page { data, page: q.page_meta(total) })
}

pub async fn search(pool: &PgPool, ctx: &RequestContext, q: &SearchQuery) -> Result<SearchResults, AppError> {
    let mut conn = pool.acquire().await?;
    let model = Model::load(&mut conn).await?;
    let f = ItemFilters { visible_class_ids: ctx.class_scope(ClassOp::View), ..filters(&mut conn, &model, q).await? };
    let (rows, total) = data::search(&mut conn, &q.q, &f, q.limit, q.offset).await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let values = data::values(&mut conn, &model, &ids).await?;

    let needle = q.q.to_lowercase();
    let hit = |s: &str| s.to_lowercase().contains(&needle);
    let q_net: Option<IpNetwork> = validate::is_ip_or_cidr(&q.q).then(|| q.q.parse().ok()).flatten();
    let in_q_net = |text: &str| {
        let ip = text.parse::<IpNetwork>().ok().map(|n| n.ip());
        q_net.zip(ip).is_some_and(|(net, ip)| net.contains(ip))
    };

    let data = rows
        .into_iter()
        .map(|r| {
            let item = summary_dto(r);
            let mut matches = Vec::new();
            let mut add = |field: &str, label: &str, value: &str| {
                matches.push(SearchMatch { field: field.into(), label: label.into(), value: value.into() })
            };
            if hit(&item.label) {
                add("label", "Label", &item.label);
            }
            if hit(&item.ident) {
                add("ident", "Ident", &item.ident);
            }
            for v in values.iter().filter(|v| v.ci_id == item.id) {
                let is_net = matches!(v.data_type, AttributeDataType::Ip | AttributeDataType::Cidr);
                if let Some(text) = v.search_text()
                    && (hit(text) || (is_net && (text.starts_with(&q.q) || in_q_net(text))))
                {
                    let shown = if text.chars().count() > 200 {
                        format!("{}…", text.chars().take(200).collect::<String>())
                    } else {
                        text.to_owned()
                    };
                    add(&format!("attributes.{}", v.key), &v.label, &shown);
                }
            }
            SearchHit { item, matches }
        })
        .collect();
    Ok(SearchResults { data, page: q.page_meta(total) })
}

/// Summary rows plus their field values and reference names (batched reads for
/// the page). References into classes outside `visible` (the reader's view
/// scope; `None` is every class, as for audit rows) come back hidden.
async fn with_attributes(
    conn: &mut PgConnection,
    model: &Model,
    rows: Vec<SummaryRow>,
    visible: Option<&[Uuid]>,
) -> Result<Vec<ConfigurationItem>, AppError> {
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let values = data::values(conn, model, &ids).await?;
    let refs: Vec<Uuid> = values.iter().filter_map(|v| v.reference()).collect();
    let names = data::reference_names(conn, &refs).await?;
    let mut items: Vec<ConfigurationItem> = rows
        .into_iter()
        .map(|row| ConfigurationItem {
            summary: summary_dto(row),
            attributes: Map::new(),
            attribute_references: Map::new(),
        })
        .collect();
    let index: HashMap<Uuid, usize> = items.iter().enumerate().map(|(i, item)| (item.summary.id, i)).collect();
    for v in values {
        let Some(&i) = index.get(&v.ci_id) else { continue };
        let item = &mut items[i];
        if let Some(ref_id) = v.reference() {
            let reference = match names.get(&ref_id) {
                Some(r) if is_visible(visible, r.class_id) => {
                    AttributeReference { id: ref_id, name: Some(r.label.clone()), deleted: r.deleted, hidden: false }
                }
                _ => AttributeReference { id: ref_id, name: None, deleted: false, hidden: true },
            };
            item.attribute_references.insert(v.key.clone(), crud::json(&reference));
        }
        item.attributes.insert(v.key, v.value);
    }
    Ok(items)
}

/// Full representations (values included) of these CIs, deleted ones included.
/// Unredacted: for audit records only.
pub async fn details(conn: &mut PgConnection, model: &Model, ids: &[Uuid]) -> Result<Vec<ConfigurationItem>, AppError> {
    let rows = data::summaries(conn, ids).await?;
    with_attributes(conn, model, rows, None).await
}

async fn detail(
    conn: &mut PgConnection,
    model: &Model,
    id: Uuid,
    visible: Option<&[Uuid]>,
) -> Result<Option<ConfigurationItem>, AppError> {
    let Some(row) = data::summary(conn, id).await? else { return Ok(None) };
    Ok(with_attributes(conn, model, vec![row], visible).await?.pop())
}

async fn must_detail(
    conn: &mut PgConnection,
    model: &Model,
    id: Uuid,
    visible: Option<&[Uuid]>,
) -> Result<ConfigurationItem, AppError> {
    detail(conn, model, id, visible).await?.ok_or_else(|| AppError::missing("Configuration item", id))
}

/// The written CI as the caller may see it: `full` (the unredacted audit
/// value) unless the caller's view scope is limited.
async fn response_detail(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    model: &Model,
    full: ConfigurationItem,
) -> Result<ConfigurationItem, AppError> {
    match ctx.class_scope(ClassOp::View) {
        None => Ok(full),
        Some(visible) => must_detail(conn, model, full.summary.id, Some(&visible)).await,
    }
}

pub async fn get(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<ConfigurationItem, AppError> {
    let mut conn = pool.acquire().await?;
    let Some(row) = data::summary(&mut conn, id).await? else {
        return Err(AppError::missing("Configuration item", id));
    };
    ctx.require_class_visible(row.class_id, "Configuration item", id)?;
    let model = Model::load(&mut conn).await?;
    let visible = ctx.class_scope(ClassOp::View);
    let dto = with_attributes(&mut conn, &model, vec![row], visible.as_deref()).await?.pop();
    dto.ok_or_else(|| AppError::missing("Configuration item", id))
}

// ---------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------

pub async fn create(
    pool: &PgPool,
    ctx: &RequestContext,
    input: &CreateItemBody,
) -> Result<ConfigurationItem, AppError> {
    ctx.require_class(input.class_id, ClassOp::Create)?;
    if input.ident.is_some() {
        ctx.require_administrator("set a CI's ident")?;
    }
    let mut tx = pool.begin().await?;
    let key = class_key(&mut tx, input.class_id).await?;
    let model = Model::load(&mut tx).await?;
    let defs = class_data::effective_attributes(&mut tx, input.class_id).await?;
    let visible = ctx.class_scope(ClassOp::View);
    let access = RefAccess { visible: visible.as_deref(), current: None };
    let prepared = prepare_new(&mut tx, &defs, input.attributes.as_ref(), &key, access).await?;

    let new = data::NewItem {
        class_id: input.class_id,
        ident: input.ident.as_deref(),
        valid_from: input.valid_from,
        valid_until: input.valid_until,
    };
    let id = data::insert(&mut tx, &new).await.map_err(registry_write_error)?;
    let lineage: Vec<Uuid> = model.lineage(input.class_id).iter().map(|c| c.id).collect();
    write_type_rows(&mut tx, &model, id, input.class_id, &prepared.set, &[], &lineage).await?;
    data::refresh_labels(&mut tx, &model, &[input.class_id], Some(&[id])).await?;

    let dto = must_detail(&mut tx, &model, id, None).await?;
    let entry = AuditEntry {
        action: AuditAction::Create,
        entity_type: "configuration_items",
        entity_id: id,
        old_value: None,
        new_value: Some(crud::json(&dto)),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    let dto = response_detail(&mut tx, ctx, &model, dto).await?;
    tx.commit().await?;
    Ok(dto)
}

pub async fn update(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    input: &UpdateItemBody,
) -> Result<ConfigurationItem, AppError> {
    let mut tx = pool.begin().await?;
    let before = data::lock(&mut tx, id).await?.ok_or_else(|| AppError::missing("Configuration item", id))?;
    ctx.require_class_visible(before.class_id, "Configuration item", id)?;
    ctx.require_class(before.class_id, ClassOp::Edit)?;
    // Moving a CI to another class also needs create rights there.
    if let Some(new_class) = input.class_id.filter(|c| *c != before.class_id) {
        ctx.require_class(new_class, ClassOp::Create)?;
    }
    if before.deleted_at.is_some() {
        return Err(AppError::conflict("This configuration item is deleted and cannot be modified"));
    }
    // Resending the current ident (a form saving every field) is no change.
    let new_ident = input.ident.as_deref().filter(|i| *i != before.ident);
    if new_ident.is_some() {
        ctx.require_administrator("change a CI's ident")?;
    }
    if let Some(sent) = input.version
        && sent != before.version
    {
        return Err(AppError::new(
            ErrorCode::VersionConflict,
            format!(
                "The item was changed by someone else (you sent version {sent}, current is {}). Reload and retry.",
                before.version
            ),
        )
        .with_details(vec![body_error(
            "version".into(),
            format!("Current version is {}", before.version),
            "stale",
        )]));
    }
    let model = Model::load(&mut tx).await?;
    let before_dto = must_detail(&mut tx, &model, id, None).await?;

    let class_id = input.class_id.unwrap_or(before.class_id);
    let class_changes = class_id != before.class_id;
    let key = class_key(&mut tx, class_id).await?;
    let defs = class_data::effective_attributes(&mut tx, class_id).await?;
    let refs = current_refs(&model, before.class_id, &before_dto.attributes);
    let visible = ctx.class_scope(ClassOp::View);
    let access = RefAccess { visible: visible.as_deref(), current: Some(&refs) };
    let prepared =
        prepare_attributes(&mut tx, &defs, input.attributes.as_ref(), &key, Some(id), class_changes, access).await?;
    check_reference_classes(&mut tx, &prepared).await?;
    check_parent_values(&mut tx, &defs, &prepared, Some(&before_dto.attributes)).await?;
    let old_lineage: Vec<Uuid> = model.lineage(before.class_id).iter().map(|c| c.id).collect();
    let new_lineage: Vec<Uuid> = model.lineage(class_id).iter().map(|c| c.id).collect();

    if class_changes {
        // Values the new class does not define must be cleared in the same request.
        let keep: HashSet<&str> = defs.iter().map(|d| d.key.as_str()).collect();
        let cleared: HashSet<&str> =
            input.attributes.iter().flatten().filter(|(_, v)| v.is_null()).map(|(k, _)| k.as_str()).collect();
        let orphaned: Vec<&str> = before_dto
            .attributes
            .keys()
            .map(String::as_str)
            .filter(|k| !keep.contains(k) && !cleared.contains(k))
            .collect();
        if let Some(first) = orphaned.first() {
            return Err(AppError::field(
                "classId",
                format!(
                    "The new class does not define: {}. Clear them in the same request (\"attributes\": {{\"{first}\": null}}).",
                    orphaned.join(", ")
                ),
                "attributes_outside_class",
            ));
        }
        // Rows in the tables of classes the CI leaves go (their values were cleared above).
        for gone in old_lineage.iter().filter(|c| !new_lineage.contains(c)) {
            if let Some(table) = model.table(*gone) {
                data::delete_type_rows(&mut tx, &table, &[id]).await?;
            }
        }
    }

    let patch = data::ItemPatch {
        class_id: input.class_id,
        ident: new_ident,
        valid_from: input.valid_from,
        valid_until: input.valid_until,
    };
    data::update(&mut tx, id, &patch).await.map_err(registry_write_error)?;
    let entering: Vec<Uuid> = new_lineage.iter().filter(|c| !old_lineage.contains(c)).copied().collect();
    write_type_rows(&mut tx, &model, id, class_id, &prepared.set, &prepared.clear, &entering).await?;
    check_required(&mut tx, &model, id, &defs).await?;
    data::refresh_labels(&mut tx, &model, &[class_id], Some(&[id])).await?;

    let dto = must_detail(&mut tx, &model, id, None).await?;
    let entry = AuditEntry {
        action: AuditAction::Update,
        entity_type: "configuration_items",
        entity_id: id,
        old_value: Some(crud::json(&before_dto)),
        new_value: Some(crud::json(&dto)),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    let dto = response_detail(&mut tx, ctx, &model, dto).await?;
    tx.commit().await?;
    Ok(dto)
}

/// Soft delete: the CI and its live relationships get deleted_at; history keeps resolving.
pub async fn remove(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    match data::lock(&mut tx, id).await? {
        Some(row) if row.deleted_at.is_none() => {
            ctx.require_class_visible(row.class_id, "Configuration item", id)?;
            ctx.require_class(row.class_id, ClassOp::Delete)?
        }
        _ => return Err(AppError::missing("Configuration item", id)),
    }
    let model = Model::load(&mut tx).await?;
    let before = must_detail(&mut tx, &model, id, None).await?;
    let edges = data::soft_delete_edges_of(&mut tx, id).await?;
    data::soft_delete(&mut tx, id).await?;

    let mut entries: Vec<AuditEntry> = edges
        .iter()
        .map(|e| AuditEntry {
            action: AuditAction::Delete,
            entity_type: "ci_relationships",
            entity_id: e.id,
            old_value: Some(crud::json(e)),
            new_value: None,
        })
        .collect();
    entries.push(AuditEntry {
        action: AuditAction::Delete,
        entity_type: "configuration_items",
        entity_id: id,
        old_value: Some(crud::json(&before)),
        new_value: None,
    });
    crud::write_audit(&mut tx, ctx, entries).await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Graph
// ---------------------------------------------------------------------------

/// Relationship rows a graph request may read per `maxNodes`, over all hops.
const GRAPH_EDGES_PER_NODE: usize = 5;

/// Keeps at most `budget` of the rows read (fetched with `budget + 1`) and
/// flags the graph as truncated when rows had to be dropped.
fn spend_edge_budget(mut found: Vec<data::EdgeRow>, budget: &mut usize, truncated: &mut bool) -> Vec<data::EdgeRow> {
    if found.len() > *budget {
        found.truncate(*budget);
        *truncated = true;
    }
    *budget -= found.len();
    found
}

/// Breadth-first expansion from a root CI: one query per hop (not per CI),
/// then one query for all node summaries. The traversal never enters CIs of
/// classes the caller may not view.
pub async fn graph(pool: &PgPool, ctx: &RequestContext, root_id: Uuid, q: &GraphQuery) -> Result<Graph, AppError> {
    let mut conn = pool.acquire().await?;
    let Some(root) = data::summary(&mut conn, root_id).await? else {
        return Err(AppError::missing("Configuration item", root_id));
    };
    ctx.require_class_visible(root.class_id, "Configuration item", root_id)?;
    let visible = ctx.class_scope(ClassOp::View);
    let visible = visible.as_deref();
    let direction = match q.direction {
        GraphDirection::Both => Direction::Both,
        GraphDirection::Outgoing => Direction::Outgoing,
        GraphDirection::Incoming => Direction::Incoming,
    };
    let types = q.relationship_type_id.as_ref().map(|l| l.0.as_slice());
    let max_nodes = q.max_nodes as usize;

    let mut depth_of: HashMap<Uuid, i32> = HashMap::from([(root_id, 0)]);
    let mut edge_ids: HashSet<Uuid> = HashSet::new();
    let mut edges: Vec<data::EdgeRow> = Vec::new();
    let mut frontier = vec![root_id];
    let mut truncated = false;

    let mut keep_edge = |e: &data::EdgeRow, depth_of: &HashMap<Uuid, i32>, edges: &mut Vec<data::EdgeRow>| {
        if depth_of.contains_key(&e.source_ci_id) && depth_of.contains_key(&e.target_ci_id) && edge_ids.insert(e.id) {
            edges.push(e.clone());
        }
    };

    // Edge rows read from the database over the whole traversal. maxNodes
    // alone does not bound them: a hub CI can have tens of thousands of
    // relationships (GH#185).
    let mut edge_budget = max_nodes * GRAPH_EDGES_PER_NODE;

    let mut hop = 1;
    while hop <= q.depth && !frontier.is_empty() {
        // One row over the budget tells a cut-off result from an exact fit.
        let found =
            data::edges_touching(&mut conn, &frontier, direction, types, visible, edge_budget as i64 + 1).await?;
        let found = spend_edge_budget(found, &mut edge_budget, &mut truncated);
        let mut next = Vec::new();
        for e in &found {
            for other in [e.source_ci_id, e.target_ci_id] {
                if depth_of.contains_key(&other) {
                    continue;
                }
                if depth_of.len() >= max_nodes {
                    truncated = true;
                    continue;
                }
                depth_of.insert(other, hop);
                next.push(other);
            }
            keep_edge(e, &depth_of, &mut edges);
        }
        frontier = next;
        hop += 1;
    }
    // Edges between nodes discovered at the last hop (e.g. app -> db when both
    // hang off the same server) are included too, so the picture is complete.
    if !frontier.is_empty() {
        let found =
            data::edges_touching(&mut conn, &frontier, Direction::Both, types, visible, edge_budget as i64 + 1).await?;
        for e in spend_edge_budget(found, &mut edge_budget, &mut truncated) {
            keep_edge(&e, &depth_of, &mut edges);
        }
    }

    let ids: Vec<Uuid> = depth_of.keys().copied().collect();
    let mut nodes: Vec<GraphNode> = data::summaries(&mut conn, &ids)
        .await?
        .into_iter()
        .map(|s| {
            let depth = depth_of.get(&s.id).copied().unwrap_or(0);
            GraphNode { summary: summary_dto(s), depth }
        })
        .collect();
    nodes.sort_by(|a, b| {
        a.depth
            .cmp(&b.depth)
            .then_with(|| a.summary.label.to_lowercase().cmp(&b.summary.label.to_lowercase()))
            .then_with(|| a.summary.label.cmp(&b.summary.label))
    });

    Ok(Graph {
        root_id,
        depth: q.depth,
        direction: q.direction,
        nodes,
        edges: edges
            .into_iter()
            .map(|e| GraphEdge {
                id: e.id,
                relationship_type_id: e.relationship_type_id,
                edge_type: GraphEdgeType {
                    key: e.type_key,
                    name: e.type_name,
                    forward_label: e.forward_label,
                    reverse_label: e.reverse_label,
                    is_directional: e.is_directional,
                },
                source_ci_id: e.source_ci_id,
                target_ci_id: e.target_ci_id,
                notes: e.notes,
            })
            .collect(),
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::route::{BodyInput, CheckedBody};
    use crate::db::scratch;

    async fn id_of(pool: &PgPool, table: &str, key: &str) -> Uuid {
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT id FROM {table} WHERE key = $1")))
            .bind(key)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn status(pool: &PgPool, key: &str) -> Uuid {
        sqlx::query_scalar(
            "SELECT v.id FROM lookup_list_values v JOIN lookup_lists l ON l.id = v.list_id
             WHERE l.key = 'status' AND v.key = $1",
        )
        .bind(key)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    fn fields(err: &AppError) -> Vec<&str> {
        let mut f: Vec<&str> = err.details.iter().flatten().map(|d| d.field.as_str()).collect();
        f.sort_unstable();
        f
    }

    fn parse<T: crate::api::route::Check + serde::de::DeserializeOwned + utoipa::ToSchema + Send + 'static>(
        v: Value,
    ) -> Result<T, InvalidBody> {
        match CheckedBody::<T>::parse(Some(v)) {
            Ok(CheckedBody(r)) => r,
            Err(_) => panic!("not a JSON object"),
        }
    }

    /// GH#45: a body with bad core fields and bad attributes reports both at once.
    #[tokio::test]
    async fn invalid_core_fields_do_not_hide_attribute_errors() {
        let Some(db) = scratch::database("invalid_core_fields_do_not_hide_attribute_errors").await else { return };
        let pool = &db.pool;
        crate::seed::install_template(pool, "it_infrastructure").await.unwrap();
        let (server, in_service) = (id_of(pool, "ci_classes", "server").await, status(pool, "in_service").await);
        let ctx = RequestContext::system("test", "test");

        let body = json!({ "classId": server, "validFrom": "yesterday",
            "attributes": { "name": "x", "status": in_service, "management_ip": "abc", "cpu_cores": "x" } });
        let Err(invalid) = parse::<CreateItemBody>(body) else { panic!("body passed") };
        let err = create_errors(pool, &ctx, invalid).await;
        assert_eq!(err.code, ErrorCode::ValidationError);
        assert_eq!(fields(&err), ["attributes.cpu_cores", "attributes.management_ip", "validFrom"]);

        // The attribute rules that need the database too: unknown keys and values out of range.
        let body = json!({ "classId": server, "ident": "-bad", "attributes": { "cpu_cores": 0, "nope": 1 } });
        let Err(invalid) = parse::<CreateItemBody>(body) else { panic!("body passed") };
        assert_eq!(
            fields(&create_errors(pool, &ctx, invalid).await),
            ["attributes.cpu_cores", "attributes.nope", "ident"]
        );

        // No usable class id: only the body's own errors.
        let body = json!({ "classId": "not-a-uuid", "attributes": { "cpu_cores": "x" } });
        let Err(invalid) = parse::<CreateItemBody>(body) else { panic!("body passed") };
        assert_eq!(fields(&create_errors(pool, &ctx, invalid).await), ["classId"]);

        // Update: the same merge, checked against the CI's current class.
        let valid = json!({ "classId": server, "attributes": { "name": "srv-1", "status": in_service } });
        let Ok(valid) = parse::<CreateItemBody>(valid) else { panic!("body failed") };
        let item = create(pool, &ctx, &valid).await.unwrap();
        let body = json!({ "validUntil": 5, "attributes": { "management_ip": "abc", "cpu_cores": null } });
        let Err(invalid) = parse::<UpdateItemBody>(body) else { panic!("body passed") };
        let err = update_errors(pool, &ctx, item.summary.id, invalid).await;
        assert_eq!(fields(&err), ["attributes.management_ip", "validUntil"]);
        db.drop().await;
    }

    /// GH#289: a NUL in a text value is a 400 invalid_character on create and
    /// update, reported with the class's other attribute errors, and nothing is stored.
    #[tokio::test]
    async fn nul_in_text_values_is_refused() {
        let Some(db) = scratch::database("nul_in_text_values_is_refused").await else { return };
        let pool = &db.pool;
        crate::seed::install_template(pool, "it_infrastructure").await.unwrap();
        let (server, in_service) = (id_of(pool, "ci_classes", "server").await, status(pool, "in_service").await);
        let ctx = RequestContext::system("test", "test");
        let codes = |err: &AppError| -> Vec<(String, String)> {
            let mut c: Vec<_> = err.details.iter().flatten().map(|d| (d.field.clone(), d.code.clone())).collect();
            c.sort_unstable();
            c
        };
        let pair = |f: &str, c: &str| (f.to_owned(), c.to_owned());

        let body = json!({ "classId": server,
            "attributes": { "name": "abc\u{0}def", "status": in_service, "cpu_cores": 0 } });
        let Err(invalid) = parse::<CreateItemBody>(body) else { panic!("body passed") };
        let err = create_errors(pool, &ctx, invalid).await;
        assert_eq!(err.code, ErrorCode::ValidationError);
        assert_eq!(
            codes(&err),
            [pair("attributes.cpu_cores", "too_small"), pair("attributes.name", "invalid_character")]
        );

        let valid = json!({ "classId": server, "attributes": { "name": "srv-1", "status": in_service } });
        let Ok(valid) = parse::<CreateItemBody>(valid) else { panic!("body failed") };
        let item = create(pool, &ctx, &valid).await.unwrap();
        let body = json!({ "attributes": { "hostname": "a\u{0}", "name": "srv-2" } });
        let Err(invalid) = parse::<UpdateItemBody>(body) else { panic!("body passed") };
        let err = update_errors(pool, &ctx, item.summary.id, invalid).await;
        assert_eq!(codes(&err), [pair("attributes.hostname", "invalid_character")]);
        let stored = get(pool, &ctx, item.summary.id).await.unwrap();
        assert_eq!(stored.summary.label, "srv-1");
        db.drop().await;
    }

    fn user_ctx(administrator: bool) -> RequestContext {
        use crate::auth::permissions::{ClassRights, Permissions};
        let permissions = Permissions { administrator, all_classes: ClassRights::ALL, ..Default::default() };
        let principal = crate::auth::Principal {
            user_id: Uuid::new_v4(),
            username: if administrator { "admin" } else { "editor" }.into(),
            credential: crate::auth::Credential::Token,
            permissions,
        };
        RequestContext::user(std::sync::Arc::new(principal), "test".into())
    }

    /// The barebone core: ident, validity, active and the label from the title attribute.
    #[tokio::test]
    async fn core_fields_ident_validity_and_label() {
        let Some(db) = scratch::database("core_fields_ident_validity_and_label").await else { return };
        let pool = &db.pool;
        crate::seed::install_template(pool, "it_infrastructure").await.unwrap();
        let (server, in_service) = (id_of(pool, "ci_classes", "server").await, status(pool, "in_service").await);
        let editor = user_ctx(false);
        let admin = user_ctx(true);

        let body = |v: Value| parse::<CreateItemBody>(v).unwrap_or_else(|_| panic!("invalid create body"));
        let created = create(
            pool,
            &editor,
            &body(json!({ "classId": server,
            "attributes": { "name": "web-01", "status": in_service, "hostname": "web-01.example.com" } })),
        )
        .await
        .unwrap();
        let ci = &created.summary;
        assert!(regex::Regex::new("^CI-[0-9A-HJKMNP-TV-Z]{8}$").unwrap().is_match(&ci.ident), "{}", ci.ident);
        assert_eq!(ci.label, "web-01");
        assert!(ci.active && ci.valid_until.is_none());

        // Only administrators set or change an ident; resending the current one is fine.
        let err = create(
            pool,
            &editor,
            &body(json!({ "classId": server, "ident": "SRV-0001",
            "attributes": { "name": "web-02", "status": in_service } })),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::Forbidden);
        let update_body = |v: Value| parse::<UpdateItemBody>(v).unwrap_or_else(|_| panic!("invalid update body"));
        let err = update(pool, &editor, ci.id, &update_body(json!({ "ident": "SRV-0001" }))).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::Forbidden);
        update(pool, &editor, ci.id, &update_body(json!({ "ident": ci.ident, "attributes": { "name": "web-01a" } })))
            .await
            .unwrap();
        let changed = update(pool, &admin, ci.id, &update_body(json!({ "ident": "SRV-0001" }))).await.unwrap();
        assert_eq!((changed.summary.ident.as_str(), changed.summary.label.as_str()), ("SRV-0001", "web-01a"));
        let audited: Value = sqlx::query_scalar(
            "SELECT jsonb_build_array(old_value->'ident', new_value->'ident') FROM audit_log
             WHERE entity_id = $1 AND action = 'update' ORDER BY id DESC LIMIT 1",
        )
        .bind(ci.id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(audited, json!([ci.ident, "SRV-0001"]));

        // Unique regardless of case.
        let err = create(
            pool,
            &admin,
            &body(json!({ "classId": server, "ident": "srv-0001",
            "attributes": { "name": "web-03", "status": in_service } })),
        )
        .await
        .unwrap_err();
        assert_eq!((err.code, fields(&err)), (ErrorCode::Conflict, vec!["ident"]));

        // Validity: a CI past its validUntil is inactive and hidden from lists by default.
        let err = update(pool, &admin, ci.id, &update_body(json!({ "validUntil": "2000-01-01T00:00:00Z" })))
            .await
            .unwrap_err();
        assert_eq!(fields(&err), ["validUntil"], "before validFrom");
        let retired = create(
            pool,
            &editor,
            &body(json!({ "classId": server, "validFrom": "2020-01-01T00:00:00Z",
            "validUntil": "2021-01-01T00:00:00Z", "attributes": { "name": "old-01", "status": in_service } })),
        )
        .await
        .unwrap();
        assert!(!retired.summary.active);
        let list = |active: &str| {
            let q: ListItemsQuery = serde_json::from_value(json!({
                "limit": 50, "offset": 0, "sort": "label", "includeSubclasses": "true", "deleted": "exclude", "active": active
            }))
            .unwrap();
            q
        };
        let labels = |page: Page<ConfigurationItem>| page.data.into_iter().map(|c| c.summary.label).collect::<Vec<_>>();
        assert_eq!(labels(super::list(pool, &editor, &list("true")).await.unwrap()), ["web-01a"]);
        assert_eq!(labels(super::list(pool, &editor, &list("false")).await.unwrap()), ["old-01"]);
        assert_eq!(labels(super::list(pool, &editor, &list("all")).await.unwrap()), ["old-01", "web-01a"]);

        // Lookup filter: by status value; another value of the same list widens, another list narrows.
        let retired_status = status(pool, "retired").await;
        let mut q = list("all");
        q.lookup_value_id = Some(crate::api::schemas::UuidList(vec![retired_status]));
        assert!(labels(super::list(pool, &editor, &q).await.unwrap()).is_empty());
        q.lookup_value_id = Some(crate::api::schemas::UuidList(vec![retired_status, in_service]));
        assert_eq!(labels(super::list(pool, &editor, &q).await.unwrap()), ["old-01", "web-01a"]);

        // The title attribute labels CIs; without one they are labelled by their ident.
        sqlx::query("UPDATE ci_classes SET title_attribute_id = NULL WHERE id = $1")
            .bind(server)
            .execute(pool)
            .await
            .unwrap();
        let model = Model::load(&mut pool.acquire().await.unwrap()).await.unwrap();
        data::refresh_labels(&mut pool.acquire().await.unwrap(), &model, &[server], None).await.unwrap();
        let again = get(pool, &editor, ci.id).await.unwrap();
        assert_eq!(again.summary.label, "SRV-0001");

        // Search finds CIs by ident.
        let q: SearchQuery = serde_json::from_value(json!({
            "limit": 10, "offset": 0, "q": "srv-0001", "includeSubclasses": "true", "deleted": "exclude", "active": "true"
        }))
        .unwrap();
        let hits = search(pool, &editor, &q).await.unwrap();
        assert_eq!(hits.data.len(), 1);
        assert!(hits.data[0].matches.iter().any(|m| m.field == "ident"));
        db.drop().await;
    }

    /// GH#269: resending a reference into a class the caller may not view
    /// answers the same whether the target is live or deleted, and whatever
    /// its class; a reference that changes attribute definition is new.
    #[tokio::test]
    async fn hidden_references_are_no_oracle() {
        use crate::auth::permissions::{ClassRights, Permissions};
        use crate::modules::classes::{AttributeDefinition, AttributeDefinitions, CiClass, CiClasses};
        use crate::modules::simple_resource as simple;
        let Some(db) = scratch::database("hidden_references_are_no_oracle").await else { return };
        let pool = &db.pool;
        crate::seed::install_template(pool, "it_infrastructure").await.unwrap();
        let (application, database) =
            (id_of(pool, "ci_classes", "application").await, id_of(pool, "ci_classes", "database").await);
        let in_service = status(pool, "in_service").await;
        let system = RequestContext::system("test", "test");
        let create_body = |v: Value| parse::<CreateItemBody>(v).unwrap_or_else(|_| panic!("invalid create body"));
        let db_item = |name: &str| {
            create_body(
                json!({ "classId": database, "attributes": { "name": name, "status": in_service, "engine": "postgresql" } }),
            )
        };
        let (live, gone) = (
            create(pool, &system, &db_item("db-live")).await.unwrap().summary.id,
            create(pool, &system, &db_item("db-gone")).await.unwrap().summary.id,
        );
        let app = |name: &str, target: Uuid| {
            create_body(json!({ "classId": application,
                "attributes": { "name": name, "status": in_service, "primary_database": target } }))
        };
        let (x_live, x_gone) = (
            create(pool, &system, &app("app-live", live)).await.unwrap().summary.id,
            create(pool, &system, &app("app-gone", gone)).await.unwrap().summary.id,
        );
        remove(pool, &system, gone).await.unwrap();

        // Two classes defining their own primary_database: one the target fits, one it does not.
        let server = id_of(pool, "ci_classes", "server").await;
        let mut classes = Vec::new();
        for (name, points_at) in [("Service A", database), ("Service B", server)] {
            let class: CiClass =
                simple::create::<CiClasses>(pool, &system, &serde_json::from_value(json!({ "name": name })).unwrap())
                    .await
                    .unwrap();
            let _: AttributeDefinition = simple::create::<AttributeDefinitions>(
                pool,
                &system,
                &serde_json::from_value(json!({ "classId": class.id, "key": "primary_database",
                    "label": "Primary database", "dataType": "reference", "referenceClassId": points_at }))
                .unwrap(),
            )
            .await
            .unwrap();
            classes.push(class.id);
        }
        let (fits, misfits) = (classes[0], classes[1]);

        // The caller may edit applications (and create the new classes), but not view databases.
        let rights = |classes: &[Uuid]| classes.iter().map(|c| (*c, ClassRights::ALL)).collect();
        let permissions = Permissions { classes: rights(&[application, fits, misfits]), ..Default::default() };
        let principal = crate::auth::Principal {
            user_id: Uuid::new_v4(),
            username: "restricted".into(),
            credential: crate::auth::Credential::Token,
            permissions,
        };
        let ctx = RequestContext::user(std::sync::Arc::new(principal), "test".into());
        let codes = |err: &AppError| -> Vec<(String, String)> {
            let mut c: Vec<_> = err.details.iter().flatten().map(|d| (d.field.clone(), d.code.clone())).collect();
            c.sort();
            c
        };
        let probe = |x: Uuid, target: Uuid, class: Option<Uuid>| {
            let mut body = json!({ "validFrom": "bogus", "attributes": { "primary_database": target } });
            if let Some(class) = class {
                body["classId"] = json!(class);
            }
            let Err(invalid) = parse::<UpdateItemBody>(body) else { panic!("body passed") };
            update_errors(pool, &ctx, x, invalid)
        };

        // Unchanged: only the body's own error, live or deleted.
        for (x, target) in [(x_live, live), (x_gone, gone)] {
            assert_eq!(codes(&probe(x, target, None).await), [("validFrom".into(), "invalid_format".into())]);
        }
        // Resending it in a valid body is no change, live or deleted.
        let update_body = |v: Value| parse::<UpdateItemBody>(v).unwrap_or_else(|_| panic!("invalid update body"));
        for (x, target) in [(x_live, live), (x_gone, gone)] {
            let body = update_body(json!({ "attributes": { "name": "renamed", "primary_database": target } }));
            let item = update(pool, &ctx, x, &body).await.unwrap();
            assert_eq!(item.summary.label, "renamed");
            assert_eq!(item.attributes["primary_database"], json!(target));
            assert_eq!(item.attribute_references["primary_database"]["hidden"], json!(true));
            assert_eq!(item.attribute_references["primary_database"]["deleted"], json!(false));
        }
        // Another class's primary_database is a new reference: not_found whatever the target's class or state.
        let expected: Vec<(String, String)> = vec![
            ("attributes.primary_database".into(), "not_found".into()),
            ("validFrom".into(), "invalid_format".into()),
        ];
        for class in [fits, misfits] {
            for (x, target) in [(x_live, live), (x_gone, gone)] {
                assert_eq!(codes(&probe(x, target, Some(class)).await), expected, "class {class}, target {target}");
            }
        }
        db.drop().await;
    }

    /// GH#112: attributes.<key> sorts a class's list on the type-table column.
    #[tokio::test]
    async fn list_sorts_by_attribute() {
        let Some(db) = scratch::database("list_sorts_by_attribute").await else { return };
        let pool = &db.pool;
        crate::seed::install_template(pool, "it_infrastructure").await.unwrap();
        let (hardware, server) =
            (id_of(pool, "ci_classes", "hardware").await, id_of(pool, "ci_classes", "server").await);
        let (switch, vm) =
            (id_of(pool, "ci_classes", "network_device").await, id_of(pool, "ci_classes", "virtual_machine").await);
        let (in_service, retired) = (status(pool, "in_service").await, status(pool, "retired").await);
        let ctx = user_ctx(false);
        for (class, attributes) in [
            (server, json!({ "name": "a", "status": retired, "hostname": "B-host", "ip_address": "10.0.0.10" })),
            (server, json!({ "name": "b", "status": in_service, "hostname": "a-host", "ip_address": "10.0.0.9" })),
            (server, json!({ "name": "c", "status": in_service, "hostname": "c-host" })),
            (switch, json!({ "name": "d", "status": retired, "device_role": "switch", "ip_address": "10.0.0.100" })),
        ] {
            let body = parse::<CreateItemBody>(json!({ "classId": class, "attributes": attributes }))
                .unwrap_or_else(|_| panic!("invalid create body"));
            create(pool, &ctx, &body).await.unwrap();
        }
        let query = |sort: &str, class_ids: &[Uuid]| {
            let mut q: ListItemsQuery = serde_json::from_value(json!({
                "limit": 50, "offset": 0, "sort": sort, "includeSubclasses": "true", "deleted": "exclude", "active": "true"
            }))
            .unwrap();
            q.class_id = (!class_ids.is_empty()).then(|| crate::api::schemas::UuidList(class_ids.to_vec()));
            q
        };
        let labels = |page: Page<ConfigurationItem>| page.data.into_iter().map(|c| c.summary.label).collect::<Vec<_>>();
        let sorted = async |sort: &str, class_ids: &[Uuid]| {
            labels(super::list(pool, &ctx, &query(sort, class_ids)).await.unwrap())
        };

        // IP addresses in address order (not text order), CIs without a value last either way.
        assert_eq!(sorted("attributes.ip_address", &[hardware]).await, ["b", "a", "d", "c"]);
        assert_eq!(sorted("-attributes.ip_address", &[hardware]).await, ["d", "a", "b", "c"]);
        // Text ignores case; an inherited attribute works on the subclass.
        assert_eq!(sorted("attributes.hostname", &[server]).await, ["b", "a", "c"]);
        // A lookup in the list's order (in_service before retired), then the label.
        assert_eq!(sorted("attributes.status", &[hardware]).await, ["b", "c", "a", "d"]);
        assert_eq!(sorted("-attributes.status", &[hardware]).await, ["a", "d", "b", "c"]);

        let code = async |sort: &str, class_ids: &[Uuid]| {
            let err = super::list(pool, &ctx, &query(sort, class_ids)).await.unwrap_err();
            let d = &err.details.as_ref().unwrap()[0];
            assert_eq!(
                (err.code, d.field.as_str(), d.location),
                (ErrorCode::ValidationError, "sort", FieldLocation::Query)
            );
            d.code.clone()
        };
        assert_eq!(code("attributes.hostname", &[]).await, "class_required");
        assert_eq!(code("attributes.nope", &[server]).await, "unknown_attribute");
        assert_eq!(code("attributes.cpu_cores", &[server, switch]).await, "unknown_attribute");
        // virtual_machine defines its own hostname: not the same attribute as the servers'.
        assert_eq!(code("attributes.hostname", &[server, vm]).await, "ambiguous_attribute");
        db.drop().await;
    }

    /// GH#177: a request holds at most one pooled connection, so list, search
    /// and graph still answer on a pool of one instead of waiting on themselves.
    #[tokio::test]
    async fn list_search_and_graph_need_one_connection() {
        let Some(db) = scratch::database("list_search_and_graph_need_one_connection").await else { return };
        crate::seed::install_template(&db.pool, "it_infrastructure").await.unwrap();
        crate::seed::seed_demo_data(&db.pool).await.unwrap();
        let server = id_of(&db.pool, "ci_classes", "server").await;
        let root: Uuid = sqlx::query_scalar("SELECT id FROM configuration_items WHERE class_id = $1 LIMIT 1")
            .bind(server)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        let one = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(5))
            .connect_with((*db.pool.connect_options()).clone())
            .await
            .unwrap();
        let ctx = user_ctx(false);

        let q: ListItemsQuery = serde_json::from_value(json!({
            "limit": 50, "offset": 0, "sort": "label", "includeSubclasses": "true", "deleted": "exclude", "active": "true"
        }))
        .unwrap();
        let page = list(&one, &ctx, &q).await.unwrap();
        assert!(!page.data.is_empty());
        assert_eq!(page.page.total, page.data.len() as i64);

        let q: SearchQuery = serde_json::from_value(json!({
            "limit": 10, "offset": 0, "q": "CI-", "includeSubclasses": "true", "deleted": "exclude", "active": "true"
        }))
        .unwrap();
        search(&one, &ctx, &q).await.unwrap();

        let q = GraphQuery { depth: 3, direction: GraphDirection::Both, relationship_type_id: None, max_nodes: 250 };
        let g = graph(&one, &ctx, root, &q).await.unwrap();
        assert!(g.nodes.len() > 1 && !g.edges.is_empty(), "{} nodes, {} edges", g.nodes.len(), g.edges.len());

        one.close().await;
        db.drop().await;
    }

    /// GH#185: the edges read for a hub CI are capped at 5 × maxNodes over the
    /// whole traversal, and hitting the cap marks the graph as truncated.
    #[tokio::test]
    async fn graph_caps_edges_read_for_a_hub() {
        let Some(db) = scratch::database("graph_caps_edges_read_for_a_hub").await else { return };
        crate::seed::install_template(&db.pool, "it_infrastructure").await.unwrap();
        let new_server = |label: String| {
            let pool = db.pool.clone();
            async move {
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO configuration_items (class_id, label)
                     VALUES ((SELECT id FROM ci_classes WHERE key = 'server'), $1) RETURNING id",
                )
                .bind(label)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        let hub = new_server("hub".into()).await;
        for i in 0..12 {
            let leaf = new_server(format!("leaf-{i:02}")).await;
            sqlx::query(
                "INSERT INTO ci_relationships (relationship_type_id, source_ci_id, target_ci_id)
                 VALUES ((SELECT id FROM relationship_types WHERE key = 'connected_to'), $1, $2)",
            )
            .bind(hub)
            .bind(leaf)
            .execute(&db.pool)
            .await
            .unwrap();
        }
        let ctx = user_ctx(false);
        let q =
            |max_nodes| GraphQuery { depth: 6, direction: GraphDirection::Both, relationship_type_id: None, max_nodes };

        // The data layer never returns more rows than asked for.
        let mut conn = db.pool.acquire().await.unwrap();
        let rows = data::edges_touching(&mut conn, &[hub], Direction::Both, None, None, 5).await.unwrap();
        assert_eq!(rows.len(), 5);
        drop(conn);

        // Budget 10 for 12 edges: cut off and flagged.
        let g = graph(&db.pool, &ctx, hub, &q(2)).await.unwrap();
        assert!(g.truncated);
        assert!(g.nodes.len() <= 2 && g.edges.len() <= 10, "{} nodes, {} edges", g.nodes.len(), g.edges.len());

        // Enough budget: the whole star, not flagged.
        let g = graph(&db.pool, &ctx, hub, &q(13)).await.unwrap();
        assert!(!g.truncated);
        assert_eq!((g.nodes.len(), g.edges.len()), (13, 12));

        db.drop().await;
    }

    #[test]
    fn edge_budget_keeps_what_fits_and_flags_the_rest() {
        let row = |_| data::EdgeRow {
            id: Uuid::new_v4(),
            relationship_type_id: Uuid::nil(),
            source_ci_id: Uuid::nil(),
            target_ci_id: Uuid::nil(),
            notes: None,
            type_key: String::new(),
            type_name: String::new(),
            forward_label: String::new(),
            reverse_label: String::new(),
            is_directional: false,
        };
        let (mut budget, mut truncated) = (3, false);
        assert_eq!(spend_edge_budget((0..3).map(row).collect(), &mut budget, &mut truncated).len(), 3);
        assert_eq!((budget, truncated), (0, false));
        assert!(spend_edge_budget(Vec::new(), &mut budget, &mut truncated).is_empty());
        assert!(!truncated);
        let (mut budget, mut truncated) = (3, false);
        assert_eq!(spend_edge_budget((0..4).map(row).collect(), &mut budget, &mut truncated).len(), 3);
        assert_eq!((budget, truncated), (0, true));
    }
}
