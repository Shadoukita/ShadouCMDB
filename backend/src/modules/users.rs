//! Administration > Users: local accounts, their passwords and profiles, and
//! the accounts identity providers created (see [`super::sso`]).
//!
//! Disabling (`isActive: false`) is the normal way to remove access; it ends
//! the user's sessions at once. Deleting is allowed too (the audit log keeps
//! the user's id and name as text). Nobody can disable or delete themselves,
//! the database refuses any change that leaves no active Administrator, and a
//! non-administrator user manager can only act on accounts, and assign
//! profiles, whose permissions they hold themselves.

use std::collections::HashSet;

use axum::http::{Method, StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use utoipa::openapi::schema::{ArrayBuilder, ObjectBuilder, Schema, Type};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::{api_tokens, profiles};
use crate::api::context::{RequestContext, forbidden};
use crate::api::route::{Body, Check, IdPath, In, Json, NoBody, NoContent, NoPath, NoQuery, Query, Route, route};
use crate::api::schemas::{
    self, Page, Paged, QueryBool, Sort, USERNAME_PATTERN, UuidList, like_pattern, name_schema, trimmed, ts, ts_opt,
};
use crate::auth::events::{self, RevokeReason};
use crate::auth::password;
use crate::auth::permissions::GlobalPermission;
use crate::data::auth::{self as data, UserRow};
use crate::data::crud::{self, AuditAction, AuditEntry, ColumnSet, Where};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::modules::lookups::email_schema;
use crate::modules::simple_resource::non_empty;
use crate::paged;

const TABLE: &str = "users";

// ---------------------------------------------------------------------------
// Schemas
// ---------------------------------------------------------------------------

/// A profile a user holds
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileRef {
    pub id: Uuid,
    pub name: String,
    pub is_builtin: bool,
}

/// The identity provider an account signs in through
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdentityProviderRef {
    pub id: Uuid,
    pub name: String,
    /// oidc or ldap
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct User {
    pub id: Uuid,
    /// Sign-in name, unique regardless of case
    pub username: String,
    pub display_name: String,
    #[schema(required = true)]
    pub email: Option<String>,
    /// Disabled users cannot sign in and their sessions end
    pub is_active: bool,
    /// Holds the built-in Administrator profile
    pub is_administrator: bool,
    /// Has set up two-factor authentication (an authenticator app)
    pub mfa_enabled: bool,
    /// Null for a local account (username and password). Otherwise the account
    /// was created by this provider and signs in only through it: it has no
    /// password here, and its name, e-mail and profiles are set from the
    /// provider at every sign-in.
    #[schema(required = true)]
    pub identity_provider: Option<IdentityProviderRef>,
    pub profiles: Vec<ProfileRef>,
    #[serde(serialize_with = "ts::serialize")]
    pub password_changed_at: DateTime<Utc>,
    #[serde(serialize_with = "ts_opt::serialize")]
    #[schema(required = true)]
    pub last_login_at: Option<DateTime<Utc>>,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
}

pub fn username_schema() -> Schema {
    ObjectBuilder::new().schema_type(Type::String).pattern(Some(USERNAME_PATTERN)).into()
}

pub fn password_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .min_length(Some(password::MIN_LENGTH))
        .max_length(Some(password::MAX_LENGTH))
        .description(Some("At least 12 characters"))
        .into()
}

fn profile_ids_schema() -> Schema {
    ArrayBuilder::new()
        .items(schemas::uuid_builder())
        .max_items(Some(100))
        .unique_items(true)
        .description(Some("Permission profiles the user holds (replaces the current set)"))
        .into()
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserCreate {
    #[schema(schema_with = username_schema)]
    pub username: String,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    pub display_name: String,
    #[schema(schema_with = email_schema)]
    #[serde(default)]
    pub email: Option<String>,
    #[schema(schema_with = password_schema)]
    pub password: String,
    /// Default true
    #[schema(nullable = false)]
    pub is_active: Option<bool>,
    #[schema(schema_with = profile_ids_schema)]
    #[serde(default)]
    pub profile_ids: Vec<Uuid>,
}

impl Check for UserCreate {
    fn check(&self) -> Vec<FieldError> {
        password_problem("password", &self.password)
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserUpdate {
    #[schema(schema_with = username_schema)]
    username: Option<String>,
    #[schema(schema_with = name_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    display_name: Option<String>,
    #[schema(schema_with = email_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    email: Option<Option<String>>,
    /// false disables the account and ends its sessions
    #[schema(nullable = false)]
    is_active: Option<bool>,
    #[schema(schema_with = profile_ids_schema)]
    profile_ids: Option<Vec<Uuid>>,
}

impl UserUpdate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("username", self.username.clone())
            .opt("display_name", self.display_name.clone())
            .opt("email", self.email.clone())
            .opt("is_active", self.is_active);
        c
    }
}

impl Check for UserUpdate {
    fn check(&self) -> Vec<FieldError> {
        if self.profile_ids.is_some() { Vec::new() } else { non_empty(&self.columns()) }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PasswordReset {
    /// The new password
    #[schema(schema_with = password_schema)]
    password: String,
}

impl Check for PasswordReset {
    fn check(&self) -> Vec<FieldError> {
        password_problem("password", &self.password)
    }
}

pub fn password_problem(field: &str, password: &str) -> Vec<FieldError> {
    password::policy_error(password)
        .map(|message| {
            vec![FieldError {
                location: FieldLocation::Body,
                field: field.into(),
                message,
                code: "password_policy".into(),
            }]
        })
        .unwrap_or_default()
}

fn sort_schema() -> Schema {
    schemas::sort_schema(&["username", "displayName", "createdAt", "lastLoginAt"], "username")
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct UserList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    offset: i64,
    /// Matches username, display name and email
    #[param(schema_with = schemas::search_schema)]
    q: Option<String>,
    #[param(required = false, schema_with = sort_schema)]
    sort: Sort,
    #[param(inline)]
    is_active: Option<QueryBool>,
    /// Users holding any of these profiles
    #[param(schema_with = schemas::uuid_list_schema)]
    profile_id: Option<UuidList>,
}
paged!(UserList);

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

async fn dtos(conn: &mut PgConnection, rows: Vec<UserRow>) -> Result<Vec<User>, AppError> {
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let held = data::profiles_of_users(conn, &ids).await?;
    let mfa = crate::data::mfa::enabled_among(conn, &ids).await?;
    let provider_ids: Vec<Uuid> = rows.iter().filter_map(|r| r.identity_provider_id).collect();
    let providers: Vec<(Uuid, String, String)> = if provider_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as("SELECT id, name, kind FROM identity_providers WHERE id = ANY($1)")
            .bind(&provider_ids)
            .fetch_all(&mut *conn)
            .await?
    };
    Ok(rows
        .into_iter()
        .map(|r| {
            let profiles: Vec<ProfileRef> = held
                .iter()
                .filter(|p| p.user_id == r.id)
                .map(|p| ProfileRef { id: p.id, name: p.name.clone(), is_builtin: p.is_builtin })
                .collect();
            User {
                id: r.id,
                username: r.username,
                display_name: r.display_name,
                email: r.email,
                is_active: r.is_active,
                is_administrator: profiles.iter().any(|p| p.is_builtin),
                mfa_enabled: mfa.contains(&r.id),
                identity_provider: r.identity_provider_id.and_then(|id| {
                    providers.iter().find(|p| p.0 == id).map(|(id, name, kind)| IdentityProviderRef {
                        id: *id,
                        name: name.clone(),
                        kind: kind.clone(),
                    })
                }),
                profiles,
                password_changed_at: r.password_changed_at,
                last_login_at: r.last_login_at,
                created_at: r.created_at,
                updated_at: r.updated_at,
            }
        })
        .collect())
}

pub async fn load(conn: &mut PgConnection, id: Uuid) -> Result<User, AppError> {
    let row = data::get_user(conn, id, false).await?.ok_or_else(|| AppError::missing("User", id))?;
    Ok(dtos(conn, vec![row]).await?.remove(0))
}

async fn lock(conn: &mut PgConnection, id: Uuid) -> Result<User, AppError> {
    let row = data::get_user(conn, id, true).await?.ok_or_else(|| AppError::missing("User", id))?;
    Ok(dtos(conn, vec![row]).await?.remove(0))
}

/// A user manager can only act on accounts whose permissions they hold themselves.
pub(crate) async fn must_cover_user(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    user_id: Uuid,
) -> Result<(), AppError> {
    let Some(me) = ctx.principal() else { return Ok(()) };
    if me.permissions.administrator {
        return Ok(());
    }
    let theirs = data::load_permissions(conn, user_id).await?;
    if me.permissions.covers(&theirs) {
        Ok(())
    } else {
        Err(forbidden("This user has permissions you do not hold yourself"))
    }
}

/// Checks that the profiles exist and that the caller may hand them out.
async fn check_profiles(conn: &mut PgConnection, ctx: &RequestContext, ids: &[Uuid]) -> Result<(), AppError> {
    let found: HashSet<Uuid> = data::existing_profiles(conn, ids).await?.into_iter().collect();
    let missing: Vec<FieldError> = ids
        .iter()
        .enumerate()
        .filter(|(_, id)| !found.contains(id))
        .map(|(i, _)| FieldError {
            location: FieldLocation::Body,
            field: format!("profileIds.{i}"),
            message: "Permission profile does not exist".into(),
            code: "not_found".into(),
        })
        .collect();
    if !missing.is_empty() {
        return Err(AppError::validation(missing));
    }
    if let Some(me) = ctx.principal()
        && !me.permissions.covers(&profiles::union_of(conn, ids).await?)
    {
        return Err(forbidden("You can only assign profiles whose permissions you hold yourself"));
    }
    Ok(())
}

fn not_yourself(ctx: &RequestContext, id: Uuid, what: &str) -> Result<(), AppError> {
    if ctx.principal().is_some_and(|me| me.user_id == id) {
        return Err(AppError::conflict(format!("You cannot {what} your own account")));
    }
    Ok(())
}

pub async fn list(pool: &PgPool, q: &UserList) -> Result<Page<User>, AppError> {
    let filter = |w: &mut Where<'_>| {
        if let Some(text) = &q.q {
            let p = like_pattern(text);
            w.and()
                .push("(username ILIKE ")
                .push_bind(p.clone())
                .push(" OR display_name ILIKE ")
                .push_bind(p.clone())
                .push(" OR email ILIKE ")
                .push_bind(p)
                .push(")");
        }
        if let Some(active) = q.is_active {
            w.and().push("is_active = ").push_bind(bool::from(active));
        }
        if let Some(ids) = &q.profile_id {
            w.and()
                .push("id IN (SELECT user_id FROM user_permission_profiles WHERE profile_id = ANY(")
                .push_bind(ids.0.clone())
                .push("))");
        }
    };
    let column = match q.sort.field.as_str() {
        "displayName" => "lower(display_name)",
        "createdAt" => "created_at",
        "lastLoginAt" => "last_login_at",
        _ => "lower(username)",
    };
    let order = format!("{column} {} NULLS LAST, id", q.sort.dir());
    let (rows, total) =
        crud::select_page::<UserRow>(pool, "users", data::USER_COLUMNS, &filter, &order, q.limit, q.offset).await?;
    let data = dtos(&mut *pool.acquire().await?, rows).await?;
    Ok(Page { data, page: q.page_meta(total) })
}

pub async fn get(pool: &PgPool, id: Uuid) -> Result<User, AppError> {
    load(&mut *pool.acquire().await?, id).await
}

/// Inserts a user in the caller's transaction (shared by the API, first-run setup and the CLI).
pub async fn create_in(conn: &mut PgConnection, ctx: &RequestContext, b: &UserCreate) -> Result<User, AppError> {
    check_profiles(conn, ctx, &b.profile_ids).await?;
    let hash = password::hash(&b.password).await?;
    let id = data::insert_user(
        conn,
        &data::NewUser {
            username: &b.username,
            display_name: &b.display_name,
            email: b.email.as_deref(),
            password_hash: &hash,
            is_active: b.is_active.unwrap_or(true),
        },
    )
    .await?;
    data::set_user_profiles(conn, id, &b.profile_ids).await?;
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

pub async fn create(pool: &PgPool, ctx: &RequestContext, b: &UserCreate) -> Result<User, AppError> {
    let mut tx = pool.begin().await?;
    let dto = create_in(&mut tx, ctx, b).await?;
    tx.commit().await?;
    Ok(dto)
}

pub async fn update(pool: &PgPool, ctx: &RequestContext, id: Uuid, b: &UserUpdate) -> Result<User, AppError> {
    if b.is_active == Some(false) {
        not_yourself(ctx, id, "disable")?;
    }
    let mut tx = pool.begin().await?;
    let before = lock(&mut tx, id).await?;
    must_cover_user(&mut tx, ctx, id).await?;
    if let Some(ids) = &b.profile_ids {
        check_profiles(&mut tx, ctx, ids).await?;
        data::set_user_profiles(&mut tx, id, ids).await?;
    }
    let columns = b.columns();
    if !columns.is_empty() {
        crud::update_row::<UserRow>(&mut tx, TABLE, data::USER_COLUMNS, id, columns).await?;
    }
    if before.is_active && b.is_active == Some(false) {
        let ended = data::delete_user_sessions(&mut tx, id, None).await?;
        events::revoked(&mut tx, ctx, &ended, RevokeReason::UserDisabled).await?;
    }
    let dto = load(&mut tx, id).await?;
    let entry = AuditEntry {
        action: AuditAction::Update,
        entity_type: TABLE,
        entity_id: id,
        old_value: Some(crud::json(&before)),
        new_value: Some(crud::json(&dto)),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    // The last-Administrator check is deferred to here (LAST_ADMINISTRATOR).
    tx.commit().await?;
    Ok(dto)
}

/// Sets a new password and ends the user's sessions (all but the caller's own).
pub async fn set_password(pool: &PgPool, ctx: &RequestContext, id: Uuid, new_password: &str) -> Result<User, AppError> {
    let hash = password::hash(new_password).await?;
    let mut tx = pool.begin().await?;
    let before = lock(&mut tx, id).await?;
    if let Some(provider) = &before.identity_provider {
        return Err(AppError::conflict(format!(
            "This account signs in through \"{}\" and has no password here",
            provider.name
        )));
    }
    must_cover_user(&mut tx, ctx, id).await?;
    data::set_password(&mut tx, id, &hash).await?;
    let own = ctx.principal().filter(|p| p.user_id == id);
    let ended = data::delete_user_sessions(&mut tx, id, own.and_then(|p| p.session_id())).await?;
    let reason = if own.is_some() { RevokeReason::PasswordChanged } else { RevokeReason::PasswordReset };
    events::revoked(&mut tx, ctx, &ended, reason).await?;
    let dto = load(&mut tx, id).await?;
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

pub async fn remove(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<(), AppError> {
    not_yourself(ctx, id, "delete")?;
    let mut tx = pool.begin().await?;
    let before = lock(&mut tx, id).await?;
    must_cover_user(&mut tx, ctx, id).await?;
    // Ended explicitly (not by the foreign key's cascade) so each gets an audit row.
    let ended = data::delete_user_sessions(&mut tx, id, None).await?;
    events::revoked(&mut tx, ctx, &ended, RevokeReason::UserDeleted).await?;
    // Their API tokens go with them; each gets a delete row.
    api_tokens::audit_deleted_with_owner(&mut tx, ctx, id).await?;
    data::delete_user(&mut tx, id).await?;
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

const TAG: &str = "Users";
const BASE: &str = "/api/v1/admin/users";
const BY_ID: &str = "/api/v1/admin/users/{id}";

pub fn routes() -> Vec<Route> {
    let manage = GlobalPermission::UsersManage;
    vec![
        route(Method::GET, BASE, "listUsers")
            .tag(TAG)
            .summary("List users (paginated, searchable, filterable by active flag and profile)")
            .requires(manage)
            .handle(|api, In(NoPath, Query(q), NoBody): In<NoPath, Query<UserList>, NoBody>| async move {
                Ok(Json(list(&api.pool, &q).await?))
            }),
        route(Method::GET, BY_ID, "getUser")
            .tag(TAG)
            .summary("Get one user with the profiles they hold")
            .requires(manage)
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(get(&api.pool, id).await?))
            }),
        route(Method::POST, BASE, "createUser")
            .tag(TAG)
            .summary("Create a local user with a password and permission profiles")
            .description("403 when assigning a profile that grants permissions the caller does not hold.")
            .status(StatusCode::CREATED)
            .requires(manage)
            .errors(&[ErrorCode::Conflict])
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<UserCreate>>| async move {
                Ok(Json(create(&api.pool, &api.ctx, &b).await?))
            }),
        route(Method::PATCH, BY_ID, "updateUser")
            .tag(TAG)
            .summary("Update a user (partial): rename, disable/enable, assign profiles")
            .description(
                "`isActive: false` disables the account and ends its sessions. `profileIds` replaces the profiles the user holds. For an account of an identity provider, the name, e-mail and profiles are set again from the provider at its next sign-in (change the group mappings instead); disabling it holds whatever the provider says. 409 LAST_ADMINISTRATOR when the change would leave no active user with the Administrator profile; 409 CONFLICT when disabling yourself.",
            )
            .requires(manage)
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict, ErrorCode::LastAdministrator])
            .handle(|api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<UserUpdate>>| async move {
                Ok(Json(update(&api.pool, &api.ctx, id, &b).await?))
            }),
        route(Method::DELETE, BY_ID, "deleteUser")
            .tag(TAG)
            .summary("Delete a user (prefer disabling; the audit log keeps their id and name)")
            .description("409 when deleting yourself or the last active Administrator.")
            .requires(manage)
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict, ErrorCode::LastAdministrator])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                remove(&api.pool, &api.ctx, id).await?;
                Ok(NoContent)
            }),
        route(Method::PUT, "/api/v1/admin/users/{id}/password", "resetUserPassword")
            .tag(TAG)
            .summary("Set a new password for a user and end their sessions")
            .description("409 for an account that signs in through an identity provider (it has no password here).")
            .requires(manage)
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict])
            .handle(|api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<PasswordReset>>| async move {
                Ok(Json(set_password(&api.pool, &api.ctx, id, &b.password).await?))
            }),
    ]
}
