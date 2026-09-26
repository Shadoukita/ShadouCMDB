//! Lookup tables: statuses, environments, locations, owners, plus lookup lists
//! an administrator defines (values for "lookup" attributes). Rows are renamed
//! and retired (isActive=false) rather than deleted once CIs reference them.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::simple_resource::{self as simple, ListQuery, Resource, Usage, Writable, bool_filter, non_empty};
use crate::api::route::{Check, Route};
use crate::api::schemas::{
    self, IdOrNone, OwnerKind, QueryBool, Sort, UuidList, description_schema, key_schema, name_schema,
    nullable_uuid_schema, sort_order_schema, trimmed, ts,
};
use crate::data::crud::{ColumnSet, Where};
use crate::http::error::FieldError;
use crate::paged;

const LOOKUP_SORT_FIELDS: &[&str] = &["sortOrder", "name", "key", "createdAt", "updatedAt"];

fn lookup_sort_by_order() -> utoipa::openapi::schema::Schema {
    schemas::sort_schema(LOOKUP_SORT_FIELDS, "sortOrder")
}
fn lookup_sort_by_name() -> utoipa::openapi::schema::Schema {
    schemas::sort_schema(LOOKUP_SORT_FIELDS, "name")
}

// ===========================================================================
// Statuses
// ===========================================================================

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Status {
    pub id: Uuid,
    pub key: String,
    pub name: String,
    #[schema(required = true)]
    pub description: Option<String>,
    pub sort_order: i32,
    pub is_active: bool,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
    /// Counts as "live" in reports (in_service, maintenance)
    pub is_operational: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StatusCreate {
    #[schema(schema_with = key_schema)]
    key: String,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    name: String,
    #[schema(schema_with = description_schema)]
    #[serde(default)]
    description: Option<String>,
    #[schema(schema_with = sort_order_schema)]
    sort_order: Option<i32>,
    #[schema(nullable = false)]
    is_active: Option<bool>,
    #[schema(nullable = false)]
    is_operational: Option<bool>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StatusUpdate {
    #[schema(schema_with = key_schema)]
    key: Option<String>,
    #[schema(schema_with = name_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    name: Option<String>,
    #[schema(schema_with = description_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    description: Option<Option<String>>,
    #[schema(schema_with = sort_order_schema)]
    sort_order: Option<i32>,
    #[schema(nullable = false)]
    is_active: Option<bool>,
    #[schema(nullable = false)]
    is_operational: Option<bool>,
}

impl Writable for StatusCreate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("key", Some(self.key.clone()))
            .opt("name", Some(self.name.clone()))
            .opt("description", self.description.clone().map(Some))
            .opt("sort_order", self.sort_order)
            .opt("is_active", self.is_active)
            .opt("is_operational", self.is_operational);
        c
    }
}
impl Check for StatusCreate {}

impl Writable for StatusUpdate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("key", self.key.clone())
            .opt("name", self.name.clone())
            .opt("description", self.description.clone())
            .opt("sort_order", self.sort_order)
            .opt("is_active", self.is_active)
            .opt("is_operational", self.is_operational);
        c
    }
}
impl Check for StatusUpdate {
    fn check(&self) -> Vec<FieldError> {
        non_empty(&self.columns())
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct StatusList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    offset: i64,
    #[param(schema_with = schemas::search_schema)]
    q: Option<String>,
    #[param(required = false, schema_with = lookup_sort_by_order)]
    sort: Sort,
    #[param(inline)]
    is_active: Option<QueryBool>,
    #[param(inline)]
    is_operational: Option<QueryBool>,
}
paged!(StatusList);

impl ListQuery for StatusList {
    fn q(&self) -> Option<&str> {
        self.q.as_deref()
    }
    fn sort(&self) -> &Sort {
        &self.sort
    }
    fn filter(&self, w: &mut Where<'_>) {
        bool_filter(w, "is_active", self.is_active);
        bool_filter(w, "is_operational", self.is_operational);
    }
}

pub struct Statuses;

impl Resource for Statuses {
    type Dto = Status;
    type Create = StatusCreate;
    type Update = StatusUpdate;
    type List = StatusList;
    const TABLE: &'static str = "statuses";
    const LABEL: &'static str = "Status";
    const BASE_PATH: &'static str = "/api/v1/statuses";
    const TAG: &'static str = "Statuses";
    const SINGULAR: &'static str = "status";
    const PLURAL: &'static str = "statuses";
    const COLUMNS: &'static str =
        "id, key, name, description, sort_order, is_active, created_at, updated_at, is_operational";
    const SEARCH_COLUMNS: &'static [&'static str] = &["key", "name", "description"];
    const USAGE: &'static [Usage] = &[
        Usage {
            kind: "configurationItems",
            label: "configuration items",
            sql: "SELECT count(*) FROM configuration_items WHERE status_id = $1 AND deleted_at IS NULL",
            blocking: true,
        },
        Usage {
            kind: "deletedConfigurationItems",
            label: "deleted configuration items (kept for history)",
            sql: "SELECT count(*) FROM configuration_items WHERE status_id = $1 AND deleted_at IS NOT NULL",
            blocking: true,
        },
    ];
    fn id(row: &Status) -> Uuid {
        row.id
    }
}

// ===========================================================================
// Environments
// ===========================================================================

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Environment {
    pub id: Uuid,
    pub key: String,
    pub name: String,
    #[schema(required = true)]
    pub description: Option<String>,
    pub sort_order: i32,
    pub is_active: bool,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentCreate {
    #[schema(schema_with = key_schema)]
    key: String,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    name: String,
    #[schema(schema_with = description_schema)]
    #[serde(default)]
    description: Option<String>,
    #[schema(schema_with = sort_order_schema)]
    sort_order: Option<i32>,
    #[schema(nullable = false)]
    is_active: Option<bool>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentUpdate {
    #[schema(schema_with = key_schema)]
    key: Option<String>,
    #[schema(schema_with = name_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    name: Option<String>,
    #[schema(schema_with = description_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    description: Option<Option<String>>,
    #[schema(schema_with = sort_order_schema)]
    sort_order: Option<i32>,
    #[schema(nullable = false)]
    is_active: Option<bool>,
}

impl Writable for EnvironmentCreate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("key", Some(self.key.clone()))
            .opt("name", Some(self.name.clone()))
            .opt("description", self.description.clone().map(Some))
            .opt("sort_order", self.sort_order)
            .opt("is_active", self.is_active);
        c
    }
}
impl Check for EnvironmentCreate {}

impl Writable for EnvironmentUpdate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("key", self.key.clone())
            .opt("name", self.name.clone())
            .opt("description", self.description.clone())
            .opt("sort_order", self.sort_order)
            .opt("is_active", self.is_active);
        c
    }
}
impl Check for EnvironmentUpdate {
    fn check(&self) -> Vec<FieldError> {
        non_empty(&self.columns())
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct EnvironmentList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    offset: i64,
    #[param(schema_with = schemas::search_schema)]
    q: Option<String>,
    #[param(required = false, schema_with = lookup_sort_by_order)]
    sort: Sort,
    #[param(inline)]
    is_active: Option<QueryBool>,
}
paged!(EnvironmentList);

impl ListQuery for EnvironmentList {
    fn q(&self) -> Option<&str> {
        self.q.as_deref()
    }
    fn sort(&self) -> &Sort {
        &self.sort
    }
    fn filter(&self, w: &mut Where<'_>) {
        bool_filter(w, "is_active", self.is_active);
    }
}

pub struct Environments;

impl Resource for Environments {
    type Dto = Environment;
    type Create = EnvironmentCreate;
    type Update = EnvironmentUpdate;
    type List = EnvironmentList;
    const TABLE: &'static str = "environments";
    const LABEL: &'static str = "Environment";
    const BASE_PATH: &'static str = "/api/v1/environments";
    const TAG: &'static str = "Environments";
    const SINGULAR: &'static str = "environment";
    const PLURAL: &'static str = "environments";
    const COLUMNS: &'static str = "id, key, name, description, sort_order, is_active, created_at, updated_at";
    const SEARCH_COLUMNS: &'static [&'static str] = &["key", "name", "description"];
    const USAGE: &'static [Usage] = &[
        Usage {
            kind: "configurationItems",
            label: "configuration items",
            sql: "SELECT count(*) FROM configuration_items WHERE environment_id = $1 AND deleted_at IS NULL",
            blocking: true,
        },
        Usage {
            kind: "deletedConfigurationItems",
            label: "deleted configuration items (kept for history)",
            sql: "SELECT count(*) FROM configuration_items WHERE environment_id = $1 AND deleted_at IS NOT NULL",
            blocking: true,
        },
    ];
    fn id(row: &Environment) -> Uuid {
        row.id
    }
}

// ===========================================================================
// Locations
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum LocationType {
    Region,
    Site,
    Building,
    Floor,
    Room,
    Rack,
    CloudRegion,
    Other,
}

impl LocationType {
    fn as_str(self) -> &'static str {
        match self {
            LocationType::Region => "region",
            LocationType::Site => "site",
            LocationType::Building => "building",
            LocationType::Floor => "floor",
            LocationType::Room => "room",
            LocationType::Rack => "rack",
            LocationType::CloudRegion => "cloud_region",
            LocationType::Other => "other",
        }
    }
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Location {
    pub id: Uuid,
    pub key: String,
    pub name: String,
    #[schema(required = true)]
    pub description: Option<String>,
    pub sort_order: i32,
    pub is_active: bool,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
    /// Parent location (region > site > building > floor > room > rack)
    #[schema(required = true)]
    pub parent_id: Option<Uuid>,
    #[schema(inline)]
    pub location_type: LocationType,
    #[schema(required = true)]
    pub address: Option<String>,
}

fn address_schema() -> utoipa::openapi::schema::Schema {
    schemas::nullable_string_schema(1000)
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocationCreate {
    #[schema(schema_with = key_schema)]
    key: String,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    name: String,
    #[schema(schema_with = description_schema)]
    #[serde(default)]
    description: Option<String>,
    #[schema(schema_with = sort_order_schema)]
    sort_order: Option<i32>,
    #[schema(nullable = false)]
    is_active: Option<bool>,
    #[schema(schema_with = nullable_uuid_schema)]
    #[serde(default)]
    parent_id: Option<Uuid>,
    #[schema(inline)]
    location_type: LocationType,
    #[schema(schema_with = address_schema)]
    #[serde(default)]
    address: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocationUpdate {
    #[schema(schema_with = key_schema)]
    key: Option<String>,
    #[schema(schema_with = name_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    name: Option<String>,
    #[schema(schema_with = description_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    description: Option<Option<String>>,
    #[schema(schema_with = sort_order_schema)]
    sort_order: Option<i32>,
    #[schema(nullable = false)]
    is_active: Option<bool>,
    #[schema(schema_with = nullable_uuid_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    parent_id: Option<Option<Uuid>>,
    #[schema(inline, nullable = false)]
    location_type: Option<LocationType>,
    #[schema(schema_with = address_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    address: Option<Option<String>>,
}

impl Writable for LocationCreate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("key", Some(self.key.clone()))
            .opt("name", Some(self.name.clone()))
            .opt("description", self.description.clone().map(Some))
            .opt("sort_order", self.sort_order)
            .opt("is_active", self.is_active)
            .opt("parent_id", self.parent_id.map(Some))
            .opt("location_type", Some(self.location_type.as_str().to_owned()))
            .opt("address", self.address.clone().map(Some));
        c
    }
}
impl Check for LocationCreate {}

impl Writable for LocationUpdate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("key", self.key.clone())
            .opt("name", self.name.clone())
            .opt("description", self.description.clone())
            .opt("sort_order", self.sort_order)
            .opt("is_active", self.is_active)
            .opt("parent_id", self.parent_id)
            .opt("location_type", self.location_type.map(|t| t.as_str().to_owned()))
            .opt("address", self.address.clone());
        c
    }
}
impl Check for LocationUpdate {
    fn check(&self) -> Vec<FieldError> {
        non_empty(&self.columns())
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct LocationList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    offset: i64,
    #[param(schema_with = schemas::search_schema)]
    q: Option<String>,
    #[param(required = false, schema_with = lookup_sort_by_name)]
    sort: Sort,
    #[param(inline)]
    is_active: Option<QueryBool>,
    #[param(schema_with = location_parent_schema)]
    parent_id: Option<IdOrNone>,
    #[param(inline)]
    location_type: Option<LocationType>,
}
paged!(LocationList);

impl ListQuery for LocationList {
    fn q(&self) -> Option<&str> {
        self.q.as_deref()
    }
    fn sort(&self) -> &Sort {
        &self.sort
    }
    fn filter(&self, w: &mut Where<'_>) {
        bool_filter(w, "is_active", self.is_active);
        match self.parent_id {
            Some(IdOrNone::None) => w.and_sql("parent_id IS NULL"),
            Some(IdOrNone::Id(id)) => {
                w.and().push("parent_id = ").push_bind(id);
            }
            None => {}
        }
        if let Some(t) = self.location_type {
            w.and().push("location_type = ").push_bind(t.as_str());
        }
    }
}

fn location_parent_schema() -> utoipa::openapi::schema::Schema {
    schemas::id_or_none_schema("Children of this location; \"none\" for top-level locations")
}

pub struct Locations;

impl Resource for Locations {
    type Dto = Location;
    type Create = LocationCreate;
    type Update = LocationUpdate;
    type List = LocationList;
    const TABLE: &'static str = "locations";
    const LABEL: &'static str = "Location";
    const BASE_PATH: &'static str = "/api/v1/locations";
    const TAG: &'static str = "Locations";
    const SINGULAR: &'static str = "location";
    const PLURAL: &'static str = "locations";
    const COLUMNS: &'static str =
        "id, key, name, description, sort_order, is_active, created_at, updated_at, parent_id, location_type, address";
    const SEARCH_COLUMNS: &'static [&'static str] = &["key", "name", "description", "address"];
    const USAGE: &'static [Usage] = &[
        Usage {
            kind: "configurationItems",
            label: "configuration items",
            sql: "SELECT count(*) FROM configuration_items WHERE location_id = $1 AND deleted_at IS NULL",
            blocking: true,
        },
        Usage {
            kind: "deletedConfigurationItems",
            label: "deleted configuration items (kept for history)",
            sql: "SELECT count(*) FROM configuration_items WHERE location_id = $1 AND deleted_at IS NOT NULL",
            blocking: true,
        },
        Usage {
            kind: "childLocations",
            label: "child locations",
            sql: "SELECT count(*) FROM locations WHERE parent_id = $1",
            blocking: true,
        },
    ];
    fn id(row: &Location) -> Uuid {
        row.id
    }
}

// ===========================================================================
// Owners
// ===========================================================================

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Owner {
    pub id: Uuid,
    #[schema(inline)]
    pub kind: OwnerKind,
    pub name: String,
    #[schema(required = true)]
    pub email: Option<String>,
    /// Identifier in an external directory (LDAP DN, IdP subject, HR id)
    #[schema(required = true)]
    pub external_ref: Option<String>,
    pub is_active: bool,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
}

pub fn email_schema() -> utoipa::openapi::schema::Schema {
    utoipa::openapi::schema::AnyOfBuilder::new()
        .item(
            utoipa::openapi::schema::ObjectBuilder::new()
                .schema_type(utoipa::openapi::schema::Type::String)
                .format(Some(utoipa::openapi::schema::SchemaFormat::KnownFormat(
                    utoipa::openapi::schema::KnownFormat::Email,
                )))
                .max_length(Some(320)),
        )
        .item(utoipa::openapi::schema::ObjectBuilder::new().schema_type(utoipa::openapi::schema::Type::Null))
        .into()
}

fn external_ref_schema() -> utoipa::openapi::schema::Schema {
    schemas::nullable_trimmed_schema(500)
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwnerCreate {
    #[schema(inline)]
    kind: OwnerKind,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    name: String,
    #[schema(schema_with = email_schema)]
    #[serde(default)]
    email: Option<String>,
    #[schema(schema_with = external_ref_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    external_ref: Option<String>,
    #[schema(nullable = false)]
    is_active: Option<bool>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwnerUpdate {
    #[schema(inline, nullable = false)]
    kind: Option<OwnerKind>,
    #[schema(schema_with = name_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    name: Option<String>,
    #[schema(schema_with = email_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    email: Option<Option<String>>,
    #[schema(schema_with = external_ref_schema)]
    #[serde(default, deserialize_with = "schemas::patch_trimmed")]
    external_ref: Option<Option<String>>,
    #[schema(nullable = false)]
    is_active: Option<bool>,
}

fn kind_str(k: OwnerKind) -> String {
    match k {
        OwnerKind::Person => "person".into(),
        OwnerKind::Team => "team".into(),
    }
}

impl Writable for OwnerCreate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("kind", Some(kind_str(self.kind)))
            .opt("name", Some(self.name.clone()))
            .opt("email", self.email.clone().map(Some))
            .opt("external_ref", self.external_ref.clone().map(Some))
            .opt("is_active", self.is_active);
        c
    }
}
impl Check for OwnerCreate {}

impl Writable for OwnerUpdate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("kind", self.kind.map(kind_str))
            .opt("name", self.name.clone())
            .opt("email", self.email.clone())
            .opt("external_ref", self.external_ref.clone())
            .opt("is_active", self.is_active);
        c
    }
}
impl Check for OwnerUpdate {
    fn check(&self) -> Vec<FieldError> {
        non_empty(&self.columns())
    }
}

fn owner_ids_schema() -> utoipa::openapi::schema::Schema {
    schemas::uuid_list_described("Only these owners (comma-separated ids)")
}

fn owner_sort() -> utoipa::openapi::schema::Schema {
    schemas::sort_schema(&["name", "kind", "email", "createdAt", "updatedAt"], "name")
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct OwnerList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    offset: i64,
    #[param(schema_with = schemas::search_schema)]
    q: Option<String>,
    #[param(required = false, schema_with = owner_sort)]
    sort: Sort,
    #[param(inline)]
    is_active: Option<QueryBool>,
    #[param(inline)]
    kind: Option<OwnerKind>,
    #[param(schema_with = owner_ids_schema)]
    id: Option<UuidList>,
}
paged!(OwnerList);

impl ListQuery for OwnerList {
    fn q(&self) -> Option<&str> {
        self.q.as_deref()
    }
    fn sort(&self) -> &Sort {
        &self.sort
    }
    fn filter(&self, w: &mut Where<'_>) {
        bool_filter(w, "is_active", self.is_active);
        if let Some(k) = self.kind {
            w.and().push("kind = ").push_bind(kind_str(k));
        }
        if let Some(ids) = &self.id {
            w.and().push("id = ANY(").push_bind(ids.0.clone()).push(")");
        }
    }
}

pub struct Owners;

impl Resource for Owners {
    type Dto = Owner;
    type Create = OwnerCreate;
    type Update = OwnerUpdate;
    type List = OwnerList;
    const TABLE: &'static str = "owners";
    const LABEL: &'static str = "Owner";
    const BASE_PATH: &'static str = "/api/v1/owners";
    const TAG: &'static str = "Owners";
    const SINGULAR: &'static str = "owner";
    const PLURAL: &'static str = "owners";
    const COLUMNS: &'static str = "id, kind, name, email, external_ref, is_active, created_at, updated_at";
    const SEARCH_COLUMNS: &'static [&'static str] = &["name", "email", "external_ref"];
    const USAGE: &'static [Usage] = &[
        Usage {
            kind: "configurationItems",
            label: "configuration items",
            sql: "SELECT count(*) FROM configuration_items WHERE owner_id = $1 AND deleted_at IS NULL",
            blocking: true,
        },
        Usage {
            kind: "deletedConfigurationItems",
            label: "deleted configuration items (kept for history)",
            sql: "SELECT count(*) FROM configuration_items WHERE owner_id = $1 AND deleted_at IS NOT NULL",
            blocking: true,
        },
    ];
    fn id(row: &Owner) -> Uuid {
        row.id
    }
}

// ===========================================================================
// Admin-defined lookup lists and their values
// ===========================================================================

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LookupList {
    pub id: Uuid,
    pub key: String,
    pub name: String,
    #[schema(required = true)]
    pub description: Option<String>,
    pub sort_order: i32,
    pub is_active: bool,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LookupListCreate {
    #[schema(schema_with = key_schema)]
    key: String,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    name: String,
    #[schema(schema_with = description_schema)]
    #[serde(default)]
    description: Option<String>,
    #[schema(schema_with = sort_order_schema)]
    sort_order: Option<i32>,
    #[schema(nullable = false)]
    is_active: Option<bool>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LookupListUpdate {
    #[schema(schema_with = key_schema)]
    key: Option<String>,
    #[schema(schema_with = name_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    name: Option<String>,
    #[schema(schema_with = description_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    description: Option<Option<String>>,
    #[schema(schema_with = sort_order_schema)]
    sort_order: Option<i32>,
    #[schema(nullable = false)]
    is_active: Option<bool>,
}

impl Writable for LookupListCreate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("key", Some(self.key.clone()))
            .opt("name", Some(self.name.clone()))
            .opt("description", self.description.clone().map(Some))
            .opt("sort_order", self.sort_order)
            .opt("is_active", self.is_active);
        c
    }
}
impl Check for LookupListCreate {}

impl Writable for LookupListUpdate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("key", self.key.clone())
            .opt("name", self.name.clone())
            .opt("description", self.description.clone())
            .opt("sort_order", self.sort_order)
            .opt("is_active", self.is_active);
        c
    }
}
impl Check for LookupListUpdate {
    fn check(&self) -> Vec<FieldError> {
        non_empty(&self.columns())
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct LookupListList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    offset: i64,
    #[param(schema_with = schemas::search_schema)]
    q: Option<String>,
    #[param(required = false, schema_with = lookup_sort_by_order)]
    sort: Sort,
    #[param(inline)]
    is_active: Option<QueryBool>,
}
paged!(LookupListList);

impl ListQuery for LookupListList {
    fn q(&self) -> Option<&str> {
        self.q.as_deref()
    }
    fn sort(&self) -> &Sort {
        &self.sort
    }
    fn filter(&self, w: &mut Where<'_>) {
        bool_filter(w, "is_active", self.is_active);
    }
}

pub struct LookupLists;

impl Resource for LookupLists {
    type Dto = LookupList;
    type Create = LookupListCreate;
    type Update = LookupListUpdate;
    type List = LookupListList;
    const TABLE: &'static str = "lookup_lists";
    const LABEL: &'static str = "Lookup list";
    const BASE_PATH: &'static str = "/api/v1/lookup-lists";
    const TAG: &'static str = "Lookup lists";
    const SINGULAR: &'static str = "lookupList";
    const PLURAL: &'static str = "lookupLists";
    const COLUMNS: &'static str = "id, key, name, description, sort_order, is_active, created_at, updated_at";
    const SEARCH_COLUMNS: &'static [&'static str] = &["key", "name", "description"];
    const DELETE_DESCRIPTION: &'static str = "Hard delete of the list and its values, allowed only while no attribute definition uses the list (409 IN_USE otherwise). Retire it with `PATCH {\"isActive\": false}` instead.";
    const USAGE: &'static [Usage] = &[
        Usage {
            kind: "attributeDefinitions",
            label: "attribute definitions",
            sql: "SELECT count(*) FROM ci_attribute_definitions WHERE lookup_list_id = $1",
            blocking: true,
        },
        Usage {
            kind: "values",
            label: "list values (deleted with the list)",
            sql: "SELECT count(*) FROM lookup_list_values WHERE list_id = $1",
            blocking: false,
        },
    ];
    fn id(row: &LookupList) -> Uuid {
        row.id
    }
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LookupListValue {
    pub id: Uuid,
    pub list_id: Uuid,
    pub key: String,
    pub name: String,
    #[schema(required = true)]
    pub description: Option<String>,
    /// Badge colour in the UI
    #[schema(required = true)]
    pub color: Option<String>,
    pub sort_order: i32,
    /// Retired values stay on the CIs that hold them but cannot be chosen again
    pub is_active: bool,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LookupListValueCreate {
    list_id: Uuid,
    #[schema(schema_with = key_schema)]
    key: String,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    name: String,
    #[schema(schema_with = description_schema)]
    #[serde(default)]
    description: Option<String>,
    #[schema(schema_with = schemas::nullable_color_schema)]
    #[serde(default)]
    color: Option<String>,
    #[schema(schema_with = sort_order_schema)]
    sort_order: Option<i32>,
    #[schema(nullable = false)]
    is_active: Option<bool>,
}

// `listId` is immutable: CIs store the value by id.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LookupListValueUpdate {
    #[schema(schema_with = key_schema)]
    key: Option<String>,
    #[schema(schema_with = name_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    name: Option<String>,
    #[schema(schema_with = description_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    description: Option<Option<String>>,
    #[schema(schema_with = schemas::nullable_color_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    color: Option<Option<String>>,
    #[schema(schema_with = sort_order_schema)]
    sort_order: Option<i32>,
    #[schema(nullable = false)]
    is_active: Option<bool>,
}

impl Writable for LookupListValueCreate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("list_id", Some(self.list_id))
            .opt("key", Some(self.key.clone()))
            .opt("name", Some(self.name.clone()))
            .opt("description", self.description.clone().map(Some))
            .opt("color", self.color.clone().map(Some))
            .opt("sort_order", self.sort_order)
            .opt("is_active", self.is_active);
        c
    }
}
impl Check for LookupListValueCreate {}

impl Writable for LookupListValueUpdate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("key", self.key.clone())
            .opt("name", self.name.clone())
            .opt("description", self.description.clone())
            .opt("color", self.color.clone())
            .opt("sort_order", self.sort_order)
            .opt("is_active", self.is_active);
        c
    }
}
impl Check for LookupListValueUpdate {
    fn check(&self) -> Vec<FieldError> {
        non_empty(&self.columns())
    }
}

fn list_ids_schema() -> utoipa::openapi::schema::Schema {
    schemas::uuid_list_described("Values of these lists (comma-separated ids)")
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct LookupListValueList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    offset: i64,
    #[param(schema_with = schemas::search_schema)]
    q: Option<String>,
    #[param(required = false, schema_with = lookup_sort_by_order)]
    sort: Sort,
    #[param(schema_with = list_ids_schema)]
    list_id: Option<UuidList>,
    #[param(inline)]
    is_active: Option<QueryBool>,
}
paged!(LookupListValueList);

impl ListQuery for LookupListValueList {
    fn q(&self) -> Option<&str> {
        self.q.as_deref()
    }
    fn sort(&self) -> &Sort {
        &self.sort
    }
    fn filter(&self, w: &mut Where<'_>) {
        if let Some(ids) = &self.list_id {
            w.and().push("list_id = ANY(").push_bind(ids.0.clone()).push(")");
        }
        bool_filter(w, "is_active", self.is_active);
    }
}

pub struct LookupListValues;

impl Resource for LookupListValues {
    type Dto = LookupListValue;
    type Create = LookupListValueCreate;
    type Update = LookupListValueUpdate;
    type List = LookupListValueList;
    const TABLE: &'static str = "lookup_list_values";
    const LABEL: &'static str = "Lookup list value";
    const BASE_PATH: &'static str = "/api/v1/lookup-list-values";
    const TAG: &'static str = "Lookup lists";
    const SINGULAR: &'static str = "lookupListValue";
    const PLURAL: &'static str = "lookupListValues";
    const COLUMNS: &'static str =
        "id, list_id, key, name, description, color, sort_order, is_active, created_at, updated_at";
    const SEARCH_COLUMNS: &'static [&'static str] = &["key", "name", "description"];
    const USAGE: &'static [Usage] = &[
        Usage {
            kind: "attributeValues",
            label: "attribute values on configuration items",
            sql: "SELECT count(*) FROM ci_attribute_values WHERE value_lookup_id = $1",
            blocking: true,
        },
        Usage {
            kind: "attributeDefaults",
            label: "attribute definitions that use it as default",
            sql: "SELECT count(*) FROM ci_attribute_definitions WHERE data_type = 'lookup' AND default_value = to_jsonb($1::text)",
            blocking: true,
        },
    ];
    fn id(row: &LookupListValue) -> Uuid {
        row.id
    }
}

pub fn routes() -> Vec<Route> {
    let mut r = simple::routes::<Statuses>();
    r.extend(simple::routes::<Environments>());
    r.extend(simple::routes::<Locations>());
    r.extend(simple::routes::<Owners>());
    r.extend(simple::routes::<LookupLists>());
    r.extend(simple::routes::<LookupListValues>());
    r
}
