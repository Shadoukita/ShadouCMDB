//! Request and response types of the configuration-item endpoints.

use std::borrow::Cow;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::{
    AdditionalProperties, AnyOfBuilder, KnownFormat, Object, ObjectBuilder, Schema, SchemaFormat, SchemaType, Type,
};
use utoipa::{IntoParams, PartialSchema, ToSchema};
use uuid::Uuid;

use crate::api::context::ActorType;
use crate::api::route::Check;
use crate::api::schemas::{self, Deleted, LookupRef, PageMeta, QueryBool, Sort, UuidList, trimmed, ts, ts_opt};
use crate::data::items::{SORT_FIELDS, SORT_PATTERN};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
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
    /// How critical the CI is (a value of the criticality lookup list); null when not set
    #[schema(required = true)]
    pub criticality: Option<CriticalityRef>,
}

/// A CI's criticality: a value of the system lookup list `criticality`.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CriticalityRef {
    /// The lookup list value id (send it as `criticalityValueId`)
    pub id: Uuid,
    pub key: String,
    pub name: String,
    /// Position in the criticality list (its sort order): 1 is the most critical
    pub rank: i64,
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
    schemas.push((CriticalityRef::name().into_owned(), CriticalityRef::schema()));
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
    /// Only on `getConfigurationItem` (`Some`): the CI's last change, null
    /// when the audit log holds none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_change: Option<Option<LastChange>>,
}

/// The kinds of audit entry that count as a change to a CI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum LastChangeAction {
    Create,
    Update,
    Delete,
    Restore,
}

/// Who made a change, as its audit entry records it.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LastChangeActor {
    #[schema(inline)]
    #[serde(rename = "type")]
    pub actor_type: ActorType,
    /// The user's id for `user` and `api_client` (the token's owner)
    #[schema(required = true)]
    pub id: Option<String>,
    /// The username at the time of the change (or the system actor's name)
    #[schema(required = true)]
    pub name: Option<String>,
}

/// The CI's newest create, update, delete or restore entry in the audit log:
/// when, what, and who. Kept per CI by the database from the audit log itself,
/// so it never disagrees with it; read events (exports) and workflow events
/// do not count.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LastChange {
    #[serde(serialize_with = "ts::serialize")]
    pub at: DateTime<Utc>,
    #[schema(inline)]
    pub action: LastChangeAction,
    /// Null when the caller lacks `audit.view`: who changed what is audit data
    #[schema(required = true)]
    pub actor: Option<LastChangeActor>,
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
        .with_last_change()
    }
}

trait WithLastChange {
    fn with_last_change(self) -> Self;
}

/// `lastChange`: returned by `getConfigurationItem` only, so not required.
impl WithLastChange for RefOr<Schema> {
    fn with_last_change(self) -> Self {
        let RefOr::T(Schema::Object(mut obj)) = self else { return self };
        let schema = AnyOfBuilder::new()
            .item(RefOr::Ref(utoipa::openapi::Ref::from_schema_name(LastChange::name())))
            .item(ObjectBuilder::new().schema_type(Type::Null))
            .description(Some(
                "Only on getConfigurationItem: the CI's last change (its newest create, update, delete or restore \
                 audit entry), without reading the audit log; null when the log holds none for it (entries removed \
                 by audit retention before this field existed).",
            ));
        obj.properties.insert("lastChange".to_owned(), schema.into());
        RefOr::T(Schema::Object(obj))
    }
}

impl ToSchema for ConfigurationItem {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("ConfigurationItem")
    }
    fn schemas(schemas: &mut Vec<(String, RefOr<Schema>)>) {
        summary_nested(schemas);
        schemas.push((LastChange::name().into_owned(), LastChange::schema()));
        <LastChange as ToSchema>::schemas(schemas);
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
    /// The relationship type's category (group heading); null: none
    #[schema(required = true)]
    pub category: Option<String>,
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

#[derive(Debug, Clone, Deserialize, ToSchema)]
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
    #[schema(schema_with = criticality_schema)]
    #[serde(default)]
    pub criticality_value_id: Option<Uuid>,
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

fn criticality_schema() -> Schema {
    AnyOfBuilder::new()
        .item(schemas::uuid_builder())
        .item(ObjectBuilder::new().schema_type(Type::Null))
        .description(Some(
            "A value of the criticality lookup list (GET /api/v1/lookup-lists?systemRole=criticality); null: not set. \
             A retired value can be kept but not newly set",
        ))
        .into()
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
    #[schema(schema_with = criticality_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    pub criticality_value_id: Option<Option<Uuid>>,
    #[schema(schema_with = version_schema)]
    pub version: Option<i32>,
}

impl Check for UpdateItemBody {
    fn check(&self) -> Vec<FieldError> {
        let any = self.class_id.is_some()
            || self.ident.is_some()
            || self.valid_from.is_some()
            || self.valid_until.is_some()
            || self.attributes.is_some()
            || self.criticality_value_id.is_some();
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
// Bulk update
// ---------------------------------------------------------------------------

/// Most CIs one bulk update may change.
pub const BULK_UPDATE_MAX: usize = 500;

fn bulk_ids_schema() -> Schema {
    utoipa::openapi::schema::ArrayBuilder::new()
        .items(schemas::uuid_builder())
        .min_items(Some(1))
        .max_items(Some(BULK_UPDATE_MAX))
        .unique_items(true)
        .description(Some("The CIs to update: 1 to 500, each at most once"))
        .into()
}

fn bulk_attributes_schema() -> Schema {
    attributes_schema(
        "Set on every CI: merged into its current values as `PATCH /configuration-items/{id}` merges them; null \
         clears an attribute. Each key must be an attribute of every CI's class.",
    )
}

/// The same change for many CIs: the fields of `PATCH /configuration-items/{id}` that make sense across classes.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BulkUpdateItemsBody {
    #[schema(schema_with = bulk_ids_schema)]
    pub ids: Vec<Uuid>,
    #[schema(schema_with = bulk_attributes_schema)]
    #[serde(default)]
    pub attributes: Option<Map<String, Value>>,
    #[schema(schema_with = criticality_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    pub criticality_value_id: Option<Option<Uuid>>,
    /// true: when any CI is refused, nothing is written (`committed` is false). false (default): the CIs that pass
    /// are written and the refused ones are reported.
    #[serde(default)]
    pub all_or_nothing: bool,
}

impl Check for BulkUpdateItemsBody {
    fn check(&self) -> Vec<FieldError> {
        let error = |field: &str, message: &str, code: &str| FieldError {
            location: FieldLocation::Body,
            field: field.into(),
            message: message.into(),
            code: code.into(),
        };
        let mut errors = Vec::new();
        if self.ids.is_empty() {
            errors.push(error("ids", "Select at least one configuration item", "too_small"));
        } else if self.ids.len() > BULK_UPDATE_MAX {
            errors.push(error("ids", &format!("At most {BULK_UPDATE_MAX} configuration items per request"), "too_big"));
        }
        // Duplicates are refused by the schema (`uniqueItems`).
        if self.attributes.as_ref().is_none_or(Map::is_empty) && self.criticality_value_id.is_none() {
            errors.push(error("(root)", "Provide at least one field to update", "custom"));
        }
        errors
    }
}

impl BulkUpdateItemsBody {
    /// The PATCH body each CI gets.
    pub fn patch(&self) -> UpdateItemBody {
        UpdateItemBody {
            class_id: None,
            ident: None,
            valid_from: None,
            valid_until: None,
            attributes: self.attributes.clone(),
            criticality_value_id: self.criticality_value_id,
            version: None,
        }
    }
}

/// One problem of a refused CI, as in the error envelope's `details`
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BulkUpdateErrorDetail {
    #[serde(rename = "in")]
    #[schema(inline)]
    pub location: FieldLocation,
    /// Dotted path in the body, e.g. `attributes.owner`
    pub field: String,
    pub message: String,
    pub code: String,
}

/// Why a CI was refused: what `PATCH /configuration-items/{id}` would have answered for it
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BulkUpdateError {
    #[schema(inline)]
    pub code: ErrorCode,
    pub message: String,
    pub details: Vec<BulkUpdateErrorDetail>,
}

impl From<AppError> for BulkUpdateError {
    fn from(e: AppError) -> Self {
        let details = e
            .details
            .unwrap_or_default()
            .into_iter()
            .map(|d| BulkUpdateErrorDetail { location: d.location, field: d.field, message: d.message, code: d.code })
            .collect();
        BulkUpdateError { code: e.code, message: e.message, details }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BulkUpdateResult {
    /// Position of the CI in `ids` (0-based)
    pub index: i32,
    pub id: Uuid,
    /// The update passed every check for this CI (written when `committed` is true)
    pub ok: bool,
    /// The CI as written (ok results when `committed` is true)
    #[schema(required = true)]
    pub item: Option<ConfigurationItemSummary>,
    /// Why the CI was refused (failed results); nothing of it was written
    #[schema(required = true)]
    pub error: Option<BulkUpdateError>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BulkUpdateReport {
    pub succeeded: i32,
    pub failed: i32,
    /// The ok results are written. false only with `allOrNothing` when a CI was refused: nothing was written.
    pub committed: bool,
    /// One result per id, in the request's order
    pub results: Vec<BulkUpdateResult>,
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

fn own_layout_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(["true", "false"]))
        .description(Some(
            "true: only CIs with a layout of their own (another template or a custom layout, see \
             /configuration-items/{id}/layout); false: only CIs that show their class's default template",
        ))
        .into()
}

fn layout_template_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .pattern(Some(schemas::KEY_PATTERN))
        .description(Some(
            "Only CIs that show this layout template (`layoutTemplates[].key`) as their own layout, not as their \
             class's default",
        ))
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

fn business_service_filter_schema() -> Schema {
    schemas::uuid_list_described(
        "Business service ids (CI ids), comma-separated: CIs that are a direct member of one of them. A service the \
         caller may not view, or a deleted one, has no members here.",
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

fn criticality_filter_schema() -> Schema {
    schemas::uuid_list_described("Criticality value ids, comma-separated: CIs holding one of them")
}

fn deleted_items_schema() -> Schema {
    schemas::deleted_schema("Soft-deleted CIs: exclude (default), include, or only")
}

/// Which CIs by the kind of their type (see `CiClass.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KindQuery {
    Asset,
    Process,
    Any,
}

fn kind_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(["asset", "process", "any"]))
        .description(Some(
            "CIs by the kind of their type: asset, process (records such as change requests) or any. Without it, \
             process records are left out unless their type is named in classId.",
        ))
        .into()
}

/// A data-quality check ("Needs attention"): which CIs it finds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QualityCheck {
    /// No value in the owner field of their type (`CiClass.ownerAttributeId`, or the nearest ancestor's)
    NoOwner,
    /// End of life (the type's `endOfLifeAttributeId` field) reached or within `endOfLifeWithinDays` days
    EndOfLife,
    /// No live relationship to a CI the caller may view, in either direction
    NoRelationships,
    /// A workflow instance on the CI waits for an approval decision
    PendingApproval,
}

impl QualityCheck {
    pub const ALL: [QualityCheck; 4] =
        [QualityCheck::NoOwner, QualityCheck::EndOfLife, QualityCheck::NoRelationships, QualityCheck::PendingApproval];
}

/// Default and bounds of `endOfLifeWithinDays`.
pub const END_OF_LIFE_DAYS_DEFAULT: i32 = 90;
pub const END_OF_LIFE_DAYS_MAX: i32 = 3650;

fn quality_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(["no_owner", "end_of_life", "no_relationships", "pending_approval"]))
        .description(Some(
            "Only CIs a data-quality check finds (see `getConfigurationItemDataQuality`): no_owner (no value in \
             the owner field of their type), end_of_life (end of life reached or within `endOfLifeWithinDays` \
             days), no_relationships (no live relationship to a CI the caller may view) or pending_approval (a \
             workflow approval request is pending). CIs of types without an owner or end-of-life field match \
             neither of those two checks.",
        ))
        .into()
}

fn end_of_life_days_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::Integer)
        .format(Some(SchemaFormat::KnownFormat(KnownFormat::Int32)))
        .minimum(Some(0))
        .maximum(Some(END_OF_LIFE_DAYS_MAX))
        .default(Some(END_OF_LIFE_DAYS_DEFAULT.into()))
        .description(Some(
            "For the end_of_life check: CIs whose end of life is today or earlier, or at most this many days from \
             today (the database server's date).",
        ))
        .into()
}

/// `endOfLifeWithinDays`, defaulted and checked.
pub fn end_of_life_days(days: Option<i32>) -> Result<i32, FieldError> {
    match days.unwrap_or(END_OF_LIFE_DAYS_DEFAULT) {
        d if (0..=END_OF_LIFE_DAYS_MAX).contains(&d) => Ok(d),
        _ => Err(FieldError {
            location: FieldLocation::Query,
            field: "endOfLifeWithinDays".into(),
            message: format!("Must be between 0 and {END_OF_LIFE_DAYS_MAX}"),
            code: "out_of_range".into(),
        }),
    }
}

/// Filters shared by the inventory list and global search.
pub trait ItemFilterQuery {
    fn class_id(&self) -> Option<&UuidList>;
    fn include_subclasses(&self) -> bool;
    fn active(&self) -> ActiveQuery;
    fn lookup_value_id(&self) -> Option<&UuidList>;
    fn ip_within(&self) -> Option<&str>;
    fn criticality_value_id(&self) -> Option<&UuidList>;
    fn deleted(&self) -> Deleted;
    fn kind(&self) -> Option<KindQuery>;
    fn business_service_id(&self) -> Option<&UuidList>;
    /// The data-quality check filter (inventory list and facets only)
    fn quality(&self) -> Option<QualityCheck> {
        None
    }
    fn end_of_life_within_days(&self) -> Option<i32> {
        None
    }
}

macro_rules! item_filters {
    ($t:ty, quality) => {
        item_filters!($t, {
            fn quality(&self) -> Option<QualityCheck> {
                self.quality
            }
            fn end_of_life_within_days(&self) -> Option<i32> {
                self.end_of_life_within_days
            }
        });
    };
    ($t:ty) => {
        item_filters!($t, {});
    };
    ($t:ty, { $($extra:tt)* }) => {
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
            fn criticality_value_id(&self) -> Option<&UuidList> {
                self.criticality_value_id.as_ref()
            }
            fn deleted(&self) -> Deleted {
                self.deleted
            }
            fn kind(&self) -> Option<KindQuery> {
                self.kind
            }
            fn business_service_id(&self) -> Option<&UuidList> {
                self.business_service_id.as_ref()
            }
            $($extra)*
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
    #[param(schema_with = criticality_filter_schema)]
    pub criticality_value_id: Option<UuidList>,
    #[param(required = false, schema_with = deleted_items_schema)]
    pub deleted: Deleted,
    #[param(schema_with = own_layout_schema)]
    pub own_layout: Option<QueryBool>,
    #[param(schema_with = layout_template_schema)]
    pub layout_template: Option<String>,
    #[param(schema_with = kind_schema)]
    pub kind: Option<KindQuery>,
    #[param(schema_with = business_service_filter_schema)]
    pub business_service_id: Option<UuidList>,
    #[param(schema_with = quality_schema)]
    pub quality: Option<QualityCheck>,
    #[param(schema_with = end_of_life_days_schema)]
    pub end_of_life_within_days: Option<i32>,
}
paged!(ListItemsQuery);
item_filters!(ListItemsQuery, quality);

/// Built-in columns of the inventory export (and `id`); attributes are `attributes.<key>`.
pub const EXPORT_BUILTIN_COLUMNS: &[&str] =
    &["id", "label", "ident", "class", "criticality", "validFrom", "validUntil", "active", "createdAt", "updatedAt"];
/// The export's columns without `columns`: the inventory list's default columns.
pub const EXPORT_DEFAULT_COLUMNS: &[&str] = &["label", "ident", "class", "active", "updatedAt"];
/// Most columns in one export (the saved-view and list limit).
pub const EXPORT_MAX_COLUMNS: usize = 50;

/// The field separator of an exported file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CsvDelimiter {
    Comma,
    Semicolon,
}

impl CsvDelimiter {
    pub fn char(self) -> char {
        match self {
            CsvDelimiter::Comma => ',',
            CsvDelimiter::Semicolon => ';',
        }
    }
}

fn delimiter_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(["comma", "semicolon"]))
        .default(Some("comma".into()))
        .description(Some(
            "Field separator: comma (default) or semicolon (what Excel expects where the decimal separator is a \
             comma)",
        ))
        .into()
}

fn export_columns_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .min_length(Some(1))
        .max_length(Some(4000))
        .description(Some(format!(
            "Comma-separated columns in file order, at most {EXPORT_MAX_COLUMNS}, each once: {}, or \
             attributes.<key> (needs classId; the attribute must be an active attribute of every class in classId, \
             its own or inherited). Default: {}, the inventory list's default columns.",
            EXPORT_BUILTIN_COLUMNS.join(", "),
            EXPORT_DEFAULT_COLUMNS.join(",")
        )))
        .into()
}

/// The filters and sort of `listConfigurationItems` (without the page), the
/// columns and the field separator.
#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ExportItemsQuery {
    #[param(schema_with = export_columns_schema)]
    pub columns: Option<String>,
    #[param(required = false, schema_with = delimiter_schema)]
    pub delimiter: CsvDelimiter,
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
    #[param(schema_with = criticality_filter_schema)]
    pub criticality_value_id: Option<UuidList>,
    #[param(required = false, schema_with = deleted_items_schema)]
    pub deleted: Deleted,
    #[param(schema_with = own_layout_schema)]
    pub own_layout: Option<QueryBool>,
    #[param(schema_with = layout_template_schema)]
    pub layout_template: Option<String>,
    #[param(schema_with = kind_schema)]
    pub kind: Option<KindQuery>,
    #[param(schema_with = business_service_filter_schema)]
    pub business_service_id: Option<UuidList>,
}
item_filters!(ExportItemsQuery);

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
    #[param(schema_with = criticality_filter_schema)]
    pub criticality_value_id: Option<UuidList>,
    #[param(required = false, schema_with = deleted_items_schema)]
    pub deleted: Deleted,
    #[param(schema_with = kind_schema)]
    pub kind: Option<KindQuery>,
    #[param(schema_with = business_service_filter_schema)]
    pub business_service_id: Option<UuidList>,
}
paged!(SearchQuery);
item_filters!(SearchQuery);

/// Width of a change histogram bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum HistogramBucket {
    Hour,
    Day,
}

impl HistogramBucket {
    /// The bucket width, and the longest range one request may cover.
    pub fn width_and_cap(self) -> (chrono::TimeDelta, chrono::TimeDelta) {
        match self {
            HistogramBucket::Hour => (chrono::TimeDelta::hours(1), chrono::TimeDelta::days(7)),
            HistogramBucket::Day => (chrono::TimeDelta::days(1), chrono::TimeDelta::days(90)),
        }
    }
}

fn bucket_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(["hour", "day"]))
        .default(Some("hour".into()))
        .description(Some(
            "Bucket width, aligned to UTC hours or days. hour covers at most 7 days per request, day at most 90 days.",
        ))
        .into()
}

fn histogram_from_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .format(Some(SchemaFormat::KnownFormat(KnownFormat::DateTime)))
        .description(Some(
            "Start of the range (ISO 8601, inclusive). Default: 24 hours before `to` for hour buckets, 30 days before \
             for day buckets.",
        ))
        .into()
}

fn histogram_to_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .format(Some(SchemaFormat::KnownFormat(KnownFormat::DateTime)))
        .description(Some("End of the range (ISO 8601, exclusive). Default: now."))
        .into()
}

/// The list filters of `listConfigurationItems`, plus the time range and bucket width.
#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ChangeHistogramQuery {
    #[param(schema_with = histogram_from_schema)]
    pub from: Option<DateTime<Utc>>,
    #[param(schema_with = histogram_to_schema)]
    pub to: Option<DateTime<Utc>>,
    #[param(required = false, schema_with = bucket_schema)]
    pub bucket: HistogramBucket,
    #[param(schema_with = list_q_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    pub q: Option<String>,
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
    #[param(schema_with = criticality_filter_schema)]
    pub criticality_value_id: Option<UuidList>,
    #[param(required = false, schema_with = deleted_items_schema)]
    pub deleted: Deleted,
    #[param(schema_with = own_layout_schema)]
    pub own_layout: Option<QueryBool>,
    #[param(schema_with = layout_template_schema)]
    pub layout_template: Option<String>,
    #[param(schema_with = kind_schema)]
    pub kind: Option<KindQuery>,
    #[param(schema_with = business_service_filter_schema)]
    pub business_service_id: Option<UuidList>,
}
item_filters!(ChangeHistogramQuery);

/// Changes to the CIs of one bucket. The three counts do not overlap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangeHistogramBucket {
    /// Start of the bucket (UTC)
    #[serde(serialize_with = "ts::serialize")]
    pub start: DateTime<Utc>,
    /// CIs created
    pub created: i64,
    /// Other changes: updates that leave the status as it was, deletions and restores
    pub updated: i64,
    /// Updates that changed the CI's `status` attribute
    pub status_changed: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangeHistogram {
    /// The range counted, as sent or defaulted: `from` inclusive, `to` exclusive
    #[serde(serialize_with = "ts::serialize")]
    pub from: DateTime<Utc>,
    #[serde(serialize_with = "ts::serialize")]
    pub to: DateTime<Utc>,
    #[schema(inline)]
    pub bucket: HistogramBucket,
    /// Every bucket of the range in order, empty ones included. The first starts at `from` rounded down to the
    /// bucket width and may count only part of its hour or day; so may the last.
    pub buckets: Vec<ChangeHistogramBucket>,
    /// Sum of all counts over all buckets
    pub total: i64,
}

/// The filters of the inventory list (`listConfigurationItems`), without paging and sort.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct FacetsQuery {
    #[param(schema_with = list_q_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    pub q: Option<String>,
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
    #[param(schema_with = criticality_filter_schema)]
    pub criticality_value_id: Option<UuidList>,
    #[param(required = false, schema_with = deleted_items_schema)]
    pub deleted: Deleted,
    #[param(schema_with = own_layout_schema)]
    pub own_layout: Option<QueryBool>,
    #[param(schema_with = layout_template_schema)]
    pub layout_template: Option<String>,
    #[param(schema_with = kind_schema)]
    pub kind: Option<KindQuery>,
    #[param(schema_with = business_service_filter_schema)]
    pub business_service_id: Option<UuidList>,
    #[param(schema_with = quality_schema)]
    pub quality: Option<QualityCheck>,
    #[param(schema_with = end_of_life_days_schema)]
    pub end_of_life_within_days: Option<i32>,
    /// Values returned per facet, most CIs first (1-200); selected values are always returned
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    pub value_limit: i64,
}
item_filters!(FacetsQuery, quality);

/// What a facet counts, and so which list filter its value ids go into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum FacetKind {
    /// CI classes (`classId`); counted per exact class, subclasses separately
    Class,
    /// Values of the criticality list (`criticalityValueId`)
    Criticality,
    /// Values of one lookup list held in lookup attributes (`lookupValueId`)
    Lookup,
    /// Business services, counting their direct members (`businessServiceId`)
    BusinessService,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FacetValue {
    /// The id to send in the facet's filter parameter
    pub id: Uuid,
    /// Class key, lookup value key, or the service's ident
    pub key: String,
    pub label: String,
    /// CIs matching every other filter that hold this value
    pub count: i64,
    /// True when the id is in the facet's filter parameter of this request
    pub selected: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Facet {
    /// `class`, `criticality`, `businessService`, or `lookup.<list key>`
    pub key: String,
    pub kind: FacetKind,
    /// Display name: "Class", "Criticality", "Business service" or the lookup list's name
    pub label: String,
    /// The `listConfigurationItems` query parameter that filters by this facet's value ids
    pub param: String,
    /// The lookup list (kind lookup); null otherwise
    #[schema(required = true)]
    pub list_id: Option<Uuid>,
    /// Values with at least one CI, most CIs first, then by label; selected values are included even at 0
    #[schema(inline)]
    pub values: Vec<FacetValue>,
    /// True when values with a count were left out by `valueLimit`
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ItemFacets {
    /// CIs matching every filter: `page.total` of the same list query
    pub total: i64,
    #[schema(inline)]
    pub facets: Vec<Facet>,
}

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

// ---------------------------------------------------------------------------
// Completeness and count history (dashboard KPIs, SHAA-2350)
// ---------------------------------------------------------------------------

/// Which fields a complete CI holds a value for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum CompletenessBasis {
    /// Active fields that are required or expected (`isRequired`, `isExpected`)
    Expected,
    /// Every active field
    All,
}

fn basis_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(["expected", "all"]))
        .default(Some("expected".into()))
        .description(Some(
            "Fields counted: expected (active fields that are required or marked `isExpected`) or all (every active \
             field of the CI's class and its ancestors)",
        ))
        .into()
}

/// The filters of the inventory list (`listConfigurationItems`), without paging and sort, plus the basis.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct CompletenessQuery {
    #[param(required = false, schema_with = basis_schema)]
    pub basis: CompletenessBasis,
    #[param(schema_with = list_q_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    pub q: Option<String>,
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
    #[param(schema_with = criticality_filter_schema)]
    pub criticality_value_id: Option<UuidList>,
    #[param(required = false, schema_with = deleted_items_schema)]
    pub deleted: Deleted,
    #[param(schema_with = own_layout_schema)]
    pub own_layout: Option<QueryBool>,
    #[param(schema_with = layout_template_schema)]
    pub layout_template: Option<String>,
    #[param(schema_with = kind_schema)]
    pub kind: Option<KindQuery>,
    #[param(schema_with = business_service_filter_schema)]
    pub business_service_id: Option<UuidList>,
}
item_filters!(CompletenessQuery);

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ItemCompletenessQuery {
    #[param(required = false, schema_with = basis_schema)]
    pub basis: CompletenessBasis,
}

/// Completeness of a set of CIs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompletenessCounts {
    /// CIs counted
    pub items: i64,
    /// CIs holding a value in every counted field. A CI of a class with no counted field is complete.
    pub complete_items: i64,
    /// Values a complete set would hold: per CI, the number of counted fields of its class
    pub expected_values: i64,
    /// Of those, the values held
    pub filled_values: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClassCompleteness {
    pub class: LookupRef,
    /// Fields counted for CIs of this class (its own and inherited)
    pub counted_fields: i64,
    pub counts: CompletenessCounts,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Completeness {
    #[schema(inline)]
    pub basis: CompletenessBasis,
    /// Every CI matching the filters. Records complete is `completeItems / items`; the share of values filled is
    /// `filledValues / expectedValues` (treat both as complete when the divisor is 0).
    pub overall: CompletenessCounts,
    /// Per exact class (subclasses separately), only classes with a matching CI, most CIs first
    pub classes: Vec<ClassCompleteness>,
}

/// A counted field of a CI.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompletenessField {
    pub key: String,
    pub label: String,
    pub is_required: bool,
    pub is_expected: bool,
    pub filled: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ItemCompleteness {
    pub id: Uuid,
    #[schema(inline)]
    pub basis: CompletenessBasis,
    /// True when every counted field holds a value (also when none is counted)
    pub complete: bool,
    pub counted_fields: i64,
    pub filled_fields: i64,
    /// The counted fields in form order, filled or not
    pub fields: Vec<CompletenessField>,
}

/// Width of a count history bucket: UTC days, or ISO weeks starting Monday 00:00 UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum CountBucket {
    Day,
    Week,
}

impl CountBucket {
    /// The bucket width, and the longest range one request may cover.
    pub fn width_and_cap(self) -> (chrono::TimeDelta, chrono::TimeDelta) {
        match self {
            CountBucket::Day => (chrono::TimeDelta::days(1), chrono::TimeDelta::days(366)),
            CountBucket::Week => (chrono::TimeDelta::weeks(1), chrono::TimeDelta::weeks(260)),
        }
    }
}

fn count_bucket_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(["day", "week"]))
        .default(Some("day".into()))
        .description(Some(
            "Bucket width: UTC days (at most 366 per request) or ISO weeks from Monday 00:00 UTC (at most 260)",
        ))
        .into()
}

fn count_from_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .format(Some(SchemaFormat::KnownFormat(KnownFormat::DateTime)))
        .description(Some(
            "Start of the range (ISO 8601), rounded down to the start of its bucket. Default: 30 days before `to` \
             for day buckets, 12 weeks before for week buckets.",
        ))
        .into()
}

/// What a CI count includes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum CountActive {
    #[serde(rename = "true")]
    True,
    #[serde(rename = "all")]
    All,
}

fn count_active_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(["true", "all"]))
        .default(Some("true".into()))
        .description(Some(
            "true: a CI counts while it is registered (created, not deleted) and inside its validity period, as the \
             inventory list counts by default; all: while it is registered, whatever its validity",
        ))
        .into()
}

fn count_class_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .description(Some(
            "Only CIs of these classes (comma-separated ids), subclasses included unless includeSubclasses=false. \
             Process records are left out unless their type is named here.",
        ))
        .into()
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ItemCountHistoryQuery {
    #[param(schema_with = count_from_schema)]
    pub from: Option<DateTime<Utc>>,
    #[param(schema_with = histogram_to_schema)]
    pub to: Option<DateTime<Utc>>,
    #[param(required = false, schema_with = count_bucket_schema)]
    pub bucket: CountBucket,
    #[param(schema_with = count_class_schema)]
    pub class_id: Option<UuidList>,
    #[param(required = false, schema_with = include_subclasses_schema)]
    pub include_subclasses: QueryBool,
    #[param(required = false, schema_with = count_active_schema)]
    pub active: CountActive,
}

fn count_type_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .description(Some("Only relationships of these types (comma-separated ids)"))
        .into()
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct RelationshipCountHistoryQuery {
    #[param(schema_with = count_from_schema)]
    pub from: Option<DateTime<Utc>>,
    #[param(schema_with = histogram_to_schema)]
    pub to: Option<DateTime<Utc>>,
    #[param(required = false, schema_with = count_bucket_schema)]
    pub bucket: CountBucket,
    #[param(schema_with = count_type_schema)]
    pub relationship_type_id: Option<UuidList>,
}

/// One bucket of a count history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CountHistoryBucket {
    /// Start of the bucket (UTC)
    #[serde(serialize_with = "ts::serialize")]
    pub start: DateTime<Utc>,
    /// How many there were at the end of the bucket (at `to` for the last one)
    pub count: i64,
    /// How many started counting in the bucket (created, or entered their validity period)
    pub added: i64,
    /// How many stopped counting in the bucket (deleted, or left their validity period)
    pub removed: i64,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct DataQualityQuery {
    #[param(schema_with = end_of_life_days_schema)]
    pub end_of_life_within_days: Option<i32>,
}

/// The inventory filter a check's drill-down opens: `listConfigurationItems` with these query parameters.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DataQualityFilter {
    pub quality: QualityCheck,
    /// Set for the end_of_life check
    #[schema(required = true)]
    pub end_of_life_within_days: Option<i32>,
}

/// One data-quality check: how many CIs it finds.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DataQualityCheck {
    pub key: QualityCheck,
    /// CIs the check finds, among the active, live, non-process CIs the caller may view
    pub count: i64,
    /// False when the check cannot find anything yet: no type the caller may view has an owner field (no_owner)
    /// or an end-of-life field (end_of_life), in its own setting or an ancestor's. The UI shows "not configured"
    /// rather than a reassuring 0.
    pub configured: bool,
    /// The inventory list filter that lists these CIs
    pub filter: DataQualityFilter,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CountHistory {
    /// Start of the first bucket (`from` rounded down)
    #[serde(serialize_with = "ts::serialize")]
    pub from: DateTime<Utc>,
    /// End of the range (exclusive), as sent or defaulted
    #[serde(serialize_with = "ts::serialize")]
    pub to: DateTime<Utc>,
    #[schema(inline)]
    pub bucket: CountBucket,
    /// How many there were at `from`; each bucket's `count` is this plus the `added` minus the `removed` so far
    pub count_at_from: i64,
    /// Every bucket of the range in order, empty ones included
    pub buckets: Vec<CountHistoryBucket>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DataQuality {
    /// Every check, in a fixed order: no_owner, end_of_life, no_relationships, pending_approval
    pub checks: Vec<DataQualityCheck>,
}
