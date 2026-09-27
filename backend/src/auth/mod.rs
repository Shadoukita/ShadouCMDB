//! Authentication and authorisation: local users with argon2id passwords,
//! server-side sessions, CSRF protection, login backoff, permission profiles
//! and API tokens.
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
pub mod session;
pub mod throttle;
pub mod token;

use std::sync::atomic::{AtomicBool, Ordering};

use axum::http::HeaderMap;
use sqlx::PgPool;
use uuid::Uuid;

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
    Session { id: Uuid, csrf_token: String },
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
    /// Changing one's own password, keyed by user id.
    pub password_throttle: LoginThrottle,
    /// Set once the "session cookie without Secure under auto" warning has been logged.
    insecure_cookie_warned: AtomicBool,
}

impl AuthState {
    pub fn new(config: AuthConfig) -> Self {
        AuthState {
            config,
            throttle: LoginThrottle::default(),
            password_throttle: LoginThrottle::per_key(),
            insecure_cookie_warned: AtomicBool::new(false),
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
}

/// The principal behind the request's session cookie, if the session is live
/// (not expired, not idle too long) and the user is active.
pub async fn authenticate(pool: &PgPool, cfg: &AuthConfig, headers: &HeaderMap) -> Result<Option<Principal>, AppError> {
    let Some(token) = session::cookie(headers, session::SESSION_COOKIE) else { return Ok(None) };
    let Some(s) = data::resolve_session(pool, &session::token_hash(token), cfg.session_idle).await? else {
        return Ok(None);
    };
    if s.needs_touch {
        data::touch_session(pool, s.session_id).await?;
    }
    let permissions = data::load_permissions(&mut *pool.acquire().await?, s.user_id).await?;
    Ok(Some(Principal {
        user_id: s.user_id,
        username: s.username,
        credential: Credential::Session { id: s.session_id, csrf_token: s.csrf_token },
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
        AuthState::new(AuthConfig {
            session_idle: Duration::from_secs(60),
            session_max_age: Duration::from_secs(3600),
            cookie_secure,
        })
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
