//! Request and response types of the business service endpoints (SHAA-927 §4).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use utoipa::openapi::schema::{ArrayBuilder, ObjectBuilder, Schema, Type};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::MAX_BATCH;
use crate::api::route::Check;
use crate::api::schemas::{self, PageMeta, QueryBool, Sort, ts};
use crate::api::validate;
use crate::http::error::FieldError;
use crate::modules::impact::schemas::Visibility;
use crate::modules::items::schemas::CriticalityRef;
use crate::paged;

// ---------------------------------------------------------------------------
// Shared
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum PrincipalKind {
    User,
    Group,
}

impl PrincipalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PrincipalKind::User => "user",
            PrincipalKind::Group => "group",
        }
    }
}

/// An owner of a business service: a user or a user group, by display name only.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrincipalRef {
    pub kind: PrincipalKind,
    pub id: Uuid,
    /// The user's display name, or the group's name
    pub display_name: String,
    /// false for a disabled user account; always true for a group
    pub active: bool,
}

/// The owners of a service per role, in the order they were assigned.
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ServiceOwners {
    #[schema(max_items = 10)]
    pub technical: Vec<PrincipalRef>,
    #[schema(max_items = 10)]
    pub business: Vec<PrincipalRef>,
}

/// A business service in lists.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BusinessServiceSummary {
    pub id: Uuid,
    pub ident: String,
    /// The service's label (display name)
    pub name: String,
    #[schema(required = true)]
    pub criticality: Option<CriticalityRef>,
    /// Inside its validity period
    pub active: bool,
    pub owners: ServiceOwners,
    /// Direct members the caller may view
    pub member_count: i64,
    /// Of memberCount, how many are business services
    pub service_member_count: i64,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
    /// The CI's optimistic-locking version (send it to PUT .../owners)
    pub version: i32,
}

/// The configured bounds (BUSINESS_SERVICE_* and the fixed ones).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BusinessServiceLimits {
    /// Direct members of one service, counted over the members the caller may view (BUSINESS_SERVICE_MAX_MEMBERS)
    pub max_members: i64,
    /// CIs one add or remove request may name
    pub max_batch: i64,
    /// Longest chain of services including services (BUSINESS_SERVICE_MAX_NESTING)
    pub max_nesting: i32,
    /// Owners per role
    pub max_owners_per_role: i64,
}

/// A business service with its class, the visibility note and the limits.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BusinessService {
    pub id: Uuid,
    pub ident: String,
    pub name: String,
    #[schema(required = true)]
    pub criticality: Option<CriticalityRef>,
    pub active: bool,
    pub owners: ServiceOwners,
    /// Direct members the caller may view
    pub member_count: i64,
    /// Of memberCount, how many are business services
    pub service_member_count: i64,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
    pub version: i32,
    pub class_id: Uuid,
    #[schema(inline)]
    pub visibility: Visibility,
    pub limits: BusinessServiceLimits,
}

impl BusinessService {
    pub fn new(
        s: BusinessServiceSummary,
        class_id: Uuid,
        visibility: Visibility,
        limits: BusinessServiceLimits,
    ) -> Self {
        BusinessService {
            id: s.id,
            ident: s.ident,
            name: s.name,
            criticality: s.criticality,
            active: s.active,
            owners: s.owners,
            member_count: s.member_count,
            service_member_count: s.service_member_count,
            updated_at: s.updated_at,
            version: s.version,
            class_id,
            visibility,
            limits,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BusinessServiceList {
    pub data: Vec<BusinessServiceSummary>,
    pub page: PageMeta,
    /// restricted: the caller's profile limits the classes they may view, so member counts leave CIs out. Derived
    /// from the permissions only, never from the data
    #[schema(inline)]
    pub visibility: Visibility,
}

// ---------------------------------------------------------------------------
// List query
// ---------------------------------------------------------------------------

/// Comma-separated ids and the literal `none`, at most 50.
#[derive(Debug, Clone)]
pub struct CriticalityFilter {
    pub ids: Vec<Uuid>,
    pub none: bool,
}

impl<'de> Deserialize<'de> for CriticalityFilter {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let s = String::deserialize(d)?;
        let parts: Vec<&str> = s.split(',').map(str::trim).filter(|p| !p.is_empty()).collect();
        if parts.is_empty() {
            return Err(D::Error::custom("too_small|Too small: expected array to have >=1 items"));
        }
        if parts.len() > MAX_FILTER_IDS {
            return Err(D::Error::custom(format!("too_big|Too big: expected array to have <={MAX_FILTER_IDS} items")));
        }
        let mut out = CriticalityFilter { ids: Vec::new(), none: false };
        for p in parts {
            if p == "none" {
                out.none = true;
            } else if let Some(id) = validate::is_uuid(p).then(|| Uuid::parse_str(p).ok()).flatten() {
                out.ids.push(id);
            } else {
                return Err(D::Error::custom("invalid_format|Invalid UUID"));
            }
        }
        Ok(out)
    }
}

/// Most ids in the `classId`, `ownerId` and `criticalityValueId` filters.
pub const MAX_FILTER_IDS: usize = 50;
/// Most ids in the member list's `ciId` filter.
pub const MAX_CI_IDS: usize = 200;

/// Comma-separated ids, at most `MAX` (repeated keys are joined with commas first).
#[derive(Debug, Clone)]
pub struct IdList<const MAX: usize>(pub Vec<Uuid>);

impl<'de, const MAX: usize> Deserialize<'de> for IdList<MAX> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let s = String::deserialize(d)?;
        let parts: Vec<&str> = s.split(',').map(str::trim).filter(|p| !p.is_empty()).collect();
        if parts.is_empty() {
            return Err(D::Error::custom("too_small|Too small: expected array to have >=1 items"));
        }
        if parts.len() > MAX {
            return Err(D::Error::custom(format!("too_big|Too big: expected array to have <={MAX} items")));
        }
        let mut ids = Vec::with_capacity(parts.len());
        for p in parts {
            match validate::is_uuid(p).then(|| Uuid::parse_str(p).ok()).flatten() {
                Some(id) if !ids.contains(&id) => ids.push(id),
                Some(_) => {}
                None => return Err(D::Error::custom("invalid_format|Invalid UUID")),
            }
        }
        Ok(IdList(ids))
    }
}

fn criticality_schema() -> Schema {
    schemas::uuid_list_described(
        "Criticality values (ids of the criticality list's values), comma-separated, at most 50; `none` selects \
         services whose criticality is not set",
    )
}

fn owner_ids_schema() -> Schema {
    schemas::uuid_list_described(
        "Services owned by any of these users or groups (ids, comma-separated, at most 50), in any role unless \
         ownerRole narrows it. A user id does not match through the user's groups",
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum OwnerRoleParam {
    Technical,
    Business,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum OwnerStateParam {
    /// No owner in either role
    None,
    /// At least one owner is a disabled user
    Disabled,
}

fn owner_role_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(["technical", "business"]))
        .description(Some("Narrows ownerId and mine to one owner role"))
        .into()
}

fn owner_state_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(["none", "disabled"]))
        .description(Some("none: services without any owner; disabled: services with a disabled user as an owner"))
        .into()
}

fn mine_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(["true", "false"]))
        .default(Some("false".into()))
        .description(Some("true: only services the caller owns, directly or through one of their groups"))
        .into()
}

fn include_inactive_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(["true", "false"]))
        .default(Some("true".into()))
        .description(Some("false: only services inside their validity period"))
        .into()
}

pub const SERVICE_SORT_FIELDS: &[&str] = &["name", "criticality", "memberCount", "updatedAt"];

fn service_sort_schema() -> Schema {
    let mut s = schemas::sort_schema(SERVICE_SORT_FIELDS, "criticality");
    if let Schema::Object(o) = &mut s {
        o.description = Some(
            "Sort field; prefix with \"-\" for descending: name, criticality (most critical first; not set last), \
             memberCount (visible members), updatedAt. Ties by name. Default: criticality"
                .into(),
        );
    }
    s
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct BusinessServiceQuery {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    pub limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    pub offset: i64,
    /// Name or ident, matched like the CI list's search
    #[param(schema_with = schemas::search_schema)]
    pub q: Option<String>,
    #[param(schema_with = criticality_schema)]
    pub criticality_value_id: Option<CriticalityFilter>,
    #[param(schema_with = owner_ids_schema)]
    pub owner_id: Option<IdList<MAX_FILTER_IDS>>,
    #[param(schema_with = owner_role_schema)]
    pub owner_role: Option<OwnerRoleParam>,
    #[param(required = false, schema_with = mine_schema)]
    pub mine: Option<QueryBool>,
    #[param(schema_with = owner_state_schema)]
    pub owner_state: Option<OwnerStateParam>,
    #[param(required = false, schema_with = include_inactive_schema)]
    pub include_inactive: Option<QueryBool>,
    #[param(required = false, schema_with = service_sort_schema)]
    pub sort: Sort,
}
paged!(BusinessServiceQuery);

// ---------------------------------------------------------------------------
// Members
// ---------------------------------------------------------------------------

/// A CI as a member list shows it.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CiRef {
    pub id: Uuid,
    pub ident: String,
    /// The CI's label (display name)
    pub name: String,
    pub class_id: Uuid,
    pub class_name: String,
    #[schema(required = true)]
    pub criticality: Option<CriticalityRef>,
    /// Inside its validity period
    pub active: bool,
}

/// A member of a business service.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Member {
    /// The membership relationship's id (ci_relationships)
    pub membership_id: Uuid,
    pub ci: CiRef,
    /// The member is itself a business service (nested)
    pub is_service: bool,
    #[serde(serialize_with = "ts::serialize")]
    pub added_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BusinessServiceMemberList {
    pub data: Vec<Member>,
    pub page: PageMeta,
    /// restricted: members of classes the caller may not view are neither listed nor counted. Derived from the
    /// permissions only, never from the data
    #[schema(inline)]
    pub visibility: Visibility,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum MemberKind {
    Ci,
    Service,
}

fn member_kind_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(["ci", "service"]))
        .description(Some("service: only members that are business services (nested); ci: only the others"))
        .into()
}

fn class_ids_schema() -> Schema {
    schemas::uuid_list_described(
        "Members of these classes (ids, comma-separated, at most 50). A class that does not exist or that the caller \
         may not view is refused with 400 VALIDATION_ERROR, the same for both",
    )
}

fn ci_ids_schema() -> Schema {
    schemas::uuid_list_described(
        "Which of these CIs are members (ids, comma-separated, at most 200). CIs the caller may not view are absent, \
         exactly like CIs that are not members",
    )
}

pub const MEMBER_SORT_FIELDS: &[&str] = &["name", "class", "criticality", "addedAt"];

fn member_sort_schema() -> Schema {
    schemas::sort_schema(MEMBER_SORT_FIELDS, "name")
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct MemberQuery {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    pub limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    pub offset: i64,
    /// Member name or ident (case-insensitive substring)
    #[param(schema_with = schemas::search_schema)]
    pub q: Option<String>,
    #[param(schema_with = class_ids_schema)]
    pub class_id: Option<IdList<MAX_FILTER_IDS>>,
    #[param(schema_with = member_kind_schema)]
    pub kind: Option<MemberKind>,
    #[param(schema_with = ci_ids_schema)]
    pub ci_id: Option<IdList<MAX_CI_IDS>>,
    #[param(required = false, schema_with = member_sort_schema)]
    pub sort: Sort,
}
paged!(MemberQuery);

/// The member list's filters and sort, without paging (CSV export).
#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct MemberExportQuery {
    /// Member name or ident (case-insensitive substring)
    #[param(schema_with = schemas::search_schema)]
    pub q: Option<String>,
    #[param(schema_with = class_ids_schema)]
    pub class_id: Option<IdList<MAX_FILTER_IDS>>,
    #[param(schema_with = member_kind_schema)]
    pub kind: Option<MemberKind>,
    #[param(schema_with = ci_ids_schema)]
    pub ci_id: Option<IdList<MAX_CI_IDS>>,
    #[param(required = false, schema_with = member_sort_schema)]
    pub sort: Sort,
}

impl From<MemberExportQuery> for MemberQuery {
    fn from(q: MemberExportQuery) -> Self {
        MemberQuery { limit: 0, offset: 0, q: q.q, class_id: q.class_id, kind: q.kind, ci_id: q.ci_id, sort: q.sort }
    }
}

fn member_ids_schema() -> Schema {
    ArrayBuilder::new()
        .items(schemas::uuid_builder())
        .min_items(Some(1))
        .max_items(Some(MAX_BATCH))
        .unique_items(true)
        .description(Some("CI ids, at most 500"))
        .into()
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BusinessServiceMembersAdd {
    #[schema(schema_with = member_ids_schema)]
    pub member_ids: Vec<Uuid>,
}
impl Check for BusinessServiceMembersAdd {}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BusinessServiceMembersAdded {
    /// The new members, by name
    pub added: Vec<Member>,
    /// Requested CIs that were already members (not an error)
    pub already_members: Vec<Uuid>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BusinessServiceMembersRemove {
    #[schema(schema_with = member_ids_schema)]
    pub member_ids: Vec<Uuid>,
}
impl Check for BusinessServiceMembersRemove {}

// ---------------------------------------------------------------------------
// Owners
// ---------------------------------------------------------------------------

/// A user or group to assign as owner.
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrincipalInput {
    pub kind: PrincipalKind,
    pub id: Uuid,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BusinessServiceOwnersReplace {
    /// The service's current version (BusinessService.version); another one answers 409 VERSION_CONFLICT
    pub version: i32,
    /// In display order, at most 10
    #[schema(inline, max_items = 10)]
    pub technical: Vec<PrincipalInput>,
    /// In display order, at most 10
    #[schema(inline, max_items = 10)]
    pub business: Vec<PrincipalInput>,
}
impl Check for BusinessServiceOwnersReplace {}

// ---------------------------------------------------------------------------
// "Part of business services"
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigurationItemService {
    pub service: BusinessServiceSummary,
    /// The service includes the CI itself
    pub direct: bool,
    /// For a nested membership, the services between the CI and this one, starting with the one that includes the
    /// CI: the CI is part of viaServiceIds[0], which is part of viaServiceIds[1], …, which is part of `service`.
    /// Empty when direct
    pub via_service_ids: Vec<Uuid>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigurationItemServiceList {
    /// Direct first, then by nesting depth, then by name; at most 200
    pub data: Vec<ConfigurationItemService>,
    /// The CI is part of more than 200 services, services including it are nested deeper than
    /// BUSINESS_SERVICE_MAX_NESTING (the deeper ones are left out), or the walk stopped at a bound of the
    /// impact analysis
    pub truncated: bool,
    #[schema(inline)]
    pub visibility: Visibility,
}

// ---------------------------------------------------------------------------
// Principals (owner picker)
// ---------------------------------------------------------------------------

fn principal_q_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .min_length(Some(2))
        .max_length(Some(100))
        .description(Some(
            "Matched (case-insensitive substring) against users' display names and usernames and group names",
        ))
        .into()
}

fn principal_kind_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(["user", "group"]))
        .description(Some("Only users or only groups; both when left out"))
        .into()
}

fn principal_inactive_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(["true", "false"]))
        .default(Some("false".into()))
        .description(Some("true: disabled user accounts are included"))
        .into()
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct PrincipalQuery {
    /// Required; checked after the permission, so a caller without it gets 403 whatever they send
    #[param(required = false, schema_with = principal_q_schema)]
    pub q: Option<String>,
    #[param(schema_with = principal_kind_schema)]
    pub kind: Option<PrincipalKind>,
    #[param(required = false, schema_with = principal_inactive_schema)]
    pub include_inactive: Option<QueryBool>,
}

/// A user or group found by the owner picker.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Principal {
    pub kind: PrincipalKind,
    pub id: Uuid,
    pub display_name: String,
    /// Users only, to tell people with the same name apart
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    pub active: bool,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PrincipalList {
    /// At most 20, best matches first (a name or username starting with q), then by name
    pub data: Vec<Principal>,
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BusinessServiceSettings {
    /// The built-in business service class (`systemRole` business_service)
    pub class_id: Uuid,
    /// The built-in member relationship type (`systemRole` business_service_member)
    pub member_relationship_type_id: Uuid,
    /// The caller may view business services
    pub can_view: bool,
    /// The caller may edit business services: their members and owners
    pub can_edit: bool,
    pub limits: BusinessServiceLimits,
}

/// Errors of one array entry, `field[index]`.
pub fn entry_error(field: &str, index: usize, message: &str, code: &str) -> FieldError {
    FieldError {
        location: crate::http::error::FieldLocation::Body,
        field: format!("{field}[{index}]"),
        message: message.into(),
        code: code.into(),
    }
}
