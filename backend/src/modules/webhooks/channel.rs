//! The webhook channel of the workflow action outbox (design SHAA-2725 §4.4,
//! §5): one signed request per delivery, the URL rules and SSRF checks before
//! every attempt, and the circuit breaker per endpoint.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use super::Webhooks;
use super::client::{self, Attempt, Request};
use super::envelope;
use super::hosts::check_url;
use super::service::{self, EndpointRow, WebhookEndpointStatus, WebhookPingResult};
use super::signing;
use crate::api::context::RequestContext;
use crate::modules::workflows::actions::outbox::{Channel, Claimed, Outcome};

/// Failures in a row, with no success for [`BREAKER_QUIET`], that suspend an endpoint.
pub const BREAKER_FAILURES: i32 = 20;
pub const BREAKER_QUIET: Duration = Duration::from_secs(15 * 60);
/// The channel's own time: the longest endpoint timeout, plus DNS and the database reads around it.
const CHANNEL_TIMEOUT: Duration = Duration::from_secs(45);

const ACTOR: &str = "webhook circuit breaker";

/// The outbox channel for webhook deliveries.
pub fn channel(pool: PgPool, w: Arc<Webhooks>) -> Channel {
    Channel {
        send: Arc::new(move |c: Claimed| {
            let (pool, w) = (pool.clone(), w.clone());
            Box::pin(async move { deliver(&pool, &w, &c).await })
        }),
        timeout: CHANNEL_TIMEOUT,
    }
}

fn permanent(reason: &str, error: impl Into<String>) -> Outcome {
    Outcome::Permanent { status_code: None, reason: reason.into(), error: error.into() }
}

/// What an attempt comes to for the outbox.
fn outcome(a: Attempt) -> Outcome {
    match a {
        Attempt::Answered { status, .. } if (200..300).contains(&status) => {
            Outcome::Delivered { status_code: Some(i32::from(status)) }
        }
        Attempt::Answered { status, location, .. } if (300..400).contains(&status) => Outcome::Permanent {
            status_code: Some(i32::from(status)),
            reason: "redirect_not_followed".into(),
            error: format!(
                "HTTP {status}: redirect to {} not followed; register the final URL instead",
                location.as_deref().unwrap_or("(no Location)")
            ),
        },
        Attempt::Answered { status, retry_after, excerpt, .. } if matches!(status, 408 | 425 | 429 | 500..=599) => {
            Outcome::Transient { status_code: Some(i32::from(status)), error: answered(status, &excerpt), retry_after }
        }
        Attempt::Answered { status, excerpt, .. } => Outcome::Permanent {
            status_code: Some(i32::from(status)),
            reason: "http_status".into(),
            error: answered(status, &excerpt),
        },
        Attempt::Refused { reason, message } => permanent(&reason, message),
        Attempt::Failed { message } => Outcome::Transient { status_code: None, error: message, retry_after: None },
    }
}

fn answered(status: u16, excerpt: &str) -> String {
    if excerpt.trim().is_empty() { format!("HTTP {status}") } else { format!("HTTP {status}: {}", excerpt.trim()) }
}

/// Builds and sends delivery `c`, then records the endpoint's result.
async fn deliver(pool: &PgPool, w: &Webhooks, c: &Claimed) -> Outcome {
    if !w.cfg.allowed {
        return permanent("webhooks_disabled", "Webhooks are switched off on this server (WEBHOOKS_ALLOWED=false)");
    }
    let Some(endpoint_id) = c.endpoint_id else {
        return permanent("endpoint_deleted", "The webhook endpoint was deleted");
    };
    let mut conn = match pool.acquire().await {
        Ok(c) => c,
        Err(e) => return Outcome::Transient { status_code: None, error: format!("database: {e}"), retry_after: None },
    };
    let e = match service::row(&mut conn, endpoint_id, false).await {
        Ok(e) => e,
        Err(_) => return permanent("endpoint_deleted", "The webhook endpoint was deleted"),
    };
    if e.status != WebhookEndpointStatus::Active {
        return Outcome::Held {
            reason: format!(
                "endpoint_{}",
                if e.status == WebhookEndpointStatus::Paused { "paused" } else { "suspended" }
            ),
        };
    }
    let built = envelope::build(&mut conn, c.run_id, c.id, w.public_url.as_deref()).await;
    let list = service::allowlist(&mut conn).await;
    drop(conn);
    let (event, body) = match built {
        Ok(Some(b)) => b,
        Ok(None) => return permanent("run_gone", "The workflow event of this delivery no longer exists"),
        Err(err) => {
            return Outcome::Transient {
                status_code: None,
                error: format!("payload: {}", err.message),
                retry_after: None,
            };
        }
    };
    let list = match list {
        Ok(l) => l,
        Err(err) => return Outcome::Transient { status_code: None, error: err.message, retry_after: None },
    };
    let out = match check_url(&e.url, w.ceiling(), &list) {
        Err(refused) => permanent(refused.code, refused.message),
        Ok(url) => outcome(attempt(w, &e, &url, &event, &c.id.to_string(), &body).await),
    };
    if let Err(err) = breaker(pool, &e, &out).await {
        tracing::warn!(endpoint = %e.key, error = %err.message, "cannot record a webhook endpoint's result");
    }
    out
}

/// Signs `body` with the endpoint's secrets and sends it once.
async fn attempt(
    w: &Webhooks,
    e: &EndpointRow,
    url: &url::Url,
    event: &str,
    delivery: &str,
    body: &serde_json::Value,
) -> Attempt {
    let unreadable = |what: &str| Attempt::Refused {
        reason: "secret_unreadable".into(),
        message: format!(
            "The {what} of webhook endpoint {} does not decrypt (encrypted with a key that is not configured, or \
             altered); rotate the secret or set the header again",
            e.key
        ),
    };
    let Ok(secrets) = e.signing_secrets(w) else { return unreadable("signing secret") };
    let Ok(header) = e.auth_header(w) else { return unreadable("auth header") };
    let bytes = serde_json::to_vec(body).unwrap_or_default();
    let raw: Vec<&[u8]> = secrets.iter().map(|s| &s[..]).collect();
    let signature = signing::header(chrono::Utc::now().timestamp(), &bytes, &raw);
    let headers = match client::headers(
        event,
        delivery,
        &e.key,
        &signature,
        header.as_ref().map(|(n, v)| (n.as_str(), &v[..])),
    ) {
        Ok(h) => h,
        Err(m) => return Attempt::Refused { reason: "invalid_header".into(), message: m },
    };
    // Everything secret or taken from the CI, to keep out of the stored excerpt.
    let mut needles: Vec<String> = raw.iter().map(|s| signing::display(s)).collect();
    needles.extend(signature.split(',').filter_map(|p| p.strip_prefix("v1=")).map(str::to_owned));
    if let Some((_, v)) = &header {
        needles.push(String::from_utf8_lossy(&v[..]).into_owned());
    }
    payload_values(&body["ci"]["attributes"], &mut needles);
    let sent = w
        .send(Request { url, body: bytes.clone(), headers, timeout: Duration::from_millis(e.timeout_ms as u64) })
        .await;
    withhold_echo(sent, &bytes, &needles)
}

/// The text of every attribute value in the payload (4 characters or more,
/// so an excerpt like `"ok"` or `true` is kept).
fn payload_values(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::Object(m) => m.values().for_each(|v| payload_values(v, out)),
        serde_json::Value::Array(a) => a.iter().for_each(|v| payload_values(v, out)),
        serde_json::Value::String(s) if s.chars().count() >= 4 => out.push(s.clone()),
        serde_json::Value::Number(n) if n.to_string().len() >= 4 => out.push(n.to_string()),
        _ => {}
    }
}

/// The response excerpt is stored on the delivery (`last_error`) to diagnose a
/// failure. A receiver that echoes the request back would put the payload, or
/// a signature, there: such an excerpt is withheld, only the status is kept.
fn withhold_echo(a: Attempt, body: &[u8], needles: &[String]) -> Attempt {
    match a {
        Attempt::Answered { status, location, retry_after, excerpt } => {
            let body = String::from_utf8_lossy(body);
            let t = excerpt.trim();
            let echoes = (t.len() >= 16 && body.contains(t))
                || body.get(..64.min(body.len())).is_some_and(|head| t.contains(head))
                || needles.iter().any(|n| !n.is_empty() && t.contains(n.as_str()));
            let excerpt =
                if echoes { "(response body withheld: it repeats part of the request)".to_owned() } else { excerpt };
            Attempt::Answered { status, location, retry_after, excerpt }
        }
        other => other,
    }
}

/// The endpoint's success and failure counters; [`BREAKER_FAILURES`] failures
/// in a row with no success for [`BREAKER_QUIET`] suspend it (audited, and an
/// inbox notice to every `webhooks.manage` holder).
async fn breaker(pool: &PgPool, e: &EndpointRow, out: &Outcome) -> Result<(), crate::http::error::AppError> {
    let ok = matches!(out, Outcome::Delivered { .. });
    if matches!(out, Outcome::Held { .. }) {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    let (failures, quiet): (i32, bool) = sqlx::query_as(
        "UPDATE cmdb.webhook_endpoints SET
           consecutive_failures = CASE WHEN $2 THEN 0 ELSE consecutive_failures + 1 END,
           last_success_at = CASE WHEN $2 THEN now() ELSE last_success_at END,
           last_failure_at = CASE WHEN $2 THEN last_failure_at ELSE now() END
         WHERE id = $1
         RETURNING consecutive_failures,
                   last_success_at IS NULL OR last_success_at < now() - $3 * interval '1 second'",
    )
    .bind(e.id)
    .bind(ok)
    .bind(BREAKER_QUIET.as_secs() as f64)
    .fetch_one(&mut *tx)
    .await?;
    if !ok && failures >= BREAKER_FAILURES && quiet {
        let system = RequestContext::system(ACTOR, format!("webhook-breaker-{}", Uuid::new_v4()));
        if service::suspend(&mut tx, &system, e.id, "breaker").await? {
            notify_suspended(&mut tx, e, failures).await?;
            tracing::warn!(
                endpoint = %e.key,
                failures,
                "webhook endpoint suspended after {failures} failures in a row; resume it under Administration > \
                 Webhooks once the receiver works"
            );
        }
    }
    tx.commit().await?;
    Ok(())
}

/// The built-in inbox notice `webhook_suspended` to every active holder of `webhooks.manage`.
async fn notify_suspended(conn: &mut sqlx::PgConnection, e: &EndpointRow, failures: i32) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO cmdb.notifications (user_id, kind, entity_type, entity_id, ci_id, data, dedupe_key)
         SELECT DISTINCT u.id, 'webhook_suspended', 'webhook_endpoints', $1, NULL::uuid, $2, $3
         FROM cmdb.users u
         JOIN cmdb.user_permission_profiles up ON up.user_id = u.id
         JOIN cmdb.permission_profiles p ON p.id = up.profile_id
         WHERE u.is_active
           AND (p.is_builtin OR EXISTS (SELECT 1 FROM cmdb.permission_profile_global_permissions g
                                         WHERE g.profile_id = p.id AND g.permission = 'webhooks.manage'))
         ON CONFLICT (user_id, dedupe_key) DO NOTHING",
    )
    .bind(e.id)
    .bind(json!({ "endpointKey": e.key, "endpointName": e.name, "reason": "breaker", "consecutiveFailures": failures }))
    .bind(format!("webhook_suspended:{}:{}", e.id, chrono::Utc::now().timestamp_millis()))
    .execute(conn)
    .await?;
    Ok(())
}

/// The endpoint's test button: the URL rules, the SSRF checks and one signed `ping`, as a delivery would go.
pub async fn ping(pool: &PgPool, w: &Webhooks, id: Uuid) -> Result<WebhookPingResult, crate::http::error::AppError> {
    w.require_enabled()?;
    let mut conn = pool.acquire().await?;
    let e = service::row(&mut conn, id, false).await?;
    let list = service::allowlist(&mut conn).await?;
    drop(conn);
    let started = Instant::now();
    let delivery = Uuid::new_v4();
    let out = match check_url(&e.url, w.ceiling(), &list) {
        Err(refused) => permanent(refused.code, refused.message),
        Ok(url) => {
            outcome(attempt(w, &e, &url, "ping", &delivery.to_string(), &envelope::ping(delivery, &e.key)).await)
        }
    };
    let duration_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
    Ok(match out {
        Outcome::Delivered { status_code } => WebhookPingResult {
            ok: true,
            status_code: status_code.and_then(|s| u16::try_from(s).ok()),
            reason: None,
            message: "The receiver accepted the test request".into(),
            duration_ms,
        },
        Outcome::Transient { status_code, error, .. } => WebhookPingResult {
            ok: false,
            status_code: status_code.and_then(|s| u16::try_from(s).ok()),
            reason: Some(if status_code.is_some() { "http_status".into() } else { "unreachable".into() }),
            message: error,
            duration_ms,
        },
        Outcome::Permanent { status_code, reason, error } => WebhookPingResult {
            ok: false,
            status_code: status_code.and_then(|s| u16::try_from(s).ok()),
            reason: Some(reason),
            message: error,
            duration_ms,
        },
        Outcome::Held { reason } | Outcome::Skipped { reason } => WebhookPingResult {
            ok: false,
            status_code: None,
            reason: Some(reason.clone()),
            message: reason,
            duration_ms,
        },
    })
}
