//! Sign-in: first-run setup, login (with the second factor when MFA is set
//! up), logout, the current user and their effective permissions, and
//! changing one's own password.

use std::time::Duration;

use axum::http::{HeaderMap, Method, StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use utoipa::ToSchema;
use utoipa::openapi::schema::{ObjectBuilder, Schema, Type};
use uuid::Uuid;

use super::mfa::{self, MfaStatus};
use super::profiles::ClassPermission;
use super::users::{self, User, UserCreate, password_problem, password_schema, required_email_schema, username_schema};
use super::{people, sso};
use crate::api::context::{RequestContext, unauthenticated};
use crate::api::route::{
    Body, Check, Either, ErrorWithCookies, In, Json, NoBody, NoContent, NoPath, NoQuery, Route, WithCookies, route,
};
use crate::api::schemas::{self, name_schema, trimmed};
use crate::auth::events::{self, LoginMethod, ProviderMfa, RevokeReason};
use crate::auth::permissions::{ClassRights, GlobalPermission, Permissions};
use crate::auth::secret::Secret;
use crate::auth::throttle::{Attempt, GLOBAL_PENALTY, Gate, LoginThrottle, Net, SLOW_LANE_WAITERS};
use crate::auth::{AuthState, Principal, password, session};
use crate::data::auth as data;
use crate::data::crud::{self, AuditAction, AuditEntry};
use crate::data::mfa as mfa_data;
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};

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
    /// The administrator's e-mail; a Person CI is created for it
    #[schema(schema_with = required_email_schema)]
    #[serde(deserialize_with = "schemas::email")]
    email: String,
    #[schema(schema_with = password_schema)]
    password: Secret,
    /// The one-time setup token from the setup token file, or the server log
    /// when there is no such file (or `SETUP_TOKEN`, when the operator set it)
    #[schema(schema_with = setup_token_schema)]
    setup_token: Secret,
}

fn setup_token_schema() -> Schema {
    schemas::secret_builder().min_length(Some(1)).max_length(Some(1024)).into()
}

impl Check for SetupBody {
    fn check(&self) -> Vec<FieldError> {
        password_problem("password", &self.password)
    }
}

pub(crate) fn login_field_schema() -> Schema {
    ObjectBuilder::new().schema_type(Type::String).min_length(Some(1)).max_length(Some(password::MAX_LENGTH)).into()
}

/// A password to check (sign-in, confirming the current one): as typed, never returned.
pub(crate) fn password_field_schema() -> Schema {
    schemas::secret_builder().min_length(Some(1)).max_length(Some(password::MAX_LENGTH)).into()
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoginBody {
    /// Case-insensitive
    #[schema(schema_with = login_field_schema)]
    username: String,
    #[schema(schema_with = password_field_schema)]
    password: Secret,
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
    #[schema(schema_with = password_field_schema)]
    current_password: Secret,
    #[schema(schema_with = password_schema)]
    new_password: Secret,
}

/// The e-mail an account created before e-mails were required enters at its
/// first sign-in after the upgrade
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EmailEntry {
    /// Unique regardless of case and Unicode form; a Person CI is created for
    /// it. Refused when a Person without an account already has the address:
    /// an administrator links the account to it.
    #[schema(schema_with = required_email_schema)]
    #[serde(deserialize_with = "schemas::email")]
    email: String,
}
impl Check for EmailEntry {}

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
    /// The account has no e-mail yet (created before e-mails were required):
    /// enter it with PUT /api/v1/auth/email; until then every other route but
    /// this one and sign-out answers 403 EMAIL_REQUIRED
    pub email_required: bool,
    /// Send as the X-CSRF-Token header on every POST, PUT, PATCH and DELETE
    /// (also readable from the shadoucmdb_csrf cookie, `__Host-shadoucmdb_csrf` behind HTTPS)
    pub csrf_token: String,
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

async fn session_dto(pool: &PgPool, user_id: Uuid, session_id: Uuid, csrf_token: String) -> Result<Session, AppError> {
    let mut conn = pool.acquire().await?;
    let user = users::load(&mut conn, user_id).await?;
    let permissions = data::load_permissions(&mut conn, user_id).await?;
    let mfa = mfa::status(&mut conn, user_id, Some(session_id)).await?;
    let email_required = user.email.is_none();
    Ok(Session { user, permissions: EffectivePermissions::from(&permissions), mfa, email_required, csrf_token })
}

/// Opens a session for the user and records `login.success`; returns its id and cookies.
///
/// `verified`: the `password_changed_at` of the password the sign-in was
/// checked against, None when an identity provider checked it. The user's row
/// is locked first; a password changed since, or an account disabled or
/// deleted since, gets no session but a 401 (GH#209), as does an account
/// whose identity provider was disabled or deleted since (GH#250), and a
/// password-only sign-in (`Password`, `Ldap`) to an account whose
/// authenticator was confirmed since (GH#303), and a second-factor sign-in
/// (`Totp`, `RecoveryCode`) to an account whose authenticator was reset or
/// turned off since (GH#341).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn open_session(
    pool: &PgPool,
    auth: &AuthState,
    headers: &HeaderMap,
    request: &RequestContext,
    user_id: Uuid,
    username: &str,
    method: LoginMethod,
    verified: Option<DateTime<Utc>>,
) -> Result<(Uuid, Vec<axum::http::HeaderValue>), AppError> {
    try_open_session(pool, auth, headers, request, user_id, username, method, verified).await?.map_err(AppError::from)
}

/// [`open_session`], with a refusal (already logged and recorded as
/// `login.failure`) as a value, for callers that answer it their own way.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn try_open_session(
    pool: &PgPool,
    auth: &AuthState,
    headers: &HeaderMap,
    request: &RequestContext,
    user_id: Uuid,
    username: &str,
    method: LoginMethod,
    verified: Option<DateTime<Utc>>,
) -> Result<Result<(Uuid, Vec<axum::http::HeaderValue>), Changed>, AppError> {
    let ctx = request.acting_as_user(user_id, username);
    let token = session::new_token();
    let csrf = session::new_token();
    let mut tx = pool.begin().await?;
    // First, so the row is locked before anything the reset or disable also takes.
    let stamp = data::record_login(&mut tx, user_id).await?;
    let mut changed = changed_since(&mut tx, stamp, verified).await?;
    // The authenticator as it is now, locked so that a disable (which locks
    // it first) waits for this transaction and then ends its session, or this
    // waits for the disable and finds it gone.
    let second_factor = matches!(method, LoginMethod::Totp | LoginMethod::RecoveryCode);
    if changed.is_none() && (second_factor || matches!(method, LoginMethod::Password | LoginMethod::Ldap)) {
        let confirmed = mfa_data::get_totp(&mut tx, user_id, true).await?.is_some_and(|t| t.confirmed);
        changed = match (second_factor, confirmed) {
            // A password alone was enough when checked; an authenticator
            // confirmed since asks for its code (GH#303).
            (false, true) => Some(Changed::MfaEnrolled),
            // The code proved an authenticator reset or turned off since,
            // whose sessions have already ended (GH#341).
            (true, false) => Some(Changed::MfaRemoved),
            _ => None,
        };
    }
    // An account with an e-mail but no Person cannot sign in (SHAA-1505
    // decision 8); one without an e-mail signs in and is asked for it.
    if changed.is_none() && !data::person_linked(&mut tx, user_id).await? {
        changed = Some(Changed::AccountIncomplete);
    }
    if let Some(changed) = changed {
        drop(tx);
        return Ok(Err(refused(pool, request, username, changed).await?));
    }
    // A cookie from an earlier session in this browser is replaced, not kept alive.
    if let Some(old) = session::session_token(&auth.config, headers)
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
        matches!(method, LoginMethod::Totp | LoginMethod::RecoveryCode | LoginMethod::Oidc(ProviderMfa::Verified)),
    )
    .await?;
    events::login_success(&mut tx, &ctx, session_id, user_id, username, method).await?;
    tx.commit().await?;
    Ok(Ok((session_id, session::login_cookies(&auth.config, auth.session_cookie_secure(headers), &token, &csrf))))
}

/// The caller's session under its new id: the token and CSRF token for its cookies.
pub(crate) struct Rotated {
    token: String,
    csrf: String,
}

impl Rotated {
    /// Set-Cookie headers replacing the old session and CSRF cookies.
    pub(crate) fn cookies(&self, auth: &AuthState, headers: &HeaderMap) -> Vec<axum::http::HeaderValue> {
        session::login_cookies(&auth.config, auth.session_cookie_secure(headers), &self.token, &self.csrf)
    }
}

/// Replaces the caller's session `session_id` with a new one (new id, token
/// and CSRF token) in the caller's transaction, and audits the old one as
/// `session.revoke` with reason `rotated`. For changes that re-prove who the
/// user is (their own password change, confirming an authenticator): a copy
/// of the old cookie must not survive them (GH#510). `mfa_verified`: the
/// request proved a second factor. 401 when the session ended meanwhile.
pub(crate) async fn rotate_own_session(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    session_id: Uuid,
    mfa_verified: bool,
) -> Result<Rotated, AppError> {
    let rotated = Rotated { token: session::new_token(), csrf: session::new_token() };
    let (new_id, old) = data::rotate_session(
        conn,
        session_id,
        &session::token_hash(&rotated.token),
        &rotated.csrf,
        ctx.client.user_agent.as_deref(),
        ctx.client.ip,
        mfa_verified,
    )
    .await?
    .ok_or_else(unauthenticated)?;
    events::rotated(conn, ctx, &old, new_id).await?;
    Ok(rotated)
}

/// Why a sign-in whose credentials were right when checked gets no session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Changed {
    /// The account was disabled, deleted or given a new password (GH#209).
    Account,
    /// The account's identity provider was disabled or deleted (GH#250).
    Provider,
    /// An authenticator was set up for the account after the password was
    /// checked; password sign-ins only (GH#303).
    MfaEnrolled,
    /// The authenticator was reset or turned off after its code was checked;
    /// second-factor sign-ins only (GH#341).
    MfaRemoved,
    /// The account has an e-mail but no linked Person (SHAA-1505 decision 8).
    /// Answered like wrong credentials; the reason is in the audit log.
    AccountIncomplete,
}

impl Changed {
    /// The `reason` of the `login.failure` row.
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Changed::Account => "account_changed",
            Changed::Provider => "provider_disabled",
            Changed::MfaEnrolled => "mfa_enrolled",
            Changed::MfaRemoved => "mfa_removed",
            Changed::AccountIncomplete => ACCOUNT_INCOMPLETE,
        }
    }
}

impl From<Changed> for AppError {
    fn from(changed: Changed) -> Self {
        let message = match changed {
            Changed::AccountIncomplete => return invalid_credentials(),
            Changed::Account => "The account was changed during the sign-in; enter your username and password again",
            Changed::MfaEnrolled => {
                "Two-factor authentication was set up for this account during the sign-in; sign in again and enter the code from your authenticator app"
            }
            Changed::MfaRemoved => {
                "Two-factor authentication was reset or turned off for this account during the sign-in; enter your username and password again"
            }
            Changed::Provider => {
                "The identity provider of this account was disabled during the sign-in; ask an administrator"
            }
        };
        AppError::new(ErrorCode::Unauthenticated, message)
    }
}

/// What changed since the sign-in's credentials were checked, given the
/// user's `stamp` as locked now; None if nothing did. The account's identity
/// provider is share-locked in turn, so disabling or deleting it waits for
/// this transaction (whose session it then ends) or this waits for that one.
async fn changed_since(
    conn: &mut PgConnection,
    stamp: Option<data::SignInStamp>,
    verified: Option<DateTime<Utc>>,
) -> sqlx::Result<Option<Changed>> {
    let Some(stamp) = stamp.filter(|now| now.allows(verified)) else { return Ok(Some(Changed::Account)) };
    match stamp.identity_provider_id {
        Some(provider) if !data::lock_provider_enabled(conn, provider).await? => Ok(Some(Changed::Provider)),
        _ => Ok(None),
    }
}

/// A sign-in refused because the account, its provider or its second factor
/// changed while it was checked (GH#209, GH#250, GH#303, GH#341), or the
/// account is incomplete (SHAA-1505): logged and audited.
async fn refused(pool: &PgPool, ctx: &RequestContext, username: &str, changed: Changed) -> Result<Changed, AppError> {
    tracing::warn!(user = %username, ip = ?ctx.client.ip, reason = changed.reason(), "sign-in refused: the account, its identity provider or its second factor changed while it was checked");
    record_failure(pool, ctx, username, Some(changed.reason()), None).await?;
    Ok(changed)
}

/// [`open_session`], answering with the session.
#[allow(clippy::too_many_arguments)]
async fn start_session(
    pool: &PgPool,
    auth: &AuthState,
    headers: &HeaderMap,
    ctx: &RequestContext,
    user_id: Uuid,
    username: &str,
    method: LoginMethod,
    verified: Option<DateTime<Utc>>,
) -> Result<WithCookies<Json<Session>>, AppError> {
    let (session_id, cookies) = open_session(pool, auth, headers, ctx, user_id, username, method, verified).await?;
    let csrf = [session::HOST_CSRF_COOKIE, session::CSRF_COOKIE]
        .into_iter()
        .find_map(|name| session::cookie_value(&cookies[1], name))
        .unwrap_or_default();
    Ok(WithCookies(Json(session_dto(pool, user_id, session_id, csrf).await?), cookies))
}

/// The one key of [`AuthState::setup_throttle`].
const SETUP_THROTTLE_KEY: &str = "setup";

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
    // Unthrottled: an installed system must answer without taking any lock.
    if !setup_required(pool).await? {
        return Err(setup_done());
    }
    // Wrong tokens lock setup for the client's network like wrong passwords
    // lock a username, and for every network past the account budget (GH#230).
    let attempt = throttle_gate(&auth.setup_throttle, SETUP_THROTTLE_KEY, request.client.net, "setup attempts").await?;
    // Arms the token if the database was not reachable when the server started.
    auth.setup.arm();
    if !auth.setup.matches(&b.setup_token) {
        attempt.failure();
        auth.setup.refused(request.client.ip);
        return Err(wrong_setup_token());
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
    attempt.success();
    auth.setup.disarm();
    tracing::info!(user = %user.username, "first-run setup created the first administrator");
    let verified = Some(user.password_changed_at);
    start_session(pool, auth, headers, request, user.id, &user.username, LoginMethod::Setup, verified).await
}

fn setup_done() -> AppError {
    AppError::conflict("Setup is already complete; sign in instead")
}

fn wrong_setup_token() -> AppError {
    const MESSAGE: &str = "The setup token is missing or wrong. The server writes it to the setup token file \
                           (SETUP_TOKEN_FILE) when it starts without users, or to its log if there is no such \
                           file; or use the SETUP_TOKEN you set";
    let mut err = AppError::new(ErrorCode::Forbidden, MESSAGE);
    err.details = Some(vec![FieldError {
        location: FieldLocation::Body,
        field: "setupToken".into(),
        message: "Does not match the setup token of this server".into(),
        code: "setup_token".into(),
    }]);
    err
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
/// get past the gate before the first failure is counted. `net`: the client's
/// network, so failures from one network do not lock the key for the others.
async fn throttle_gate<'a>(
    throttle: &'a LoginThrottle,
    key: &str,
    net: Net,
    what: &str,
) -> Result<Attempt<'a>, AppError> {
    let locked = |wait| rate_limited(wait, &format!("Too many failed {what}"));
    match throttle.begin(key, net, false) {
        Ok(attempt) => Ok(attempt),
        Err(Gate::Locked(wait)) => Err(locked(wait)),
        Err(Gate::Open | Gate::Slow) => {
            if !throttle.slow_lane(net).await {
                let wait = GLOBAL_PENALTY * SLOW_LANE_WAITERS as u32;
                return Err(rate_limited(wait, "Too many sign-ins are waiting on this server"));
            }
            // Failures for this key may have locked it while it waited.
            throttle.begin(key, net, true).map_err(|gate| match gate {
                Gate::Locked(wait) => locked(wait),
                Gate::Open | Gate::Slow => locked(GLOBAL_PENALTY),
            })
        }
    }
}

/// The throttle reservations of one sign-in: the name typed and, once a
/// directory resolved it, the directory entry it found (GH#406). Counted
/// together: a failure for both, a success for both.
struct Reservation<'a> {
    name: Attempt<'a>,
    entry: Option<Attempt<'a>>,
}

impl<'a> From<Attempt<'a>> for Reservation<'a> {
    fn from(name: Attempt<'a>) -> Self {
        Reservation { name, entry: None }
    }
}

impl Reservation<'_> {
    /// Records the failure; returns the longer lock it triggered, if any.
    fn failure(self) -> Option<Duration> {
        let name = self.name.failure();
        self.entry.and_then(Attempt::failure).max(name)
    }

    fn success(self) {
        self.name.success();
        if let Some(entry) = self.entry {
            entry.success();
        }
    }
}

/// The key a directory entry's sign-ins are throttled under: the directory
/// and the entry's stable id, whatever name found it.
fn entry_key(provider: Uuid, external_id: &str) -> String {
    format!("{provider} {external_id}")
}

/// Reserves an attempt for the entry `external_id` of directory `provider`;
/// `None` while the entry is locked (or all its free failures are in flight).
fn admit_entry<'a>(throttle: &'a LoginThrottle, net: Net, provider: Uuid, external_id: &str) -> Option<Attempt<'a>> {
    throttle.begin(&entry_key(provider, external_id), net, false).ok()
}

/// Whether a name may be looked up, locally or in a directory: one a
/// ShadouCMDB account could have (`USERNAME_PATTERN`, so no control, format or
/// non-ASCII characters). Other spellings a directory (GH#406) or PostgreSQL's
/// `lower()` (GH#438: `admİn` finds `admin`) might match to an account would
/// each get their own throttle budget, so they are refused like an unknown
/// name, before any lookup.
fn account_name(typed: &str) -> Option<&str> {
    let name = typed.trim();
    crate::api::validate::cached_regex(schemas::USERNAME_PATTERN).filter(|re| re.is_match(name)).map(|_| name)
}

fn invalid_credentials() -> AppError {
    AppError::new(ErrorCode::Unauthenticated, "Invalid username or password")
}

/// Records `login.failure` (with `reason` when valid credentials were still
/// refused), and `login.locked` when this failure set a lock.
async fn record_failure(
    pool: &PgPool,
    ctx: &RequestContext,
    username: &str,
    reason: Option<&str>,
    locked: Option<Duration>,
) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    let attempt = events::login_failure(&mut tx, ctx, username, reason).await?;
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

/// Sign-in with a password. Every refusal (401: a wrong password, an unknown
/// name, a disabled account, a directory's refusal) answers no earlier than
/// `SIGN_IN_FAILURE_FLOOR_MS` after the throttle let it through, plus up to 5 %
/// jitter: otherwise the directory round trip an unknown name costs would tell
/// local accounts apart (GH#216). The failure is recorded before the wait, and
/// the wait itself is `api::route`'s ([`AppError::hold_until`]), once this has
/// returned its database connections and throttle reservation. So is the 503
/// for a directory account whose directory cannot be reached (GH#586). 429
/// answers, a second factor due and a success are not held.
async fn login(
    pool: &PgPool,
    auth: &AuthState,
    headers: &HeaderMap,
    ctx: &RequestContext,
    b: LoginBody,
) -> Result<LoginAnswer, AppError> {
    let attempt = throttle_gate(&auth.throttle, &b.username, ctx.client.net, "sign-ins for this username").await?;
    let start = tokio::time::Instant::now();
    check_login(pool, auth, headers, ctx, attempt, b).await.map_err(|mut e| {
        if matches!(e.code, ErrorCode::Unauthenticated | ErrorCode::IdentityProviderUnavailable) {
            e.hold_until = Some(start + with_jitter(auth.config.sign_in_failure_floor));
        }
        e
    })
}

/// `floor` plus a random 0-5 % of it.
fn with_jitter(floor: Duration) -> Duration {
    let mut bytes = [0u8; 2];
    getrandom::fill(&mut bytes).expect("OS random number generator");
    floor + floor * u32::from(u16::from_le_bytes(bytes)) / (20 * u32::from(u16::MAX))
}

async fn check_login(
    pool: &PgPool,
    auth: &AuthState,
    headers: &HeaderMap,
    ctx: &RequestContext,
    attempt: Attempt<'_>,
    b: LoginBody,
) -> Result<LoginAnswer, AppError> {
    // A name no account could have finds none, as for an unknown name (GH#438).
    let row = match account_name(&b.username) {
        Some(_) => data::find_for_login(pool, &b.username).await?,
        None => None,
    };
    // Directory accounts, and names no account has while a directory is enabled, go to LDAP.
    let directory = match &row {
        Some(r) => r.provider.as_ref().filter(|(_, kind)| kind == sso::LDAP).map(|(id, _)| Some(*id)),
        None => sso::any_directory(pool).await?.then_some(None),
    };
    let account = row.as_ref().map(|r| r.username.as_str());
    if let Some(linked) = directory {
        return directory_login(pool, auth, headers, ctx, attempt, &b, linked, account).await;
    }
    // An OIDC account has no password: verify() then checks a dummy hash, so it takes as long.
    if !password::verify(&b.password, row.as_ref().and_then(|r| r.password_hash.as_deref())).await? {
        return Err(wrong_credentials(pool, attempt.into(), ctx, &b.username, account, None).await?);
    }
    let Some(user) = row else { return Err(invalid_credentials()) };
    if !user.is_active {
        // Treated exactly like a wrong password: same throttle, same lock, same
        // answer. Otherwise the right password for a disabled account would be
        // an unthrottled way to grow audit_log, and the answer would confirm
        // the password (GH#437). The reason is in the log and the audit row.
        let locked = attempt.failure();
        tracing::warn!(username = %user.username, ip = ?ctx.client.ip, locked_secs = locked.map(|d| d.as_secs()), "sign-in to a disabled account");
        record_failure(pool, ctx, &b.username, Some(ACCOUNT_DISABLED), locked).await?;
        return Err(invalid_credentials());
    }
    if !user.person_linked {
        // An account with an e-mail but no Person (SHAA-1505 decision 8):
        // answered and throttled like a disabled one; the reason is in the log
        // and the audit row, and the Users page shows the account as incomplete.
        let locked = attempt.failure();
        tracing::warn!(username = %user.username, ip = ?ctx.client.ip, locked_secs = locked.map(|d| d.as_secs()), "sign-in to an incomplete account (no linked person)");
        record_failure(pool, ctx, &b.username, Some(ACCOUNT_INCOMPLETE), locked).await?;
        return Err(invalid_credentials());
    }
    let verified = Some(user.password_changed_at);
    let attempt = attempt.into();
    password_accepted(pool, auth, headers, ctx, attempt, user.id, &user.username, LoginMethod::Password, verified).await
}

/// After a right password, local or directory: the second-factor challenge
/// when the user has set up MFA (401 MFA_REQUIRED and the `shadoucmdb_mfa`
/// cookie), otherwise the session. `attempt`: the throttle reservations for
/// the name signed in with, counted a success only once the sign-in is complete.
/// `verified`: as for [`open_session`]; the challenge is refused alike.
#[allow(clippy::too_many_arguments)]
async fn password_accepted(
    pool: &PgPool,
    auth: &AuthState,
    headers: &HeaderMap,
    ctx: &RequestContext,
    attempt: Reservation<'_>,
    user_id: Uuid,
    username: &str,
    method: LoginMethod,
    verified: Option<DateTime<Utc>>,
) -> Result<LoginAnswer, AppError> {
    if mfa_data::get_totp(&mut *pool.acquire().await?, user_id, false).await?.is_some_and(|t| t.confirmed) {
        // The username's failure count is left alone (the attempt is dropped):
        // were a right password to clear it, each one would buy a fresh set of
        // guesses at the code.
        mfa_data::purge_challenges(pool).await?;
        let token = session::new_token();
        let mut tx = pool.begin().await?;
        // A reset or disable deletes the user's challenges under its lock on
        // the row: one committed after this check finds the challenge.
        let stamp = data::lock_sign_in(&mut tx, user_id).await?;
        if let Some(changed) = changed_since(&mut tx, stamp, verified).await? {
            drop(tx);
            return Err(refused(pool, ctx, username, changed).await?.into());
        }
        mfa_data::create_challenge(&mut tx, user_id, &session::token_hash(&token), MFA_CHALLENGE_TTL).await?;
        tx.commit().await?;
        tracing::info!(user = %username, ip = ?ctx.client.ip, method = ?method, "password accepted, second factor due");
        let err = AppError::new(
            ErrorCode::MfaRequired,
            "Enter the code from your authenticator app, or a recovery code (POST /api/v1/auth/login/mfa)",
        );
        let cookie = session::mfa_cookie(auth.session_cookie_secure(headers), &token, MFA_CHALLENGE_TTL);
        return Ok(Either::Right(ErrorWithCookies(err, vec![cookie])));
    }
    let purged = data::purge_sessions(pool, auth.config.session_idle).await?;
    let session = start_session(pool, auth, headers, ctx, user_id, username, method, verified).await?;
    attempt.success();
    tracing::info!(user = %username, ip = ?ctx.client.ip, method = ?method, purged_sessions = purged, "signed in");
    Ok(Either::Left(session))
}

/// A wrong password (or unknown name): counted, logged and audited (with
/// `reason` when the password was not checked); returns the 401.
/// `account`: the name of the account `username` matched, if any.
///
/// The log line names the account only when one exists (GH#415): a name that
/// matches none is often a password typed into the wrong field, and the
/// server log is read more widely than the audit log, so it gets
/// `unknown_user=true` instead. `request_id` leads to the `login.failure`
/// audit row, which keeps the name as typed (see [`events`]).
async fn wrong_credentials(
    pool: &PgPool,
    attempt: Reservation<'_>,
    ctx: &RequestContext,
    username: &str,
    account: Option<&str>,
    reason: Option<&str>,
) -> Result<AppError, AppError> {
    let locked = attempt.failure();
    // One call site: a field set to `None` is left out of the line.
    let unknown_user = account.is_none().then_some(true);
    tracing::warn!(username = account, unknown_user, ip = ?ctx.client.ip, request_id = %ctx.request_id, locked_secs = locked.map(|d| d.as_secs()), reason, "sign-in failed");
    record_failure(pool, ctx, username, reason, locked).await?;
    Ok(invalid_credentials())
}

/// Sign-in with a directory password, under the same throttle as local
/// passwords, and with the second factor when the user has set one up. Also
/// throttled per directory entry (GH#406): a directory may resolve many
/// spellings of a name to one entry, and each spelling would otherwise get
/// its own budget. A locked entry's password is not checked; the answer is
/// the one a wrong password gets, so it does not tell which spellings find
/// an entry. `account`: the name of the local account the username matched,
/// if any.
#[allow(clippy::too_many_arguments)]
async fn directory_login(
    pool: &PgPool,
    auth: &AuthState,
    headers: &HeaderMap,
    ctx: &RequestContext,
    attempt: Attempt<'_>,
    b: &LoginBody,
    linked: Option<Uuid>,
    account: Option<&str>,
) -> Result<LoginAnswer, AppError> {
    let mut entry = None;
    let answer = match account_name(&b.username) {
        Some(name) => {
            let mut admit = |provider: Uuid, external_id: &str| {
                entry = admit_entry(&auth.directory_throttle, ctx.client.net, provider, external_id);
                entry.is_some()
            };
            sso::directory_sign_in(pool, &auth.keyring, ctx, name, &b.password, linked, &mut admit).await?
        }
        None => sso::DirectoryAnswer::NoMatch,
    };
    let attempt = Reservation { name: attempt, entry };
    directory_answer(pool, auth, headers, ctx, attempt, b, account, answer).await
}

/// What the directory's answer makes of the sign-in.
#[allow(clippy::too_many_arguments)]
async fn directory_answer(
    pool: &PgPool,
    auth: &AuthState,
    headers: &HeaderMap,
    ctx: &RequestContext,
    attempt: Reservation<'_>,
    b: &LoginBody,
    account: Option<&str>,
    answer: sso::DirectoryAnswer,
) -> Result<LoginAnswer, AppError> {
    match answer {
        sso::DirectoryAnswer::SignedIn { user_id, username } => {
            password_accepted(pool, auth, headers, ctx, attempt, user_id, &username, LoginMethod::Ldap, None).await
        }
        sso::DirectoryAnswer::NoMatch => {
            // A local account's wrong password costs an argon2 verify; so does
            // this answer, or its speed would tell the names apart (GH#190).
            password::verify(&b.password, None).await?;
            Err(wrong_credentials(pool, attempt, ctx, &b.username, account, None).await?)
        }
        sso::DirectoryAnswer::NotAdmitted => {
            password::verify(&b.password, None).await?;
            Err(wrong_credentials(pool, attempt, ctx, &b.username, account, Some(DIRECTORY_ENTRY_LOCKED)).await?)
        }
        // A right password that is still refused counts like a disabled
        // account's, and gets a wrong password's answer: the refusal's own
        // text would confirm the directory password (GH#437).
        sso::DirectoryAnswer::Refused(refusal) => {
            let locked = attempt.failure();
            tracing::warn!(username = %b.username, ip = ?ctx.client.ip, locked_secs = locked.map(|d| d.as_secs()), reason = refusal.code(), "directory sign-in refused");
            record_failure(pool, ctx, &b.username, Some(refusal.code()), locked).await?;
            Err(invalid_credentials())
        }
        // A name no account has gets an unknown name's answer: a 503 for it
        // alone would tell, quickly and unthrottled, which names are local
        // accounts while a directory is down (GH#499).
        sso::DirectoryAnswer::Unavailable if account.is_none() => {
            password::verify(&b.password, None).await?;
            Err(wrong_credentials(pool, attempt, ctx, &b.username, None, Some(DIRECTORY_UNAVAILABLE)).await?)
        }
        // A directory account's own directory. The 503 tells the name is a
        // directory account (accepted in GH#499), but not for free: counted,
        // audited and held like a wrong password, so an outage does not let
        // anyone list those names unthrottled and unseen (GH#586).
        sso::DirectoryAnswer::Unavailable => {
            wrong_credentials(pool, attempt, ctx, &b.username, account, Some(DIRECTORY_UNAVAILABLE)).await?;
            Err(AppError::new(
                ErrorCode::IdentityProviderUnavailable,
                "The directory service could not be reached; try again shortly, or sign in with a local account",
            ))
        }
    }
}

/// `login.failure` reason: a directory that might know the name could not be
/// reached, so the password was not checked (GH#499).
const DIRECTORY_UNAVAILABLE: &str = "directory_unavailable";

/// `login.failure` reason: the right password for a disabled local account.
const ACCOUNT_DISABLED: &str = "account_disabled";
/// The `login.failure` reason for an account with an e-mail but no Person.
pub(crate) const ACCOUNT_INCOMPLETE: &str = "account_incomplete";

/// `login.failure` reason: the name found a directory entry whose sign-ins
/// are locked, so the password was not checked (GH#406).
const DIRECTORY_ENTRY_LOCKED: &str = "directory_entry_locked";

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
    let Some(token) = session::second_factor_challenge(&auth.config, headers) else { return Err(sign_in_expired()) };
    let hash = session::token_hash(token);
    // Wait for the username's turn before locking anything.
    let Some(pending) = mfa_data::take_challenge(&mut *pool.acquire().await?, &hash).await? else {
        return Err(sign_in_expired());
    };
    let attempt =
        throttle_gate(&auth.throttle, &pending.username, ctx.client.net, "sign-ins for this username").await?;

    let mut tx = pool.begin().await?;
    // The user's row before the challenge's, in the order of a reset or
    // disable. While the challenge is there, neither has committed since the
    // password was checked: the session is opened only if none has until then.
    let Some(verified) = data::lock_sign_in(&mut tx, pending.user_id).await? else { return Err(sign_in_expired()) };
    let Some(challenge) = mfa_data::take_challenge(&mut tx, &hash).await? else { return Err(sign_in_expired()) };
    let (user_id, username) = (challenge.user_id, challenge.username.as_str());
    let as_user = ctx.acting_as_user(user_id, username);
    let method = match mfa::verify_second_factor(&mut tx, &auth.keyring, user_id, &b.code).await? {
        mfa::Verdict::Accepted(method) => method,
        refused => {
            mfa_data::challenge_failed(&mut tx, challenge.id, MFA_CHALLENGE_ATTEMPTS).await?;
            let locked = attempt.failure();
            tracing::warn!(user = %username, ip = ?ctx.client.ip, locked_secs = locked.map(|d| d.as_secs()), "sign-in: wrong second factor");
            let mut extra = serde_json::json!({ "stage": "login" });
            if let Some(reason) = refused.failure_reason() {
                extra["reason"] = reason.into();
            }
            events::mfa(&mut tx, ctx, AuditAction::MfaFailure, user_id, username, extra).await?;
            if let Some(lock) = locked {
                events::login_locked(&mut tx, ctx, Uuid::new_v4(), username, lock).await?;
            }
            tx.commit().await?;
            return Err(AppError::new(ErrorCode::Unauthenticated, "The code is wrong or was already used"));
        }
    };
    mfa_data::delete_challenge(&mut tx, challenge.id).await?;
    if matches!(method, LoginMethod::RecoveryCode) {
        mfa::audit_recovery_code_used(&mut tx, &as_user, user_id, username, "login").await?;
    }
    tx.commit().await?;
    let purged = data::purge_sessions(pool, auth.config.session_idle).await?;
    let verified = Some(verified.password_changed_at);
    // A reset or disable committed from here on is caught when the session opens (GH#341).
    let WithCookies(session, mut cookies) =
        start_session(pool, auth, headers, ctx, user_id, username, method, verified).await?;
    attempt.success();
    tracing::info!(user = %username, ip = ?ctx.client.ip, purged_sessions = purged, "signed in with a second factor");
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

/// Waits for the per-user password-confirmation throttle. Shared by every
/// kind of account, so a stolen session cannot be turned into a known
/// password, local or directory, by guessing the current one. Not per
/// network: only the signed-in user can lock their own key.
pub(crate) async fn password_gate<'a>(auth: &'a AuthState, me: &Principal) -> Result<Attempt<'a>, AppError> {
    let key = me.user_id.to_string();
    throttle_gate(&auth.password_throttle, &key, Net::default(), "attempts at your current password").await
}

/// Counts a wrong current password against the user; returns the 400.
fn wrong_current_password(attempt: Attempt<'_>, me: &Principal) -> AppError {
    let locked = attempt.failure();
    tracing::warn!(user = %me.username, locked_secs = locked.map(|d| d.as_secs()), "wrong current password");
    AppError::field("currentPassword", "The current password is wrong", "invalid_credentials")
}

/// Ends a verified password attempt for a route that asks no second factor.
///
/// With MFA set up, a right password leaves the per-user count alone, as at
/// login: were it to clear it, each right password would buy a fresh set of
/// guesses at the code (GH#141). The count is then cleared only by a right
/// password together with a right code ([`confirm_current_password_attempt`]).
async fn password_only_success(pool: &PgPool, attempt: Attempt<'_>, me: &Principal) -> Result<(), AppError> {
    if !mfa_data::get_totp(&mut *pool.acquire().await?, me.user_id, false).await?.is_some_and(|t| t.confirmed) {
        attempt.success();
    }
    Ok(())
}

/// Checks the signed-in user's password before changing it. Throttled per
/// user like login. An account of an identity provider has no password here
/// (a directory password is changed in the directory): 409.
async fn check_current_password(
    pool: &PgPool,
    auth: &AuthState,
    me: &Principal,
    current_password: &str,
) -> Result<(), AppError> {
    let hash = data::password_hash(&mut *pool.acquire().await?, me.user_id).await?;
    if let Some(None) = hash {
        return Err(AppError::conflict(
            "Your account signs in through an identity provider and has no password here; change it there",
        ));
    }
    let attempt = password_gate(auth, me).await?;
    if !password::verify(current_password, hash.flatten().as_deref()).await? {
        return Err(wrong_current_password(attempt, me));
    }
    password_only_success(pool, attempt, me).await
}

/// Confirms who is at the keyboard before an MFA change, like
/// [`confirm_current_password_attempt`], for a route that asks no second
/// factor (starting a set-up).
pub(crate) async fn confirm_current_password(
    pool: &PgPool,
    auth: &AuthState,
    me: &Principal,
    current_password: &str,
) -> Result<(), AppError> {
    let attempt = confirm_current_password_attempt(pool, auth, me, current_password).await?;
    password_only_success(pool, attempt, me).await
}

/// Confirms who is at the keyboard before an MFA change: the local password,
/// or for a directory account the password of their directory entry (asked
/// of that directory only). An OIDC account has no password to confirm: 409.
/// Under the same per-user throttle as [`check_current_password`].
///
/// Leaves the verified attempt open: the caller reports how it ended once the
/// second factor has been checked too, so a wrong code counts against the
/// same lock.
pub(crate) async fn confirm_current_password_attempt<'a>(
    pool: &PgPool,
    auth: &'a AuthState,
    me: &Principal,
    current_password: &str,
) -> Result<Attempt<'a>, AppError> {
    let account = data::password_check(&mut *pool.acquire().await?, me.user_id).await?;
    let attempt = password_gate(auth, me).await?;
    let right = match account.as_ref().map(|a| (a, a.provider.as_ref())) {
        Some((a, Some((provider_id, kind)))) if kind == sso::LDAP => {
            let external_id = a.external_id.as_deref().unwrap_or_default();
            match sso::directory_reauthenticate(
                pool,
                &auth.keyring,
                *provider_id,
                &a.username,
                external_id,
                current_password,
            )
            .await?
            {
                sso::Reauth::Accepted => true,
                sso::Reauth::Wrong => false,
                sso::Reauth::Disabled => {
                    return Err(AppError::conflict(
                        "Your account's directory is disabled, so your password cannot be confirmed; ask an administrator",
                    ));
                }
                sso::Reauth::Unavailable => {
                    return Err(AppError::new(
                        ErrorCode::IdentityProviderUnavailable,
                        "The directory service could not be reached to confirm your password; try again shortly",
                    ));
                }
            }
        }
        Some((_, Some(_))) => {
            return Err(AppError::conflict(
                "Your account signs in through an identity provider and has no password here; its sign-in and second factor are managed there",
            ));
        }
        _ => password::verify(current_password, account.and_then(|a| a.password_hash).as_deref()).await?,
    };
    if !right {
        return Err(wrong_current_password(attempt, me));
    }
    Ok(attempt)
}

/// The e-mail an account without one enters (SHAA-1505 decision 9): set once,
/// linked to its Person in the same transaction. Changing it afterwards needs
/// users.manage (Administration > Users).
async fn enter_email(pool: &PgPool, ctx: &RequestContext, b: EmailEntry) -> Result<Session, AppError> {
    let me = principal(ctx)?;
    let session_id = me.session_id().ok_or_else(unauthenticated)?;
    let csrf_token = me.csrf_token().ok_or_else(unauthenticated)?.to_owned();
    let mut tx = pool.begin().await?;
    let before = users::lock_for_update(&mut tx, me.user_id).await?;
    if before.email.is_some() {
        return Err(AppError::conflict(
            "Your account already has an e-mail address; an administrator can change it (Administration > Users)",
        ));
    }
    data::set_email(&mut tx, me.user_id, &b.email).await.map_err(AppError::from)?;
    people::link(&mut tx, ctx, me.user_id, people::Linking::SelfService).await?;
    let after = users::load(&mut tx, me.user_id).await?;
    let entry = AuditEntry {
        action: AuditAction::Update,
        entity_type: "users",
        entity_id: me.user_id,
        old_value: Some(crud::json(&before)),
        new_value: Some(crud::json(&after)),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    tx.commit().await?;
    tracing::info!(user = %me.username, "account entered its e-mail address");
    session_dto(pool, me.user_id, session_id, csrf_token).await
}

async fn change_password(
    pool: &PgPool,
    auth: &AuthState,
    ctx: &RequestContext,
    headers: &HeaderMap,
    b: PasswordChange,
) -> Result<Vec<axum::http::HeaderValue>, AppError> {
    let me = principal(ctx)?;
    check_current_password(pool, auth, me, &b.current_password).await?;
    let (_, rotated) = users::set_password(pool, ctx, me.user_id, &b.new_password).await?;
    Ok(rotated.map(|r| r.cookies(auth, headers)).unwrap_or_default())
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
                let setup_required = setup_required(&api.pool).await?;
                if setup_required {
                    api.auth.setup.arm();
                }
                Ok(Json(SetupStatus { setup_required }))
            }),
        route(Method::POST, "/api/v1/setup", "completeSetup")
            .tag(TAG)
            .summary("Create the first administrator and sign them in (only while no users exist)")
            .description(
                "The new user holds the built-in Administrator profile. 409 once any user exists. `setupToken` must be the one-time token the server writes to the setup token file (`SETUP_TOKEN_FILE`) when it runs without users, or to its log when there is no such file, or the operator's `SETUP_TOKEN`; 403 FORBIDDEN when it is missing or wrong. The first 4 wrong tokens from one client network (the IPv4 /24 or IPv6 /64 of the client address) cost nothing; from the 5th on, each one locks setup for that network for 1 s, 2 s, 4 s, ... up to 15 min, and wrong tokens from several networks that add up to 15 lock it for every network; while locked the answer is 429 RATE_LIMITED with Retry-After and the token is not checked. The 409 answer is never throttled. The token stops working once the first administrator exists. Sets the session and CSRF cookies. `shadoucmdb create-admin` does the same from the command line and needs no token.",
            )
            .public()
            .status(StatusCode::CREATED)
            .errors(&[ErrorCode::Forbidden, ErrorCode::Conflict, ErrorCode::RateLimited])
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<SetupBody>>| async move {
                setup(&api.pool, &api.auth, &api.headers, &api.ctx, b).await
            }),
        route(Method::POST, "/api/v1/auth/login", "login")
            .tag(TAG)
            .summary("Sign in with username and password")
            .description(
                "Sets the `shadoucmdb_session` cookie (HttpOnly, SameSite=Lax) and the `shadoucmdb_csrf` cookie; behind HTTPS they are `Secure` and named `__Host-shadoucmdb_session` and `__Host-shadoucmdb_csrf`. 401 for a wrong username or password. When the user has set up two-factor authentication, a right password answers 401 MFA_REQUIRED instead and sets the `shadoucmdb_mfa` cookie (HttpOnly, 5 min; `__Host-shadoucmdb_mfa` behind HTTPS): send the code to POST /api/v1/auth/login/mfa. Every 401 for a wrong username or password, a disabled account or a directory's refusal, and every 503 IDENTITY_PROVIDER_UNAVAILABLE, is answered no earlier than `SIGN_IN_FAILURE_FLOOR_MS` (default 1 s) after the throttle let the attempt through, so response times do not tell which names are accounts; 429, MFA_REQUIRED and successful answers are not delayed. The first 4 failures for a username from one client network (the IPv4 /24 or IPv6 /64 of the TCP peer address or, when the peer is listed in `TRUSTED_PROXIES`, of the client address the proxies report) cost nothing; from the 5th on, each failure locks it for that network for 1 s, 2 s, 4 s, ... up to 15 min; while locked the answer is 429 RATE_LIMITED with Retry-After. Other networks are not locked, so guessing cannot lock the account holder out; only failures from several networks that add up to 15 (at most 5 counted per network) lock the username for every network, with the same backoff. Attempts for a username that arrive while as many earlier ones as it has free failures left are still being checked are answered 429 with Retry-After: 1. Once 300 failures in 10 min for all usernames together are reached, sign-in is slowed rather than refused: attempts queue and go through one per 2 s (a correct password still signs in); only when 64 are already queued, or 4 from the same client network, is the next one answered 429. When an LDAP/AD directory is enabled, directory accounts sign in here too with their directory password (see GET /api/v1/auth/providers): a name no local account has is looked up in the enabled directories in order, under the same throttle; 503 IDENTITY_PROVIDER_UNAVAILABLE when the directory a directory account belongs to cannot be reached; it counts as a failed sign-in for the throttle and is recorded as `login.failure` with reason `directory_unavailable`, like a wrong password. While a directory cannot be reached, a name no account has gets the 401 for a wrong username or password, so the answer does not tell which names are local accounts. A directory account that has set up two-factor authentication gets MFA_REQUIRED after the directory password, like a local one. Accounts of an OIDC provider cannot sign in here.",
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
                "After POST /api/v1/auth/login answered MFA_REQUIRED: reads the `shadoucmdb_mfa` cookie it set (`__Host-shadoucmdb_mfa` behind HTTPS) and, for a right code, sets the session cookies like login. Each authenticator code works once; each recovery code works once and is then used up. 401 for a wrong code; after 5 wrong codes, or 5 minutes, the password is asked for again (401). Wrong codes count as failed sign-ins for the username: the same lock applies as for wrong passwords (429 RATE_LIMITED with Retry-After).",
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
            .before_email_entry()
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
            .before_email_entry()
            .handle(|api, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| async move {
                let me = principal(&api.ctx)?;
                let csrf_token = me.csrf_token().ok_or_else(unauthenticated)?.to_owned();
                let session_id = me.session_id().ok_or_else(unauthenticated)?;
                Ok(Json(session_dto(&api.pool, me.user_id, session_id, csrf_token).await?))
            }),
        route(Method::PUT, "/api/v1/auth/email", "enterOwnEmail")
            .tag(TAG)
            .summary("Enter the e-mail address of your account (accounts without one only)")
            .description("For an account created before e-mail addresses were required (`emailRequired` in GET /api/v1/auth/me): until it has one, every other route but GET /api/v1/auth/me and sign-out answers 403 EMAIL_REQUIRED. The address must be unique regardless of case and Unicode form (409 CONFLICT otherwise); a Person CI is created for it. A user cannot take over an existing Person: when a Person without an account already has the address, the request is refused with 409 CONFLICT (`person_email_taken`) and an administrator sets the address on the account (PATCH /api/v1/admin/users/{id}), which links it to that Person. Answers the session as GET /api/v1/auth/me does. 409 CONFLICT for an account that already has an e-mail: an administrator changes it (PATCH /api/v1/admin/users/{id}).")
            .session_only()
            .before_mfa_enrolment()
            .before_email_entry()
            .errors(&[ErrorCode::Conflict])
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<EmailEntry>>| async move {
                Ok(Json(enter_email(&api.pool, &api.ctx, b).await?))
            }),
        route(Method::PUT, "/api/v1/auth/password", "changeOwnPassword")
            .tag(TAG)
            .summary("Change your own password (ends your other sessions, renews this one and revokes your API tokens)")
            .session_only()
            .before_mfa_enrolment()
            .description(
                "Every API token you own that still works is revoked; create new ones after the change. This session continues under a new session and CSRF token: the response sets both cookies again, the old session cookie stops working (audited as `session.revoke`, reason rotated, with `replacedBy`), and GET /api/v1/auth/me returns the new CSRF token. 400 when `currentPassword` is wrong; 409 for an account that signs in through an identity provider. The first 4 wrong current passwords cost nothing; from the 5th on, each one locks password changes for this user for 1 s, 2 s, 4 s, ... up to 15 min; while locked the answer is 429 RATE_LIMITED with Retry-After.",
            )
            .errors(&[ErrorCode::RateLimited, ErrorCode::Conflict])
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<PasswordChange>>| async move {
                let cookies = change_password(&api.pool, &api.auth, &api.ctx, &api.headers, b).await?;
                Ok(WithCookies(NoContent, cookies))
            }),
    ]
}

#[cfg(test)]
pub(crate) mod tests {
    use sqlx::Executor;

    use super::*;
    use crate::auth::Credential;
    use crate::config::{AuthConfig, CookieSecure};
    use crate::db::scratch;

    fn auth_state() -> AuthState {
        AuthState::new(
            AuthConfig {
                session_idle: Duration::from_secs(3600),
                session_max_age: Duration::from_secs(3600),
                cookie_secure: CookieSecure::Never,
                public_url: None,
                oidc_allowed_hosts: None,
                setup_token: Some(crate::auth::setup_token::TEST_TOKEN.into()),
                setup_token_file: None,
                trusted_proxies: Default::default(),
                sign_in_failure_floor: Duration::ZERO,
            },
            crate::secrets::Keyring::for_tests(),
        )
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
            email: format!("{username}@example.test"),
            password: OWNER_PASSWORD.clone().into(),
            setup_token: crate::auth::setup_token::TEST_TOKEN.into(),
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
        let writer_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&mut *writer).await.unwrap();
        // Watch for the lock wait itself rather than timing the call (GH#736):
        // under CPU load the 409 path can be slow without waiting on anything,
        // while a real lock wait lasts as long as the writer stays open.
        let ctx = anon();
        let late = {
            let call = setup(pool, &auth, &headers, &ctx, body("late"));
            tokio::pin!(call);
            let watch = async {
                loop {
                    tokio::select! {
                        done = &mut call => break done,
                        () = tokio::time::sleep(Duration::from_millis(20)) => {
                            let blocked: bool = sqlx::query_scalar(
                                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid)))",
                            )
                            .bind(writer_pid)
                            .fetch_one(pool)
                            .await
                            .unwrap();
                            assert!(!blocked, "setup waited on a lock held by a users writer");
                        }
                    }
                }
            };
            tokio::time::timeout(Duration::from_secs(120), watch).await.expect("setup hung")
        };
        assert_eq!(late.err().map(|e| e.code), Some(ErrorCode::Conflict));
        writer.rollback().await.unwrap();
        db.drop().await;
    }

    /// GitHub #192 / T14: a fresh install cannot be claimed without the
    /// generated one-time token, which only the operator can read (log, file).
    #[tokio::test]
    async fn setup_requires_the_generated_one_time_token() {
        let Some(db) = scratch::database("setup_requires_the_generated_one_time_token").await else { return };
        let dir = std::env::temp_dir().join(format!("shadoucmdb-setup-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("setup-token");
        let auth = AuthState::new(
            AuthConfig { setup_token: None, setup_token_file: Some(file.clone()), ..auth_state().config },
            crate::secrets::Keyring::for_tests(),
        );
        let (pool, headers) = (&db.pool, HeaderMap::new());
        let with = |token: &str| SetupBody { setup_token: token.into(), ..body("owner") };

        // Nothing is armed until the server has seen the empty database; the first attempt arms it and fails.
        for guess in [
            "",
            crate::auth::setup_token::TEST_TOKEN,
            "0000000000000000000000000000000000000000000000000000000000000000",
        ] {
            let err = setup(pool, &auth, &headers, &anon(), with(guess)).await.err().expect("refused");
            assert_eq!(err.code, ErrorCode::Forbidden, "{guess:?}");
            assert_eq!(err.details.unwrap()[0].field, "setupToken");
        }
        assert!(setup_required(pool).await.unwrap(), "a refused setup creates no user");

        let token = std::fs::read_to_string(&file).unwrap().trim().to_owned();
        setup(pool, &auth, &headers, &anon(), with(&token)).await.expect("setup with the token");
        assert!(!file.exists(), "the token file is deleted once used");
        assert!(!auth.setup.matches(&token), "the token works once");

        // Installed: 409 whatever the token, and never 403 (the token is not the gate any more).
        let late =
            setup(pool, &auth, &headers, &anon(), SetupBody { setup_token: token.as_str().into(), ..body("late") })
                .await;
        assert_eq!(late.err().map(|e| e.code), Some(ErrorCode::Conflict));
        std::fs::remove_dir_all(&dir).unwrap();
        db.drop().await;
    }

    /// GH#230: wrong setup tokens lock setup for the client's network (429,
    /// token not checked, nothing logged); other networks can still set up,
    /// and the 409 of an installed system is never throttled.
    #[tokio::test]
    async fn wrong_setup_tokens_are_throttled_per_network() {
        let Some(db) = scratch::database("wrong_setup_tokens_are_throttled_per_network").await else { return };
        let (pool, auth, headers) = (&db.pool, auth_state(), HeaderMap::new());
        let from = |ip: [u8; 4]| {
            let ip = std::net::IpAddr::from(ip);
            anon().with_client(crate::api::context::ClientInfo {
                ip: Some(ip),
                net: Net::of(Some(ip)),
                ..Default::default()
            })
        };
        let (guesser, owner) = (from([198, 51, 100, 7]), from([203, 0, 113, 9]));
        let wrong = || SetupBody { setup_token: "0".repeat(64).into(), ..body("owner") };
        for i in 0..crate::auth::throttle::FREE_FAILURES {
            let err = setup(pool, &auth, &headers, &guesser, wrong()).await.err().expect("refused");
            assert_eq!(err.code, ErrorCode::Forbidden, "attempt {i}");
        }
        assert!(matches!(auth.setup_throttle.check(SETUP_THROTTLE_KEY, guesser.client.net), Gate::Locked(_)));
        // Lengthen the lock so what follows does not depend on how fast the database answers.
        for _ in 0..10 {
            auth.setup_throttle.failure(SETUP_THROTTLE_KEY, guesser.client.net);
        }
        assert_eq!(auth.setup.refusals(), u64::from(crate::auth::throttle::FREE_FAILURES), "each one reported");

        let locked = setup(pool, &auth, &headers, &guesser, wrong()).await.err().expect("locked");
        assert_eq!(locked.code, ErrorCode::RateLimited);
        assert!(locked.retry_after.is_some_and(|s| s > 1));
        let right = setup(pool, &auth, &headers, &guesser, body("owner")).await.err().expect("locked");
        assert_eq!(right.code, ErrorCode::RateLimited, "the token is not checked while locked");
        assert_eq!(auth.setup.refusals(), u64::from(crate::auth::throttle::FREE_FAILURES), "a 429 is not reported");
        assert!(setup_required(pool).await.unwrap());

        // Another network is unaffected: its wrong token is checked, its right one sets up.
        let other = setup(pool, &auth, &headers, &owner, wrong()).await.err().expect("refused");
        assert_eq!(other.code, ErrorCode::Forbidden);
        setup(pool, &auth, &headers, &owner, body("owner")).await.expect("setup from another network");

        // Installed: 409 for everyone, the locked network included, however often.
        for _ in 0..crate::auth::throttle::FREE_FAILURES * 2 {
            let late = setup(pool, &auth, &headers, &guesser, wrong()).await;
            assert_eq!(late.err().map(|e| e.code), Some(ErrorCode::Conflict));
        }
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
            auth.throttle.failure(&format!("junk-{i}"), Net::default());
        }
        assert_eq!(auth.throttle.check("admin", Net::default()), Gate::Slow);
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
            let b = LoginBody { username: "admin".into(), password: wrong.clone().into() };
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

    /// Rotating keeps the user, expiry and second-factor state under a new
    /// token and ends the old row; a session that ended meanwhile (signed out,
    /// or rotated already) is not resurrected (GH#510).
    #[tokio::test]
    async fn rotating_a_session_ends_it_and_never_revives_an_ended_one() {
        let Some(db) = scratch::database("rotating_a_session_ends_it_and_never_revives_an_ended_one").await else {
            return;
        };
        let (pool, auth) = (&db.pool, auth_state());
        setup(pool, &auth, &HeaderMap::new(), &anon(), body("owner")).await.expect("setup");
        let mut held = pool.acquire().await.unwrap();
        let conn: &mut PgConnection = &mut held;
        let user_id: Uuid = sqlx::query_scalar("SELECT id FROM users").fetch_one(&mut *conn).await.unwrap();
        let old =
            data::create_session(conn, user_id, &[1; 32], "csrf-old", Duration::from_secs(3600), None, None, true)
                .await
                .unwrap();
        async fn rotate(conn: &mut PgConnection, id: Uuid, token: u8) -> Option<(Uuid, data::EndedSession)> {
            data::rotate_session(conn, id, &[token; 32], "csrf-new", Some("ua"), None, false).await.unwrap()
        }

        let (new, ended) = rotate(conn, old, 2).await.expect("a live session rotates");
        assert_eq!((ended.id, ended.user_id), (old, user_id));
        let rows: Vec<(Uuid, Uuid, bool, String)> =
            sqlx::query_as("SELECT id, user_id, mfa_verified, csrf_token FROM sessions WHERE id IN ($1, $2)")
                .bind(old)
                .bind(new)
                .fetch_all(&mut *conn)
                .await
                .unwrap();
        assert_eq!(rows, vec![(new, user_id, true, "csrf-new".to_owned())], "only the new row, second factor kept");
        let same_expiry: bool = sqlx::query_scalar(
            "SELECT expires_at <= now() + interval '3600 seconds' AND expires_at > now() + interval '3500 seconds' FROM sessions WHERE id = $1",
        )
        .bind(new)
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert!(same_expiry, "the renewed session keeps the old expiry");

        assert!(rotate(conn, old, 3).await.is_none(), "an already rotated session does not rotate again");
        data::delete_session(conn, new).await.unwrap().expect("signed out");
        assert!(rotate(conn, new, 4).await.is_none(), "a signed-out session does not come back");
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions WHERE id = $1 OR user_agent = 'ua'")
            .bind(old)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(left, 0);
        drop(held);
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
                email_required: false,
                recently_confirmed: true,
            },
            permissions,
        };
        let ctx = RequestContext::user(std::sync::Arc::new(principal), String::new());
        let change = |current: &str| PasswordChange {
            current_password: current.into(),
            new_password: "a brand new passphrase".into(),
        };
        auth.password_throttle.freeze();
        for _ in 0..crate::auth::throttle::FREE_FAILURES {
            let e = change_password(pool, &auth, &ctx, &HeaderMap::new(), change(&wrong)).await.unwrap_err();
            assert_eq!(e.code, ErrorCode::ValidationError);
        }
        let e = change_password(pool, &auth, &ctx, &HeaderMap::new(), change(right)).await.unwrap_err();
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

    /// A request from client `ip` through trusted proxy `peer`, which forwarded it as sent.
    fn via(ip: &str, peer: &str) -> RequestContext {
        let client = crate::api::context::ClientInfo {
            ip: Some(ip.parse().unwrap()),
            claimed_ip: Some(ip.parse().unwrap()),
            peer_ip: Some(peer.parse().unwrap()),
            user_agent: Some("audit-test".into()),
            net: Net::of(Some(ip.parse().unwrap())),
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
            email: "alice@example.test".into(),
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
                email_required: false,
                recently_confirmed: true,
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
            email: "gone@example.test".into(),
            password: "gone correct horse".into(),
            is_active: Some(false),
            profile_ids: vec![],
        };
        users::create(pool, &RequestContext::system("test", "test"), &input).await.unwrap();
        auth.throttle.freeze();
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
            serde_json::json!({ "attemptedUsername": "gone", "ipAddress": "198.51.100.8", "userAgent": "audit-test", "reason": "account_disabled" })
        );
        let e = login(pool, &auth, &headers, &from("198.51.100.8"), login_body("gone", "gone correct horse"))
            .await
            .err()
            .expect("locked");
        assert_eq!(e.code, ErrorCode::RateLimited);
        assert_eq!(auth_rows(pool, "login.failure").await.len(), failures.len(), "a 429 writes no row");
        db.drop().await;
    }

    /// The response body a refusal is sent as.
    async fn answer_bytes(e: AppError) -> (axum::http::StatusCode, axum::body::Bytes) {
        let res = axum::response::IntoResponse::into_response(e);
        (res.status(), axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
    }

    /// GH#437: a right password that is refused all the same (a disabled
    /// account, a directory's refusal) gets the body a wrong password gets,
    /// byte for byte; only the audit row tells why.
    #[tokio::test]
    async fn a_refused_right_password_answers_like_a_wrong_one() {
        let Some(db) = scratch::database("a_refused_right_password_answers_like_a_wrong_one").await else { return };
        let (pool, auth, headers) = (&db.pool, auth_state(), HeaderMap::new());
        setup(pool, &auth, &headers, &from("192.0.2.1"), body("owner")).await.unwrap();
        let (password, guess) = (Uuid::new_v4().to_string(), Uuid::new_v4().to_string());
        let input = UserCreate {
            username: "gone".into(),
            display_name: "Gone".into(),
            email: "gone@example.test".into(),
            password: password.as_str().into(),
            is_active: Some(false),
            profile_ids: vec![],
        };
        users::create(pool, &RequestContext::system("test", "test"), &input).await.unwrap();
        let ctx = from("198.51.100.8");
        let wrong = login(pool, &auth, &headers, &ctx, login_body("gone", &guess)).await.err().expect("wrong");
        let wrong = answer_bytes(wrong).await;
        assert_eq!(wrong.0, axum::http::StatusCode::UNAUTHORIZED);
        let right = login(pool, &auth, &headers, &ctx, login_body("gone", &password)).await.err().expect("disabled");
        assert_eq!(answer_bytes(right).await, wrong, "a disabled account's right password");
        let failures = auth_rows(pool, "login.failure").await;
        assert_eq!(failures[0].3.get("reason"), None, "the wrong password");
        assert_eq!(failures[1].3["reason"], "account_disabled", "audited with the reason");

        use sso::Refusal::*;
        let refusals = [
            InvalidUsername,
            AccountConflict,
            AccountDisabled,
            NotAuthorised,
            LastAdministrator,
            MfaNotEnforced,
            ProviderDisabled,
        ];
        for refusal in refusals {
            // A name each, so the throttle's lock does not answer instead.
            let name = format!("dave-{}", refusal.code());
            let attempt = throttle_gate(&auth.throttle, &name, ctx.client.net, "sign-ins").await.unwrap();
            let attempt = Reservation { name: attempt, entry: None };
            let answer = sso::DirectoryAnswer::Refused(refusal);
            let e = directory_answer(pool, &auth, &headers, &ctx, attempt, &login_body(&name, &guess), None, answer)
                .await
                .err()
                .expect("refused");
            assert_eq!(answer_bytes(e).await, wrong, "{refusal:?}");
            let last = auth_rows(pool, "login.failure").await.pop().unwrap();
            assert_eq!(last.3["reason"], refusal.code(), "{refusal:?} audited with the reason");
        }
        db.drop().await;
    }

    /// GH#415: a failed sign-in logs the account's name when the username
    /// matches one, and never a name that matches none (often a password).
    #[tokio::test]
    async fn a_failed_sign_in_logs_only_existing_account_names() {
        let Some(db) = scratch::database("a_failed_sign_in_logs_only_existing_account_names").await else { return };
        let (pool, auth, headers) = (&db.pool, auth_state(), HeaderMap::new());
        setup(pool, &auth, &headers, &from("192.0.2.1"), body("owner")).await.unwrap();
        let (log, _guard) = crate::auth::setup_token::capture::warnings();

        let typed = "Tr0ub4dor&3-not-a-name";
        // Generated, so no fixed value reaches the password check.
        let wrong = Uuid::new_v4().to_string();
        login(pool, &auth, &headers, &from("198.51.100.9"), login_body(typed, &wrong)).await.err().expect("refused");
        login(pool, &auth, &headers, &from("198.51.100.9"), login_body("OWNER", &wrong)).await.err().expect("refused");

        let failed: Vec<String> = log.lines().into_iter().filter(|l| l.contains("sign-in failed")).collect();
        assert_eq!(failed.len(), 2, "{failed:?}");
        assert!(log.lines().iter().all(|l| !l.contains(typed)), "{:?}", log.lines());
        assert!(failed[0].contains("unknown_user=true") && !failed[0].contains("username="), "{}", failed[0]);
        assert!(failed[1].contains(r#"username="owner""#) && !failed[1].contains("unknown_user"), "{}", failed[1]);
        // The audit row keeps the name as typed, so the two can be correlated.
        let failures = auth_rows(pool, "login.failure").await;
        assert_eq!(failures[0].3["attemptedUsername"], typed);
        db.drop().await;
    }

    /// GH#187: wrong passwords from one network lock the name for that
    /// network only; the account holder signs in from another.
    #[tokio::test]
    async fn guessing_from_one_network_does_not_lock_the_owner_out_elsewhere() {
        let Some(db) = scratch::database("guessing_from_one_network_does_not_lock_the_owner_out_elsewhere").await
        else {
            return;
        };
        let (pool, auth, headers) = (&db.pool, auth_state(), HeaderMap::new());
        setup(pool, &auth, &headers, &from("192.0.2.1"), body("admin")).await.unwrap();
        let (right, wrong) = (OWNER_PASSWORD.as_str(), OWNER_PASSWORD.to_uppercase());
        auth.throttle.freeze();
        for _ in 0..crate::auth::throttle::FREE_FAILURES {
            let e = login(pool, &auth, &headers, &from("198.51.100.7"), login_body("admin", &wrong)).await.err();
            assert_eq!(e.map(|e| e.code), Some(ErrorCode::Unauthenticated));
        }
        let e = login(pool, &auth, &headers, &from("198.51.100.99"), login_body("admin", right)).await.err();
        assert_eq!(e.map(|e| e.code), Some(ErrorCode::RateLimited), "locked for the guessing /24");
        let owner = login(pool, &auth, &headers, &from("203.0.113.5"), login_body("admin", right)).await;
        assert!(owner.is_ok(), "refused from another network: {:?}", owner.err().map(|e| e.code));
        db.drop().await;
    }

    /// GH#438: a spelling PostgreSQL's `lower()` folds onto a locked account
    /// (`admİn` for `admin` under glibc collations) does not get a fresh
    /// throttle budget for it: it is refused like an unknown name, and the
    /// account's password is not checked, even when right.
    #[tokio::test]
    async fn a_non_ascii_spelling_does_not_reach_a_locked_account() {
        let Some(db) = scratch::database("a_non_ascii_spelling_does_not_reach_a_locked_account").await else {
            return;
        };
        let (pool, auth, headers) = (&db.pool, auth_state(), HeaderMap::new());
        setup(pool, &auth, &headers, &from("192.0.2.1"), body("admin")).await.unwrap();
        let (right, wrong) = (OWNER_PASSWORD.as_str(), OWNER_PASSWORD.to_uppercase());
        auth.throttle.freeze();
        let guesser = from("198.51.100.7");
        for _ in 0..crate::auth::throttle::FREE_FAILURES {
            let e = login(pool, &auth, &headers, &guesser, login_body("admin", &wrong)).await.err();
            assert_eq!(e.map(|e| e.code), Some(ErrorCode::Unauthenticated));
        }
        let e = login(pool, &auth, &headers, &guesser, login_body("admin", right)).await.err();
        assert_eq!(e.map(|e| e.code), Some(ErrorCode::RateLimited), "admin is locked for the guessing /24");
        for spelling in ["adm\u{130}n", "adm\u{131}n", "\u{212a}admin", "ADM\u{130}N"] {
            let before = password::DUMMY_VERIFIES.with(|n| n.get());
            let e = login(pool, &auth, &headers, &guesser, login_body(spelling, right)).await.err();
            let e = e.unwrap_or_else(|| panic!("{spelling:?} signed in to admin"));
            assert_eq!(e.code, ErrorCode::Unauthenticated, "{spelling:?}");
            assert_eq!(e.message, invalid_credentials().message, "{spelling:?}: the unknown-name answer");
            assert_eq!(password::DUMMY_VERIFIES.with(|n| n.get()), before + 1, "{spelling:?}: only the dummy hash");
        }
        db.drop().await;
    }

    /// GH#190: a name the directory does not match costs an argon2 verify,
    /// like a local account's wrong password, so timing does not tell them apart.
    #[tokio::test]
    async fn a_directory_no_match_costs_a_password_verify() {
        let Some(db) = scratch::database("a_directory_no_match_costs_a_password_verify").await else { return };
        let (pool, auth, headers) = (&db.pool, auth_state(), HeaderMap::new());
        // A disabled directory answers NoMatch without being contacted: the
        // same answer as an enabled one that finds no entry for the name.
        let ldap = crate::modules::mfa::tests::provider(pool, "ldap", false, "ldaps://dc.example.test").await;
        let mut tx = pool.begin().await.unwrap();
        let linked = crate::data::identity_providers::NewLinkedUser {
            provider_id: ldap,
            external_id: "erin",
            username: "erin",
            display_name: "erin",
            email: None,
        };
        crate::data::identity_providers::insert_linked(&mut tx, &linked).await.unwrap();
        tx.commit().await.unwrap();
        let before = password::DUMMY_VERIFIES.with(|n| n.get());
        let e = login(pool, &auth, &headers, &from("198.51.100.7"), login_body("erin", OWNER_PASSWORD.as_str()))
            .await
            .err();
        assert_eq!(e.map(|e| e.code), Some(ErrorCode::Unauthenticated));
        assert_eq!(password::DUMMY_VERIFIES.with(|n| n.get()), before + 1, "the dummy hash was verified");
        db.drop().await;
    }

    /// GH#406: only names an account could have are looked up in a directory;
    /// spellings a normalising directory would match to the same entry
    /// (full-width, zero-width, soft hyphen, control characters) are not.
    #[test]
    fn only_account_names_reach_the_directory() {
        assert_eq!(account_name(" Bob\t"), Some("Bob"));
        assert_eq!(account_name("bob@corp.example"), Some("bob@corp.example"));
        for name in ["ｂｏｂ", "bob\u{200b}", "bo\u{ad}b", "b\u{0}ob", "bob\nbob", "bob smith", "", "j\u{fc}rgen"] {
            assert_eq!(account_name(name), None, "{name:?}");
        }
    }

    /// The entry a stub directory finds for both `bob` and `bob@corp.example`
    /// (a filter that also matches `mail`), as `directory_login` handles it.
    async fn stub_directory_login(
        pool: &PgPool,
        auth: &AuthState,
        ctx: &RequestContext,
        bob: (Uuid, Uuid),
        name: &str,
        password: &str,
    ) -> Result<LoginAnswer, AppError> {
        const RIGHT: &str = "bobs-directory-password";
        let (provider, user_id) = bob;
        let attempt = throttle_gate(&auth.throttle, name, ctx.client.net, "sign-ins for this username").await?;
        let mut entry = None;
        let answer = match account_name(name).map(str::to_lowercase).as_deref() {
            Some("bob" | "bob@corp.example") => {
                entry = admit_entry(&auth.directory_throttle, ctx.client.net, provider, "entryUUID:b0b");
                match entry {
                    None => sso::DirectoryAnswer::NotAdmitted,
                    Some(_) if password == RIGHT => sso::DirectoryAnswer::SignedIn { user_id, username: "bob".into() },
                    Some(_) => sso::DirectoryAnswer::NoMatch,
                }
            }
            _ => sso::DirectoryAnswer::NoMatch,
        };
        let attempt = Reservation { name: attempt, entry };
        directory_answer(pool, auth, &HeaderMap::new(), ctx, attempt, &login_body(name, password), None, answer).await
    }

    /// GH#406: failures split over two names the directory resolves to one
    /// entry lock it after the same total as one name, and while it is locked
    /// its password is not checked: the right one gets the wrong one's 401.
    #[tokio::test]
    async fn names_for_one_directory_entry_share_its_lock() {
        let Some(db) = scratch::database("names_for_one_directory_entry_share_its_lock").await else { return };
        let (pool, auth) = (&db.pool, auth_state());
        let ldap = crate::modules::mfa::tests::provider(pool, "ldap", true, "ldaps://dc.example.test").await;
        let mut tx = pool.begin().await.unwrap();
        let linked = crate::data::identity_providers::NewLinkedUser {
            provider_id: ldap,
            external_id: "entryUUID:b0b",
            username: "bob",
            display_name: "bob",
            email: None,
        };
        let bob = (ldap, crate::data::identity_providers::insert_linked(&mut tx, &linked).await.unwrap());
        tx.commit().await.unwrap();
        auth.throttle.freeze();
        auth.directory_throttle.freeze();
        let (guesser, owner) = (from("198.51.100.7"), from("203.0.113.5"));
        let spellings = ["bob", "bob@corp.example"];
        for i in 0..crate::auth::throttle::FREE_FAILURES as usize {
            let e = stub_directory_login(pool, &auth, &guesser, bob, spellings[i % 2], "guess").await.err();
            assert_eq!(e.map(|e| e.code), Some(ErrorCode::Unauthenticated), "guess {i}");
        }
        // Neither name has used up its own free failures; the entry has.
        let e = stub_directory_login(pool, &auth, &guesser, bob, "BOB", "bobs-directory-password").await.err();
        assert_eq!(e.as_ref().map(|e| e.code), Some(ErrorCode::Unauthenticated), "the entry is locked");
        assert_eq!(e.unwrap().message, invalid_credentials().message, "the answer a wrong password gets");
        let failures = auth_rows(pool, "login.failure").await;
        assert_eq!(failures.len(), crate::auth::throttle::FREE_FAILURES as usize + 1);
        assert_eq!(failures.last().unwrap().3["reason"], DIRECTORY_ENTRY_LOCKED, "audited as not checked");
        assert_eq!(failures[0].3.get("reason"), None);
        // Per network, as the name's lock is (GH#187): the owner still signs in.
        let answer =
            stub_directory_login(pool, &auth, &owner, bob, "bob@corp.example", "bobs-directory-password").await;
        assert!(matches!(answer, Ok(Either::Left(_))), "the owner's network is not locked");
        db.drop().await;
    }

    /// GH#120: after the directory accepted the password, a user with an
    /// authenticator gets the second-factor challenge, not a session, and the
    /// name's failure count is kept until the code is right. Without an
    /// authenticator the same step opens the session.
    #[tokio::test]
    async fn a_directory_sign_in_asks_for_the_second_factor() {
        let Some(db) = scratch::database("a_directory_sign_in_asks_for_the_second_factor").await else { return };
        let (pool, auth, headers) = (&db.pool, auth_state(), HeaderMap::new());
        let ldap = crate::modules::mfa::tests::provider(pool, "ldap", true, "ldaps://dc.example.test").await;
        let mut tx = pool.begin().await.unwrap();
        let linked = |username: &'static str| crate::data::identity_providers::NewLinkedUser {
            provider_id: ldap,
            external_id: username,
            username,
            display_name: username,
            email: None,
        };
        let dirk = crate::data::identity_providers::insert_linked(&mut tx, &linked("dirk")).await.unwrap();
        let dora = crate::data::identity_providers::insert_linked(&mut tx, &linked("dora")).await.unwrap();
        let sealed = crate::secrets::sealed::seal_totp_secret(
            &crate::secrets::Keyring::for_tests(),
            dirk,
            &crate::auth::totp::new_secret(),
        );
        mfa_data::put_pending_totp(&mut tx, dirk, &sealed).await.unwrap();
        mfa_data::confirm_totp(&mut tx, dirk, 1).await.unwrap();
        tx.commit().await.unwrap();
        let sessions = |id: Uuid| {
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM sessions WHERE user_id = $1").bind(id).fetch_one(pool)
        };

        let attempt = |name: &str| auth.throttle.begin(name, Net::default(), false).expect("the gate lets it through");
        for _ in 1..crate::auth::throttle::FREE_FAILURES {
            auth.throttle.failure("Dirk", Net::default());
        }
        let answer = password_accepted(
            pool,
            &auth,
            &headers,
            &anon(),
            attempt("Dirk").into(),
            dirk,
            "dirk",
            LoginMethod::Ldap,
            None,
        )
        .await
        .unwrap();
        let Either::Right(ErrorWithCookies(err, cookies)) = answer else { panic!("a session was opened") };
        assert_eq!(err.code, ErrorCode::MfaRequired);
        assert!(session::cookie_value(&cookies[0], session::MFA_COOKIE).is_some_and(|t| !t.is_empty()));
        assert_eq!(sessions(dirk).await.unwrap(), 0, "no session before the second factor");
        let challenges: i64 = sqlx::query_scalar("SELECT count(*) FROM mfa_challenges WHERE user_id = $1")
            .bind(dirk)
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(challenges, 1);
        let last_login: Option<chrono::DateTime<chrono::Utc>> =
            sqlx::query_scalar("SELECT last_login_at FROM users WHERE id = $1")
                .bind(dirk)
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(last_login, None, "not recorded as a sign-in yet");
        assert!(auth.throttle.failure("Dirk", Net::default()).is_some(), "the failure count was not cleared");

        let answer = password_accepted(
            pool,
            &auth,
            &headers,
            &anon(),
            attempt("dora").into(),
            dora,
            "dora",
            LoginMethod::Ldap,
            None,
        )
        .await
        .unwrap();
        assert!(matches!(answer, Either::Left(_)), "no authenticator: signed in");
        assert_eq!(sessions(dora).await.unwrap(), 1);
        db.drop().await;
    }

    /// A local account `name` and its password, generated per run as
    /// `OWNER_PASSWORD` is, with an authenticator when `secret` is given.
    async fn account(pool: &PgPool, name: &str, secret: Option<&[u8]>) -> (Uuid, String) {
        let password = format!("passphrase {}", Uuid::new_v4());
        let input = UserCreate {
            username: name.into(),
            display_name: name.into(),
            email: format!("{name}@example.test"),
            password: password.clone().into(),
            is_active: Some(true),
            profile_ids: vec![],
        };
        let id = users::create(pool, &RequestContext::system("test", "test"), &input).await.unwrap().id;
        if let Some(secret) = secret {
            let sealed = crate::secrets::sealed::seal_totp_secret(&crate::secrets::Keyring::for_tests(), id, secret);
            let mut conn = pool.acquire().await.unwrap();
            mfa_data::put_pending_totp(&mut conn, id, &sealed).await.unwrap();
            mfa_data::confirm_totp(&mut conn, id, 1).await.unwrap();
        }
        (id, password)
    }

    /// An administrator's password reset (`disable`: disabling) of `user`,
    /// with the row already locked as `users::set_password` and
    /// `users::update` lock it. Its changes are made and committed once a
    /// sign-in waits for that lock, so the sign-in had checked the password
    /// before the reset committed.
    async fn change_account_under(pool: &PgPool, mut tx: sqlx::PgTransaction<'_>, user: Uuid, disable: bool) {
        assert!(a_lock_is_awaited(pool).await, "the sign-in waits for the lock on the user's row");
        if disable {
            sqlx::query("UPDATE users SET is_active = false WHERE id = $1").bind(user).execute(&mut *tx).await.unwrap();
        } else {
            let hash = password::hash(&format!("reset {}", Uuid::new_v4())).await.unwrap();
            data::set_password(&mut tx, user, &hash).await.unwrap();
        }
        mfa_data::delete_challenges_of_user(&mut tx, user).await.unwrap();
        data::delete_user_sessions(&mut tx, user, None).await.unwrap();
        tx.commit().await.unwrap();
    }

    /// Whether a session of the test database waits for a lock, within 10 seconds.
    pub(crate) async fn a_lock_is_awaited(pool: &PgPool) -> bool {
        for _ in 0..400 {
            let n: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock'",
            )
            .fetch_one(pool)
            .await
            .unwrap();
            if n > 0 {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        false
    }

    /// An administrator disabling `provider`, with its row already locked as
    /// `identity_providers::update` locks it: once a sign-in waits for that
    /// lock, the provider is disabled, its accounts' sessions end, and it commits.
    pub(crate) async fn locked_provider(pool: &PgPool, provider: Uuid) -> sqlx::PgTransaction<'_> {
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("SELECT id FROM identity_providers WHERE id = $1 FOR UPDATE")
            .bind(provider)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        tx
    }

    pub(crate) async fn disable_provider_under(pool: &PgPool, mut tx: sqlx::PgTransaction<'_>, provider: Uuid) {
        assert!(a_lock_is_awaited(pool).await, "the sign-in waits for the lock on the provider's row");
        sqlx::query("UPDATE identity_providers SET is_enabled = false WHERE id = $1")
            .bind(provider)
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("DELETE FROM sessions WHERE user_id IN (SELECT id FROM users WHERE identity_provider_id = $1)")
            .bind(provider)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    async fn locked_user(pool: &PgPool, user: Uuid) -> sqlx::PgTransaction<'_> {
        let mut tx = pool.begin().await.unwrap();
        data::get_user(&mut tx, user, true).await.unwrap().expect("the user");
        tx
    }

    async fn rows_of(pool: &PgPool, table: &str, user: Uuid) -> i64 {
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table} WHERE user_id = $1")))
            .bind(user)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// A right password checked just before a reset or disable commits gets
    /// neither a session nor, with MFA set up, a second-factor step (GH#209).
    #[tokio::test]
    async fn a_password_checked_before_a_reset_or_disable_gets_no_session_or_challenge() {
        let Some(db) =
            scratch::database("a_password_checked_before_a_reset_or_disable_gets_no_session_or_challenge").await
        else {
            return;
        };
        let (pool, auth, headers) = (&db.pool, auth_state(), HeaderMap::new());
        setup(pool, &auth, &headers, &anon(), body("owner")).await.unwrap();
        let secret = crate::auth::totp::new_secret();
        for (name, mfa, disable) in
            [("reset", false, false), ("disabled", false, true), ("resetmfa", true, false), ("disabledmfa", true, true)]
        {
            let (user, password) = account(pool, name, mfa.then_some(secret.as_slice())).await;
            let tx = locked_user(pool, user).await;
            let ctx = anon();
            let (answer, ()) = tokio::join!(
                login(pool, &auth, &headers, &ctx, login_body(name, &password)),
                change_account_under(pool, tx, user, disable)
            );
            let e = answer.err().unwrap_or_else(|| panic!("{name}: signed in"));
            assert_eq!(e.code, ErrorCode::Unauthenticated, "{name}: {}", e.message);
            assert_eq!(rows_of(pool, "sessions", user).await, 0, "{name}: no session");
            assert_eq!(rows_of(pool, "mfa_challenges", user).await, 0, "{name}: no second-factor step");
            let last_login: Option<chrono::DateTime<chrono::Utc>> =
                sqlx::query_scalar("SELECT last_login_at FROM users WHERE id = $1")
                    .bind(user)
                    .fetch_one(pool)
                    .await
                    .unwrap();
            assert_eq!(last_login, None, "{name}: not recorded as a sign-in");
        }
        let refused = auth_rows(pool, "login.failure").await;
        assert_eq!(refused.len(), 4);
        assert!(refused.iter().all(|r| r.3["reason"] == "account_changed"), "{refused:?}");
        db.drop().await;
    }

    /// The second factor entered while a reset or disable is under way gets no
    /// session, nor does a session opened for a password changed since it was
    /// checked (GH#209).
    #[tokio::test]
    async fn a_second_factor_during_a_reset_or_disable_gets_no_session() {
        let Some(db) = scratch::database("a_second_factor_during_a_reset_or_disable_gets_no_session").await else {
            return;
        };
        let (pool, auth) = (&db.pool, auth_state());
        setup(pool, &auth, &HeaderMap::new(), &anon(), body("owner")).await.unwrap();
        let secret = crate::auth::totp::new_secret();
        for (name, disable) in [("reset", false), ("disabled", true)] {
            let (user, password) = account(pool, name, Some(&secret)).await;
            let answer = login(pool, &auth, &HeaderMap::new(), &anon(), login_body(name, &password)).await.unwrap();
            let Either::Right(ErrorWithCookies(_, cookies)) = answer else { panic!("{name}: no second factor asked") };
            let token = session::cookie_value(&cookies[0], session::MFA_COOKIE).unwrap();
            let mut headers = HeaderMap::new();
            headers.insert(axum::http::header::COOKIE, format!("{}={token}", session::MFA_COOKIE).parse().unwrap());

            let tx = locked_user(pool, user).await;
            let code = crate::auth::totp::code_at(&secret, crate::auth::totp::current_step());
            let ctx = anon();
            let (answer, ()) = tokio::join!(
                login_mfa(pool, &auth, &headers, &ctx, MfaLoginBody { code }),
                change_account_under(pool, tx, user, disable)
            );
            let e = answer.err().unwrap_or_else(|| panic!("{name}: signed in"));
            assert_eq!(e.code, ErrorCode::Unauthenticated, "{name}: {}", e.message);
            assert_eq!(rows_of(pool, "sessions", user).await, 0, "{name}: no session");
            assert_eq!(rows_of(pool, "mfa_challenges", user).await, 0, "{name}: no second-factor step");
        }

        // The window after the second factor's transaction: the stamp it
        // was checked with no longer matches the row.
        let (user, _) = account(pool, "late", None).await;
        let mut conn = pool.acquire().await.unwrap();
        let checked = data::get_user(&mut conn, user, false).await.unwrap().unwrap();
        let hash = password::hash(&format!("reset {}", Uuid::new_v4())).await.unwrap();
        data::set_password(&mut conn, user, &hash).await.unwrap();
        drop(conn);
        let method = LoginMethod::Totp;
        let verified = Some(checked.password_changed_at);
        let e = open_session(pool, &auth, &HeaderMap::new(), &anon(), user, "late", method, verified)
            .await
            .expect_err("a session for the old password");
        assert_eq!(e.code, ErrorCode::Unauthenticated);
        assert_eq!(rows_of(pool, "sessions", user).await, 0);
        db.drop().await;
    }

    /// An administrator's reset or the user's own disable of the authenticator,
    /// as `mfa::reset` and `mfa::disable` make it (the authenticator locked
    /// first), not committed yet.
    async fn removing_mfa(pool: &PgPool, user: Uuid) -> sqlx::PgTransaction<'_> {
        let mut tx = pool.begin().await.unwrap();
        mfa_data::get_totp(&mut tx, user, true).await.unwrap().expect("an authenticator");
        tx
    }

    async fn remove_mfa_under(mut tx: sqlx::PgTransaction<'_>, user: Uuid) {
        assert!(mfa_data::delete_mfa(&mut tx, user).await.unwrap(), "a confirmed authenticator");
        data::delete_user_sessions(&mut tx, user, None).await.unwrap();
        tx.commit().await.unwrap();
    }

    /// A code checked just before the authenticator is reset or turned off
    /// gets no session that outlives it, whether the reset commits between
    /// the code's check and the session's start or while the session is
    /// being opened (GH#341).
    #[tokio::test]
    async fn a_second_factor_checked_before_a_reset_or_disable_gets_no_session() {
        let Some(db) = scratch::database("a_second_factor_checked_before_a_reset_or_disable_gets_no_session").await
        else {
            return;
        };
        let (pool, auth, headers) = (&db.pool, auth_state(), HeaderMap::new());
        setup(pool, &auth, &headers, &anon(), body("owner")).await.unwrap();
        let secret = crate::auth::totp::new_secret();
        for (name, method) in [("totp", LoginMethod::Totp), ("recovery", LoginMethod::RecoveryCode)] {
            let (user, _) = account(pool, name, Some(&secret)).await;
            let checked = data::get_user(&mut pool.acquire().await.unwrap(), user, false).await.unwrap().unwrap();
            let verified = Some(checked.password_changed_at);

            // Committed after the code's transaction, before the session's.
            remove_mfa_under(removing_mfa(pool, user).await, user).await;
            let e = open_session(pool, &auth, &headers, &anon(), user, name, method, verified)
                .await
                .expect_err("a session for a removed authenticator");
            assert_eq!(e.code, ErrorCode::Unauthenticated, "{name}: {}", e.message);
            assert_eq!(rows_of(pool, "sessions", user).await, 0, "{name}: no session");

            // Committed while the session is being opened: the sign-in waits
            // for the authenticator's lock and then finds it gone.
            let late = format!("{name}-late");
            let (user, _) = account(pool, &late, Some(&secret)).await;
            let checked = data::get_user(&mut pool.acquire().await.unwrap(), user, false).await.unwrap().unwrap();
            let verified = Some(checked.password_changed_at);
            let tx = removing_mfa(pool, user).await;
            let ctx = anon();
            let removed = async {
                assert!(a_lock_is_awaited(pool).await, "{name}: the sign-in waits for the authenticator's lock");
                remove_mfa_under(tx, user).await;
            };
            let (answer, ()) =
                tokio::join!(open_session(pool, &auth, &headers, &ctx, user, &late, method, verified), removed);
            let e = answer.err().unwrap_or_else(|| panic!("{name}: signed in"));
            assert_eq!(e.code, ErrorCode::Unauthenticated, "{name}: {}", e.message);
            assert_eq!(rows_of(pool, "sessions", user).await, 0, "{name}: no session");
        }
        let refused = auth_rows(pool, "login.failure").await;
        assert_eq!(refused.len(), 4);
        assert!(refused.iter().all(|r| r.3["reason"] == "mfa_removed"), "{refused:?}");
        db.drop().await;
    }

    /// A directory password checked just before the directory is disabled
    /// gets neither a session nor a second-factor step (GH#250).
    #[tokio::test]
    async fn a_directory_sign_in_during_a_provider_disable_gets_no_session_or_challenge() {
        let Some(db) =
            scratch::database("a_directory_sign_in_during_a_provider_disable_gets_no_session_or_challenge").await
        else {
            return;
        };
        let (pool, auth, headers) = (&db.pool, auth_state(), HeaderMap::new());
        let ldap = crate::modules::mfa::tests::provider(pool, "ldap", true, "ldaps://dc.example.test").await;
        let mut tx = pool.begin().await.unwrap();
        let linked = |username: &'static str| crate::data::identity_providers::NewLinkedUser {
            provider_id: ldap,
            external_id: username,
            username,
            display_name: username,
            email: None,
        };
        let dana = crate::data::identity_providers::insert_linked(&mut tx, &linked("dana")).await.unwrap();
        let dirk = crate::data::identity_providers::insert_linked(&mut tx, &linked("dirk")).await.unwrap();
        let sealed = crate::secrets::sealed::seal_totp_secret(
            &crate::secrets::Keyring::for_tests(),
            dirk,
            &crate::auth::totp::new_secret(),
        );
        mfa_data::put_pending_totp(&mut tx, dirk, &sealed).await.unwrap();
        mfa_data::confirm_totp(&mut tx, dirk, 1).await.unwrap();
        tx.commit().await.unwrap();

        for (user, name) in [(dana, "dana"), (dirk, "dirk")] {
            sqlx::query("UPDATE identity_providers SET is_enabled = true WHERE id = $1")
                .bind(ldap)
                .execute(pool)
                .await
                .unwrap();
            let attempt = auth.throttle.begin(name, Net::default(), false).expect("the gate lets it through");
            let tx = locked_provider(pool, ldap).await;
            let ctx = anon();
            let (answer, ()) = tokio::join!(
                password_accepted(pool, &auth, &headers, &ctx, attempt.into(), user, name, LoginMethod::Ldap, None),
                disable_provider_under(pool, tx, ldap)
            );
            let e = answer.err().unwrap_or_else(|| panic!("{name}: signed in"));
            assert_eq!(e.code, ErrorCode::Unauthenticated, "{name}: {}", e.message);
            assert_eq!(rows_of(pool, "sessions", user).await, 0, "{name}: no session");
            assert_eq!(rows_of(pool, "mfa_challenges", user).await, 0, "{name}: no second-factor step");
        }
        let refused = auth_rows(pool, "login.failure").await;
        assert_eq!(refused.len(), 2);
        assert!(refused.iter().all(|r| r.3["reason"] == "provider_disabled"), "{refused:?}");
        db.drop().await;
    }

    /// GH#216: every refused sign-in (local wrong password, unknown name,
    /// directory no-match, disabled account) is held to the floor, counted from
    /// after the throttle; a success and a 429 are not held.
    #[tokio::test]
    async fn refused_sign_ins_are_held_to_the_floor() {
        let Some(db) = scratch::database("refused_sign_ins_are_held_to_the_floor").await else { return };
        let floor = Duration::from_secs(1);
        let base = auth_state();
        let auth = AuthState::new(
            AuthConfig { sign_in_failure_floor: floor, ..base.config.clone() },
            crate::secrets::Keyring::for_tests(),
        );
        let (pool, headers) = (&db.pool, HeaderMap::new());
        setup(pool, &auth, &headers, &from("192.0.2.1"), body("owner")).await.unwrap();
        let gone_password = format!("gone passphrase {}", Uuid::new_v4());
        let disabled = UserCreate {
            username: "gone".into(),
            display_name: "Gone".into(),
            email: "gone@example.test".into(),
            password: gone_password.as_str().into(),
            is_active: Some(false),
            profile_ids: vec![],
        };
        users::create(pool, &RequestContext::system("test", "test"), &disabled).await.unwrap();
        let ldap = crate::modules::mfa::tests::provider(pool, "ldap", false, "ldaps://dc.example.test").await;
        let mut tx = pool.begin().await.unwrap();
        let linked = crate::data::identity_providers::NewLinkedUser {
            provider_id: ldap,
            external_id: "erin",
            username: "erin",
            display_name: "erin",
            email: None,
        };
        crate::data::identity_providers::insert_linked(&mut tx, &linked).await.unwrap();
        tx.commit().await.unwrap();

        let wrong = OWNER_PASSWORD.to_uppercase();
        for (name, password) in [
            ("owner", wrong.as_str()),
            ("nobody", wrong.as_str()),
            ("erin", wrong.as_str()),
            ("gone", gone_password.as_str()),
        ] {
            let start = tokio::time::Instant::now();
            let e =
                login(pool, &auth, &headers, &from("198.51.100.7"), login_body(name, password)).await.err().unwrap();
            let done = tokio::time::Instant::now();
            assert_eq!(e.code, ErrorCode::Unauthenticated, "{name}");
            let until = e.hold_until.expect("held");
            assert!(until >= start + floor, "{name}: answered before the floor");
            assert!(until <= done + floor + floor / 20, "{name}: counted from after the throttle, 5 % jitter at most");
        }
        assert_eq!(auth_rows(pool, "login.failure").await.len(), 4, "recorded before the wait");

        let ok =
            login(pool, &auth, &headers, &from("198.51.100.7"), login_body("owner", OWNER_PASSWORD.as_str())).await;
        assert!(ok.is_ok(), "a success is not refused: {:?}", ok.err().map(|e| e.code));
        auth.throttle.freeze();
        for _ in 0..crate::auth::throttle::FREE_FAILURES {
            let _ = login(pool, &auth, &headers, &from("198.51.100.8"), login_body("owner", &wrong)).await;
        }
        let e = login(pool, &auth, &headers, &from("198.51.100.8"), login_body("owner", &wrong)).await.err().unwrap();
        assert_eq!((e.code, e.hold_until), (ErrorCode::RateLimited, None), "a 429 is not held");
        db.drop().await;
    }

    /// GH#499: while the directory cannot be reached, a name no account has
    /// gets a local account's answer (same body, held to the floor, counted
    /// and audited), not a quick unthrottled 503. A directory account still
    /// gets the 503.
    #[tokio::test]
    async fn an_unreachable_directory_does_not_tell_unknown_names_apart() {
        let Some(db) = scratch::database("an_unreachable_directory_does_not_tell_unknown_names_apart").await else {
            return;
        };
        let (pool, auth, headers) = (&db.pool, auth_state(), HeaderMap::new());
        setup(pool, &auth, &headers, &from("192.0.2.1"), body("owner")).await.unwrap();
        // Nothing listens on port 1: every sign-in it is asked about is Unavailable.
        let ldap = crate::modules::mfa::tests::provider(pool, "ldap", true, "ldaps://127.0.0.1:1").await;
        let mut tx = pool.begin().await.unwrap();
        let linked = crate::data::identity_providers::NewLinkedUser {
            provider_id: ldap,
            external_id: "erin",
            username: "erin",
            display_name: "erin",
            email: None,
        };
        crate::data::identity_providers::insert_linked(&mut tx, &linked).await.unwrap();
        tx.commit().await.unwrap();

        let wrong = OWNER_PASSWORD.to_uppercase();
        let mut answers = vec![];
        for name in ["owner", "nobody"] {
            let start = tokio::time::Instant::now();
            let e = login(pool, &auth, &headers, &from("198.51.100.7"), login_body(name, &wrong)).await.err().unwrap();
            assert_eq!(e.code, ErrorCode::Unauthenticated, "{name}");
            assert!(e.hold_until.expect("held") >= start + auth.config.sign_in_failure_floor, "{name}: held");
            answers.push(answer_bytes(e).await);
        }
        assert_eq!(answers[0], answers[1], "an unknown name answers like a local account");
        let failures = auth_rows(pool, "login.failure").await;
        assert_eq!(failures.len(), 2, "both audited");
        assert_eq!(failures[1].3["attemptedUsername"], "nobody");
        assert_eq!(failures[1].3["reason"], "directory_unavailable");

        // A directory account gets the 503, held and audited like the 401 (GH#586).
        let start = tokio::time::Instant::now();
        let e = login(pool, &auth, &headers, &from("198.51.100.7"), login_body("erin", &wrong)).await.err().unwrap();
        assert_eq!(e.code, ErrorCode::IdentityProviderUnavailable, "a directory account");
        assert!(e.hold_until.expect("held") >= start + auth.config.sign_in_failure_floor, "held");
        let failures = auth_rows(pool, "login.failure").await;
        assert_eq!(failures.len(), 3, "audited");
        assert_eq!(failures[2].3["reason"], "directory_unavailable");

        // An unknown name is throttled like any failure: one free failure was spent above.
        auth.throttle.freeze();
        for _ in 1..crate::auth::throttle::FREE_FAILURES {
            let e =
                login(pool, &auth, &headers, &from("198.51.100.7"), login_body("nobody", &wrong)).await.err().unwrap();
            assert_eq!(e.code, ErrorCode::Unauthenticated, "free failures left");
        }
        let e = login(pool, &auth, &headers, &from("198.51.100.7"), login_body("nobody", &wrong)).await.err().unwrap();
        assert_eq!(e.code, ErrorCode::RateLimited, "locked");
        db.drop().await;
    }

    /// GH#586: while a directory cannot be reached, the 503 for a directory
    /// account costs what a wrong password costs: held to the floor, audited,
    /// counted towards the name's lock and the server-wide budget. So the
    /// outage does not let anyone test names unthrottled and unseen.
    #[tokio::test]
    async fn a_directory_account_s_503_is_held_counted_and_audited() {
        let Some(db) = scratch::database("a_directory_account_s_503_is_held_counted_and_audited").await else {
            return;
        };
        use crate::auth::throttle::{FREE_FAILURES, GLOBAL_BUDGET, Gate, Net};
        let (pool, auth, headers) = (&db.pool, auth_state(), HeaderMap::new());
        setup(pool, &auth, &headers, &from("192.0.2.1"), body("owner")).await.unwrap();
        let ldap = crate::modules::mfa::tests::provider(pool, "ldap", true, "ldaps://127.0.0.1:1").await;
        // Enough directory accounts to use up the server-wide budget with one probe each.
        let probes = GLOBAL_BUDGET - FREE_FAILURES as usize;
        let names: Vec<String> = (0..probes).map(|i| format!("dir-{i:03}")).collect();
        let mut tx = pool.begin().await.unwrap();
        for name in std::iter::once("erin").chain(names.iter().map(String::as_str)) {
            let linked = crate::data::identity_providers::NewLinkedUser {
                provider_id: ldap,
                external_id: name,
                username: name,
                display_name: name,
                email: None,
            };
            crate::data::identity_providers::insert_linked(&mut tx, &linked).await.unwrap();
        }
        tx.commit().await.unwrap();
        auth.throttle.freeze();
        let floor = auth.config.sign_in_failure_floor;
        let wrong = OWNER_PASSWORD.to_uppercase();

        // One name: every 503 is held and audited, and the name locks like a wrong password's.
        for i in 1..=FREE_FAILURES {
            let start = tokio::time::Instant::now();
            let e =
                login(pool, &auth, &headers, &from("198.51.100.7"), login_body("erin", &wrong)).await.err().unwrap();
            assert_eq!(e.code, ErrorCode::IdentityProviderUnavailable, "attempt {i}");
            assert!(e.hold_until.expect("held") >= start + floor, "attempt {i}: held to the floor");
        }
        let failures = auth_rows(pool, "login.failure").await;
        assert_eq!(failures.len(), FREE_FAILURES as usize, "each one audited");
        assert!(
            failures.iter().all(|f| f.3["reason"] == "directory_unavailable" && f.3["attemptedUsername"] == "erin")
        );
        assert_eq!(auth_rows(pool, "login.locked").await.len(), 1, "the last free failure set the lock");
        let e = login(pool, &auth, &headers, &from("198.51.100.7"), login_body("erin", &wrong)).await.err().unwrap();
        assert_eq!((e.code, e.hold_until), (ErrorCode::RateLimited, None), "locked: 429, not another 503");

        // Many names, one probe each: the budget for all names runs out, and
        // sign-in is slowed to the slow lane rather than listing names at will.
        for name in &names {
            let e = login(pool, &auth, &headers, &from("203.0.113.9"), login_body(name, &wrong)).await.err().unwrap();
            assert_eq!(e.code, ErrorCode::IdentityProviderUnavailable, "{name}");
        }
        assert_eq!(auth_rows(pool, "login.failure").await.len(), GLOBAL_BUDGET, "every probe audited");
        let net = Net::of(Some("203.0.113.9".parse().unwrap()));
        assert_eq!(auth.throttle.check("dir-next", net), Gate::Slow, "the server-wide budget is used up");
        db.drop().await;
    }

    /// GH#570: while a directory drops packets, only the first lookup waits
    /// for the LDAP timeout; the directory is then skipped, and an unknown
    /// name answers at the floor like a local account's wrong password.
    #[tokio::test]
    async fn a_directory_that_times_out_is_skipped() {
        let Some(db) = scratch::database("a_directory_that_times_out_is_skipped").await else { return };
        let floor = Duration::from_secs(1);
        let base = auth_state();
        let auth = AuthState::new(
            AuthConfig { sign_in_failure_floor: floor, ..base.config.clone() },
            crate::secrets::Keyring::for_tests(),
        );
        let (pool, headers) = (&db.pool, HeaderMap::new());
        setup(pool, &auth, &headers, &from("192.0.2.1"), body("owner")).await.unwrap();
        // Accepts the connection and never answers: the TLS handshake waits for the timeout.
        let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ldaps://127.0.0.1:{}", silent.local_addr().unwrap().port());
        // Whether a sign-in asked the directory is counted here, not timed: under
        // full-suite load a skipped sign-in can queue for the password hashing
        // permits for seconds (GH#701).
        let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let accepted = asked.clone();
        tokio::spawn(async move {
            let mut held = vec![];
            while let Ok((socket, _)) = silent.accept().await {
                accepted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                held.push(socket);
            }
        });
        let asked = move || asked.load(std::sync::atomic::Ordering::SeqCst);
        let ldap = crate::modules::mfa::tests::provider(pool, "ldap", true, &url).await;
        let mut tx = pool.begin().await.unwrap();
        let linked = crate::data::identity_providers::NewLinkedUser {
            provider_id: ldap,
            external_id: "erin",
            username: "erin",
            display_name: "erin",
            email: None,
        };
        crate::data::identity_providers::insert_linked(&mut tx, &linked).await.unwrap();
        tx.commit().await.unwrap();

        let wrong = OWNER_PASSWORD.to_uppercase();
        // The answer's time: the later of the handler returning and the hold.
        let answer_time = |name: &'static str| {
            let (auth, headers, wrong) = (&auth, &headers, &wrong);
            async move {
                let start = tokio::time::Instant::now();
                let e = login(pool, auth, headers, &from("198.51.100.7"), login_body(name, wrong)).await.err().unwrap();
                let done = tokio::time::Instant::now();
                (e.code, e.hold_until.map_or(done, |until| until.max(done)) - start)
            }
        };
        let (code, first) = answer_time("nobody-1").await;
        assert_eq!(code, ErrorCode::Unauthenticated);
        // Waiting for the directory takes the LDAP timeout; anything under half
        // of it was not kept waiting, however loaded the machine (CI) is.
        let waited = crate::auth::sso::ldap::TIMEOUT / 2;
        assert!(first >= waited, "the first lookup waits for the timeout: {first:?}");
        assert_eq!(asked(), 1, "the first lookup asks the directory");
        // Skipped: none of these connects to the directory, so none waits for
        // its timeout, and each is held to the floor like a local account's.
        for name in ["owner", "nobody-2"] {
            let (code, took) = answer_time(name).await;
            assert_eq!(code, ErrorCode::Unauthenticated, "{name}");
            assert!(took >= floor, "{name}: held to the floor: {took:?}");
            assert_eq!(asked(), 1, "{name}: skipped, not asked");
        }
        let failures = auth_rows(pool, "login.failure").await;
        assert_eq!(failures[2].3["attemptedUsername"], "nobody-2");
        assert_eq!(failures[2].3["reason"], "directory_unavailable", "audited as before");

        // A directory account gets the 503 without the timeout, held to the floor (GH#586).
        let (code, took) = answer_time("erin").await;
        assert_eq!(code, ErrorCode::IdentityProviderUnavailable, "a directory account");
        assert!(took >= floor, "held to the floor: {took:?}");
        assert_eq!(asked(), 1, "skipped, not asked");

        // After the window, sign-ins sent at once (GH#595): none asks the
        // directory, so none waits for the timeout; a background probe does.
        crate::modules::sso::expire_skip(ldap);
        let burst = ["owner", "nobody-3", "nobody-4", "nobody-5", "nobody-6", "nobody-7"];
        let answers = futures_util::future::join_all(burst.map(&answer_time)).await;
        assert!(
            answers.iter().all(|(code, took)| *code == ErrorCode::Unauthenticated && *took >= floor),
            "{answers:?}"
        );
        assert_eq!(crate::modules::sso::skip_state(ldap), Some((false, true)), "the probe runs");
        // The probe times out too: skipped for another window, still at the floor.
        let deadline = tokio::time::Instant::now() + 2 * crate::auth::sso::ldap::TIMEOUT;
        while crate::modules::sso::skip_state(ldap) != Some((true, false)) {
            assert!(tokio::time::Instant::now() < deadline, "{:?}", crate::modules::sso::skip_state(ldap));
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(asked(), 2, "only the background probe asked the directory");
        for name in ["owner", "nobody-8"] {
            let (code, took) = answer_time(name).await;
            assert_eq!(code, ErrorCode::Unauthenticated, "{name}");
            assert!(took >= floor, "{name}: held to the floor after the probe: {took:?}");
            assert_eq!(asked(), 2, "{name}: skipped after the probe, not asked");
        }
        db.drop().await;
    }

    /// GH#216 through the router: the wait holds neither a database connection
    /// nor a capacity permit, and a success is not held.
    #[tokio::test]
    async fn a_held_sign_in_holds_no_connection_or_permit() {
        use axum::body::Body as HttpBody;
        use axum::http::Request;
        use tower::ServiceExt;

        let Some(db) = scratch::database("a_held_sign_in_holds_no_connection_or_permit").await else { return };
        let pool = db.pool.clone();
        setup(&pool, &auth_state(), &HeaderMap::new(), &anon(), body("owner")).await.unwrap();
        let capacity = crate::http::Capacity::with_sizes(8, 4, Duration::from_secs(10));
        let floor = |ms| move |cfg: &mut AuthConfig| cfg.sign_in_failure_floor = Duration::from_millis(ms);
        let post = |app: axum::Router, password: &str| {
            let body = serde_json::json!({ "username": "owner", "password": password }).to_string();
            let req = Request::post("/api/v1/auth/login").header("content-type", "application/json");
            async move { app.oneshot(req.body(HttpBody::from(body)).unwrap()).await.unwrap().status().as_u16() }
        };

        let long = crate::modules::api_tokens::tests::app_with_auth(pool.clone(), capacity.clone(), floor(3_600_000));
        let ok = tokio::time::timeout(Duration::from_secs(60), post(long.clone(), OWNER_PASSWORD.as_str())).await;
        assert_eq!(ok.expect("a success is not held"), 200);
        let held = tokio::spawn(post(long, &OWNER_PASSWORD.to_uppercase()));
        tokio::time::timeout(Duration::from_secs(60), async {
            while auth_rows(&pool, "login.failure").await.is_empty() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the failure is recorded before the wait");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!held.is_finished(), "held to the floor");
        assert_eq!(capacity.available(true), 4, "the permit was given back");
        assert_eq!(pool.num_idle() as u32, pool.size(), "no connection is held");
        held.abort();

        let short = crate::modules::api_tokens::tests::app_with_auth(pool.clone(), capacity, floor(300));
        let start = tokio::time::Instant::now();
        assert_eq!(post(short, "wrong").await, 401);
        assert!(start.elapsed() >= Duration::from_millis(300), "no earlier than the floor");
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

    /// GH#282 through the router: `ipAddress`, `sessions.ip_address` and the
    /// throttle agree on the client a trusted proxy reports; the forged
    /// leftmost hop is kept only as `claimedIpAddress`. Without a trusted
    /// proxy, a client's own `X-Forwarded-For` never becomes `ipAddress`.
    #[tokio::test]
    async fn the_audit_trail_records_the_client_a_trusted_proxy_reports() {
        use axum::body::Body as HttpBody;
        use axum::extract::ConnectInfo;
        use axum::http::Request;
        use tower::ServiceExt;

        let Some(db) = scratch::database("the_audit_trail_records_the_client_a_trusted_proxy_reports").await else {
            return;
        };
        let pool = db.pool.clone();
        setup(&pool, &auth_state(), &HeaderMap::new(), &anon(), body("owner")).await.unwrap();
        let capacity = crate::http::Capacity::with_sizes(8, 4, Duration::from_secs(10));
        let post = |app: axum::Router, peer: &str, xff: &str, password: &str| {
            let body = serde_json::json!({ "username": "owner", "password": password }).to_string();
            let peer: std::net::SocketAddr = format!("{peer}:40000").parse().unwrap();
            let req = Request::post("/api/v1/auth/login")
                .header("content-type", "application/json")
                .header("x-forwarded-for", xff)
                .extension(ConnectInfo(peer));
            async move { app.oneshot(req.body(HttpBody::from(body)).unwrap()).await.unwrap().status().as_u16() }
        };

        let proxied = crate::modules::api_tokens::tests::app_with_auth(pool.clone(), capacity.clone(), |cfg| {
            cfg.trusted_proxies = crate::auth::session::TrustedProxies::parse("127.0.0.1").unwrap()
        });
        let mut statuses = Vec::new();
        for i in 0..8 {
            statuses.push(post(proxied.clone(), "127.0.0.1", &format!("198.51.{i}.9, 127.0.5.20"), "wrong").await);
        }
        assert_eq!(statuses.last(), Some(&429), "the real network is locked: {statuses:?}");
        let locked = auth_rows(&pool, "login.locked").await;
        let v = &locked.last().expect("a login.locked row").3;
        assert_eq!((v["ipAddress"].as_str(), v["peerIpAddress"].as_str()), (Some("127.0.5.20"), Some("127.0.0.1")));
        let claimed = v["claimedIpAddress"].as_str().unwrap_or_default();
        assert!(claimed.starts_with("198.51.") && claimed.ends_with(".9"), "the forged hop: {claimed}");

        let direct = crate::modules::api_tokens::tests::app_with_auth(pool.clone(), capacity, |_| {});
        assert_eq!(post(direct, "127.0.6.30", "198.51.100.1", OWNER_PASSWORD.as_str()).await, 200);
        let success = auth_rows(&pool, "login.success").await;
        let v = &success.last().unwrap().3;
        assert_eq!(
            (v["ipAddress"].as_str(), v["claimedIpAddress"].as_str()),
            (Some("127.0.6.30"), Some("198.51.100.1"))
        );
        assert!(v.get("peerIpAddress").is_none(), "the peer is ipAddress");
        let ip: Option<String> =
            sqlx::query_scalar("SELECT host(ip_address) FROM sessions ORDER BY created_at DESC LIMIT 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(ip.as_deref(), Some("127.0.6.30"));
        db.drop().await;
    }

    /// Over HTTPS the session lives in `__Host-` cookies (GH-192): a planted
    /// plain-named cookie is never read (GH#285), and the CSRF header must
    /// match the session that was actually used.
    #[tokio::test]
    async fn host_prefixed_cookies_over_https() {
        use axum::body::Body as HttpBody;
        use axum::http::{Request, header};
        use serde_json::{Value, json};
        use tower::ServiceExt;

        let Some(db) = scratch::database("host_prefixed_cookies_over_https").await else { return };
        let app = crate::modules::api_tokens::tests::app_with(db.pool.clone(), CookieSecure::Auto);
        let send = |method: &str,
                    path: &str,
                    https: bool,
                    cookie: Option<&str>,
                    csrf: Option<&str>,
                    body: Option<Value>| {
            let mut req = Request::builder().method(method).uri(path);
            if https {
                req = req.header("x-forwarded-proto", "https");
            }
            if let Some(c) = cookie {
                req = req.header(header::COOKIE, c);
            }
            if let Some(c) = csrf {
                req = req.header("x-csrf-token", c);
            }
            let req = match body {
                Some(b) => req.header(header::CONTENT_TYPE, "application/json").body(HttpBody::from(b.to_string())),
                None => req.body(HttpBody::empty()),
            };
            let app = app.clone();
            async move {
                let res = app.oneshot(req.unwrap()).await.unwrap();
                let status = res.status().as_u16();
                let set: Vec<String> =
                    res.headers().get_all(header::SET_COOKIE).iter().map(|v| v.to_str().unwrap().to_owned()).collect();
                let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
                (status, serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null), set)
            }
        };
        let value = |set: &[String], name: &str| {
            set.iter().find_map(|c| c.split(';').next()?.strip_prefix(&format!("{name}=")).map(str::to_owned))
        };

        // Session A: opened over plain HTTP, so under the plain names.
        let setup = json!({ "username": "owner", "email": "owner@example.test", "displayName": "Owner", "password": OWNER_PASSWORD.as_str(), "setupToken": crate::auth::setup_token::TEST_TOKEN });
        let (status, a, set) = send("POST", "/api/v1/setup", false, None, None, Some(setup)).await;
        assert_eq!(status, 201, "{a}");
        assert!(set.iter().all(|c| !c.starts_with("__Host-")), "{set:?}");
        let (a_token, a_csrf) =
            (value(&set, "shadoucmdb_session").unwrap(), a["csrfToken"].as_str().unwrap().to_owned());

        // Session B: signed in over HTTPS, so under __Host- names; the plain ones are deleted.
        let login = json!({ "username": "owner", "password": OWNER_PASSWORD.as_str() });
        let (status, b, set) = send("POST", "/api/v1/auth/login", true, None, None, Some(login)).await;
        assert_eq!(status, 200, "{b}");
        let b_token = value(&set, "__Host-shadoucmdb_session").unwrap();
        let b_csrf = b["csrfToken"].as_str().unwrap().to_owned();
        assert_eq!(value(&set, "__Host-shadoucmdb_csrf").as_deref(), Some(b_csrf.as_str()));
        for c in &set {
            assert!(c.contains("; Path=/;") && c.contains("; Secure") && !c.contains("Domain"), "{c}");
        }
        assert_eq!(value(&set, "shadoucmdb_session").as_deref(), Some(""));
        assert_eq!(value(&set, "shadoucmdb_csrf").as_deref(), Some(""));

        // Both names, different sessions, either order: always B.
        for cookie in [
            format!("__Host-shadoucmdb_session={b_token}; shadoucmdb_session={a_token}"),
            format!("shadoucmdb_session={a_token}; __Host-shadoucmdb_session={b_token}"),
        ] {
            let (status, me, set) = send("GET", "/api/v1/auth/me", true, Some(&cookie), None, None).await;
            assert_eq!((status, me["csrfToken"].as_str()), (200, Some(b_csrf.as_str())), "{cookie}");
            assert!(set.is_empty(), "nothing to move: {set:?}");
            // The CSRF header must be B's: A's token (the plain cookie's session) is refused.
            let (status, v, _) =
                send("PUT", "/api/v1/ui-settings", true, Some(&cookie), Some(&a_csrf), Some(json!({}))).await;
            assert_eq!((status, v["error"]["code"].as_str()), (403, Some("CSRF_TOKEN_INVALID")));
            let (status, v, _) =
                send("PUT", "/api/v1/ui-settings", true, Some(&cookie), Some(&b_csrf), Some(json!({}))).await;
            assert_ne!(v["error"]["code"].as_str(), Some("CSRF_TOKEN_INVALID"), "{status} {v}");
        }
        // A __Host- cookie naming no session is not rescued by the plain one.
        let dead = format!("shadoucmdb_session={a_token}; __Host-shadoucmdb_session={}", "0".repeat(64));
        assert_eq!(send("GET", "/api/v1/auth/me", true, Some(&dead), None, None).await.0, 401);

        // Session A over plain HTTP: read by its plain name.
        let plain = format!("shadoucmdb_session={a_token}");
        let (status, _, set) = send("GET", "/api/v1/auth/me", false, Some(&plain), None, None).await;
        assert_eq!((status, set.len()), (200, 0));
        // GH#285: over HTTPS the same live session under the plain name is not
        // read (a sibling subdomain could have planted it), nor moved over.
        let (status, v, set) = send("GET", "/api/v1/auth/me", true, Some(&plain), None, None).await;
        assert_eq!((status, v["error"]["code"].as_str()), (401, Some("UNAUTHENTICATED")));
        assert!(set.iter().all(|c| !c.starts_with("__Host-shadoucmdb_session=")), "{set:?}");
        let (status, v, _) =
            send("PUT", "/api/v1/ui-settings", true, Some(&plain), Some(&a_csrf), Some(json!({}))).await;
        assert_eq!((status, v["error"]["code"].as_str()), (401, Some("UNAUTHENTICATED")));
        // A tossed plain cookie does not keep a signed-out browser signed in to it either.
        let tossed = format!("shadoucmdb_session={a_token}; shadoucmdb_csrf={a_csrf}");
        assert_eq!(send("GET", "/api/v1/auth/me", true, Some(&tossed), None, None).await.0, 401);
        // Session A itself is untouched.
        assert_eq!(send("GET", "/api/v1/auth/me", false, Some(&plain), None, None).await.0, 200);
        db.drop().await;
    }
}
