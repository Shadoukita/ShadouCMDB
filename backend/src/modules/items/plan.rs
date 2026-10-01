//! Plan and apply for CI writes (D10 of the bulk import spec, SHAA-714).
//!
//! A write is **planned** first and **applied** afterwards. Planning checks
//! the rights, validates every attribute value against the class, and decides
//! the values to set and clear; it writes nothing. What the checks need to
//! know about other rows (lookup values, reference targets, parent values)
//! comes from a [`Resolver`]. `POST`/`PATCH /configuration-items` use a
//! [`DbResolver`] that loads what one body needs; bulk import plans many rows
//! with a resolver it preloads per chunk. Both run the same checks here, so an
//! import can never store what the API would refuse.
//!
//! The required-field check of an update is computed from the CI's current
//! values plus the change, before anything is written (there is no
//! write-then-check).

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use super::schemas::{ConfigurationItem, CreateItemBody, UpdateItemBody};
use crate::api::context::RequestContext;
use crate::api::{pg_error, validate};
use crate::auth::permissions::ClassOp;
use crate::data::classes::EffectiveAttributeRow;
use crate::data::items::{self as data, StoredValue};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::modules::classes::AttributeDataType;
use crate::schema::model::{Field, Model};

// ---------------------------------------------------------------------------
// Resolver: what the checks need to know about other rows
// ---------------------------------------------------------------------------

/// Answers the questions attribute validation asks about other rows. The
/// answers must reflect the caller's view scope: a CI the caller may not view
/// is reported exactly like a missing one, so writes are no existence oracle.
pub trait Resolver {
    /// `Some(is_active)` when `value_id` is a value of `list_id`, else `None`.
    fn lookup_state(&self, list_id: Uuid, value_id: Uuid) -> Option<bool>;
    /// The value of the parent list that `value_id` belongs to.
    fn lookup_parent(&self, value_id: Uuid) -> Option<Uuid>;
    /// The class of a live CI the caller may view; `None` when it is missing,
    /// deleted or in a class the caller may not view.
    fn visible_target(&self, ci_id: Uuid) -> Option<Uuid>;
}

/// The ids a plan will ask its resolver about: the lookup values and CIs that
/// the input (and, for an update, the CI's current values) refer to. Values
/// that are not uuids are left out; validation reports them.
#[derive(Debug, Default, Clone)]
pub struct Needs {
    pub lookup_values: Vec<Uuid>,
    pub cis: Vec<Uuid>,
}

impl Needs {
    /// Adds the lookup and reference values of `attributes`.
    pub fn add(&mut self, defs: &[EffectiveAttributeRow], attributes: Option<&Map<String, Value>>) {
        for (key, value) in attributes.into_iter().flatten() {
            let Some(def) = defs.iter().find(|d| d.key == *key) else { continue };
            let Some(id) = value.as_str().and_then(|s| Uuid::parse_str(s).ok()) else { continue };
            match def.data_type {
                AttributeDataType::Lookup => self.lookup_values.push(id),
                AttributeDataType::Reference => self.cis.push(id),
                _ => {}
            }
        }
    }

    /// The needs of a create: the input with the class's defaults filled in.
    pub fn for_create(defs: &[EffectiveAttributeRow], input: Option<&Map<String, Value>>) -> Needs {
        let mut needs = Needs::default();
        needs.add(defs, Some(&with_defaults(defs, input)));
        needs
    }

    /// The needs of an update: the input and the CI's current values.
    pub fn for_update(
        defs: &[EffectiveAttributeRow],
        input: Option<&Map<String, Value>>,
        current: &Map<String, Value>,
    ) -> Needs {
        let mut needs = Needs::default();
        needs.add(defs, input);
        needs.add(defs, Some(current));
        needs
    }
}

/// A [`Resolver`] backed by the database: [`DbResolver::load`] reads what a
/// set of [`Needs`] asks about (one query each for lookup values and CIs).
#[derive(Debug, Default)]
pub struct DbResolver {
    /// value id -> (list id, is_active, parent value id)
    values: HashMap<Uuid, (Uuid, bool, Option<Uuid>)>,
    /// live CI id -> class id, only CIs in classes the caller may view
    cis: HashMap<Uuid, Uuid>,
}

impl DbResolver {
    /// `visible` is the caller's view scope (`None`: every class).
    pub async fn load(conn: &mut PgConnection, visible: Option<&[Uuid]>, needs: &Needs) -> sqlx::Result<DbResolver> {
        let mut out = DbResolver::default();
        if !needs.lookup_values.is_empty() {
            let rows: Vec<(Uuid, Uuid, bool, Option<Uuid>)> = sqlx::query_as(
                "SELECT id, list_id, is_active, parent_value_id FROM cmdb.lookup_list_values WHERE id = ANY($1)",
            )
            .bind(&needs.lookup_values)
            .fetch_all(&mut *conn)
            .await?;
            out.values = rows.into_iter().map(|(id, list, active, parent)| (id, (list, active, parent))).collect();
        }
        out.cis = data::live_items(conn, &needs.cis)
            .await?
            .into_iter()
            .filter(|(_, class_id)| is_visible(visible, *class_id))
            .collect();
        Ok(out)
    }
}

impl Resolver for DbResolver {
    fn lookup_state(&self, list_id: Uuid, value_id: Uuid) -> Option<bool> {
        self.values.get(&value_id).filter(|(list, _, _)| *list == list_id).map(|(_, active, _)| *active)
    }

    fn lookup_parent(&self, value_id: Uuid) -> Option<Uuid> {
        self.values.get(&value_id).and_then(|(_, _, parent)| *parent)
    }

    fn visible_target(&self, ci_id: Uuid) -> Option<Uuid> {
        self.cis.get(&ci_id).copied()
    }
}

pub(crate) fn is_visible(visible: Option<&[Uuid]>, class_id: Uuid) -> bool {
    visible.is_none_or(|classes| classes.contains(&class_id))
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
            // Line breaks, tabs and bidi controls only in multiline text (GH#289).
            if rules.get("multiline") == Some(&Value::Bool(true)) {
                s["x-multiline"] = true.into();
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

/// The CI's reference values by attribute definition id, from its `attributes`.
/// Resending one of them is no change: it is accepted as is, without the live
/// and view checks, so it tells nothing about a target the caller may not
/// view (GH#269).
pub fn current_refs(model: &Model, class_id: Uuid, attributes: &Map<String, Value>) -> HashMap<Uuid, Uuid> {
    model
        .lineage(class_id)
        .into_iter()
        .flat_map(|c| model.own_fields(c.id))
        .filter(|f| f.data_type == AttributeDataType::Reference)
        .filter_map(|f| Some((f.id, attributes.get(&f.key)?.as_str().and_then(|v| Uuid::parse_str(v).ok())?)))
        .collect()
}

/// The values a write sets and clears.
#[derive(Debug)]
pub struct Prepared<'d> {
    pub set: Vec<(&'d EffectiveAttributeRow, StoredValue)>,
    pub clear: Vec<Uuid>,
}

pub(crate) fn body_error(field: String, message: impl Into<String>, code: &str) -> FieldError {
    FieldError { location: FieldLocation::Body, field, message: message.into(), code: code.into() }
}

fn errors_or<T>(errors: Vec<FieldError>, ok: T) -> Result<T, AppError> {
    if errors.is_empty() { Ok(ok) } else { Err(AppError::validation(errors)) }
}

/// Validates attribute input against the class's effective definitions.
/// `lenient_clear` accepts `null` for keys the class does not define (they are
/// being cleared as part of a class change). A reference the caller may not
/// make fails exactly like one to a missing CI, so writes are no existence
/// oracle. A reference the attribute already has (`current`) is left out of
/// the result: resending it changes nothing.
fn prepare_attributes<'d>(
    defs: &'d [EffectiveAttributeRow],
    input: Option<&Map<String, Value>>,
    class_key: &str,
    self_id: Option<Uuid>,
    lenient_clear: bool,
    current: Option<&HashMap<Uuid, Uuid>>,
    resolver: &dyn Resolver,
) -> Result<Prepared<'d>, AppError> {
    let by_key: HashMap<&str, &EffectiveAttributeRow> = defs.iter().map(|d| (d.key.as_str(), d)).collect();
    let mut errors = Vec::new();
    let mut prepared = Prepared { set: Vec::new(), clear: Vec::new() };

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
            StoredValue::Reference(id) if current.and_then(|c| c.get(&def.id)) == Some(&id) => continue,
            StoredValue::Reference(id) if Some(id) == self_id => {
                errors.push(body_error(field, "A CI cannot reference itself", "reference_self"));
                continue;
            }
            StoredValue::Reference(id) if resolver.visible_target(id).is_none() => {
                errors.push(body_error(field, "Referenced CI does not exist or is deleted", "not_found"));
                continue;
            }
            StoredValue::Lookup(id) => match resolver.lookup_state(def.lookup_list_id.unwrap_or_default(), id) {
                Some(true) => {}
                Some(false) => {
                    errors.push(body_error(field, "This list value is retired", "lookup_value_inactive"));
                    continue;
                }
                None => {
                    errors.push(body_error(field, "Not a value of this attribute's list", "not_found"));
                    continue;
                }
            },
            _ => {}
        }
        prepared.set.push((def, stored));
    }
    errors_or(errors, prepared)
}

/// A reference must point at a CI of the field's class (or a subclass); the
/// foreign key only guarantees that the CI exists.
fn check_reference_classes(model: &Model, prepared: &Prepared<'_>, resolver: &dyn Resolver) -> Result<(), AppError> {
    let mut errors = Vec::new();
    for (def, value) in &prepared.set {
        let (StoredValue::Reference(target), Some(class)) = (value, def.reference_class_id) else { continue };
        let Some(target_class) = resolver.visible_target(*target) else { continue };
        if !model.lineage(target_class).iter().any(|c| c.id == class) {
            errors.push(body_error(
                format!("attributes.{}", def.key),
                "Referenced CI is not of the class this field points at",
                "reference_class",
            ));
        }
    }
    errors_or(errors, ())
}

/// Dependent dropdowns: a value of a field with a parent field must belong to
/// the CI's value of that field (`current` holds the CI's values before this
/// write). Checked for the fields this write sets or clears and for those
/// whose parent field it sets or clears, so an unrelated edit of a CI whose
/// values predate the rule is not refused.
fn check_parent_values(
    defs: &[EffectiveAttributeRow],
    prepared: &Prepared<'_>,
    current: Option<&Map<String, Value>>,
    resolver: &dyn Resolver,
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
    let mut errors = Vec::new();
    for (def, parent_def) in dependent {
        if !(touched.contains(&def.id) || touched.contains(&parent_def.id)) {
            continue;
        }
        let Some(child) = value_of(def) else { continue };
        let field = format!("attributes.{}", def.key);
        match value_of(parent_def) {
            None => errors.push(body_error(
                field,
                format!("Choose {} first; this value depends on it", parent_def.label),
                "lookup_parent_missing",
            )),
            Some(parent) if resolver.lookup_parent(child) != Some(parent) => errors.push(body_error(
                field,
                format!("Not a value of the chosen {}", parent_def.label),
                "lookup_parent_mismatch",
            )),
            Some(_) => {}
        }
    }
    errors_or(errors, ())
}

/// The input plus the default of every active attribute it leaves out.
pub fn with_defaults(defs: &[EffectiveAttributeRow], input: Option<&Map<String, Value>>) -> Map<String, Value> {
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

fn required_error(d: &EffectiveAttributeRow) -> FieldError {
    body_error(format!("attributes.{}", d.key), format!("{} is required", d.label), "required")
}

/// Attribute checks for a new CI: the values (defaults filled in), the classes
/// they reference, their parent values, and that every required attribute has one.
pub fn prepare_new<'d>(
    model: &Model,
    defs: &'d [EffectiveAttributeRow],
    input: Option<&Map<String, Value>>,
    class_key: &str,
    resolver: &dyn Resolver,
) -> Result<Prepared<'d>, AppError> {
    let attributes = with_defaults(defs, input);
    let prepared = prepare_attributes(defs, Some(&attributes), class_key, None, false, None, resolver)?;
    check_reference_classes(model, &prepared, resolver)?;
    check_parent_values(defs, &prepared, None, resolver)?;
    let given: HashSet<Uuid> = prepared.set.iter().map(|(d, _)| d.id).collect();
    let missing: Vec<FieldError> =
        defs.iter().filter(|d| d.is_required && d.is_active && !given.contains(&d.id)).map(required_error).collect();
    errors_or(missing, prepared)
}

/// Attribute checks for a change of an existing CI (`before`), moving to
/// `class_id` when it differs. Everything except the required fields, which
/// [`plan_update`] checks against the CI's values after the change.
pub fn prepare_changed<'d>(
    model: &Model,
    defs: &'d [EffectiveAttributeRow],
    before: &ConfigurationItem,
    class_id: Uuid,
    input: Option<&Map<String, Value>>,
    resolver: &dyn Resolver,
) -> Result<Prepared<'d>, AppError> {
    let key = class_key(model, class_id)?;
    let refs = current_refs(model, before.summary.class_id, &before.attributes);
    let class_changes = class_id != before.summary.class_id;
    let id = Some(before.summary.id);
    let prepared = prepare_attributes(defs, input, key, id, class_changes, Some(&refs), resolver)?;
    check_reference_classes(model, &prepared, resolver)?;
    check_parent_values(defs, &prepared, Some(&before.attributes), resolver)?;
    Ok(prepared)
}

pub fn class_key(model: &Model, class_id: Uuid) -> Result<&str, AppError> {
    match model.class(class_id) {
        Some(c) => Ok(&c.key),
        None => Err(AppError::field("classId", "CI class does not exist", "not_found")),
    }
}

// ---------------------------------------------------------------------------
// Plans
// ---------------------------------------------------------------------------

/// The registry side of a planned write.
#[derive(Debug, Clone)]
pub enum Registry {
    Create {
        /// `None`: generated by the database.
        id: Option<Uuid>,
        class_id: Uuid,
        /// `None`: generated by the database.
        ident: Option<String>,
        valid_from: Option<DateTime<Utc>>,
        valid_until: Option<DateTime<Utc>>,
        criticality_value_id: Option<Uuid>,
    },
    Update {
        id: Uuid,
        /// The CI's class before the write.
        old_class_id: Uuid,
        /// `Some` when the write moves the CI to another class.
        new_class_id: Option<Uuid>,
        /// `Some` when the write changes the ident.
        ident: Option<String>,
        valid_from: Option<DateTime<Utc>>,
        valid_until: Option<Option<DateTime<Utc>>>,
        /// `Some(None)` clears the criticality.
        criticality_value_id: Option<Option<Uuid>>,
    },
}

/// A checked write, ready for [`apply`]. Nothing has been written yet.
#[derive(Debug)]
pub struct Plan<'d> {
    pub registry: Registry,
    pub set: Vec<(&'d EffectiveAttributeRow, StoredValue)>,
    pub clear: Vec<Uuid>,
    /// The CI before the write (the audit record's old value); `None` for a create.
    pub before: Option<ConfigurationItem>,
}

impl Plan<'_> {
    /// The CI's class after the write.
    pub fn class_id(&self) -> Uuid {
        match &self.registry {
            Registry::Create { class_id, .. } => *class_id,
            Registry::Update { old_class_id, new_class_id, .. } => new_class_id.unwrap_or(*old_class_id),
        }
    }
}

/// Plans a new CI of `input.class_id`. `defs` are that class's effective
/// attribute definitions. Needs create on the class, and the Administrator
/// profile to set the ident.
pub fn plan_create<'d>(
    ctx: &RequestContext,
    model: &Model,
    defs: &'d [EffectiveAttributeRow],
    input: &CreateItemBody,
    resolver: &dyn Resolver,
) -> Result<Plan<'d>, AppError> {
    ctx.require_class(input.class_id, ClassOp::Create)?;
    if input.ident.is_some() {
        ctx.require_administrator("set a CI's ident")?;
    }
    let key = class_key(model, input.class_id)?;
    let prepared = prepare_new(model, defs, input.attributes.as_ref(), key, resolver)?;
    Ok(Plan {
        registry: Registry::Create {
            id: None,
            class_id: input.class_id,
            ident: input.ident.clone(),
            valid_from: input.valid_from,
            valid_until: input.valid_until,
            criticality_value_id: input.criticality_value_id,
        },
        set: prepared.set,
        clear: prepared.clear,
        before: None,
    })
}

/// Plans a change of the CI `before` (its current, unredacted representation,
/// read under a row lock). `defs` are the effective attribute definitions of
/// the class it has after the write. Needs view and edit on its class, create
/// on the class it moves to, and the Administrator profile to change the ident.
/// `service_class` is the business service class, which no CI enters or leaves
/// (None where the caller never changes a CI's class).
pub fn plan_update<'d>(
    ctx: &RequestContext,
    model: &Model,
    defs: &'d [EffectiveAttributeRow],
    before: ConfigurationItem,
    input: &UpdateItemBody,
    resolver: &dyn Resolver,
    service_class: Option<Uuid>,
) -> Result<Plan<'d>, AppError> {
    let id = before.summary.id;
    let old_class_id = before.summary.class_id;
    ctx.require_class_visible(old_class_id, "Configuration item", id)?;
    ctx.require_class(old_class_id, ClassOp::Edit)?;
    // Moving a CI to another class also needs create rights there.
    let new_class_id = input.class_id.filter(|c| *c != old_class_id);
    if let Some(new_class) = new_class_id {
        ctx.require_class(new_class, ClassOp::Create)?;
    }
    if before.summary.deleted_at.is_some() {
        return Err(AppError::conflict("This configuration item is deleted and cannot be modified"));
    }
    // A business service keeps its type, and no CI becomes one, whatever its
    // members and owners: the same refusal for every service, so it never tells
    // whether one has members the caller may not view (§3.2). The database
    // trigger configuration_items_keep_service stays as the backstop.
    if let (Some(new_class), Some(service)) = (new_class_id, service_class)
        && (old_class_id == service || new_class == service)
    {
        return Err(AppError::field(
            "classId",
            "A business service cannot change its class, and a configuration item cannot become a business service. \
             Create a new item of the class you need instead.",
            "business_service_class",
        ));
    }
    // The validity period is checked against the stored dates the request leaves
    // unchanged, so the order is never left to a database constraint.
    let valid_from = input.valid_from.unwrap_or(before.summary.valid_from);
    let valid_until = input.valid_until.unwrap_or(before.summary.valid_until);
    if (input.valid_from.is_some() || input.valid_until.is_some())
        && valid_until.is_some_and(|until| until <= valid_from)
    {
        let (field, message) = match input.valid_until {
            Some(_) => ("validUntil", "Must be after validFrom"),
            None => ("validFrom", "Must be before validUntil"),
        };
        return Err(AppError::field(field, message, "custom"));
    }
    // Resending the current ident (a form saving every field) is no change.
    let new_ident = input.ident.as_deref().filter(|i| *i != before.summary.ident);
    if new_ident.is_some() {
        ctx.require_administrator("change a CI's ident")?;
    }
    if let Some(sent) = input.version
        && sent != before.summary.version
    {
        let current = before.summary.version;
        return Err(AppError::new(
            ErrorCode::VersionConflict,
            format!("The item was changed by someone else (you sent version {sent}, current is {current}). Reload and retry."),
        )
        .with_details(vec![body_error("version".into(), format!("Current version is {current}"), "stale")]));
    }

    let class_id = new_class_id.unwrap_or(old_class_id);
    let prepared = prepare_changed(model, defs, &before, class_id, input.attributes.as_ref(), resolver)?;

    if new_class_id.is_some() {
        // Values the new class does not define must be cleared in the same request.
        let keep: HashSet<&str> = defs.iter().map(|d| d.key.as_str()).collect();
        let cleared: HashSet<&str> =
            input.attributes.iter().flatten().filter(|(_, v)| v.is_null()).map(|(k, _)| k.as_str()).collect();
        let orphaned: Vec<&str> = before
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
    }

    // Required fields, from the values the CI keeps plus the change. A value
    // is kept when its field's table stays in the CI's lineage.
    let new_lineage: HashSet<Uuid> = model.lineage(class_id).iter().map(|c| c.id).collect();
    let mut present: HashSet<Uuid> = model
        .lineage(old_class_id)
        .into_iter()
        .filter(|c| new_lineage.contains(&c.id))
        .flat_map(|c| model.own_fields(c.id))
        .filter(|f| before.attributes.get(&f.key).is_some_and(|v| !v.is_null()))
        .map(|f| f.id)
        .collect();
    for id in &prepared.clear {
        present.remove(id);
    }
    present.extend(prepared.set.iter().map(|(d, _)| d.id));
    let missing: Vec<FieldError> =
        defs.iter().filter(|d| d.is_required && d.is_active && !present.contains(&d.id)).map(required_error).collect();
    let prepared = errors_or(missing, prepared)?;

    Ok(Plan {
        registry: Registry::Update {
            id,
            old_class_id,
            new_class_id,
            ident: new_ident.map(str::to_owned),
            valid_from: input.valid_from,
            valid_until: input.valid_until,
            criticality_value_id: input.criticality_value_id,
        },
        set: prepared.set,
        clear: prepared.clear,
        before: Some(before),
    })
}

// ---------------------------------------------------------------------------
// Apply
// ---------------------------------------------------------------------------

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

/// Writes a plan's registry and type rows, without refreshing the label (a
/// caller applying many plans refreshes the labels once for all of them).
/// Returns the CI's id.
pub async fn apply_rows(conn: &mut PgConnection, model: &Model, plan: &Plan<'_>) -> Result<Uuid, AppError> {
    match &plan.registry {
        Registry::Create { id, class_id, ident, valid_from, valid_until, criticality_value_id } => {
            let new = data::NewItem {
                id: *id,
                class_id: *class_id,
                ident: ident.as_deref(),
                valid_from: *valid_from,
                valid_until: *valid_until,
                criticality_value_id: *criticality_value_id,
            };
            let id = data::insert(conn, &new).await.map_err(registry_write_error)?;
            let lineage: Vec<Uuid> = model.lineage(*class_id).iter().map(|c| c.id).collect();
            write_type_rows(conn, model, id, *class_id, &plan.set, &[], &lineage).await?;
            Ok(id)
        }
        Registry::Update { id, old_class_id, new_class_id, ident, valid_from, valid_until, criticality_value_id } => {
            let class_id = new_class_id.unwrap_or(*old_class_id);
            let old_lineage: Vec<Uuid> = model.lineage(*old_class_id).iter().map(|c| c.id).collect();
            let new_lineage: Vec<Uuid> = model.lineage(class_id).iter().map(|c| c.id).collect();
            // Rows in the tables of classes the CI leaves go (their values were cleared in the plan).
            for gone in old_lineage.iter().filter(|c| !new_lineage.contains(c)) {
                if let Some(table) = model.table(*gone) {
                    data::delete_type_rows(conn, &table, &[*id]).await?;
                }
            }
            let patch = data::ItemPatch {
                class_id: *new_class_id,
                ident: ident.as_deref(),
                valid_from: *valid_from,
                valid_until: *valid_until,
                criticality_value_id: *criticality_value_id,
            };
            data::update(conn, *id, &patch).await.map_err(registry_write_error)?;
            let entering: Vec<Uuid> = new_lineage.iter().filter(|c| !old_lineage.contains(c)).copied().collect();
            write_type_rows(conn, model, *id, class_id, &plan.set, &plan.clear, &entering).await?;
            Ok(*id)
        }
    }
}

/// Writes a plan and recomputes the CI's label. Returns the CI's id.
pub async fn apply(conn: &mut PgConnection, model: &Model, plan: &Plan<'_>) -> Result<Uuid, AppError> {
    let id = apply_rows(conn, model, plan).await?;
    data::refresh_labels(conn, model, &[plan.class_id()], Some(&[id])).await?;
    Ok(id)
}
