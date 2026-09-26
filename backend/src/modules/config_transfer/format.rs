//! The configuration file: one JSON document holding the data model, the
//! lookups, the permission profiles and the UI settings of an install.
//!
//! Everything refers to everything else by key (class key, list key, value
//! key, profile name), never by id, so a file moves between installs. Users,
//! passwords, sessions, CIs and relationships are never part of it.
//!
//! Every section is optional: a file with only `uiSettings` imports only the
//! UI settings. Within a section, fields that a hand-written file leaves out
//! take the same defaults as the create endpoints.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::openapi::schema::{ArrayBuilder, ObjectBuilder, Schema, Type};
use utoipa::{PartialSchema, ToSchema};

use super::super::classes::{AttributeDataType, ValidationRules};
use super::super::lookups::LocationType;
use super::super::ui_settings::assets::ImageType;
use super::super::ui_settings::document::UiSettingsDocument;
use crate::api::schemas::{self, OwnerKind, description_schema, key_schema, name_schema, sort_order_schema, trimmed};
use crate::auth::permissions::GlobalPermission;

pub const FORMAT: &str = "shadoucmdb.config";
pub const FORMAT_VERSION: i32 = 1;

fn yes() -> bool {
    true
}

fn nullable_key_schema() -> Schema {
    utoipa::openapi::schema::AnyOfBuilder::new()
        .item(key_schema())
        .item(ObjectBuilder::new().schema_type(Type::Null))
        .into()
}

fn format_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some([FORMAT]))
        .description(Some("Always \"shadoucmdb.config\""))
        .into()
}

fn list<T: ToSchema>(max: usize) -> Schema {
    ArrayBuilder::new().items(T::schema()).max_items(Some(max)).into()
}

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClassSpec {
    #[schema(schema_with = key_schema)]
    pub key: String,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    pub name: String,
    #[schema(schema_with = description_schema)]
    #[serde(default)]
    pub description: Option<String>,
    /// Key of the parent class (in the file or already in the target)
    #[schema(schema_with = nullable_key_schema)]
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub is_abstract: bool,
    #[schema(schema_with = crate::modules::classes::icon_schema)]
    #[serde(default)]
    pub icon: Option<String>,
    #[schema(schema_with = schemas::nullable_color_schema)]
    #[serde(default)]
    pub color: Option<String>,
    #[schema(schema_with = sort_order_schema)]
    #[serde(default)]
    pub sort_order: i32,
    #[serde(default = "yes")]
    pub is_active: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttributeSpec {
    /// Key of the class that defines it
    #[schema(schema_with = key_schema)]
    pub class: String,
    #[schema(schema_with = key_schema)]
    pub key: String,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    pub label: String,
    #[schema(schema_with = description_schema)]
    #[serde(default)]
    pub description: Option<String>,
    #[schema(inline)]
    pub data_type: AttributeDataType,
    #[serde(default)]
    pub is_required: bool,
    #[schema(schema_with = crate::modules::classes::enum_values_schema)]
    #[serde(default)]
    pub enum_values: Option<Vec<String>>,
    /// reference attributes: key of the class the value must be a CI of
    #[schema(schema_with = nullable_key_schema)]
    #[serde(default)]
    pub reference_class: Option<String>,
    /// lookup attributes: key of the lookup list
    #[schema(schema_with = nullable_key_schema)]
    #[serde(default)]
    pub lookup_list: Option<String>,
    #[schema(schema_with = crate::modules::classes::validation_schema)]
    #[serde(default)]
    pub validation: Option<ValidationRules>,
    #[schema(schema_with = crate::modules::classes::group_name_schema)]
    #[serde(default)]
    pub group_name: Option<String>,
    #[schema(schema_with = crate::modules::classes::help_text_schema)]
    #[serde(default)]
    pub help_text: Option<String>,
    /// As for the attribute API, except that a lookup default is the list value's key
    #[schema(schema_with = crate::modules::classes::default_value_schema)]
    #[serde(default)]
    pub default_value: Option<Value>,
    #[schema(schema_with = sort_order_schema)]
    #[serde(default)]
    pub sort_order: i32,
    #[serde(default = "yes")]
    pub is_active: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationshipTypeSpec {
    #[schema(schema_with = key_schema)]
    pub key: String,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    pub name: String,
    #[schema(schema_with = description_schema)]
    #[serde(default)]
    pub description: Option<String>,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    pub forward_label: String,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    pub reverse_label: String,
    #[serde(default = "yes")]
    pub is_directional: bool,
    #[schema(schema_with = sort_order_schema)]
    #[serde(default)]
    pub sort_order: i32,
    #[serde(default = "yes")]
    pub is_active: bool,
}

/// Which classes a relationship type may connect (by keys)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationshipRuleSpec {
    #[schema(schema_with = key_schema)]
    pub relationship_type: String,
    #[schema(schema_with = key_schema)]
    pub source_class: String,
    #[schema(schema_with = key_schema)]
    pub target_class: String,
}

fn classes_schema() -> Schema {
    list::<ClassSpec>(5000)
}
fn attributes_schema() -> Schema {
    list::<AttributeSpec>(50_000)
}
fn relationship_types_schema() -> Schema {
    list::<RelationshipTypeSpec>(1000)
}
fn relationship_rules_schema() -> Schema {
    list::<RelationshipRuleSpec>(20_000)
}

/// Classes (parents before children is not required), attributes, relationship types and rules
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct DataModelSection {
    #[schema(schema_with = classes_schema)]
    pub classes: Vec<ClassSpec>,
    #[schema(schema_with = attributes_schema)]
    pub attributes: Vec<AttributeSpec>,
    #[schema(schema_with = relationship_types_schema)]
    pub relationship_types: Vec<RelationshipTypeSpec>,
    #[schema(schema_with = relationship_rules_schema)]
    pub relationship_rules: Vec<RelationshipRuleSpec>,
}

// ---------------------------------------------------------------------------
// Lookups
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StatusSpec {
    #[schema(schema_with = key_schema)]
    pub key: String,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    pub name: String,
    #[schema(schema_with = description_schema)]
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub is_operational: bool,
    #[schema(schema_with = sort_order_schema)]
    #[serde(default)]
    pub sort_order: i32,
    #[serde(default = "yes")]
    pub is_active: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentSpec {
    #[schema(schema_with = key_schema)]
    pub key: String,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    pub name: String,
    #[schema(schema_with = description_schema)]
    #[serde(default)]
    pub description: Option<String>,
    #[schema(schema_with = sort_order_schema)]
    #[serde(default)]
    pub sort_order: i32,
    #[serde(default = "yes")]
    pub is_active: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocationSpec {
    #[schema(schema_with = key_schema)]
    pub key: String,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    pub name: String,
    #[schema(schema_with = description_schema)]
    #[serde(default)]
    pub description: Option<String>,
    /// Key of the parent location
    #[schema(schema_with = nullable_key_schema)]
    #[serde(default)]
    pub parent: Option<String>,
    #[schema(inline)]
    pub location_type: LocationType,
    #[schema(schema_with = crate::modules::lookups::address_schema)]
    #[serde(default)]
    pub address: Option<String>,
    #[schema(schema_with = sort_order_schema)]
    #[serde(default)]
    pub sort_order: i32,
    #[serde(default = "yes")]
    pub is_active: bool,
}

/// Owners have no key; they are matched by kind and name (case-insensitive)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwnerSpec {
    #[schema(inline)]
    pub kind: OwnerKind,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    pub name: String,
    #[schema(schema_with = crate::modules::lookups::email_schema)]
    #[serde(default)]
    pub email: Option<String>,
    #[schema(schema_with = crate::modules::lookups::external_ref_schema)]
    #[serde(default)]
    pub external_ref: Option<String>,
    #[serde(default = "yes")]
    pub is_active: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LookupValueSpec {
    #[schema(schema_with = key_schema)]
    pub key: String,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    pub name: String,
    #[schema(schema_with = description_schema)]
    #[serde(default)]
    pub description: Option<String>,
    #[schema(schema_with = schemas::nullable_color_schema)]
    #[serde(default)]
    pub color: Option<String>,
    #[schema(schema_with = sort_order_schema)]
    #[serde(default)]
    pub sort_order: i32,
    #[serde(default = "yes")]
    pub is_active: bool,
}

fn values_schema() -> Schema {
    list::<LookupValueSpec>(5000)
}

/// An admin-defined lookup list with its values
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LookupListSpec {
    #[schema(schema_with = key_schema)]
    pub key: String,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    pub name: String,
    #[schema(schema_with = description_schema)]
    #[serde(default)]
    pub description: Option<String>,
    #[schema(schema_with = sort_order_schema)]
    #[serde(default)]
    pub sort_order: i32,
    #[serde(default = "yes")]
    pub is_active: bool,
    #[schema(schema_with = values_schema)]
    #[serde(default)]
    pub values: Vec<LookupValueSpec>,
}

fn statuses_schema() -> Schema {
    list::<StatusSpec>(1000)
}
fn environments_schema() -> Schema {
    list::<EnvironmentSpec>(1000)
}
fn locations_schema() -> Schema {
    list::<LocationSpec>(20_000)
}
fn owners_schema() -> Schema {
    list::<OwnerSpec>(20_000)
}
fn lists_schema() -> Schema {
    list::<LookupListSpec>(1000)
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct LookupSection {
    #[schema(schema_with = statuses_schema)]
    pub statuses: Vec<StatusSpec>,
    #[schema(schema_with = environments_schema)]
    pub environments: Vec<EnvironmentSpec>,
    #[schema(schema_with = locations_schema)]
    pub locations: Vec<LocationSpec>,
    #[schema(schema_with = owners_schema)]
    pub owners: Vec<OwnerSpec>,
    #[schema(schema_with = lists_schema)]
    pub lists: Vec<LookupListSpec>,
}

// ---------------------------------------------------------------------------
// Permission profiles
// ---------------------------------------------------------------------------

/// Rights on one class (by key), or on every class when `class` is null
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClassGrantSpec {
    #[schema(schema_with = nullable_key_schema)]
    pub class: Option<String>,
    #[serde(default)]
    pub view: bool,
    #[serde(default)]
    pub create: bool,
    #[serde(default)]
    pub edit: bool,
    #[serde(default)]
    pub delete: bool,
}

fn global_permissions_schema() -> Schema {
    ArrayBuilder::new().items(GlobalPermission::schema()).unique_items(true).into()
}
fn class_grants_schema() -> Schema {
    list::<ClassGrantSpec>(5000)
}

/// A permission profile, matched by name (case-insensitive). The built-in Administrator profile is never exported or imported.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileSpec {
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    pub name: String,
    #[schema(schema_with = description_schema)]
    #[serde(default)]
    pub description: Option<String>,
    #[schema(schema_with = global_permissions_schema)]
    #[serde(default)]
    pub global_permissions: Vec<GlobalPermission>,
    #[schema(schema_with = class_grants_schema)]
    #[serde(default)]
    pub class_permissions: Vec<ClassGrantSpec>,
}

// ---------------------------------------------------------------------------
// UI settings
// ---------------------------------------------------------------------------

/// An image, base64-encoded
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetData {
    #[schema(inline)]
    pub content_type: ImageType,
    #[schema(min_length = 1, max_length = 700_000)]
    pub data: String,
}

/// The UI settings document with the logo and favicon. Importing it replaces
/// the current settings (as a new version) and the images: a null logo or
/// favicon removes the current one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiSettingsSection {
    pub settings: UiSettingsDocument,
    #[serde(default)]
    pub logo: Option<AssetData>,
    #[serde(default)]
    pub favicon: Option<AssetData>,
}

fn profiles_schema() -> Schema {
    list::<ProfileSpec>(1000)
}

fn exported_at_schema() -> Schema {
    schemas::nullable_string_schema(64)
}

/// A whole configuration: data model, lookups, permission profiles and UI settings (no users, passwords or CIs)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigFile {
    #[schema(schema_with = format_schema)]
    pub format: String,
    /// File format version; this server reads version 1
    #[schema(minimum = 1, maximum = 1)]
    pub format_version: i32,
    /// When and by which server version the file was written (informational)
    #[schema(schema_with = exported_at_schema)]
    #[serde(default)]
    pub exported_at: Option<String>,
    #[schema(schema_with = exported_at_schema)]
    #[serde(default)]
    pub app_version: Option<String>,
    #[serde(default)]
    pub data_model: Option<DataModelSection>,
    #[serde(default)]
    pub lookups: Option<LookupSection>,
    #[schema(schema_with = profiles_schema)]
    #[serde(default)]
    pub permission_profiles: Option<Vec<ProfileSpec>>,
    #[serde(default)]
    pub ui_settings: Option<UiSettingsSection>,
}

impl crate::api::route::Check for ConfigFile {
    fn check(&self) -> Vec<crate::http::error::FieldError> {
        match &self.ui_settings {
            Some(ui) => ui.settings.problems("uiSettings.settings."),
            None => Vec::new(),
        }
    }
}
