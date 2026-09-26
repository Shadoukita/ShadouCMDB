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
use crate::api::schemas::{
    self, Deleted, LookupRef, OwnerRef, PageMeta, QueryBool, Sort, UuidList, description_schema, name_schema,
    nullable_uuid_schema, trimmed, ts, ts_opt,
};
use crate::data::items::SORT_FIELDS;
use crate::http::error::{FieldError, FieldLocation};
use crate::paged;

// ---------------------------------------------------------------------------
// Responses
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigurationItemSummary {
    pub id: Uuid,
    pub name: String,
    pub class_id: Uuid,
    pub class: LookupRef,
    pub status_id: Uuid,
    pub status: LookupRef,
    #[schema(required = true)]
    pub environment_id: Option<Uuid>,
    #[schema(required = true)]
    pub environment: Option<LookupRef>,
    #[schema(required = true)]
    pub owner_id: Option<Uuid>,
    #[schema(required = true)]
    pub owner: Option<OwnerRef>,
    #[schema(required = true)]
    pub location_id: Option<Uuid>,
    #[schema(required = true)]
    pub location: Option<LookupRef>,
    #[schema(required = true)]
    pub hostname: Option<String>,
    #[schema(required = true)]
    pub ip_address: Option<String>,
    #[schema(required = true)]
    pub serial_number: Option<String>,
    #[schema(required = true)]
    pub notes: Option<String>,
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
    schemas.push((OwnerRef::name().into_owned(), OwnerRef::schema()));
    OwnerRef::schemas(schemas);
}

/// The referenced CI of a reference attribute.
#[derive(Debug, Clone, Serialize)]
pub struct AttributeReference {
    pub id: Uuid,
    pub name: String,
    pub deleted: bool,
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
            "text/enum/date (YYYY-MM-DD)/datetime (ISO 8601)/ip/cidr/reference (CI id) are strings; number/integer are numbers; boolean is a boolean",
        ))
}

impl PartialSchema for ConfigurationItem {
    fn schema() -> RefOr<Schema> {
        let reference = ObjectBuilder::new()
            .property("id", schemas::uuid_builder())
            .property("name", ObjectBuilder::new().schema_type(Type::String))
            .property("deleted", ObjectBuilder::new().schema_type(Type::Boolean))
            .required("id")
            .required("name")
            .required("deleted")
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
    /// "name", "hostname", "serialNumber", "ipAddress", "notes" or "attributes.<key>"
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
    /// true when maxNodes stopped the expansion early
    pub truncated: bool,
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

fn serial_schema() -> Schema {
    schemas::nullable_trimmed_schema(200)
}

fn attributes_schema(description: &str) -> Schema {
    let value = AnyOfBuilder::new().item(attribute_value_schema()).item(ObjectBuilder::new().schema_type(Type::Null));
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .description(Some(description))
        .additional_properties(Some(Schema::from(value)))
        .into()
}

fn create_attributes_schema() -> Schema {
    attributes_schema(
        "Values by attribute key (see GET /api/v1/ci-classes/{id}/attributes). Required attributes must be present.",
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
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    pub name: String,
    pub status_id: Uuid,
    #[schema(schema_with = nullable_uuid_schema)]
    #[serde(default)]
    pub environment_id: Option<Uuid>,
    #[schema(schema_with = nullable_uuid_schema)]
    #[serde(default)]
    pub owner_id: Option<Uuid>,
    #[schema(schema_with = nullable_uuid_schema)]
    #[serde(default)]
    pub location_id: Option<Uuid>,
    #[schema(schema_with = schemas::nullable_hostname_schema)]
    #[serde(default)]
    pub hostname: Option<String>,
    #[schema(schema_with = schemas::nullable_ip_schema)]
    #[serde(default)]
    pub ip_address: Option<String>,
    #[schema(schema_with = serial_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    pub serial_number: Option<String>,
    #[schema(schema_with = description_schema)]
    #[serde(default)]
    pub notes: Option<String>,
    #[schema(schema_with = create_attributes_schema)]
    #[serde(default)]
    pub attributes: Option<Map<String, Value>>,
}
impl Check for CreateItemBody {}

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
    #[schema(schema_with = name_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    pub name: Option<String>,
    #[schema(nullable = false)]
    pub status_id: Option<Uuid>,
    #[schema(schema_with = nullable_uuid_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    pub environment_id: Option<Option<Uuid>>,
    #[schema(schema_with = nullable_uuid_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    pub owner_id: Option<Option<Uuid>>,
    #[schema(schema_with = nullable_uuid_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    pub location_id: Option<Option<Uuid>>,
    #[schema(schema_with = schemas::nullable_hostname_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    pub hostname: Option<Option<String>>,
    #[schema(schema_with = schemas::nullable_ip_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    pub ip_address: Option<Option<String>>,
    #[schema(schema_with = serial_schema)]
    #[serde(default, deserialize_with = "schemas::patch_trimmed")]
    pub serial_number: Option<Option<String>>,
    #[schema(schema_with = description_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    pub notes: Option<Option<String>>,
    #[schema(schema_with = update_attributes_schema)]
    #[serde(default)]
    pub attributes: Option<Map<String, Value>>,
    #[schema(schema_with = version_schema)]
    pub version: Option<i32>,
}

impl Check for UpdateItemBody {
    fn check(&self) -> Vec<FieldError> {
        let any = self.class_id.is_some()
            || self.name.is_some()
            || self.status_id.is_some()
            || self.environment_id.is_some()
            || self.owner_id.is_some()
            || self.location_id.is_some()
            || self.hostname.is_some()
            || self.ip_address.is_some()
            || self.serial_number.is_some()
            || self.notes.is_some()
            || self.attributes.is_some();
        if any {
            Vec::new()
        } else {
            vec![FieldError {
                location: FieldLocation::Body,
                field: "(root)".into(),
                message: "Provide at least one field to update".into(),
                code: "custom".into(),
            }]
        }
    }
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

fn item_sort() -> Schema {
    schemas::sort_schema(SORT_FIELDS, "name")
}

fn list_q_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .min_length(Some(1))
        .max_length(Some(200))
        .description(Some("Search name, hostname, serial number, IP address, notes and attribute values"))
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
        a.description = Some("Only CIs whose ipAddress is inside this CIDR, e.g. 10.20.0.0/16".into());
    }
    s
}

fn deleted_items_schema() -> Schema {
    schemas::deleted_schema("Soft-deleted CIs: exclude (default), include, or only")
}

/// Filters shared by the inventory list and global search.
pub trait ItemFilterQuery {
    fn class_id(&self) -> Option<&UuidList>;
    fn include_subclasses(&self) -> bool;
    fn status_id(&self) -> Option<&UuidList>;
    fn environment_id(&self) -> Option<&UuidList>;
    fn owner_id(&self) -> Option<&UuidList>;
    fn location_id(&self) -> Option<&UuidList>;
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
            fn status_id(&self) -> Option<&UuidList> {
                self.status_id.as_ref()
            }
            fn environment_id(&self) -> Option<&UuidList> {
                self.environment_id.as_ref()
            }
            fn owner_id(&self) -> Option<&UuidList> {
                self.owner_id.as_ref()
            }
            fn location_id(&self) -> Option<&UuidList> {
                self.location_id.as_ref()
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
    #[param(schema_with = schemas::uuid_list_schema)]
    pub status_id: Option<UuidList>,
    #[param(schema_with = schemas::uuid_list_schema)]
    pub environment_id: Option<UuidList>,
    #[param(schema_with = schemas::uuid_list_schema)]
    pub owner_id: Option<UuidList>,
    #[param(schema_with = schemas::uuid_list_schema)]
    pub location_id: Option<UuidList>,
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
    #[param(schema_with = schemas::uuid_list_schema)]
    pub status_id: Option<UuidList>,
    #[param(schema_with = schemas::uuid_list_schema)]
    pub environment_id: Option<UuidList>,
    #[param(schema_with = schemas::uuid_list_schema)]
    pub owner_id: Option<UuidList>,
    #[param(schema_with = schemas::uuid_list_schema)]
    pub location_id: Option<UuidList>,
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
