//! Configuration-item rules: typed attribute values, class changes, optimistic
//! locking, soft delete with cascading edge removal, search and the graph.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value, json};
use sqlx::{Connection, PgConnection, PgPool};
use uuid::Uuid;

use super::schemas::{
    AttributeReference, ConfigurationItem, ConfigurationItemSummary, CreateItemBody, Graph, GraphDirection, GraphEdge,
    GraphEdgeType, GraphNode, GraphQuery, ItemFilterQuery, ListItemsQuery, SearchHit, SearchMatch, SearchQuery,
    SearchResults, UpdateItemBody,
};
use crate::api::context::RequestContext;
use crate::api::schemas::{LookupRef, OwnerRef, Page, Paged, iso};
use crate::api::{pg_error, validate};
use crate::auth::permissions::ClassOp;
use crate::data::classes::{self as class_data, EffectiveAttributeRow};
use crate::data::crud::{self, AuditAction, AuditEntry};
use crate::data::items::{self as data, Direction, ItemFilters, StoredValue, StoredValueRow, SummaryRow, inet_text};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::modules::classes::AttributeDataType;

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

fn number_json(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < 9.0e15 {
        Value::from(n as i64)
    } else {
        serde_json::Number::from_f64(n).map(Value::Number).unwrap_or(Value::Null)
    }
}

fn value_to_json(v: &StoredValueRow) -> Option<Value> {
    if let Some(t) = &v.value_text {
        return Some(Value::String(t.clone()));
    }
    if let Some(n) = v.value_number {
        return Some(number_json(n));
    }
    if let Some(b) = v.value_boolean {
        return Some(Value::Bool(b));
    }
    if let Some(d) = &v.value_date {
        return Some(Value::String(d.clone()));
    }
    if let Some(t) = &v.value_datetime {
        return Some(Value::String(iso(t)));
    }
    if let Some(ip) = v.value_ip.as_ref().or(v.value_cidr.as_ref()) {
        return Some(Value::String(ip.clone()));
    }
    v.value_ref_ci_id.map(|id| Value::String(id.to_string()))
}

// ---------------------------------------------------------------------------
// Attribute validation
// ---------------------------------------------------------------------------

/// The JSON Schema a value of this attribute must satisfy (same rules the
/// SHAA-3 API enforced), including the definition's extra validation.
fn value_schema(def: &EffectiveAttributeRow) -> Value {
    let rules = def.validation.as_ref().map(|v| v.0.clone()).unwrap_or_default();
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
    match def.data_type {
        AttributeDataType::Text => {
            let mut s = json!({ "type": "string", "maxLength": rules.get("maxLength").and_then(Value::as_u64).unwrap_or(10_000) });
            if let Some(p) = rules.get("pattern").and_then(Value::as_str) {
                s["pattern"] = p.into();
            }
            s
        }
        AttributeDataType::Enum => {
            json!({ "enum": def.enum_values.as_ref().map(|v| v.0.clone()).unwrap_or_default() })
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
        AttributeDataType::Reference => json!({ "type": "string", "format": "uuid" }),
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
        let problems = validate::check(&value_schema(def), value, FieldLocation::Body, None);
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
        if let StoredValue::Reference(id) = stored {
            refs.push((def, id));
        }
        prepared.set.push((def, stored));
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

async fn write_attributes(
    conn: &mut PgConnection,
    ci_id: Uuid,
    set: &[(&EffectiveAttributeRow, StoredValue)],
) -> Result<(), AppError> {
    for (def, value) in set {
        // A savepoint keeps the transaction usable so the error can be reported per field.
        let mut sp = conn.begin().await?;
        match data::upsert_attribute_value(&mut sp, ci_id, def.id, value).await {
            Ok(()) => sp.commit().await?,
            Err(err) => {
                return Err(pg_error::map(&err, Some(&format!("attributes.{}", def.key))).unwrap_or_else(|| err.into()));
            }
        }
    }
    Ok(())
}

async fn check_required(conn: &mut PgConnection, ci_id: Uuid, defs: &[EffectiveAttributeRow]) -> Result<(), AppError> {
    let present: HashSet<String> = data::attribute_values(conn, &[ci_id]).await?.into_iter().map(|v| v.key).collect();
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
    })
}

/// Only CIs of classes the caller may view.
pub async fn list(
    pool: &PgPool,
    ctx: &RequestContext,
    q: &ListItemsQuery,
) -> Result<Page<ConfigurationItemSummary>, AppError> {
    let f =
        ItemFilters { q: q.q.clone(), visible_class_ids: ctx.class_scope(ClassOp::View), ..filters(pool, q).await? };
    let (rows, total) = data::list(pool, &f, &q.sort.field, q.sort.desc, q.limit, q.offset).await?;
    Ok(Page { data: rows.into_iter().map(summary_dto).collect(), page: q.page_meta(total) })
}

pub async fn search(pool: &PgPool, ctx: &RequestContext, q: &SearchQuery) -> Result<SearchResults, AppError> {
    let f = ItemFilters { visible_class_ids: ctx.class_scope(ClassOp::View), ..filters(pool, q).await? };
    let (rows, total) = data::search(pool, &q.q, &f, q.limit, q.offset).await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let values = data::attribute_values(&mut *pool.acquire().await?, &ids).await?;

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
                let is_net = v.value_ip.is_some() || v.value_cidr.is_some();
                if let Some(text) = v.value_text.as_ref().or(v.value_ip.as_ref()).or(v.value_cidr.as_ref())
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

async fn detail(conn: &mut PgConnection, id: Uuid) -> Result<Option<ConfigurationItem>, AppError> {
    let Some(row) = data::summary(conn, id).await? else { return Ok(None) };
    let values = data::attribute_values(conn, &[id]).await?;
    let mut attributes = Map::new();
    let mut attribute_references = Map::new();
    for v in &values {
        let Some(json) = value_to_json(v) else { continue };
        attributes.insert(v.key.clone(), json);
        if let Some(ref_id) = v.value_ref_ci_id {
            let reference = AttributeReference {
                id: ref_id,
                name: v.ref_name.clone().unwrap_or_default(),
                deleted: v.ref_deleted.unwrap_or(false),
            };
            attribute_references.insert(v.key.clone(), crud::json(&reference));
        }
    }
    Ok(Some(ConfigurationItem { summary: summary_dto(row), attributes, attribute_references }))
}

async fn must_detail(conn: &mut PgConnection, id: Uuid) -> Result<ConfigurationItem, AppError> {
    detail(conn, id).await?.ok_or_else(|| AppError::missing("Configuration item", id))
}

pub async fn get(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<ConfigurationItem, AppError> {
    let dto = must_detail(&mut *pool.acquire().await?, id).await?;
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
    let defs = class_data::effective_attributes(&mut tx, input.class_id).await?;
    let prepared = prepare_attributes(&mut tx, &defs, input.attributes.as_ref(), &key, None, false).await?;

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
    write_attributes(&mut tx, id, &prepared.set).await?;
    check_required(&mut tx, id, &defs).await?;

    let dto = must_detail(&mut tx, id).await?;
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
    let before_dto = must_detail(&mut tx, id).await?;

    let class_id = input.class_id.unwrap_or(before.class_id);
    let class_changes = class_id != before.class_id;
    let key = class_key(&mut tx, class_id).await?;
    let defs = class_data::effective_attributes(&mut tx, class_id).await?;
    let prepared = prepare_attributes(&mut tx, &defs, input.attributes.as_ref(), &key, Some(id), class_changes).await?;

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
        let current = data::attribute_values(&mut tx, &[id]).await?;
        let outside: Vec<Uuid> =
            current.iter().filter(|v| !keep.contains(v.key.as_str())).map(|v| v.attribute_id).collect();
        data::delete_attribute_values(&mut tx, id, &outside).await?;
    }
    data::delete_attribute_values(&mut tx, id, &prepared.clear).await?;

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
    write_attributes(&mut tx, id, &prepared.set).await?;
    check_required(&mut tx, id, &defs).await?;

    let dto = must_detail(&mut tx, id).await?;
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
    let before = must_detail(&mut tx, id).await?;
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
