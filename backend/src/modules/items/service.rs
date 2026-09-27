//! Configuration-item rules: typed attribute values, class changes, optimistic
//! locking, soft delete with cascading edge removal, search and the graph.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value, json};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::schemas::{
    AttributeReference, ConfigurationItem, ConfigurationItemSummary, CreateItemBody, Graph, GraphDirection, GraphEdge,
    GraphEdgeType, GraphNode, GraphQuery, ItemFilterQuery, ListItemsQuery, SearchHit, SearchMatch, SearchQuery,
    SearchResults, UpdateItemBody,
};
use crate::api::context::RequestContext;
use crate::api::route::InvalidBody;
use crate::api::schemas::{LookupRef, OwnerRef, Page, Paged};
use crate::api::{pg_error, validate};
use crate::auth::permissions::ClassOp;
use crate::data::classes::{self as class_data, EffectiveAttributeRow};
use crate::data::crud::{self, AuditAction, AuditEntry};
use crate::data::items::{self as data, Direction, ItemFilters, StoredValue, SummaryRow, inet_text};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::modules::classes::AttributeDataType;
use crate::schema::model::{Field, Model};

pub fn summary_dto(r: SummaryRow) -> ConfigurationItemSummary {
    let lookup = |id: Option<Uuid>, key: Option<String>, name: Option<String>| match (id, key, name) {
        (Some(id), Some(key), Some(name)) => Some(LookupRef { id, key, name }),
        _ => None,
    };
    ConfigurationItemSummary {
        id: r.id,
        name: r.name,
        class_id: r.class_id,
        class: LookupRef { id: r.class_id, key: r.class_key, name: r.class_name },
        status_id: r.status_id,
        status: LookupRef { id: r.status_id, key: r.status_key, name: r.status_name },
        environment_id: r.environment_id,
        environment: lookup(r.environment_id, r.environment_key, r.environment_name),
        owner_id: r.owner_id,
        owner: match (r.owner_id, r.owner_name, r.owner_kind) {
            (Some(id), Some(name), Some(kind)) => Some(OwnerRef { id, name, kind }),
            _ => None,
        },
        location_id: r.location_id,
        location: lookup(r.location_id, r.location_key, r.location_name),
        hostname: r.hostname,
        ip_address: r.ip_address.as_ref().map(inet_text),
        serial_number: r.serial_number,
        notes: r.notes,
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

struct Prepared<'d> {
    set: Vec<(&'d EffectiveAttributeRow, StoredValue)>,
    clear: Vec<Uuid>,
}

fn body_error(field: String, message: impl Into<String>, code: &str) -> FieldError {
    FieldError { location: FieldLocation::Body, field, message: message.into(), code: code.into() }
}

/// Validates attribute input against the class's effective definitions.
/// `lenient_clear` accepts `null` for keys the class does not define (they are
/// being cleared as part of a class change).
async fn prepare_attributes<'d>(
    conn: &mut PgConnection,
    defs: &'d [EffectiveAttributeRow],
    input: Option<&Map<String, Value>>,
    class_key: &str,
    self_id: Option<Uuid>,
    lenient_clear: bool,
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
                    p.message = format!("Must match {pattern}");
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
        let live: HashSet<Uuid> = data::live_items(conn, &ids).await?.into_iter().collect();
        for (def, id) in refs {
            let field = format!("attributes.{}", def.key);
            if Some(id) == self_id {
                errors.push(body_error(field, "A CI cannot reference itself", "reference_self"));
            } else if !live.contains(&id) {
                errors.push(body_error(field, "Referenced CI does not exist or is deleted", "not_found"));
            }
        }
    }

    if errors.is_empty() { Ok(prepared) } else { Err(AppError::validation(errors)) }
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

/// Attribute checks for a new CI: the values (defaults filled in), the classes
/// they reference, and that every required attribute has one.
async fn prepare_new<'d>(
    conn: &mut PgConnection,
    defs: &'d [EffectiveAttributeRow],
    input: Option<&Map<String, Value>>,
    class_key: &str,
) -> Result<Prepared<'d>, AppError> {
    let attributes = with_defaults(defs, input);
    let prepared = prepare_attributes(conn, defs, Some(&attributes), class_key, None, false).await?;
    check_reference_classes(conn, &prepared).await?;
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
            check_new_attributes(pool, class_id, attributes.flatten()).await
        }
        _ => Ok(()),
    };
    merged(invalid.errors, checked)
}

async fn check_new_attributes(
    pool: &PgPool,
    class_id: Uuid,
    input: Option<&Map<String, Value>>,
) -> Result<(), AppError> {
    let mut conn = pool.acquire().await?;
    let key = class_key(&mut conn, class_id).await?;
    let defs = class_data::effective_attributes(&mut conn, class_id).await?;
    prepare_new(&mut conn, &defs, input, &key).await.map(drop)
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
    let prepared = prepare_attributes(&mut conn, &defs, Some(input), &key, Some(id), class_changes).await?;
    check_reference_classes(&mut conn, &prepared).await
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

async fn filters(pool: &PgPool, q: &impl ItemFilterQuery) -> Result<ItemFilters, AppError> {
    let class_ids = match q.class_id() {
        Some(ids) if q.include_subclasses() => {
            Some(class_data::with_descendant_classes(&mut *pool.acquire().await?, &ids.0).await?)
        }
        Some(ids) => Some(ids.0.clone()),
        None => None,
    };
    Ok(ItemFilters {
        q: None,
        class_ids,
        status_ids: q.status_id().map(|l| l.0.clone()),
        environment_ids: q.environment_id().map(|l| l.0.clone()),
        owner_ids: q.owner_id().map(|l| l.0.clone()),
        location_ids: q.location_id().map(|l| l.0.clone()),
        ip_within: q.ip_within().map(str::to_owned),
        deleted: Some(q.deleted()),
        visible_class_ids: None,
        search_tables: Vec::new(),
    })
}

/// Type tables searched by `q`.
async fn search_tables(pool: &PgPool) -> Result<Vec<data::SearchTable>, AppError> {
    let model = Model::load(&mut *pool.acquire().await?).await?;
    Ok(data::search_tables(&model))
}

/// Only CIs of classes the caller may view.
pub async fn list(
    pool: &PgPool,
    ctx: &RequestContext,
    q: &ListItemsQuery,
) -> Result<Page<ConfigurationItem>, AppError> {
    let search_tables = if q.q.is_some() { search_tables(pool).await? } else { Vec::new() };
    let f = ItemFilters {
        q: q.q.clone(),
        visible_class_ids: ctx.class_scope(ClassOp::View),
        search_tables,
        ..filters(pool, q).await?
    };
    let (rows, total) = data::list(pool, &f, &q.sort.field, q.sort.desc, q.limit, q.offset).await?;
    let mut conn = pool.acquire().await?;
    let model = Model::load(&mut conn).await?;
    Ok(Page { data: with_attributes(&mut conn, &model, rows).await?, page: q.page_meta(total) })
}

pub async fn search(pool: &PgPool, ctx: &RequestContext, q: &SearchQuery) -> Result<SearchResults, AppError> {
    let mut conn = pool.acquire().await?;
    let model = Model::load(&mut conn).await?;
    let f = ItemFilters {
        visible_class_ids: ctx.class_scope(ClassOp::View),
        search_tables: data::search_tables(&model),
        ..filters(pool, q).await?
    };
    let (rows, total) = data::search(pool, &q.q, &f, q.limit, q.offset).await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let values = data::values(&mut conn, &model, &ids).await?;

    let needle = q.q.to_lowercase();
    let hit = |s: &str| s.to_lowercase().contains(&needle);
    let words = data::query_words(&q.q);
    let q_is_net = validate::is_ip_or_cidr(&q.q);

    let data = rows
        .into_iter()
        .map(|r| {
            let item = summary_dto(r);
            let mut matches = Vec::new();
            let mut add = |field: &str, label: &str, value: &str| {
                matches.push(SearchMatch { field: field.into(), label: label.into(), value: value.into() })
            };
            if hit(&item.name) {
                add("name", "Name", &item.name);
            }
            if let Some(h) = item.hostname.as_deref().filter(|h| hit(h)) {
                add("hostname", "Hostname", h);
            }
            if let Some(s) = item.serial_number.as_deref().filter(|s| hit(s)) {
                add("serialNumber", "Serial number", s);
            }
            if let Some(ip) = item.ip_address.as_deref().filter(|ip| ip.starts_with(&q.q) || q_is_net) {
                add("ipAddress", "IP address", ip);
            }
            if let Some(notes) = item.notes.as_deref() {
                let lower = notes.to_lowercase();
                if hit(notes) || (!words.is_empty() && words.iter().all(|w| lower.contains(w.as_str()))) {
                    let shown = if notes.chars().count() > 200 {
                        format!("{}…", notes.chars().take(200).collect::<String>())
                    } else {
                        notes.to_owned()
                    };
                    add("notes", "Notes", &shown);
                }
            }
            for v in values.iter().filter(|v| v.ci_id == item.id) {
                let is_net = matches!(v.data_type, AttributeDataType::Ip | AttributeDataType::Cidr);
                if let Some(text) = v.search_text()
                    && (hit(text) || (is_net && text.starts_with(&q.q)))
                {
                    add(&format!("attributes.{}", v.key), &v.label, text);
                }
            }
            SearchHit { item, matches }
        })
        .collect();
    Ok(SearchResults { data, page: q.page_meta(total) })
}

/// Summary rows plus their field values and reference names (batched reads for the page).
async fn with_attributes(
    conn: &mut PgConnection,
    model: &Model,
    rows: Vec<SummaryRow>,
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
            let (name, deleted) = names.get(&ref_id).cloned().unwrap_or_default();
            let reference = AttributeReference { id: ref_id, name, deleted };
            item.attribute_references.insert(v.key.clone(), crud::json(&reference));
        }
        item.attributes.insert(v.key, v.value);
    }
    Ok(items)
}

async fn detail(conn: &mut PgConnection, model: &Model, id: Uuid) -> Result<Option<ConfigurationItem>, AppError> {
    let Some(row) = data::summary(conn, id).await? else { return Ok(None) };
    Ok(with_attributes(conn, model, vec![row]).await?.pop())
}

async fn must_detail(conn: &mut PgConnection, model: &Model, id: Uuid) -> Result<ConfigurationItem, AppError> {
    detail(conn, model, id).await?.ok_or_else(|| AppError::missing("Configuration item", id))
}

pub async fn get(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<ConfigurationItem, AppError> {
    let mut conn = pool.acquire().await?;
    let model = Model::load(&mut conn).await?;
    let dto = must_detail(&mut conn, &model, id).await?;
    ctx.require_class(dto.summary.class_id, ClassOp::View)?;
    Ok(dto)
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
    let mut tx = pool.begin().await?;
    let key = class_key(&mut tx, input.class_id).await?;
    let model = Model::load(&mut tx).await?;
    let defs = class_data::effective_attributes(&mut tx, input.class_id).await?;
    let prepared = prepare_new(&mut tx, &defs, input.attributes.as_ref(), &key).await?;

    let id = data::insert(
        &mut tx,
        &data::NewItem {
            class_id: input.class_id,
            name: &input.name,
            status_id: input.status_id,
            environment_id: input.environment_id,
            owner_id: input.owner_id,
            location_id: input.location_id,
            hostname: input.hostname.as_deref(),
            ip_address: input.ip_address.as_deref(),
            serial_number: input.serial_number.as_deref(),
            notes: input.notes.as_deref(),
        },
    )
    .await?;
    let lineage: Vec<Uuid> = model.lineage(input.class_id).iter().map(|c| c.id).collect();
    write_type_rows(&mut tx, &model, id, input.class_id, &prepared.set, &[], &lineage).await?;

    let dto = must_detail(&mut tx, &model, id).await?;
    let entry = AuditEntry {
        action: AuditAction::Create,
        entity_type: "configuration_items",
        entity_id: id,
        old_value: None,
        new_value: Some(crud::json(&dto)),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
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
    ctx.require_class(before.class_id, ClassOp::Edit)?;
    // Moving a CI to another class also needs create rights there.
    if let Some(new_class) = input.class_id.filter(|c| *c != before.class_id) {
        ctx.require_class(new_class, ClassOp::Create)?;
    }
    if before.deleted_at.is_some() {
        return Err(AppError::conflict("This configuration item is deleted and cannot be modified"));
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
    let before_dto = must_detail(&mut tx, &model, id).await?;

    let class_id = input.class_id.unwrap_or(before.class_id);
    let class_changes = class_id != before.class_id;
    let key = class_key(&mut tx, class_id).await?;
    let defs = class_data::effective_attributes(&mut tx, class_id).await?;
    let prepared = prepare_attributes(&mut tx, &defs, input.attributes.as_ref(), &key, Some(id), class_changes).await?;
    check_reference_classes(&mut tx, &prepared).await?;
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
        name: input.name.as_deref(),
        status_id: input.status_id,
        environment_id: input.environment_id,
        owner_id: input.owner_id,
        location_id: input.location_id,
        hostname: input.hostname.as_ref().map(|h| h.as_deref()),
        ip_address: input.ip_address.as_ref().map(|h| h.as_deref()),
        serial_number: input.serial_number.as_ref().map(|h| h.as_deref()),
        notes: input.notes.as_ref().map(|h| h.as_deref()),
    };
    data::update(&mut tx, id, &patch).await?;
    let entering: Vec<Uuid> = new_lineage.iter().filter(|c| !old_lineage.contains(c)).copied().collect();
    write_type_rows(&mut tx, &model, id, class_id, &prepared.set, &prepared.clear, &entering).await?;
    check_required(&mut tx, &model, id, &defs).await?;

    let dto = must_detail(&mut tx, &model, id).await?;
    let entry = AuditEntry {
        action: AuditAction::Update,
        entity_type: "configuration_items",
        entity_id: id,
        old_value: Some(crud::json(&before_dto)),
        new_value: Some(crud::json(&dto)),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    tx.commit().await?;
    Ok(dto)
}

/// Soft delete: the CI and its live relationships get deleted_at; history keeps resolving.
pub async fn remove(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    match data::lock(&mut tx, id).await? {
        Some(row) if row.deleted_at.is_none() => ctx.require_class(row.class_id, ClassOp::Delete)?,
        _ => return Err(AppError::missing("Configuration item", id)),
    }
    let model = Model::load(&mut tx).await?;
    let before = must_detail(&mut tx, &model, id).await?;
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

/// Breadth-first expansion from a root CI: one query per hop (not per CI),
/// then one query for all node summaries. The traversal never enters CIs of
/// classes the caller may not view.
pub async fn graph(pool: &PgPool, ctx: &RequestContext, root_id: Uuid, q: &GraphQuery) -> Result<Graph, AppError> {
    let mut conn = pool.acquire().await?;
    let Some(root) = data::summary(&mut conn, root_id).await? else {
        return Err(AppError::missing("Configuration item", root_id));
    };
    ctx.require_class(root.class_id, ClassOp::View)?;
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

    let mut hop = 1;
    while hop <= q.depth && !frontier.is_empty() {
        let found = data::edges_touching(pool, &frontier, direction, types, visible).await?;
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
        for e in data::edges_touching(pool, &frontier, Direction::Both, types, visible).await? {
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
            .then_with(|| a.summary.name.to_lowercase().cmp(&b.summary.name.to_lowercase()))
            .then_with(|| a.summary.name.cmp(&b.summary.name))
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

    fn fields(err: &AppError) -> Vec<&str> {
        let mut f: Vec<&str> = err.details.iter().flatten().map(|d| d.field.as_str()).collect();
        f.sort_unstable();
        f
    }

    /// GH#45: a body with bad core fields and bad attributes reports both at once.
    #[tokio::test]
    async fn invalid_core_fields_do_not_hide_attribute_errors() {
        let Some(db) = scratch::database("invalid_core_fields_do_not_hide_attribute_errors").await else { return };
        let pool = &db.pool;
        crate::seed::install_template(pool, "it_infrastructure").await.unwrap();
        let (server, in_service) =
            (id_of(pool, "ci_classes", "server").await, id_of(pool, "statuses", "in_service").await);
        let ctx = RequestContext::system("test", "test");

        let body = json!({ "name": "x", "classId": server, "statusId": in_service, "ipAddress": "999.1.1.1",
            "attributes": { "management_ip": "abc", "cpu_cores": "x" } });
        let Ok(CheckedBody(Err(invalid))) = CheckedBody::<CreateItemBody>::parse(Some(body)) else {
            panic!("body passed")
        };
        let err = create_errors(pool, &ctx, invalid).await;
        assert_eq!(err.code, ErrorCode::ValidationError);
        assert_eq!(fields(&err), ["attributes.cpu_cores", "attributes.management_ip", "ipAddress"]);

        // The attribute rules that need the database too: unknown keys and values out of range.
        let body = json!({ "name": "", "classId": server, "statusId": in_service, "attributes": { "cpu_cores": 0, "nope": 1 } });
        let Ok(CheckedBody(Err(invalid))) = CheckedBody::<CreateItemBody>::parse(Some(body)) else {
            panic!("body passed")
        };
        assert_eq!(
            fields(&create_errors(pool, &ctx, invalid).await),
            ["attributes.cpu_cores", "attributes.nope", "name"]
        );

        // No usable class id: only the body's own errors.
        let body =
            json!({ "name": "x", "classId": "not-a-uuid", "statusId": in_service, "attributes": { "cpu_cores": "x" } });
        let Ok(CheckedBody(Err(invalid))) = CheckedBody::<CreateItemBody>::parse(Some(body)) else {
            panic!("body passed")
        };
        assert_eq!(fields(&create_errors(pool, &ctx, invalid).await), ["classId"]);

        // Update: the same merge, checked against the CI's current class.
        let valid = json!({ "name": "srv-1", "classId": server, "statusId": in_service });
        let Ok(CheckedBody(Ok(valid))) = CheckedBody::<CreateItemBody>::parse(Some(valid)) else {
            panic!("body failed")
        };
        let item = create(pool, &ctx, &valid).await.unwrap();
        let body = json!({ "hostname": "-bad-", "attributes": { "management_ip": "abc", "cpu_cores": null } });
        let Ok(CheckedBody(Err(invalid))) = CheckedBody::<UpdateItemBody>::parse(Some(body)) else {
            panic!("body passed")
        };
        let err = update_errors(pool, &ctx, item.summary.id, invalid).await;
        assert_eq!(fields(&err), ["attributes.management_ip", "hostname"]);
        db.drop().await;
    }
}
