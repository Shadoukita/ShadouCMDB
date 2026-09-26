//! Sign-in: first-run setup, login, logout, the current user and their
//! effective permissions, and changing one's own password.

use axum::http::{HeaderMap, Method, StatusCode, header};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use utoipa::openapi::schema::{ObjectBuilder, Schema, Type};
use uuid::Uuid;

use super::profiles::ClassPermission;
use super::users::{self, User, UserCreate, password_problem, password_schema, username_schema};
use crate::api::context::{RequestContext, unauthenticated};
use crate::api::route::{Body, Check, In, Json, NoBody, NoContent, NoPath, NoQuery, Route, WithCookies, route};
use crate::api::schemas::{name_schema, trimmed};
use crate::auth::permissions::{ClassRights, GlobalPermission, Permissions};
use crate::auth::{AuthState, Principal, password, session};
use crate::data::auth as data;
use crate::http::error::{AppError, ErrorCode, FieldError};
use crate::modules::lookups::email_schema;

// ---------------------------------------------------------------------------
// Schemas
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetupStatus {
    /// True while no user exists: the UI shows the first-run screen
    pub setup_required: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetupBody {
    #[schema(schema_with = username_schema)]
    username: String,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    display_name: String,
    #[schema(schema_with = email_schema)]
    #[serde(default)]
    email: Option<String>,
    #[schema(schema_with = password_schema)]
    password: String,
}

impl Check for SetupBody {
    fn check(&self) -> Vec<FieldError> {
        password_problem("password", &self.password)
    }
}

fn login_field_schema() -> Schema {
    ObjectBuilder::new().schema_type(Type::String).min_length(Some(1)).max_length(Some(password::MAX_LENGTH)).into()
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoginBody {
    /// Case-insensitive
    #[schema(schema_with = login_field_schema)]
    username: String,
    #[schema(schema_with = login_field_schema)]
    password: String,
}
impl Check for LoginBody {}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PasswordChange {
    #[schema(schema_with = login_field_schema)]
    current_password: String,
    #[schema(schema_with = password_schema)]
    new_password: String,
}

impl Check for PasswordChange {
    fn check(&self) -> Vec<FieldError> {
        password_problem("newPassword", &self.new_password)
    }
}

/// What the signed-in user may do (the union of their profiles).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EffectivePermissions {
    /// Holds the built-in Administrator profile (everything below is then all-true)
    pub administrator: bool,
    #[schema(inline)]
    pub global: Vec<GlobalPermission>,
    /// Rights on every class
    pub all_classes: ClassRights,
    /// Rights on individual classes, beyond allClasses
    pub classes: Vec<ClassPermission>,
}

impl From<&Permissions> for EffectivePermissions {
    fn from(p: &Permissions) -> Self {
        if p.administrator {
            return EffectivePermissions {
                administrator: true,
                global: GlobalPermission::ALL.to_vec(),
                all_classes: ClassRights::ALL,
                classes: Vec::new(),
            };
        }
        EffectivePermissions {
            administrator: false,
            global: p.global.iter().copied().collect(),
            all_classes: p.all_classes,
            classes: p.classes.iter().map(|(id, r)| ClassPermission::new(Some(*id), *r)).collect(),
        }
    }
}

/// The signed-in user, their permissions and the CSRF token to send back.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Session {
    pub user: User,
    pub permissions: EffectivePermissions,
    /// Send as the X-CSRF-Token header on every POST, PUT, PATCH and DELETE
    /// (also readable from the shadoucmdb_csrf cookie)
    pub csrf_token: String,
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

async fn session_dto(pool: &PgPool, user_id: Uuid, csrf_token: String) -> Result<Session, AppError> {
    let mut conn = pool.acquire().await?;
    let user = users::load(&mut conn, user_id).await?;
    let permissions = data::load_permissions(&mut conn, user_id).await?;
    Ok(Session { user, permissions: EffectivePermissions::from(&permissions), csrf_token })
}

fn user_agent(headers: &HeaderMap) -> Option<String> {
    headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).map(|s| s.chars().take(400).collect())
}

/// Opens a session for the user; returns the session and its cookies.
async fn start_session(
    pool: &PgPool,
    auth: &AuthState,
    headers: &HeaderMap,
    user_id: Uuid,
) -> Result<WithCookies<Json<Session>>, AppError> {
    // A cookie from an earlier session in this browser is replaced, not kept alive.
    if let Some(old) = session::cookie(headers, session::SESSION_COOKIE) {
        sqlx::query("DELETE FROM sessions WHERE token_hash = $1").bind(session::token_hash(old)).execute(pool).await?;
    }
    let token = session::new_token();
    let csrf = session::new_token();
    let mut conn = pool.acquire().await?;
    data::create_session(
        &mut conn,
        user_id,
        &session::token_hash(&token),
        &csrf,
        auth.config.session_max_age,
        user_agent(headers).as_deref(),
    )
    .await?;
    drop(conn);
    let cookies = session::login_cookies(&auth.config, session::secure_cookies(&auth.config, headers), &token, &csrf);
    Ok(WithCookies(Json(session_dto(pool, user_id, csrf).await?), cookies))
}

pub async fn setup_required(pool: &PgPool) -> Result<bool, AppError> {
    Ok(data::count_users(&mut *pool.acquire().await?).await? == 0)
}

/// Creates the first user with the Administrator profile, only while there are no users.
async fn setup(
    pool: &PgPool,
    auth: &AuthState,
    headers: &HeaderMap,
    b: SetupBody,
) -> Result<WithCookies<Json<Session>>, AppError> {
    let ctx = RequestContext::system("first-run setup", crate::http::request_id::current());
    let mut tx = pool.begin().await?;
    data::lock_users_table(&mut tx).await?;
    if data::count_users(&mut tx).await? > 0 {
        return Err(AppError::conflict("Setup is already complete; sign in instead"));
    }
    let admin = data::builtin_profile_id(&mut tx).await?;
    let input = UserCreate {
        username: b.username,
        display_name: b.display_name,
        email: b.email,
        password: b.password,
        is_active: Some(true),
        profile_ids: vec![admin],
    };
    let user = users::create_in(&mut tx, &ctx, &input).await?;
    tx.commit().await?;
    tracing::info!(user = %user.username, "first-run setup created the first administrator");
    data::record_login(pool, user.id).await?;
    start_session(pool, auth, headers, user.id).await
}

fn invalid_credentials() -> AppError {
    AppError::new(ErrorCode::Unauthenticated, "Invalid username or password")
}

async fn login(
    pool: &PgPool,
    auth: &AuthState,
    headers: &HeaderMap,
    b: LoginBody,
) -> Result<WithCookies<Json<Session>>, AppError> {
    if let Some(wait) = auth.throttle.check(&b.username) {
        let secs = wait.as_secs().max(1);
        let mut err = AppError::new(
            ErrorCode::RateLimited,
            format!("Too many failed sign-ins for this username. Try again in {secs} s."),
        );
        err.retry_after = Some(secs);
        return Err(err);
    }
    let row = data::find_for_login(pool, &b.username).await?;
    if !password::verify(&b.password, row.as_ref().map(|r| r.password_hash.as_str())).await? {
        let locked = auth.throttle.failure(&b.username);
        tracing::warn!(username = %b.username.chars().take(64).collect::<String>(), locked_secs = locked.map(|d| d.as_secs()), "sign-in failed");
        return Err(invalid_credentials());
    }
    let Some(user) = row else { return Err(invalid_credentials()) };
    if !user.is_active {
        return Err(AppError::new(ErrorCode::Unauthenticated, "This account is disabled"));
    }
    auth.throttle.success(&b.username);
    let purged = data::purge_sessions(pool, auth.config.session_idle).await?;
    data::record_login(pool, user.id).await?;
    tracing::info!(user = %user.username, purged_sessions = purged, "signed in");
    start_session(pool, auth, headers, user.id).await
}

fn principal(ctx: &RequestContext) -> Result<&Principal, AppError> {
    ctx.principal().ok_or_else(unauthenticated)
}

async fn change_password(pool: &PgPool, ctx: &RequestContext, b: PasswordChange) -> Result<(), AppError> {
    let me = principal(ctx)?;
    let hash = data::password_hash(&mut *pool.acquire().await?, me.user_id).await?;
    if !password::verify(&b.current_password, hash.as_deref()).await? {
        return Err(AppError::field("currentPassword", "The current password is wrong", "invalid_credentials"));
    }
    users::set_password(pool, ctx, me.user_id, &b.new_password).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

const TAG: &str = "Authentication";

pub fn routes() -> Vec<Route> {
    vec![
        route(Method::GET, "/api/v1/setup", "getSetupStatus")
            .tag(TAG)
            .summary("Whether first-run setup is needed (no users exist yet)")
            .public()
            .handle(|api, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| async move {
                Ok(Json(SetupStatus { setup_required: setup_required(&api.pool).await? }))
            }),
        route(Method::POST, "/api/v1/setup", "completeSetup")
            .tag(TAG)
            .summary("Create the first administrator and sign them in (only while no users exist)")
            .description(
                "The new user holds the built-in Administrator profile. 409 once any user exists. Sets the session and CSRF cookies. `shadoucmdb create-admin` does the same from the command line.",
            )
            .public()
            .status(StatusCode::CREATED)
            .errors(&[ErrorCode::Conflict])
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<SetupBody>>| async move {
                setup(&api.pool, &api.auth, &api.headers, b).await
            }),
        route(Method::POST, "/api/v1/auth/login", "login")
            .tag(TAG)
            .summary("Sign in with username and password")
            .description(
                "Sets the `shadoucmdb_session` cookie (HttpOnly, SameSite=Lax, Secure behind HTTPS) and the `shadoucmdb_csrf` cookie. 401 for a wrong username or password. After 5 failures for a username, each further failure locks it for 1 s, 2 s, 4 s, ... up to 15 min; while locked the answer is 429 RATE_LIMITED with Retry-After.",
            )
            .public()
            .errors(&[ErrorCode::Unauthenticated, ErrorCode::RateLimited])
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<LoginBody>>| async move {
                login(&api.pool, &api.auth, &api.headers, b).await
            }),
        route(Method::POST, "/api/v1/auth/logout", "logout")
            .tag(TAG)
            .summary("Sign out: end this session and clear its cookies")
            .handle(|api, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| async move {
                data::delete_session(&api.pool, principal(&api.ctx)?.session_id).await?;
                let secure = session::secure_cookies(&api.auth.config, &api.headers);
                Ok(WithCookies(NoContent, session::logout_cookies(secure)))
            }),
        route(Method::GET, "/api/v1/auth/me", "getCurrentSession")
            .tag(TAG)
            .summary("The signed-in user, their effective permissions and the CSRF token")
            .handle(|api, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| async move {
                let me = principal(&api.ctx)?;
                Ok(Json(session_dto(&api.pool, me.user_id, me.csrf_token.clone()).await?))
            }),
        route(Method::PUT, "/api/v1/auth/password", "changeOwnPassword")
            .tag(TAG)
            .summary("Change your own password (ends your other sessions)")
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<PasswordChange>>| async move {
                change_password(&api.pool, &api.ctx, b).await?;
                Ok(NoContent)
            }),
    ]
}
