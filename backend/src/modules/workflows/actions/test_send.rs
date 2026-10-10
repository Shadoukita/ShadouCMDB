//! The designer's test send of a saved action (design SHAA-2725 §11.1,
//! SHAA-3042): one notification now, to the caller only, never through the
//! outbox and never to the action's recipients.
//!
//! - `email`: the action's message for the caller's language, marked as a
//!   test, to the caller's own address; about a CI only when one is given.
//! - `webhook`: one signed `ping` to the action's endpoint, with the URL
//!   rules and address checks of a delivery (as `POST
//!   /admin/webhook-endpoints/{id}/ping`, but for `workflows.manage`).
//! - `inbox`: one `workflow_action` notification to the caller, about the
//!   workflow (`entity_type` `workflow_definitions`, `data.test` true).
//!
//! Every send is audited as `workflow.action_test` on the workflow, and a
//! caller may send [`MAX_PER_MINUTE`] a minute (counted in the audit log, so
//! across every server process).

use std::time::Instant;

use lettre::message::Mailbox;
use serde::Serialize;
use serde_json::json;
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    ActionKeyPath, WorkflowActionKind, WorkflowActionPreviewQuery, WorkflowActionTrigger, email, load, sample_ci,
};
use crate::api::context::RequestContext;
use crate::data::auth as auth_data;
use crate::data::crud::{self, AuditAction, AuditEntry};
use crate::http::error::{AppError, ErrorCode};
use crate::modules::mail::render::{self, Locale};
use crate::modules::mail::{Mail, SendError};
use crate::modules::webhooks::service::{self as webhook_service, WebhookEndpointStatus};
use crate::modules::webhooks::{Webhooks, channel};
use crate::modules::workflows::service;

/// Test sends per caller and minute.
pub const MAX_PER_MINUTE: i64 = 10;

/// What a test send came to
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowActionTestResult {
    pub key: String,
    pub kind: WorkflowActionKind,
    /// It arrived: the relay accepted the e-mail (it may still bounce later), the receiver answered 2xx, or the
    /// inbox entry was written
    pub ok: bool,
    /// Where it went: the caller's e-mail address, the endpoint's key, or `inbox`
    pub to: String,
    /// The relay's SMTP reply code when it refused, or the receiver's HTTP status
    pub status_code: Option<u16>,
    /// Why it failed: `smtp_rejected` (5xx) or `smtp_deferred` (no connection, 4xx) for e-mail; for a webhook as
    /// a delivery would record it (`host_not_allowed`, `address_blocked:<ip>`, `redirect_not_followed`,
    /// `http_status`, `unreachable`, ...)
    pub reason: Option<String>,
    pub message: String,
    pub duration_ms: i64,
}

fn elapsed(started: Instant) -> i64 {
    i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX)
}

/// The workflow event kind (`workflow_instance_events.kind`) an action's trigger reports, as an inbox entry names it.
fn event_of(trigger: WorkflowActionTrigger) -> &'static str {
    match trigger {
        WorkflowActionTrigger::Transition => "transition",
        WorkflowActionTrigger::ApprovalRequested => "approval_request",
        WorkflowActionTrigger::ApprovalStep => "approval_decision",
        WorkflowActionTrigger::ApprovalClosed => "approval_close",
        WorkflowActionTrigger::ApprovalOverdue => "approval_overdue",
        WorkflowActionTrigger::InstanceCancelled => "cancel",
        WorkflowActionTrigger::InstanceForced => "force",
    }
}

pub async fn test(
    pool: &PgPool,
    ctx: &RequestContext,
    mail: &Mail,
    webhooks: &Webhooks,
    path: &ActionKeyPath,
    q: &WorkflowActionPreviewQuery,
) -> Result<WorkflowActionTestResult, AppError> {
    let principal = ctx.principal().ok_or_else(crate::api::context::unauthenticated)?;
    let user = principal.user_id;
    let mut conn = pool.acquire().await?;
    let d = service::load_for(&mut conn, ctx, path.0, false, service::Access::Read).await?;
    let action = load(&mut conn, d.id).await?.into_iter().find(|a| a.key == path.1).ok_or_else(|| {
        AppError::new(
            ErrorCode::NotFound,
            format!("Workflow {} has no saved action {}: save the actions, then send a test", d.key, path.1),
        )
    })?;
    let ci = match q.ci_id {
        Some(ci) => Some(sample_ci(&mut conn, ctx, ci).await?),
        None => None,
    };
    match action.kind {
        WorkflowActionKind::Email if !mail.enabled() => {
            return Err(AppError::new(
                ErrorCode::MailNotConfigured,
                "Outbound e-mail is off (MAIL=off); the operator sets MAIL=smtp and the SMTP_* variables",
            ));
        }
        WorkflowActionKind::Webhook => webhooks.require_enabled()?,
        _ => {}
    }
    let recent: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM cmdb.audit_log
         WHERE actor_id = $1 AND occurred_at > now() - interval '1 minute' AND action = 'workflow.action_test'",
    )
    .bind(user.to_string())
    .fetch_one(&mut *conn)
    .await?;
    if recent >= MAX_PER_MINUTE {
        let mut err = AppError::new(
            ErrorCode::RateLimited,
            format!("You sent {MAX_PER_MINUTE} tests in the last minute; wait a moment before the next one"),
        );
        err.retry_after = Some(60);
        return Err(err);
    }

    let started = Instant::now();
    let result = match action.kind {
        WorkflowActionKind::Email => {
            let (email, display_name, username, locale): (Option<String>, String, String, Option<String>) =
                sqlx::query_as("SELECT email, display_name, username, locale FROM cmdb.users WHERE id = $1")
                    .bind(user)
                    .fetch_one(&mut *conn)
                    .await?;
            let address = email
                .as_deref()
                .map(str::trim)
                .filter(|e| !e.is_empty())
                .ok_or_else(|| {
                    AppError::new(ErrorCode::Conflict, "Your account has no e-mail address to send a test message to")
                })?
                .to_owned();
            let mailbox = Mailbox::new(
                Some(display_name),
                address.parse().map_err(|_| {
                    AppError::new(ErrorCode::Conflict, "Your account's e-mail address cannot be used as a recipient")
                })?,
            );
            let permissions =
                auth_data::load_permissions_of(&mut conn, &[user]).await?.remove(&user).unwrap_or_default();
            let locale = Locale::of(locale.as_deref(), mail.cfg.default_locale);
            let rendered =
                email::test_message(&mut conn, mail, &d, &action, ci.as_ref(), (locale, &username, &permissions))
                    .await?;
            let message = render::message(mail, Uuid::new_v4(), mailbox, &rendered).map_err(|e| {
                tracing::error!(error = %e, "cannot build the workflow action test message");
                AppError::internal()
            })?;
            drop(conn);
            let outcome = mail.send(message).await;
            WorkflowActionTestResult {
                key: action.key.clone(),
                kind: action.kind,
                ok: outcome.is_ok(),
                status_code: outcome.as_ref().err().and_then(SendError::code),
                reason: outcome.as_ref().err().map(|e| match e {
                    SendError::Permanent { .. } => "smtp_rejected".into(),
                    SendError::Transient { .. } => "smtp_deferred".into(),
                }),
                message: match &outcome {
                    Ok(()) => format!("The relay accepted the test message to {address}"),
                    Err(e) => e.message().to_owned(),
                },
                to: address,
                duration_ms: elapsed(started),
            }
        }
        WorkflowActionKind::Webhook => {
            let endpoint = action.endpoint.as_ref().ok_or_else(|| {
                AppError::new(ErrorCode::Conflict, format!("Action {} has no webhook endpoint", action.key))
            })?;
            let e = webhook_service::row(&mut conn, endpoint.id, false).await?;
            match e.status {
                WebhookEndpointStatus::Active => {}
                WebhookEndpointStatus::Paused | WebhookEndpointStatus::Suspended => {
                    let status = if e.status == WebhookEndpointStatus::Paused { "paused" } else { "suspended" };
                    return Err(AppError::new(
                        ErrorCode::Conflict,
                        format!(
                            "The webhook endpoint {} is {status}; resume it under Administration > Webhooks, then \
                             send a test",
                            e.key
                        ),
                    ));
                }
            }
            drop(conn);
            let pong = channel::ping(pool, webhooks, e.id).await?;
            WorkflowActionTestResult {
                key: action.key.clone(),
                kind: action.kind,
                ok: pong.ok,
                to: e.key,
                status_code: pong.status_code,
                reason: pong.reason,
                message: pong.message,
                duration_ms: pong.duration_ms,
            }
        }
        WorkflowActionKind::Inbox => {
            let transition: Option<String> = match &action.transition {
                Some(key) => {
                    sqlx::query_scalar(
                        "SELECT t.name FROM cmdb.workflow_transitions t JOIN cmdb.workflow_definitions d
                       ON d.current_version_id = t.version_id WHERE d.id = $1 AND t.key = $2",
                    )
                    .bind(d.id)
                    .bind(key)
                    .fetch_optional(&mut *conn)
                    .await?
                }
                None => None,
            };
            let data = json!({
                "test": true, "definitionId": d.id, "definitionName": d.name,
                "actionKey": action.key, "actionName": action.name, "event": event_of(action.trigger),
                "transitionKey": action.transition, "transitionName": transition,
                "ciId": ci.as_ref().map(|c| c.id), "ciLabel": ci.as_ref().map(|c| &c.label),
                "ciIdent": ci.as_ref().and_then(|c| c.ident.as_ref()),
            });
            sqlx::query(
                "INSERT INTO cmdb.notifications (user_id, kind, entity_type, entity_id, ci_id, data, dedupe_key)
                 VALUES ($1, 'workflow_action', 'workflow_definitions', $2, $3, $4, $5)",
            )
            .bind(user)
            .bind(d.id)
            .bind(ci.as_ref().map(|c| c.id))
            .bind(data)
            .bind(format!("action_test:{}:{}", action.id, Uuid::new_v4()))
            .execute(&mut *conn)
            .await?;
            drop(conn);
            WorkflowActionTestResult {
                key: action.key.clone(),
                kind: action.kind,
                ok: true,
                to: "inbox".into(),
                status_code: None,
                reason: None,
                message: "A test notification is in your inbox".into(),
                duration_ms: elapsed(started),
            }
        }
    };
    let entry = AuditEntry {
        action: AuditAction::WorkflowActionTest,
        entity_type: "workflow_definitions",
        entity_id: d.id,
        old_value: None,
        new_value: Some(json!({
            "action": action.key, "kind": action.kind, "ciId": q.ci_id, "to": match action.kind {
                // The user, not their address: the audit log outlives an erased address.
                WorkflowActionKind::Email => json!(principal.username),
                _ => json!(result.to),
            },
            "ok": result.ok, "statusCode": result.status_code, "reason": result.reason,
        })),
    };
    let mut conn = pool.acquire().await?;
    crud::write_audit(&mut conn, ctx, vec![entry]).await?;
    Ok(result)
}
