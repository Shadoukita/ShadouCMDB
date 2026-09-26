//! CI classes (with the effective-attributes view), attribute definitions,
//! relationship types and relationship rules.

use axum::http::Method;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Number, Value};
use sqlx::PgConnection;
use sqlx::types::Json as SqlJson;
use utoipa::openapi::schema::{ArrayBuilder, ObjectBuilder, Schema, Type};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::simple_resource::{self as simple, BoxFuture, ListQuery, Resource, Writable, bool_filter, non_empty};
use crate::api::route::{Check, IdPath, In, Json, NoBody, Query, Route, route};
use crate::api::schemas::{
    self, IdOrNone, QueryBool, Sort, UuidList, description_schema, key_schema, name_schema, nullable_uuid_schema,
    sort_order_schema, trimmed, ts,
};
use crate::api::validate;
use crate::data::classes as data;
use crate::data::crud::{ColumnSet, Where};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::paged;

fn custom(field: &str, message: impl Into<String>) -> FieldError {
    FieldError { location: FieldLocation::Body, field: field.into(), message: message.into(), code: "custom".into() }
}

// ===========================================================================
// CI classes
// ===========================================================================

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CiClass {
    pub id: Uuid,
    pub key: String,
    pub name: String,
    #[schema(required = true)]
    pub description: Option<String>,
    /// Parent class; attributes and relationship rules are inherited from it
    #[schema(required = true)]
    pub parent_id: Option<Uuid>,
    /// Abstract classes group attributes and rules but cannot hold CIs
    pub is_abstract: bool,
    #[schema(required = true)]
    pub icon: Option<String>,
    pub is_active: bool,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
}

fn icon_schema() -> Schema {
    schemas::nullable_string_schema(100)
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CiClassCreate {
    #[schema(schema_with = key_schema)]
    key: String,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    name: String,
    #[schema(schema_with = description_schema)]
    #[serde(default)]
    description: Option<String>,
    #[schema(schema_with = nullable_uuid_schema)]
    #[serde(default)]
    parent_id: Option<Uuid>,
    #[schema(nullable = false)]
    is_abstract: Option<bool>,
    #[schema(schema_with = icon_schema)]
    #[serde(default)]
    icon: Option<String>,
    #[schema(nullable = false)]
    is_active: Option<bool>,
}

// `key` is immutable: imports, integrations and reports refer to it.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CiClassUpdate {
    #[schema(schema_with = name_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    name: Option<String>,
    #[schema(schema_with = description_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    description: Option<Option<String>>,
    #[schema(schema_with = nullable_uuid_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    parent_id: Option<Option<Uuid>>,
    #[schema(nullable = false)]
    is_abstract: Option<bool>,
    #[schema(schema_with = icon_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    icon: Option<Option<String>>,
    #[schema(nullable = false)]
    is_active: Option<bool>,
}

impl Writable for CiClassCreate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("key", Some(self.key.clone()))
            .opt("name", Some(self.name.clone()))
            .opt("description", self.description.clone().map(Some))
            .opt("parent_id", self.parent_id.map(Some))
            .opt("is_abstract", self.is_abstract)
            .opt("icon", self.icon.clone().map(Some))
            .opt("is_active", self.is_active);
        c
    }
}
impl Check for CiClassCreate {}

impl Writable for CiClassUpdate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("name", self.name.clone())
            .opt("description", self.description.clone())
            .opt("parent_id", self.parent_id)
            .opt("is_abstract", self.is_abstract)
            .opt("icon", self.icon.clone())
            .opt("is_active", self.is_active);
        c
    }
}
impl Check for CiClassUpdate {
    fn check(&self) -> Vec<FieldError> {
        non_empty(&self.columns())
    }
}

fn class_parent_schema() -> Schema {
    schemas::id_or_none_schema("Direct children of this class; \"none\" for root classes")
}

fn class_sort() -> Schema {
    schemas::sort_schema(&["name", "key", "createdAt", "updatedAt"], "name")
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct CiClassList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    offset: i64,
    #[param(schema_with = schemas::search_schema)]
    q: Option<String>,
    #[param(required = false, schema_with = class_sort)]
    sort: Sort,
    #[param(inline)]
    is_active: Option<QueryBool>,
    #[param(inline)]
    is_abstract: Option<QueryBool>,
    #[param(schema_with = class_parent_schema)]
    parent_id: Option<IdOrNone>,
    /// This class and every class below it
    descendant_of: Option<Uuid>,
}
paged!(CiClassList);

impl ListQuery for CiClassList {
    fn q(&self) -> Option<&str> {
        self.q.as_deref()
    }
    fn sort(&self) -> &Sort {
        &self.sort
    }
    fn filter(&self, w: &mut Where<'_>) {
        bool_filter(w, "is_active", self.is_active);
        bool_filter(w, "is_abstract", self.is_abstract);
        match self.parent_id {
            Some(IdOrNone::None) => w.and_sql("parent_id IS NULL"),
            Some(IdOrNone::Id(id)) => {
                w.and().push("parent_id = ").push_bind(id);
            }
            None => {}
        }
        if let Some(id) = self.descendant_of {
            w.and().push("ci_class_is_a(id, ").push_bind(id).push(")");
        }
    }
}

pub struct CiClasses;

impl Resource for CiClasses {
    type Dto = CiClass;
    type Create = CiClassCreate;
    type Update = CiClassUpdate;
    type List = CiClassList;
    const TABLE: &'static str = "ci_classes";
    const LABEL: &'static str = "CI class";
    const BASE_PATH: &'static str = "/api/v1/ci-classes";
    const TAG: &'static str = "CI classes";
    const SINGULAR: &'static str = "ciClass";
    const PLURAL: &'static str = "ciClasses";
    const COLUMNS: &'static str =
        "id, key, name, description, parent_id, is_abstract, icon, is_active, created_at, updated_at";
    const SEARCH_COLUMNS: &'static [&'static str] = &["key", "name", "description"];

    fn id(row: &CiClass) -> Uuid {
        row.id
    }

    fn after_write<'a>(
        conn: &'a mut PgConnection,
        row: &'a CiClass,
        previous: Option<&'a CiClass>,
    ) -> BoxFuture<'a, Result<(), AppError>> {
        Box::pin(async move {
            let Some(previous) = previous else { return Ok(()) };
            if row.is_abstract && !previous.is_abstract && data::class_has_items(conn, row.id).await? {
                return Err(AppError::field(
                    "isAbstract",
                    "Class still holds CIs; an abstract class cannot",
                    "class_has_items",
                ));
            }
            if row.parent_id != previous.parent_id {
                let orphaned = data::orphaned_attribute_values(conn, row.id).await?;
                if !orphaned.is_empty() {
                    return Err(AppError::field(
                        "parentId",
                        format!(
                            "CIs of this class hold values for attributes that the new parent does not provide: {}",
                            orphaned.join(", ")
                        ),
                        "attributes_outside_lineage",
                    ));
                }
            }
            Ok(())
        })
    }
}

// ===========================================================================
// Attribute definitions
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum AttributeDataType {
    Text,
    Number,
    Integer,
    Boolean,
    Enum,
    Date,
    Datetime,
    Ip,
    Cidr,
    Reference,
}

impl AttributeDataType {
    pub fn as_str(self) -> &'static str {
        match self {
            AttributeDataType::Text => "text",
            AttributeDataType::Number => "number",
            AttributeDataType::Integer => "integer",
            AttributeDataType::Boolean => "boolean",
            AttributeDataType::Enum => "enum",
            AttributeDataType::Date => "date",
            AttributeDataType::Datetime => "datetime",
            AttributeDataType::Ip => "ip",
            AttributeDataType::Cidr => "cidr",
            AttributeDataType::Reference => "reference",
        }
    }
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttributeDefinition {
    pub id: Uuid,
    pub class_id: Uuid,
    pub key: String,
    pub label: String,
    #[schema(required = true)]
    pub description: Option<String>,
    #[schema(inline)]
    pub data_type: AttributeDataType,
    pub is_required: bool,
    /// Allowed values when dataType is "enum"
    #[schema(value_type = Option<Vec<String>>, required = true)]
    pub enum_values: Option<SqlJson<Vec<String>>>,
    /// When dataType is "reference": the class (or ancestor) the referenced CI must belong to
    #[schema(required = true)]
    pub reference_class_id: Option<Uuid>,
    #[schema(value_type = Option<std::collections::HashMap<String, serde_json::Value>>, required = true)]
    pub validation: Option<SqlJson<Map<String, Value>>>,
    /// UI grouping, e.g. "Hardware"
    #[schema(required = true)]
    pub group_name: Option<String>,
    pub sort_order: i32,
    pub is_active: bool,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
}

// Where an inherited attribute is defined.
#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DefinedOn {
    pub id: Uuid,
    pub key: String,
    pub name: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EffectiveAttribute {
    pub id: Uuid,
    pub class_id: Uuid,
    pub key: String,
    pub label: String,
    #[schema(required = true)]
    pub description: Option<String>,
    #[schema(inline)]
    pub data_type: AttributeDataType,
    pub is_required: bool,
    /// Allowed values when dataType is "enum"
    #[schema(value_type = Option<Vec<String>>, required = true)]
    pub enum_values: Option<SqlJson<Vec<String>>>,
    /// When dataType is "reference": the class (or ancestor) the referenced CI must belong to
    #[schema(required = true)]
    pub reference_class_id: Option<Uuid>,
    #[schema(value_type = Option<std::collections::HashMap<String, serde_json::Value>>, required = true)]
    pub validation: Option<SqlJson<Map<String, Value>>>,
    /// UI grouping, e.g. "Hardware"
    #[schema(required = true)]
    pub group_name: Option<String>,
    pub sort_order: i32,
    pub is_active: bool,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
    /// Defined on an ancestor class rather than this one
    pub inherited: bool,
    #[schema(inline)]
    pub defined_on: DefinedOn,
}

/// All attributes a CI of this class can carry (not paginated; bounded by the class lineage)
#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EffectiveAttributeList {
    pub data: Vec<EffectiveAttribute>,
}

impl From<data::EffectiveAttributeRow> for EffectiveAttribute {
    fn from(r: data::EffectiveAttributeRow) -> Self {
        EffectiveAttribute {
            id: r.id,
            class_id: r.class_id,
            key: r.key,
            label: r.label,
            description: r.description,
            data_type: r.data_type,
            is_required: r.is_required,
            enum_values: r.enum_values,
            reference_class_id: r.reference_class_id,
            validation: r.validation,
            group_name: r.group_name,
            sort_order: r.sort_order,
            is_active: r.is_active,
            created_at: r.created_at,
            updated_at: r.updated_at,
            inherited: r.depth > 0,
            defined_on: DefinedOn { id: r.class_id, key: r.defined_on_key, name: r.defined_on_name },
        }
    }
}

/// Extra validation the API enforces on attribute values
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ValidationRules {
    /// number/integer: minimum
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(schema_with = min_schema)]
    pub min: Option<Number>,
    /// number/integer: maximum
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(schema_with = max_schema)]
    pub max: Option<Number>,
    /// text: maximum length
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, minimum = 1)]
    pub max_length: Option<i64>,
    /// text: regular expression the value must match
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, max_length = 500)]
    pub pattern: Option<String>,
    /// Display unit, e.g. "GB"
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, max_length = 20)]
    pub unit: Option<String>,
}

impl ValidationRules {
    fn check(&self, data_type: AttributeDataType, errors: &mut Vec<FieldError>) {
        let numeric = matches!(data_type, AttributeDataType::Number | AttributeDataType::Integer);
        if (self.min.is_some() || self.max.is_some()) && !numeric {
            errors.push(custom("validation", "min/max apply to number and integer attributes only"));
        }
        if (self.pattern.is_some() || self.max_length.is_some()) && data_type != AttributeDataType::Text {
            errors.push(custom("validation", "pattern/maxLength apply to text attributes only"));
        }
        if let (Some(min), Some(max)) = (&self.min, &self.max)
            && min.as_f64() > max.as_f64()
        {
            errors.push(custom("validation.min", "min must not exceed max"));
        }
        if let Some(p) = &self.pattern
            && validate::cached_regex(p).is_none()
        {
            errors.push(custom("validation.pattern", "Not a valid regular expression"));
        }
    }
}

fn number_schema(description: &str) -> Schema {
    ObjectBuilder::new().schema_type(Type::Number).description(Some(description)).into()
}
fn min_schema() -> Schema {
    number_schema("number/integer: minimum")
}
fn max_schema() -> Schema {
    number_schema("number/integer: maximum")
}

fn enum_values_schema() -> Schema {
    let item = ObjectBuilder::new()
        .schema_type(Type::String)
        .min_length(Some(1))
        .max_length(Some(200))
        .pattern(Some(schemas::NOT_BLANK_PATTERN));
    let array = ArrayBuilder::new().items(item).min_items(Some(1)).max_items(Some(500)).unique_items(true);
    utoipa::openapi::schema::AnyOfBuilder::new().item(array).item(ObjectBuilder::new().schema_type(Type::Null)).into()
}

fn validation_schema() -> Schema {
    utoipa::openapi::schema::AnyOfBuilder::new()
        .item(ValidationRules::schema_inline())
        .item(ObjectBuilder::new().schema_type(Type::Null))
        .into()
}

impl ValidationRules {
    fn schema_inline() -> utoipa::openapi::RefOr<Schema> {
        <ValidationRules as utoipa::PartialSchema>::schema()
    }
}

fn group_name_schema() -> Schema {
    utoipa::openapi::schema::AnyOfBuilder::new()
        .item(ObjectBuilder::new().schema_type(Type::String).max_length(Some(100)))
        .item(ObjectBuilder::new().schema_type(Type::Null))
        .into()
}

fn trimmed_list<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<Vec<String>>>, D::Error> {
    Ok(Some(Option::<Vec<String>>::deserialize(d)?.map(|v| v.into_iter().map(|s| s.trim().to_owned()).collect())))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttributeDefinitionCreate {
    class_id: Uuid,
    #[schema(schema_with = key_schema)]
    key: String,
    #[schema(inline)]
    data_type: AttributeDataType,
    #[schema(schema_with = nullable_uuid_schema)]
    #[serde(default)]
    reference_class_id: Option<Uuid>,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    label: String,
    #[schema(schema_with = description_schema)]
    #[serde(default)]
    description: Option<String>,
    #[schema(nullable = false)]
    is_required: Option<bool>,
    #[schema(schema_with = enum_values_schema)]
    #[serde(default, deserialize_with = "trimmed_list")]
    enum_values: Option<Option<Vec<String>>>,
    #[schema(schema_with = validation_schema)]
    #[serde(default)]
    validation: Option<ValidationRules>,
    #[schema(schema_with = group_name_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    group_name: Option<String>,
    #[schema(schema_with = sort_order_schema)]
    sort_order: Option<i32>,
    #[schema(nullable = false)]
    is_active: Option<bool>,
}

// `classId`, `key` and `dataType` are immutable: stored values depend on them.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttributeDefinitionUpdate {
    #[schema(schema_with = name_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    label: Option<String>,
    #[schema(schema_with = description_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    description: Option<Option<String>>,
    #[schema(nullable = false)]
    is_required: Option<bool>,
    #[schema(schema_with = enum_values_schema)]
    #[serde(default, deserialize_with = "trimmed_list")]
    enum_values: Option<Option<Vec<String>>>,
    #[schema(schema_with = validation_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    validation: Option<Option<ValidationRules>>,
    #[schema(schema_with = group_name_schema)]
    #[serde(default, deserialize_with = "schemas::patch_trimmed")]
    group_name: Option<Option<String>>,
    #[schema(schema_with = sort_order_schema)]
    sort_order: Option<i32>,
    #[schema(nullable = false)]
    is_active: Option<bool>,
}

fn json_list(v: &[String]) -> Value {
    Value::Array(v.iter().cloned().map(Value::String).collect())
}

fn json_rules(v: &ValidationRules) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

impl Writable for AttributeDefinitionCreate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("class_id", Some(self.class_id))
            .opt("key", Some(self.key.clone()))
            .opt("data_type", Some(self.data_type.as_str().to_owned()))
            .opt("reference_class_id", self.reference_class_id.map(Some))
            .opt("label", Some(self.label.clone()))
            .opt("description", self.description.clone().map(Some))
            .opt("is_required", self.is_required)
            .opt("enum_values", self.enum_values.clone().map(|v| v.map(|l| json_list(&l))))
            .opt("validation", self.validation.as_ref().map(|v| Some(json_rules(v))))
            .opt("group_name", self.group_name.clone().map(Some))
            .opt("sort_order", self.sort_order)
            .opt("is_active", self.is_active);
        c
    }
}

impl Check for AttributeDefinitionCreate {
    fn check(&self) -> Vec<FieldError> {
        let mut errors = Vec::new();
        let has_enum = matches!(self.enum_values, Some(Some(_)));
        let is_enum = self.data_type == AttributeDataType::Enum;
        let is_ref = self.data_type == AttributeDataType::Reference;
        if is_enum && !has_enum {
            errors.push(custom("enumValues", "Required for enum attributes"));
        }
        if !is_enum && has_enum {
            errors.push(custom("enumValues", "Only allowed for enum attributes"));
        }
        if is_ref && self.reference_class_id.is_none() {
            errors.push(custom("referenceClassId", "Required for reference attributes"));
        }
        if !is_ref && self.reference_class_id.is_some() {
            errors.push(custom("referenceClassId", "Only allowed for reference attributes"));
        }
        if let Some(v) = &self.validation {
            v.check(self.data_type, &mut errors);
        }
        errors
    }
}

impl Writable for AttributeDefinitionUpdate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("label", self.label.clone())
            .opt("description", self.description.clone())
            .opt("is_required", self.is_required)
            .opt("enum_values", self.enum_values.clone().map(|v| v.map(|l| json_list(&l))))
            .opt("validation", self.validation.as_ref().map(|v| v.as_ref().map(json_rules)))
            .opt("group_name", self.group_name.clone())
            .opt("sort_order", self.sort_order)
            .opt("is_active", self.is_active);
        c
    }
}
impl Check for AttributeDefinitionUpdate {
    fn check(&self) -> Vec<FieldError> {
        non_empty(&self.columns())
    }
}

fn defined_on_schema() -> Schema {
    schemas::uuid_list_described("Defined directly on these classes")
}

fn attribute_sort() -> Schema {
    schemas::sort_schema(&["sortOrder", "key", "label", "createdAt", "updatedAt"], "sortOrder")
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct AttributeDefinitionList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    offset: i64,
    #[param(schema_with = schemas::search_schema)]
    q: Option<String>,
    #[param(required = false, schema_with = attribute_sort)]
    sort: Sort,
    #[param(schema_with = defined_on_schema)]
    class_id: Option<UuidList>,
    /// Everything a CI of this class can carry, including inherited definitions
    effective_for_class_id: Option<Uuid>,
    #[param(inline)]
    data_type: Option<AttributeDataType>,
    #[param(inline)]
    is_active: Option<QueryBool>,
    #[param(inline)]
    is_required: Option<QueryBool>,
}
paged!(AttributeDefinitionList);

impl ListQuery for AttributeDefinitionList {
    fn q(&self) -> Option<&str> {
        self.q.as_deref()
    }
    fn sort(&self) -> &Sort {
        &self.sort
    }
    fn filter(&self, w: &mut Where<'_>) {
        if let Some(ids) = &self.class_id {
            w.and().push("class_id = ANY(").push_bind(ids.0.clone()).push(")");
        }
        if let Some(id) = self.effective_for_class_id {
            w.and().push("ci_class_is_a(").push_bind(id).push(", class_id)");
        }
        if let Some(t) = self.data_type {
            w.and().push("data_type = ").push_bind(t.as_str());
        }
        bool_filter(w, "is_active", self.is_active);
        bool_filter(w, "is_required", self.is_required);
    }
}

pub struct AttributeDefinitions;

impl Resource for AttributeDefinitions {
    type Dto = AttributeDefinition;
    type Create = AttributeDefinitionCreate;
    type Update = AttributeDefinitionUpdate;
    type List = AttributeDefinitionList;
    const TABLE: &'static str = "ci_attribute_definitions";
    const LABEL: &'static str = "Attribute definition";
    const BASE_PATH: &'static str = "/api/v1/attribute-definitions";
    const TAG: &'static str = "Attribute definitions";
    const SINGULAR: &'static str = "attributeDefinition";
    const PLURAL: &'static str = "attributeDefinitions";
    const COLUMNS: &'static str = "id, class_id, key, label, description, data_type, is_required, enum_values, reference_class_id, validation, group_name, sort_order, is_active, created_at, updated_at";
    const SEARCH_COLUMNS: &'static [&'static str] = &["key", "label", "description", "group_name"];

    fn id(row: &AttributeDefinition) -> Uuid {
        row.id
    }

    fn after_write<'a>(
        conn: &'a mut PgConnection,
        row: &'a AttributeDefinition,
        previous: Option<&'a AttributeDefinition>,
    ) -> BoxFuture<'a, Result<(), AppError>> {
        Box::pin(async move {
            if let Some(clash) = data::attribute_key_clash(conn, row.class_id, &row.key, row.id).await? {
                return Err(AppError::conflict(format!(
                    "Attribute \"{}\" is already defined on class \"{clash}\" in the same lineage",
                    row.key
                ))
                .with_details(vec![FieldError {
                    location: FieldLocation::Body,
                    field: "key".into(),
                    message: "Already defined on an ancestor or descendant class".into(),
                    code: "unique".into(),
                }]));
            }
            if previous.is_none() {
                return Ok(());
            }
            let rules: ValidationRules = row
                .validation
                .as_ref()
                .and_then(|v| serde_json::from_value(Value::Object(v.0.clone())).ok())
                .unwrap_or_default();
            let mut issues = Vec::new();
            rules.check(row.data_type, &mut issues);
            if !issues.is_empty() {
                for i in &mut issues {
                    i.code = "invalid".into();
                }
                return Err(AppError::validation(issues));
            }
            if row.data_type == AttributeDataType::Enum
                && let Some(allowed) = &row.enum_values
            {
                let stale = data::enum_values_in_use(conn, row.id, &allowed.0).await?;
                if !stale.is_empty() {
                    return Err(AppError::field(
                        "enumValues",
                        format!("Values still stored on CIs cannot be removed: {}", stale.join(", ")),
                        "enum_value_in_use",
                    ));
                }
            }
            Ok(())
        })
    }
}

// ===========================================================================
// Relationship types and rules
// ===========================================================================

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationshipType {
    pub id: Uuid,
    pub key: String,
    pub name: String,
    #[schema(required = true)]
    pub description: Option<String>,
    /// Reads source -> target, e.g. "runs on"
    pub forward_label: String,
    /// Reads target -> source, e.g. "hosts"
    pub reverse_label: String,
    /// false for symmetric types such as connected_to
    pub is_directional: bool,
    pub sort_order: i32,
    pub is_active: bool,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationshipTypeCreate {
    #[schema(schema_with = key_schema)]
    key: String,
    #[schema(nullable = false)]
    is_directional: Option<bool>,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    name: String,
    #[schema(schema_with = description_schema)]
    #[serde(default)]
    description: Option<String>,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    forward_label: String,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    reverse_label: String,
    #[schema(schema_with = sort_order_schema)]
    sort_order: Option<i32>,
    #[schema(nullable = false)]
    is_active: Option<bool>,
}

// `key` and `isDirectional` are immutable: existing edges were validated against them.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationshipTypeUpdate {
    #[schema(schema_with = name_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    name: Option<String>,
    #[schema(schema_with = description_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    description: Option<Option<String>>,
    #[schema(schema_with = name_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    forward_label: Option<String>,
    #[schema(schema_with = name_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    reverse_label: Option<String>,
    #[schema(schema_with = sort_order_schema)]
    sort_order: Option<i32>,
    #[schema(nullable = false)]
    is_active: Option<bool>,
}

impl Writable for RelationshipTypeCreate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("key", Some(self.key.clone()))
            .opt("is_directional", self.is_directional)
            .opt("name", Some(self.name.clone()))
            .opt("description", self.description.clone().map(Some))
            .opt("forward_label", Some(self.forward_label.clone()))
            .opt("reverse_label", Some(self.reverse_label.clone()))
            .opt("sort_order", self.sort_order)
            .opt("is_active", self.is_active);
        c
    }
}
impl Check for RelationshipTypeCreate {}

impl Writable for RelationshipTypeUpdate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("name", self.name.clone())
            .opt("description", self.description.clone())
            .opt("forward_label", self.forward_label.clone())
            .opt("reverse_label", self.reverse_label.clone())
            .opt("sort_order", self.sort_order)
            .opt("is_active", self.is_active);
        c
    }
}
impl Check for RelationshipTypeUpdate {
    fn check(&self) -> Vec<FieldError> {
        non_empty(&self.columns())
    }
}

fn relationship_type_sort() -> Schema {
    schemas::sort_schema(&["sortOrder", "name", "key", "createdAt", "updatedAt"], "sortOrder")
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct RelationshipTypeList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    offset: i64,
    #[param(schema_with = schemas::search_schema)]
    q: Option<String>,
    #[param(required = false, schema_with = relationship_type_sort)]
    sort: Sort,
    #[param(inline)]
    is_active: Option<QueryBool>,
    /// Only types a CI of this class may use as source (rules are inherited)
    source_class_id: Option<Uuid>,
    /// Only types a CI of this class may use as target; combine with sourceClassId for a pair
    target_class_id: Option<Uuid>,
}
paged!(RelationshipTypeList);

/// `(ci_class_is_a(a, r.source_class_id) AND ci_class_is_a(b, r.target_class_id))`, either side optional.
fn push_pair(w: &mut sqlx::QueryBuilder<sqlx::Postgres>, a: Option<Uuid>, b: Option<Uuid>) {
    w.push("(");
    match a {
        Some(a) => w.push("ci_class_is_a(").push_bind(a).push(", r.source_class_id)"),
        None => w.push("true"),
    };
    w.push(" AND ");
    match b {
        Some(b) => w.push("ci_class_is_a(").push_bind(b).push(", r.target_class_id)"),
        None => w.push("true"),
    };
    w.push(")");
}

impl ListQuery for RelationshipTypeList {
    fn q(&self) -> Option<&str> {
        self.q.as_deref()
    }
    fn sort(&self) -> &Sort {
        &self.sort
    }
    fn filter(&self, w: &mut Where<'_>) {
        bool_filter(w, "is_active", self.is_active);
        let (src, tgt) = (self.source_class_id, self.target_class_id);
        if src.is_some() || tgt.is_some() {
            let qb = w.and();
            qb.push(
                "EXISTS (SELECT 1 FROM relationship_type_rules r WHERE r.relationship_type_id = relationship_types.id AND (",
            );
            push_pair(qb, src, tgt);
            qb.push(" OR (NOT relationship_types.is_directional AND ");
            push_pair(qb, tgt, src);
            qb.push(")))");
        }
    }
}

pub struct RelationshipTypes;

impl Resource for RelationshipTypes {
    type Dto = RelationshipType;
    type Create = RelationshipTypeCreate;
    type Update = RelationshipTypeUpdate;
    type List = RelationshipTypeList;
    const TABLE: &'static str = "relationship_types";
    const LABEL: &'static str = "Relationship type";
    const BASE_PATH: &'static str = "/api/v1/relationship-types";
    const TAG: &'static str = "Relationship types";
    const SINGULAR: &'static str = "relationshipType";
    const PLURAL: &'static str = "relationshipTypes";
    const COLUMNS: &'static str = "id, key, name, description, forward_label, reverse_label, is_directional, sort_order, is_active, created_at, updated_at";
    const SEARCH_COLUMNS: &'static [&'static str] = &["key", "name", "forward_label", "reverse_label"];

    fn id(row: &RelationshipType) -> Uuid {
        row.id
    }
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationshipRule {
    pub id: Uuid,
    pub relationship_type_id: Uuid,
    /// Matches this class and all its descendants
    pub source_class_id: Uuid,
    /// Matches this class and all its descendants
    pub target_class_id: Uuid,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationshipRuleCreate {
    relationship_type_id: Uuid,
    source_class_id: Uuid,
    target_class_id: Uuid,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationshipRuleUpdate {
    #[schema(nullable = false)]
    relationship_type_id: Option<Uuid>,
    #[schema(nullable = false)]
    source_class_id: Option<Uuid>,
    #[schema(nullable = false)]
    target_class_id: Option<Uuid>,
}

impl Writable for RelationshipRuleCreate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("relationship_type_id", Some(self.relationship_type_id))
            .opt("source_class_id", Some(self.source_class_id))
            .opt("target_class_id", Some(self.target_class_id));
        c
    }
}
impl Check for RelationshipRuleCreate {}

impl Writable for RelationshipRuleUpdate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("relationship_type_id", self.relationship_type_id)
            .opt("source_class_id", self.source_class_id)
            .opt("target_class_id", self.target_class_id);
        c
    }
}
impl Check for RelationshipRuleUpdate {
    fn check(&self) -> Vec<FieldError> {
        non_empty(&self.columns())
    }
}

fn rule_sort() -> Schema {
    schemas::sort_schema(&["createdAt", "updatedAt"], "createdAt")
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct RelationshipRuleList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    offset: i64,
    #[param(required = false, schema_with = rule_sort)]
    sort: Sort,
    #[param(schema_with = schemas::uuid_list_schema)]
    relationship_type_id: Option<UuidList>,
    /// Rules on this class or an ancestor (i.e. rules that apply to it)
    source_class_id: Option<Uuid>,
    /// Rules on this class or an ancestor (i.e. rules that apply to it)
    target_class_id: Option<Uuid>,
}
paged!(RelationshipRuleList);

impl ListQuery for RelationshipRuleList {
    fn sort(&self) -> &Sort {
        &self.sort
    }
    fn filter(&self, w: &mut Where<'_>) {
        if let Some(ids) = &self.relationship_type_id {
            w.and().push("relationship_type_id = ANY(").push_bind(ids.0.clone()).push(")");
        }
        if let Some(id) = self.source_class_id {
            w.and().push("ci_class_is_a(").push_bind(id).push(", source_class_id)");
        }
        if let Some(id) = self.target_class_id {
            w.and().push("ci_class_is_a(").push_bind(id).push(", target_class_id)");
        }
    }
}

pub struct RelationshipRules;

impl Resource for RelationshipRules {
    type Dto = RelationshipRule;
    type Create = RelationshipRuleCreate;
    type Update = RelationshipRuleUpdate;
    type List = RelationshipRuleList;
    const TABLE: &'static str = "relationship_type_rules";
    const LABEL: &'static str = "Relationship rule";
    const BASE_PATH: &'static str = "/api/v1/relationship-rules";
    const TAG: &'static str = "Relationship types";
    const SINGULAR: &'static str = "relationshipRule";
    const PLURAL: &'static str = "relationshipRules";
    const COLUMNS: &'static str = "id, relationship_type_id, source_class_id, target_class_id, created_at, updated_at";
    const SEARCH_COLUMNS: &'static [&'static str] = &[];
    const DELETE_DESCRIPTION: &'static str =
        "Hard delete. Existing relationships stay; new ones need another matching rule.";

    fn id(row: &RelationshipRule) -> Uuid {
        row.id
    }
}

// ===========================================================================

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct EffectiveQuery {
    /// Include retired definitions (default false)
    #[param(inline)]
    include_inactive: Option<QueryBool>,
}

pub async fn effective_attributes(
    pool: &sqlx::PgPool,
    class_id: Uuid,
    include_inactive: bool,
) -> Result<EffectiveAttributeList, AppError> {
    let mut conn = pool.acquire().await?;
    if !data::class_exists(&mut conn, class_id).await? {
        return Err(AppError::missing("CI class", class_id));
    }
    let rows = data::effective_attributes(&mut conn, class_id).await?;
    Ok(EffectiveAttributeList {
        data: rows.into_iter().filter(|r| include_inactive || r.is_active).map(EffectiveAttribute::from).collect(),
    })
}

pub fn routes() -> Vec<Route> {
    let effective = route(Method::GET, "/api/v1/ci-classes/{id}/attributes", "listCiClassEffectiveAttributes")
        .tag("CI classes")
        .summary("Every attribute a CI of this class can carry, including inherited ones")
        .description("Ordered root class first, then by sortOrder. Use it to render the CI form for a class.")
        .errors(&[ErrorCode::NotFound])
        .handle(|api, In(IdPath(id), Query(q), NoBody): In<IdPath, Query<EffectiveQuery>, NoBody>| async move {
            let include = q.include_inactive.map(bool::from).unwrap_or(false);
            Ok(Json(effective_attributes(&api.pool, id, include).await?))
        });

    let mut r = simple::routes::<CiClasses>();
    r.push(effective);
    r.extend(simple::routes::<AttributeDefinitions>());
    r.extend(simple::routes::<RelationshipTypes>());
    r.extend(simple::routes::<RelationshipRules>());
    r
}
