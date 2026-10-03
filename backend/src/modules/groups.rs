//! Administration > Access > Groups: named sets of users that can own
//! business services (SHAA-927 §1.4, §4.9).
//!
//! Managed with `users.manage`, like users. Groups grant no permission (they
//! are not part of permission profiles in this release), so a user manager
//! may put any user in any group. Groups are identity data and are not part
//! of the configuration export.
//!
//! Changing a group or its members needs the `version` the administrator
//! loaded (409 VERSION_CONFLICT when stale). Deleting a group that owns
//! business services is allowed: its owner rows go with it, and every
//! affected service gets an `update` row in its history (see
//! [`crate::data::service_owners`]).

use std::collections::{HashMap, HashSet};

use axum::http::{Method, StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use utoipa::openapi::schema::{ArrayBuilder, Schema};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::api::context::{Count, RequestContext};
use crate::api::route::{Body, Check, IdPath, In, Json, NoBody, NoPath, NoQuery, Query, Route, route};
use crate::api::schemas::{self, Page, Paged, Sort, description_schema, like_pattern, name_schema, trimmed, ts};
use crate::auth::permissions::GlobalPermission;
use crate::data::crud::{self, AuditAction, AuditEntry, ColumnSet, Where};
use crate::data::service_owners::{self, OwnerCleanup, Principal};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::modules::simple_resource::non_empty;
use crate::paged;

const TABLE: &str = "user_groups";
const LABEL: &str = "User group";
/// Users per group (§1.3).
pub const MAX_MEMBERS: usize = 1000;

// ---------------------------------------------------------------------------
// Schemas
// ---------------------------------------------------------------------------

/// A user group
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserGroup {
    pub id: Uuid,
    /// Unique regardless of case
    pub name: String,
    #[schema(required = true)]
    pub description: Option<String>,
    pub member_count: i64,
    /// Send it back with a change; a stale one fails with 409 VERSION_CONFLICT
    pub version: i32,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
}

/// A user group with the number of business services it owns
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserGroupDetail {
    pub id: Uuid,
    pub name: String,
    #[schema(required = true)]
    pub description: Option<String>,
    pub member_count: i64,
    pub version: i32,
    /// Business services the group owns in any role (deleted ones that can still be restored included): the
    /// owner entries a delete removes. Null when withheld: the caller may not view the business service class.
    #[schema(value_type = Option<i64>, required = true)]
    pub owned_service_count: Count,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
}

/// A user in a group
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserGroupMember {
    /// The user's id
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
    /// Disabled users stay members
    pub is_active: bool,
    #[serde(serialize_with = "ts::serialize")]
    pub added_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserGroupCreate {
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    pub name: String,
    #[schema(schema_with = description_schema)]
    #[serde(default)]
    pub description: Option<String>,
}

impl Check for UserGroupCreate {}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserGroupUpdate {
    /// The version you loaded; if someone saved in between, the request fails with 409 VERSION_CONFLICT
    #[schema(minimum = 1)]
    pub version: i32,
    #[schema(schema_with = name_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    pub name: Option<String>,
    #[schema(schema_with = description_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    pub description: Option<Option<String>>,
}

impl UserGroupUpdate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("name", self.name.clone()).opt("description", self.description.clone());
        c
    }
}

impl Check for UserGroupUpdate {
    fn check(&self) -> Vec<FieldError> {
        non_empty(&self.columns())
    }
}

fn user_ids_schema() -> Schema {
    ArrayBuilder::new()
        .items(schemas::uuid_builder())
        .max_items(Some(MAX_MEMBERS))
        .unique_items(true)
        .description(Some("Every member of the group (replaces the current set; empty removes all)"))
        .into()
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserGroupMembersReplace {
    /// The version you loaded; if someone saved in between, the request fails with 409 VERSION_CONFLICT
    #[schema(minimum = 1)]
    pub version: i32,
    #[schema(schema_with = user_ids_schema)]
    pub user_ids: Vec<Uuid>,
}

impl Check for UserGroupMembersReplace {
    fn check(&self) -> Vec<FieldError> {
        let mut seen = HashSet::new();
        self.user_ids
            .iter()
            .enumerate()
            .filter(|(_, id)| !seen.insert(**id))
            .map(|(i, _)| FieldError {
                location: FieldLocation::Body,
                field: format!("userIds[{i}]"),
                message: "Listed more than once".into(),
                code: "duplicate".into(),
            })
            .collect()
    }
}

/// What a delete changed
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwnerRemoval {
    /// Business services that lost this owner (each has an `update` row in its history). Null when withheld: the
    /// caller may not view the business service class.
    #[schema(value_type = Option<i64>, required = true)]
    pub affected_services: Count,
}

fn sort_schema() -> Schema {
    schemas::sort_schema(&["name", "memberCount"], "name")
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct UserGroupList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    offset: i64,
    /// Matches the name and description
    #[param(schema_with = schemas::search_schema)]
    q: Option<String>,
    #[param(required = false, schema_with = sort_schema)]
    sort: Sort,
}
paged!(UserGroupList);

fn member_sort_schema() -> Schema {
    schemas::sort_schema(&["displayName", "username", "addedAt"], "displayName")
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct UserGroupMemberList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    offset: i64,
    /// Matches username and display name
    #[param(schema_with = schemas::search_schema)]
    q: Option<String>,
    #[param(required = false, schema_with = member_sort_schema)]
    sort: Sort,
}
paged!(UserGroupMemberList);

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

const COLUMNS: &str = "g.id, g.name, g.description, g.version, g.created_at, g.updated_at, \
     (SELECT count(*) FROM user_group_members m WHERE m.group_id = g.id) AS member_count";

async fn load(conn: &mut PgConnection, id: Uuid, for_update: bool) -> Result<UserGroup, AppError> {
    let lock = if for_update { " FOR UPDATE OF g" } else { "" };
    sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {COLUMNS} FROM user_groups g WHERE g.id = $1{lock}")))
        .bind(id)
        .fetch_optional(conn)
        .await?
        .ok_or_else(|| AppError::missing(LABEL, id))
}

fn stale(sent: i32, current: i32) -> Result<(), AppError> {
    if sent == current {
        return Ok(());
    }
    Err(AppError::new(
        ErrorCode::VersionConflict,
        format!(
            "The group was changed by someone else (you sent version {sent}, current is {current}). Reload and retry."
        ),
    )
    .with_details(vec![FieldError {
        location: FieldLocation::Body,
        field: "version".into(),
        message: format!("Current version is {current}"),
        code: "stale".into(),
    }]))
}

/// A count of business services, told only to a caller who may view their class.
async fn scoped(conn: &mut PgConnection, ctx: &RequestContext, n: i64) -> Result<Count, AppError> {
    let class = service_owners::service_class_id(conn).await?;
    Ok(Count::scoped(ctx, &[class], n))
}

pub async fn list(pool: &PgPool, q: &UserGroupList) -> Result<Page<UserGroup>, AppError> {
    let filter = |w: &mut Where<'_>| {
        if let Some(text) = &q.q {
            let p = like_pattern(text);
            w.and().push("(g.name ILIKE ").push_bind(p.clone()).push(" OR g.description ILIKE ").push_bind(p).push(")");
        }
    };
    let column = match q.sort.field.as_str() {
        "memberCount" => "member_count",
        _ => "lower(g.name)",
    };
    let order = format!("{column} {}, lower(g.name), g.id", q.sort.dir());
    let (data, total) = crud::select_page_counted::<UserGroup>(
        &mut *pool.acquire().await?,
        "user_groups g",
        "user_groups g",
        COLUMNS,
        &filter,
        &order,
        q.limit,
        q.offset,
    )
    .await?;
    Ok(Page { data, page: q.page_meta(total) })
}

pub async fn get(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<UserGroupDetail, AppError> {
    let mut conn = pool.acquire().await?;
    let g = load(&mut conn, id, false).await?;
    let owned = service_owners::count_owned(&mut conn, Principal::Group(id)).await?;
    let owned_service_count = scoped(&mut conn, ctx, owned).await?;
    Ok(UserGroupDetail {
        id: g.id,
        name: g.name,
        description: g.description,
        member_count: g.member_count,
        version: g.version,
        owned_service_count,
        created_at: g.created_at,
        updated_at: g.updated_at,
    })
}

pub async fn create(pool: &PgPool, ctx: &RequestContext, b: &UserGroupCreate) -> Result<UserGroup, AppError> {
    let mut tx = pool.begin().await?;
    let id: Uuid = sqlx::query_scalar("INSERT INTO user_groups (name, description) VALUES ($1, $2) RETURNING id")
        .bind(&b.name)
        .bind(&b.description)
        .fetch_one(&mut *tx)
        .await?;
    let dto = load(&mut tx, id, false).await?;
    let entry = AuditEntry {
        action: AuditAction::Create,
        entity_type: TABLE,
        entity_id: id,
        old_value: None,
        new_value: Some(crud::json(&dto)),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    tx.commit().await?;
    Ok(dto)
}

pub async fn update(pool: &PgPool, ctx: &RequestContext, id: Uuid, b: &UserGroupUpdate) -> Result<UserGroup, AppError> {
    let mut tx = pool.begin().await?;
    let before = load(&mut tx, id, true).await?;
    stale(b.version, before.version)?;
    let mut columns = b.columns();
    columns.0.push(("version", crud::Val::Int(Some(before.version + 1))));
    let _: (Uuid,) = crud::update_row(&mut tx, TABLE, "id", id, columns).await?;
    let dto = load(&mut tx, id, false).await?;
    let entry = AuditEntry {
        action: AuditAction::Update,
        entity_type: TABLE,
        entity_id: id,
        old_value: Some(crud::json(&before)),
        new_value: Some(crud::json(&dto)),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    tx.commit().await?;
    Ok(dto)
}

pub async fn remove(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<OwnerRemoval, AppError> {
    let mut tx = pool.begin().await?;
    let before = load(&mut tx, id, true).await?;
    let cleanup = OwnerCleanup::prepare(&mut tx, Principal::Group(id)).await?;
    crud::delete_row(&mut tx, TABLE, id).await?;
    let entry = AuditEntry {
        action: AuditAction::Delete,
        entity_type: TABLE,
        entity_id: id,
        old_value: Some(crud::json(&before)),
        new_value: None,
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    let affected = cleanup.finish(&mut tx, ctx).await?;
    let affected_services = scoped(&mut tx, ctx, affected).await?;
    tx.commit().await?;
    Ok(OwnerRemoval { affected_services })
}

pub async fn members(pool: &PgPool, id: Uuid, q: &UserGroupMemberList) -> Result<Page<UserGroupMember>, AppError> {
    let mut conn = pool.acquire().await?;
    load(&mut conn, id, false).await?;
    let filter = |w: &mut Where<'_>| {
        w.and().push("m.group_id = ").push_bind(id);
        if let Some(text) = &q.q {
            let p = like_pattern(text);
            w.and()
                .push("(u.username ILIKE ")
                .push_bind(p.clone())
                .push(" OR u.display_name ILIKE ")
                .push_bind(p)
                .push(")");
        }
    };
    let column = match q.sort.field.as_str() {
        "username" => "lower(u.username)",
        "addedAt" => "m.created_at",
        _ => "lower(u.display_name)",
    };
    let order = format!("{column} {}, lower(u.username), u.id", q.sort.dir());
    let (data, total) = crud::select_page::<UserGroupMember>(
        &mut conn,
        "user_group_members m JOIN users u ON u.id = m.user_id",
        "u.id, u.username, u.display_name, u.is_active, m.created_at AS added_at",
        &filter,
        &order,
        q.limit,
        q.offset,
    )
    .await?;
    Ok(Page { data, page: q.page_meta(total) })
}

/// `[{id, name}]` of these users, in the given order (audit values).
async fn named(conn: &mut PgConnection, ids: &[Uuid]) -> Result<Vec<Value>, AppError> {
    let names: HashMap<Uuid, String> =
        sqlx::query_as::<_, (Uuid, String)>("SELECT id, display_name FROM users WHERE id = ANY($1)")
            .bind(ids)
            .fetch_all(&mut *conn)
            .await?
            .into_iter()
            .collect();
    Ok(ids.iter().map(|id| json!({ "id": id, "name": names.get(id) })).collect())
}

pub async fn replace_members(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    b: &UserGroupMembersReplace,
) -> Result<UserGroup, AppError> {
    let mut tx = pool.begin().await?;
    let before = load(&mut tx, id, true).await?;
    stale(b.version, before.version)?;

    let found: HashSet<Uuid> = sqlx::query_scalar("SELECT id FROM users WHERE id = ANY($1)")
        .bind(&b.user_ids)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .collect();
    let missing: Vec<FieldError> = b
        .user_ids
        .iter()
        .enumerate()
        .filter(|(_, u)| !found.contains(u))
        .map(|(i, _)| FieldError {
            location: FieldLocation::Body,
            field: format!("userIds[{i}]"),
            message: "User does not exist".into(),
            code: "not_found".into(),
        })
        .collect();
    if !missing.is_empty() {
        return Err(AppError::validation(missing));
    }

    let current: Vec<Uuid> = sqlx::query_scalar("SELECT user_id FROM user_group_members WHERE group_id = $1")
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
    let current_set: HashSet<Uuid> = current.iter().copied().collect();
    let wanted: HashSet<Uuid> = b.user_ids.iter().copied().collect();
    let added: Vec<Uuid> = b.user_ids.iter().copied().filter(|u| !current_set.contains(u)).collect();
    let mut removed: Vec<Uuid> = current.into_iter().filter(|u| !wanted.contains(u)).collect();
    removed.sort();
    if added.is_empty() && removed.is_empty() {
        tx.commit().await?;
        return Ok(before);
    }

    sqlx::query("DELETE FROM user_group_members WHERE group_id = $1 AND user_id = ANY($2)")
        .bind(id)
        .bind(&removed)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO user_group_members (group_id, user_id) SELECT $1, u FROM unnest($2::uuid[]) AS u ON CONFLICT DO NOTHING",
    )
    .bind(id)
    .bind(&added)
    .execute(&mut *tx)
    .await?;
    // The member list is part of the group: a change bumps its version.
    sqlx::query("UPDATE user_groups SET version = version + 1, updated_at = now() WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let dto = load(&mut tx, id, false).await?;
    let diff = json!({ "added": named(&mut tx, &added).await?, "removed": named(&mut tx, &removed).await? });
    let entry = AuditEntry {
        action: AuditAction::Update,
        entity_type: TABLE,
        entity_id: id,
        old_value: Some(json!({ "version": before.version, "memberCount": before.member_count })),
        new_value: Some(json!({ "version": dto.version, "memberCount": dto.member_count, "members": diff })),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    tx.commit().await?;
    Ok(dto)
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

const TAG: &str = "User groups";
const BASE: &str = "/api/v1/admin/groups";
const BY_ID: &str = "/api/v1/admin/groups/{id}";
const MEMBERS: &str = "/api/v1/admin/groups/{id}/members";

pub fn routes() -> Vec<Route> {
    let manage = GlobalPermission::UsersManage;
    vec![
        route(Method::GET, BASE, "listUserGroups")
            .tag(TAG)
            .summary("List user groups (paginated, searchable, sortable by name or member count)")
            .requires(manage)
            .handle(|api, In(NoPath, Query(q), NoBody): In<NoPath, Query<UserGroupList>, NoBody>| async move {
                Ok(Json(list(&api.pool, &q).await?))
            }),
        route(Method::GET, BY_ID, "getUserGroup")
            .tag(TAG)
            .summary("Get one user group with the number of business services it owns")
            .description(
                "`ownedServiceCount` is what a delete would remove the group from. It is null when the caller may \
                 not view the business service class: the count spans services they could not open.",
            )
            .requires(manage)
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(get(&api.pool, &api.ctx, id).await?))
            }),
        route(Method::POST, BASE, "createUserGroup")
            .tag(TAG)
            .summary("Create a user group")
            .description("409 CONFLICT on `name` when another group has the same name (compared regardless of case).")
            .status(StatusCode::CREATED)
            .requires(manage)
            .session_only()
            .errors(&[ErrorCode::Conflict])
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<UserGroupCreate>>| async move {
                Ok(Json(create(&api.pool, &api.ctx, &b).await?))
            }),
        route(Method::PATCH, BY_ID, "updateUserGroup")
            .tag(TAG)
            .summary("Rename a user group or change its description")
            .description(
                "Send the `version` you loaded: 409 VERSION_CONFLICT if someone changed the group or its members in \
                 between. 409 CONFLICT on `name` when another group has the same name (regardless of case).",
            )
            .requires(manage)
            .session_only()
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict, ErrorCode::VersionConflict])
            .handle(|api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<UserGroupUpdate>>| async move {
                Ok(Json(update(&api.pool, &api.ctx, id, &b).await?))
            }),
        route(Method::DELETE, BY_ID, "deleteUserGroup")
            .tag(TAG)
            .summary("Delete a user group; it is removed as owner from every business service it owns")
            .description(
                "Allowed while the group owns business services: each loses the group as owner and gets an `update` \
                 row in its history. `affectedServices` counts them (null when the caller may not view the business \
                 service class). Ask first with getUserGroup's `ownedServiceCount`.",
            )
            .requires(manage)
            .session_only()
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(remove(&api.pool, &api.ctx, id).await?))
            }),
        route(Method::GET, MEMBERS, "listUserGroupMembers")
            .tag(TAG)
            .summary("List the users in a group (paginated)")
            .requires(manage)
            .errors(&[ErrorCode::NotFound])
            .handle(
                |api, In(IdPath(id), Query(q), NoBody): In<IdPath, Query<UserGroupMemberList>, NoBody>| async move {
                    Ok(Json(members(&api.pool, id, &q).await?))
                },
            ),
        route(Method::PUT, MEMBERS, "replaceUserGroupMembers")
            .tag(TAG)
            .summary("Replace the users in a group (at most 1000)")
            .description(
                "`userIds` is the complete new member list. Send the group's `version`: 409 VERSION_CONFLICT if \
                 someone changed it in between. An unknown user is 400 VALIDATION_ERROR `not_found` on `userIds[i]`; disabled users \
                 may be members. A change bumps the group's version and writes one `update` audit row whose \
                 `newValue.members` lists who was added and removed.",
            )
            .requires(manage)
            .session_only()
            .errors(&[ErrorCode::NotFound, ErrorCode::VersionConflict])
            .handle(
                |api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<UserGroupMembersReplace>>| async move {
                    Ok(Json(replace_members(&api.pool, &api.ctx, id, &b).await?))
                },
            ),
    ]
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use uuid::Uuid;

    use crate::db::scratch;
    use crate::modules::api_tokens::tests::{Creds, app, call, code, session_of};

    async fn login(app: &axum::Router, username: &str, password: &str) -> Creds {
        let body = json!({ "username": username, "password": password });
        let (status, me, headers) = call(app, "POST", "/api/v1/auth/login", &Creds::default(), Some(body)).await;
        assert_eq!(status, 200, "{me}");
        session_of(&me, &headers)
    }

    fn details(v: &Value) -> Vec<(String, String)> {
        v["error"]["details"]
            .as_array()
            .map(|d| {
                d.iter()
                    .map(|e| {
                        (e["field"].as_str().unwrap_or("").to_owned(), e["code"].as_str().unwrap_or("").to_owned())
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// SHAA-927 §4.9, §1.4, §7.1 "Owners" (delete cases), §7.2 oracle 10:
    /// group CRUD with versions and case-insensitive names, member
    /// replacement, and deleting a group or a user that owns services: the
    /// owner rows go, one `update` row per service (actor: the deleting
    /// admin), and the counts are withheld from an admin who may not view the
    /// business service class.
    #[tokio::test]
    async fn groups_are_managed_and_their_deletion_cleans_up_owners() {
        let Some(db) = scratch::database("groups_are_managed").await else { return };
        let app = app(db.pool.clone());
        let pool = &db.pool;

        // Random per test run, so no hard-coded credential reaches the hasher or verifier.
        let password = format!("test passphrase {}", Uuid::new_v4());
        let setup = json!({ "username": "admin", "email": "admin@example.test", "displayName": "Admin", "password": password,
            "setupToken": crate::auth::setup_token::TEST_TOKEN });
        let (status, me, headers) = call(&app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
        assert_eq!(status, 201, "{me}");
        let admin = session_of(&me, &headers);
        let admin_id = me["user"]["id"].as_str().unwrap().to_owned();

        // Create; the name is unique regardless of case.
        let body = json!({ "name": "DBA team", "description": "Database administrators" });
        let (status, g, _) = call(&app, "POST", "/api/v1/admin/groups", &admin, Some(body)).await;
        assert_eq!(status, 201, "{g}");
        assert_eq!((g["memberCount"].as_i64(), g["version"].as_i64()), (Some(0), Some(1)));
        let group = g["id"].as_str().unwrap().to_owned();
        let by_id = format!("/api/v1/admin/groups/{group}");
        let (status, v, _) =
            call(&app, "POST", "/api/v1/admin/groups", &admin, Some(json!({ "name": "dba TEAM" }))).await;
        assert_eq!((status, code(&v)), (409, "CONFLICT"), "{v}");
        assert_eq!(details(&v), vec![("name".to_owned(), "unique".to_owned())]);
        let (status, v, _) = call(&app, "POST", "/api/v1/admin/groups", &admin, Some(json!({ "name": "  " }))).await;
        assert_eq!(status, 400, "{v}");
        let (status, other, _) =
            call(&app, "POST", "/api/v1/admin/groups", &admin, Some(json!({ "name": "Network" }))).await;
        assert_eq!(status, 201, "{other}");

        // PATCH needs the current version; a taken name is a conflict.
        let (status, v, _) = call(&app, "PATCH", &by_id, &admin, Some(json!({ "version": 7, "name": "DBAs" }))).await;
        assert_eq!((status, code(&v)), (409, "VERSION_CONFLICT"), "{v}");
        let (status, v, _) =
            call(&app, "PATCH", &by_id, &admin, Some(json!({ "version": 1, "name": "NETWORK" }))).await;
        assert_eq!((status, code(&v)), (409, "CONFLICT"), "{v}");
        let (status, v, _) = call(&app, "PATCH", &by_id, &admin, Some(json!({ "version": 1 }))).await;
        assert_eq!(status, 400, "nothing to change: {v}");
        let (status, v, _) =
            call(&app, "PATCH", &by_id, &admin, Some(json!({ "version": 1, "name": "DBAs", "description": null })))
                .await;
        assert_eq!(status, 200, "{v}");
        assert_eq!(
            (v["name"].as_str(), v["description"].is_null(), v["version"].as_i64()),
            (Some("DBAs"), true, Some(2))
        );

        // Members: replaced as a whole, with the version; unknown users are named by index.
        let mut users = Vec::new();
        for name in ["alice", "bob"] {
            let body = json!({ "username": name, "email": format!("{name}@example.test"), "displayName": name.to_uppercase(), "password": password });
            let (status, u, _) = call(&app, "POST", "/api/v1/admin/users", &admin, Some(body)).await;
            assert_eq!(status, 201, "{u}");
            users.push(u["id"].as_str().unwrap().to_owned());
        }
        let (alice, bob) = (users[0].clone(), users[1].clone());
        let members = format!("{by_id}/members");
        let ghost = Uuid::new_v4().to_string();
        let (status, v, _) =
            call(&app, "PUT", &members, &admin, Some(json!({ "version": 2, "userIds": [alice, ghost] }))).await;
        assert_eq!(status, 400, "{v}");
        assert_eq!(details(&v), vec![("userIds[1]".to_owned(), "not_found".to_owned())]);
        let too_many: Vec<String> = (0..1001).map(|_| Uuid::new_v4().to_string()).collect();
        let (status, v, _) =
            call(&app, "PUT", &members, &admin, Some(json!({ "version": 2, "userIds": too_many }))).await;
        assert_eq!(status, 400, "{v}");
        let (status, v, _) =
            call(&app, "PUT", &members, &admin, Some(json!({ "version": 1, "userIds": [alice] }))).await;
        assert_eq!((status, code(&v)), (409, "VERSION_CONFLICT"), "{v}");
        let (status, v, _) =
            call(&app, "PUT", &members, &admin, Some(json!({ "version": 2, "userIds": [alice, bob] }))).await;
        assert_eq!(status, 200, "{v}");
        assert_eq!((v["memberCount"].as_i64(), v["version"].as_i64()), (Some(2), Some(3)));
        let (status, v, _) = call(&app, "GET", &format!("{members}?sort=-displayName"), &admin, None).await;
        assert_eq!(status, 200, "{v}");
        let names: Vec<&str> = v["data"].as_array().unwrap().iter().map(|m| m["username"].as_str().unwrap()).collect();
        assert_eq!((names, v["page"]["total"].as_i64()), (vec!["bob", "alice"], Some(2)));
        // The same set again is no change: no new version.
        let (status, v, _) =
            call(&app, "PUT", &members, &admin, Some(json!({ "version": 3, "userIds": [bob, alice] }))).await;
        assert_eq!((status, v["version"].as_i64()), (200, Some(3)), "{v}");
        let (status, v, _) = call(&app, "GET", "/api/v1/admin/groups?sort=-memberCount", &admin, None).await;
        assert_eq!(status, 200, "{v}");
        assert_eq!(v["data"][0]["name"].as_str(), Some("DBAs"));

        // Three services owned through the group (one of them soft-deleted), two by users.
        let class: Uuid = sqlx::query_scalar("SELECT id FROM ci_classes WHERE system_role = 'business_service'")
            .fetch_one(pool)
            .await
            .unwrap();
        let mut services = Vec::new();
        for label in ["Online shop", "Payroll", "Old intranet"] {
            let id: Uuid =
                sqlx::query_scalar("INSERT INTO configuration_items (class_id, label) VALUES ($1, $2) RETURNING id")
                    .bind(class)
                    .bind(label)
                    .fetch_one(pool)
                    .await
                    .unwrap();
            services.push(id);
        }
        sqlx::query("UPDATE configuration_items SET deleted_at = now() WHERE id = $1")
            .bind(services[2])
            .execute(pool)
            .await
            .unwrap();
        let group_id: Uuid = group.parse().unwrap();
        let (alice_id, bob_id): (Uuid, Uuid) = (alice.parse().unwrap(), bob.parse().unwrap());
        for (service, role, user, grp, pos) in [
            (services[0], "technical", None, Some(group_id), 0),
            (services[0], "business", Some(alice_id), None, 0),
            (services[1], "technical", Some(bob_id), None, 0),
            (services[1], "business", None, Some(group_id), 0),
            (services[1], "business", Some(alice_id), None, 1),
            (services[2], "technical", None, Some(group_id), 0),
        ] {
            sqlx::query(
                "INSERT INTO business_service_owners (service_ci_id, role, user_id, group_id, position)
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(service)
            .bind(role)
            .bind(user)
            .bind(grp)
            .bind(pos)
            .execute(pool)
            .await
            .unwrap();
        }
        let (status, v, _) = call(&app, "GET", &by_id, &admin, None).await;
        assert_eq!((status, v["ownedServiceCount"].as_i64()), (200, Some(3)), "{v}");

        // A user manager who may not view the service class gets null counts (§7.2 oracle 10).
        let managers: Uuid =
            sqlx::query_scalar("INSERT INTO permission_profiles (name) VALUES ('User managers') RETURNING id")
                .fetch_one(pool)
                .await
                .unwrap();
        sqlx::query(
            "INSERT INTO permission_profile_global_permissions (profile_id, permission) VALUES ($1, 'users.manage')",
        )
        .bind(managers)
        .execute(pool)
        .await
        .unwrap();
        let body = json!({ "username": "carol", "email": "carol@example.test", "displayName": "Carol", "password": password,
            "profileIds": [managers] });
        let (status, v, _) = call(&app, "POST", "/api/v1/admin/users", &admin, Some(body)).await;
        assert_eq!(status, 201, "{v}");
        let carol = login(&app, "carol", &password).await;
        let (status, v, _) = call(&app, "GET", &by_id, &carol, None).await;
        assert_eq!(status, 200, "{v}");
        assert!(v["ownedServiceCount"].is_null(), "{v}");
        assert_eq!(v["memberCount"].as_i64(), Some(2));

        let owners = |service: Uuid| async move {
            sqlx::query_as::<_, (String, Option<Uuid>, Option<Uuid>)>(
                "SELECT role, user_id, group_id FROM business_service_owners WHERE service_ci_id = $1
                 ORDER BY role, position",
            )
            .bind(service)
            .fetch_all(pool)
            .await
            .unwrap()
        };
        let audits = |service: Uuid| async move {
            sqlx::query_as::<_, (String, Option<String>, Value, Value)>(
                "SELECT action, actor_id, old_value, new_value FROM audit_log
                 WHERE entity_type = 'configuration_items' AND entity_id = $1 ORDER BY id",
            )
            .bind(service)
            .fetch_all(pool)
            .await
            .unwrap()
        };

        // Deleting the group: allowed, owner rows go, one update row per service, soft-deleted one included.
        let (status, v, _) = call(&app, "DELETE", &by_id, &admin, None).await;
        assert_eq!(status, 200, "{v}");
        assert_eq!(v, json!({ "affectedServices": 3 }));
        assert_eq!(owners(services[0]).await, vec![("business".to_owned(), Some(alice_id), None)]);
        assert!(owners(services[2]).await.is_empty());
        for s in &services {
            let rows = audits(*s).await;
            assert_eq!(rows.len(), 1, "{rows:?}");
            let (action, actor, old, new) = &rows[0];
            assert_eq!((action.as_str(), actor.as_deref()), ("update", Some(admin_id.as_str())));
            assert!(old.to_string().contains(&group), "{old}");
            assert!(!new.to_string().contains(&group), "{new}");
        }
        let (_, _, old, new) = &audits(services[1]).await[0];
        assert_eq!(
            old["owners"]["business"],
            json!([{ "kind": "group", "id": group, "name": "DBAs" }, { "kind": "user", "id": alice, "name": "ALICE" }])
        );
        assert_eq!(new["owners"]["business"], json!([{ "kind": "user", "id": alice, "name": "ALICE" }]));
        assert_eq!(new["owners"]["technical"], json!([{ "kind": "user", "id": bob, "name": "BOB" }]));
        let (status, _, _) = call(&app, "GET", &by_id, &admin, None).await;
        assert_eq!(status, 404);

        // Hard-deleting a user that owns services: same clean-up, the count in the response.
        let (status, v, _) = call(&app, "DELETE", &format!("/api/v1/admin/users/{alice}"), &admin, None).await;
        assert_eq!(status, 200, "{v}");
        assert_eq!(v, json!({ "affectedServices": 2 }));
        assert!(owners(services[0]).await.is_empty());
        assert_eq!(audits(services[0]).await.len(), 2);
        // ... withheld from the user manager without the class.
        let (status, v, _) = call(&app, "DELETE", &format!("/api/v1/admin/users/{bob}"), &carol, None).await;
        assert_eq!(status, 200, "{v}");
        assert_eq!(v, json!({ "affectedServices": null }));
        assert!(owners(services[1]).await.is_empty());
        let rows = audits(services[1]).await;
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert_eq!(
            rows[2].1.as_deref(),
            sqlx::query_scalar::<_, String>("SELECT id::text FROM users WHERE username = 'carol'")
                .fetch_one(pool)
                .await
                .ok()
                .as_deref()
        );
        // A user who owns nothing: 0, and no service rows.
        let (status, v, _) =
            call(&app, "DELETE", &format!("/api/v1/admin/groups/{}", other["id"].as_str().unwrap()), &admin, None)
                .await;
        assert_eq!((status, v), (200, json!({ "affectedServices": 0 })));

        // The audit log filters on the new entity type; member diffs name who changed.
        let (status, v, _) =
            call(&app, "GET", "/api/v1/audit-log?entityType=user_groups&sort=occurredAt&limit=50", &admin, None).await;
        assert_eq!(status, 200, "{v}");
        let rows = v["data"].as_array().unwrap();
        let actions: Vec<&str> =
            rows.iter().filter(|r| r["entityId"] == json!(group)).map(|r| r["action"].as_str().unwrap()).collect();
        assert_eq!(actions, vec!["create", "update", "update", "delete"], "{v}");
        let diff = rows.iter().find(|r| r["newValue"]["members"].is_object()).unwrap();
        assert_eq!(
            diff["newValue"]["members"],
            json!({ "added": [{ "id": alice, "name": "ALICE" }, { "id": bob, "name": "BOB" }], "removed": [] })
        );
        assert_eq!(diff["oldValue"], json!({ "version": 2, "memberCount": 0 }));

        db.drop().await;
    }
}
