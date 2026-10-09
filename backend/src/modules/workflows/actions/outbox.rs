//! The workers of the workflow action outbox (design SHAA-2725 §4.2-§4.6).
//!
//! The enqueue trigger writes runs in the event's transaction (ids only).
//! Every API process with `WORKFLOW_ACTIONS_WORKER=on` then works the queue;
//! no leader is elected, and any number of processes may share it:
//!
//! - **Fan-out.** A run is claimed (`pending` -> `fanning_out`, with a lease)
//!   with `FOR UPDATE SKIP LOCKED`, then fanned out in one short transaction
//!   that holds the run's row and checks the lease is still ours: recipients
//!   are resolved now, with the view check on the CI's class, and one
//!   delivery is written per recipient (`UNIQUE (run_id, recipient_key)`).
//!   The inbox channel writes its notifications in that same transaction, so
//!   a worker that dies mid-fan-out leaves nothing behind; once the lease
//!   ends the run is `pending` again and the next worker starts over.
//! - **Delivery.** Channels that talk to another system (e-mail, webhooks)
//!   send each delivery outside any transaction. A delivery is claimed with
//!   `attempts + 1` and a fencing `lease_epoch`; its outcome is written only
//!   while the epoch is still the one claimed, so a worker that stalled past
//!   its lease cannot overwrite its successor's result. A transient failure
//!   is retried with exponential backoff and jitter (honouring
//!   `Retry-After`), a permanent one is dead at once, and every dead letter
//!   is audited (`workflow.action_dead`, actor system).
//! - **Housekeeping**, every tick: leases that ran out are returned,
//!   deliveries past `WORKFLOW_ACTIONS_MAX_AGE_HOURS` die as `expired`, the
//!   queue's overload flag and the per-instance limit the enqueue trigger
//!   reads are refreshed, suppressed runs are audited (at most once a minute
//!   per definition and reason), and once an hour finished rows past their
//!   retention are deleted.
//!
//! Workers hold no CI, instance or request lock and never write CI data.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};
use uuid::Uuid;

use super::{WorkflowActionKind, WorkflowActionRecipient, WorkflowActionRecipientSource, WorkflowActionSettings};
use crate::api::context::RequestContext;
use crate::auth::permissions::ClassOp;
use crate::config::WorkflowActionsConfig;
use crate::data::auth as auth_data;
use crate::data::crud::{self, AuditAction, AuditEntry};

/// How long a fan-out may take before another worker may take the run over.
const FAN_OUT_LEASE: Duration = Duration::from_secs(60);
/// Runs claimed at once by one fan-out worker.
const RUN_BATCH: i64 = 10;
/// Added to a channel's send timeout for a delivery's lease (§4.2).
const LEASE_MARGIN: Duration = Duration::from_secs(30);
/// First retry delay and the most a delay grows to (§4.4).
const BACKOFF_BASE: Duration = Duration::from_secs(30);
const BACKOFF_CAP: Duration = Duration::from_secs(60 * 60);
/// Retention runs at most this often.
const RETENTION_EVERY: Duration = Duration::from_secs(60 * 60);
/// Rows deleted per retention statement.
const RETENTION_BATCH: i64 = 10_000;
/// `last_error` is capped by the table.
const MAX_ERROR: usize = 1024;

const ACTOR: &str = "workflow actions";

/// A recipient source of an action: the source and its profile, group or user.
type RecipientIds = (WorkflowActionRecipientSource, Option<Uuid>, Option<Uuid>, Option<Uuid>);
/// The definition, transition, from and to state names and the request number of an event.
type EventNames = (Option<String>, Option<String>, Option<String>, Option<String>, Option<i32>);
/// A dead delivery: id, run, action key, kind, CI, reason, attempts.
type DeadRow = (Uuid, i64, String, String, Uuid, Option<String>, i16);

// ---------------------------------------------------------------------------
// Channels
// ---------------------------------------------------------------------------

/// A delivery claimed for sending. The e-mail and webhook channels (S4, S5)
/// read what they need of it.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Claimed {
    pub id: Uuid,
    pub run_id: i64,
    pub kind: WorkflowActionKind,
    pub recipient_key: String,
    pub user_id: Option<Uuid>,
    pub endpoint_id: Option<Uuid>,
    /// This attempt's number, from 1.
    pub attempts: i16,
    pub epoch: i32,
    pub created_at: DateTime<Utc>,
}

/// What one attempt came to. The channels of S4 and S5 produce every variant.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Outcome {
    Delivered {
        status_code: Option<i32>,
    },
    /// Worth another attempt: a connection, TLS or DNS failure, a timeout, HTTP 408/425/429/5xx, SMTP 4xx.
    Transient {
        status_code: Option<i32>,
        error: String,
        retry_after: Option<Duration>,
    },
    /// Never worth another: dead at once, with `reason`.
    Permanent {
        status_code: Option<i32>,
        reason: String,
        error: String,
    },
    /// Not attempted after all (a webhook endpoint paused or suspended since
    /// the claim): held, uncounted, until the endpoint is resumed.
    Held {
        reason: String,
    },
}

/// Sends one delivery; never inside a database transaction.
pub type Send = Arc<dyn Fn(Claimed) -> BoxFuture<'static, Outcome> + std::marker::Send + Sync>;

/// A channel that sends deliveries to another system, with its send timeout.
#[derive(Clone)]
pub struct Channel {
    pub send: Send,
    pub timeout: Duration,
}

/// The sending channels of this process, by kind. The inbox needs none: its
/// fan-out delivers. E-mail (S4) and webhooks (S5) register theirs here.
#[derive(Clone, Default)]
pub struct Channels(HashMap<WorkflowActionKind, Channel>);

impl Channels {
    #[allow(dead_code)]
    pub fn with(mut self, kind: WorkflowActionKind, channel: Channel) -> Self {
        self.0.insert(kind, channel);
        self
    }

    fn kinds(&self) -> Vec<WorkflowActionKind> {
        let mut k: Vec<_> = self.0.keys().copied().collect();
        k.sort();
        k
    }
}

/// The delay before attempt `attempts + 1`: 30 s doubling per attempt, at most
/// an hour, times a jitter of 0.8 to 1.2; a `Retry-After` replaces it, also
/// capped at an hour.
pub fn backoff(attempts: i16, retry_after: Option<Duration>, jitter: f64) -> Duration {
    if let Some(after) = retry_after {
        return after.min(BACKOFF_CAP);
    }
    let exp = u32::try_from(attempts.max(1) - 1).unwrap_or(0).min(16);
    let base = BACKOFF_BASE.saturating_mul(2u32.saturating_pow(exp)).min(BACKOFF_CAP);
    base.mul_f64(jitter.clamp(0.8, 1.2))
}

fn jitter() -> f64 {
    let mut b = [0u8; 2];
    let _ = getrandom::fill(&mut b);
    0.8 + f64::from(u16::from_le_bytes(b)) / f64::from(u16::MAX) * 0.4
}

fn capped(error: &str) -> String {
    let mut e: String = error.chars().take(MAX_ERROR).collect();
    while e.len() > MAX_ERROR {
        e.pop();
    }
    e
}

// ---------------------------------------------------------------------------
// Recipients
// ---------------------------------------------------------------------------

/// The users the profile, group and named-user sources name, inactive ones
/// included, each with the sources that name them (`group CAB`).
pub async fn resolve_static(
    conn: &mut PgConnection,
    recipients: &[WorkflowActionRecipient],
) -> sqlx::Result<BTreeMap<Uuid, BTreeSet<String>>> {
    let ids = |source: WorkflowActionRecipientSource| -> Vec<Uuid> {
        recipients
            .iter()
            .filter(|r| r.source == source)
            .filter_map(|r| r.profile.as_ref().or(r.group.as_ref()).or(r.user.as_ref()).map(|p| p.id))
            .collect()
    };
    let members: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT profile_id, user_id FROM cmdb.user_permission_profiles WHERE profile_id = ANY($1)
         UNION ALL
         SELECT group_id, user_id FROM cmdb.user_group_members WHERE group_id = ANY($2)
         UNION ALL
         SELECT id, id FROM cmdb.users WHERE id = ANY($3)",
    )
    .bind(ids(WorkflowActionRecipientSource::Profile))
    .bind(ids(WorkflowActionRecipientSource::Group))
    .bind(ids(WorkflowActionRecipientSource::User))
    .fetch_all(&mut *conn)
    .await?;
    let labels: HashMap<Uuid, String> = recipients
        .iter()
        .filter_map(|r| {
            r.profile.as_ref().or(r.group.as_ref()).or(r.user.as_ref()).map(|p| (p.id, super::source_label(r)))
        })
        .collect();
    let mut out: BTreeMap<Uuid, BTreeSet<String>> = BTreeMap::new();
    for (source, user) in members {
        out.entry(user).or_default().insert(labels.get(&source).cloned().unwrap_or_default());
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Fan-out
// ---------------------------------------------------------------------------

/// Claims up to `n` pending runs for `owner`.
pub async fn claim_runs(pool: &PgPool, owner: &str, n: i64) -> sqlx::Result<Vec<i64>> {
    sqlx::query_scalar(
        "UPDATE cmdb.workflow_action_runs r
            SET status = 'fanning_out', lease_owner = $1, lease_until = now() + $2 * interval '1 millisecond'
          WHERE r.id IN (SELECT id FROM cmdb.workflow_action_runs WHERE status = 'pending'
                          ORDER BY created_at, id LIMIT $3 FOR UPDATE SKIP LOCKED)
          RETURNING r.id",
    )
    .bind(owner)
    .bind(i64::try_from(FAN_OUT_LEASE.as_millis()).unwrap_or(i64::MAX))
    .bind(n)
    .fetch_all(pool)
    .await
}

#[derive(sqlx::FromRow)]
struct RunRow {
    event_id: i64,
    action_id: Option<Uuid>,
    action_key: String,
    kind: WorkflowActionKind,
    instance_id: Uuid,
    ci_id: Uuid,
}

#[derive(sqlx::FromRow)]
struct EventRow {
    kind: String,
    transition_key: Option<String>,
    from_state_key: Option<String>,
    to_state_key: String,
    actor_type: String,
    actor_id: Option<String>,
    actor_name: Option<String>,
    approval_request_id: Option<Uuid>,
    approval_step_no: Option<i16>,
}

/// What a fan-out came to, for tests and logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanOut {
    /// Deliveries written; the run is `fanned_out`.
    Done,
    /// The run was `cancelled` (its action, CI or event is gone, or the kind cannot be sent here).
    Cancelled,
    /// Another worker holds the run now: nothing written.
    Lost,
}

/// Fans out run `id`, claimed by `owner`, in one transaction.
pub async fn fan_out(pool: &PgPool, cfg: &WorkflowActionsConfig, id: i64, owner: &str) -> sqlx::Result<FanOut> {
    let mut tx = pool.begin().await?;
    let run: Option<RunRow> = sqlx::query_as(
        "SELECT event_id, action_id, action_key, kind, instance_id, ci_id FROM cmdb.workflow_action_runs
         WHERE id = $1 AND status = 'fanning_out' AND lease_owner = $2 FOR UPDATE",
    )
    .bind(id)
    .bind(owner)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(run) = run else {
        return Ok(FanOut::Lost);
    };
    let cancel = |reason: &'static str| async move {
        sqlx::query(
            "UPDATE cmdb.workflow_action_runs SET status = 'cancelled', status_reason = $2, completed_at = now(),
               lease_owner = NULL, lease_until = NULL WHERE id = $1",
        )
        .bind(id)
        .bind(reason)
    };
    let action: Option<(bool, WorkflowActionKind, String, sqlx::types::Json<WorkflowActionSettings>)> =
        match run.action_id {
            Some(a) => {
                sqlx::query_as("SELECT enabled, kind, name, settings FROM cmdb.workflow_actions WHERE id = $1")
                    .bind(a)
                    .fetch_optional(&mut *tx)
                    .await?
            }
            None => None,
        };
    let event: Option<EventRow> = sqlx::query_as(
        "SELECT kind, transition_key, from_state_key, to_state_key, actor_type, actor_id, actor_name,
                approval_request_id, approval_step_no
         FROM cmdb.workflow_instance_events WHERE id = $1",
    )
    .bind(run.event_id)
    .fetch_optional(&mut *tx)
    .await?;
    let ci: Option<(Uuid, String, Option<String>)> =
        sqlx::query_as("SELECT class_id, label, ident FROM cmdb.configuration_items WHERE id = $1")
            .bind(run.ci_id)
            .fetch_optional(&mut *tx)
            .await?;
    let reason = match (&action, &event, &ci) {
        (None, ..) => Some("action_deleted"),
        (Some((false, ..)), ..) => Some("action_disabled"),
        (Some((_, kind, ..)), ..) if *kind != run.kind => Some("action_changed"),
        (_, None, _) => Some("event_gone"),
        (_, _, None) => Some("ci_deleted"),
        _ if run.kind == WorkflowActionKind::Email => Some("kind_unavailable"),
        _ => None,
    };
    if let Some(reason) = reason {
        cancel(reason).await.execute(&mut *tx).await?;
        tx.commit().await?;
        return Ok(FanOut::Cancelled);
    }
    let (Some((_, _, action_name, settings)), Some(event), Some((class_id, ci_label, ci_ident))) = (action, event, ci)
    else {
        return Ok(FanOut::Lost);
    };
    if run.kind == WorkflowActionKind::Webhook {
        return fan_out_webhook(tx, id, run.action_id.unwrap_or_default()).await;
    }

    // Recipients, now: by user id, the first WORKFLOW_ACTIONS_MAX_RECIPIENTS.
    let recipients = recipients_of(&mut tx, run.action_id.unwrap_or_default()).await?;
    let mut users: Vec<Uuid> = resolve_static(&mut tx, &recipients).await?.into_keys().collect();
    let truncated = users.len() > cfg.max_recipients;
    users.truncate(cfg.max_recipients);
    let active: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM cmdb.users WHERE id = ANY($1) AND is_active")
        .bind(&users)
        .fetch_all(&mut *tx)
        .await?;
    let permissions = auth_data::load_permissions_of(&mut tx, &active).await?;
    let actor = (event.actor_type == "user" || event.actor_type == "api_client")
        .then(|| event.actor_id.as_deref().and_then(|a| a.parse::<Uuid>().ok()))
        .flatten()
        .filter(|_| settings.exclude_actor.unwrap_or(true));
    // The users the built-in notifications of 0072 already told of this event.
    let builtin: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT user_id FROM cmdb.notifications WHERE user_id = ANY($1) AND dedupe_key = ANY($2)",
    )
    .bind(&users)
    .bind(builtin_keys(run.event_id, &event))
    .fetch_all(&mut *tx)
    .await?;

    let mut keys = Vec::with_capacity(users.len());
    let mut statuses = Vec::with_capacity(users.len());
    let mut reasons: Vec<Option<&str>> = Vec::with_capacity(users.len());
    let mut notify = Vec::new();
    for u in &users {
        let reason = if Some(*u) == actor {
            Some("actor")
        } else if !active.contains(u) {
            Some("inactive")
        } else if !permissions.get(u).is_some_and(|p| p.can(class_id, ClassOp::View)) {
            Some("no_view")
        } else if builtin.contains(u) {
            Some("builtin_notified")
        } else {
            None
        };
        keys.push(format!("user:{u}"));
        statuses.push(if reason.is_some() { "skipped" } else { "delivered" });
        reasons.push(reason);
        if reason.is_none() {
            notify.push(*u);
        }
    }
    sqlx::query(
        "INSERT INTO cmdb.workflow_action_deliveries
           (run_id, recipient_key, user_id, status, status_reason, attempts, completed_at)
         SELECT $1, u.key, u.user_id, u.status, u.reason, CASE WHEN u.status = 'delivered' THEN 1 ELSE 0 END, now()
         FROM unnest($2::text[], $3::uuid[], $4::text[], $5::text[]) AS u(key, user_id, status, reason)
         ON CONFLICT (run_id, recipient_key) DO NOTHING",
    )
    .bind(id)
    .bind(&keys)
    .bind(&users)
    .bind(&statuses)
    .bind(&reasons)
    .execute(&mut *tx)
    .await?;

    let approval = event.approval_request_id;
    let data = inbox_data(&mut tx, &run, &event, &action_name, &ci_label, ci_ident.as_deref()).await?;
    sqlx::query(
        "INSERT INTO cmdb.notifications (user_id, kind, entity_type, entity_id, ci_id, data, dedupe_key)
         SELECT u, 'workflow_action', $2, $3, $4, $5, $6 FROM unnest($1::uuid[]) AS u
         ON CONFLICT (user_id, dedupe_key) DO NOTHING",
    )
    .bind(&notify)
    .bind(if approval.is_some() { "workflow_approval_requests" } else { "workflow_instances" })
    .bind(approval.unwrap_or(run.instance_id))
    .bind(run.ci_id)
    .bind(data)
    .bind(format!("action:{id}"))
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE cmdb.workflow_action_runs SET status = 'fanned_out', status_reason = $2, completed_at = now(),
           lease_owner = NULL, lease_until = NULL WHERE id = $1",
    )
    .bind(id)
    .bind(truncated.then_some("truncated"))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(FanOut::Done)
}

/// A webhook run: one delivery to the action's endpoint, held while the
/// endpoint is paused or suspended. No recipient, no view check: the receiver
/// gets what the action lists, and the payload is built when it is sent.
async fn fan_out_webhook(mut tx: sqlx::Transaction<'_, sqlx::Postgres>, id: i64, action: Uuid) -> sqlx::Result<FanOut> {
    let endpoint: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT e.id, e.status FROM cmdb.workflow_actions a JOIN cmdb.webhook_endpoints e ON e.id = a.endpoint_id
         WHERE a.id = $1",
    )
    .bind(action)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((endpoint, status)) = endpoint else {
        sqlx::query(
            "UPDATE cmdb.workflow_action_runs SET status = 'cancelled', status_reason = 'endpoint_deleted',
               completed_at = now(), lease_owner = NULL, lease_until = NULL WHERE id = $1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(FanOut::Cancelled);
    };
    let held = status != "active";
    sqlx::query(
        "INSERT INTO cmdb.workflow_action_deliveries (run_id, recipient_key, endpoint_id, status, status_reason)
         VALUES ($1, $2, $3, $4, $5) ON CONFLICT (run_id, recipient_key) DO NOTHING",
    )
    .bind(id)
    .bind(format!("endpoint:{endpoint}"))
    .bind(endpoint)
    .bind(if held { "held" } else { "pending" })
    .bind(held.then(|| format!("endpoint_{status}")))
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE cmdb.workflow_action_runs SET status = 'fanned_out', completed_at = now(),
           lease_owner = NULL, lease_until = NULL WHERE id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(FanOut::Done)
}

async fn recipients_of(conn: &mut PgConnection, action: Uuid) -> sqlx::Result<Vec<WorkflowActionRecipient>> {
    let rows: Vec<RecipientIds> = sqlx::query_as(
        "SELECT source, profile_id, group_id, user_id FROM cmdb.workflow_action_recipients WHERE action_id = $1
         ORDER BY position",
    )
    .bind(action)
    .fetch_all(&mut *conn)
    .await?;
    let named = |id: Option<Uuid>| id.map(|id| super::WorkflowPrincipalRef { id, name: String::new() });
    Ok(rows
        .into_iter()
        .map(|(source, profile, group, user)| WorkflowActionRecipient {
            source,
            profile: named(profile),
            group: named(group),
            user: named(user),
            attribute: None,
            service_owner_role: None,
            participant: None,
            address: None,
        })
        .collect())
}

/// The dedupe keys the 0072 triggers give their notifications about this event.
fn builtin_keys(event_id: i64, e: &EventRow) -> Vec<String> {
    let mut keys = vec![format!("workflow_event:{event_id}")];
    if let Some(r) = e.approval_request_id {
        keys.push(format!("approval_closed:{r}"));
        let step = match e.kind.as_str() {
            "approval_decision" => e.approval_step_no.map_or(1, |s| s + 1),
            _ => e.approval_step_no.unwrap_or(1),
        };
        keys.push(format!("approval_requested:{r}:{step}"));
    }
    keys
}

/// The inbox entry's display values, as the 0072 notifications carry them.
async fn inbox_data(
    conn: &mut PgConnection,
    run: &RunRow,
    e: &EventRow,
    action_name: &str,
    ci_label: &str,
    ci_ident: Option<&str>,
) -> sqlx::Result<Value> {
    let names: EventNames = sqlx::query_as(
        "SELECT d.name, tr.name, fs.name, ts.name, r.request_no
         FROM cmdb.workflow_instances i
         JOIN cmdb.workflow_definitions d ON d.id = i.definition_id
         LEFT JOIN cmdb.workflow_approval_requests r ON r.id = $3
         LEFT JOIN cmdb.workflow_transitions tr ON tr.version_id = coalesce(r.version_id, i.version_id)
                                               AND tr.key = coalesce($2, r.transition_key)
         LEFT JOIN cmdb.workflow_states fs ON fs.version_id = i.version_id AND fs.key = $4
         LEFT JOIN cmdb.workflow_states ts ON ts.version_id = i.version_id AND ts.key = $5
         WHERE i.id = $1",
    )
    .bind(run.instance_id)
    .bind(&e.transition_key)
    .bind(e.approval_request_id)
    .bind(&e.from_state_key)
    .bind(&e.to_state_key)
    .fetch_optional(&mut *conn)
    .await?
    .unwrap_or_default();
    let (definition_name, transition_name, from_name, to_name, request_no) = names;
    Ok(json!({
        "instanceId": run.instance_id, "ciId": run.ci_id, "ciLabel": ci_label, "ciIdent": ci_ident,
        "definitionName": definition_name, "actionKey": run.action_key, "actionName": action_name,
        "event": e.kind, "transitionKey": e.transition_key, "transitionName": transition_name,
        "fromStateKey": e.from_state_key, "fromStateName": from_name,
        "toStateKey": e.to_state_key, "toStateName": to_name, "actorName": e.actor_name,
        "approvalRequestId": e.approval_request_id, "requestNo": request_no,
    }))
}

// ---------------------------------------------------------------------------
// Delivery
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct ClaimedRow {
    id: Uuid,
    run_id: i64,
    kind: WorkflowActionKind,
    recipient_key: String,
    user_id: Option<Uuid>,
    endpoint_id: Option<Uuid>,
    attempts: i16,
    lease_epoch: i32,
    created_at: DateTime<Utc>,
}

/// Claims up to `n` deliveries of `kind` that are due, for `owner`, leased
/// for the channel's timeout plus a margin.
pub async fn claim_deliveries(
    pool: &PgPool,
    owner: &str,
    kind: WorkflowActionKind,
    timeout: Duration,
    n: i64,
) -> sqlx::Result<Vec<Claimed>> {
    if kind == WorkflowActionKind::Webhook {
        return claim_webhook_deliveries(pool, owner, timeout, n).await;
    }
    let rows: Vec<ClaimedRow> = sqlx::query_as(
        "UPDATE cmdb.workflow_action_deliveries d
            SET status = 'sending', attempts = d.attempts + 1, lease_owner = $1,
                lease_until = now() + $2 * interval '1 millisecond', lease_epoch = d.lease_epoch + 1
           FROM cmdb.workflow_action_runs r
          WHERE r.id = d.run_id
            AND d.id IN (SELECT x.id FROM cmdb.workflow_action_deliveries x
                           JOIN cmdb.workflow_action_runs xr ON xr.id = x.run_id
                          WHERE x.status = 'pending' AND x.next_attempt_at <= now() AND xr.kind = $3
                          ORDER BY x.next_attempt_at, x.id LIMIT $4 FOR UPDATE OF x SKIP LOCKED)
          RETURNING d.id, d.run_id, r.kind, d.recipient_key, d.user_id, d.endpoint_id, d.attempts, d.lease_epoch,
                    d.created_at",
    )
    .bind(owner)
    .bind(i64::try_from((timeout + LEASE_MARGIN).as_millis()).unwrap_or(i64::MAX))
    .bind(kind)
    .bind(n)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Claimed {
            id: r.id,
            run_id: r.run_id,
            kind: r.kind,
            recipient_key: r.recipient_key,
            user_id: r.user_id,
            endpoint_id: r.endpoint_id,
            attempts: r.attempts,
            epoch: r.lease_epoch,
            created_at: r.created_at,
        })
        .collect())
}

/// [`claim_deliveries`] for webhooks: only for active endpoints, and per
/// endpoint at most `max_in_flight` sending at once and `max_per_minute`
/// claimed per minute (design §4.5, §4.6). An endpoint's row is locked while
/// its deliveries are claimed (`SKIP LOCKED`: another process takes another
/// endpoint), so both limits hold across processes; the minute's count lives
/// in `workflow_action_rate_windows`. A delivery still pending for an endpoint
/// that is no longer active is held first.
async fn claim_webhook_deliveries(pool: &PgPool, owner: &str, timeout: Duration, n: i64) -> sqlx::Result<Vec<Claimed>> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "UPDATE cmdb.workflow_action_deliveries d SET status = 'held',
           status_reason = 'endpoint_' || e.status
         FROM cmdb.webhook_endpoints e
         WHERE e.id = d.endpoint_id AND e.status <> 'active' AND d.status = 'pending'",
    )
    .execute(&mut *tx)
    .await?;
    let rows: Vec<ClaimedRow> = sqlx::query_as(
        "WITH ep AS (
           SELECT e.id,
                  least(e.max_in_flight - (SELECT count(*) FROM cmdb.workflow_action_deliveries s
                                            WHERE s.endpoint_id = e.id AND s.status = 'sending'),
                        e.max_per_minute - coalesce((SELECT w.count FROM cmdb.workflow_action_rate_windows w
                                                     WHERE w.scope = 'endpoint:' || e.id
                                                       AND w.window_start = date_trunc('minute', now())), 0)) AS slots
           FROM cmdb.webhook_endpoints e
           WHERE e.status = 'active'
             AND EXISTS (SELECT 1 FROM cmdb.workflow_action_deliveries p
                          WHERE p.endpoint_id = e.id AND p.status = 'pending' AND p.next_attempt_at <= now())
           FOR UPDATE OF e SKIP LOCKED),
         pick AS (
           SELECT p.id, p.next_attempt_at FROM ep CROSS JOIN LATERAL (
             SELECT d.id, d.next_attempt_at FROM cmdb.workflow_action_deliveries d
              WHERE d.endpoint_id = ep.id AND d.status = 'pending' AND d.next_attempt_at <= now()
              ORDER BY d.next_attempt_at, d.id LIMIT greatest(ep.slots, 0)) p
           ORDER BY p.next_attempt_at, p.id LIMIT $3),
         claimed AS (
           UPDATE cmdb.workflow_action_deliveries d
              SET status = 'sending', attempts = d.attempts + 1, lease_owner = $1,
                  lease_until = now() + $2 * interval '1 millisecond', lease_epoch = d.lease_epoch + 1
             FROM cmdb.workflow_action_runs r
            WHERE r.id = d.run_id AND d.id IN (SELECT id FROM pick) AND d.status = 'pending'
            RETURNING d.id, d.run_id, r.kind, d.recipient_key, d.user_id, d.endpoint_id, d.attempts, d.lease_epoch,
                      d.created_at),
         counted AS (
           INSERT INTO cmdb.workflow_action_rate_windows (scope, window_start, count)
           SELECT 'endpoint:' || endpoint_id, date_trunc('minute', now()), count(*) FROM claimed GROUP BY endpoint_id
           ON CONFLICT (scope, window_start)
             DO UPDATE SET count = cmdb.workflow_action_rate_windows.count + EXCLUDED.count)
         SELECT * FROM claimed",
    )
    .bind(owner)
    .bind(i64::try_from((timeout + LEASE_MARGIN).as_millis()).unwrap_or(i64::MAX))
    .bind(n)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows.into_iter().map(ClaimedRow::claimed).collect())
}

impl ClaimedRow {
    fn claimed(self) -> Claimed {
        Claimed {
            id: self.id,
            run_id: self.run_id,
            kind: self.kind,
            recipient_key: self.recipient_key,
            user_id: self.user_id,
            endpoint_id: self.endpoint_id,
            attempts: self.attempts,
            epoch: self.lease_epoch,
            created_at: self.created_at,
        }
    }
}

/// Writes the outcome of attempt `c`, if its lease is still the current one.
/// Returns false when it was not (another worker took the delivery over).
pub async fn record(pool: &PgPool, cfg: &WorkflowActionsConfig, c: &Claimed, outcome: Outcome) -> sqlx::Result<bool> {
    let mut tx = pool.begin().await?;
    let written = match outcome {
        Outcome::Delivered { status_code } => sqlx::query(
            "UPDATE cmdb.workflow_action_deliveries SET status = 'delivered', status_reason = NULL, completed_at = now(),
               last_status_code = $3, last_error = NULL, lease_owner = NULL, lease_until = NULL
             WHERE id = $1 AND lease_epoch = $2 AND status = 'sending'",
        )
        .bind(c.id)
        .bind(c.epoch)
        .bind(status_code)
        .execute(&mut *tx)
        .await?
        .rows_affected(),
        Outcome::Transient { status_code, error, retry_after } if c.attempts < cfg.max_attempts => {
            let delay = backoff(c.attempts, retry_after, jitter());
            sqlx::query(
                "UPDATE cmdb.workflow_action_deliveries SET status = 'pending',
                   next_attempt_at = now() + $3 * interval '1 millisecond', last_status_code = $4, last_error = $5,
                   lease_owner = NULL, lease_until = NULL
                 WHERE id = $1 AND lease_epoch = $2 AND status = 'sending'",
            )
            .bind(c.id)
            .bind(c.epoch)
            .bind(i64::try_from(delay.as_millis()).unwrap_or(i64::MAX))
            .bind(status_code)
            .bind(capped(&error))
            .execute(&mut *tx)
            .await?
            .rows_affected()
        }
        Outcome::Transient { status_code, error, .. } => {
            dead(&mut tx, c, "max_attempts", status_code, &error).await?
        }
        Outcome::Permanent { status_code, reason, error } => dead(&mut tx, c, &reason, status_code, &error).await?,
        Outcome::Held { reason } => sqlx::query(
            "UPDATE cmdb.workflow_action_deliveries SET status = 'held', status_reason = $3,
               attempts = greatest(attempts - 1, 0), lease_owner = NULL, lease_until = NULL
             WHERE id = $1 AND lease_epoch = $2 AND status = 'sending'",
        )
        .bind(c.id)
        .bind(c.epoch)
        .bind(reason)
        .execute(&mut *tx)
        .await?
        .rows_affected(),
    };
    tx.commit().await?;
    Ok(written == 1)
}

async fn dead(
    tx: &mut PgConnection,
    c: &Claimed,
    reason: &str,
    status_code: Option<i32>,
    error: &str,
) -> sqlx::Result<u64> {
    let n = sqlx::query(
        "UPDATE cmdb.workflow_action_deliveries SET status = 'dead', status_reason = $3, completed_at = now(),
           last_status_code = $4, last_error = $5, lease_owner = NULL, lease_until = NULL
         WHERE id = $1 AND lease_epoch = $2 AND status = 'sending'",
    )
    .bind(c.id)
    .bind(c.epoch)
    .bind(reason)
    .bind(status_code)
    .bind(capped(error))
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if n == 1 {
        audit_dead(&mut *tx, &[c.id]).await?;
    }
    Ok(n)
}

/// One `workflow.action_dead` row (actor system) per delivery that just died;
/// no recipient address, no error text.
pub(crate) async fn audit_dead(tx: &mut PgConnection, ids: &[Uuid]) -> sqlx::Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let rows: Vec<DeadRow> = sqlx::query_as(
        "SELECT d.id, r.id, r.action_key, r.kind, r.ci_id, d.status_reason, d.attempts
         FROM cmdb.workflow_action_deliveries d JOIN cmdb.workflow_action_runs r ON r.id = d.run_id
         WHERE d.id = ANY($1) ORDER BY d.id",
    )
    .bind(ids)
    .fetch_all(&mut *tx)
    .await?;
    let entries = rows
        .into_iter()
        .map(|(id, run, action_key, kind, ci, reason, attempts)| AuditEntry {
            action: AuditAction::WorkflowActionDead,
            entity_type: "workflow_action_deliveries",
            entity_id: id,
            old_value: None,
            new_value: Some(json!({ "runId": run, "actionKey": action_key, "kind": kind, "ciId": ci,
                "reason": reason, "attempts": attempts })),
        })
        .collect();
    crud::write_audit(tx, &system(), entries).await
}

fn system() -> RequestContext {
    RequestContext::system(ACTOR, format!("workflow-actions-{}", Uuid::new_v4()))
}

// ---------------------------------------------------------------------------
// Housekeeping
// ---------------------------------------------------------------------------

/// What one housekeeping pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Housekeeping {
    pub runs_released: u64,
    pub deliveries_released: u64,
    pub expired: u64,
    pub backlog: i64,
    pub overloaded: bool,
    pub suppressed_audited: u64,
}

/// One pass: leases, max age, the queue flag and suppression audits.
pub async fn housekeeping(pool: &PgPool, cfg: &WorkflowActionsConfig) -> sqlx::Result<Housekeeping> {
    let mut out = Housekeeping {
        runs_released: sqlx::query(
            "UPDATE cmdb.workflow_action_runs SET status = 'pending', lease_owner = NULL, lease_until = NULL
             WHERE status = 'fanning_out' AND lease_until < now()",
        )
        .execute(pool)
        .await?
        .rows_affected(),
        ..Housekeeping::default()
    };

    // A sending lease that ran out: the attempt counted. Back to pending, or
    // dead when it was the last one.
    let mut tx = pool.begin().await?;
    out.deliveries_released = sqlx::query(
        "UPDATE cmdb.workflow_action_deliveries SET status = 'pending', next_attempt_at = now(), lease_owner = NULL,
           lease_until = NULL, last_error = 'The attempt did not finish within its lease'
         WHERE status = 'sending' AND lease_until < now() AND attempts < $1",
    )
    .bind(cfg.max_attempts)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let died: Vec<Uuid> = sqlx::query_scalar(
        "UPDATE cmdb.workflow_action_deliveries SET status = 'dead', status_reason = 'max_attempts',
           completed_at = now(), lease_owner = NULL, lease_until = NULL,
           last_error = 'The attempt did not finish within its lease'
         WHERE status = 'sending' AND lease_until < now() RETURNING id",
    )
    .fetch_all(&mut *tx)
    .await?;
    // Never sent late without notice; a manual retry starts the clock again (0076).
    let expired: Vec<Uuid> = sqlx::query_scalar(
        "UPDATE cmdb.workflow_action_deliveries
            SET status = 'dead', completed_at = now(),
                status_reason = CASE WHEN status = 'held' THEN 'endpoint_suspended' ELSE 'expired' END
         WHERE id IN (SELECT id FROM cmdb.workflow_action_deliveries
                       WHERE status IN ('pending', 'held')
                         AND coalesce(retried_at, created_at) < now() - $1 * interval '1 hour'
                       LIMIT 1000 FOR UPDATE SKIP LOCKED)
         RETURNING id",
    )
    .bind(cfg.max_age_hours)
    .fetch_all(&mut *tx)
    .await?;
    out.expired = expired.len() as u64;
    audit_dead(&mut tx, &[died, expired].concat()).await?;
    tx.commit().await?;

    // The queue flag and the per-instance limit the enqueue trigger reads.
    let (runs, deliveries): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM (SELECT 1 FROM cmdb.workflow_action_runs WHERE status = 'pending' LIMIT $1) r),
                (SELECT count(*) FROM (SELECT 1 FROM cmdb.workflow_action_deliveries
                                        WHERE status IN ('pending', 'held') LIMIT $1) d)",
    )
    .bind(cfg.queue_max)
    .fetch_one(pool)
    .await?;
    out.backlog = runs + deliveries;
    out.overloaded = out.backlog >= cfg.queue_max;
    sqlx::query(
        "UPDATE cmdb.workflow_action_queue_state SET overloaded = $1, backlog = least($2, 2147483647)::int,
           max_per_instance_per_hour = $3, checked_at = now()",
    )
    .bind(out.overloaded)
    .bind(out.backlog)
    .bind(cfg.max_per_instance_per_hour)
    .execute(pool)
    .await?;
    if out.overloaded {
        tracing::warn!(
            backlog = out.backlog,
            queue_max = cfg.queue_max,
            "the workflow action queue is full: new runs are suppressed (WORKFLOW_ACTIONS_QUEUE_MAX)"
        );
    }

    out.suppressed_audited = audit_suppressed(pool).await?;
    Ok(out)
}

/// Audits the suppressed runs not audited yet: one `workflow.action_suppressed`
/// row per definition and reason, at most once a minute each (a window row
/// per minute, shared by every process); what a minute already audited waits
/// for the next one. Returns the runs audited.
pub async fn audit_suppressed(pool: &PgPool) -> sqlx::Result<u64> {
    let groups: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT DISTINCT definition_id, status_reason FROM cmdb.workflow_action_runs
         WHERE status = 'suppressed' AND completed_at IS NULL",
    )
    .fetch_all(pool)
    .await?;
    let mut total = 0;
    for (definition, reason) in groups {
        let mut tx = pool.begin().await?;
        let window: Option<DateTime<Utc>> = sqlx::query_scalar(
            "INSERT INTO cmdb.workflow_action_rate_windows (scope, window_start, count)
             VALUES ($1, date_trunc('minute', now()), 0) ON CONFLICT DO NOTHING RETURNING window_start",
        )
        .bind(format!("suppressed:{definition}:{reason}"))
        .fetch_optional(&mut *tx)
        .await?;
        let Some(window) = window else { continue };
        let (count, first, last): (i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>) = sqlx::query_as(
            "WITH done AS (
               UPDATE cmdb.workflow_action_runs SET completed_at = now()
               WHERE definition_id = $1 AND status = 'suppressed' AND status_reason = $2 AND completed_at IS NULL
               RETURNING created_at)
             SELECT count(*), min(created_at), max(created_at) FROM done",
        )
        .bind(definition)
        .bind(&reason)
        .fetch_one(&mut *tx)
        .await?;
        if count == 0 {
            continue;
        }
        sqlx::query("UPDATE cmdb.workflow_action_rate_windows SET count = $3 WHERE scope = $1 AND window_start = $2")
            .bind(format!("suppressed:{definition}:{reason}"))
            .bind(window)
            .bind(i32::try_from(count).unwrap_or(i32::MAX))
            .execute(&mut *tx)
            .await?;
        let key: Option<String> = sqlx::query_scalar("SELECT key FROM cmdb.workflow_definitions WHERE id = $1")
            .bind(definition)
            .fetch_optional(&mut *tx)
            .await?;
        let entry = AuditEntry {
            action: AuditAction::WorkflowActionSuppressed,
            entity_type: "workflow_definitions",
            entity_id: definition,
            old_value: None,
            new_value: Some(json!({ "definitionKey": key, "reason": reason, "count": count,
                "window": { "from": first, "to": last } })),
        };
        crud::write_audit(&mut tx, &system(), vec![entry]).await?;
        tx.commit().await?;
        tracing::warn!(definition = %definition, reason, count, "workflow action runs suppressed");
        total += count as u64;
    }
    Ok(total)
}

/// What one retention pass deleted.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
    pub deliveries: u64,
    pub dead: u64,
    pub runs: u64,
}

/// Deletes delivered and skipped deliveries and finished runs past
/// `WORKFLOW_ACTIONS_RETENTION_DAYS`, dead ones past
/// `WORKFLOW_ACTIONS_DEAD_RETENTION_DAYS`, rate windows older than a day and
/// per-instance counts older than an hour.
pub async fn retention(pool: &PgPool, cfg: &WorkflowActionsConfig) -> sqlx::Result<Retention> {
    let mut out = Retention::default();
    loop {
        let n = sqlx::query(
            "DELETE FROM cmdb.workflow_action_deliveries WHERE id IN (
               SELECT id FROM cmdb.workflow_action_deliveries WHERE status IN ('delivered', 'skipped')
                  AND created_at < now() - $1 * interval '1 day' LIMIT $2)",
        )
        .bind(cfg.retention_days)
        .bind(RETENTION_BATCH)
        .execute(pool)
        .await?
        .rows_affected();
        out.deliveries += n;
        if n < RETENTION_BATCH as u64 {
            break;
        }
    }
    loop {
        let n = sqlx::query(
            "DELETE FROM cmdb.workflow_action_deliveries WHERE id IN (
               SELECT id FROM cmdb.workflow_action_deliveries WHERE status = 'dead'
                  AND created_at < now() - $1 * interval '1 day' LIMIT $2)",
        )
        .bind(cfg.dead_retention_days)
        .bind(RETENTION_BATCH)
        .execute(pool)
        .await?
        .rows_affected();
        out.dead += n;
        if n < RETENTION_BATCH as u64 {
            break;
        }
    }
    loop {
        let n = sqlx::query(
            "DELETE FROM cmdb.workflow_action_runs WHERE id IN (
               SELECT r.id FROM cmdb.workflow_action_runs r
                WHERE r.created_at < now() - $1 * interval '1 day' AND r.completed_at IS NOT NULL
                  AND r.status IN ('fanned_out', 'suppressed', 'cancelled')
                  AND NOT EXISTS (SELECT 1 FROM cmdb.workflow_action_deliveries d WHERE d.run_id = r.id)
                LIMIT $2)",
        )
        .bind(cfg.retention_days)
        .bind(RETENTION_BATCH)
        .execute(pool)
        .await?
        .rows_affected();
        out.runs += n;
        if n < RETENTION_BATCH as u64 {
            break;
        }
    }
    sqlx::query("DELETE FROM cmdb.workflow_action_rate_windows WHERE window_start < now() - interval '1 day'")
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM cmdb.workflow_action_instance_rate WHERE minute < now() - interval '1 hour'")
        .execute(pool)
        .await?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// The workers of this process
// ---------------------------------------------------------------------------

/// The outbox workers of this server process.
pub struct Outbox {
    stop: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
}

impl Outbox {
    /// Starts the workers, or nothing with `WORKFLOW_ACTIONS_WORKER=off`.
    pub fn spawn(pool: PgPool, cfg: WorkflowActionsConfig, channels: Channels) -> Option<Self> {
        cfg.worker.then(|| Self::start(pool, cfg, channels))
    }

    /// Starts the workers whatever `cfg.worker` says (tests).
    pub fn start(pool: PgPool, cfg: WorkflowActionsConfig, channels: Channels) -> Self {
        let (stop, rx) = watch::channel(false);
        let owner = Uuid::new_v4().to_string();
        let mut tasks: Vec<JoinHandle<()>> = (0..cfg.concurrency)
            .map(|i| tokio::spawn(fan_out_loop(pool.clone(), cfg, format!("{owner}/{i}"), rx.clone())))
            .collect();
        if !channels.0.is_empty() {
            tasks.push(tokio::spawn(delivery_loop(pool.clone(), cfg, channels, format!("{owner}/send"), rx.clone())));
        }
        tasks.push(tokio::spawn(housekeeping_loop(pool, cfg, rx)));
        Outbox { stop, tasks }
    }

    /// Stops claiming and waits up to 10 s; what is in flight is taken over when its lease ends.
    pub async fn stop(self) {
        let _ = self.stop.send(true);
        for t in self.tasks {
            if tokio::time::timeout(Duration::from_secs(10), t).await.is_err() {
                tracing::warn!(
                    "a workflow action worker did not stop within 10 s; its work is taken over when the \
                    lease ends"
                );
            }
        }
    }
}

async fn idle(stop: &mut watch::Receiver<bool>, wait: Duration) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(wait) => false,
        _ = stop.changed() => true,
    }
}

async fn fan_out_loop(pool: PgPool, cfg: WorkflowActionsConfig, owner: String, mut stop: watch::Receiver<bool>) {
    loop {
        if *stop.borrow() {
            return;
        }
        let wait = match claim_runs(&pool, &owner, RUN_BATCH).await {
            Ok(ids) if ids.is_empty() => cfg.poll,
            Ok(ids) => {
                for id in ids {
                    if let Err(e) = fan_out(&pool, &cfg, id, &owner).await {
                        // The run stays leased; housekeeping returns it when the lease ends.
                        tracing::warn!(run = id, error = %e, "workflow action fan-out failed; retried after its lease");
                    }
                }
                Duration::ZERO
            }
            Err(e) => {
                tracing::warn!(error = %e, "workflow action worker cannot claim runs; retrying");
                cfg.poll * 5
            }
        };
        if !wait.is_zero() && idle(&mut stop, wait).await {
            return;
        }
    }
}

async fn delivery_loop(
    pool: PgPool,
    cfg: WorkflowActionsConfig,
    channels: Channels,
    owner: String,
    mut stop: watch::Receiver<bool>,
) {
    let n = i64::try_from(cfg.concurrency).unwrap_or(1);
    loop {
        if *stop.borrow() {
            return;
        }
        let mut busy = false;
        for kind in channels.kinds() {
            let channel = channels.0[&kind].clone();
            let claimed = match claim_deliveries(&pool, &owner, kind, channel.timeout, n).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(error = %e, "workflow action worker cannot claim deliveries; retrying");
                    continue;
                }
            };
            busy |= !claimed.is_empty();
            let mut sends = JoinSet::new();
            for c in claimed {
                let (pool, channel) = (pool.clone(), channel.clone());
                sends.spawn(async move {
                    let outcome = match tokio::time::timeout(channel.timeout, (channel.send)(c.clone())).await {
                        Ok(o) => o,
                        Err(_) => Outcome::Transient {
                            status_code: None,
                            error: "The attempt timed out".into(),
                            retry_after: None,
                        },
                    };
                    if let Err(e) = record(&pool, &cfg, &c, outcome).await {
                        tracing::warn!(delivery = %c.id, error = %e, "cannot record a workflow action delivery; \
                            retried after its lease");
                    }
                });
            }
            while sends.join_next().await.is_some() {}
        }
        if !busy && idle(&mut stop, cfg.poll).await {
            return;
        }
    }
}

async fn housekeeping_loop(pool: PgPool, cfg: WorkflowActionsConfig, mut stop: watch::Receiver<bool>) {
    let tick = cfg.poll.max(Duration::from_secs(1));
    let mut last_retention: Option<Instant> = None;
    loop {
        match housekeeping(&pool, &cfg).await {
            Ok(h) if h.runs_released + h.deliveries_released + h.expired > 0 => tracing::info!(
                runs_released = h.runs_released,
                deliveries_released = h.deliveries_released,
                expired = h.expired,
                "workflow action housekeeping"
            ),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "workflow action housekeeping failed; retrying"),
        }
        if last_retention.is_none_or(|t| t.elapsed() >= RETENTION_EVERY) {
            last_retention = Some(Instant::now());
            match retention(&pool, &cfg).await {
                Ok(r) if r != Retention::default() => {
                    tracing::info!(deliveries = r.deliveries, dead = r.dead, runs = r.runs, "workflow action retention")
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "workflow action retention failed; retried in an hour"),
            }
        }
        if idle(&mut stop, tick).await {
            return;
        }
    }
}
