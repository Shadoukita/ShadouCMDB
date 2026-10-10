//! The e-mail channel of workflow actions (design SHAA-2725 §6, §4.6; slice
//! S4).
//!
//! **Fan-out** (in the run's transaction, [`fan_out`]): the recipients are
//! resolved now, and each gets one delivery row:
//!
//! - `skipped` with the reason when they get nothing: `actor`, `inactive`,
//!   `no_view` (they may not view the CI's class; the CI is never named to
//!   them), `no_email`, `address_not_allowed`, `mail_off`;
//! - a pending **lead** delivery, the message that will be sent;
//! - `skipped` / `coalesced`, pointing at a lead (`digest_id`), when one bulk
//!   request (runs sharing the action and the request id) already has a
//!   lead for that recipient: the runs of a bulk transition are fanned out
//!   together, and each recipient gets one message listing the CIs;
//! - `skipped` / `throttled_digest`, pointing at a **digest** delivery due at
//!   the end of the hour, past `MAIL_MAX_PER_RECIPIENT_PER_HOUR` leads to one
//!   recipient in the hour, or past 5 approval requests by one requester on
//!   one instance in the hour (SHAA-1869 §7.3).
//!
//! The counters are rows of `workflow_action_rate_windows`, shared by every
//! process. No address of a user is stored: it is read when the message is
//! sent, so a corrected address is used on a retry and an erased user leaves
//! no copy.
//!
//! **Sending** ([`channel`]): a lead or digest is rendered for its recipient
//! at send time, with the view check again (a retry may come hours later):
//! an event whose CI the recipient may not view now is left out, and with
//! none left the delivery is `skipped` / `no_view`. Fixed addresses get
//! minimal content only. The relay's 4xx is retried, its 5xx is dead for
//! that recipient only.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures_util::FutureExt;
use lettre::message::Mailbox;
use serde_json::Value;
use sqlx::types::Json as SqlJson;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::outbox::{Channel, Claimed, Outcome};
use super::recipients::{self, EventRef, RunRef, Subject};
use super::{WorkflowAction, WorkflowActionContent, WorkflowActionRecipientSource as Source, WorkflowActionSettings};
use crate::auth::permissions::{ClassOp, Permissions};
use crate::config::{MailConfig, WorkflowActionsConfig};
use crate::data::auth as auth_data;
use crate::modules::classes::AttributeDataType;
use crate::modules::mail::render::{self, Ci, Content, Custom, EventKind, Locale, Why};
use crate::modules::mail::{Mail, SendError};
use crate::schema::model::Model;

/// A bulk request's leads wait this long, so its later runs (and later
/// requests with the same request id) fold into them.
const BULK_GRACE: Duration = Duration::from_secs(10);
/// Approval requests one requester may cause to be mailed per instance and hour (SHAA-1869 §7.3).
const MAX_REQUESTS_PER_REQUESTER_PER_HOUR: i32 = 5;
/// How many fields a `detailed` message lists besides the subtitle.
const SUMMARY_FIELDS: usize = 5;

// ---------------------------------------------------------------------------
// Fan-out
// ---------------------------------------------------------------------------

/// One run of a fan-out group.
#[derive(Debug, Clone, sqlx::FromRow)]
pub(super) struct GroupRun {
    pub id: i64,
    pub event_id: i64,
    pub instance_id: Uuid,
    pub ci_id: Uuid,
}

#[derive(sqlx::FromRow)]
struct GroupEvent {
    id: i64,
    kind: String,
    actor_type: String,
    actor_id: Option<String>,
    approval_request_id: Option<Uuid>,
    requested_by_id: Option<Uuid>,
}

/// What [`fan_out`] wrote, per run.
pub(super) struct Written {
    /// Runs fanned out (the claimed one and the group), with `truncated` where recipients were cut.
    pub runs: Vec<(i64, bool)>,
    /// Runs whose CI or event is gone: cancelled.
    pub gone: Vec<i64>,
}

/// One delivery row to write.
struct Row {
    id: Uuid,
    run: i64,
    key: String,
    user: Option<Uuid>,
    status: &'static str,
    reason: Option<&'static str>,
    digest: Option<Uuid>,
    /// Pending rows: due now plus this.
    delay: Option<Duration>,
}

fn user_key(u: Uuid) -> String {
    format!("user:{u}")
}

fn addr_key(a: &str) -> String {
    format!("addr:{a}")
}

/// Fans out e-mail run `lead` (claimed by `owner`, its action `action`) and,
/// when it came from a request, every other pending run of the same action
/// and request id, in the caller's transaction.
#[allow(clippy::too_many_arguments)]
pub(super) async fn fan_out(
    tx: &mut PgConnection,
    cfg: &WorkflowActionsConfig,
    mail: &MailConfig,
    model: &Model,
    owner: &str,
    lead: GroupRun,
    request_id: Option<&str>,
    definition_id: Uuid,
    action: &WorkflowAction,
) -> sqlx::Result<Written> {
    let mut group = vec![lead];
    if let Some(req) = request_id {
        // One bulk request at a time per action: a second worker holding
        // runs of the same request waits here, then folds into these leads.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("workflow-action-mail:{}:{req}", action.id))
            .execute(&mut *tx)
            .await?;
        let more: Vec<GroupRun> = sqlx::query_as(
            "UPDATE cmdb.workflow_action_runs r
                SET status = 'fanning_out', lease_owner = $3, lease_until = now() + interval '60 seconds'
              WHERE r.id IN (SELECT id FROM cmdb.workflow_action_runs
                              WHERE status = 'pending' AND action_id = $1 AND http_request_id = $2
                              ORDER BY id FOR UPDATE SKIP LOCKED)
              RETURNING r.id, r.event_id, r.instance_id, r.ci_id",
        )
        .bind(action.id)
        .bind(req)
        .bind(owner)
        .fetch_all(&mut *tx)
        .await?;
        group.extend(more);
    }

    // Events and CIs of the group, in two reads.
    let event_ids: Vec<i64> = group.iter().map(|r| r.event_id).collect();
    let events: HashMap<i64, GroupEvent> = sqlx::query_as::<_, GroupEvent>(
        "SELECT e.id, e.kind, e.actor_type, e.actor_id, e.approval_request_id, req.requested_by_id
         FROM cmdb.workflow_instance_events e
         LEFT JOIN cmdb.workflow_approval_requests req ON req.id = e.approval_request_id
         WHERE e.id = ANY($1)",
    )
    .bind(&event_ids)
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(|e| (e.id, e))
    .collect();
    let ci_ids: Vec<Uuid> = group.iter().map(|r| r.ci_id).collect();
    let classes: HashMap<Uuid, Uuid> =
        sqlx::query_as::<_, (Uuid, Uuid)>("SELECT id, class_id FROM cmdb.configuration_items WHERE id = ANY($1)")
            .bind(&ci_ids)
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .collect();
    let mut written = Written { runs: Vec::new(), gone: Vec::new() };
    group.retain(|r| {
        let ok = events.contains_key(&r.event_id) && classes.contains_key(&r.ci_id);
        if !ok {
            written.gone.push(r.id);
        }
        ok
    });

    // Recipients: the static sources once, the CI and participant ones per run.
    let (dynamic, fixed): (Vec<_>, Vec<_>) = action.recipients.iter().cloned().partition(|r| {
        matches!(r.source, Source::CiOwner | Source::CiAttribute | Source::ServiceOwner | Source::Participant)
    });
    let base = recipients::resolve(&mut *tx, model, &fixed, None).await?;
    let mut per_run: Vec<(GroupRun, Vec<Uuid>, bool)> = Vec::with_capacity(group.len());
    for run in &group {
        let mut users: BTreeSet<Uuid> = base.users.keys().copied().collect();
        if !dynamic.is_empty() {
            let e = &events[&run.event_id];
            let subject = Subject {
                ci_id: run.ci_id,
                class_id: classes[&run.ci_id],
                run: Some(RunRef {
                    instance_id: run.instance_id,
                    definition_id,
                    event: EventRef {
                        kind: e.kind.clone(),
                        actor_type: e.actor_type.clone(),
                        actor_id: e.actor_id.clone(),
                        approval_request_id: e.approval_request_id,
                    },
                }),
            };
            users.extend(recipients::resolve(&mut *tx, model, &dynamic, Some(&subject)).await?.users.into_keys());
        }
        let truncated = users.len() > cfg.max_recipients;
        per_run.push((run.clone(), users.into_iter().take(cfg.max_recipients).collect(), truncated));
    }
    let everyone: Vec<Uuid> =
        per_run.iter().flat_map(|(_, u, _)| u.iter().copied()).collect::<BTreeSet<_>>().into_iter().collect();
    let accounts: HashMap<Uuid, (bool, bool)> = sqlx::query_as::<_, (Uuid, bool, bool)>(
        "SELECT id, is_active, coalesce(btrim(email), '') <> '' FROM cmdb.users WHERE id = ANY($1)",
    )
    .bind(&everyone)
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(|(id, active, email)| (id, (active, email)))
    .collect();
    let permissions = auth_data::load_permissions_of(&mut *tx, &everyone).await?;
    let exclude_actor = action.settings.exclude_actor.unwrap_or(true);

    // Who gets something, per run; and who is skipped why.
    let mut rows: Vec<Row> = Vec::new();
    let mut candidates: Vec<(i64, Vec<String>, Vec<Option<Uuid>>)> = Vec::with_capacity(per_run.len());
    for (run, users, truncated) in &per_run {
        let e = &events[&run.event_id];
        let class = classes[&run.ci_id];
        let actor = EventRef { actor_type: e.actor_type.clone(), actor_id: e.actor_id.clone(), ..EventRef::default() }
            .actor()
            .filter(|_| exclude_actor);
        let mut keys = Vec::new();
        let mut ids = Vec::new();
        for u in users {
            let (active, has_email) = accounts.get(u).copied().unwrap_or((false, false));
            let reason = if Some(*u) == actor {
                Some("actor")
            } else if !active {
                Some("inactive")
            } else if !permissions.get(u).is_some_and(|p| p.can(class, ClassOp::View)) {
                Some("no_view")
            } else if !has_email {
                Some("no_email")
            } else if !mail.enabled {
                Some("mail_off")
            } else {
                None
            };
            match reason {
                Some(r) => rows.push(skipped(run.id, user_key(*u), Some(*u), r, None)),
                None => {
                    keys.push(user_key(*u));
                    ids.push(Some(*u));
                }
            }
        }
        for a in &base.addresses {
            if !mail.address_allowed(a) {
                rows.push(skipped(run.id, addr_key(a), None, "address_not_allowed", None));
            } else if !mail.enabled {
                rows.push(skipped(run.id, addr_key(a), None, "mail_off", None));
            } else {
                keys.push(addr_key(a));
                ids.push(None);
            }
        }
        written.runs.push((run.id, *truncated));
        candidates.push((run.id, keys, ids));
    }

    // Approval requests: at most 5 mails per requester and instance an hour; the rest go to the digests.
    let mut requester_throttled: HashSet<i64> = HashSet::new();
    for (run, ..) in &per_run {
        let e = &events[&run.event_id];
        if e.kind == "approval_request"
            && let Some(requester) = e.requested_by_id
            && bump(&mut *tx, &[format!("mail:requester:{requester}:{}", run.instance_id)]).await?[0]
                > MAX_REQUESTS_PER_REQUESTER_PER_HOUR
        {
            requester_throttled.insert(run.id);
        }
    }

    // Leads already waiting for this request (an earlier request with the same id).
    let wanted: Vec<String> = candidates
        .iter()
        .filter(|(run, ..)| !requester_throttled.contains(run))
        .flat_map(|(_, k, _)| k.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut leads: HashMap<String, Uuid> = match request_id {
        Some(req) if !wanted.is_empty() => sqlx::query_as::<_, (String, Uuid)>(
            "SELECT d.recipient_key, d.id FROM cmdb.workflow_action_deliveries d
             JOIN cmdb.workflow_action_runs r ON r.id = d.run_id
             WHERE d.status = 'pending' AND d.digest_id IS NULL AND d.recipient_key = ANY($1)
               AND r.action_id = $2 AND r.http_request_id = $3
             FOR UPDATE OF d SKIP LOCKED",
        )
        .bind(&wanted)
        .bind(action.id)
        .bind(req)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .collect(),
        _ => HashMap::new(),
    };
    let existing: Vec<Uuid> = leads.values().copied().collect();

    // A new lead counts towards its recipient's hour; past the limit it is folded into the digest.
    let new_keys: Vec<String> = wanted.iter().filter(|k| !leads.contains_key(*k)).cloned().collect();
    let counts = bump(&mut *tx, &new_keys.iter().map(|k| format!("mail:{k}")).collect::<Vec<_>>()).await?;
    let over: HashSet<String> = new_keys
        .iter()
        .zip(&counts)
        .filter(|(_, n)| **n > mail.max_per_recipient_per_hour)
        .map(|(k, _)| k.clone())
        .collect();
    let mut digested = over.clone();
    for (run, keys, _) in &candidates {
        if requester_throttled.contains(run) {
            digested.extend(keys.iter().cloned());
        }
    }
    let digests = digest_leads(&mut *tx, &digested).await?;
    let mut new_digests: HashMap<String, Uuid> = HashMap::new();

    let bulk = request_id.is_some() && (group.len() > 1 || !existing.is_empty());
    for (run, keys, ids) in candidates {
        let throttled_run = requester_throttled.contains(&run);
        for (key, user) in keys.into_iter().zip(ids) {
            if throttled_run || (over.contains(&key) && !leads.contains_key(&key)) {
                let digest = match digests.get(&key).or(new_digests.get(&key)) {
                    Some(d) => *d,
                    None => {
                        let id = Uuid::new_v4();
                        new_digests.insert(key.clone(), id);
                        rows.push(Row {
                            id,
                            run,
                            key: digest_key(&key),
                            user,
                            status: "pending",
                            reason: None,
                            digest: None,
                            delay: None,
                        });
                        id
                    }
                };
                rows.push(skipped(run, key, user, "throttled_digest", Some(digest)));
            } else if let Some(lead) = leads.get(&key) {
                rows.push(skipped(run, key, user, "coalesced", Some(*lead)));
            } else {
                let id = Uuid::new_v4();
                leads.insert(key.clone(), id);
                rows.push(Row {
                    id,
                    run,
                    key,
                    user,
                    status: "pending",
                    reason: None,
                    digest: None,
                    delay: bulk.then_some(BULK_GRACE),
                });
            }
        }
    }
    write(&mut *tx, &rows).await?;
    if !existing.is_empty() {
        // Later runs of the request keep the lead waiting a little longer.
        sqlx::query(
            "UPDATE cmdb.workflow_action_deliveries
                SET next_attempt_at = greatest(next_attempt_at, clock_timestamp() + $2 * interval '1 millisecond')
              WHERE id = ANY($1) AND status = 'pending'",
        )
        .bind(&existing)
        .bind(i64::try_from(BULK_GRACE.as_millis()).unwrap_or(10_000))
        .execute(&mut *tx)
        .await?;
    }
    Ok(written)
}

fn skipped(run: i64, key: String, user: Option<Uuid>, reason: &'static str, digest: Option<Uuid>) -> Row {
    Row { id: Uuid::new_v4(), run, key, user, status: "skipped", reason: Some(reason), digest, delay: None }
}

/// The digest of recipient `key` for the current hour.
fn digest_key(key: &str) -> String {
    format!("digest:{key}:{}", Utc::now().timestamp() / 3600)
}

/// Adds one to each of the current hour's counters `scopes`, in a fixed
/// order (so two fan-outs never wait on each other crosswise), and returns
/// the new counts in the order given.
async fn bump(tx: &mut PgConnection, scopes: &[String]) -> sqlx::Result<Vec<i32>> {
    if scopes.is_empty() {
        return Ok(Vec::new());
    }
    let mut sorted: Vec<&String> = scopes.iter().collect();
    sorted.sort();
    sorted.dedup();
    let counts: HashMap<String, i32> = sqlx::query_as::<_, (String, i32)>(
        "INSERT INTO cmdb.workflow_action_rate_windows AS w (scope, window_start, count)
         SELECT s, date_trunc('hour', now()), 1 FROM unnest($1::text[]) WITH ORDINALITY AS u(s, n) ORDER BY n
         ON CONFLICT (scope, window_start) DO UPDATE SET count = w.count + 1
         RETURNING scope, count",
    )
    .bind(sorted)
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .collect();
    Ok(scopes.iter().map(|s| counts.get(s).copied().unwrap_or(1)).collect())
}

/// The digests still waiting for these recipients this hour.
async fn digest_leads(tx: &mut PgConnection, keys: &HashSet<String>) -> sqlx::Result<HashMap<String, Uuid>> {
    if keys.is_empty() {
        return Ok(HashMap::new());
    }
    let by_digest: HashMap<String, String> = keys.iter().map(|k| (digest_key(k), k.clone())).collect();
    let wanted: Vec<&String> = by_digest.keys().collect();
    let rows: Vec<(String, Uuid)> = sqlx::query_as(
        "SELECT recipient_key, id FROM cmdb.workflow_action_deliveries
         WHERE status = 'pending' AND recipient_key = ANY($1) FOR UPDATE SKIP LOCKED",
    )
    .bind(wanted)
    .fetch_all(&mut *tx)
    .await?;
    Ok(rows.into_iter().filter_map(|(k, id)| by_digest.get(&k).map(|key| (key.clone(), id))).collect())
}

async fn write(tx: &mut PgConnection, rows: &[Row]) -> sqlx::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let runs: Vec<i64> = rows.iter().map(|r| r.run).collect();
    let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
    let users: Vec<Option<Uuid>> = rows.iter().map(|r| r.user).collect();
    let statuses: Vec<&str> = rows.iter().map(|r| r.status).collect();
    let reasons: Vec<Option<&str>> = rows.iter().map(|r| r.reason).collect();
    let digests: Vec<Option<Uuid>> = rows.iter().map(|r| r.digest).collect();
    // Digests are due when their hour ends; bulk leads after the grace period.
    let delays: Vec<Option<i64>> = rows
        .iter()
        .map(|r| {
            if r.key.starts_with("digest:") {
                Some(-1)
            } else {
                r.delay.map(|d| i64::try_from(d.as_millis()).unwrap_or(0))
            }
        })
        .collect();
    sqlx::query(
        "INSERT INTO cmdb.workflow_action_deliveries
           (id, run_id, recipient_key, user_id, status, status_reason, digest_id, next_attempt_at, completed_at)
         SELECT u.id, u.run, u.key, u.user_id, u.status, u.reason, u.digest,
                CASE WHEN u.delay = -1 THEN date_trunc('hour', now()) + interval '1 hour'
                     WHEN u.delay IS NOT NULL THEN clock_timestamp() + u.delay * interval '1 millisecond'
                     ELSE now() END,
                CASE WHEN u.status = 'skipped' THEN now() END
         FROM unnest($1::uuid[], $2::bigint[], $3::text[], $4::uuid[], $5::text[], $6::text[], $7::uuid[], $8::bigint[])
           AS u(id, run, key, user_id, status, reason, digest, delay)
         ON CONFLICT (run_id, recipient_key) DO NOTHING",
    )
    .bind(&ids)
    .bind(&runs)
    .bind(&keys)
    .bind(&users)
    .bind(&statuses)
    .bind(&reasons)
    .bind(&digests)
    .bind(&delays)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Sending
// ---------------------------------------------------------------------------

/// The e-mail channel of this process.
pub fn channel(pool: PgPool, mail: Arc<Mail>) -> Channel {
    let timeout = mail.send_timeout() + Duration::from_secs(15);
    let send: super::outbox::Send = Arc::new(move |c: Claimed| {
        let (pool, mail) = (pool.clone(), mail.clone());
        async move { send(&pool, &mail, c).await }.boxed()
    });
    Channel { send, timeout }
}

/// What a message is, or why there is none.
enum Compose {
    Send(Mailbox, render::Rendered),
    Skip(&'static str),
}

async fn send(pool: &PgPool, mail: &Mail, c: Claimed) -> Outcome {
    let composed = match compose(pool, mail, &c).await {
        Ok(m) => m,
        Err(e) => {
            return Outcome::Transient {
                status_code: None,
                error: format!("Cannot read the message's data: {e}"),
                retry_after: None,
            };
        }
    };
    let (to, rendered) = match composed {
        Compose::Skip(reason) => return Outcome::Skipped { reason: reason.into() },
        Compose::Send(to, r) => (to, r),
    };
    let message = match render::message(mail, c.id, to, &rendered) {
        Ok(m) => m,
        Err(e) => {
            return Outcome::Permanent {
                status_code: None,
                reason: "invalid_message".into(),
                error: format!("The message cannot be built: {e}"),
            };
        }
    };
    match mail.send(message).await {
        Ok(()) => Outcome::Delivered { status_code: Some(250) },
        Err(SendError::Transient { code, message }) => {
            Outcome::Transient { status_code: code.map(i32::from), error: message, retry_after: None }
        }
        Err(SendError::Permanent { code, message }) => {
            Outcome::Permanent { status_code: code.map(i32::from), reason: "smtp_rejected".into(), error: message }
        }
    }
}

/// The recipient of a delivery: a user or a fixed address.
enum Recipient {
    User { id: Uuid, mailbox: Mailbox, locale: Locale, permissions: Permissions },
    Address { mailbox: Mailbox },
}

#[derive(sqlx::FromRow)]
struct MessageRun {
    instance_id: Uuid,
    ci_id: Uuid,
    definition_id: Uuid,
    action_id: Option<Uuid>,
    action_key: String,
    action_name: Option<String>,
    settings: Option<SqlJson<WorkflowActionSettings>>,
    kind: String,
    transition_key: Option<String>,
    from_state_key: Option<String>,
    to_state_key: String,
    occurred_at: DateTime<Utc>,
    actor_type: String,
    actor_id: Option<String>,
    actor_name: Option<String>,
    comment: Option<String>,
    approval_request_id: Option<Uuid>,
    definition_name: Option<String>,
    transition_name: Option<String>,
    from_name: Option<String>,
    to_name: Option<String>,
    request_no: Option<i32>,
    request_status: Option<String>,
    requested_by_name: Option<String>,
    step_name: Option<String>,
    due_at: Option<DateTime<Utc>>,
    class_id: Uuid,
    class_name: String,
    ci_label: String,
    ci_ident: Option<String>,
}

/// The runs a lead or digest delivery tells of: its own run (a lead) and
/// those of the deliveries folded into it, oldest first.
async fn runs_of(conn: &mut PgConnection, c: &Claimed, digest: bool) -> sqlx::Result<Vec<MessageRun>> {
    sqlx::query_as(
        "WITH runs AS (
           SELECT $1::bigint AS run_id WHERE NOT $3
           UNION SELECT run_id FROM cmdb.workflow_action_deliveries WHERE digest_id = $2)
         SELECT r.id AS run_id, r.instance_id, r.ci_id, r.definition_id, r.action_id, r.action_key,
                a.name AS action_name, a.settings, e.kind, e.transition_key, e.from_state_key, e.to_state_key,
                e.occurred_at, e.actor_type, e.actor_id, e.actor_name, e.comment, e.approval_request_id,
                d.name AS definition_name, tr.name AS transition_name, fs.name AS from_name, ts.name AS to_name,
                req.request_no, req.status AS request_status, req.requested_by_name, ps.name AS step_name,
                st.due_at, ci.class_id, cl.name AS class_name, ci.label AS ci_label, ci.ident AS ci_ident
         FROM runs JOIN cmdb.workflow_action_runs r ON r.id = runs.run_id
         JOIN cmdb.workflow_instance_events e ON e.id = r.event_id
         JOIN cmdb.workflow_instances i ON i.id = r.instance_id
         JOIN cmdb.workflow_definitions d ON d.id = i.definition_id
         JOIN cmdb.configuration_items ci ON ci.id = r.ci_id
         JOIN cmdb.ci_classes cl ON cl.id = ci.class_id
         LEFT JOIN cmdb.workflow_actions a ON a.id = r.action_id
         LEFT JOIN cmdb.workflow_approval_requests req ON req.id = e.approval_request_id
         LEFT JOIN cmdb.workflow_transitions tr ON tr.version_id = coalesce(req.version_id, i.version_id)
                                               AND tr.key = coalesce(e.transition_key, req.transition_key)
         LEFT JOIN cmdb.workflow_states fs ON fs.version_id = i.version_id AND fs.key = e.from_state_key
         LEFT JOIN cmdb.workflow_states ts ON ts.version_id = i.version_id AND ts.key = e.to_state_key
         LEFT JOIN cmdb.workflow_approval_request_steps st
           ON st.request_id = req.id AND st.step_no = req.current_step_no
         LEFT JOIN cmdb.workflow_transition_approval_steps ps ON ps.transition_id = tr.id AND ps.step_no = req.current_step_no
         ORDER BY e.occurred_at, r.id",
    )
    .bind(c.run_id)
    .bind(c.id)
    .bind(digest)
    .fetch_all(&mut *conn)
    .await
}

fn event_kind(kind: &str, request_status: Option<&str>) -> EventKind {
    match kind {
        "approval_request" => EventKind::ApprovalRequested,
        "approval_decision" => EventKind::ApprovalStep,
        "approval_close" => EventKind::ApprovalClosed(match request_status {
            Some("approved") => "approved",
            Some("rejected") => "rejected",
            Some("withdrawn") => "withdrawn",
            _ => "cancelled",
        }),
        "approval_overdue" => EventKind::ApprovalOverdue,
        "cancel" => EventKind::Cancelled,
        "force" => EventKind::Forced,
        _ => EventKind::Transition,
    }
}

async fn compose(pool: &PgPool, mail: &Mail, c: &Claimed) -> sqlx::Result<Compose> {
    let mut conn = pool.acquire().await?;
    let digest = c.recipient_key.starts_with("digest:");
    let target = c.recipient_key.strip_prefix("digest:").map_or(c.recipient_key.as_str(), |k| {
        // digest:<user:id | addr:a>:<hour>
        k.rsplit_once(':').map_or(k, |(target, _)| target)
    });
    let recipient = if let Some(user) = target.strip_prefix("user:").and_then(|u| u.parse::<Uuid>().ok()) {
        let row: Option<(bool, Option<String>, String, Option<String>)> =
            sqlx::query_as("SELECT is_active, email, display_name, locale FROM cmdb.users WHERE id = $1")
                .bind(user)
                .fetch_optional(&mut *conn)
                .await?;
        let Some((active, email, display_name, locale)) = row else { return Ok(Compose::Skip("user_deleted")) };
        if !active {
            return Ok(Compose::Skip("inactive"));
        }
        let Some(address) = email.as_deref().map(str::trim).filter(|e| !e.is_empty()).and_then(|e| e.parse().ok())
        else {
            return Ok(Compose::Skip("no_email"));
        };
        let permissions = auth_data::load_permissions_of(&mut conn, &[user]).await?.remove(&user).unwrap_or_default();
        Recipient::User {
            id: user,
            mailbox: Mailbox::new(Some(display_name), address),
            locale: Locale::of(locale.as_deref(), mail.cfg.default_locale),
            permissions,
        }
    } else if let Some(address) = target.strip_prefix("addr:") {
        if !mail.cfg.address_allowed(address) {
            return Ok(Compose::Skip("address_not_allowed"));
        }
        let Ok(address) = address.parse() else { return Ok(Compose::Skip("invalid_address")) };
        Recipient::Address { mailbox: Mailbox::new(None, address) }
    } else {
        return Ok(Compose::Skip("invalid_address"));
    };
    let locale = match &recipient {
        Recipient::User { locale, .. } => *locale,
        Recipient::Address { .. } => Locale::of(None, mail.cfg.default_locale),
    };

    let runs = runs_of(&mut conn, c, digest).await?;
    // The view check again, now: an event about a CI the recipient may not view is left out.
    let visible: Vec<&MessageRun> = runs
        .iter()
        .filter(|r| match &recipient {
            Recipient::User { permissions, .. } => permissions.can(r.class_id, ClassOp::View),
            Recipient::Address { .. } => true,
        })
        .collect();
    let Some(first) = visible.first() else { return Ok(Compose::Skip("no_view")) };
    let base = mail.cfg.public_url.clone().unwrap_or_default();
    let list_url = format!("{base}/workflows");
    let minimal = matches!(recipient, Recipient::Address { .. });
    let model = if !minimal
        && visible
            .iter()
            .any(|r| r.settings.as_ref().and_then(|s| s.0.content) == Some(WorkflowActionContent::Detailed))
    {
        Some(Model::load(&mut conn).await?)
    } else {
        None
    };
    let mut events = Vec::with_capacity(visible.len());
    for r in &visible {
        let content = if minimal {
            Content::Minimal
        } else {
            match r.settings.as_ref().and_then(|s| s.0.content) {
                Some(WorkflowActionContent::Minimal) => Content::Minimal,
                Some(WorkflowActionContent::Detailed) => Content::Detailed,
                _ => Content::Standard,
            }
        };
        let fields = match (&model, &recipient, content) {
            (Some(model), Recipient::User { permissions, .. }, Content::Detailed) => {
                summary_fields(&mut conn, model, r.ci_id, r.class_id, permissions, locale).await?
            }
            _ => Vec::new(),
        };
        events.push((content, event_of(r, content, fields, &base)));
    }

    let rendered = if digest {
        let list: Vec<render::Event> = events.into_iter().map(|(_, e)| e).collect();
        render::digest(locale, &list, mail.cfg.max_per_recipient_per_hour, &list_url)
    } else {
        let settings = first.settings.as_ref().map(|s| s.0.clone()).unwrap_or_default();
        let custom = custom(&settings, locale);
        let action = first.action_name.clone().unwrap_or_else(|| first.action_key.clone());
        let why = match &recipient {
            Recipient::Address { .. } => vec![Why::Address],
            Recipient::User { id, .. } => why(&mut conn, first, *id).await?,
        };
        if runs.len() > 1 {
            let mut cis: Vec<Ci> = events.iter().filter_map(|(_, e)| e.ci.clone()).collect();
            cis.sort_by(|a, b| a.label.cmp(&b.label).then_with(|| a.ident.cmp(&b.ident)));
            if cis.is_empty() {
                // Minimal content: one message that names no CI.
                render::single(locale, Content::Minimal, &custom, &events[0].1, &why, &action)
            } else {
                render::bulk(locale, &custom, &events[0].1, &cis, &list_url, &why, &action)
            }
        } else {
            let (content, e) = &events[0];
            render::single(locale, *content, &custom, e, &why, &action)
        }
    };
    let mailbox = match recipient {
        Recipient::User { mailbox, .. } | Recipient::Address { mailbox } => mailbox,
    };
    Ok(Compose::Send(mailbox, rendered))
}

/// The administrator's subject and intro in `locale`, else in the other language.
fn custom(s: &WorkflowActionSettings, locale: Locale) -> Custom {
    let pick = |en: Option<&String>, de: Option<&String>| -> Option<String> {
        match locale {
            Locale::En => en.or(de),
            Locale::De => de.or(en),
        }
        .cloned()
    };
    Custom {
        subject: s.subject.as_ref().and_then(|t| pick(t.en.as_ref(), t.de.as_ref())),
        intro: s.intro.as_ref().and_then(|t| pick(t.en.as_ref(), t.de.as_ref())),
    }
}

fn event_of(r: &MessageRun, content: Content, fields: Vec<(String, String)>, base: &str) -> render::Event {
    let minimal = content == Content::Minimal;
    render::Event {
        kind: event_kind(&r.kind, r.request_status.as_deref()),
        at: r.occurred_at,
        workflow: r.definition_name.clone().unwrap_or_default(),
        transition: r.transition_name.clone().or_else(|| r.transition_key.clone()),
        from: (!minimal).then(|| r.from_name.clone().or_else(|| r.from_state_key.clone())).flatten(),
        to: (!minimal).then(|| r.to_name.clone().unwrap_or_else(|| r.to_state_key.clone())),
        actor: (!minimal).then(|| r.actor_name.clone()).flatten(),
        comment: (!minimal).then(|| r.comment.clone()).flatten(),
        ci: (!minimal).then(|| Ci {
            label: r.ci_label.clone(),
            ident: r.ci_ident.clone(),
            class: r.class_name.clone(),
        }),
        approval: (!minimal && r.approval_request_id.is_some()).then(|| render::Approval {
            request_no: r.request_no,
            step: r.step_name.clone(),
            due_at: r.due_at,
            requested_by: r.requested_by_name.clone(),
        }),
        fields,
        url: format!("{base}/workflows/{}", r.instance_id),
    }
}

/// Why user `user` gets the message of run `r`: its sources, resolved again.
async fn why(conn: &mut PgConnection, r: &MessageRun, user: Uuid) -> sqlx::Result<Vec<Why>> {
    let Some(action) = r.action_id else { return Ok(Vec::new()) };
    let recipients = super::load_recipients(&mut *conn, &[action]).await?.remove(&action).unwrap_or_default();
    let model = Model::load(&mut *conn).await?;
    let subject = Subject {
        ci_id: r.ci_id,
        class_id: r.class_id,
        run: Some(RunRef {
            instance_id: r.instance_id,
            definition_id: r.definition_id,
            event: EventRef {
                kind: r.kind.clone(),
                actor_type: r.actor_type.clone(),
                actor_id: r.actor_id.clone(),
                approval_request_id: r.approval_request_id,
            },
        }),
    };
    let resolved = recipients::resolve(&mut *conn, &model, &recipients, Some(&subject)).await?;
    Ok(resolved.users.get(&user).map(|w| w.iter().cloned().collect()).unwrap_or_default())
}

/// A `detailed` message's fields: the type's subtitle field, then the first
/// five fields with a value in form order; a reference to a CI of a class
/// the recipient may not view reads "a CI you cannot view".
async fn summary_fields(
    conn: &mut PgConnection,
    model: &Model,
    ci: Uuid,
    class: Uuid,
    permissions: &Permissions,
    locale: Locale,
) -> sqlx::Result<Vec<(String, String)>> {
    let lineage: Vec<Uuid> = model.lineage(class).iter().map(|c| c.id).collect();
    let subtitle: Option<Uuid> = sqlx::query_scalar(
        "SELECT subtitle_attribute_id FROM cmdb.ci_classes WHERE id = ANY($1) AND subtitle_attribute_id IS NOT NULL
         ORDER BY array_position($1, id) DESC LIMIT 1",
    )
    .bind(&lineage)
    .fetch_optional(&mut *conn)
    .await?
    .flatten();
    let subtitle_key = subtitle.and_then(|id| model.field(id)).map(|f| f.key.clone());
    let mut values = crate::data::items::values(&mut *conn, model, &[ci]).await?;
    values.sort_by_key(|v| (Some(&v.key) != subtitle_key.as_ref(), v.sort_order));
    values.truncate(SUMMARY_FIELDS + usize::from(subtitle_key.is_some()));
    let refs: Vec<Uuid> = values.iter().filter_map(|v| v.reference()).collect();
    let names = crate::data::items::reference_names(&mut *conn, &refs).await?;
    let lookups: Vec<Uuid> = values
        .iter()
        .filter(|v| v.data_type == AttributeDataType::Lookup)
        .filter_map(|v| v.value.as_str().and_then(|s| s.parse().ok()))
        .collect();
    let lookups: HashMap<Uuid, String> =
        crate::data::impact::lookup_values(&mut *conn, &lookups).await?.into_iter().map(|l| (l.id, l.name)).collect();
    let hidden = render::hidden_reference(locale);
    Ok(values
        .into_iter()
        .map(|v| {
            let shown = match v.data_type {
                AttributeDataType::Reference => match v.reference().and_then(|r| names.get(&r)) {
                    Some(n) if permissions.can(n.class_id, ClassOp::View) => n.label.clone(),
                    _ => hidden.clone(),
                },
                AttributeDataType::Lookup => v
                    .value
                    .as_str()
                    .and_then(|s| s.parse::<Uuid>().ok())
                    .and_then(|id| lookups.get(&id).cloned())
                    .unwrap_or_default(),
                _ => match &v.value {
                    Value::String(s) => s.clone(),
                    Value::Bool(b) => render::yes_no(locale, *b),
                    other => other.to_string(),
                },
            };
            (v.label, shown)
        })
        .collect())
}

/// The subject an action's e-mail would have, for the preview: in `locale`,
/// for the CI `ci` (label, ident, type) when given.
pub(super) async fn preview_subject(
    conn: &mut PgConnection,
    d: &super::WorkflowDefinition,
    action: &WorkflowAction,
    ci: Option<(String, Option<String>, String)>,
    locale: Locale,
) -> sqlx::Result<String> {
    let transition: Option<String> = match &action.transition {
        Some(key) => sqlx::query_scalar(
            "SELECT t.name FROM cmdb.workflow_transitions t JOIN cmdb.workflow_definitions d
               ON d.current_version_id = t.version_id WHERE d.id = $1 AND t.key = $2",
        )
        .bind(d.id)
        .bind(key)
        .fetch_optional(&mut *conn)
        .await?
        .or_else(|| Some(key.clone())),
        None => None,
    };
    let content = match action.settings.content {
        Some(WorkflowActionContent::Minimal) => Content::Minimal,
        Some(WorkflowActionContent::Detailed) => Content::Detailed,
        _ => Content::Standard,
    };
    let e = render::Event {
        kind: match action.trigger {
            super::WorkflowActionTrigger::Transition => EventKind::Transition,
            super::WorkflowActionTrigger::ApprovalRequested => EventKind::ApprovalRequested,
            super::WorkflowActionTrigger::ApprovalStep => EventKind::ApprovalStep,
            super::WorkflowActionTrigger::ApprovalClosed => EventKind::ApprovalClosed("approved"),
            super::WorkflowActionTrigger::ApprovalOverdue => EventKind::ApprovalOverdue,
            super::WorkflowActionTrigger::InstanceCancelled => EventKind::Cancelled,
            super::WorkflowActionTrigger::InstanceForced => EventKind::Forced,
        },
        at: Utc::now(),
        workflow: d.name.clone(),
        transition,
        from: None,
        to: None,
        actor: None,
        comment: None,
        ci: ci.filter(|_| content != Content::Minimal).map(|(label, ident, class)| Ci { label, ident, class }),
        approval: None,
        fields: Vec::new(),
        url: String::new(),
    };
    let custom = custom(&action.settings, locale);
    Ok(render::single(locale, content, &custom, &e, &[], &action.name).subject)
}
