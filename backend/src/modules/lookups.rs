//! Lookup tables: statuses, environments, locations, owners, plus lookup lists
//! an administrator defines (values for "lookup" attributes). Rows are renamed
//! and retired (isActive=false) rather than deleted once CIs reference them.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::classes::{AttributeDefinition, AttributeDefinitions};
use super::simple_resource::{self as simple, BoxFuture, ListQuery, Resource, Usage, Writable, bool_filter, non_empty};
use crate::api::context::RequestContext;
use crate::api::route::{Check, Route};
use crate::api::schemas::{
    self, IdOrNone, OwnerKind, QueryBool, Sort, UuidList, description_schema, key_schema, name_schema,
    nullable_uuid_schema, sort_order_schema, trimmed, ts,
};
use crate::data::crud::{self, AuditAction, AuditEntry, ColumnSet, Where};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
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
    pub(crate) fn as_str(self) -> &'static str {
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

pub(crate) fn address_schema() -> utoipa::openapi::schema::Schema {
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

pub(crate) fn external_ref_schema() -> utoipa::openapi::schema::Schema {
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
    /// The list this one depends on (e.g. "Model" depends on "Manufacturer"): each value names its parent value
    #[schema(required = true)]
    pub parent_list_id: Option<Uuid>,
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
    #[schema(schema_with = nullable_uuid_schema)]
    #[serde(default)]
    parent_list_id: Option<Uuid>,
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
    #[schema(schema_with = nullable_uuid_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    parent_list_id: Option<Option<Uuid>>,
}

impl Writable for LookupListCreate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("key", Some(self.key.clone()))
            .opt("name", Some(self.name.clone()))
            .opt("description", self.description.clone().map(Some))
            .opt("sort_order", self.sort_order)
            .opt("is_active", self.is_active)
            .opt("parent_list_id", self.parent_list_id.map(Some));
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
            .opt("is_active", self.is_active)
            .opt("parent_list_id", self.parent_list_id);
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
    #[param(schema_with = list_parent_schema)]
    parent_list_id: Option<IdOrNone>,
}
paged!(LookupListList);

fn list_parent_schema() -> utoipa::openapi::schema::Schema {
    schemas::id_or_none_schema("Lists that depend on this list; \"none\" for lists without a parent list")
}

impl ListQuery for LookupListList {
    fn q(&self) -> Option<&str> {
        self.q.as_deref()
    }
    fn sort(&self) -> &Sort {
        &self.sort
    }
    fn filter(&self, w: &mut Where<'_>) {
        bool_filter(w, "is_active", self.is_active);
        id_or_none_filter(w, "parent_list_id", self.parent_list_id);
    }
}

fn id_or_none_filter(w: &mut Where<'_>, column: &str, value: Option<IdOrNone>) {
    match value {
        Some(IdOrNone::None) => w.and_sql(&format!("{column} IS NULL")),
        Some(IdOrNone::Id(id)) => {
            w.and().push(column).push(" = ").push_bind(id);
        }
        None => {}
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
    const COLUMNS: &'static str =
        "id, key, name, description, sort_order, is_active, created_at, updated_at, parent_list_id";
    const SEARCH_COLUMNS: &'static [&'static str] = &["key", "name", "description"];
    const DELETE_DESCRIPTION: &'static str = "Hard delete of the list and its values, allowed only while no attribute definition uses the list and no other list depends on it (409 IN_USE otherwise). Retire it with `PATCH {\"isActive\": false}` instead.";
    const UPDATE_DESCRIPTION: &'static str = "`parentListId` makes the list depend on another list (no cycles). Setting, changing or clearing it unassigns every value's `parentValueId` and every bound field's `parentAttributeId` in the same transaction (each change audited): reassign them afterwards with `PATCH /api/v1/lookup-list-values/{id}` and `PATCH /api/v1/attribute-definitions/{id}`. Until a value is assigned, it cannot be chosen on a field that has a parent field.";
    const USAGE: &'static [Usage] = &[
        Usage {
            kind: "attributeDefinitions",
            label: "attribute definitions",
            sql: "SELECT count(*) FROM ci_attribute_definitions WHERE lookup_list_id = $1",
            blocking: true,
        },
        Usage {
            kind: "childLists",
            label: "lists that depend on it",
            sql: "SELECT count(*) FROM lookup_lists WHERE parent_list_id = $1",
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

    fn after_write<'a>(
        conn: &'a mut PgConnection,
        ctx: &'a RequestContext,
        row: &'a LookupList,
        previous: Option<&'a LookupList>,
    ) -> BoxFuture<'a, Result<(), AppError>> {
        Box::pin(async move {
            match previous {
                Some(p) if p.parent_list_id != row.parent_list_id => unassign_parents(conn, ctx, row.id).await,
                _ => Ok(()),
            }
        })
    }
}

/// A list got another parent list (or none): its values' parent values and
/// its fields' parent fields point into the old one, so they are cleared.
async fn unassign_parents(conn: &mut PgConnection, ctx: &RequestContext, list_id: Uuid) -> Result<(), AppError> {
    let cols = LookupListValues::COLUMNS;
    let before: Vec<LookupListValue> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {cols} FROM lookup_list_values WHERE list_id = $1 AND parent_value_id IS NOT NULL ORDER BY id FOR UPDATE"
    )))
    .bind(list_id)
    .fetch_all(&mut *conn)
    .await?;
    let after: Vec<LookupListValue> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "UPDATE lookup_list_values SET parent_value_id = NULL WHERE list_id = $1 AND parent_value_id IS NOT NULL \
         RETURNING {cols}"
    )))
    .bind(list_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut entries = audit_updates(LookupListValues::TABLE, before, after, |v| v.id);

    let cols = AttributeDefinitions::COLUMNS;
    let before: Vec<AttributeDefinition> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {cols} FROM ci_attribute_definitions WHERE lookup_list_id = $1 AND parent_attribute_id IS NOT NULL \
         ORDER BY id FOR UPDATE"
    )))
    .bind(list_id)
    .fetch_all(&mut *conn)
    .await?;
    let after: Vec<AttributeDefinition> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "UPDATE ci_attribute_definitions SET parent_attribute_id = NULL \
         WHERE lookup_list_id = $1 AND parent_attribute_id IS NOT NULL RETURNING {cols}"
    )))
    .bind(list_id)
    .fetch_all(&mut *conn)
    .await?;
    entries.extend(audit_updates(AttributeDefinitions::TABLE, before, after, |a| a.id));
    crud::write_audit(conn, ctx, entries).await?;
    Ok(())
}

/// One update audit row per changed row, `before` and `after` matched by id.
fn audit_updates<T: Serialize>(
    table: &'static str,
    before: Vec<T>,
    after: Vec<T>,
    id: impl Fn(&T) -> Uuid,
) -> Vec<AuditEntry> {
    let mut old: HashMap<Uuid, T> = before.into_iter().map(|r| (id(&r), r)).collect();
    let mut entries: Vec<AuditEntry> = after
        .into_iter()
        .map(|new| {
            let entity_id = id(&new);
            AuditEntry {
                action: AuditAction::Update,
                entity_type: table,
                entity_id,
                old_value: old.remove(&entity_id).map(|o| crud::json(&o)),
                new_value: Some(crud::json(&new)),
            }
        })
        .collect();
    entries.sort_by_key(|e| e.entity_id);
    entries
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
    /// The value of the parent list this value belongs to (lists with a parent list only). Null on a value
    /// left unassigned when its list got another parent list: it cannot be chosen until it is assigned again.
    #[schema(required = true)]
    pub parent_value_id: Option<Uuid>,
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
    /// Required when the list has a parent list: a value of that list
    #[schema(schema_with = nullable_uuid_schema)]
    #[serde(default)]
    parent_value_id: Option<Uuid>,
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
    /// Moves the value under another value of the parent list; it cannot be cleared once assigned
    #[schema(schema_with = nullable_uuid_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    parent_value_id: Option<Option<Uuid>>,
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
            .opt("is_active", self.is_active)
            .opt("parent_value_id", self.parent_value_id.map(Some));
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
            .opt("is_active", self.is_active)
            .opt("parent_value_id", self.parent_value_id);
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
    #[param(schema_with = value_parent_schema)]
    parent_value_id: Option<IdOrNone>,
}
paged!(LookupListValueList);

fn value_parent_schema() -> utoipa::openapi::schema::Schema {
    schemas::id_or_none_schema(
        "Values that belong to this value of the parent list (what a dependent dropdown offers once its parent \
         is chosen); \"none\" for values without a parent value",
    )
}

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
        id_or_none_filter(w, "parent_value_id", self.parent_value_id);
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
        "id, list_id, key, name, description, color, sort_order, is_active, created_at, updated_at, parent_value_id";
    const SEARCH_COLUMNS: &'static [&'static str] = &["key", "name", "description"];
    const DELETE_DESCRIPTION: &'static str = "Hard delete, allowed only while no CI stores the value, no attribute uses it as default and no value of a dependent list belongs to it (409 IN_USE otherwise; the details name what still refers to it). Retire it with `PATCH {\"isActive\": false}` instead.";
    const UPDATE_DESCRIPTION: &'static str = "`isActive: false` on a value that other values belong to retires those too (and theirs, down the chain), each change audited. It is refused (409 IN_USE) while CIs store one of those active dependent values: the parent could then no longer be chosen while its children stay on the CIs. Reactivating a value does not reactivate its dependents, and a value whose parent value is retired cannot be reactivated or created.";
    const WRITE_ERRORS: &'static [ErrorCode] = &[ErrorCode::InUse];
    const USAGE: &'static [Usage] = &[
        Usage {
            kind: "attributeValues",
            label: "attribute values on configuration items",
            sql: "SELECT cmdb.lookup_value_count($1)",
            blocking: true,
        },
        Usage {
            kind: "attributeDefaults",
            label: "attribute definitions that use it as default",
            sql: "SELECT count(*) FROM ci_attribute_definitions WHERE data_type = 'lookup' AND default_value = to_jsonb($1::text)",
            blocking: true,
        },
        Usage {
            kind: "childValues",
            label: "values of dependent lists that belong to it",
            sql: "SELECT count(*) FROM lookup_list_values WHERE parent_value_id = $1",
            blocking: true,
        },
    ];
    fn id(row: &LookupListValue) -> Uuid {
        row.id
    }

    fn after_write<'a>(
        conn: &'a mut PgConnection,
        ctx: &'a RequestContext,
        row: &'a LookupListValue,
        previous: Option<&'a LookupListValue>,
    ) -> BoxFuture<'a, Result<(), AppError>> {
        Box::pin(async move {
            let (was_active, old_parent) = previous.map_or((false, None), |p| (p.is_active, p.parent_value_id));
            if let Some(parent) = row.parent_value_id
                && row.is_active
                && (!was_active || old_parent != row.parent_value_id)
            {
                let active: Option<bool> = sqlx::query_scalar("SELECT is_active FROM lookup_list_values WHERE id = $1")
                    .bind(parent)
                    .fetch_optional(&mut *conn)
                    .await?;
                if active == Some(false) {
                    let field = if old_parent != row.parent_value_id { "parentValueId" } else { "isActive" };
                    return Err(AppError::field(
                        field,
                        "The parent value is retired; reactivate it first or choose another one",
                        "parent_value_inactive",
                    ));
                }
            }
            if was_active && !row.is_active {
                retire_dependents(conn, ctx, row.id).await?;
            }
            Ok(())
        })
    }
}

/// Retires the active values that belong to `value_id`, down the chain of
/// dependent lists, unless CIs store one of them (409 IN_USE).
async fn retire_dependents(conn: &mut PgConnection, ctx: &RequestContext, value_id: Uuid) -> Result<(), AppError> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "WITH RECURSIVE down AS (
           SELECT id FROM lookup_list_values WHERE parent_value_id = $1
           UNION
           SELECT v.id FROM lookup_list_values v JOIN down ON v.parent_value_id = down.id
         )
         SELECT v.id FROM down JOIN lookup_list_values v ON v.id = down.id WHERE v.is_active ORDER BY v.id",
    )
    .bind(value_id)
    .fetch_all(&mut *conn)
    .await?;
    if ids.is_empty() {
        return Ok(());
    }
    let used: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT l.key, v.key, n FROM lookup_list_values v
         JOIN lookup_lists l ON l.id = v.list_id
         CROSS JOIN LATERAL cmdb.lookup_value_count(v.id) AS n
         WHERE v.id = ANY($1) AND n > 0
         ORDER BY l.key, v.key",
    )
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await?;
    if !used.is_empty() {
        let summary =
            used.iter().map(|(l, v, n)| format!("{l}.{v} ({n} configuration items)")).collect::<Vec<_>>().join(", ");
        return Err(AppError::new(
            ErrorCode::InUse,
            format!(
                "Values that belong to this value are still used by configuration items: {summary}. Change those \
                 configuration items or retire the dependent values first."
            ),
        )
        .with_details(
            used.into_iter()
                .map(|(l, v, n)| FieldError {
                    location: FieldLocation::Body,
                    field: "isActive".into(),
                    message: format!("{l}.{v} is stored on {n} configuration items"),
                    code: "dependent_value_in_use".into(),
                })
                .collect(),
        ));
    }
    let cols = LookupListValues::COLUMNS;
    let before: Vec<LookupListValue> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {cols} FROM lookup_list_values WHERE id = ANY($1) ORDER BY id FOR UPDATE"
    )))
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await?;
    let after: Vec<LookupListValue> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "UPDATE lookup_list_values SET is_active = false WHERE id = ANY($1) RETURNING {cols}"
    )))
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await?;
    crud::write_audit(conn, ctx, audit_updates(LookupListValues::TABLE, before, after, |v| v.id)).await?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use serde::de::DeserializeOwned;
    use serde_json::{Value, json};
    use sqlx::PgPool;

    use super::*;
    use crate::db::scratch;
    use crate::modules::items::schemas::{CreateItemBody, UpdateItemBody};
    use crate::modules::items::service as items;

    fn body<T: DeserializeOwned>(v: Value) -> T {
        serde_json::from_value(v).unwrap()
    }

    /// (field, code) of each detail.
    fn problems(err: &AppError) -> Vec<(&str, &str)> {
        err.details.iter().flatten().map(|d| (d.field.as_str(), d.code.as_str())).collect()
    }

    async fn id_of(pool: &PgPool, table: &str, key: &str) -> Uuid {
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT id FROM {table} WHERE key = $1")))
            .bind(key)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn audit_updates(pool: &PgPool, entity_id: Uuid) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE entity_id = $1 AND action = 'update'")
            .bind(entity_id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// SHAA-268: dependent lists, values and fields; CI writes; retiring and
    /// deleting parent values; a list getting another parent list.
    #[tokio::test]
    async fn dependent_lookup_lists() {
        let Some(db) = scratch::database("dependent_lookup_lists").await else { return };
        let pool = &db.pool;
        crate::seed::install_template(pool, "it_infrastructure").await.unwrap();
        let ctx = RequestContext::system("test", "test");
        let (server, in_service) =
            (id_of(pool, "ci_classes", "server").await, id_of(pool, "statuses", "in_service").await);

        // Lists: a parent list, no self-parent, no cycle.
        let maker = simple::create::<LookupLists>(pool, &ctx, &body(json!({ "key": "maker", "name": "Manufacturer" })))
            .await
            .unwrap();
        let model = simple::create::<LookupLists>(
            pool,
            &ctx,
            &body(json!({ "key": "model", "name": "Model", "parentListId": maker.id })),
        )
        .await
        .unwrap();
        assert_eq!(model.parent_list_id, Some(maker.id));
        let err = simple::update::<LookupLists>(pool, &ctx, maker.id, &body(json!({ "parentListId": model.id })))
            .await
            .unwrap_err();
        assert_eq!(problems(&err), [("parentListId", "lookup_lists_no_cycle")]);
        let err = simple::update::<LookupLists>(pool, &ctx, model.id, &body(json!({ "parentListId": model.id })))
            .await
            .unwrap_err();
        // The cycle check runs first: a list is its own ancestor.
        assert_eq!(problems(&err), [("parentListId", "lookup_lists_no_cycle")]);
        let err = simple::remove::<LookupLists>(pool, &ctx, maker.id).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::InUse);

        // Values: a child value needs a parent value from the parent list.
        let value = |list: Uuid, key: &str, parent: Option<Uuid>| {
            body::<LookupListValueCreate>(json!({ "listId": list, "key": key, "name": key, "parentValueId": parent }))
        };
        let ctx_ref = &ctx;
        let create_value =
            |b: LookupListValueCreate| async move { simple::create::<LookupListValues>(pool, ctx_ref, &b).await };
        let cisco = create_value(value(maker.id, "cisco", None)).await.unwrap();
        let hp = create_value(value(maker.id, "hp", None)).await.unwrap();
        let c9300 = create_value(value(model.id, "c9300", Some(cisco.id))).await.unwrap();
        let dl380 = create_value(value(model.id, "dl380", Some(hp.id))).await.unwrap();
        let err = create_value(value(model.id, "orphan", None)).await.unwrap_err();
        assert_eq!(problems(&err), [("parentValueId", "lookup_list_values_parent_required")]);
        let err = create_value(value(model.id, "nested", Some(c9300.id))).await.unwrap_err();
        assert_eq!(problems(&err), [("parentValueId", "lookup_list_values_parent_list")]);
        let err = create_value(value(maker.id, "top", Some(cisco.id))).await.unwrap_err();
        assert_eq!(problems(&err), [("parentValueId", "lookup_list_values_parent_list")]);
        let err = simple::update::<LookupListValues>(pool, &ctx, c9300.id, &body(json!({ "parentValueId": null })))
            .await
            .unwrap_err();
        assert_eq!(problems(&err), [("parentValueId", "lookup_list_values_parent_required")]);

        // What a dependent dropdown offers once its parent is chosen.
        let q = |parent: &str| {
            body::<LookupListValueList>(json!({ "limit": 50, "offset": 0, "sort": "key", "parentValueId": parent }))
        };
        let page = simple::list::<LookupListValues>(pool, &q(&cisco.id.to_string())).await.unwrap();
        assert_eq!(page.data.iter().map(|v| v.key.as_str()).collect::<Vec<_>>(), ["c9300"]);
        let page = simple::list::<LookupListValues>(pool, &q("none")).await.unwrap();
        assert_eq!(page.data.iter().map(|v| v.key.as_str()).collect::<Vec<_>>(), ["cisco", "hp"]);

        // Fields: the parent field is a lookup field on the parent list.
        let field = |key: &str, list: Uuid, parent: Option<Uuid>| {
            body::<crate::modules::classes::AttributeDefinitionCreate>(json!({ "classId": server, "key": key,
                "label": key, "dataType": "lookup", "lookupListId": list, "parentAttributeId": parent }))
        };
        let maker_field =
            simple::create::<AttributeDefinitions>(pool, &ctx, &field("vendor_name", maker.id, None)).await.unwrap();
        let err =
            simple::create::<AttributeDefinitions>(pool, &ctx, &field("vendor_name2", maker.id, Some(maker_field.id)))
                .await
                .unwrap_err();
        assert_eq!(problems(&err), [("parentAttributeId", "ci_attribute_definitions_parent_attribute")]);
        let model_field =
            simple::create::<AttributeDefinitions>(pool, &ctx, &field("vendor_model", model.id, Some(maker_field.id)))
                .await
                .unwrap();
        assert_eq!(model_field.parent_attribute_id, Some(maker_field.id));

        // CIs: the model must belong to the CI's manufacturer.
        let new_ci = |attributes: Value| {
            body::<CreateItemBody>(json!({ "name": "srv", "classId": server, "statusId": in_service,
                "attributes": attributes }))
        };
        let err = items::create(pool, &ctx, &new_ci(json!({ "vendor_model": c9300.id }))).await.unwrap_err();
        assert_eq!(problems(&err), [("attributes.vendor_model", "lookup_parent_missing")]);
        let err = items::create(pool, &ctx, &new_ci(json!({ "vendor_name": hp.id, "vendor_model": c9300.id })))
            .await
            .unwrap_err();
        assert_eq!(problems(&err), [("attributes.vendor_model", "lookup_parent_mismatch")]);
        let ci = items::create(pool, &ctx, &new_ci(json!({ "vendor_name": cisco.id, "vendor_model": c9300.id })))
            .await
            .unwrap();
        let ci = ci.summary.id;
        let edit = |v: Value| body::<UpdateItemBody>(v);
        let err =
            items::update(pool, &ctx, ci, &edit(json!({ "attributes": { "vendor_name": hp.id } }))).await.unwrap_err();
        assert_eq!(problems(&err), [("attributes.vendor_model", "lookup_parent_mismatch")]);
        let err =
            items::update(pool, &ctx, ci, &edit(json!({ "attributes": { "vendor_name": null } }))).await.unwrap_err();
        assert_eq!(problems(&err), [("attributes.vendor_model", "lookup_parent_missing")]);
        items::update(
            pool,
            &ctx,
            ci,
            &edit(json!({ "attributes": { "vendor_name": hp.id, "vendor_model": dl380.id } })),
        )
        .await
        .unwrap();
        items::update(pool, &ctx, ci, &edit(json!({ "name": "srv-renamed" }))).await.unwrap();

        // Retiring a parent value: refused while CIs hold a dependent value, otherwise cascades.
        let retire = |active: bool| body::<LookupListValueUpdate>(json!({ "isActive": active }));
        let err = simple::update::<LookupListValues>(pool, &ctx, hp.id, &retire(false)).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::InUse);
        assert_eq!(problems(&err), [("isActive", "dependent_value_in_use")]);
        assert!(simple::get::<LookupListValues>(pool, hp.id).await.unwrap().is_active);
        simple::update::<LookupListValues>(pool, &ctx, cisco.id, &retire(false)).await.unwrap();
        assert!(!simple::get::<LookupListValues>(pool, c9300.id).await.unwrap().is_active);
        assert_eq!(audit_updates(pool, c9300.id).await, 1);
        let err = simple::update::<LookupListValues>(pool, &ctx, c9300.id, &retire(true)).await.unwrap_err();
        assert_eq!(problems(&err), [("isActive", "parent_value_inactive")]);
        let err = create_value(value(model.id, "c9500", Some(cisco.id))).await.unwrap_err();
        assert_eq!(problems(&err), [("parentValueId", "parent_value_inactive")]);

        // Deleting a value other values belong to is refused.
        let err = simple::remove::<LookupListValues>(pool, &ctx, hp.id).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::InUse);
        assert!(problems(&err).contains(&("childValues", "in_use")));

        // Another parent list unassigns the values and the fields, audited.
        let vendor = simple::create::<LookupLists>(pool, &ctx, &body(json!({ "key": "vendor", "name": "Vendor" })))
            .await
            .unwrap();
        let before = audit_updates(pool, dl380.id).await;
        simple::update::<LookupLists>(pool, &ctx, model.id, &body(json!({ "parentListId": vendor.id }))).await.unwrap();
        assert_eq!(simple::get::<LookupListValues>(pool, dl380.id).await.unwrap().parent_value_id, None);
        assert_eq!(simple::get::<AttributeDefinitions>(pool, model_field.id).await.unwrap().parent_attribute_id, None);
        assert_eq!(audit_updates(pool, dl380.id).await, before + 1);
        // An unassigned value can be assigned to a value of the new parent list.
        let acme = create_value(value(vendor.id, "acme", None)).await.unwrap();
        simple::update::<LookupListValues>(pool, &ctx, dl380.id, &body(json!({ "parentValueId": acme.id })))
            .await
            .unwrap();
        db.drop().await;
    }
}
