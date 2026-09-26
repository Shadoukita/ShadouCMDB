//! Types and schema fragments shared by the API modules: pagination, sorting,
//! id lists, compact references, and the scalar formats of the SHAA-3 contract.

use std::borrow::Cow;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use utoipa::openapi::schema::{ArrayBuilder, KnownFormat, ObjectBuilder, Schema, SchemaFormat, Type};
use utoipa::openapi::{RefOr, schema::AnyOfBuilder};
use utoipa::{PartialSchema, ToSchema};
use uuid::Uuid;

use super::validate;

/// Machine keys are lower_snake_case so they are safe in URLs, JSON and code.
pub const KEY_PATTERN: &str = "^[a-z][a-z0-9_]{0,62}$";
pub const HOSTNAME_PATTERN: &str = "^[A-Za-z0-9]([A-Za-z0-9._-]{0,252})$";
/// At least one non-whitespace character; values are trimmed when read.
pub const NOT_BLANK_PATTERN: &str = "\\S";
/// "#rrggbb"
pub const COLOR_PATTERN: &str = "^#[0-9a-fA-F]{6}$";
pub const USERNAME_PATTERN: &str = "^[A-Za-z0-9][A-Za-z0-9._@-]{0,63}$";

// ---------------------------------------------------------------------------
// Timestamps: ISO 8601 in UTC with milliseconds, e.g. 2026-09-26T15:34:06.645Z
// ---------------------------------------------------------------------------

pub fn iso(t: &DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub mod ts {
    use super::*;
    pub fn serialize<S: Serializer>(t: &DateTime<Utc>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&iso(t))
    }
}

pub mod ts_opt {
    use super::*;
    pub fn serialize<S: Serializer>(t: &Option<DateTime<Utc>>, s: S) -> Result<S::Ok, S::Error> {
        match t {
            Some(t) => s.serialize_str(&iso(t)),
            None => s.serialize_none(),
        }
    }
}

// ---------------------------------------------------------------------------
// Request scalars
// ---------------------------------------------------------------------------

/// Deserialise a string and trim it (the schema's `\S` pattern has already
/// rejected blank values).
pub fn trimmed<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(String::deserialize(d)?.trim().to_owned())
}

pub fn trimmed_opt<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(Option::<String>::deserialize(d)?.map(|s| s.trim().to_owned()))
}

/// PATCH fields: absent (`None`), explicit null (`Some(None)`) or a value.
pub fn patch<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Some(Option::<T>::deserialize(d)?))
}

pub fn patch_trimmed<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Ok(Some(Option::<String>::deserialize(d)?.map(|s| s.trim().to_owned())))
}

fn string() -> ObjectBuilder {
    ObjectBuilder::new().schema_type(Type::String)
}

fn nullable(schema: impl Into<RefOr<Schema>>) -> Schema {
    AnyOfBuilder::new().item(schema).item(ObjectBuilder::new().schema_type(Type::Null)).into()
}

pub fn key_schema() -> Schema {
    string().pattern(Some(KEY_PATTERN)).description(Some("Stable machine key, lower_snake_case")).into()
}

fn name_builder() -> ObjectBuilder {
    string().min_length(Some(1)).max_length(Some(200)).pattern(Some(NOT_BLANK_PATTERN))
}

pub fn name_schema() -> Schema {
    name_builder().into()
}

pub fn description_schema() -> Schema {
    nullable(string().max_length(Some(4000)))
}

pub fn uuid_builder() -> ObjectBuilder {
    string().format(Some(SchemaFormat::KnownFormat(KnownFormat::Uuid)))
}

pub fn nullable_uuid_schema() -> Schema {
    nullable(uuid_builder())
}

pub fn sort_order_schema() -> Schema {
    ObjectBuilder::new().schema_type(Type::Integer).minimum(Some(-1_000_000)).maximum(Some(1_000_000)).into()
}

fn ip_union() -> Schema {
    AnyOfBuilder::new()
        .item(string().format(Some(SchemaFormat::KnownFormat(KnownFormat::Ipv4))))
        .item(string().format(Some(SchemaFormat::KnownFormat(KnownFormat::Ipv6))))
        .into()
}

/// IPv4 or IPv6 address, or null.
pub fn nullable_ip_schema() -> Schema {
    let mut s = nullable(ip_union());
    if let Schema::AnyOf(a) = &mut s {
        a.description = Some("IPv4 or IPv6 address".into());
    }
    s
}

pub fn cidr_schema() -> Schema {
    AnyOfBuilder::new()
        .item(string().format(Some(SchemaFormat::Custom("cidrv4".into()))))
        .item(string().format(Some(SchemaFormat::Custom("cidrv6".into()))))
        .into()
}

pub fn nullable_hostname_schema() -> Schema {
    nullable(string().pattern(Some(HOSTNAME_PATTERN)))
}

pub fn nullable_trimmed_schema(max: usize) -> Schema {
    nullable(string().min_length(Some(1)).max_length(Some(max)).pattern(Some(NOT_BLANK_PATTERN)))
}

/// "#rrggbb" or null.
pub fn nullable_color_schema() -> Schema {
    nullable(string().pattern(Some(COLOR_PATTERN)).description(Some("Hex colour, e.g. \"#1f6feb\"")))
}

pub fn nullable_string_schema(max: usize) -> Schema {
    nullable(string().max_length(Some(max)))
}

// ---------------------------------------------------------------------------
// Query parameters
// ---------------------------------------------------------------------------

// Query-string boolean: only the literal strings "true" / "false".
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
pub enum QueryBool {
    #[serde(rename = "true")]
    True,
    #[serde(rename = "false")]
    False,
}

impl From<QueryBool> for bool {
    fn from(b: QueryBool) -> bool {
        matches!(b, QueryBool::True)
    }
}

/// Soft-deleted rows: exclude (default), include, or only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Deleted {
    Exclude,
    Include,
    Only,
}

impl Deleted {
    /// SQL predicate for a `deleted_at` column, or None for "include".
    pub fn predicate(self, column: &str) -> Option<String> {
        match self {
            Deleted::Exclude => Some(format!("{column} IS NULL")),
            Deleted::Only => Some(format!("{column} IS NOT NULL")),
            Deleted::Include => None,
        }
    }
}

pub fn deleted_schema(description: &str) -> Schema {
    string()
        .enum_values(Some(["exclude", "include", "only"]))
        .default(Some("exclude".into()))
        .description(Some(description))
        .into()
}

/// `sort=field` ascending, `sort=-field` descending.
#[derive(Debug, Clone)]
pub struct Sort {
    pub field: String,
    pub desc: bool,
}

impl<'de> Deserialize<'de> for Sort {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(match s.strip_prefix('-') {
            Some(f) => Sort { field: f.to_owned(), desc: true },
            None => Sort { field: s, desc: false },
        })
    }
}

impl Sort {
    pub fn dir(&self) -> &'static str {
        if self.desc { "DESC" } else { "ASC" }
    }
}

/// Schema for a sort parameter: only the listed fields (optionally with "-").
pub fn sort_schema(fields: &[&str], fallback: &str) -> Schema {
    let values: Vec<String> = fields.iter().flat_map(|f| [f.to_string(), format!("-{f}")]).collect();
    string()
        .enum_values(Some(values))
        .default(Some(fallback.into()))
        .description(Some(format!("Sort field; prefix with \"-\" for descending. One of: {}", fields.join(", "))))
        .into()
}

/// One or more ids, comma-separated (repeated keys are joined with commas first).
#[derive(Debug, Clone)]
pub struct UuidList(pub Vec<Uuid>);

impl<'de> Deserialize<'de> for UuidList {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let s = String::deserialize(d)?;
        let parts: Vec<&str> = s.split(',').map(str::trim).filter(|p| !p.is_empty()).collect();
        if parts.is_empty() {
            return Err(D::Error::custom("too_small|Too small: expected array to have >=1 items"));
        }
        if parts.len() > 100 {
            return Err(D::Error::custom("too_big|Too big: expected array to have <=100 items"));
        }
        let mut ids = Vec::with_capacity(parts.len());
        for p in parts {
            match validate::is_uuid(p).then(|| Uuid::parse_str(p).ok()).flatten() {
                Some(id) => ids.push(id),
                None => return Err(D::Error::custom("invalid_format|Invalid UUID")),
            }
        }
        Ok(UuidList(ids))
    }
}

pub fn uuid_list_schema() -> Schema {
    uuid_list_described("One or more ids, comma-separated")
}

pub fn uuid_list_described(description: &str) -> Schema {
    string().description(Some(description)).into()
}

/// Either the literal "none" or an id (e.g. `parentId=none` for root rows).
#[derive(Debug, Clone, Copy)]
pub enum IdOrNone {
    None,
    Id(Uuid),
}

impl<'de> Deserialize<'de> for IdOrNone {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        if s == "none" {
            return Ok(IdOrNone::None);
        }
        Uuid::parse_str(&s).map(IdOrNone::Id).map_err(|_| serde::de::Error::custom("invalid_union|Invalid input"))
    }
}

pub fn id_or_none_schema(description: &str) -> Schema {
    AnyOfBuilder::new()
        .item(string().enum_values(Some(["none"])))
        .item(uuid_builder())
        .description(Some(description))
        .into()
}

// ---------------------------------------------------------------------------
// Pagination
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PageMeta {
    pub limit: i64,
    pub offset: i64,
    /// Total rows matching the filters
    pub total: i64,
}

/// `{ data, page }`. Documented as `<Item>List`.
#[derive(Debug, Serialize)]
pub struct Page<T> {
    pub data: Vec<T>,
    pub page: PageMeta,
}

impl<T: ToSchema> PartialSchema for Page<T> {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .property("data", ArrayBuilder::new().items(RefOr::Ref(utoipa::openapi::Ref::from_schema_name(T::name()))))
            .property("page", RefOr::Ref(utoipa::openapi::Ref::from_schema_name(PageMeta::name())))
            .required("data")
            .required("page")
            .additional_properties(Some(utoipa::openapi::schema::AdditionalProperties::FreeForm(false)))
            .into()
    }
}

impl<T: ToSchema> ToSchema for Page<T> {
    /// `StatusList`, `RelationshipList`, ...; a page of `ConfigurationItem`
    /// is the `ConfigurationItemList`.
    fn name() -> Cow<'static, str> {
        Cow::Owned(format!("{}List", T::name().trim_end_matches("Summary")))
    }

    fn schemas(schemas: &mut Vec<(String, RefOr<Schema>)>) {
        schemas.push((T::name().into_owned(), T::schema()));
        T::schemas(schemas);
        schemas.push((PageMeta::name().into_owned(), PageMeta::schema()));
    }
}

/// `q` on the simple resources.
pub fn search_schema() -> Schema {
    string().min_length(Some(1)).max_length(Some(200)).description(Some("Case-insensitive substring search")).into()
}

/// Implements [`Paged`] for a query struct with `limit` and `offset` fields.
#[macro_export]
macro_rules! paged {
    ($t:ty) => {
        impl $crate::api::schemas::Paged for $t {
            fn limit(&self) -> i64 {
                self.limit
            }
            fn offset(&self) -> i64 {
                self.offset
            }
        }
    };
}

/// `limit` / `offset` as read from a list query.
pub trait Paged {
    fn limit(&self) -> i64;
    fn offset(&self) -> i64;
    fn page_meta(&self, total: i64) -> PageMeta {
        PageMeta { limit: self.limit(), offset: self.offset(), total }
    }
}

/// Escape LIKE wildcards so user input is matched literally.
pub fn like_pattern(q: &str) -> String {
    format!("%{}%", escape_like(q))
}

pub fn escape_like(q: &str) -> String {
    let mut out = String::with_capacity(q.len());
    for c in q.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

// ---------------------------------------------------------------------------
// Small embedded references (avoid N+1 lookups in the UI)
// ---------------------------------------------------------------------------

/// Compact reference to a lookup row
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct LookupRef {
    pub id: Uuid,
    pub key: String,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum OwnerKind {
    Person,
    Team,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OwnerRef {
    pub id: Uuid,
    pub name: String,
    #[schema(inline)]
    pub kind: OwnerKind,
}
