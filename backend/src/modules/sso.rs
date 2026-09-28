//! Sign-in through identity providers: the OIDC redirect flow, LDAP/AD
//! sign-in behind the normal password form, and the accounts both lead to.
//!
//! An identity (provider + its stable id for the person) maps to one account.
//! The first sign-in creates it; every sign-in refreshes its name and e-mail
//! and sets its permission profiles to exactly those its groups map to. A
//! sign-in whose groups map to no profile is refused, and the account (if it
//! exists) loses its profiles. An account is never linked by username: if the
//! name is already taken the sign-in is refused ("account_conflict"). An
//! account disabled here stays disabled whatever the provider says. Local
//! accounts are not affected by any of this and remain the way in when a
//! provider is down (break-glass).

use axum::http::{HeaderMap, Method, StatusCode};
use serde::Serialize;
use serde_json::{Map, Value};
use sqlx::PgPool;
use utoipa::ToSchema;
use utoipa::openapi::path::{Parameter, ParameterBuilder, ParameterIn};
use utoipa::openapi::schema::{ObjectBuilder, Type};
use utoipa::openapi::{RefOr, Required};
use uuid::Uuid;

use super::users;
use crate::api::context::RequestContext;
use crate::api::route::{In, Json, NoBody, NoPath, NoQuery, PathInput, QueryInput, Redirect, Route, route};
use crate::api::schemas::USERNAME_PATTERN;
use crate::api::validate;
use crate::auth::events::{self, LoginMethod};
use crate::auth::sso::{ldap, oidc};
use crate::auth::{AuthState, session};
use crate::data::auth as auth_data;
use crate::data::crud::{self, AuditAction, AuditEntry};
use crate::data::identity_providers::{self as data, ProviderRow};
use crate::http::error::{AppError, ErrorCode};

pub const OIDC: &str = "oidc";
pub const LDAP: &str = "ldap";

/// How long the browser has to come back from the provider.
const LOGIN_STATE_TTL: std::time::Duration = std::time::Duration::from_secs(10 * 60);
/// Pending OIDC sign-ins kept at most (anonymous callers create them).
const MAX_PENDING: i64 = 10_000;
const CALLBACK_PATH: &str = "/api/v1/auth/oidc/callback";
const DISPLAY_NAME_MAX: usize = 200;

// ---------------------------------------------------------------------------
// Provider settings
// ---------------------------------------------------------------------------

pub fn oidc_settings(p: &ProviderRow) -> oidc::Settings {
    oidc::Settings {
        issuer_url: p.issuer_url.clone().unwrap_or_default(),
        client_id: p.client_id.clone().unwrap_or_default(),
        client_secret: p.client_secret.clone(),
        scopes: p.scopes.clone().unwrap_or_default(),
        username_claim: p.username_claim.clone().unwrap_or_default(),
        groups_claim: p.groups_claim.clone().unwrap_or_default(),
        ca_certificate: p.ca_certificate.clone(),
    }
}

pub fn ldap_settings(p: &ProviderRow) -> ldap::Settings {
    ldap::Settings {
        url: p.ldap_url.clone().unwrap_or_default(),
        start_tls: p.start_tls.unwrap_or(true),
        bind_dn: p.bind_dn.clone(),
        bind_password: p.bind_password.clone(),
        user_base_dn: p.user_base_dn.clone().unwrap_or_default(),
        user_filter: p.user_filter.clone().unwrap_or_default(),
        username_attribute: p.username_attribute.clone().unwrap_or_default(),
        display_name_attribute: p.display_name_attribute.clone().unwrap_or_default(),
        email_attribute: p.email_attribute.clone().unwrap_or_default(),
        group_attribute: p.group_attribute.clone().unwrap_or_default(),
        ca_certificate: p.ca_certificate.clone(),
    }
}

/// The redirect URI to register at the provider, when `PUBLIC_URL` is set.
pub fn redirect_uri(auth: &AuthState) -> Option<String> {
    auth.config.public_url.as_ref().map(|base| format!("{base}{CALLBACK_PATH}"))
}

// ---------------------------------------------------------------------------
// Accounts
// ---------------------------------------------------------------------------

/// What a provider says about the person signing in.
#[derive(Debug, Clone)]
pub struct ExternalIdentity {
    pub external_id: String,
    pub username: Option<String>,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub groups: Vec<String>,
}

impl ExternalIdentity {
    /// The name to record for a refused attempt.
    pub fn attempted(&self) -> &str {
        self.username.as_deref().unwrap_or(&self.external_id)
    }
}

/// Why a provider-vouched sign-in was still refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The provider sent no usable username (pattern of local usernames).
    InvalidUsername,
    /// Another account already has the username.
    AccountConflict,
    AccountDisabled,
    /// None of the user's groups maps to a profile.
    NotAuthorised,
    /// Recomputing the profiles would leave no active Administrator.
    LastAdministrator,
}

impl Refusal {
    /// The `ssoError` value the web UI gets after an OIDC sign-in.
    pub fn code(self) -> &'static str {
        match self {
            Refusal::InvalidUsername => "invalid_username",
            Refusal::AccountConflict => "account_conflict",
            Refusal::AccountDisabled => "account_disabled",
            Refusal::NotAuthorised => "not_authorised",
            Refusal::LastAdministrator => "last_administrator",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Refusal::InvalidUsername => {
                "Your identity provider did not send a usable username; ask an administrator to check the provider settings"
            }
            Refusal::AccountConflict => {
                "An account with your username already exists in ShadouCMDB; ask an administrator to resolve the conflict"
            }
            Refusal::AccountDisabled => "This account is disabled",
            Refusal::NotAuthorised => "None of your groups gives access to ShadouCMDB; ask an administrator for access",
            Refusal::LastAdministrator => {
                "Signing in would remove the last active administrator; ask another administrator to check the group mappings"
            }
        }
    }
}

fn usable_username(name: Option<&str>) -> Option<String> {
    let name = name?.trim();
    validate::cached_regex(USERNAME_PATTERN).filter(|re| re.is_match(name)).map(|_| name.to_owned())
}

fn usable_email(email: Option<&str>) -> Option<String> {
    let email = email?.trim();
    (!email.is_empty()
        && email.len() <= 254
        && email.split_once('@').is_some_and(|(a, b)| !a.is_empty() && !b.is_empty())
        && !email.chars().any(char::is_whitespace)
        && email.matches('@').count() == 1)
        .then(|| email.to_owned())
}

fn display_name(identity: &ExternalIdentity, username: &str) -> String {
    let name = identity.display_name.as_deref().map(str::trim).filter(|n| !n.is_empty()).unwrap_or(username);
    name.chars().filter(|c| !c.is_control()).take(DISPLAY_NAME_MAX).collect()
}

/// The provider as audit actor for the account changes a sign-in makes.
fn provider_actor(provider: &ProviderRow, request: &RequestContext) -> RequestContext {
    RequestContext::system(format!("identity provider \"{}\"", provider.name), request.request_id.clone())
        .with_client(request.client.clone())
}

fn user_audit(action: AuditAction, id: Uuid, old: Option<&users::User>, new: Option<&users::User>) -> AuditEntry {
    AuditEntry {
        action,
        entity_type: "users",
        entity_id: id,
        old_value: old.map(crud::json),
        new_value: new.map(crud::json),
    }
}

/// Finds or creates the identity's account and brings it up to date; returns
/// the account to open a session for, or why the sign-in is refused.
pub async fn link_account(
    pool: &PgPool,
    provider: &ProviderRow,
    identity: &ExternalIdentity,
    request: &RequestContext,
) -> Result<Result<(Uuid, String), Refusal>, AppError> {
    let actor = provider_actor(provider, request);
    let mut tx = pool.begin().await?;
    let existing = data::find_linked(&mut tx, provider.id, &identity.external_id, true).await?;
    let mapped = data::profiles_for_groups(&mut tx, provider.id, &identity.groups).await?;

    let Some(account) = existing else {
        if mapped.is_empty() {
            return Ok(Err(Refusal::NotAuthorised));
        }
        let Some(username) = usable_username(identity.username.as_deref()) else {
            return Ok(Err(Refusal::InvalidUsername));
        };
        if data::username_taken(&mut tx, &username, None).await? {
            return Ok(Err(Refusal::AccountConflict));
        }
        let email = usable_email(identity.email.as_deref());
        let new = data::NewLinkedUser {
            provider_id: provider.id,
            external_id: &identity.external_id,
            username: &username,
            display_name: &display_name(identity, &username),
            email: email.as_deref(),
        };
        let id = match data::insert_linked(&mut tx, &new).await.map_err(AppError::from) {
            Ok(id) => id,
            // Someone took the name between the check and the insert.
            Err(e) if e.code == ErrorCode::Conflict => return Ok(Err(Refusal::AccountConflict)),
            Err(e) => return Err(e),
        };
        auth_data::set_user_profiles(&mut tx, id, &mapped).await?;
        let dto = users::load(&mut tx, id).await?;
        crud::write_audit(&mut tx, &actor, vec![user_audit(AuditAction::Create, id, None, Some(&dto))]).await?;
        tx.commit().await?;
        tracing::info!(user = %username, provider = %provider.name, "account created by an identity provider");
        return Ok(Ok((id, username)));
    };

    if !account.is_active {
        return Ok(Err(Refusal::AccountDisabled));
    }
    let before = users::load(&mut tx, account.id).await?;
    if mapped.is_empty() {
        // Taken away at once: their other sessions lose access with the profiles.
        auth_data::set_user_profiles(&mut tx, account.id, &[]).await?;
        let after = users::load(&mut tx, account.id).await?;
        if crud::json(&after) != crud::json(&before) {
            let entry = user_audit(AuditAction::Update, account.id, Some(&before), Some(&after));
            crud::write_audit(&mut tx, &actor, vec![entry]).await?;
        }
        if let Err(e) = tx.commit().await {
            tracing::warn!(user = %account.username, error = %AppError::from(e).message, "could not remove the profiles of a user whose groups no longer map to any");
        }
        return Ok(Err(Refusal::NotAuthorised));
    }
    // A renamed identity takes its new name when it is free; otherwise it keeps the old one.
    let username = match usable_username(identity.username.as_deref()) {
        Some(name) if name == account.username => name,
        Some(name) if !data::username_taken(&mut tx, &name, Some(account.id)).await? => name,
        Some(name) => {
            tracing::warn!(user = %account.username, new_name = %name, "identity provider renamed the user to a name another account has; keeping the old name");
            account.username.clone()
        }
        None => account.username.clone(),
    };
    let email = usable_email(identity.email.as_deref()).or(account.email.clone());
    let name =
        if identity.display_name.is_some() { display_name(identity, &username) } else { account.display_name.clone() };
    data::refresh_linked(&mut tx, account.id, &username, &name, email.as_deref()).await?;
    auth_data::set_user_profiles(&mut tx, account.id, &mapped).await?;
    let after = users::load(&mut tx, account.id).await?;
    if crud::json(&after) != crud::json(&before) {
        let entry = user_audit(AuditAction::Update, account.id, Some(&before), Some(&after));
        crud::write_audit(&mut tx, &actor, vec![entry]).await?;
    }
    match tx.commit().await {
        Ok(()) => Ok(Ok((account.id, username))),
        Err(e) => match AppError::from(e) {
            e if e.code == ErrorCode::LastAdministrator => Ok(Err(Refusal::LastAdministrator)),
            e => Err(e),
        },
    }
}

/// Records a refused provider sign-in (`login.failure`, the name as the provider sent it).
async fn record_refusal(pool: &PgPool, ctx: &RequestContext, attempted: &str) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    events::login_failure(&mut tx, ctx, attempted).await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// LDAP behind the password form
// ---------------------------------------------------------------------------

pub enum DirectoryAnswer {
    SignedIn {
        user_id: Uuid,
        username: String,
    },
    /// The password was right, the sign-in is still refused.
    Refused(Refusal),
    /// No directory knows the name, or the password is wrong.
    NoMatch,
    /// A directory that might know the name could not be asked.
    Unavailable,
}

fn directory_identity(user: ldap::DirectoryUser) -> ExternalIdentity {
    ExternalIdentity {
        external_id: user.external_id,
        username: user.username,
        display_name: user.display_name,
        email: user.email,
        groups: user.groups,
    }
}

/// Signs in with a directory password. `linked`: the directory the account
/// already belongs to; otherwise the enabled directories are asked in order
/// and the first that knows the name decides (a wrong password there does not
/// fall through to the next, which might hold a different person of that name).
pub async fn directory_sign_in(
    pool: &PgPool,
    ctx: &RequestContext,
    username: &str,
    password: &str,
    linked: Option<Uuid>,
) -> Result<DirectoryAnswer, AppError> {
    let mut conn = pool.acquire().await?;
    let directories: Vec<ProviderRow> = match linked {
        Some(id) => {
            data::get(&mut conn, id, false).await?.filter(|p| p.is_enabled && p.kind == LDAP).into_iter().collect()
        }
        None => data::enabled(&mut conn, LDAP).await?,
    };
    drop(conn);
    let mut unavailable = false;
    for provider in directories {
        let outcome = ldap::authenticate(&ldap_settings(&provider), username, password).await;
        match outcome {
            Ok(ldap::Outcome::NotFound) => continue,
            Ok(ldap::Outcome::WrongPassword) => return Ok(DirectoryAnswer::NoMatch),
            Ok(ldap::Outcome::Ambiguous(n)) => {
                tracing::warn!(provider = %provider.name, entries = n, "LDAP user filter matched several entries; sign-in refused");
                return Ok(DirectoryAnswer::NoMatch);
            }
            Ok(ldap::Outcome::SignedIn(user)) => {
                let identity = directory_identity(user);
                return match link_account(pool, &provider, &identity, ctx).await? {
                    Ok((user_id, username)) => Ok(DirectoryAnswer::SignedIn { user_id, username }),
                    Err(refusal) => {
                        tracing::warn!(provider = %provider.name, user = %identity.attempted(), reason = refusal.code(), "directory sign-in refused");
                        Ok(DirectoryAnswer::Refused(refusal))
                    }
                };
            }
            Err(e) => {
                tracing::error!(provider = %provider.name, error = %e, "LDAP directory unavailable");
                unavailable = true;
            }
        }
    }
    Ok(if unavailable { DirectoryAnswer::Unavailable } else { DirectoryAnswer::NoMatch })
}

/// Whether any directory is enabled (the password form then asks it for unknown names).
pub async fn any_directory(pool: &PgPool) -> Result<bool, AppError> {
    Ok(sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM identity_providers WHERE is_enabled AND kind = 'ldap')")
        .fetch_one(pool)
        .await?)
}

// ---------------------------------------------------------------------------
// OIDC
// ---------------------------------------------------------------------------

/// Only a path on this server, so the sign-in cannot be turned into an open redirect.
pub fn safe_return_to(raw: Option<&str>) -> Option<String> {
    let r = raw?;
    let ok = r.starts_with('/')
        && !r.starts_with("//")
        && !r.starts_with("/\\")
        && r.len() <= 2048
        && r.bytes().all(|b| b.is_ascii_graphic())
        && !r.contains('\\');
    ok.then(|| r.to_owned())
}

fn to_ui(auth: &AuthState, path: &str) -> String {
    format!("{}{path}", auth.config.public_url.as_deref().unwrap_or_default())
}

fn failed(auth: &AuthState, headers: &HeaderMap, code: &str) -> Redirect {
    let secure = session::secure_cookies(&auth.config, headers);
    Redirect {
        location: to_ui(auth, &format!("/login?ssoError={code}")),
        cookies: vec![session::clear_oidc_cookie(secure)],
    }
}

/// `{id}` of an OIDC provider; anything but a UUID ends at the sign-in page.
pub struct ProviderPath(pub Option<Uuid>);

impl PathInput for ProviderPath {
    fn params() -> Vec<Parameter> {
        crate::api::route::IdPath::params()
    }
    fn parse(raw: &axum::extract::RawPathParams) -> Result<Self, AppError> {
        Ok(ProviderPath(crate::api::route::IdPath::parse(raw).ok().map(|p| p.0)))
    }
}

/// Query parameters of a browser navigation: the documented ones are read,
/// anything else is ignored (providers add their own, e.g. `session_state`).
#[derive(Default)]
pub struct NavigationQuery(pub std::collections::HashMap<String, String>);

impl NavigationQuery {
    fn get(&self, k: &str) -> Option<&str> {
        self.0.get(k).map(String::as_str)
    }
}

fn query_param(name: &str, description: &str) -> Parameter {
    ParameterBuilder::new()
        .name(name)
        .parameter_in(ParameterIn::Query)
        .required(Required::False)
        .description(Some(description))
        .schema(Some(RefOr::T(ObjectBuilder::new().schema_type(Type::String).max_length(Some(4096)).into())))
        .build()
}

pub struct StartQuery(pub NavigationQuery);
pub struct CallbackQuery(pub NavigationQuery);

fn parse_navigation(raw: Option<&str>) -> NavigationQuery {
    let mut map = std::collections::HashMap::new();
    for (k, v) in url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
        if v.len() <= 4096 {
            map.entry(k.into_owned()).or_insert_with(|| v.into_owned());
        }
    }
    NavigationQuery(map)
}

impl QueryInput for StartQuery {
    fn params() -> Vec<Parameter> {
        vec![query_param("returnTo", "Path on this server to open after signing in (default /)")]
    }
    fn parse(raw: Option<&str>) -> Result<Self, AppError> {
        Ok(StartQuery(parse_navigation(raw)))
    }
}

impl QueryInput for CallbackQuery {
    fn params() -> Vec<Parameter> {
        vec![
            query_param("code", "Authorization code"),
            query_param("state", "The state sent with the authorization request"),
            query_param("iss", "The provider's issuer (RFC 9207), checked when present"),
            query_param("error", "Set by the provider when the sign-in did not happen"),
            query_param("error_description", "The provider's explanation (logged only)"),
        ]
    }
    fn parse(raw: Option<&str>) -> Result<Self, AppError> {
        Ok(CallbackQuery(parse_navigation(raw)))
    }
}

async fn oidc_start(
    pool: &PgPool,
    auth: &AuthState,
    headers: &HeaderMap,
    id: Option<Uuid>,
    q: &NavigationQuery,
) -> Result<Redirect, AppError> {
    let provider = match id {
        Some(id) => data::get(&mut *pool.acquire().await?, id, false).await?,
        None => None,
    };
    let Some(provider) = provider.filter(|p| p.is_enabled && p.kind == OIDC) else {
        return Ok(failed(auth, headers, "unavailable"));
    };
    // A deployment setting, not a server fault: warn so the operator sees it.
    let Some(redirect_uri) = redirect_uri(auth) else {
        tracing::warn!(provider = %provider.name, "OIDC sign-in needs PUBLIC_URL (the address users open the web UI at)");
        return Ok(failed(auth, headers, "not_configured"));
    };
    let settings = oidc_settings(&provider);
    let discovered = match auth.oidc.provider(provider.id, &provider.updated_at.to_rfc3339(), &settings).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(provider = %provider.name, error = %e, "OIDC provider unavailable");
            return Ok(failed(auth, headers, "unavailable"));
        }
    };
    let pending: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oidc_login_states WHERE expires_at > now()").fetch_one(pool).await?;
    if pending >= MAX_PENDING {
        tracing::warn!(pending, "too many pending OIDC sign-ins; refusing new ones until they expire");
        return Ok(failed(auth, headers, "unavailable"));
    }
    let (state, nonce, verifier) = (oidc::random_value(), oidc::random_value(), oidc::random_value());
    let return_to = safe_return_to(q.get("returnTo"));
    data::create_login_state(
        pool,
        &data::NewLoginState {
            state_hash: &session::token_hash(&state),
            provider_id: provider.id,
            nonce: &nonce,
            code_verifier: &verifier,
            return_to: return_to.as_deref(),
            ttl: LOGIN_STATE_TTL,
        },
    )
    .await?;
    let request = oidc::AuthorizationRequest {
        redirect_uri: &redirect_uri,
        state: &state,
        nonce: &nonce,
        code_verifier: &verifier,
    };
    let location = match oidc::authorization_url(&discovered, &settings, &request) {
        Ok(url) => url,
        Err(e) => {
            tracing::error!(provider = %provider.name, error = %e, "OIDC provider unavailable");
            return Ok(failed(auth, headers, "unavailable"));
        }
    };
    let cookie = session::oidc_cookie(auth.session_cookie_secure(headers), &state, LOGIN_STATE_TTL);
    Ok(Redirect { location, cookies: vec![cookie] })
}

/// The identity in a verified ID token.
fn oidc_identity(claims: &Map<String, Value>, s: &oidc::Settings) -> ExternalIdentity {
    let text = |name: &str| oidc::claim(claims, name).and_then(Value::as_str).map(str::to_owned);
    ExternalIdentity {
        external_id: text("sub").unwrap_or_default(),
        username: text(&s.username_claim),
        display_name: text("name"),
        email: text("email"),
        groups: oidc::groups(claims, &s.groups_claim),
    }
}

async fn oidc_callback(
    pool: &PgPool,
    auth: &AuthState,
    headers: &HeaderMap,
    ctx: &RequestContext,
    q: &NavigationQuery,
) -> Result<Redirect, AppError> {
    // Only a callback carrying the state of this browser's own sign-in goes further.
    let (Some(state), Some(cookie)) = (q.get("state"), session::cookie(headers, session::OIDC_COOKIE)) else {
        return Ok(failed(auth, headers, "expired"));
    };
    if !session::constant_time_eq(state.as_bytes(), cookie.as_bytes()) {
        return Ok(failed(auth, headers, "expired"));
    }
    let Some(pending) = data::take_login_state(pool, &session::token_hash(state)).await? else {
        return Ok(failed(auth, headers, "expired"));
    };
    let provider = data::get(&mut *pool.acquire().await?, pending.provider_id, false).await?;
    let Some(provider) = provider.filter(|p| p.is_enabled && p.kind == OIDC) else {
        return Ok(failed(auth, headers, "unavailable"));
    };
    let attempted = format!("({})", provider.name);
    if let Some(error) = q.get("error") {
        let description: String = q.get("error_description").unwrap_or_default().chars().take(300).collect();
        tracing::warn!(provider = %provider.name, error, description, "OIDC provider did not sign the user in");
        record_refusal(pool, ctx, &attempted).await?;
        return Ok(failed(auth, headers, if error == "access_denied" { "cancelled" } else { "failed" }));
    }
    let settings = oidc_settings(&provider);
    let Some(redirect_uri) = redirect_uri(auth) else { return Ok(failed(auth, headers, "not_configured")) };
    let verified = async {
        let discovered = auth.oidc.provider(provider.id, &provider.updated_at.to_rfc3339(), &settings).await?;
        if let Some(iss) = q.get("iss")
            && iss != discovered.discovery.issuer
        {
            return Err(oidc::OidcError::new(format!(
                "callback names issuer {iss:?}, not {:?}",
                discovered.discovery.issuer
            )));
        }
        let code =
            q.get("code").filter(|c| !c.is_empty()).ok_or_else(|| oidc::OidcError::new("callback has no code"))?;
        let token = oidc::exchange_code(&discovered, &settings, &redirect_uri, code, &pending.code_verifier).await?;
        discovered.validate_id_token(&settings, &token, &pending.nonce, chrono::Utc::now().timestamp()).await
    }
    .await;
    let claims = match verified {
        Ok(claims) => claims,
        Err(e) => {
            tracing::error!(provider = %provider.name, error = %e, "OIDC sign-in failed");
            record_refusal(pool, ctx, &attempted).await?;
            return Ok(failed(auth, headers, "failed"));
        }
    };
    let identity = oidc_identity(&claims, &settings);
    let (user_id, username) = match link_account(pool, &provider, &identity, ctx).await? {
        Ok(account) => account,
        Err(refusal) => {
            tracing::warn!(provider = %provider.name, user = %identity.attempted(), reason = refusal.code(), "OIDC sign-in refused");
            record_refusal(pool, ctx, identity.attempted()).await?;
            return Ok(failed(auth, headers, refusal.code()));
        }
    };
    let purged = auth_data::purge_sessions(pool, auth.config.session_idle).await?;
    auth_data::record_login(pool, user_id).await?;
    tracing::info!(user = %username, provider = %provider.name, ip = ?ctx.client.ip, purged_sessions = purged, "signed in through OIDC");
    let mut cookies =
        super::auth::open_session(pool, auth, headers, ctx, user_id, &username, LoginMethod::Oidc).await?;
    cookies.push(session::clear_oidc_cookie(session::secure_cookies(&auth.config, headers)));
    let location = to_ui(auth, pending.return_to.as_deref().unwrap_or("/"));
    Ok(Redirect { location, cookies })
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

/// A button on the sign-in page.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignInProvider {
    pub id: Uuid,
    /// "Sign in with {name}"
    pub name: String,
    /// GET this (a browser navigation, not a fetch) to start signing in
    pub start_url: String,
}

/// How users can sign in besides a local account.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignInOptions {
    /// Enabled OIDC providers, in button order (empty unless PUBLIC_URL is set)
    pub oidc: Vec<SignInProvider>,
    /// An LDAP/AD directory is enabled: the username/password form also takes directory accounts
    pub directory: bool,
}

async fn sign_in_options(pool: &PgPool, auth: &AuthState) -> Result<SignInOptions, AppError> {
    let mut conn = pool.acquire().await?;
    let oidc = if auth.config.public_url.is_some() {
        data::enabled(&mut conn, OIDC)
            .await?
            .into_iter()
            .map(|p| SignInProvider { start_url: format!("/api/v1/auth/oidc/{}/start", p.id), id: p.id, name: p.name })
            .collect()
    } else {
        Vec::new()
    };
    let directory = !data::enabled(&mut conn, LDAP).await?.is_empty();
    Ok(SignInOptions { oidc, directory })
}

const TAG: &str = "Authentication";

pub const SSO_ERRORS: &str = "`expired` (no pending sign-in for this browser, or older than 10 minutes), `cancelled` (the user declined at the provider), `failed` (the provider refused, or its answer did not pass the checks; see the server log), `unavailable` (provider disabled, unknown or unreachable), `not_configured` (PUBLIC_URL is not set), `not_authorised` (none of the user's groups maps to a permission profile), `account_conflict` (another account has the username), `account_disabled`, `invalid_username` (the username claim is missing or not a valid username), `last_administrator`";

pub fn routes() -> Vec<Route> {
    vec![
        route(Method::GET, "/api/v1/auth/providers", "getSignInOptions")
            .tag(TAG)
            .summary("How users can sign in besides a local account: OIDC buttons and whether a directory is enabled")
            .public()
            .handle(|api, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| async move {
                Ok(Json(sign_in_options(&api.pool, &api.auth).await?))
            }),
        route(Method::GET, "/api/v1/auth/oidc/{id}/start", "startOidcSignIn")
            .tag(TAG)
            .summary("Start signing in with an OIDC provider (browser navigation; redirects to the provider)")
            .description(format!(
                "Redirects (302) to the provider's authorization endpoint with the authorization code flow, PKCE (S256), `state` and `nonce`, and sets the `shadoucmdb_oidc` cookie (HttpOnly, 10 min). On a problem it redirects to `/login?ssoError=<code>` instead: {SSO_ERRORS}."
            ))
            .public()
            .status(StatusCode::FOUND)
            .handle(|api, In(ProviderPath(id), StartQuery(q), NoBody): In<ProviderPath, StartQuery, NoBody>| async move {
                oidc_start(&api.pool, &api.auth, &api.headers, id, &q).await
            }),
        route(Method::GET, CALLBACK_PATH, "completeOidcSignIn")
            .tag(TAG)
            .summary("The redirect URI to register at OIDC providers: finishes the sign-in")
            .description(format!(
                "`{{PUBLIC_URL}}/api/v1/auth/oidc/callback`. Needs the `shadoucmdb_oidc` cookie set by the start route in the same browser. Exchanges the code, checks the ID token, creates or updates the account and sets its profiles from the group mappings, then sets the session cookies (like POST /api/v1/auth/login) and redirects (302) to `returnTo`. On a problem it redirects to `/login?ssoError=<code>`: {SSO_ERRORS}."
            ))
            .public()
            .status(StatusCode::FOUND)
            .handle(|api, In(NoPath, CallbackQuery(q), NoBody): In<NoPath, CallbackQuery, NoBody>| async move {
                oidc_callback(&api.pool, &api.auth, &api.headers, &api.ctx, &q).await
            }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn return_to_is_a_local_path() {
        assert_eq!(safe_return_to(Some("/items?q=a")).as_deref(), Some("/items?q=a"));
        assert_eq!(safe_return_to(Some("/")).as_deref(), Some("/"));
        for bad in ["//evil.example", "/\\evil.example", "https://evil.example", "items", "/a b", "/a\\b", ""] {
            assert_eq!(safe_return_to(Some(bad)), None, "{bad}");
        }
        assert_eq!(safe_return_to(None), None);
    }

    #[test]
    fn usernames_and_emails_from_providers() {
        assert_eq!(usable_username(Some(" alice@example.com ")).as_deref(), Some("alice@example.com"));
        assert_eq!(usable_username(Some("Alice Smith")), None);
        assert_eq!(usable_username(Some(&"a".repeat(65))), None);
        assert_eq!(usable_username(None), None);
        assert_eq!(usable_email(Some("a@b.test")).as_deref(), Some("a@b.test"));
        assert_eq!(usable_email(Some("not an email")), None);
        assert_eq!(usable_email(Some("a@b@c")), None);
    }
}
