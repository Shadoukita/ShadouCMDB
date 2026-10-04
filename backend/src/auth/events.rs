//! Authentication events in `audit_log`: sessions (entity type `sessions`),
//! API token use (entity type `api_tokens`, the token's id) and two-factor
//! sign-in (`mfa.*`, entity type `users`, the user's id).
//!
//! Each row carries the request's id, the client IP (and the TCP peer when it
//! differs) and user agent (see
//! [`crate::api::context::ClientInfo`]) and, in `new_value`, the event's
//! details. Never recorded: the password, the session token or its hash, the
//! CSRF token, an API token's secret or its hash, a TOTP secret, an
//! authenticator or recovery code or its hash.
//!
//! A failed sign-in stores the username as typed (truncated) and nothing about
//! whether it exists, so the audit log is not an enumeration oracle for those
//! who can read it. That name may be a password typed into the wrong field
//! (GH#415): it is kept here, where reading needs the audit permission and the
//! retention period removes it, because brute-force forensics need it. The
//! server log names only existing accounts. Answers refused while a username
//! is locked (429) are not recorded: they cost no password check, and
//! recording them would let an anonymous client grow the table at will. The
//! lock itself is.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use super::token::{Refusal, Use};
use crate::api::context::RequestContext;
use crate::data::api_tokens::PresentedToken;
use crate::data::auth::EndedSession;
use crate::data::crud::{self, AuditAction, AuditEntry};

const ENTITY: &str = "sessions";
pub const TOKEN_ENTITY: &str = "api_tokens";
const MFA_ENTITY: &str = "users";

/// Longest attempted username kept (the real ones are at most 64 characters).
const ATTEMPTED_USERNAME_MAX: usize = 64;
/// Longest request path kept in `token.use`: the path is recorded before it is
/// validated, and a row must stay small enough to export (GH#179).
pub const TOKEN_PATH_MAX: usize = 512;

/// Why the API ended a session.
#[derive(Debug, Clone, Copy)]
pub enum RevokeReason {
    UserDisabled,
    UserDeleted,
    /// An administrator (or the CLI) set a new password.
    PasswordReset,
    /// The user changed their own password; their other sessions end.
    PasswordChanged,
    /// A new sign-in in the same browser replaced the session its cookie named.
    Replaced,
    /// The identity provider the user signs in through was disabled or deleted.
    ProviderDisabled,
    /// An OIDC session no longer met `requireMfa`: a profile now requires MFA,
    /// or the provider now verifies MFA and the sign-in did not prove it.
    MfaNotEnforced,
    /// The user turned their own MFA off; their other sessions end (GH#280).
    MfaDisabled,
    /// An administrator reset the user's MFA; their sessions end (GH#280).
    MfaReset,
    /// The user confirmed an authenticator; their other sessions that did not
    /// prove it end (GH#292).
    MfaEnrolled,
    /// The user changed their own password or confirmed an authenticator in
    /// this session; it continues under a new id, token and CSRF token (GH#510).
    Rotated,
}

impl RevokeReason {
    fn as_str(self) -> &'static str {
        match self {
            RevokeReason::UserDisabled => "user_disabled",
            RevokeReason::UserDeleted => "user_deleted",
            RevokeReason::PasswordReset => "password_reset",
            RevokeReason::PasswordChanged => "password_changed",
            RevokeReason::Replaced => "replaced",
            RevokeReason::ProviderDisabled => "provider_disabled",
            RevokeReason::MfaNotEnforced => "mfa_not_enforced",
            RevokeReason::MfaDisabled => "mfa_disabled",
            RevokeReason::MfaReset => "mfa_reset",
            RevokeReason::MfaEnrolled => "mfa_enrolled",
            RevokeReason::Rotated => "rotated",
        }
    }
}

/// What an OIDC sign-in says about the second factor (`providerMfa` in the
/// login row), so an auditor can tell which sessions rest on unverified trust.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderMfa {
    /// The ID token proved a second factor (provider set to verify).
    Verified,
    /// The provider is trusted to enforce MFA; nothing was checked.
    Trusted,
    /// No proof (allowed only while no profile of the user requires MFA).
    None,
}

impl ProviderMfa {
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderMfa::Verified => "verified",
            ProviderMfa::Trusted => "trusted",
            ProviderMfa::None => "none",
        }
    }
}

/// How a session was opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginMethod {
    Password,
    /// Password, then an authenticator code.
    Totp,
    /// Password, then a one-time recovery code.
    RecoveryCode,
    /// First-run setup signs the new administrator in.
    Setup,
    /// An OpenID Connect provider vouched for the user.
    Oidc(ProviderMfa),
    /// An LDAP / Active Directory bind with the user's password.
    Ldap,
}

fn attempted(username: &str) -> String {
    username.chars().take(ATTEMPTED_USERNAME_MAX).collect()
}

/// The first [`TOKEN_PATH_MAX`] characters of `path`, `…` marking a cut.
fn bounded_path(path: &str) -> String {
    match path.char_indices().nth(TOKEN_PATH_MAX) {
        Some((cut, _)) => format!("{}…", &path[..cut]),
        None => path.to_owned(),
    }
}

/// `{...details, ipAddress, userAgent}` of the request being handled.
/// `ipAddress` is the TCP peer, or behind a trusted proxy the client it
/// reports (GH#282), so the client cannot choose it. `peerIpAddress` (the TCP
/// peer: the proxy) and `claimedIpAddress` (the leftmost forwarded hop, which
/// the client may have made up) are added when they differ from it.
fn details(ctx: &RequestContext, mut fields: serde_json::Map<String, Value>) -> Value {
    fields.insert("ipAddress".into(), json!(ctx.client.ip));
    if let Some(peer) = ctx.client.peer_ip.filter(|p| Some(*p) != ctx.client.ip) {
        fields.insert("peerIpAddress".into(), json!(peer));
    }
    if let Some(claimed) = ctx.client.claimed_ip.filter(|c| Some(*c) != ctx.client.ip) {
        fields.insert("claimedIpAddress".into(), json!(claimed));
    }
    fields.insert("userAgent".into(), json!(ctx.client.user_agent));
    Value::Object(fields)
}

fn fields(v: Value) -> serde_json::Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    }
}

async fn write(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    action: AuditAction,
    entity_id: Uuid,
    new_value: Value,
) -> sqlx::Result<()> {
    write_for(conn, ctx, action, ENTITY, entity_id, new_value).await
}

async fn write_for(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    action: AuditAction,
    entity_type: &'static str,
    entity_id: Uuid,
    new_value: Value,
) -> sqlx::Result<()> {
    let entry = AuditEntry { action, entity_type, entity_id, old_value: None, new_value: Some(new_value) };
    crud::write_audit(conn, ctx, vec![entry]).await
}

/// A session was opened. `ctx` should already act as the user.
pub async fn login_success(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    session_id: Uuid,
    user_id: Uuid,
    username: &str,
    method: LoginMethod,
) -> sqlx::Result<()> {
    let name = match method {
        LoginMethod::Password => "password",
        LoginMethod::Totp => "totp",
        LoginMethod::RecoveryCode => "recovery_code",
        LoginMethod::Setup => "setup",
        LoginMethod::Oidc(_) => "oidc",
        LoginMethod::Ldap => "ldap",
    };
    let mut f = fields(json!({ "userId": user_id, "username": username, "method": name }));
    if let LoginMethod::Oidc(mfa) = method {
        f.insert("providerMfa".into(), json!(mfa.as_str()));
    }
    let v = details(ctx, f);
    write(conn, ctx, AuditAction::LoginSuccess, session_id, v).await
}

/// A sign-in was refused. `reason`: why a sign-in with valid credentials was
/// still refused (the refusal's code, e.g. `mfa_not_enforced`); None for a
/// wrong password or unknown name. Returns the attempt's id, which is the
/// row's entity id (no session exists) and is shared with a `login.locked`
/// row for the same attempt.
pub async fn login_failure(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    username: &str,
    reason: Option<&str>,
) -> sqlx::Result<Uuid> {
    let attempt = Uuid::new_v4();
    let mut f = fields(json!({ "attemptedUsername": attempted(username) }));
    if let Some(reason) = reason {
        f.insert("reason".into(), json!(reason));
    }
    let v = details(ctx, f);
    write(conn, ctx, AuditAction::LoginFailure, attempt, v).await?;
    Ok(attempt)
}

/// The failure of attempt `attempt` locked the username for `lock`.
pub async fn login_locked(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    attempt: Uuid,
    username: &str,
    lock: Duration,
) -> sqlx::Result<()> {
    let v = details(
        ctx,
        fields(json!({ "attemptedUsername": attempted(username), "lockedForSeconds": lock.as_secs().max(1) })),
    );
    write(conn, ctx, AuditAction::LoginLocked, attempt, v).await
}

fn session_fields(s: &EndedSession) -> serde_json::Map<String, Value> {
    fields(json!({
        "userId": s.user_id,
        "username": s.username,
        "session": {
            "createdAt": s.created_at,
            "ipAddress": s.ip_address.map(|n| n.ip()),
            "userAgent": s.user_agent,
        },
    }))
}

/// The user signed out of this session.
pub async fn logout(conn: &mut PgConnection, ctx: &RequestContext, session: &EndedSession) -> sqlx::Result<()> {
    let v = details(ctx, session_fields(session));
    write(conn, ctx, AuditAction::Logout, session.id, v).await
}

/// The API ended these sessions; the actor is whoever caused it.
pub async fn revoked(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    sessions: &[EndedSession],
    reason: RevokeReason,
) -> sqlx::Result<()> {
    for s in sessions {
        let mut f = session_fields(s);
        f.insert("reason".into(), json!(reason.as_str()));
        write(conn, ctx, AuditAction::SessionRevoke, s.id, details(ctx, f)).await?;
    }
    Ok(())
}

/// The caller's session `old` was replaced by `new_session_id` (reason
/// `rotated`, with `replacedBy`).
pub async fn rotated(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    old: &EndedSession,
    new_session_id: Uuid,
) -> sqlx::Result<()> {
    let mut f = session_fields(old);
    f.insert("reason".into(), json!(RevokeReason::Rotated.as_str()));
    f.insert("replacedBy".into(), json!(new_session_id));
    write(conn, ctx, AuditAction::SessionRevoke, old.id, details(ctx, f)).await
}

/// A request was made with this API token; `refusal` is why it was turned away, if it was.
/// `unrecorded`: refused uses like this one left out since the last row (see
/// [`super::token`]).
pub async fn token_use(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    token: &PresentedToken,
    refusal: Option<Refusal>,
    used: &Use<'_>,
    unrecorded: u64,
) -> sqlx::Result<()> {
    let mut f = fields(json!({
            "tokenName": token.name,
            "tokenPrefix": token.token_prefix,
            "userId": token.user_id,
            "username": token.username,
            "outcome": refusal.map_or("accepted", Refusal::outcome),
            "method": used.method.as_str(),
            "path": bounded_path(used.path),
            "operationId": used.operation_id,
    }));
    if used.path.chars().nth(TOKEN_PATH_MAX).is_some() {
        f.insert("pathLength".into(), json!(used.path.chars().count()));
    }
    if unrecorded > 0 {
        f.insert("unrecordedRefusals".into(), json!(unrecorded));
    }
    write_for(conn, ctx, AuditAction::TokenUse, TOKEN_ENTITY, token.id, details(ctx, f)).await
}

/// `unrecorded` uses of this dead token, refused with `outcome` between
/// `start` and `end`, that no request row counted (GH#213): no request, so no
/// method, path or client.
pub async fn token_refusals(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    token: &PresentedToken,
    outcome: &str,
    unrecorded: u64,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> sqlx::Result<()> {
    let v = json!({
        "tokenName": token.name,
        "tokenPrefix": token.token_prefix,
        "userId": token.user_id,
        "username": token.username,
        "outcome": outcome,
        "unrecordedRefusals": unrecorded,
        "windowStart": start,
        "windowEnd": end,
    });
    write_for(conn, ctx, AuditAction::TokenUse, TOKEN_ENTITY, token.id, v).await
}

/// The session's owner confirmed their credentials again (GH#498).
pub async fn reauthenticated(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    session_id: Uuid,
    user_id: Uuid,
    username: &str,
    method: &str,
) -> sqlx::Result<()> {
    let v = details(ctx, fields(json!({ "userId": user_id, "username": username, "method": method })));
    write(conn, ctx, AuditAction::SessionReauthenticate, session_id, v).await
}

/// A write that needs recently confirmed credentials was refused (GH#498).
pub async fn reauthentication_required(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    session_id: Uuid,
    user_id: Uuid,
    username: &str,
    used: &Use<'_>,
) -> sqlx::Result<()> {
    let v = details(
        ctx,
        fields(json!({
            "userId": user_id,
            "username": username,
            "method": used.method.as_str(),
            "path": bounded_path(used.path),
            "operationId": used.operation_id,
        })),
    );
    write(conn, ctx, AuditAction::SessionReauthenticationRequired, session_id, v).await
}

/// A two-factor event for this user (`mfa.*`): `extra` adds the event's own details.
pub async fn mfa(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    action: AuditAction,
    user_id: Uuid,
    username: &str,
    extra: Value,
) -> sqlx::Result<()> {
    let mut f = fields(json!({ "userId": user_id, "username": username }));
    f.extend(fields(extra));
    write_for(conn, ctx, action, MFA_ENTITY, user_id, details(ctx, f)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_use_paths_are_bounded() {
        assert_eq!(bounded_path("/api/v1/audit-log"), "/api/v1/audit-log");
        let exact = "a".repeat(TOKEN_PATH_MAX);
        assert_eq!(bounded_path(&exact), exact);
        let long = format!("/{}", "é".repeat(65_000));
        let kept = bounded_path(&long);
        assert_eq!(kept.chars().count(), TOKEN_PATH_MAX + 1);
        assert!(kept.ends_with("é…"), "cut on a character boundary, marked");
    }
}
