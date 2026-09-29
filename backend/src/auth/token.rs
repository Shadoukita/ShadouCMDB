//! API tokens: `Authorization: Bearer scmdb_<64 hex>`.
//!
//! The secret is 256 random bits behind a fixed prefix (so secret scanners
//! and people recognise it). The database keeps its SHA-256 and the first
//! [`SHOWN_PREFIX_LEN`] characters; the secret itself is returned once, by
//! the request that creates the token.
//!
//! A request carrying a Bearer header is authenticated by the token alone:
//! its cookies are ignored, and a bad token is 401 rather than a fallback to
//! the session. Every accepted request writes a `token.use` audit row; an
//! unknown token writes none (anyone could grow the table with made-up
//! tokens). A token that can no longer authenticate (revoked, expired, owner
//! disabled, owner's MFA requirement unmet, profile deleted) is recorded at most once a minute per outcome,
//! and the next row counts the uses left out (`unrecordedRefusals`): a dead
//! token replayed in a loop must not grow the table, or queue on the audit
//! chain, at the caller's pace (GH#179). A live token refused a route
//! (`session_only`, `forbidden`) writes a row per request, as its accepted
//! uses do.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, Method, header};
use sqlx::PgPool;
use uuid::Uuid;

use super::permissions::{GlobalPermission, Permissions};
use super::{Credential, Principal, events, session};
use crate::api::context::{ClientInfo, RequestContext, forbidden};
use crate::data::api_tokens as data;
use crate::data::auth as auth_data;
use crate::http::error::{AppError, ErrorCode};

pub const PREFIX: &str = "scmdb_";
/// The prefix plus 8 hex characters: enough to tell tokens apart, far too little to guess one.
pub const SHOWN_PREFIX_LEN: usize = PREFIX.len() + 8;
const SECRET_LEN: usize = PREFIX.len() + 64;

pub fn new_secret() -> String {
    format!("{PREFIX}{}", session::new_token())
}

pub fn shown_prefix(secret: &str) -> String {
    secret.chars().take(SHOWN_PREFIX_LEN).collect()
}

/// The credential of an `Authorization: Bearer` header, if the request has
/// one (possibly empty). Other schemes (a proxy's Basic auth) are not ours
/// and leave the request to the session cookie.
pub fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?.trim();
    let (scheme, rest) = value.split_once(' ').unwrap_or((value, ""));
    scheme.eq_ignore_ascii_case("bearer").then(|| rest.trim())
}

/// The request being authorised, for the `token.use` row.
pub struct Use<'a> {
    pub method: &'a Method,
    pub path: &'a str,
    pub operation_id: &'a str,
}

/// Why a known token was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Revoked,
    Expired,
    OwnerDisabled,
    /// The owner must use two-factor authentication and the token was not
    /// created from a session that proved it (GH#200).
    MfaRequired,
    /// Its permission profile was deleted.
    NoScope,
    /// The route needs a browser session (sign-out, password, token, user, identity provider and permission profile
    /// administration, configuration import).
    SessionOnly,
    Forbidden(GlobalPermission),
}

impl Refusal {
    pub fn outcome(self) -> &'static str {
        match self {
            Refusal::Revoked => "revoked",
            Refusal::Expired => "expired",
            Refusal::OwnerDisabled => "owner_disabled",
            Refusal::MfaRequired => "mfa_required",
            Refusal::NoScope => "no_scope",
            Refusal::SessionOnly => "session_only",
            Refusal::Forbidden(_) => "forbidden",
        }
    }

    /// The token itself is refused, whatever the route: its uses are rate-limited in the audit log.
    fn is_dead_token(self) -> bool {
        matches!(
            self,
            Refusal::Revoked | Refusal::Expired | Refusal::OwnerDisabled | Refusal::MfaRequired | Refusal::NoScope
        )
    }

    fn error(self) -> AppError {
        let unauthenticated = |m: &str| AppError::new(ErrorCode::Unauthenticated, m);
        match self {
            Refusal::Revoked => unauthenticated("This API token has been revoked"),
            Refusal::Expired => unauthenticated("This API token has expired"),
            Refusal::OwnerDisabled => unauthenticated("The owner of this API token is disabled"),
            Refusal::MfaRequired => unauthenticated(
                "The owner of this API token must use two-factor authentication, and this token was not created from \
                 a session signed in with a second factor. Create a new token after signing in with two-factor \
                 authentication.",
            ),
            Refusal::NoScope => unauthenticated("The permission profile of this API token was deleted"),
            Refusal::SessionOnly => forbidden("This endpoint needs a signed-in session; API tokens cannot call it"),
            Refusal::Forbidden(p) => forbidden(format!("This requires the {} permission", p.as_str())),
        }
    }
}

/// How long the refused uses of a dead token, for one outcome, share a `token.use` row.
const REFUSAL_WINDOW: Duration = Duration::from_secs(60);

struct Window {
    since: Instant,
    unrecorded: u64,
}

type RefusalKey = (Uuid, &'static str);

/// Per process: each replica records its own refusals.
static REFUSALS: LazyLock<Mutex<HashMap<RefusalKey, Window>>> = LazyLock::new(Default::default);

/// Whether to record this refused use: Some(uses left out since the last row), or None to leave it out.
fn record_refusal(key: RefusalKey, now: Instant) -> Option<u64> {
    let mut windows = REFUSALS.lock().unwrap_or_else(PoisonError::into_inner);
    if windows.len() >= 4_096 {
        windows.retain(|_, w| now.duration_since(w.since) < REFUSAL_WINDOW);
    }
    match windows.get_mut(&key) {
        Some(w) if now.duration_since(w.since) < REFUSAL_WINDOW => {
            w.unrecorded += 1;
            None
        }
        Some(w) => Some(std::mem::replace(w, Window { since: now, unrecorded: 0 }).unrecorded),
        None => {
            windows.insert(key, Window { since: now, unrecorded: 0 });
            Some(0)
        }
    }
}

fn invalid() -> AppError {
    AppError::new(ErrorCode::Unauthenticated, "Invalid API token")
}

/// Resolves the Bearer secret, enforces the route's rules for tokens and
/// audits the use. `required` is the route's global permission, if any.
#[allow(clippy::too_many_arguments)]
pub async fn authenticate(
    pool: &PgPool,
    secret: &str,
    required: Option<GlobalPermission>,
    session_only: bool,
    request_id: String,
    client: ClientInfo,
    used: Use<'_>,
) -> Result<RequestContext, AppError> {
    if secret.len() != SECRET_LEN || !secret.starts_with(PREFIX) {
        return Err(invalid());
    }
    let Some(t) = data::find_by_hash(pool, &session::token_hash(secret)).await? else {
        tracing::warn!(ip = ?client.ip, "request with an unknown API token");
        return Err(invalid());
    };
    let mut refusal = if t.revoked {
        Some(Refusal::Revoked)
    } else if t.expired {
        Some(Refusal::Expired)
    } else if !t.user_active {
        Some(Refusal::OwnerDisabled)
    } else if t.mfa_required {
        Some(Refusal::MfaRequired)
    } else if session_only {
        Some(Refusal::SessionOnly)
    } else {
        None
    };
    let mut permissions = Permissions::default();
    if refusal.is_none() {
        match t.profile_id {
            None => refusal = Some(Refusal::NoScope),
            Some(profile_id) => {
                let mut conn = pool.acquire().await?;
                let owner = auth_data::load_permissions(&mut conn, t.user_id).await?;
                permissions = owner.intersect(&data::profile_permissions(&mut conn, profile_id).await?);
                // A token minted for someone else is also capped at what its
                // creator holds now, so promoting the owner does not widen it
                // beyond its creator's rights (GH#178).
                if let Some(creator) = t.created_by_user_id.filter(|&c| c != t.user_id) {
                    permissions = if t.creator_active {
                        permissions.intersect(&auth_data::load_permissions(&mut conn, creator).await?)
                    } else {
                        Permissions::default()
                    };
                }
                refusal = required.filter(|p| !permissions.has(*p)).map(Refusal::Forbidden);
            }
        }
    }
    let principal =
        Principal { user_id: t.user_id, username: t.username.clone(), credential: Credential::Token, permissions };
    let ctx = RequestContext::token(Arc::new(principal), request_id).with_client(client);
    let Some(r) = refusal else {
        let mut tx = pool.begin().await?;
        events::token_use(&mut tx, &ctx, &t, None, &used, 0).await?;
        data::record_use(&mut tx, t.id, ctx.client.ip).await?;
        tx.commit().await?;
        return Ok(ctx);
    };
    let unrecorded = if r.is_dead_token() { record_refusal((t.id, r.outcome()), Instant::now()) } else { Some(0) };
    if let Some(unrecorded) = unrecorded {
        tracing::warn!(token = %t.token_prefix, outcome = r.outcome(), unrecorded, "API token refused");
        let mut tx = pool.begin().await?;
        events::token_use(&mut tx, &ctx, &t, refusal, &used, unrecorded).await?;
        tx.commit().await?;
    }
    Err(r.error())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn auth(v: &'static str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::AUTHORIZATION, HeaderValue::from_static(v));
        h
    }

    #[test]
    fn secrets_have_the_prefix_and_256_random_bits() {
        let s = new_secret();
        assert_eq!(s.len(), SECRET_LEN);
        assert!(s.starts_with(PREFIX));
        assert!(s[PREFIX.len()..].bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(s, new_secret());
        assert_eq!(shown_prefix(&s).len(), SHOWN_PREFIX_LEN);
        assert!(s.starts_with(&shown_prefix(&s)));
    }

    #[test]
    fn refused_uses_are_recorded_once_a_window() {
        let (token, t0) = (Uuid::new_v4(), Instant::now());
        assert_eq!(record_refusal((token, "revoked"), t0), Some(0));
        for _ in 0..5 {
            assert_eq!(record_refusal((token, "revoked"), t0 + Duration::from_secs(30)), None);
        }
        // Another outcome, or another token, has its own row.
        assert_eq!(record_refusal((token, "expired"), t0), Some(0));
        assert_eq!(record_refusal((Uuid::new_v4(), "revoked"), t0), Some(0));
        // A minute on, the next row counts the five left out.
        assert_eq!(record_refusal((token, "revoked"), t0 + REFUSAL_WINDOW), Some(5));
        assert_eq!(record_refusal((token, "revoked"), t0 + REFUSAL_WINDOW), None);
        assert!(
            !Refusal::SessionOnly.is_dead_token() && !Refusal::Forbidden(GlobalPermission::UsersManage).is_dead_token()
        );
        // An unmet MFA requirement refuses the token on every route, so a replay loop is rate-limited too.
        assert!(Refusal::MfaRequired.is_dead_token());
    }

    #[test]
    fn only_the_bearer_scheme_is_a_token() {
        assert_eq!(bearer(&auth("Bearer scmdb_abc")), Some("scmdb_abc"));
        assert_eq!(bearer(&auth("bearer   scmdb_abc ")), Some("scmdb_abc"));
        assert_eq!(bearer(&auth("Bearer")), Some(""), "an empty Bearer is refused, not ignored");
        assert_eq!(bearer(&auth("Basic dXNlcjpwdw==")), None, "a proxy's Basic auth is not ours");
        assert_eq!(bearer(&HeaderMap::new()), None);
    }
}
