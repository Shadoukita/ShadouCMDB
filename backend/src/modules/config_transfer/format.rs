//! The configuration file: one JSON document holding the data model, the
//! lookups, the permission profiles and the UI settings of an install.
//!
//! Everything refers to everything else by key (class key, list key, value
//! key, profile name), never by id, so a file moves between installs. The
//! built-in classes and relationship types are the exception: they are matched
//! by their system role (version 5), because their key differs between
//! installs. Users, user groups, passwords, sessions, CIs, relationships and
//! business services with their members and owners are never part of it.
//!
//! Identity providers are not part of it either. If they ever are, their
//! secrets stay out: a provider carries `clientSecretSet` / `bindPasswordSet`
//! only, an import creates it disabled, and an administrator enters the secret
//! again. Never export the ciphertext: it is bound to the source provider's id
//! and to the source install's encryption key, so it would be useless and
//! misleading (GH#199, SHAA-490 §A7).
//!
//! Every section is optional: a file with only `uiSettings` imports only the
//! UI settings. Within a section, fields that a hand-written file leaves out
//! take the same defaults as the create endpoints.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::openapi::schema::{ArrayBuilder, ObjectBuilder, Schema, Type};
use utoipa::{PartialSchema, ToSchema};

use super::super::classes::{AttributeDataType, ClassSystemRole, RelationshipTypeSystemRole, ValidationRules};
use super::super::impact::ImpactDirection;
use super::super::lookups::{LocationType, SystemRole};
use super::super::ui_settings::assets::ImageType;
use super::super::ui_settings::document::UiSettingsDocument;
use crate::api::schemas::{self, OwnerKind, description_schema, key_schema, name_schema, sort_order_schema, trimmed};
use crate::auth::permissions::GlobalPermission;

pub const FORMAT: &str = "shadoucmdb.config";
/// Version 2 adds areas (and the area of each class), version 3 dependent
/// lookup lists (the parent of a list, a value and a field), version 4 the
/// impact direction of relationship types, the system role of lookup lists
/// (the criticality list) and saved import mappings, version 5 the system role
/// of classes and relationship types (business services) and of class grants;
/// versions 1 to 4 are still read.
pub const FORMAT_VERSION: i32 = 5;

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

/// An area: a menu tab and the PostgreSQL schema of its types' tables
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AreaSpec {
    /// Technical name: the schema name
    #[schema(schema_with = key_schema)]
    pub key: String,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    pub name: String,
    #[schema(schema_with = description_schema)]
    #[serde(default)]
    pub description: Option<String>,
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
pub struct ClassSpec {
    /// Technical name: the table name in the area's schema
    #[schema(schema_with = key_schema)]
    pub key: String,
    /// Key of the area (in the file or already in the target). Left out (version 1 files): the class's current
    /// area, or "infrastruktur" for a new class. A class cannot move to another area.
    #[schema(schema_with = nullable_key_schema)]
    #[serde(default)]
    pub area: Option<String>,
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
    /// Key of the field (of the class or an ancestor) whose value labels its CIs; null: labelled by their ident.
    /// Left out (files from before SHAA-267): unchanged, or the parent's for a new class.
    #[schema(schema_with = nullable_key_schema)]
    #[serde(default, deserialize_with = "schemas::patch", skip_serializing_if = "Option::is_none")]
    pub title_attribute: Option<Option<String>>,
    /// Set on the built-in business service class (version 5). An import matches such a class to this install's
    /// class of the same role, whatever its key, and keeps that class's key and area; it never gives a class a
    /// role or takes one away
    #[schema(inline)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_role: Option<ClassSystemRole>,
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
    /// lookup attributes on a list with a parent list: key of the field bound to the parent list, defined on
    /// this class or an ancestor. Left out in files before version 3: an existing field keeps its parent field.
    #[schema(schema_with = nullable_key_schema)]
    #[serde(default)]
    pub parent_attribute: Option<String>,
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
    /// How impact flows across the type's edges. Left out (files before version 4): an existing type keeps its
    /// value, a new one gets none
    #[schema(inline)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact_direction: Option<ImpactDirection>,
    #[schema(schema_with = sort_order_schema)]
    #[serde(default)]
    pub sort_order: i32,
    #[serde(default = "yes")]
    pub is_active: bool,
    /// Set on the built-in business service membership type (version 5). An import matches such a type to this
    /// install's type of the same role, whatever its key, and keeps that type's key; it never gives a type a role
    /// or takes one away
    #[schema(inline)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_role: Option<RelationshipTypeSystemRole>,
}

/// Which classes a relationship type may connect (by keys). Ordered by
/// (type, source, target), the order export writes them in.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema)]
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

fn areas_schema() -> Schema {
    list::<AreaSpec>(1_000)
}

/// Areas, classes (parents before children is not required), attributes, relationship types and rules
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct DataModelSection {
    #[schema(schema_with = areas_schema)]
    pub areas: Vec<AreaSpec>,
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

// The former statuses, environments, locations and owners tables. Since
// migration 0016 their values are lookup lists; files of older releases still
// carry them, and the import converts them (`legacy_lookups`).

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
    /// Values of a list with a parent list: key of the value of the parent list it belongs to
    #[schema(schema_with = nullable_key_schema)]
    #[serde(default)]
    pub parent: Option<String>,
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
    /// Key of the list this list depends on (in the file or already in the target)
    #[schema(schema_with = nullable_key_schema)]
    #[serde(default)]
    pub parent: Option<String>,
    /// Set on a system list (criticality). Informational: an import never gives a list a system role or takes
    /// one away, and never deletes a list
    #[schema(inline)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_role: Option<SystemRole>,
    #[schema(schema_with = values_schema)]
    #[serde(default)]
    pub values: Vec<LookupValueSpec>,
}

/// A section of files exported before this release: read on import, never written.
fn legacy<T: ToSchema>(max: usize, table: &str, list: &str) -> Schema {
    ArrayBuilder::new()
        .items(T::schema())
        .max_items(Some(max))
        .description(Some(format!(
            "Deprecated, read on import only: rows of the former {table} table, written by exports of \
             0.1.0-rc.1 and earlier builds. Exports leave it out; the values are in `lists` (list \"{list}\"). On \
             import the rows become values of the lookup list \"{list}\", as migration 0016 converts the table, \
             unless the file's own lists already hold them; the former table is never written."
        )))
        .deprecated(Some(utoipa::openapi::Deprecated::True))
        .into()
}
fn statuses_schema() -> Schema {
    legacy::<StatusSpec>(1000, "statuses", "status")
}
fn environments_schema() -> Schema {
    legacy::<EnvironmentSpec>(1000, "environments", "environment")
}
fn locations_schema() -> Schema {
    legacy::<LocationSpec>(20_000, "locations", "location")
}
fn owners_schema() -> Schema {
    legacy::<OwnerSpec>(20_000, "owners", "owner")
}
fn lists_schema() -> Schema {
    list::<LookupListSpec>(1000)
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct LookupSection {
    #[schema(schema_with = statuses_schema)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub statuses: Vec<StatusSpec>,
    #[schema(schema_with = environments_schema)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub environments: Vec<EnvironmentSpec>,
    #[schema(schema_with = locations_schema)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub locations: Vec<LocationSpec>,
    #[schema(schema_with = owners_schema)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
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
    /// Set when `class` is a built-in class (version 5). On import the grant applies to this install's class of
    /// that role, whatever `class` says
    #[schema(inline)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class_system_role: Option<ClassSystemRole>,
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

// ---------------------------------------------------------------------------
// Saved import mappings
// ---------------------------------------------------------------------------

/// A saved import mapping (SHAA-714 §6.2), matched by class key and name
/// (case-insensitive). Its class, attributes and relationship types are
/// referenced by key, so it moves between installs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportMappingSpec {
    #[schema(min_length = 1, max_length = 100)]
    #[serde(deserialize_with = "trimmed")]
    pub name: String,
    #[schema(max_length = 500)]
    #[serde(default)]
    pub description: Option<String>,
    /// Key of the class the mapping imports into
    #[schema(schema_with = key_schema)]
    pub class_key: String,
    pub definition: crate::modules::imports::saved::MappingDefinition,
}

fn import_mappings_schema() -> Schema {
    list::<ImportMappingSpec>(crate::modules::imports::saved::MAX_SAVED as usize)
}

fn exported_at_schema() -> Schema {
    schemas::nullable_string_schema(64)
}

/// A whole configuration: data model, lookups, permission profiles, UI settings and saved import mappings (no users, passwords or CIs)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigFile {
    #[schema(schema_with = format_schema)]
    pub format: String,
    /// File format version; this server writes version 5 and reads 1 to 5
    #[schema(minimum = 1, maximum = 5)]
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
    /// Left out of an export when the caller may not read profiles (GH#186).
    #[schema(schema_with = profiles_schema)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_profiles: Option<Vec<ProfileSpec>>,
    #[serde(default)]
    pub ui_settings: Option<UiSettingsSection>,
    /// Saved import mappings (version 4). Left out of an export when the caller does not hold `cis.import`.
    #[schema(schema_with = import_mappings_schema)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_mappings: Option<Vec<ImportMappingSpec>>,
}

impl crate::api::route::Check for ConfigFile {
    fn check(&self) -> Vec<crate::http::error::FieldError> {
        match &self.ui_settings {
            Some(ui) => ui.settings.problems("uiSettings.settings."),
            None => Vec::new(),
        }
    }
}
