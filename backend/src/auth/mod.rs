//! Authentication and authorisation: local users with argon2id passwords,
//! TOTP two-factor sign-in, sign-in through OIDC providers and LDAP/AD
//! directories ([`sso`]), server-side sessions, CSRF protection, login
//! backoff, permission profiles and API tokens.
//!
//! [`authenticate`] turns the session cookie into a [`Principal`]
//! ([`token::authenticate`] does the same for `Authorization: Bearer`); the route
//! layer ([`crate::api::route`]) calls it for every non-public route, checks
//! CSRF on state-changing requests and the route's global permission, and hands
//! the principal to the service in its [`crate::api::context::RequestContext`].

pub mod cli;
pub mod events;
pub mod password;
pub mod permissions;
pub mod secret;
pub mod session;
pub mod setup_token;
pub mod sso;
pub mod throttle;
pub mod token;
pub mod totp;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::http::HeaderMap;
use sqlx::PgPool;
use uuid::Uuid;

use crate::api::context::{ClientInfo, RequestContext};
use crate::config::{AuthConfig, CookieSecure};
use crate::data::auth as data;
use crate::http::error::AppError;
use permissions::Permissions;
use throttle::LoginThrottle;

/// A signed-in user, resolved from their session or API token for one request.
#[derive(Debug, Clone)]
pub struct Principal {
    pub user_id: Uuid,
    pub username: String,
    pub credential: Credential,
    /// For a token: the owner's permissions narrowed to the token's profile.
    pub permissions: Permissions,
}

/// How the request authenticated.
#[derive(Debug, Clone)]
pub enum Credential {
    /// The session cookie; state-changing requests must echo `csrf_token`.
    /// While `mfa_enrolment_required`, only the routes marked
    /// `before_mfa_enrolment` answer (a profile requires MFA, none is set up);
    /// while `email_required`, only those marked `before_email_entry` (the
    /// account was created before e-mails were required, SHAA-1505).
    Session { id: Uuid, csrf_token: String, mfa_enrolment_required: bool, email_required: bool },
    /// `Authorization: Bearer`; not sent by browsers on their own, so no CSRF token.
    Token,
}

impl Principal {
    pub fn session_id(&self) -> Option<Uuid> {
        match &self.credential {
            Credential::Session { id, .. } => Some(*id),
            Credential::Token => None,
        }
    }

    pub fn mfa_enrolment_required(&self) -> bool {
        matches!(self.credential, Credential::Session { mfa_enrolment_required: true, .. })
    }

    pub fn email_required(&self) -> bool {
        matches!(self.credential, Credential::Session { email_required: true, .. })
    }

    pub fn csrf_token(&self) -> Option<&str> {
        match &self.credential {
            Credential::Session { csrf_token, .. } => Some(csrf_token),
            Credential::Token => None,
        }
    }
}

/// Process-wide authentication state, shared by every request.
pub struct AuthState {
    pub config: AuthConfig,
    /// Login, keyed by username.
    pub throttle: LoginThrottle,
    /// Directory sign-in, keyed by the entry the name found (GH#406), on top
    /// of `throttle`: every name the directory resolves to one entry shares
    /// its budget. Per key only: `throttle` holds the server-wide budget.
    pub directory_throttle: LoginThrottle,
    /// Changing one's own password, keyed by user id.
    pub password_throttle: LoginThrottle,
    /// Wrong setup tokens on first-run setup, one key for all requests (GH#230).
    pub setup_throttle: LoginThrottle,
    /// Discovered OIDC providers and their signing keys.
    pub oidc: sso::oidc::Cache,
    /// Seals the pending OIDC sign-in into its cookie; loaded on first use.
    oidc_state_key: tokio::sync::OnceCell<sso::login_state::SealingKey>,
    /// Set once the "session cookie without Secure under auto" warning has been logged.
    insecure_cookie_warned: AtomicBool,
    /// Encrypts and decrypts the TOTP seeds (`ENCRYPTION_KEY_FILE`).
    pub keyring: Arc<crate::secrets::Keyring>,
    /// The one-time token `POST /api/v1/setup` requires.
    pub setup: setup_token::SetupGate,
}

impl AuthState {
    pub fn new(config: AuthConfig, keyring: Arc<crate::secrets::Keyring>) -> Self {
        AuthState {
            oidc: sso::oidc::Cache::new(config.oidc_allowed_hosts.clone()),
            setup: setup_token::SetupGate::new(config.setup_token.clone(), config.setup_token_file.clone()),
            config,
            throttle: LoginThrottle::default(),
            directory_throttle: LoginThrottle::per_key(),
            password_throttle: LoginThrottle::per_key(),
            setup_throttle: LoginThrottle::per_key(),
            oidc_state_key: tokio::sync::OnceCell::new(),
            insecure_cookie_warned: AtomicBool::new(false),
            keyring,
        }
    }

    /// Whether a new session cookie gets `Secure`. Under `COOKIE_SECURE=auto`
    /// a "no" usually means a TLS-terminating proxy that does not forward the
    /// scheme, so the first one is logged: `auto` must not fail open silently.
    /// `never` is the operator's deliberate choice and is not warned about.
    pub fn session_cookie_secure(&self, headers: &HeaderMap) -> bool {
        let secure = session::secure_cookies(&self.config, headers);
        if !secure
            && self.config.cookie_secure == CookieSecure::Auto
            && !self.insecure_cookie_warned.swap(true, Ordering::Relaxed)
        {
            tracing::warn!(
                "issued a session cookie without the Secure attribute: COOKIE_SECURE=auto and the request did not \
                 arrive over HTTPS (no X-Forwarded-Proto: https or Forwarded: proto=https). If a reverse proxy \
                 terminates TLS in front of this server, make it forward the scheme or set COOKIE_SECURE=always. \
                 This warning is logged once per process."
            );
        }
        secure
    }

    /// The key sealing the `shadoucmdb_oidc` cookie, shared by every API
    /// process through `server_keys`. A database error is not cached: the
    /// next sign-in tries again.
    pub async fn oidc_state_key(&self, pool: &PgPool) -> sqlx::Result<&sso::login_state::SealingKey> {
        self.oidc_state_key.get_or_try_init(|| sso::login_state::load_or_create_key(pool)).await
    }
}

/// The principal behind the request's session cookie, if the session is live
/// (not expired, not idle too long) and the user is active.
///
/// An OIDC session that a `requireMfa` profile no longer exempts (see
/// [`data::MFA_REQUIRED`]) is ended here (`session.revoke`, reason
/// `mfa_not_enforced`) and the request answered like an expired session: its
/// account has no password to set up MFA with, and signing in again lets the
/// provider step up. `client`: the caller, for that audit row.
pub async fn authenticate(
    pool: &PgPool,
    cfg: &AuthConfig,
    headers: &HeaderMap,
    client: &ClientInfo,
) -> Result<Option<Principal>, AppError> {
    let Some(token) = session::session_token(cfg, headers) else { return Ok(None) };
    let Some(s) = data::resolve_session(pool, &session::token_hash(token), cfg.session_idle).await? else {
        return Ok(None);
    };
    if s.mfa_not_enforced {
        let ctx =
            RequestContext::system("requireMfa policy", crate::http::request_id::current()).with_client(client.clone());
        let mut tx = pool.begin().await?;
        let ended: Vec<_> = data::delete_session(&mut tx, s.session_id).await?.into_iter().collect();
        events::revoked(&mut tx, &ctx, &ended, events::RevokeReason::MfaNotEnforced).await?;
        tx.commit().await?;
        tracing::warn!(user = %s.username, "OIDC session ended: a profile requires MFA and the sign-in did not prove it");
        return Ok(None);
    }
    if s.needs_touch {
        data::touch_session(pool, s.session_id).await?;
    }
    let permissions = data::load_permissions(&mut *pool.acquire().await?, s.user_id).await?;
    Ok(Some(Principal {
        user_id: s.user_id,
        username: s.username,
        credential: Credential::Session {
            id: s.session_id,
            csrf_token: s.csrf_token,
            mfa_enrolment_required: s.mfa_enrolment_required,
            email_required: s.email_required,
        },
        permissions,
    }))
}

/// A state-changing request with a session must echo its CSRF token in
/// `X-CSRF-Token`. Token requests need none: the route layer only takes the
/// token path when an `Authorization: Bearer` header is present, and then
/// ignores the cookies, so a cross-site page (which cannot set that header)
/// never reaches a session without the CSRF check.
pub fn csrf_ok(principal: &Principal, headers: &HeaderMap) -> bool {
    match principal.csrf_token() {
        None => true,
        Some(expected) => headers
            .get(session::CSRF_HEADER)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|sent| session::constant_time_eq(sent.as_bytes(), expected.as_bytes())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use std::time::Duration;

    fn state(cookie_secure: CookieSecure) -> AuthState {
        AuthState::new(
            AuthConfig {
                session_idle: Duration::from_secs(60),
                session_max_age: Duration::from_secs(3600),
                cookie_secure,
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

    #[test]
    fn insecure_cookie_warning_only_under_auto_and_only_once() {
        let mut https = HeaderMap::new();
        https.insert("x-forwarded-proto", HeaderValue::from_static("https"));

        let auto = state(CookieSecure::Auto);
        assert!(auto.session_cookie_secure(&https));
        assert!(!auto.insecure_cookie_warned.load(Ordering::Relaxed), "a Secure cookie is not warned about");
        assert!(!auto.session_cookie_secure(&HeaderMap::new()));
        assert!(auto.insecure_cookie_warned.load(Ordering::Relaxed));
        // Later insecure cookies find the flag already set, so the warning is not repeated.
        assert!(!auto.session_cookie_secure(&HeaderMap::new()));
        assert!(auto.insecure_cookie_warned.load(Ordering::Relaxed));

        let never = state(CookieSecure::Never);
        assert!(!never.session_cookie_secure(&HeaderMap::new()));
        assert!(!never.insecure_cookie_warned.load(Ordering::Relaxed), "never is a deliberate choice");
    }
}
