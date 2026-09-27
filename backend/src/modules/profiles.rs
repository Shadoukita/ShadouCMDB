//! Administration > Permission profiles: named sets of global and per-class
//! permissions, each of which may require two-factor authentication of its
//! holders. The built-in Administrator profile is read-only except for that
//! requirement.

use std::collections::{HashMap, HashSet};

use axum::http::{Method, StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use utoipa::openapi::schema::Schema;
use utoipa::{IntoParams, PartialSchema, ToSchema};
use uuid::Uuid;

use crate::api::context::{RequestContext, forbidden};
use crate::api::route::{Body, Check, IdPath, In, Json, NoBody, NoContent, NoPath, NoQuery, Query, Route, route};
use crate::api::schemas::{self, Page, Paged, Sort, description_schema, like_pattern, name_schema, trimmed, ts};
use crate::auth::permissions::{ClassRights, GlobalPermission, Permissions};
use crate::data::auth::{self as data, ProfileRow};
use crate::data::crud::{self, AuditAction, AuditEntry, Where};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::paged;

const TABLE: &str = "permission_profiles";

// ---------------------------------------------------------------------------
// Schemas
// ---------------------------------------------------------------------------

/// Rights on one CI class, or on every class when `classId` is null.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClassPermission {
    /// The class; null is the "all classes" wildcard (also covers classes created later)
    #[schema(required = true)]
    pub class_id: Option<Uuid>,
    /// See the CIs of this class (implied by the other three)
    pub view: bool,
    pub create: bool,
    pub edit: bool,
    pub delete: bool,
}

impl ClassPermission {
    pub fn rights(&self) -> ClassRights {
        ClassRights { view: self.view, create: self.create, edit: self.edit, delete: self.delete }
    }
    pub fn new(class_id: Option<Uuid>, r: ClassRights) -> Self {
        ClassPermission { class_id, view: r.view, create: r.create, edit: r.edit, delete: r.delete }
    }
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PermissionProfile {
    pub id: Uuid,
    pub name: String,
    #[schema(required = true)]
    pub description: Option<String>,
    /// The Administrator profile: every permission, read-only (except requireMfa), cannot be deleted
    pub is_builtin: bool,
    /// Holders must set up two-factor authentication; until they do, their
    /// session only reaches the MFA set-up routes
    pub require_mfa: bool,
    #[schema(inline)]
    pub global_permissions: Vec<GlobalPermission>,
    pub class_permissions: Vec<ClassPermission>,
    /// Users holding this profile
    pub user_count: i64,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
}

fn class_permissions_schema() -> Schema {
    utoipa::openapi::schema::ArrayBuilder::new()
        .items(ClassPermission::schema())
        .max_items(Some(1000))
        .description(Some("Replaces all class grants. Entries granting nothing are dropped; one entry per class."))
        .into()
}

fn global_permissions_schema() -> Schema {
    utoipa::openapi::schema::ArrayBuilder::new()
        .items(GlobalPermission::schema())
        .unique_items(true)
        .description(Some("Replaces all global permissions"))
        .into()
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileCreate {
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    name: String,
    #[schema(schema_with = description_schema)]
    #[serde(default)]
    description: Option<String>,
    #[schema(schema_with = global_permissions_schema)]
    #[serde(default)]
    global_permissions: Vec<GlobalPermission>,
    #[schema(schema_with = class_permissions_schema)]
    #[serde(default)]
    class_permissions: Vec<ClassPermission>,
    /// Holders must set up two-factor authentication (default false)
    #[serde(default)]
    require_mfa: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileUpdate {
    #[schema(schema_with = name_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    name: Option<String>,
    #[schema(schema_with = description_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    description: Option<Option<String>>,
    #[schema(schema_with = global_permissions_schema)]
    global_permissions: Option<Vec<GlobalPermission>>,
    #[schema(schema_with = class_permissions_schema)]
    class_permissions: Option<Vec<ClassPermission>>,
    /// Holders must set up two-factor authentication. The only field the
    /// built-in Administrator profile accepts.
    #[schema(nullable = false)]
    require_mfa: Option<bool>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileClone {
    /// Name of the copy
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    name: String,
}
impl Check for ProfileClone {}

fn duplicate_classes(grants: &[ClassPermission]) -> Vec<FieldError> {
    let mut seen = HashSet::new();
    grants
        .iter()
        .enumerate()
        .filter(|(_, g)| !seen.insert(g.class_id))
        .map(|(i, _)| FieldError {
            location: FieldLocation::Body,
            field: format!("classPermissions.{i}.classId"),
            message: "Only one entry per class (and one wildcard)".into(),
            code: "duplicate".into(),
        })
        .collect()
}

impl Check for ProfileCreate {
    fn check(&self) -> Vec<FieldError> {
        duplicate_classes(&self.class_permissions)
    }
}

impl Check for ProfileUpdate {
    fn check(&self) -> Vec<FieldError> {
        if self.name.is_none()
            && self.description.is_none()
            && self.global_permissions.is_none()
            && self.class_permissions.is_none()
            && self.require_mfa.is_none()
        {
            return vec![FieldError {
                location: FieldLocation::Body,
                field: "(root)".into(),
                message: "Provide at least one field to update".into(),
                code: "custom".into(),
            }];
        }
        self.class_permissions.as_deref().map(duplicate_classes).unwrap_or_default()
    }
}

fn sort_schema() -> Schema {
    schemas::sort_schema(&["name", "createdAt", "updatedAt"], "name")
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ProfileList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    offset: i64,
    #[param(schema_with = schemas::search_schema)]
    q: Option<String>,
    #[param(required = false, schema_with = sort_schema)]
    sort: Sort,
}
paged!(ProfileList);

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

/// DTOs for these rows, with their permissions (two queries for any number of rows).
async fn dtos(conn: &mut PgConnection, rows: Vec<ProfileRow>) -> Result<Vec<PermissionProfile>, AppError> {
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let mut globals: HashMap<Uuid, Vec<GlobalPermission>> = HashMap::new();
    for (id, name) in data::profile_global_permissions(conn, &ids).await? {
        if let Some(p) = GlobalPermission::parse(&name) {
            globals.entry(id).or_default().push(p);
        }
    }
    let mut classes: HashMap<Uuid, Vec<ClassPermission>> = HashMap::new();
    for c in data::profile_class_permissions(conn, &ids).await? {
        let rights = ClassRights { view: c.can_view, create: c.can_create, edit: c.can_edit, delete: c.can_delete };
        classes.entry(c.profile_id).or_default().push(ClassPermission::new(c.class_id, rights));
    }
    Ok(rows
        .into_iter()
        .map(|r| {
            let (mut global_permissions, class_permissions) = if r.is_builtin {
                (GlobalPermission::ALL.to_vec(), vec![ClassPermission::new(None, ClassRights::ALL)])
            } else {
                (globals.remove(&r.id).unwrap_or_default(), classes.remove(&r.id).unwrap_or_default())
            };
            global_permissions.sort();
            PermissionProfile {
                id: r.id,
                name: r.name,
                description: r.description,
                is_builtin: r.is_builtin,
                require_mfa: r.require_mfa,
                global_permissions,
                class_permissions,
                user_count: r.user_count,
                created_at: r.created_at,
                updated_at: r.updated_at,
            }
        })
        .collect())
}

async fn load(conn: &mut PgConnection, id: Uuid) -> Result<PermissionProfile, AppError> {
    let row = data::get_profile(conn, id, false).await?.ok_or_else(|| AppError::missing("Permission profile", id))?;
    Ok(dtos(conn, vec![row]).await?.remove(0))
}

/// What a set of profiles grants together.
pub async fn union_of(conn: &mut PgConnection, profile_ids: &[Uuid]) -> Result<Permissions, AppError> {
    let mut p = Permissions::default();
    let mut rows = Vec::new();
    for id in profile_ids {
        if let Some(r) = data::get_profile(conn, *id, false).await? {
            rows.push(r);
        }
    }
    for profile in dtos(conn, rows).await? {
        add_profile(&mut p, &profile);
    }
    Ok(p)
}

fn add_profile(p: &mut Permissions, profile: &PermissionProfile) {
    if profile.is_builtin {
        p.administrator = true;
        return;
    }
    for g in &profile.global_permissions {
        p.merge_global(*g);
    }
    for c in &profile.class_permissions {
        p.merge_class(c.class_id, c.rights().normalised());
    }
}

fn grants(global: &[GlobalPermission], classes: &[ClassPermission]) -> Permissions {
    let mut p = Permissions::default();
    for g in global {
        p.merge_global(*g);
    }
    for c in classes {
        p.merge_class(c.class_id, c.rights().normalised());
    }
    p
}

/// Profile managers cannot grant, or change profiles that grant, more than they hold.
fn must_cover(ctx: &RequestContext, p: &Permissions, what: &str) -> Result<(), AppError> {
    match ctx.principal() {
        Some(me) if !me.permissions.covers(p) => {
            Err(forbidden(format!("{what} grants permissions you do not hold yourself")))
        }
        _ => Ok(()),
    }
}

async fn check_classes(conn: &mut PgConnection, classes: &[ClassPermission]) -> Result<(), AppError> {
    let ids: Vec<Uuid> = classes.iter().filter_map(|c| c.class_id).collect();
    let found: HashSet<Uuid> = data::existing_classes(conn, &ids).await?.into_iter().collect();
    let errors: Vec<FieldError> = classes
        .iter()
        .enumerate()
        .filter(|(_, c)| c.class_id.is_some_and(|id| !found.contains(&id)))
        .map(|(i, _)| FieldError {
            location: FieldLocation::Body,
            field: format!("classPermissions.{i}.classId"),
            message: "CI class does not exist".into(),
            code: "not_found".into(),
        })
        .collect();
    if errors.is_empty() { Ok(()) } else { Err(AppError::validation(errors)) }
}

fn builtin_is_read_only() -> AppError {
    AppError::conflict("The built-in Administrator profile cannot be changed (except requireMfa) or deleted")
}

pub async fn list(pool: &PgPool, q: &ProfileList) -> Result<Page<PermissionProfile>, AppError> {
    let filter = |w: &mut Where<'_>| {
        if let Some(text) = &q.q {
            let pattern = like_pattern(text);
            w.and()
                .push("(p.name ILIKE ")
                .push_bind(pattern.clone())
                .push(" OR p.description ILIKE ")
                .push_bind(pattern)
                .push(")");
        }
    };
    let column = match q.sort.field.as_str() {
        "createdAt" => "p.created_at",
        "updatedAt" => "p.updated_at",
        _ => "lower(p.name)",
    };
    // The built-in profile first, then the requested order.
    let order = format!("p.is_builtin DESC, {column} {}, p.id", q.sort.dir());
    let (rows, total) = crud::select_page::<ProfileRow>(
        pool,
        "permission_profiles p",
        data::PROFILE_COLUMNS,
        &filter,
        &order,
        q.limit,
        q.offset,
    )
    .await?;
    let data = dtos(&mut *pool.acquire().await?, rows).await?;
    Ok(Page { data, page: q.page_meta(total) })
}

pub async fn get(pool: &PgPool, id: Uuid) -> Result<PermissionProfile, AppError> {
    load(&mut *pool.acquire().await?, id).await
}

pub(crate) async fn insert(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    name: &str,
    description: Option<&str>,
    global: &[GlobalPermission],
    classes: &[ClassPermission],
    require_mfa: bool,
) -> Result<PermissionProfile, AppError> {
    must_cover(ctx, &grants(global, classes), "This profile")?;
    check_classes(conn, classes).await?;
    let id = data::insert_profile(conn, name, description, require_mfa).await?;
    data::set_global_permissions(conn, id, global).await?;
    let class_grants: Vec<(Option<Uuid>, ClassRights)> = classes.iter().map(|c| (c.class_id, c.rights())).collect();
    data::set_class_permissions(conn, id, &class_grants).await?;
    let dto = load(conn, id).await?;
    let entry = AuditEntry {
        action: AuditAction::Create,
        entity_type: TABLE,
        entity_id: id,
        old_value: None,
        new_value: Some(crud::json(&dto)),
    };
    crud::write_audit(conn, ctx, vec![entry]).await?;
    Ok(dto)
}

pub async fn create(pool: &PgPool, ctx: &RequestContext, b: &ProfileCreate) -> Result<PermissionProfile, AppError> {
    let mut tx = pool.begin().await?;
    let dto = insert(
        &mut tx,
        ctx,
        &b.name,
        b.description.as_deref(),
        &b.global_permissions,
        &b.class_permissions,
        b.require_mfa,
    )
    .await?;
    tx.commit().await?;
    Ok(dto)
}

/// A new, editable profile with the same permissions (cloning Administrator
/// gives an editable profile with every permission spelled out).
pub async fn clone(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    b: &ProfileClone,
) -> Result<PermissionProfile, AppError> {
    let mut tx = pool.begin().await?;
    let source = load(&mut tx, id).await?;
    let description = source.description.as_deref().filter(|_| !source.is_builtin);
    let dto = insert(
        &mut tx,
        ctx,
        &b.name,
        description,
        &source.global_permissions,
        &source.class_permissions,
        source.require_mfa,
    )
    .await?;
    tx.commit().await?;
    Ok(dto)
}

/// Every profile except the built-in one, by name (configuration export).
pub(crate) async fn all_editable(conn: &mut PgConnection) -> Result<Vec<PermissionProfile>, AppError> {
    let rows = data::editable_profiles(conn).await?;
    dtos(conn, rows).await
}

/// Changes a profile in the caller's transaction; `None` leaves a part as it is.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn update_in(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    id: Uuid,
    name: Option<&str>,
    description: Option<Option<&str>>,
    global_permissions: Option<&[GlobalPermission]>,
    class_permissions: Option<&[ClassPermission]>,
    require_mfa: Option<bool>,
) -> Result<PermissionProfile, AppError> {
    let row = data::get_profile(conn, id, true).await?.ok_or_else(|| AppError::missing("Permission profile", id))?;
    let only_mfa =
        name.is_none() && description.is_none() && global_permissions.is_none() && class_permissions.is_none();
    if row.is_builtin && !only_mfa {
        return Err(builtin_is_read_only());
    }
    let before = dtos(conn, vec![row]).await?.remove(0);
    must_cover(ctx, &grants(&before.global_permissions, &before.class_permissions), "This profile")?;
    let global = global_permissions.unwrap_or(&before.global_permissions);
    let classes = class_permissions.unwrap_or(&before.class_permissions);
    must_cover(ctx, &grants(global, classes), "The updated profile")?;

    data::update_profile(conn, id, name, description, require_mfa).await?;
    if let Some(g) = global_permissions {
        data::set_global_permissions(conn, id, g).await?;
    }
    if let Some(c) = class_permissions {
        check_classes(conn, c).await?;
        let class_grants: Vec<(Option<Uuid>, ClassRights)> = c.iter().map(|c| (c.class_id, c.rights())).collect();
        data::set_class_permissions(conn, id, &class_grants).await?;
    }
    let dto = load(conn, id).await?;
    let entry = AuditEntry {
        action: AuditAction::Update,
        entity_type: TABLE,
        entity_id: id,
        old_value: Some(crud::json(&before)),
        new_value: Some(crud::json(&dto)),
    };
    crud::write_audit(conn, ctx, vec![entry]).await?;
    Ok(dto)
}

pub async fn update(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    b: &ProfileUpdate,
) -> Result<PermissionProfile, AppError> {
    let mut tx = pool.begin().await?;
    let dto = update_in(
        &mut tx,
        ctx,
        id,
        b.name.as_deref(),
        b.description.as_ref().map(|d| d.as_deref()),
        b.global_permissions.as_deref(),
        b.class_permissions.as_deref(),
        b.require_mfa,
    )
    .await?;
    tx.commit().await?;
    Ok(dto)
}

pub async fn remove(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    let row = data::get_profile(&mut tx, id, true).await?.ok_or_else(|| AppError::missing("Permission profile", id))?;
    if row.is_builtin {
        return Err(builtin_is_read_only());
    }
    let before = dtos(&mut tx, vec![row]).await?.remove(0);
    must_cover(ctx, &grants(&before.global_permissions, &before.class_permissions), "This profile")?;
    data::delete_profile(&mut tx, id).await?;
    let entry = AuditEntry {
        action: AuditAction::Delete,
        entity_type: TABLE,
        entity_id: id,
        old_value: Some(crud::json(&before)),
        new_value: None,
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

const TAG: &str = "Permission profiles";
const BASE: &str = "/api/v1/admin/profiles";
const BY_ID: &str = "/api/v1/admin/profiles/{id}";

pub fn routes() -> Vec<Route> {
    let manage = GlobalPermission::ProfilesManage;
    vec![
        route(Method::GET, BASE, "listPermissionProfiles")
            .tag(TAG)
            .summary("List permission profiles (paginated; the built-in Administrator profile first)")
            .description("`q` matches name and description. Requires `profiles.manage` or `users.manage` (to assign profiles).")
            .errors(&[ErrorCode::Forbidden])
            .handle(|api, In(NoPath, Query(q), NoBody): In<NoPath, Query<ProfileList>, NoBody>| async move {
                require_any(&api.ctx)?;
                Ok(Json(list(&api.pool, &q).await?))
            }),
        route(Method::GET, BY_ID, "getPermissionProfile")
            .tag(TAG)
            .summary("Get one permission profile with its permissions")
            .description("Requires `profiles.manage` or `users.manage`.")
            .errors(&[ErrorCode::Forbidden, ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                require_any(&api.ctx)?;
                Ok(Json(get(&api.pool, id).await?))
            }),
        route(Method::POST, BASE, "createPermissionProfile")
            .tag(TAG)
            .summary("Create a permission profile")
            .description("A non-administrator can only grant permissions they hold themselves (403 otherwise).")
            .status(StatusCode::CREATED)
            .requires(manage)
            .errors(&[ErrorCode::Conflict])
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<ProfileCreate>>| async move {
                Ok(Json(create(&api.pool, &api.ctx, &b).await?))
            }),
        route(Method::PATCH, BY_ID, "updatePermissionProfile")
            .tag(TAG)
            .summary("Update a permission profile (partial; permission lists replace the current ones)")
            .description("The built-in Administrator profile accepts only `requireMfa` (409 for anything else). Takes effect on the holders' next request.")
            .requires(manage)
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict])
            .handle(|api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<ProfileUpdate>>| async move {
                Ok(Json(update(&api.pool, &api.ctx, id, &b).await?))
            }),
        route(Method::DELETE, BY_ID, "deletePermissionProfile")
            .tag(TAG)
            .summary("Delete a permission profile (users holding it lose it)")
            .description("The built-in Administrator profile cannot be deleted (409).")
            .requires(manage)
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                remove(&api.pool, &api.ctx, id).await?;
                Ok(NoContent)
            }),
        route(Method::POST, "/api/v1/admin/profiles/{id}/clone", "clonePermissionProfile")
            .tag(TAG)
            .summary("Copy a profile (including the built-in one) into a new, editable profile")
            .status(StatusCode::CREATED)
            .requires(manage)
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict])
            .handle(|api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<ProfileClone>>| async move {
                Ok(Json(clone(&api.pool, &api.ctx, id, &b).await?))
            }),
    ]
}

/// Reading profiles: profile managers, and user managers who assign them.
fn require_any(ctx: &RequestContext) -> Result<(), AppError> {
    ctx.require(GlobalPermission::ProfilesManage).or_else(|_| ctx.require(GlobalPermission::UsersManage))
}
