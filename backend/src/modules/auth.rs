//! Sign-in: first-run setup, login (with the second factor when MFA is set
//! up), logout, the current user and their effective permissions, and
//! changing one's own password.

use std::time::Duration;

use axum::http::{HeaderMap, Method, StatusCode};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use utoipa::openapi::schema::{ObjectBuilder, Schema, Type};
use uuid::Uuid;

use super::mfa::{self, MfaStatus};
use super::profiles::ClassPermission;
use super::sso;
use super::users::{self, User, UserCreate, password_problem, password_schema, username_schema};
use crate::api::context::{RequestContext, unauthenticated};
use crate::api::route::{
    Body, Check, Either, ErrorWithCookies, In, Json, NoBody, NoContent, NoPath, NoQuery, Route, WithCookies, route,
};
use crate::api::schemas::{name_schema, trimmed};
use crate::auth::events::{self, LoginMethod, RevokeReason};
use crate::auth::permissions::{ClassRights, GlobalPermission, Permissions};
use crate::auth::throttle::{Attempt, GLOBAL_PENALTY, Gate, LoginThrottle, SLOW_LANE_WAITERS};
use crate::auth::{AuthState, Principal, password, session};
use crate::data::auth as data;
use crate::data::crud::AuditAction;
use crate::data::mfa as mfa_data;
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

pub(crate) fn login_field_schema() -> Schema {
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
pub struct MfaLoginBody {
    #[schema(schema_with = mfa::code_schema)]
    code: String,
}
impl Check for MfaLoginBody {}

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
    pub mfa: MfaStatus,
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
    let mfa = mfa::status(&mut conn, user_id).await?;
    Ok(Session { user, permissions: EffectivePermissions::from(&permissions), mfa, csrf_token })
}

/// Opens a session for the user and records `login.success`; returns its cookies.
pub(crate) async fn open_session(
    pool: &PgPool,
    auth: &AuthState,
    headers: &HeaderMap,
    ctx: &RequestContext,
    user_id: Uuid,
    username: &str,
    method: LoginMethod,
) -> Result<Vec<axum::http::HeaderValue>, AppError> {
    let ctx = ctx.acting_as_user(user_id, username);
    let token = session::new_token();
    let csrf = session::new_token();
    let mut tx = pool.begin().await?;
    // A cookie from an earlier session in this browser is replaced, not kept alive.
    if let Some(old) = session::cookie(headers, session::SESSION_COOKIE)
        && let Some(ended) = data::delete_session_by_token(&mut tx, &session::token_hash(old)).await?
    {
        events::revoked(&mut tx, &ctx, &[ended], RevokeReason::Replaced).await?;
    }
    let session_id = data::create_session(
        &mut tx,
        user_id,
        &session::token_hash(&token),
        &csrf,
        auth.config.session_max_age,
        ctx.client.user_agent.as_deref(),
        ctx.client.ip,
    )
    .await?;
    events::login_success(&mut tx, &ctx, session_id, user_id, username, method).await?;
    tx.commit().await?;
    Ok(session::login_cookies(&auth.config, auth.session_cookie_secure(headers), &token, &csrf))
}

/// [`open_session`], answering with the session.
async fn start_session(
    pool: &PgPool,
    auth: &AuthState,
    headers: &HeaderMap,
    ctx: &RequestContext,
    user_id: Uuid,
    username: &str,
    method: LoginMethod,
) -> Result<WithCookies<Json<Session>>, AppError> {
    let cookies = open_session(pool, auth, headers, ctx, user_id, username, method).await?;
    let csrf = session::cookie_value(&cookies[1], session::CSRF_COOKIE).unwrap_or_default();
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
    request: &RequestContext,
    b: SetupBody,
) -> Result<WithCookies<Json<Session>>, AppError> {
    let ctx = RequestContext::system("first-run setup", request.request_id.clone()).with_client(request.client.clone());
    // Anonymous and unthrottled: an installed system must answer without taking any lock.
    if !setup_required(pool).await? {
        return Err(setup_done());
    }
    let mut tx = pool.begin().await?;
    data::lock_setup(&mut tx).await?;
    if data::count_users(&mut tx).await? > 0 {
        return Err(setup_done());
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
    start_session(pool, auth, headers, request, user.id, &user.username, LoginMethod::Setup).await
}

fn setup_done() -> AppError {
    AppError::conflict("Setup is already complete; sign in instead")
}

fn rate_limited(wait: Duration, message: &str) -> AppError {
    let secs = wait.as_secs().max(1);
    let mut err = AppError::new(ErrorCode::RateLimited, format!("{message}. Try again in {secs} s."));
    err.retry_after = Some(secs);
    err
}

/// Refuses an attempt while its key is locked; over the global budget, waits
/// for a turn in the slow lane instead of refusing (so a correct password
/// still gets in while someone sprays wrong ones). The attempt is reserved
/// until the returned [`Attempt`] is done, so concurrent requests cannot all
/// get past the gate before the first failure is counted.
async fn throttle_gate<'a>(throttle: &'a LoginThrottle, key: &str, what: &str) -> Result<Attempt<'a>, AppError> {
    let locked = |wait| rate_limited(wait, &format!("Too many failed {what}"));
    match throttle.begin(key, false) {
        Ok(attempt) => Ok(attempt),
        Err(Gate::Locked(wait)) => Err(locked(wait)),
        Err(Gate::Open | Gate::Slow) => {
            if !throttle.slow_lane().await {
                let wait = GLOBAL_PENALTY * SLOW_LANE_WAITERS as u32;
                return Err(rate_limited(wait, "Too many sign-ins are waiting on this server"));
            }
            // Failures for this key may have locked it while it waited.
            throttle.begin(key, true).map_err(|gate| match gate {
                Gate::Locked(wait) => locked(wait),
                Gate::Open | Gate::Slow => locked(GLOBAL_PENALTY),
            })
        }
    }
}

fn invalid_credentials() -> AppError {
    AppError::new(ErrorCode::Unauthenticated, "Invalid username or password")
}

/// Records `login.failure`, and `login.locked` when this failure set a lock.
async fn record_failure(
    pool: &PgPool,
    ctx: &RequestContext,
    username: &str,
    locked: Option<Duration>,
) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    let attempt = events::login_failure(&mut tx, ctx, username).await?;
    if let Some(lock) = locked {
        events::login_locked(&mut tx, ctx, attempt, username, lock).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// How long the second factor may take after the password.
const MFA_CHALLENGE_TTL: Duration = Duration::from_secs(5 * 60);
/// Wrong codes per challenge before the password is asked for again.
const MFA_CHALLENGE_ATTEMPTS: i32 = 5;

type LoginAnswer = Either<WithCookies<Json<Session>>, ErrorWithCookies>;

async fn login(
    pool: &PgPool,
    auth: &AuthState,
    headers: &HeaderMap,
    ctx: &RequestContext,
    b: LoginBody,
) -> Result<LoginAnswer, AppError> {
    let attempt = throttle_gate(&auth.throttle, &b.username, "sign-ins for this username").await?;
    let row = data::find_for_login(pool, &b.username).await?;
    // Directory accounts, and names no account has while a directory is enabled, go to LDAP.
    let directory = match &row {
        Some(r) => r.provider.as_ref().filter(|(_, kind)| kind == sso::LDAP).map(|(id, _)| Some(*id)),
        None => sso::any_directory(pool).await?.then_some(None),
    };
    if let Some(linked) = directory {
        return directory_login(pool, auth, headers, ctx, attempt, &b, linked).await.map(Either::Left);
    }
    // An OIDC account has no password: verify() then checks a dummy hash, so it takes as long.
    if !password::verify(&b.password, row.as_ref().and_then(|r| r.password_hash.as_deref())).await? {
        return Err(wrong_credentials(pool, attempt, ctx, &b.username).await?);
    }
    let Some(user) = row else { return Err(invalid_credentials()) };
    if !user.is_active {
        // Treated exactly like a wrong password: same throttle, same rows, same
        // lock. Otherwise the right password for a disabled account would be
        // an unthrottled way to grow audit_log, and its rows would stand out.
        let locked = attempt.failure();
        tracing::warn!(username = %user.username, ip = ?ctx.client.ip, locked_secs = locked.map(|d| d.as_secs()), "sign-in to a disabled account");
        record_failure(pool, ctx, &b.username, locked).await?;
        return Err(AppError::new(ErrorCode::Unauthenticated, "This account is disabled"));
    }
    if mfa_data::get_totp(&mut *pool.acquire().await?, user.id, false).await?.is_some_and(|t| t.confirmed) {
        // The username's failure count is left alone (the attempt is dropped):
        // were a right password to clear it, each one would buy a fresh set of
        // guesses at the code.
        mfa_data::purge_challenges(pool).await?;
        let token = session::new_token();
        mfa_data::create_challenge(
            &mut *pool.acquire().await?,
            user.id,
            &session::token_hash(&token),
            MFA_CHALLENGE_TTL,
        )
        .await?;
        tracing::info!(user = %user.username, ip = ?ctx.client.ip, "password accepted, second factor due");
        let err = AppError::new(
            ErrorCode::MfaRequired,
            "Enter the code from your authenticator app, or a recovery code (POST /api/v1/auth/login/mfa)",
        );
        let cookie = session::mfa_cookie(auth.session_cookie_secure(headers), &token, MFA_CHALLENGE_TTL);
        return Ok(Either::Right(ErrorWithCookies(err, vec![cookie])));
    }
    attempt.success();
    let purged = data::purge_sessions(pool, auth.config.session_idle).await?;
    data::record_login(pool, user.id).await?;
    tracing::info!(user = %user.username, ip = ?ctx.client.ip, purged_sessions = purged, "signed in");
    Ok(Either::Left(start_session(pool, auth, headers, ctx, user.id, &user.username, LoginMethod::Password).await?))
}

/// A wrong password (or unknown name): counted, logged and audited; returns the 401.
async fn wrong_credentials(
    pool: &PgPool,
    attempt: Attempt<'_>,
    ctx: &RequestContext,
    username: &str,
) -> Result<AppError, AppError> {
    let locked = attempt.failure();
    tracing::warn!(username = %username.chars().take(64).collect::<String>(), ip = ?ctx.client.ip, locked_secs = locked.map(|d| d.as_secs()), "sign-in failed");
    record_failure(pool, ctx, username, locked).await?;
    Ok(invalid_credentials())
}

/// Sign-in with a directory password, under the same throttle as local passwords.
async fn directory_login(
    pool: &PgPool,
    auth: &AuthState,
    headers: &HeaderMap,
    ctx: &RequestContext,
    attempt: Attempt<'_>,
    b: &LoginBody,
    linked: Option<Uuid>,
) -> Result<WithCookies<Json<Session>>, AppError> {
    match sso::directory_sign_in(pool, ctx, &b.username, &b.password, linked).await? {
        sso::DirectoryAnswer::SignedIn { user_id, username } => {
            attempt.success();
            let purged = data::purge_sessions(pool, auth.config.session_idle).await?;
            data::record_login(pool, user_id).await?;
            tracing::info!(user = %username, ip = ?ctx.client.ip, purged_sessions = purged, "signed in through a directory");
            start_session(pool, auth, headers, ctx, user_id, &username, LoginMethod::Ldap).await
        }
        sso::DirectoryAnswer::NoMatch => Err(wrong_credentials(pool, attempt, ctx, &b.username).await?),
        // A right password that is still refused counts like a disabled account's.
        sso::DirectoryAnswer::Refused(refusal) => {
            let locked = attempt.failure();
            record_failure(pool, ctx, &b.username, locked).await?;
            Err(AppError::new(ErrorCode::Unauthenticated, refusal.message()))
        }
        sso::DirectoryAnswer::Unavailable => Err(AppError::new(
            ErrorCode::IdentityProviderUnavailable,
            "The directory service could not be reached; try again shortly, or sign in with a local account",
        )),
    }
}

fn sign_in_expired() -> AppError {
    AppError::new(ErrorCode::Unauthenticated, "The sign-in has expired; enter your username and password again")
}

/// The second step of a sign-in: the code for the challenge in the MFA cookie.
/// Throttled with the password, per username: a wrong code is a failed sign-in.
async fn login_mfa(
    pool: &PgPool,
    auth: &AuthState,
    headers: &HeaderMap,
    ctx: &RequestContext,
    b: MfaLoginBody,
) -> Result<WithCookies<Json<Session>>, AppError> {
    let Some(token) = session::cookie(headers, session::MFA_COOKIE) else { return Err(sign_in_expired()) };
    let hash = session::token_hash(token);
    // Wait for the username's turn before locking anything.
    let Some(pending) = mfa_data::take_challenge(&mut *pool.acquire().await?, &hash).await? else {
        return Err(sign_in_expired());
    };
    let attempt = throttle_gate(&auth.throttle, &pending.username, "sign-ins for this username").await?;

    let mut tx = pool.begin().await?;
    let Some(challenge) = mfa_data::take_challenge(&mut tx, &hash).await? else { return Err(sign_in_expired()) };
    let (user_id, username) = (challenge.user_id, challenge.username.as_str());
    let as_user = ctx.acting_as_user(user_id, username);
    let Some(method) = mfa::verify_second_factor(&mut tx, user_id, &b.code).await? else {
        mfa_data::challenge_failed(&mut tx, challenge.id, MFA_CHALLENGE_ATTEMPTS).await?;
        let locked = attempt.failure();
        tracing::warn!(user = %username, ip = ?ctx.client.ip, locked_secs = locked.map(|d| d.as_secs()), "sign-in: wrong second factor");
        let extra = serde_json::json!({ "stage": "login" });
        events::mfa(&mut tx, ctx, AuditAction::MfaFailure, user_id, username, extra).await?;
        if let Some(lock) = locked {
            events::login_locked(&mut tx, ctx, Uuid::new_v4(), username, lock).await?;
        }
        tx.commit().await?;
        return Err(AppError::new(ErrorCode::Unauthenticated, "The code is wrong or was already used"));
    };
    mfa_data::delete_challenge(&mut tx, challenge.id).await?;
    if matches!(method, LoginMethod::RecoveryCode) {
        mfa::audit_recovery_code_used(&mut tx, &as_user, user_id, username, "login").await?;
    }
    tx.commit().await?;
    attempt.success();
    let purged = data::purge_sessions(pool, auth.config.session_idle).await?;
    data::record_login(pool, user_id).await?;
    tracing::info!(user = %username, ip = ?ctx.client.ip, purged_sessions = purged, "signed in with a second factor");
    let WithCookies(session, mut cookies) = start_session(pool, auth, headers, ctx, user_id, username, method).await?;
    cookies.push(session::clear_mfa_cookie(session::secure_cookies(&auth.config, headers)));
    Ok(WithCookies(session, cookies))
}

async fn logout(pool: &PgPool, ctx: &RequestContext) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    let Some(session_id) = principal(ctx)?.session_id() else { return Err(unauthenticated()) };
    if let Some(ended) = data::delete_session(&mut tx, session_id).await? {
        events::logout(&mut tx, ctx, &ended).await?;
    }
    tx.commit().await?;
    Ok(())
}

fn principal(ctx: &RequestContext) -> Result<&Principal, AppError> {
    ctx.principal().ok_or_else(unauthenticated)
}

/// Checks the signed-in user's password before a sensitive change. Throttled
/// per user like login, so a stolen session cannot be turned into a known
/// password by guessing the current one. An account of an identity provider
/// has no password here: 409.
pub(crate) async fn check_current_password(
    pool: &PgPool,
    auth: &AuthState,
    me: &Principal,
    current_password: &str,
) -> Result<(), AppError> {
    let key = me.user_id.to_string();
    let hash = data::password_hash(&mut *pool.acquire().await?, me.user_id).await?;
    if let Some(None) = hash {
        return Err(AppError::conflict(
            "Your account signs in through an identity provider and has no password here; change it there",
        ));
    }
    let attempt = throttle_gate(&auth.password_throttle, &key, "attempts at your current password").await?;
    if !password::verify(current_password, hash.flatten().as_deref()).await? {
        let locked = attempt.failure();
        tracing::warn!(user = %me.username, locked_secs = locked.map(|d| d.as_secs()), "wrong current password");
        return Err(AppError::field("currentPassword", "The current password is wrong", "invalid_credentials"));
    }
    attempt.success();
    Ok(())
}

async fn change_password(
    pool: &PgPool,
    auth: &AuthState,
    ctx: &RequestContext,
    b: PasswordChange,
) -> Result<(), AppError> {
    let me = principal(ctx)?;
    check_current_password(pool, auth, me, &b.current_password).await?;
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
                setup(&api.pool, &api.auth, &api.headers, &api.ctx, b).await
            }),
        route(Method::POST, "/api/v1/auth/login", "login")
            .tag(TAG)
            .summary("Sign in with username and password")
            .description(
                "Sets the `shadoucmdb_session` cookie (HttpOnly, SameSite=Lax, Secure behind HTTPS) and the `shadoucmdb_csrf` cookie. 401 for a wrong username or password. When the user has set up two-factor authentication, a right password answers 401 MFA_REQUIRED instead and sets the `shadoucmdb_mfa` cookie (HttpOnly, 5 min): send the code to POST /api/v1/auth/login/mfa. After 5 failures for a username, each further failure locks it for 1 s, 2 s, 4 s, ... up to 15 min; while locked the answer is 429 RATE_LIMITED with Retry-After. Attempts for a username that arrive while as many earlier ones as it has free failures left are still being checked are answered 429 with Retry-After: 1. Once 300 failures in 10 min for all usernames together are reached, sign-in is slowed rather than refused: attempts queue and go through one per 2 s (a correct password still signs in); only when 64 are already queued is the next one answered 429. When an LDAP/AD directory is enabled, directory accounts sign in here too (see GET /api/v1/auth/providers): a name no local account has is looked up in the enabled directories in order, under the same throttle; 503 IDENTITY_PROVIDER_UNAVAILABLE when a directory that might know the name cannot be reached. Accounts of an OIDC provider cannot sign in here.",
            )
            .public()
            .errors(&[ErrorCode::MfaRequired, ErrorCode::RateLimited, ErrorCode::IdentityProviderUnavailable])
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<LoginBody>>| async move {
                login(&api.pool, &api.auth, &api.headers, &api.ctx, b).await
            }),
        route(Method::POST, "/api/v1/auth/login/mfa", "loginSecondFactor")
            .tag(TAG)
            .summary("Finish signing in with an authenticator code or a recovery code")
            .description(
                "After POST /api/v1/auth/login answered MFA_REQUIRED: reads the `shadoucmdb_mfa` cookie it set and, for a right code, sets the session cookies like login. Each authenticator code works once; each recovery code works once and is then used up. 401 for a wrong code; after 5 wrong codes, or 5 minutes, the password is asked for again (401). Wrong codes count as failed sign-ins for the username: the same lock applies as for wrong passwords (429 RATE_LIMITED with Retry-After).",
            )
            .public()
            .errors(&[ErrorCode::Unauthenticated, ErrorCode::RateLimited])
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<MfaLoginBody>>| async move {
                login_mfa(&api.pool, &api.auth, &api.headers, &api.ctx, b).await
            }),
        route(Method::POST, "/api/v1/auth/logout", "logout")
            .tag(TAG)
            .summary("Sign out: end this session and clear its cookies")
            .session_only()
            .before_mfa_enrolment()
            .handle(|api, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| async move {
                logout(&api.pool, &api.ctx).await?;
                let secure = session::secure_cookies(&api.auth.config, &api.headers);
                Ok(WithCookies(NoContent, session::logout_cookies(secure)))
            }),
        route(Method::GET, "/api/v1/auth/me", "getCurrentSession")
            .tag(TAG)
            .summary("The signed-in user, their effective permissions, MFA status and the CSRF token")
            .session_only()
            .before_mfa_enrolment()
            .handle(|api, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| async move {
                let me = principal(&api.ctx)?;
                let csrf_token = me.csrf_token().ok_or_else(unauthenticated)?.to_owned();
                Ok(Json(session_dto(&api.pool, me.user_id, csrf_token).await?))
            }),
        route(Method::PUT, "/api/v1/auth/password", "changeOwnPassword")
            .tag(TAG)
            .summary("Change your own password (ends your other sessions and revokes your API tokens)")
            .session_only()
            .before_mfa_enrolment()
            .description(
                "Every API token you own that still works is revoked; create new ones after the change. 400 when `currentPassword` is wrong; 409 for an account that signs in through an identity provider. After 5 wrong current passwords, each further one locks password changes for this user for 1 s, 2 s, 4 s, ... up to 15 min; while locked the answer is 429 RATE_LIMITED with Retry-After.",
            )
            .errors(&[ErrorCode::RateLimited, ErrorCode::Conflict])
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<PasswordChange>>| async move {
                change_password(&api.pool, &api.auth, &api.ctx, b).await?;
                Ok(NoContent)
            }),
    ]
}

#[cfg(test)]
mod tests {
    use sqlx::Executor;

    use super::*;
    use crate::auth::Credential;
    use crate::config::{AuthConfig, CookieSecure};
    use crate::db::scratch;

    fn auth_state() -> AuthState {
        AuthState::new(AuthConfig {
            session_idle: Duration::from_secs(3600),
            session_max_age: Duration::from_secs(3600),
            cookie_secure: CookieSecure::Never,
            public_url: None,
        })
    }

    fn anon() -> RequestContext {
        RequestContext::anonymous(String::new())
    }

    /// The first administrator's password: random per test run, so no
    /// hard-coded credential reaches the hasher or verifier.
    static OWNER_PASSWORD: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| format!("owner passphrase {}", Uuid::new_v4()));

    fn body(username: &str) -> SetupBody {
        SetupBody {
            username: username.into(),
            display_name: "First admin".into(),
            email: None,
            password: OWNER_PASSWORD.clone(),
        }
    }

    /// Setup on an installed system must answer 409 without a lock that
    /// blocks writes to `users` (sign-in updates last_login_at).
    #[tokio::test]
    async fn setup_on_an_installed_system_does_not_block_user_writes() {
        let Some(db) = scratch::database("setup_on_an_installed_system_does_not_block_user_writes").await else {
            return;
        };
        let (pool, auth) = (&db.pool, auth_state());
        let headers = HeaderMap::new();
        setup(pool, &auth, &headers, &anon(), body("first")).await.expect("first setup");

        // A writer holding ROW EXCLUSIVE on users, as a sign-in does mid-transaction.
        let mut writer = pool.begin().await.unwrap();
        writer.execute("UPDATE users SET last_login_at = last_login_at WHERE false").await.unwrap();
        let late = tokio::time::timeout(Duration::from_secs(5), setup(pool, &auth, &headers, &anon(), body("late")))
            .await
            .expect("setup waited on a lock held by a users writer");
        assert_eq!(late.err().map(|e| e.code), Some(ErrorCode::Conflict));
        writer.rollback().await.unwrap();
        db.drop().await;
    }

    /// Two setups at once on an empty database: exactly one administrator.
    #[tokio::test]
    async fn concurrent_setups_create_exactly_one_administrator() {
        let Some(db) = scratch::database("concurrent_setups_create_exactly_one_administrator").await else { return };
        let (pool, auth) = (&db.pool, auth_state());
        for round in 0..5 {
            let (headers, ctx) = (HeaderMap::new(), anon());
            let (a, b) = tokio::join!(
                setup(pool, &auth, &headers, &ctx, body(&format!("a{round}"))),
                setup(pool, &auth, &headers, &ctx, body(&format!("b{round}"))),
            );
            let codes = [a.err().map(|e| e.code), b.err().map(|e| e.code)];
            assert!(codes.contains(&None) && codes.contains(&Some(ErrorCode::Conflict)), "round {round}: {codes:?}");
            assert_eq!(data::count_users(&mut pool.acquire().await.unwrap()).await.unwrap(), 1);
            // Back to "no users" for the next round, past the last-administrator guard
            // (a trigger; the scratch database's owner may switch triggers off).
            let mut tx = pool.begin().await.unwrap();
            tx.execute("SET LOCAL session_replication_role = replica").await.unwrap();
            tx.execute("DELETE FROM sessions").await.unwrap();
            tx.execute("DELETE FROM user_permission_profiles").await.unwrap();
            tx.execute("DELETE FROM users").await.unwrap();
            tx.commit().await.unwrap();
        }
        db.drop().await;
    }

    /// Spraying wrong passwords over many usernames slows sign-in down; it
    /// must not lock the administrator out of the CMDB.
    #[tokio::test]
    async fn a_correct_password_signs_in_after_the_global_budget_is_spent() {
        let Some(db) = scratch::database("a_correct_password_signs_in_after_the_global_budget_is_spent").await else {
            return;
        };
        let (pool, auth) = (&db.pool, auth_state());
        setup(pool, &auth, &HeaderMap::new(), &anon(), body("admin")).await.expect("setup");
        for i in 0..crate::auth::throttle::GLOBAL_BUDGET {
            auth.throttle.failure(&format!("junk-{i}"));
        }
        assert_eq!(auth.throttle.check("admin"), Gate::Slow);
        let started = std::time::Instant::now();
        let login_as = |password: &str| LoginBody { username: "admin".into(), password: password.into() };
        let signed_in = login(pool, &auth, &HeaderMap::new(), &anon(), login_as(OWNER_PASSWORD.as_str())).await;
        assert!(signed_in.is_ok(), "refused: {:?}", signed_in.err().map(|e| e.code));
        assert!(started.elapsed() >= GLOBAL_PENALTY, "through the slow lane");
        let wrong = login(pool, &auth, &HeaderMap::new(), &anon(), login_as(&OWNER_PASSWORD.to_uppercase())).await;
        assert_eq!(wrong.err().map(|e| e.code), Some(ErrorCode::Unauthenticated), "slowed, then checked");
        db.drop().await;
    }

    /// Concurrent wrong passwords for one username: only the free failures
    /// get their password checked, the rest are refused (GH#118).
    #[tokio::test]
    async fn concurrent_wrong_passwords_cannot_bypass_the_lock() {
        let Some(db) = scratch::database("concurrent_wrong_passwords_cannot_bypass_the_lock").await else { return };
        let (pool, auth) = (&db.pool, auth_state());
        setup(pool, &auth, &HeaderMap::new(), &anon(), body("admin")).await.expect("setup");
        let wrong = OWNER_PASSWORD.to_uppercase();
        let (headers, ctx) = (HeaderMap::new(), anon());
        let tries = (0..50).map(|_| {
            let b = LoginBody { username: "admin".into(), password: wrong.clone() };
            login(pool, &auth, &headers, &ctx, b)
        });
        let codes: Vec<_> =
            futures_util::future::join_all(tries).await.into_iter().map(|r| r.err().map(|e| e.code)).collect();
        let checked = codes.iter().filter(|c| **c == Some(ErrorCode::Unauthenticated)).count();
        let refused = codes.iter().filter(|c| **c == Some(ErrorCode::RateLimited)).count();
        assert_eq!((checked, refused), (crate::auth::throttle::FREE_FAILURES as usize, 50 - checked), "{codes:?}");
        let failures: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'login.failure'")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(failures as usize, checked);
        db.drop().await;
    }

    /// Wrong current passwords lock password changes for that user, like login.
    #[tokio::test]
    async fn guessing_the_current_password_is_throttled() {
        let Some(db) = scratch::database("guessing_the_current_password_is_throttled").await else { return };
        let (pool, auth) = (&db.pool, auth_state());
        let (right, wrong) = (OWNER_PASSWORD.as_str(), OWNER_PASSWORD.to_uppercase());
        setup(pool, &auth, &HeaderMap::new(), &anon(), body("owner")).await.expect("setup");
        let user_id: Uuid = sqlx::query_scalar("SELECT id FROM users").fetch_one(pool).await.unwrap();
        let permissions = data::load_permissions(&mut pool.acquire().await.unwrap(), user_id).await.unwrap();
        let principal = Principal {
            user_id,
            username: "owner".into(),
            credential: Credential::Session {
                id: Uuid::nil(),
                csrf_token: String::new(),
                mfa_enrolment_required: false,
            },
            permissions,
        };
        let ctx = RequestContext::user(std::sync::Arc::new(principal), String::new());
        let change = |current: &str| PasswordChange {
            current_password: current.into(),
            new_password: "a brand new passphrase".into(),
        };
        for _ in 0..crate::auth::throttle::FREE_FAILURES {
            let e = change_password(pool, &auth, &ctx, change(&wrong)).await.unwrap_err();
            assert_eq!(e.code, ErrorCode::ValidationError);
        }
        let e = change_password(pool, &auth, &ctx, change(right)).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::RateLimited, "locked: not even the right password is checked");
        assert_eq!(e.retry_after, Some(1));
        let hash = data::password_hash(&mut pool.acquire().await.unwrap(), user_id).await.unwrap().flatten();
        assert!(password::verify(right, hash.as_deref()).await.unwrap(), "password unchanged");
        db.drop().await;
    }

    /// A request straight from `ip`, no proxy.
    fn from(ip: &str) -> RequestContext {
        via(ip, ip)
    }

    /// A request whose forwarded headers say `ip`, from TCP peer `peer`.
    fn via(ip: &str, peer: &str) -> RequestContext {
        let client = crate::api::context::ClientInfo {
            ip: Some(ip.parse().unwrap()),
            peer_ip: Some(peer.parse().unwrap()),
            user_agent: Some("audit-test".into()),
        };
        anon().with_client(client)
    }

    fn login_body(username: &str, password: &str) -> LoginBody {
        LoginBody { username: username.into(), password: password.into() }
    }

    type Row = (String, Option<String>, Uuid, serde_json::Value);

    async fn auth_rows(pool: &PgPool, action: &str) -> Vec<Row> {
        sqlx::query_as(
            "SELECT actor_type, actor_id, entity_id, new_value FROM audit_log
             WHERE entity_type = 'sessions' AND action = $1 AND old_value IS NULL ORDER BY id",
        )
        .bind(action)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    /// Sign-in success, failure, the lock and a revocation by disabling each
    /// write an audit row with the actor and client IP, and nothing secret.
    #[tokio::test]
    async fn authentication_events_are_audited() {
        let Some(db) = scratch::database("authentication_events_are_audited").await else { return };
        let (pool, auth, headers) = (&db.pool, auth_state(), HeaderMap::new());
        let WithCookies(Json(owner), _) =
            setup(pool, &auth, &headers, &from("192.0.2.1"), body("owner")).await.unwrap();

        // A second user signs in from 203.0.113.5.
        let system = RequestContext::system("test", "test");
        let input = UserCreate {
            username: "alice".into(),
            display_name: "Alice".into(),
            email: None,
            password: "alice correct horse".into(),
            is_active: Some(true),
            profile_ids: vec![],
        };
        let alice = users::create(pool, &system, &input).await.unwrap();
        login(pool, &auth, &headers, &from("203.0.113.5"), login_body("ALICE", "alice correct horse")).await.unwrap();
        let success = auth_rows(pool, "login.success").await;
        let (actor_type, actor_id, session_id, v) = success.last().unwrap();
        assert_eq!((actor_type.as_str(), actor_id.as_deref()), ("user", Some(alice.id.to_string().as_str())));
        assert_eq!((v["ipAddress"].as_str(), v["userAgent"].as_str()), (Some("203.0.113.5"), Some("audit-test")));
        assert_eq!((v["username"].as_str(), v["method"].as_str()), (Some("alice"), Some("password")));
        let ip: Option<String> = sqlx::query_scalar("SELECT host(ip_address) FROM sessions WHERE id = $1")
            .bind(session_id)
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(ip.as_deref(), Some("203.0.113.5"), "the session keeps the client IP");
        assert_eq!(success[0].3["method"], "setup");

        // Failures: no actor id; a real and an unknown name leave identically shaped rows.
        login(pool, &auth, &headers, &from("198.51.100.7"), login_body("alice", "wrong password 1"))
            .await
            .err()
            .expect("sign-in refused");
        login(pool, &auth, &headers, &from("198.51.100.7"), login_body("nobody", "wrong password 1"))
            .await
            .err()
            .expect("sign-in refused");
        let failures = auth_rows(pool, "login.failure").await;
        assert_eq!(failures.len(), 2);
        for ((actor_type, actor_id, _, v), name) in failures.iter().zip(["alice", "nobody"]) {
            assert_eq!((actor_type.as_str(), actor_id.as_deref()), ("api_client", None));
            assert_eq!(
                v,
                &serde_json::json!({ "attemptedUsername": name, "ipAddress": "198.51.100.7", "userAgent": "audit-test" })
            );
        }

        // The failure that sets the lock also writes login.locked, for the same attempt.
        for _ in 1..crate::auth::throttle::FREE_FAILURES {
            login(pool, &auth, &headers, &from("198.51.100.7"), login_body("nobody", "wrong password 2"))
                .await
                .err()
                .expect("sign-in refused");
        }
        let locked = auth_rows(pool, "login.locked").await;
        assert_eq!(locked.len(), 1);
        let (_, actor_id, attempt, v) = &locked[0];
        assert_eq!(
            (actor_id, v["attemptedUsername"].as_str(), v["lockedForSeconds"].as_u64()),
            (&None, Some("nobody"), Some(1))
        );
        assert_eq!(auth_rows(pool, "login.failure").await.last().unwrap().2, *attempt);

        // Disabling alice ends her session: session.revoke, the administrator as actor.
        let (tokens, csrf): (Vec<Vec<u8>>, Vec<String>) =
            sqlx::query_as::<_, (Vec<u8>, String)>("SELECT token_hash, csrf_token FROM sessions")
                .fetch_all(pool)
                .await
                .unwrap()
                .into_iter()
                .unzip();
        let permissions = data::load_permissions(&mut pool.acquire().await.unwrap(), owner.user.id).await.unwrap();
        let principal = Principal {
            user_id: owner.user.id,
            username: "owner".into(),
            credential: Credential::Session {
                id: Uuid::nil(),
                csrf_token: String::new(),
                mfa_enrolment_required: false,
            },
            permissions,
        };
        let admin =
            RequestContext::user(std::sync::Arc::new(principal), "req-1".into()).with_client(from("192.0.2.1").client);
        let update: users::UserUpdate = serde_json::from_value(serde_json::json!({ "isActive": false })).unwrap();
        users::update(pool, &admin, alice.id, &update).await.unwrap();
        let revoked = auth_rows(pool, "session.revoke").await;
        assert_eq!(revoked.len(), 1);
        let (_, actor_id, entity_id, v) = &revoked[0];
        assert_eq!((actor_id.as_deref(), entity_id), (Some(owner.user.id.to_string().as_str()), session_id));
        assert_eq!(
            (v["reason"].as_str(), v["userId"].as_str()),
            (Some("user_disabled"), Some(alice.id.to_string().as_str()))
        );
        assert_eq!(
            (v["ipAddress"].as_str(), v["session"]["ipAddress"].as_str()),
            (Some("192.0.2.1"), Some("203.0.113.5"))
        );

        // Nothing secret in any authentication row.
        let all: Vec<String> =
            sqlx::query_scalar("SELECT new_value::text FROM audit_log WHERE entity_type = 'sessions'")
                .fetch_all(pool)
                .await
                .unwrap();
        let mut secrets: Vec<String> =
            [OWNER_PASSWORD.as_str(), "alice correct horse", "wrong password", "argon2"].map(String::from).to_vec();
        secrets.extend(tokens.iter().map(hex::encode).chain(csrf).chain([owner.csrf_token.clone()]));
        for row in &all {
            for secret in &secrets {
                assert!(!row.contains(secret.as_str()), "{row} contains secret material");
            }
        }
        db.drop().await;
    }
    /// The right password for a disabled account counts as a failure for the
    /// throttle: it locks the name like a wrong one, and the rows look the same.
    #[tokio::test]
    async fn a_disabled_account_is_throttled_like_a_wrong_password() {
        let Some(db) = scratch::database("a_disabled_account_is_throttled_like_a_wrong_password").await else { return };
        let (pool, auth, headers) = (&db.pool, auth_state(), HeaderMap::new());
        setup(pool, &auth, &headers, &from("192.0.2.1"), body("owner")).await.unwrap();
        let input = UserCreate {
            username: "gone".into(),
            display_name: "Gone".into(),
            email: None,
            password: "gone correct horse".into(),
            is_active: Some(false),
            profile_ids: vec![],
        };
        users::create(pool, &RequestContext::system("test", "test"), &input).await.unwrap();
        for _ in 0..crate::auth::throttle::FREE_FAILURES {
            let e = login(pool, &auth, &headers, &from("198.51.100.8"), login_body("gone", "gone correct horse"))
                .await
                .err()
                .expect("disabled");
            assert_eq!(e.code, ErrorCode::Unauthenticated);
        }
        let locked = auth_rows(pool, "login.locked").await;
        assert_eq!(locked.len(), 1, "the disabled account's name is locked");
        assert_eq!(locked[0].3["attemptedUsername"], "gone");
        let failures = auth_rows(pool, "login.failure").await;
        assert_eq!(failures.len() as u32, crate::auth::throttle::FREE_FAILURES);
        assert_eq!(
            failures[0].3,
            serde_json::json!({ "attemptedUsername": "gone", "ipAddress": "198.51.100.8", "userAgent": "audit-test" })
        );
        let e = login(pool, &auth, &headers, &from("198.51.100.8"), login_body("gone", "gone correct horse"))
            .await
            .err()
            .expect("locked");
        assert_eq!(e.code, ErrorCode::RateLimited);
        assert_eq!(auth_rows(pool, "login.failure").await.len(), failures.len(), "a 429 writes no row");
        db.drop().await;
    }

    /// When the forwarded address is not the TCP peer, the row keeps both.
    #[tokio::test]
    async fn the_peer_address_is_kept_when_forwarded_headers_differ() {
        let Some(db) = scratch::database("the_peer_address_is_kept_when_forwarded_headers_differ").await else {
            return;
        };
        let (pool, auth, headers) = (&db.pool, auth_state(), HeaderMap::new());
        setup(pool, &auth, &headers, &from("192.0.2.1"), body("owner")).await.unwrap();
        login(pool, &auth, &headers, &via("198.51.100.9", "10.0.0.2"), login_body("owner", OWNER_PASSWORD.as_str()))
            .await
            .unwrap();
        login(pool, &auth, &headers, &via("198.51.100.9", "10.0.0.2"), login_body("owner", "wrong"))
            .await
            .err()
            .expect("refused");
        let success = auth_rows(pool, "login.success").await;
        let failure = auth_rows(pool, "login.failure").await;
        for v in [&success[1].3, &failure[0].3] {
            assert_eq!(
                (v["ipAddress"].as_str(), v["peerIpAddress"].as_str()),
                (Some("198.51.100.9"), Some("10.0.0.2"))
            );
        }
        assert!(success[0].3.get("peerIpAddress").is_none(), "no peerIpAddress when it equals ipAddress");
        db.drop().await;
    }
}
