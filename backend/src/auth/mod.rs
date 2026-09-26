//! Authentication and authorisation: local users with argon2id passwords,
//! server-side sessions, CSRF protection, login backoff and permission profiles.
//!
//! [`authenticate`] turns the session cookie into a [`Principal`]; the route
//! layer ([`crate::api::route`]) calls it for every non-public route, checks
//! CSRF on state-changing requests and the route's global permission, and hands
//! the principal to the service in its [`crate::api::context::RequestContext`].

pub mod cli;
pub mod password;
pub mod permissions;
pub mod session;
pub mod throttle;

use axum::http::HeaderMap;
use sqlx::PgPool;
use uuid::Uuid;

use crate::config::AuthConfig;
use crate::data::auth as data;
use crate::http::error::AppError;
use permissions::Permissions;
use throttle::LoginThrottle;

/// A signed-in user, resolved from their session for one request.
#[derive(Debug, Clone)]
pub struct Principal {
    pub user_id: Uuid,
    pub username: String,
    pub session_id: Uuid,
    pub csrf_token: String,
    pub permissions: Permissions,
}

/// Process-wide authentication state, shared by every request.
pub struct AuthState {
    pub config: AuthConfig,
    /// Login, keyed by username.
    pub throttle: LoginThrottle,
    /// Changing one's own password, keyed by user id.
    pub password_throttle: LoginThrottle,
}

impl AuthState {
    pub fn new(config: AuthConfig) -> Self {
        AuthState { config, throttle: LoginThrottle::default(), password_throttle: LoginThrottle::per_key() }
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
        session_id: s.session_id,
        csrf_token: s.csrf_token,
        permissions,
    }))
}

/// A state-changing request must echo the session's CSRF token in `X-CSRF-Token`.
pub fn csrf_ok(principal: &Principal, headers: &HeaderMap) -> bool {
    headers
        .get(session::CSRF_HEADER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|sent| session::constant_time_eq(sent.as_bytes(), principal.csrf_token.as_bytes()))
}
