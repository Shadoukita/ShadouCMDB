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
//! chain, at the caller's pace (GH#179). Uses left out of a window that no
//! later request closes are written by [`RefusalFlush`] as a summary row once
//! the window has passed, and on shutdown (GH#213). A live token refused a route
//! (`session_only`, `forbidden`) writes a row per request, as its accepted
//! uses do.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, Method, header};
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::permissions::{GlobalPermission, Permissions};
use super::{Credential, Principal, events, session};
use crate::api::context::{ClientInfo, RequestContext, forbidden};
use crate::data::api_tokens::{self as data, PresentedToken};
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
    /// The owner signs in through an identity provider that is disabled (GH#257).
    ProviderDisabled,
    /// The owner must use two-factor authentication and the token was not
    /// created from a session that proved it (GH#200).
    MfaRequired,
    /// The owner has an e-mail but no linked Person, so cannot sign in (SHAA-1505).
    AccountIncomplete,
    /// The owner has no e-mail yet: until they enter one in a session, their
    /// tokens are refused as their sessions are gated (SHAA-1505).
    EmailRequired,
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
            Refusal::ProviderDisabled => "provider_disabled",
            Refusal::MfaRequired => "mfa_required",
            Refusal::AccountIncomplete => "account_incomplete",
            Refusal::EmailRequired => "email_required",
            Refusal::NoScope => "no_scope",
            Refusal::SessionOnly => "session_only",
            Refusal::Forbidden(_) => "forbidden",
        }
    }

    /// The token itself is refused, whatever the route: its uses are rate-limited in the audit log.
    fn is_dead_token(self) -> bool {
        matches!(
            self,
            Refusal::Revoked
                | Refusal::Expired
                | Refusal::OwnerDisabled
                | Refusal::ProviderDisabled
                | Refusal::MfaRequired
                | Refusal::AccountIncomplete
                | Refusal::EmailRequired
                | Refusal::NoScope
        )
    }

    fn error(self) -> AppError {
        let unauthenticated = |m: &str| AppError::new(ErrorCode::Unauthenticated, m);
        match self {
            Refusal::Revoked => unauthenticated("This API token has been revoked"),
            Refusal::Expired => unauthenticated("This API token has expired"),
            Refusal::OwnerDisabled => unauthenticated("The owner of this API token is disabled"),
            Refusal::ProviderDisabled => {
                unauthenticated("The identity provider the owner of this API token signs in through is disabled")
            }
            Refusal::MfaRequired => unauthenticated(
                "The owner of this API token must use two-factor authentication, and this token was not created from \
                 a session signed in with a second factor. Create a new token after signing in with two-factor \
                 authentication.",
            ),
            Refusal::AccountIncomplete => unauthenticated(
                "The owner of this API token has no linked person and cannot sign in; an administrator must fix the \
                 account",
            ),
            Refusal::EmailRequired => AppError::new(
                ErrorCode::EmailRequired,
                "The owner of this API token has no e-mail address yet: they must sign in and enter it first",
            ),
            Refusal::NoScope => unauthenticated("The permission profile of this API token was deleted"),
            Refusal::SessionOnly => forbidden("This endpoint needs a signed-in session; API tokens cannot call it"),
            Refusal::Forbidden(p) => forbidden(format!("This requires the {} permission", p.as_str())),
        }
    }
}

/// How long the refused uses of a dead token, for one outcome, share a `token.use` row.
const REFUSAL_WINDOW: Duration = Duration::from_secs(60);

/// How often [`RefusalFlush`] looks for windows that have passed.
const FLUSH_INTERVAL: Duration = Duration::from_secs(10);

struct Window {
    since: Instant,
    /// `since` on the wall clock, for the summary row.
    started: DateTime<Utc>,
    unrecorded: u64,
    token: PresentedToken,
}

type RefusalKey = (Uuid, &'static str);

/// Per process: each replica records its own refusals.
static REFUSALS: LazyLock<Mutex<HashMap<RefusalKey, Window>>> = LazyLock::new(Default::default);

fn refusals() -> std::sync::MutexGuard<'static, HashMap<RefusalKey, Window>> {
    REFUSALS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether to record this refused use: Some(uses left out since the last row), or None to leave it out.
fn record_refusal(token: &PresentedToken, outcome: &'static str, now: Instant) -> Option<u64> {
    let mut windows = refusals();
    if windows.len() >= 4_096 {
        // Uses left out are kept for the flush.
        windows.retain(|_, w| now.duration_since(w.since) < REFUSAL_WINDOW || w.unrecorded > 0);
    }
    let started =
        Utc::now() - chrono::Duration::from_std(Instant::now().saturating_duration_since(now)).unwrap_or_default();
    let fresh = || Window { since: now, started, unrecorded: 0, token: token.clone() };
    match windows.get_mut(&(token.id, outcome)) {
        Some(w) if now.duration_since(w.since) < REFUSAL_WINDOW => {
            w.unrecorded += 1;
            None
        }
        Some(w) => Some(std::mem::replace(w, fresh()).unrecorded),
        None => {
            windows.insert((token.id, outcome), fresh());
            Some(0)
        }
    }
}

/// A summary row still to write: uses of `token` refused with `outcome` and
/// left out between `start` and `end`.
struct Unrecorded {
    token: PresentedToken,
    outcome: &'static str,
    count: u64,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
}

/// Removes the windows that have passed (every window when `all`, at
/// shutdown) among those `pick` selects, and returns the ones that left uses out.
fn take_windows(now: Instant, all: bool, pick: impl Fn(&RefusalKey) -> bool) -> Vec<Unrecorded> {
    let mut windows = refusals();
    let done: Vec<RefusalKey> = windows
        .iter()
        .filter(|(k, w)| (all || now.duration_since(w.since) >= REFUSAL_WINDOW) && pick(k))
        .map(|(k, _)| *k)
        .collect();
    let wall = Utc::now();
    done.into_iter()
        .filter_map(|k| windows.remove(&k).map(|w| (k.1, w)))
        .filter(|(_, w)| w.unrecorded > 0)
        .map(|(outcome, w)| {
            let window = chrono::Duration::from_std(REFUSAL_WINDOW).unwrap_or_default();
            Unrecorded {
                end: (w.started + window).min(wall),
                token: w.token,
                outcome,
                count: w.unrecorded,
                start: w.started,
            }
        })
        .collect()
}

/// Writes a `token.use` summary row for each passed window that left uses out.
async fn flush_where(pool: &PgPool, all: bool, pick: impl Fn(&RefusalKey) -> bool) {
    for u in take_windows(Instant::now(), all, pick) {
        let ctx = RequestContext::system("API token refusal summary", format!("token-refusals-{}", Uuid::new_v4()));
        let written = async {
            let mut tx = pool.begin().await?;
            events::token_refusals(&mut tx, &ctx, &u.token, u.outcome, u.count, u.start, u.end).await?;
            tx.commit().await
        }
        .await;
        match written {
            Ok(()) => tracing::warn!(
                token = %u.token.token_prefix,
                outcome = u.outcome,
                unrecorded = u.count,
                "API token refused, uses left out of the audit log summarised"
            ),
            // The count stays in the server log at least.
            Err(err) => tracing::error!(
                token = %u.token.token_prefix,
                outcome = u.outcome,
                unrecorded = u.count,
                window_start = %u.start,
                window_end = %u.end,
                error = %err,
                "cannot write the summary of refused API token uses to the audit log"
            ),
        }
    }
}

/// Writes the refused uses a window left out when no later request records
/// them: every [`FLUSH_INTERVAL`], and for every open window on [`stop`](Self::stop).
pub struct RefusalFlush {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl RefusalFlush {
    pub fn spawn(pool: PgPool) -> Self {
        Self::spawn_picking(pool, |_| true)
    }

    /// Only the windows `pick` selects (tests share [`REFUSALS`]).
    fn spawn_picking(pool: PgPool, pick: impl Fn(&RefusalKey) -> bool + Send + Sync + 'static) -> Self {
        let (stop, mut rx) = watch::channel(false);
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(FLUSH_INTERVAL) => flush_where(&pool, false, &pick).await,
                    _ = rx.changed() => {
                        flush_where(&pool, true, &pick).await;
                        return;
                    }
                }
            }
        });
        RefusalFlush { stop, task }
    }

    /// Writes every open window that left uses out, and stops; gives up after 5 s.
    pub async fn stop(self) {
        let _ = self.stop.send(true);
        if tokio::time::timeout(Duration::from_secs(5), self.task).await.is_err() {
            tracing::warn!("the summary of refused API token uses was not written within 5 s");
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
    } else if !t.provider_enabled {
        Some(Refusal::ProviderDisabled)
    } else if t.mfa_required {
        Some(Refusal::MfaRequired)
    } else if t.account_incomplete {
        Some(Refusal::AccountIncomplete)
    } else if t.email_missing {
        Some(Refusal::EmailRequired)
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
    let unrecorded = if r.is_dead_token() { record_refusal(&t, r.outcome(), Instant::now()) } else { Some(0) };
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

    fn presented(name: &str) -> PresentedToken {
        PresentedToken {
            id: Uuid::new_v4(),
            name: name.into(),
            token_prefix: "scmdb_0123abcd".into(),
            user_id: Uuid::new_v4(),
            username: "owner".into(),
            user_active: true,
            provider_enabled: true,
            profile_id: None,
            created_by_user_id: None,
            creator_active: false,
            revoked: true,
            expired: false,
            mfa_required: false,
            account_incomplete: false,
            email_missing: false,
        }
    }

    #[test]
    fn refused_uses_are_recorded_once_a_window() {
        let (token, t0) = (presented("t"), Instant::now());
        assert_eq!(record_refusal(&token, "revoked", t0), Some(0));
        for _ in 0..5 {
            assert_eq!(record_refusal(&token, "revoked", t0 + Duration::from_secs(30)), None);
        }
        // Another outcome, or another token, has its own row.
        assert_eq!(record_refusal(&token, "expired", t0), Some(0));
        assert_eq!(record_refusal(&presented("other"), "revoked", t0), Some(0));
        // A minute on, the next row counts the five left out.
        assert_eq!(record_refusal(&token, "revoked", t0 + REFUSAL_WINDOW), Some(5));
        assert_eq!(record_refusal(&token, "revoked", t0 + REFUSAL_WINDOW), None);
        assert!(
            !Refusal::SessionOnly.is_dead_token() && !Refusal::Forbidden(GlobalPermission::UsersManage).is_dead_token()
        );
        // An unmet MFA requirement refuses the token on every route, so a replay loop is rate-limited too.
        assert!(Refusal::MfaRequired.is_dead_token());
    }

    /// The `token.use` rows written for `token`: (outcome, unrecordedRefusals, the row's new_value).
    async fn rows(pool: &PgPool, token: &PresentedToken) -> Vec<(String, Option<u64>, serde_json::Value)> {
        let rows: Vec<(serde_json::Value,)> =
            sqlx::query_as("SELECT new_value FROM audit_log WHERE action = 'token.use' AND entity_id = $1 ORDER BY id")
                .bind(token.id)
                .fetch_all(pool)
                .await
                .unwrap();
        rows.into_iter()
            .map(|(v,)| (v["outcome"].as_str().unwrap_or("-").to_owned(), v["unrecordedRefusals"].as_u64(), v))
            .collect()
    }

    /// GH#213: a replay that stops inside a window leaves no later request to
    /// carry its count; the flush writes it once the window has passed.
    #[tokio::test]
    async fn uses_left_out_of_a_passed_window_are_flushed_once() {
        let Some(db) = crate::db::scratch::database("uses_left_out_of_a_passed_window_are_flushed_once").await else {
            return;
        };
        let (token, quiet, open) = (presented("leaked"), presented("quiet"), presented("open"));
        let ids = [token.id, quiet.id, open.id];
        let mine = move |k: &RefusalKey| ids.contains(&k.0);
        let passed = Instant::now() - REFUSAL_WINDOW - Duration::from_secs(1);
        assert_eq!(record_refusal(&token, "revoked", passed), Some(0));
        for _ in 0..999 {
            assert_eq!(record_refusal(&token, "revoked", passed + Duration::from_secs(50)), None);
        }
        // A passed window with nothing left out, and an open one, write nothing on a tick.
        assert_eq!(record_refusal(&quiet, "expired", passed), Some(0));
        assert_eq!(record_refusal(&open, "no_scope", Instant::now()), Some(0));
        assert_eq!(record_refusal(&open, "no_scope", Instant::now()), None);

        flush_where(&db.pool, false, mine).await;
        flush_where(&db.pool, false, mine).await;
        let got = rows(&db.pool, &token).await;
        assert_eq!(got.len(), 1, "exactly one summary row: {got:?}");
        let (outcome, count, v) = &got[0];
        assert_eq!((outcome.as_str(), *count), ("revoked", Some(999)));
        assert_eq!((v["tokenPrefix"].as_str(), v["username"].as_str()), (Some("scmdb_0123abcd"), Some("owner")));
        assert!(v.get("path").is_none() && v.get("method").is_none(), "no request, no path: {v}");
        let start: DateTime<Utc> = serde_json::from_value(v["windowStart"].clone()).unwrap();
        let end: DateTime<Utc> = serde_json::from_value(v["windowEnd"].clone()).unwrap();
        assert_eq!(end - start, chrono::Duration::from_std(REFUSAL_WINDOW).unwrap());
        let (actor_type, actor): (String, Option<String>) =
            sqlx::query_as("SELECT actor_type, actor_name FROM audit_log WHERE entity_id = $1")
                .bind(token.id)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!((actor_type.as_str(), actor.as_deref()), ("system", Some("API token refusal summary")));
        assert!(rows(&db.pool, &quiet).await.is_empty() && rows(&db.pool, &open).await.is_empty());
        // The next use after the flush opens a new window with a first-use row, as before.
        assert_eq!(record_refusal(&token, "revoked", Instant::now()), Some(0));

        // Shutdown writes the window still open.
        RefusalFlush::spawn_picking(db.pool.clone(), mine).stop().await;
        let got = rows(&db.pool, &open).await;
        assert_eq!(got.iter().map(|r| (r.0.as_str(), r.1)).collect::<Vec<_>>(), vec![("no_scope", Some(1))]);
        let start: DateTime<Utc> = serde_json::from_value(got[0].2["windowStart"].clone()).unwrap();
        let end: DateTime<Utc> = serde_json::from_value(got[0].2["windowEnd"].clone()).unwrap();
        assert!(start <= end && end <= Utc::now(), "an open window ends at shutdown");
        assert!(take_windows(Instant::now(), true, mine).is_empty(), "nothing is left to write");
        db.drop().await;
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
