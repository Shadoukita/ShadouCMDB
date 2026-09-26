//! Sign-in: first-run setup, login, logout, the current user and their
//! effective permissions, and changing one's own password.

use std::time::Duration;

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
use crate::auth::events::{self, LoginMethod, RevokeReason};
use crate::auth::permissions::{ClassRights, GlobalPermission, Permissions};
use crate::auth::throttle::{GLOBAL_PENALTY, Gate, LoginThrottle, SLOW_LANE_WAITERS};
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

/// Opens a session for the user and records `login.success`; returns the session and its cookies.
async fn start_session(
    pool: &PgPool,
    auth: &AuthState,
    headers: &HeaderMap,
    ctx: &RequestContext,
    user_id: Uuid,
    username: &str,
    method: LoginMethod,
) -> Result<WithCookies<Json<Session>>, AppError> {
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
/// still gets in while someone sprays wrong ones).
async fn throttle_gate(throttle: &LoginThrottle, key: &str, what: &str) -> Result<(), AppError> {
    let locked = |wait| rate_limited(wait, &format!("Too many failed {what}"));
    match throttle.check(key) {
        Gate::Open => Ok(()),
        Gate::Locked(wait) => Err(locked(wait)),
        Gate::Slow => {
            if !throttle.slow_lane().await {
                let wait = GLOBAL_PENALTY * SLOW_LANE_WAITERS as u32;
                return Err(rate_limited(wait, "Too many sign-ins are waiting on this server"));
            }
            // Failures for this key may have locked it while it waited.
            match throttle.check(key) {
                Gate::Locked(wait) => Err(locked(wait)),
                Gate::Open | Gate::Slow => Ok(()),
            }
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
    locked: Option<std::time::Duration>,
) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    let attempt = events::login_failure(&mut tx, ctx, username).await?;
    if let Some(lock) = locked {
        events::login_locked(&mut tx, ctx, attempt, username, lock).await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn login(
    pool: &PgPool,
    auth: &AuthState,
    headers: &HeaderMap,
    ctx: &RequestContext,
    b: LoginBody,
) -> Result<WithCookies<Json<Session>>, AppError> {
    throttle_gate(&auth.throttle, &b.username, "sign-ins for this username").await?;
    let row = data::find_for_login(pool, &b.username).await?;
    if !password::verify(&b.password, row.as_ref().map(|r| r.password_hash.as_str())).await? {
        let locked = auth.throttle.failure(&b.username);
        tracing::warn!(username = %b.username.chars().take(64).collect::<String>(), ip = ?ctx.client.ip, locked_secs = locked.map(|d| d.as_secs()), "sign-in failed");
        record_failure(pool, ctx, &b.username, locked).await?;
        return Err(invalid_credentials());
    }
    let Some(user) = row else { return Err(invalid_credentials()) };
    if !user.is_active {
        // Recorded like any other failure: the row does not say why.
        record_failure(pool, ctx, &b.username, None).await?;
        return Err(AppError::new(ErrorCode::Unauthenticated, "This account is disabled"));
    }
    auth.throttle.success(&b.username);
    let purged = data::purge_sessions(pool, auth.config.session_idle).await?;
    data::record_login(pool, user.id).await?;
    tracing::info!(user = %user.username, ip = ?ctx.client.ip, purged_sessions = purged, "signed in");
    start_session(pool, auth, headers, ctx, user.id, &user.username, LoginMethod::Password).await
}

async fn logout(pool: &PgPool, ctx: &RequestContext) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    if let Some(ended) = data::delete_session(&mut tx, principal(ctx)?.session_id).await? {
        events::logout(&mut tx, ctx, &ended).await?;
    }
    tx.commit().await?;
    Ok(())
}

fn principal(ctx: &RequestContext) -> Result<&Principal, AppError> {
    ctx.principal().ok_or_else(unauthenticated)
}

/// Throttled per user like login, so a stolen session cannot be turned into a
/// known password by guessing the current one.
async fn change_password(
    pool: &PgPool,
    auth: &AuthState,
    ctx: &RequestContext,
    b: PasswordChange,
) -> Result<(), AppError> {
    let me = principal(ctx)?;
    let key = me.user_id.to_string();
    throttle_gate(&auth.password_throttle, &key, "attempts at your current password").await?;
    let hash = data::password_hash(&mut *pool.acquire().await?, me.user_id).await?;
    if !password::verify(&b.current_password, hash.as_deref()).await? {
        let locked = auth.password_throttle.failure(&key);
        tracing::warn!(user = %me.username, locked_secs = locked.map(|d| d.as_secs()), "password change: wrong current password");
        return Err(AppError::field("currentPassword", "The current password is wrong", "invalid_credentials"));
    }
    auth.password_throttle.success(&key);
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
                "Sets the `shadoucmdb_session` cookie (HttpOnly, SameSite=Lax, Secure behind HTTPS) and the `shadoucmdb_csrf` cookie. 401 for a wrong username or password. After 5 failures for a username, each further failure locks it for 1 s, 2 s, 4 s, ... up to 15 min; while locked the answer is 429 RATE_LIMITED with Retry-After. Once 300 failures in 10 min for all usernames together are reached, sign-in is slowed rather than refused: attempts queue and go through one per 2 s (a correct password still signs in); only when 64 are already queued is the next one answered 429.",
            )
            .public()
            .errors(&[ErrorCode::Unauthenticated, ErrorCode::RateLimited])
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<LoginBody>>| async move {
                login(&api.pool, &api.auth, &api.headers, &api.ctx, b).await
            }),
        route(Method::POST, "/api/v1/auth/logout", "logout")
            .tag(TAG)
            .summary("Sign out: end this session and clear its cookies")
            .handle(|api, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| async move {
                logout(&api.pool, &api.ctx).await?;
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
            .description(
                "400 when `currentPassword` is wrong. After 5 wrong current passwords, each further one locks password changes for this user for 1 s, 2 s, 4 s, ... up to 15 min; while locked the answer is 429 RATE_LIMITED with Retry-After.",
            )
            .errors(&[ErrorCode::RateLimited])
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
    use crate::config::{AuthConfig, CookieSecure};
    use crate::db::scratch;

    fn auth_state() -> AuthState {
        AuthState::new(AuthConfig {
            session_idle: Duration::from_secs(3600),
            session_max_age: Duration::from_secs(3600),
            cookie_secure: CookieSecure::Never,
        })
    }

    fn anon() -> RequestContext {
        RequestContext::anonymous(String::new())
    }

    fn body(username: &str) -> SetupBody {
        SetupBody {
            username: username.into(),
            display_name: "First admin".into(),
            email: None,
            password: "correct horse battery".into(),
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
        setup(pool, &auth, &HeaderMap::new(), body("admin")).await.expect("setup");
        for i in 0..crate::auth::throttle::GLOBAL_BUDGET {
            auth.throttle.failure(&format!("junk-{i}"));
        }
        assert_eq!(auth.throttle.check("admin"), Gate::Slow);
        let started = std::time::Instant::now();
        let login_as = |password: &str| LoginBody { username: "admin".into(), password: password.into() };
        let signed_in = login(pool, &auth, &HeaderMap::new(), login_as("correct horse battery")).await;
        assert!(signed_in.is_ok(), "refused: {:?}", signed_in.err().map(|e| e.code));
        assert!(started.elapsed() >= GLOBAL_PENALTY, "through the slow lane");
        let wrong = login(pool, &auth, &HeaderMap::new(), login_as("wrong guess")).await;
        assert_eq!(wrong.err().map(|e| e.code), Some(ErrorCode::Unauthenticated), "slowed, then checked");
        db.drop().await;
    }

    /// Wrong current passwords lock password changes for that user, like login.
    #[tokio::test]
    async fn guessing_the_current_password_is_throttled() {
        let Some(db) = scratch::database("guessing_the_current_password_is_throttled").await else { return };
        let (pool, auth) = (&db.pool, auth_state());
        setup(pool, &auth, &HeaderMap::new(), &anon(), body("owner")).await.expect("setup");
        let user_id: Uuid = sqlx::query_scalar("SELECT id FROM users").fetch_one(pool).await.unwrap();
        let permissions = data::load_permissions(&mut pool.acquire().await.unwrap(), user_id).await.unwrap();
        let principal = Principal {
            user_id,
            username: "owner".into(),
            session_id: Uuid::nil(),
            csrf_token: String::new(),
            permissions,
        };
        let ctx = RequestContext::user(std::sync::Arc::new(principal), String::new());
        let change = |current: &str| PasswordChange {
            current_password: current.into(),
            new_password: "a brand new passphrase".into(),
        };
        for _ in 0..crate::auth::throttle::FREE_FAILURES {
            let e = change_password(pool, &auth, &ctx, change("wrong guess")).await.unwrap_err();
            assert_eq!(e.code, ErrorCode::ValidationError);
        }
        let e = change_password(pool, &auth, &ctx, change("correct horse battery")).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::RateLimited, "locked: not even the right password is checked");
        assert_eq!(e.retry_after, Some(1));
        let hash = data::password_hash(&mut pool.acquire().await.unwrap(), user_id).await.unwrap();
        assert!(password::verify("correct horse battery", hash.as_deref()).await.unwrap(), "password unchanged");
        db.drop().await;
    }

    fn from(ip: &str) -> RequestContext {
        let client =
            crate::api::context::ClientInfo { ip: Some(ip.parse().unwrap()), user_agent: Some("audit-test".into()) };
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
            session_id: Uuid::nil(),
            csrf_token: String::new(),
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
            ["correct horse battery", "alice correct horse", "wrong password", "argon2"].map(String::from).to_vec();
        secrets.extend(tokens.iter().map(hex::encode).chain(csrf).chain([owner.csrf_token.clone()]));
        for row in &all {
            for secret in &secrets {
                assert!(!row.contains(secret.as_str()), "{row} contains secret material");
            }
        }
        db.drop().await;
    }
}
