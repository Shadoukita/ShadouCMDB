//! Request and response types of the configuration-item endpoints.

use std::borrow::Cow;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::{AdditionalProperties, AnyOfBuilder, Object, ObjectBuilder, Schema, SchemaType, Type};
use utoipa::{IntoParams, PartialSchema, ToSchema};
use uuid::Uuid;

use crate::api::route::Check;
use crate::api::schemas::{self, Deleted, LookupRef, PageMeta, QueryBool, Sort, UuidList, trimmed, ts, ts_opt};
use crate::data::items::{SORT_FIELDS, SORT_PATTERN};
use crate::http::error::{FieldError, FieldLocation};
use crate::paged;

// ---------------------------------------------------------------------------
// Responses
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigurationItemSummary {
    pub id: Uuid,
    /// Short unique identifier, e.g. "CI-7K3M9Q2X" (generated; only an administrator can change it)
    pub ident: String,
    /// Display name: the value of the class's title attribute, or the ident when there is none
    pub label: String,
    pub class_id: Uuid,
    pub class: LookupRef,
    /// Start of the validity period
    #[serde(serialize_with = "ts::serialize")]
    pub valid_from: DateTime<Utc>,
    /// End of the validity period (exclusive); null means open-ended
    #[serde(serialize_with = "ts_opt::serialize")]
    #[schema(required = true)]
    pub valid_until: Option<DateTime<Utc>>,
    /// True while validFrom <= now < validUntil (derived, not stored)
    pub active: bool,
    /// Optimistic-locking counter; send it back in PATCH to detect concurrent edits
    pub version: i32,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
    /// Set when the CI was deleted (soft delete); history keeps resolving
    #[serde(serialize_with = "ts_opt::serialize")]
    #[schema(required = true)]
    pub deleted_at: Option<DateTime<Utc>>,
}

/// The summary's object schema with extra properties, for types that
/// serialise as a summary plus a few fields (`#[serde(flatten)]`).
fn summary_with(extra: Vec<(&str, RefOr<Schema>, Option<&str>)>) -> RefOr<Schema> {
    let mut obj: Object = match ConfigurationItemSummary::schema() {
        RefOr::T(Schema::Object(o)) => o,
        _ => Object::new(),
    };
    for (name, schema, description) in extra {
        let schema = match (schema, description) {
            (RefOr::T(Schema::Object(mut o)), Some(d)) => {
                o.description = Some(d.to_owned());
                RefOr::T(Schema::Object(o))
            }
            (s, _) => s,
        };
        obj.properties.insert(name.to_owned(), schema);
        obj.required.push(name.to_owned());
    }
    RefOr::T(Schema::Object(obj))
}

fn summary_nested(schemas: &mut Vec<(String, RefOr<Schema>)>) {
    schemas.push((LookupRef::name().into_owned(), LookupRef::schema()));
}

/// The referenced CI of a reference attribute; `name` is its label. When the
/// caller may not view the referenced CI's class, `hidden` is true, `name` is
/// null and `deleted` is false: only the id (already the attribute's value) is
/// disclosed.
#[derive(Debug, Clone, Serialize)]
pub struct AttributeReference {
    pub id: Uuid,
    pub name: Option<String>,
    pub deleted: bool,
    pub hidden: bool,
}

/// A CI with its attribute values.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigurationItem {
    #[serde(flatten)]
    pub summary: ConfigurationItemSummary,
    pub attributes: Map<String, Value>,
    pub attribute_references: Map<String, Value>,
}

fn attribute_value_schema() -> ObjectBuilder {
    ObjectBuilder::new()
        .schema_type(SchemaType::from_iter([Type::String, Type::Number, Type::Boolean]))
        .description(Some(
            "text/enum/date (YYYY-MM-DD)/datetime (ISO 8601)/ip/cidr/reference (CI id)/lookup (lookup list value id) are strings; number/integer are numbers; boolean is a boolean",
        ))
}

impl PartialSchema for ConfigurationItem {
    fn schema() -> RefOr<Schema> {
        let reference = ObjectBuilder::new()
            .property("id", schemas::uuid_builder())
            .property(
                "name",
                ObjectBuilder::new()
                    .schema_type(SchemaType::from_iter([Type::String, Type::Null]))
                    .description(Some("The referenced CI's label; null when `hidden`")),
            )
            .property(
                "deleted",
                ObjectBuilder::new().schema_type(Type::Boolean).description(Some("Always false when `hidden`")),
            )
            .property(
                "hidden",
                ObjectBuilder::new().schema_type(Type::Boolean).description(Some(
                    "True when the caller may not view the referenced CI's class; its name and state are withheld",
                )),
            )
            .required("id")
            .required("name")
            .required("deleted")
            .required("hidden")
            .additional_properties(Some(AdditionalProperties::FreeForm(false)));
        summary_with(vec![
            (
                "attributes",
                ObjectBuilder::new()
                    .schema_type(Type::Object)
                    .additional_properties(Some(attribute_value_schema()))
                    .into(),
                Some("Class attribute values by attribute key; unset attributes are absent"),
            ),
            (
                "attributeReferences",
                ObjectBuilder::new().schema_type(Type::Object).additional_properties(Some(reference)).into(),
                Some("For reference attributes: the referenced CI, so the UI can show a name without another request"),
            ),
        ])
    }
}

impl ToSchema for ConfigurationItem {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("ConfigurationItem")
    }
    fn schemas(schemas: &mut Vec<(String, RefOr<Schema>)>) {
        summary_nested(schemas);
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchMatch {
    /// "label", "ident" or "attributes.<key>"
    pub field: String,
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchHit {
    pub item: ConfigurationItemSummary,
    #[schema(inline)]
    pub matches: Vec<SearchMatch>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchResults {
    #[schema(inline)]
    pub data: Vec<SearchHit>,
    pub page: PageMeta,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GraphEdgeType {
    pub key: String,
    pub name: String,
    pub forward_label: String,
    pub reverse_label: String,
    pub is_directional: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GraphEdge {
    /// Relationship id
    pub id: Uuid,
    pub relationship_type_id: Uuid,
    #[schema(inline)]
    #[serde(rename = "type")]
    pub edge_type: GraphEdgeType,
    pub source_ci_id: Uuid,
    pub target_ci_id: Uuid,
    #[schema(required = true)]
    pub notes: Option<String>,
}

/// A CI in a graph: its summary plus the hop count from the root.
#[derive(Debug, Clone, Serialize)]
pub struct GraphNode {
    #[serde(flatten)]
    pub summary: ConfigurationItemSummary,
    pub depth: i32,
}

impl PartialSchema for GraphNode {
    fn schema() -> RefOr<Schema> {
        summary_with(vec![(
            "depth",
            ObjectBuilder::new().schema_type(Type::Integer).into(),
            Some("Hops from the root (root = 0)"),
        )])
    }
}

impl ToSchema for GraphNode {
    fn schemas(schemas: &mut Vec<(String, RefOr<Schema>)>) {
        summary_nested(schemas);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum GraphDirection {
    Both,
    Outgoing,
    Incoming,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schema(as = RelationshipGraph)]
pub struct Graph {
    pub root_id: Uuid,
    pub depth: i32,
    #[schema(inline)]
    pub direction: GraphDirection,
    #[schema(inline)]
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    /// true when maxNodes, or the edge budget of 5 × maxNodes, stopped the expansion early
    pub truncated: bool,
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

fn ident_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .pattern(Some(schemas::IDENT_PATTERN))
        .max_length(Some(64))
        .description(Some(
            "Administrators only (403 for anyone else). Unique regardless of case; leave out to have one generated",
        ))
        .into()
}

fn valid_from_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .format(Some(utoipa::openapi::SchemaFormat::KnownFormat(utoipa::openapi::KnownFormat::DateTime)))
        .description(Some("Start of the validity period; defaults to now"))
        .into()
}

fn valid_until_schema() -> Schema {
    AnyOfBuilder::new()
        .item(
            ObjectBuilder::new()
                .schema_type(Type::String)
                .format(Some(utoipa::openapi::SchemaFormat::KnownFormat(utoipa::openapi::KnownFormat::DateTime))),
        )
        .item(ObjectBuilder::new().schema_type(Type::Null))
        .description(Some("End of the validity period (exclusive, after validFrom); null: open-ended"))
        .into()
}

fn attributes_schema(description: &str) -> Schema {
    // Line breaks are up to each attribute: its value rules check them (GH#289).
    let value = AnyOfBuilder::new()
        .item(attribute_value_schema())
        .item(ObjectBuilder::new().schema_type(Type::Null))
        .extensions(Some(crate::api::schemas::multiline_extension()));
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .description(Some(description))
        .additional_properties(Some(Schema::from(value)))
        .into()
}

fn create_attributes_schema() -> Schema {
    attributes_schema(
        "Values by attribute key (see GET /api/v1/ci-classes/{id}/attributes). Required attributes must be present; attributes left out get their defaultValue.",
    )
}

fn update_attributes_schema() -> Schema {
    attributes_schema("Merged into the current values; null clears an attribute")
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateItemBody {
    /// A concrete (non-abstract), active class
    pub class_id: Uuid,
    #[schema(schema_with = ident_schema)]
    #[serde(default)]
    pub ident: Option<String>,
    #[schema(schema_with = valid_from_schema)]
    #[serde(default)]
    pub valid_from: Option<DateTime<Utc>>,
    #[schema(schema_with = valid_until_schema)]
    #[serde(default)]
    pub valid_until: Option<DateTime<Utc>>,
    #[schema(schema_with = create_attributes_schema)]
    #[serde(default)]
    pub attributes: Option<Map<String, Value>>,
}
impl Check for CreateItemBody {
    fn check(&self) -> Vec<FieldError> {
        validity_errors(self.valid_from, self.valid_until)
    }
}

fn validity_errors(from: Option<DateTime<Utc>>, until: Option<DateTime<Utc>>) -> Vec<FieldError> {
    match (from, until) {
        (Some(from), Some(until)) if until <= from => vec![FieldError {
            location: FieldLocation::Body,
            field: "validUntil".into(),
            message: "Must be after validFrom".into(),
            code: "custom".into(),
        }],
        _ => Vec::new(),
    }
}

fn version_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::Integer)
        .minimum(Some(1))
        .description(Some("If sent and stale, the update fails with 409 VERSION_CONFLICT"))
        .into()
}

fn class_change_schema() -> Schema {
    schemas::uuid_builder()
        .description(Some("Changing class requires clearing attributes the new class does not have"))
        .into()
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateItemBody {
    #[schema(schema_with = class_change_schema)]
    pub class_id: Option<Uuid>,
    #[schema(schema_with = ident_schema)]
    #[serde(default)]
    pub ident: Option<String>,
    #[schema(schema_with = valid_from_schema)]
    #[serde(default)]
    pub valid_from: Option<DateTime<Utc>>,
    #[schema(schema_with = valid_until_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    pub valid_until: Option<Option<DateTime<Utc>>>,
    #[schema(schema_with = update_attributes_schema)]
    #[serde(default)]
    pub attributes: Option<Map<String, Value>>,
    #[schema(schema_with = version_schema)]
    pub version: Option<i32>,
}

impl Check for UpdateItemBody {
    fn check(&self) -> Vec<FieldError> {
        let any = self.class_id.is_some()
            || self.ident.is_some()
            || self.valid_from.is_some()
            || self.valid_until.is_some()
            || self.attributes.is_some();
        if !any {
            return vec![FieldError {
                location: FieldLocation::Body,
                field: "(root)".into(),
                message: "Provide at least one field to update".into(),
                code: "custom".into(),
            }];
        }
        validity_errors(self.valid_from, self.valid_until.flatten())
    }
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

fn item_sort() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .pattern(Some(SORT_PATTERN))
        .default(Some("label".into()))
        .description(Some(format!(
            "Sort field; prefix with \"-\" for descending. One of: {}, or attributes.<key> (needs classId; the \
             attribute must be the same one on every class in classId, and not a reference). Attributes sort \
             case-insensitively for text, by address for IP/CIDR and by list order for lookups; CIs without a \
             value come last.",
            SORT_FIELDS.join(", ")
        )))
        .into()
}

fn list_q_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .min_length(Some(1))
        .max_length(Some(200))
        .description(Some("Search label, ident and attribute values"))
        .into()
}

fn class_filter_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .description(Some("Filter by class (includes subclasses unless includeSubclasses=false)"))
        .into()
}

fn include_subclasses_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(["true", "false"]))
        .default(Some("true".into()))
        .into()
}

fn ip_within_schema() -> Schema {
    let mut s = schemas::cidr_schema();
    if let Schema::AnyOf(a) = &mut s {
        a.description = Some("Only CIs with a value of an IP attribute inside this CIDR, e.g. 10.20.0.0/16".into());
    }
    s
}

fn lookup_value_filter_schema() -> Schema {
    schemas::uuid_list_described(
        "Lookup list value ids, comma-separated: CIs holding one of them in a lookup attribute. Values of different \
         lists must all match (status A or B, and environment C).",
    )
}

/// Which CIs by validity: active (the default), inactive, or all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum ActiveQuery {
    #[serde(rename = "true")]
    True,
    #[serde(rename = "false")]
    False,
    #[serde(rename = "all")]
    All,
}

fn active_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(["true", "false", "all"]))
        .default(Some("true".into()))
        .description(Some(
            "true: only CIs inside their validity period (validFrom <= now < validUntil); false: only those outside \
             it; all: both",
        ))
        .into()
}

fn deleted_items_schema() -> Schema {
    schemas::deleted_schema("Soft-deleted CIs: exclude (default), include, or only")
}

/// Filters shared by the inventory list and global search.
pub trait ItemFilterQuery {
    fn class_id(&self) -> Option<&UuidList>;
    fn include_subclasses(&self) -> bool;
    fn active(&self) -> ActiveQuery;
    fn lookup_value_id(&self) -> Option<&UuidList>;
    fn ip_within(&self) -> Option<&str>;
    fn deleted(&self) -> Deleted;
}

macro_rules! item_filters {
    ($t:ty) => {
        impl ItemFilterQuery for $t {
            fn class_id(&self) -> Option<&UuidList> {
                self.class_id.as_ref()
            }
            fn include_subclasses(&self) -> bool {
                self.include_subclasses.into()
            }
            fn active(&self) -> ActiveQuery {
                self.active
            }
            fn lookup_value_id(&self) -> Option<&UuidList> {
                self.lookup_value_id.as_ref()
            }
            fn ip_within(&self) -> Option<&str> {
                self.ip_within.as_deref()
            }
            fn deleted(&self) -> Deleted {
                self.deleted
            }
        }
    };
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ListItemsQuery {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    pub limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    pub offset: i64,
    #[param(schema_with = list_q_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    pub q: Option<String>,
    #[param(required = false, schema_with = item_sort)]
    pub sort: Sort,
    #[param(schema_with = class_filter_schema)]
    pub class_id: Option<UuidList>,
    #[param(required = false, schema_with = include_subclasses_schema)]
    pub include_subclasses: QueryBool,
    #[param(required = false, schema_with = active_schema)]
    pub active: ActiveQuery,
    #[param(schema_with = lookup_value_filter_schema)]
    pub lookup_value_id: Option<UuidList>,
    #[param(schema_with = ip_within_schema)]
    pub ip_within: Option<String>,
    #[param(required = false, schema_with = deleted_items_schema)]
    pub deleted: Deleted,
}
paged!(ListItemsQuery);
item_filters!(ListItemsQuery);

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct SearchQuery {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    pub limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    pub offset: i64,
    /// Search text
    #[param(min_length = 1, max_length = 200)]
    #[serde(deserialize_with = "trimmed")]
    pub q: String,
    #[param(schema_with = class_filter_schema)]
    pub class_id: Option<UuidList>,
    #[param(required = false, schema_with = include_subclasses_schema)]
    pub include_subclasses: QueryBool,
    #[param(required = false, schema_with = active_schema)]
    pub active: ActiveQuery,
    #[param(schema_with = lookup_value_filter_schema)]
    pub lookup_value_id: Option<UuidList>,
    #[param(schema_with = ip_within_schema)]
    pub ip_within: Option<String>,
    #[param(required = false, schema_with = deleted_items_schema)]
    pub deleted: Deleted,
}
paged!(SearchQuery);
item_filters!(SearchQuery);

fn direction_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(["both", "outgoing", "incoming"]))
        .default(Some("both".into()))
        .description(Some(
            "outgoing follows source->target (app -> runs_on -> server); incoming the reverse; symmetric types are always followed",
        ))
        .into()
}

fn type_filter_schema() -> Schema {
    ObjectBuilder::new().schema_type(Type::String).description(Some("Only follow these relationship types")).into()
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct GraphQuery {
    /// Hops from the root CI (1-6)
    #[param(required = false, default = 2, minimum = 1, maximum = 6)]
    pub depth: i32,
    #[param(required = false, schema_with = direction_schema)]
    pub direction: GraphDirection,
    #[param(schema_with = type_filter_schema)]
    pub relationship_type_id: Option<UuidList>,
    /// Stop expanding once this many CIs are collected
    #[param(required = false, default = 250, minimum = 1, maximum = 1000)]
    pub max_nodes: i32,
}
