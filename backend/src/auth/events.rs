//! Authentication events in `audit_log`: sessions (entity type `sessions`)
//! and API token use (entity type `api_tokens`, the token's id).
//!
//! Each row carries the request's id, the client IP (and the TCP peer when it
//! differs) and user agent (see
//! [`crate::api::context::ClientInfo`]) and, in `new_value`, the event's
//! details. Never recorded: the password, the session token or its hash, the
//! CSRF token, an API token's secret or its hash.
//!
//! A failed sign-in stores the username as typed (truncated) and nothing about
//! whether it exists, so the audit log is not an enumeration oracle for those
//! who can read it. Answers refused while a username is locked (429) are not
//! recorded: they cost no password check, and recording them would let an
//! anonymous client grow the table at will. The lock itself is.

use std::time::Duration;

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

/// Longest attempted username kept (the real ones are at most 64 characters).
const ATTEMPTED_USERNAME_MAX: usize = 64;

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
}

impl RevokeReason {
    fn as_str(self) -> &'static str {
        match self {
            RevokeReason::UserDisabled => "user_disabled",
            RevokeReason::UserDeleted => "user_deleted",
            RevokeReason::PasswordReset => "password_reset",
            RevokeReason::PasswordChanged => "password_changed",
            RevokeReason::Replaced => "replaced",
        }
    }
}

/// How a session was opened.
#[derive(Debug, Clone, Copy)]
pub enum LoginMethod {
    Password,
    /// First-run setup signs the new administrator in.
    Setup,
}

fn attempted(username: &str) -> String {
    username.chars().take(ATTEMPTED_USERNAME_MAX).collect()
}

/// `{...details, ipAddress, userAgent}` of the request being handled, plus
/// `peerIpAddress` when the TCP peer differs from `ipAddress` (a proxy, or a
/// client that sent forwarded headers itself): the one address in the row the
/// client could not have made up.
fn details(ctx: &RequestContext, mut fields: serde_json::Map<String, Value>) -> Value {
    fields.insert("ipAddress".into(), json!(ctx.client.ip));
    if let Some(peer) = ctx.client.peer_ip.filter(|p| Some(*p) != ctx.client.ip) {
        fields.insert("peerIpAddress".into(), json!(peer));
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
    let method = match method {
        LoginMethod::Password => "password",
        LoginMethod::Setup => "setup",
    };
    let v = details(ctx, fields(json!({ "userId": user_id, "username": username, "method": method })));
    write(conn, ctx, AuditAction::LoginSuccess, session_id, v).await
}

/// A sign-in was refused. Returns the attempt's id, which is the row's entity
/// id (no session exists) and is shared with a `login.locked` row for the same attempt.
pub async fn login_failure(conn: &mut PgConnection, ctx: &RequestContext, username: &str) -> sqlx::Result<Uuid> {
    let attempt = Uuid::new_v4();
    let v = details(ctx, fields(json!({ "attemptedUsername": attempted(username) })));
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

/// A request was made with this API token; `refusal` is why it was turned away, if it was.
pub async fn token_use(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    token: &PresentedToken,
    refusal: Option<Refusal>,
    used: &Use<'_>,
) -> sqlx::Result<()> {
    let v = details(
        ctx,
        fields(json!({
            "tokenName": token.name,
            "tokenPrefix": token.token_prefix,
            "userId": token.user_id,
            "username": token.username,
            "outcome": refusal.map_or("accepted", Refusal::outcome),
            "method": used.method.as_str(),
            "path": used.path,
            "operationId": used.operation_id,
        })),
    );
    write_for(conn, ctx, AuditAction::TokenUse, TOKEN_ENTITY, token.id, v).await
}
