//! Configuration-item rules: typed attribute values, class changes, optimistic
//! locking, soft delete with cascading edge removal, search and the graph.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, DurationRound, TimeDelta, Utc};
use serde_json::{Map, Value};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use ipnetwork::IpNetwork;

use super::plan::{self, DbResolver, Needs, is_visible};
use super::schemas::{
    ActiveQuery, AttributeReference, ChangeHistogram, ChangeHistogramBucket, ChangeHistogramQuery, ConfigurationItem,
    ConfigurationItemSummary, CreateItemBody, CriticalityRef, Graph, GraphDirection, GraphEdge, GraphEdgeType,
    GraphNode, GraphQuery, HistogramBucket, ItemFilterQuery, KindQuery, ListItemsQuery, SearchHit, SearchMatch,
    SearchQuery, SearchResults, UpdateItemBody,
};
use crate::api::context::{Caller, RequestContext};
use crate::api::route::InvalidBody;
use crate::api::schemas::{KEY_PATTERN, LookupRef, Page, Paged};
use crate::api::validate;
use crate::auth::permissions::ClassOp;
use crate::data::classes as class_data;
use crate::data::crud::{self, AuditAction, AuditEntry};
use crate::data::items::{
    self as data, ATTRIBUTE_SORT_PREFIX, ActiveFilter, Direction, ItemFilters, ListSort, SORT_FIELDS, SummaryRow,
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
        criticality: match (r.criticality_id, r.criticality_key, r.criticality_name, r.criticality_rank) {
            (Some(id), Some(key), Some(name), Some(rank)) => Some(CriticalityRef { id, key, name, rank }),
            _ => None,
        },
    }
}

/// A criticality value sent in a body: one of the criticality list's values,
/// active unless the CI already holds it (`current`).
async fn check_criticality(
    conn: &mut PgConnection,
    value: Option<Uuid>,
    current: Option<Uuid>,
) -> Result<(), AppError> {
    let Some(id) = value.filter(|v| Some(*v) != current) else { return Ok(()) };
    match data::criticality_value_state(conn, id).await? {
        Some(true) => Ok(()),
        Some(false) => Err(AppError::field("criticalityValueId", "This criticality value is retired", "invalid")),
        None => Err(AppError::field("criticalityValueId", "Not a value of the criticality list", "not_found")),
    }
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
    let model = Model::load(&mut conn).await?;
    let key = plan::class_key(&model, class_id)?;
    let defs = class_data::effective_attributes(&mut conn, class_id).await?;
    let visible = ctx.class_scope(ClassOp::View);
    let resolver = DbResolver::load(&mut conn, visible.as_deref(), &Needs::for_create(&defs, input)).await?;
    plan::prepare_new(&model, &defs, input, key, &resolver).map(drop)
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
    let Some(locked) = data::lock(&mut conn, id).await? else { return Ok(()) };
    if locked.deleted_at.is_some() || ctx.require_class(locked.class_id, ClassOp::Edit).is_err() {
        return Ok(());
    }
    let class_id = new_class.unwrap_or(locked.class_id);
    if class_id != locked.class_id && ctx.require_class(class_id, ClassOp::Create).is_err() {
        return Ok(());
    }
    let model = Model::load(&mut conn).await?;
    let defs = class_data::effective_attributes(&mut conn, class_id).await?;
    // Same reference access as update(), so this 400 is no existence oracle either.
    let before = must_detail(&mut conn, &model, id, None).await?;
    let visible = ctx.class_scope(ClassOp::View);
    let needs = Needs::for_update(&defs, Some(input), &before.attributes);
    let resolver = DbResolver::load(&mut conn, visible.as_deref(), &needs).await?;
    plan::prepare_changed(&model, &defs, &before, class_id, Some(input), &resolver).map(drop)
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

pub(super) async fn filters(
    conn: &mut PgConnection,
    model: &Model,
    q: &impl ItemFilterQuery,
) -> Result<ItemFilters, AppError> {
    let mut class_ids = match q.class_id() {
        Some(ids) if q.include_subclasses() => Some(class_data::with_descendant_classes(conn, &ids.0).await?),
        Some(ids) => Some(ids.0.clone()),
        None => None,
    };
    // Process records stay out of the inventory unless asked for by kind or by naming their type.
    let process = class_data::process_class_ids(conn).await?;
    let excluded_class_ids = match q.kind() {
        Some(KindQuery::Any) => None,
        Some(KindQuery::Asset) => Some(process),
        Some(KindQuery::Process) => {
            class_ids = Some(match class_ids {
                Some(ids) => ids.into_iter().filter(|c| process.contains(c)).collect(),
                None => process,
            });
            None
        }
        None => {
            let named: &[Uuid] = q.class_id().map(|l| l.0.as_slice()).unwrap_or_default();
            Some(process.into_iter().filter(|c| !named.contains(c)).collect())
        }
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
    let business_services = match q.business_service_id() {
        Some(ids) => Some(data::ServiceMembers {
            service_ids: ids.0.clone(),
            member_type: crate::data::business_services::roles(conn).await?.map(|r| r.member_type),
        }),
        None => None,
    };
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
        criticality_value_ids: q.criticality_value_id().map(|l| l.0.clone()),
        deleted: Some(q.deleted()),
        visible_class_ids: None,
        own_layout: None,
        layout_template: None,
        excluded_class_ids,
        // Also where ipWithin looks.
        search_tables: data::search_tables(model),
        business_services,
    })
}

/// The inventory list's filters, view scope included: the list and its facet counts.
pub(super) async fn inventory_filters(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    model: &Model,
    q: &impl ItemFilterQuery,
    text: Option<&str>,
    own_layout: Option<bool>,
    layout_template: Option<&str>,
) -> Result<ItemFilters, AppError> {
    check_layout_template(layout_template)?;
    Ok(ItemFilters {
        q: text.map(str::to_owned),
        visible_class_ids: ctx.class_scope(ClassOp::View),
        own_layout,
        layout_template: layout_template.map(str::to_owned),
        ..filters(conn, model, q).await?
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
    let own_layout = q.own_layout.map(bool::from);
    let f =
        inventory_filters(&mut conn, ctx, &model, q, q.q.as_deref(), own_layout, q.layout_template.as_deref()).await?;
    let (rows, total) = data::list(&mut conn, &f, sort, q.sort.desc, q.limit, q.offset).await?;
    let data = with_attributes(&mut conn, &model, rows, f.visible_class_ids.as_deref()).await?;
    Ok(Page { data, page: q.page_meta(total) })
}

fn check_layout_template(key: Option<&str>) -> Result<(), AppError> {
    match key {
        Some(key) if !validate::cached_regex(KEY_PATTERN).is_some_and(|r| r.is_match(key)) => {
            Err(query_error("layoutTemplate", "Not a layout template key (lower_snake_case)", "invalid_string"))
        }
        _ => Ok(()),
    }
}

fn query_error(field: &str, message: &str, code: &str) -> AppError {
    AppError::validation(vec![FieldError {
        location: FieldLocation::Query,
        field: field.into(),
        message: message.into(),
        code: code.into(),
    }])
}

/// Changes per bucket to the CIs the list would return for the same filters,
/// counted from the audit log entries the caller may read there.
pub async fn change_histogram(
    pool: &PgPool,
    ctx: &RequestContext,
    q: &ChangeHistogramQuery,
) -> Result<ChangeHistogram, AppError> {
    let (width, cap) = q.bucket.width_and_cap();
    let to = q.to.unwrap_or_else(Utc::now);
    let from = q.from.unwrap_or_else(|| match q.bucket {
        HistogramBucket::Hour => to - TimeDelta::hours(24),
        HistogramBucket::Day => to - TimeDelta::days(30),
    });
    if from >= to {
        return Err(query_error("from", "Must be before `to`", "invalid_range"));
    }
    if to - from > cap {
        let message = format!("The range may span at most {} days with {} buckets", cap.num_days(), unit(q.bucket));
        return Err(query_error("from", &message, "range_too_large"));
    }
    let mut conn = pool.acquire().await?;
    let model = Model::load(&mut conn).await?;
    let own_layout = q.own_layout.map(bool::from);
    let f =
        inventory_filters(&mut conn, ctx, &model, q, q.q.as_deref(), own_layout, q.layout_template.as_deref()).await?;
    let counts = data::change_counts(&mut conn, &f, f.visible_class_ids.as_deref(), unit(q.bucket), from, to).await?;

    let buckets = histogram_buckets(q.bucket, from, to, width, counts);
    let total = buckets.iter().map(|b| b.created + b.updated + b.status_changed).sum();
    Ok(ChangeHistogram { from, to, bucket: q.bucket, buckets, total })
}

fn unit(bucket: HistogramBucket) -> &'static str {
    match bucket {
        HistogramBucket::Hour => "hour",
        HistogramBucket::Day => "day",
    }
}

/// Every bucket from `from` (rounded down) up to `to`, filled from the non-empty ones.
fn histogram_buckets(
    bucket: HistogramBucket,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    width: TimeDelta,
    counts: Vec<data::ChangeCounts>,
) -> Vec<ChangeHistogramBucket> {
    let mut start = match bucket {
        HistogramBucket::Hour => from.duration_trunc(TimeDelta::hours(1)),
        HistogramBucket::Day => from.duration_trunc(TimeDelta::days(1)),
    }
    .unwrap_or(from);
    let mut counts = counts.into_iter().peekable();
    let mut out = Vec::new();
    while start < to {
        let mut b = ChangeHistogramBucket { start, created: 0, updated: 0, status_changed: 0 };
        while let Some(c) = counts.next_if(|c| c.start < start + width) {
            b.created += c.created;
            b.updated += c.updated;
            b.status_changed += c.status_changed;
        }
        out.push(b);
        start += width;
    }
    out
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
    let model = Model::load(&mut tx).await?;
    let defs = class_data::effective_attributes(&mut tx, input.class_id).await?;
    let visible = ctx.class_scope(ClassOp::View);
    let needs = Needs::for_create(&defs, input.attributes.as_ref());
    let resolver = DbResolver::load(&mut tx, visible.as_deref(), &needs).await?;
    let plan = plan::plan_create(ctx, &model, &defs, input, &resolver)?;
    check_criticality(&mut tx, input.criticality_value_id, None).await?;
    let id = plan::apply(&mut tx, &model, &plan).await?;

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
    let locked = data::lock(&mut tx, id).await?.ok_or_else(|| AppError::missing("Configuration item", id))?;
    // A CI the caller may not view is missing, before anything else is looked at.
    ctx.require_class_visible(locked.class_id, "Configuration item", id)?;
    let model = Model::load(&mut tx).await?;
    let before = must_detail(&mut tx, &model, id, None).await?;
    let class_id = input.class_id.unwrap_or(locked.class_id);
    let defs = class_data::effective_attributes(&mut tx, class_id).await?;
    let visible = ctx.class_scope(ClassOp::View);
    let needs = Needs::for_update(&defs, input.attributes.as_ref(), &before.attributes);
    let resolver = DbResolver::load(&mut tx, visible.as_deref(), &needs).await?;
    let current_criticality = before.summary.criticality.as_ref().map(|c| c.id);
    let service_class = crate::data::business_services::roles(&mut tx).await?.map(|r| r.service_class);
    let plan = plan::plan_update(ctx, &model, &defs, before, input, &resolver, service_class)?;
    check_criticality(&mut tx, input.criticality_value_id.flatten(), current_criticality).await?;
    plan::apply(&mut tx, &model, &plan).await?;

    let dto = must_detail(&mut tx, &model, id, None).await?;
    let entry = AuditEntry {
        action: AuditAction::Update,
        entity_type: "configuration_items",
        entity_id: id,
        old_value: plan.before.as_ref().map(crud::json),
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
    // A Person linked to a sign-in account stays (SHAA-1505 decision 7); the
    // trigger configuration_items_keep_person is the backstop.
    if let Some(account) = crate::data::people::linked_user(&mut tx, id).await? {
        return Err(AppError::new(
            ErrorCode::Conflict,
            format!(
                "This person is linked to the sign-in account \"{}\" and cannot be deleted. Disable or delete the \
                 account first (Administration > Users).",
                account.username
            ),
        )
        .with_details(vec![FieldError {
            location: FieldLocation::Params,
            field: "id".into(),
            message: format!("Linked to the sign-in account \"{}\"", account.username),
            code: "person_linked".into(),
        }]));
    }
    let model = Model::load(&mut tx).await?;
    let before = must_detail(&mut tx, &model, id, None).await?;
    let edges = data::soft_delete_edges_of(&mut tx, id).await?;
    data::soft_delete(&mut tx, id).await?;
    // Its running workflows end with it (the CI row is locked first, as on every workflow path).
    let cancelled = crate::modules::workflows::runtime::cancel_for_deleted_ci(&mut tx, ctx, id).await?;

    let mut entries: Vec<AuditEntry> = cancelled;
    entries.extend(edges.iter().map(|e| AuditEntry {
        action: AuditAction::Delete,
        entity_type: "ci_relationships",
        entity_id: e.id,
        old_value: Some(crud::json(e)),
        new_value: None,
    }));
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
// Writes on behalf of another operation
// ---------------------------------------------------------------------------

/// `ctx` with the system caller's rights: for CI writes another operation
/// makes as part of its own (a user's Person, SHAA-1505), which needs the
/// rights of that operation, not class rights. The audit rows still name `ctx`.
fn on_behalf(ctx: &RequestContext) -> RequestContext {
    RequestContext { caller: Caller::System, ..ctx.clone() }
}

/// Creates a CI of `class_id` with these attribute values, validated as any
/// create is, in the caller's transaction; writes its audit row. Returns its id.
pub(crate) async fn create_on_behalf(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    class_id: Uuid,
    attributes: Map<String, Value>,
) -> Result<Uuid, AppError> {
    let system = on_behalf(ctx);
    let model = Model::load(conn).await?;
    let defs = class_data::effective_attributes(conn, class_id).await?;
    let input = CreateItemBody {
        class_id,
        ident: None,
        valid_from: None,
        valid_until: None,
        attributes: Some(attributes),
        criticality_value_id: None,
    };
    let needs = Needs::for_create(&defs, input.attributes.as_ref());
    let resolver = DbResolver::load(conn, None, &needs).await?;
    let plan = plan::plan_create(&system, &model, &defs, &input, &resolver)?;
    let id = plan::apply(conn, &model, &plan).await?;
    let dto = must_detail(conn, &model, id, None).await?;
    let entry = AuditEntry {
        action: AuditAction::Create,
        entity_type: "configuration_items",
        entity_id: id,
        old_value: None,
        new_value: Some(crud::json(&dto)),
    };
    crud::write_audit(conn, ctx, vec![entry]).await?;
    Ok(id)
}

/// Sets these attribute values on a live CI, validated as any update is, in
/// the caller's transaction; writes its audit row unless nothing changed.
pub(crate) async fn update_on_behalf(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    id: Uuid,
    attributes: Map<String, Value>,
) -> Result<(), AppError> {
    let system = on_behalf(ctx);
    let locked = data::lock(conn, id).await?.ok_or_else(|| AppError::missing("Configuration item", id))?;
    let model = Model::load(conn).await?;
    let before = must_detail(conn, &model, id, None).await?;
    let defs = class_data::effective_attributes(conn, locked.class_id).await?;
    let input = UpdateItemBody {
        class_id: None,
        ident: None,
        valid_from: None,
        valid_until: None,
        attributes: Some(attributes),
        criticality_value_id: None,
        version: None,
    };
    let needs = Needs::for_update(&defs, input.attributes.as_ref(), &before.attributes);
    let resolver = DbResolver::load(conn, None, &needs).await?;
    let plan = plan::plan_update(&system, &model, &defs, before, &input, &resolver, None)?;
    plan::apply(conn, &model, &plan).await?;
    let dto = must_detail(conn, &model, id, None).await?;
    let old = plan.before.as_ref().map(crud::json);
    let new = crud::json(&dto);
    if old.as_ref() != Some(&new) {
        let entry = AuditEntry {
            action: AuditAction::Update,
            entity_type: "configuration_items",
            entity_id: id,
            old_value: old,
            new_value: Some(new),
        };
        crud::write_audit(conn, ctx, vec![entry]).await?;
    }
    Ok(())
}

/// What a workflow step wrote on its CI.
pub(crate) struct WorkflowWrite {
    /// The CI before and after; equal when nothing changed.
    pub before: ConfigurationItem,
    pub after: ConfigurationItem,
}

/// Sets the fields a workflow step writes (its transition fields and the
/// state field) on a live CI the caller already locked, with the rules of
/// `PATCH /configuration-items/{id}`: the caller's edit right on the class,
/// the same value validation, and references only to CIs the caller may view.
/// Writes the CI's `update` audit row unless nothing changed. Validation
/// errors name the fields `attributes.<key>`, as a PATCH does. Without
/// `apply` it only validates (the CI is left as it is).
pub(crate) async fn update_for_workflow(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    id: Uuid,
    class_id: Uuid,
    attributes: Map<String, Value>,
    apply: bool,
) -> Result<WorkflowWrite, AppError> {
    let model = Model::load(conn).await?;
    let before = must_detail(conn, &model, id, None).await?;
    if attributes.is_empty() {
        return Ok(WorkflowWrite { after: before.clone(), before });
    }
    let defs = class_data::effective_attributes(conn, class_id).await?;
    let input = UpdateItemBody {
        class_id: None,
        ident: None,
        valid_from: None,
        valid_until: None,
        attributes: Some(attributes),
        criticality_value_id: None,
        version: None,
    };
    let visible = ctx.class_scope(ClassOp::View);
    let needs = Needs::for_update(&defs, input.attributes.as_ref(), &before.attributes);
    let resolver = DbResolver::load(conn, visible.as_deref(), &needs).await?;
    let plan = plan::plan_update(ctx, &model, &defs, before.clone(), &input, &resolver, None)?;
    if !apply {
        return Ok(WorkflowWrite { after: before.clone(), before });
    }
    plan::apply(conn, &model, &plan).await?;
    let after = must_detail(conn, &model, id, None).await?;
    let old = plan.before.as_ref().map(crud::json);
    let new = crud::json(&after);
    if old.as_ref() != Some(&new) {
        let entry = AuditEntry {
            action: AuditAction::Update,
            entity_type: "configuration_items",
            entity_id: id,
            old_value: old,
            new_value: Some(new),
        };
        crud::write_audit(conn, ctx, vec![entry]).await?;
    }
    Ok(WorkflowWrite { before, after })
}

/// Brings a soft-deleted CI back (its relationships stay deleted), in the
/// caller's transaction, with a `restore` audit row.
pub(crate) async fn restore_on_behalf(conn: &mut PgConnection, ctx: &RequestContext, id: Uuid) -> Result<(), AppError> {
    let model = Model::load(conn).await?;
    let before = must_detail(conn, &model, id, None).await?;
    if before.summary.deleted_at.is_none() {
        return Ok(());
    }
    data::restore(conn, id).await?;
    let after = must_detail(conn, &model, id, None).await?;
    let entry = AuditEntry {
        action: AuditAction::Restore,
        entity_type: "configuration_items",
        entity_id: id,
        old_value: Some(crud::json(&before)),
        new_value: Some(crud::json(&after)),
    };
    crud::write_audit(conn, ctx, vec![entry]).await?;
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
    // Process records are not part of the graph.
    let visible = class_data::asset_scope(&mut conn, ctx.class_scope(ClassOp::View).as_deref()).await?;
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
    use serde_json::json;

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
            credential: crate::auth::Credential::Token { profile_id: None },
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

    /// T1 (SHAA-799): the required-field result of a PATCH comes from the CI's
    /// values plus the change, before anything is written. Clearing a required
    /// value is refused with `required`, and nothing changes.
    #[tokio::test]
    async fn patch_refuses_clearing_a_required_value_before_writing() {
        let Some(db) = scratch::database("patch_refuses_clearing_a_required_value_before_writing").await else {
            return;
        };
        let pool = &db.pool;
        crate::seed::install_template(pool, "it_infrastructure").await.unwrap();
        let (server, in_service) = (id_of(pool, "ci_classes", "server").await, status(pool, "in_service").await);
        let editor = user_ctx(false);
        let body = parse::<CreateItemBody>(json!({ "classId": server,
            "attributes": { "name": "web-01", "status": in_service, "hostname": "web-01.example.com" } }));
        let Ok(body) = body else { panic!("invalid create body") };
        let ci = create(pool, &editor, &body).await.unwrap().summary;
        let update_body = |v: Value| parse::<UpdateItemBody>(v).unwrap_or_else(|_| panic!("invalid update body"));
        let audit_rows = || async {
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM audit_log WHERE entity_id = $1")
                .bind(ci.id)
                .fetch_one(pool)
                .await
                .unwrap()
        };
        let audited = audit_rows().await;

        for patch in [
            json!({ "attributes": { "name": null } }),
            json!({ "attributes": { "status": null, "hostname": "web-02.example.com" } }),
            json!({ "attributes": { "name": null, "status": null } }),
        ] {
            let err = update(pool, &editor, ci.id, &update_body(patch.clone())).await.unwrap_err();
            assert_eq!(err.code, ErrorCode::ValidationError, "{patch}");
            let codes: Vec<&str> = err.details.iter().flatten().map(|d| d.code.as_str()).collect();
            assert!(!codes.is_empty() && codes.iter().all(|c| *c == "required"), "{patch}: {codes:?}");
        }
        let after = get(pool, &editor, ci.id).await.unwrap();
        assert_eq!(after.summary.version, ci.version, "nothing was written");
        assert_eq!(after.attributes.get("name"), Some(&json!("web-01")));
        assert_eq!(audit_rows().await, audited);

        // Clearing an optional value and replacing a required one pass.
        let ok =
            update(pool, &editor, ci.id, &update_body(json!({ "attributes": { "hostname": null, "name": "web-1" } })))
                .await
                .unwrap();
        assert_eq!(ok.summary.label, "web-1");
        assert!(!ok.attributes.contains_key("hostname"));
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
            credential: crate::auth::Credential::Token { profile_id: None },
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

    fn histogram_query(raw: &str) -> ChangeHistogramQuery {
        use crate::api::route::{Query, QueryInput};
        match Query::<ChangeHistogramQuery>::parse(Some(raw)) {
            Ok(Query(q)) => q,
            Err(e) => panic!("{raw}: {:?}", e.details),
        }
    }

    /// SHAA-1687: audit entries per bucket on the CIs of the list query, split
    /// into created / statusChanged / updated, with the audit log's visibility.
    #[tokio::test]
    async fn change_histogram_counts_ci_history_per_bucket() {
        let Some(db) = scratch::database("items_change_histogram").await else { return };
        let pool = &db.pool;
        crate::seed::install_template(pool, "it_infrastructure").await.unwrap();
        crate::seed::seed_demo_data(pool).await.unwrap();
        let ci = |label: &'static str| async move {
            sqlx::query_as::<_, (Uuid, Uuid)>("SELECT id, class_id FROM configuration_items WHERE label = $1")
                .bind(label)
                .fetch_one(pool)
                .await
                .unwrap()
        };
        let (srv, server) = ci("fra1-esx-01").await;
        let (vm, vm_class) = ci("crm-app-01").await;
        let (in_service, retired) = (status(pool, "in_service").await, status(pool, "retired").await);
        let with_status = |s: Uuid| json!({ "classId": server, "attributes": { "status": s, "name": "x" } });
        let insert = |at: &'static str, action: &'static str, ci: Uuid, old: Option<Value>, new: Option<Value>| async move {
            sqlx::query(
                "INSERT INTO audit_log (occurred_at, actor_type, actor_name, action, entity_type, entity_id, old_value, new_value)
                 VALUES ($1::timestamptz, 'user', 'admin', $2, 'configuration_items', $3, $4, $5)",
            )
            .bind(at)
            .bind(action)
            .bind(ci)
            .bind(old)
            .bind(new)
            .execute(pool)
            .await
            .unwrap();
        };
        insert("2026-01-05T10:15:00Z", "create", srv, None, Some(with_status(in_service))).await;
        insert("2026-01-05T10:30:00Z", "update", srv, Some(with_status(in_service)), Some(with_status(retired))).await;
        insert(
            "2026-01-05T10:20:00Z",
            "update",
            vm,
            Some(json!({ "classId": vm_class })),
            Some(json!({ "classId": vm_class })),
        )
        .await;
        insert("2026-01-05T11:05:00Z", "update", srv, Some(with_status(retired)), Some(with_status(retired))).await;
        insert("2026-01-05T11:10:00Z", "export", srv, None, Some(json!({ "kind": "impact" }))).await;
        insert("2026-01-05T09:59:59Z", "update", srv, Some(with_status(retired)), Some(with_status(in_service))).await;
        insert("2026-01-05T13:00:00Z", "delete", srv, Some(with_status(retired)), None).await;

        let range = "from=2026-01-05T10:00:00Z&to=2026-01-05T13:00:00Z&active=all";
        let counts = |h: &ChangeHistogram| -> Vec<(i64, i64, i64)> {
            h.buckets.iter().map(|b| (b.created, b.updated, b.status_changed)).collect()
        };
        let admin = RequestContext::system("test", "test");
        let all = change_histogram(pool, &admin, &histogram_query(range)).await.unwrap();
        assert_eq!(all.bucket, HistogramBucket::Hour);
        assert_eq!(
            all.buckets.iter().map(|b| b.start.to_rfc3339()).collect::<Vec<_>>(),
            ["2026-01-05T10:00:00+00:00", "2026-01-05T11:00:00+00:00", "2026-01-05T12:00:00+00:00"]
        );
        assert_eq!(counts(&all), [(1, 1, 1), (0, 1, 0), (0, 0, 0)]);
        assert_eq!(all.total, 4);

        // The list filters choose the CIs.
        let vms =
            change_histogram(pool, &admin, &histogram_query(&format!("{range}&classId={vm_class}"))).await.unwrap();
        assert_eq!((counts(&vms), vms.total), (vec![(0, 1, 0), (0, 0, 0), (0, 0, 0)], 1));

        // A caller limited to some classes counts only those CIs.
        let restricted = crate::modules::audit::tests::viewer(&[server]);
        let mine = change_histogram(pool, &restricted, &histogram_query(range)).await.unwrap();
        assert_eq!((counts(&mine), mine.total), (vec![(1, 0, 1), (0, 1, 0), (0, 0, 0)], 3));

        // Day buckets start at midnight UTC; the partial first day counts only from `from`.
        let day = "from=2026-01-05T10:00:00Z&to=2026-01-06T10:00:00Z&bucket=day&active=all";
        let days = change_histogram(pool, &admin, &histogram_query(day)).await.unwrap();
        assert_eq!(
            days.buckets.iter().map(|b| b.start.to_rfc3339()).collect::<Vec<_>>(),
            ["2026-01-05T00:00:00+00:00", "2026-01-06T00:00:00+00:00"]
        );
        assert_eq!((counts(&days), days.total), (vec![(1, 3, 1), (0, 0, 0)], 5));

        // Defaults: the last 24 hours, hourly.
        let recent = change_histogram(pool, &admin, &histogram_query("")).await.unwrap();
        assert_eq!(recent.to - recent.from, TimeDelta::hours(24));
        assert!(matches!(recent.buckets.len(), 24 | 25), "{}", recent.buckets.len());

        // The range is capped per bucket width, and must not be empty.
        for (raw, code) in [
            ("from=2026-01-01T00:00:00Z&to=2026-01-08T00:00:01Z", "range_too_large"),
            ("from=2026-01-01T00:00:00Z&to=2026-04-02T00:00:00Z&bucket=day", "range_too_large"),
            ("from=2026-01-02T00:00:00Z&to=2026-01-02T00:00:00Z", "invalid_range"),
        ] {
            let err = change_histogram(pool, &admin, &histogram_query(raw)).await.unwrap_err();
            assert_eq!(fields(&err), ["from"], "{raw}");
            assert_eq!(err.details.iter().flatten().next().unwrap().code, code, "{raw}");
        }
        assert!(
            change_histogram(pool, &admin, &histogram_query("from=2026-01-01T00:00:00Z&to=2026-01-08T00:00:00Z"))
                .await
                .is_ok()
        );
        db.drop().await;
    }
}
