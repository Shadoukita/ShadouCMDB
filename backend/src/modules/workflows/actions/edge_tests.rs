//! QA edge cases for the action outbox (SHAA-2849, #842) on a scratch
//! database: the inbox honours each recipient's rights and membership at
//! fan-out, not at enqueue; a sender lost mid-delivery is fenced and its
//! attempt counts; concurrent claims never hand a delivery out twice; the
//! retry limits at their edges; and a run whose fan-out always fails.

use std::collections::HashSet;
use std::time::Duration;

use sqlx::PgPool;
use uuid::Uuid;

use super::WorkflowActionKind;
use super::outbox::{self, Claimed, FanOut, Outcome};
use super::tests::{cfg, count, delivery, delivery_again, drain, next_in, queued};
use crate::config::WorkflowActionsConfig;
use crate::db::scratch;
use crate::db::upgrade_0046::{Fixture, ok};

/// One transition event of the fixture's instance: one run of its `tell` action.
async fn event(pool: &PgPool, f: &Fixture) {
    ok(
        pool,
        &format!(
            "INSERT INTO workflow_instance_events (instance_id, kind, transition_key, from_state_key, to_state_key,
               to_version_no, actor_type, actor_name)
             VALUES ('{}', 'transition', 'finish', 'planned', 'done', 1, 'system', 'qa')",
            f.instance
        ),
    )
    .await;
}

/// The latest run's deliveries, as (user, status, reason), by username.
async fn latest(pool: &PgPool) -> Vec<(String, String, Option<String>)> {
    sqlx::query_as(
        "SELECT u.username, d.status, d.status_reason FROM workflow_action_deliveries d JOIN users u ON u.id = d.user_id
         WHERE d.run_id = (SELECT max(id) FROM workflow_action_runs) ORDER BY u.username",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

fn row(user: &str, status: &str, reason: Option<&str>) -> (String, String, Option<String>) {
    (user.to_owned(), status.to_owned(), reason.map(str::to_owned))
}

async fn notifications(pool: &PgPool, user: Uuid) -> i64 {
    count(pool, &format!("SELECT count(*) FROM notifications WHERE kind = 'workflow_action' AND user_id = '{user}'"))
        .await
}

/// Recipients and their rights are resolved when the run is fanned out:
/// view taken away, an account disabled, a member added to or removed from a
/// recipient profile between the transition and the fan-out all count, and a
/// user named by two sources gets one delivery.
#[tokio::test]
async fn the_inbox_honours_the_recipients_rights_at_fan_out() {
    let Some(db) = scratch::database("actions_edge_rights").await else { return };
    let pool = &db.pool;
    let cfg = cfg();
    let (f, alice) = queued(pool, 0).await;
    let viewers = "(SELECT id FROM permission_profiles WHERE name = 'Viewers')";

    // View taken away after the transition: skipped, no notification.
    event(pool, &f).await;
    let grant: (Uuid, Uuid) = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "DELETE FROM permission_profile_class_permissions WHERE profile_id = {viewers} RETURNING profile_id, class_id"
    )))
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(drain(pool, &cfg).await, 1);
    assert_eq!(latest(pool).await, [row("alice", "skipped", Some("no_view"))]);
    assert_eq!(notifications(pool, alice).await, 0);

    // View given back, the account disabled after the transition: skipped as inactive.
    ok(
        pool,
        &format!(
            "INSERT INTO permission_profile_class_permissions (profile_id, class_id, can_view) VALUES ('{}', '{}', true)",
            grant.0, grant.1
        ),
    )
    .await;
    event(pool, &f).await;
    ok(pool, &format!("UPDATE users SET is_active = false WHERE id = '{alice}'")).await;
    assert_eq!(drain(pool, &cfg).await, 1);
    assert_eq!(latest(pool).await, [row("alice", "skipped", Some("inactive"))]);
    assert_eq!(notifications(pool, alice).await, 0);

    // Enabled again: delivered.
    ok(pool, &format!("UPDATE users SET is_active = true WHERE id = '{alice}'")).await;
    event(pool, &f).await;
    assert_eq!(drain(pool, &cfg).await, 1);
    assert_eq!(latest(pool).await, [row("alice", "delivered", None)]);
    assert_eq!(notifications(pool, alice).await, 1);

    // The Viewers profile becomes a recipient too; bob joins it after the transition.
    ok(
        pool,
        &format!(
            "INSERT INTO workflow_action_recipients (action_id, position, source, profile_id)
             SELECT id, 2, 'profile', {viewers} FROM workflow_actions WHERE key = 'tell'"
        ),
    )
    .await;
    event(pool, &f).await;
    let bob: Uuid = sqlx::query_scalar(
        "INSERT INTO users (username, display_name, password_hash) VALUES ('bob', 'Bob', '$argon2id$v=19$test')
         RETURNING id",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    ok(pool, &format!("INSERT INTO user_permission_profiles (user_id, profile_id) VALUES ('{bob}', {viewers})")).await;
    assert_eq!(drain(pool, &cfg).await, 1);
    assert_eq!(
        latest(pool).await,
        [row("alice", "delivered", None), row("bob", "delivered", None)],
        "alice, named twice, once; bob, a member by the fan-out"
    );
    assert_eq!((notifications(pool, alice).await, notifications(pool, bob).await), (2, 1));

    // bob leaves the profile after the next transition: not a recipient any more, no row at all.
    event(pool, &f).await;
    ok(pool, &format!("DELETE FROM user_permission_profiles WHERE user_id = '{bob}'")).await;
    assert_eq!(drain(pool, &cfg).await, 1);
    assert_eq!(latest(pool).await, [row("alice", "delivered", None)]);
    assert_eq!((notifications(pool, alice).await, notifications(pool, bob).await), (3, 1));
    db.drop().await;
}

async fn claim(pool: &PgPool, owner: &str) -> Vec<Claimed> {
    outbox::claim_deliveries(pool, owner, WorkflowActionKind::Email, Duration::from_secs(10), 10).await.unwrap()
}

async fn state(pool: &PgPool, d: Uuid) -> (String, Option<String>, i16, i32, Option<String>) {
    sqlx::query_as(
        "SELECT status, status_reason, attempts, lease_epoch, last_error FROM workflow_action_deliveries WHERE id = $1",
    )
    .bind(d)
    .fetch_one(pool)
    .await
    .unwrap()
}

fn delivered() -> Outcome {
    Outcome::Delivered { status_code: Some(250) }
}

/// A sender that stops mid-delivery (a restart): its lease runs out, its
/// attempt counts, the next sender takes over under a new epoch, and the
/// first one's late result writes nothing. A lost last attempt is dead and
/// audited once, and nothing written after that changes it.
#[tokio::test]
async fn a_sender_lost_mid_delivery_is_fenced_and_its_attempt_counts() {
    let Some(db) = scratch::database("actions_edge_lost_sender").await else { return };
    let pool = &db.pool;
    let cfg = WorkflowActionsConfig { max_attempts: 3, ..cfg() };
    let d = delivery(pool).await;

    let a = claim(pool, "process-a").await;
    assert_eq!((a.len(), a[0].attempts, a[0].epoch), (1, 1, 1));
    assert!(claim(pool, "process-b").await.is_empty(), "leased: nobody else may send it");
    assert_eq!(outbox::housekeeping(pool, &cfg).await.unwrap().deliveries_released, 0, "the lease has not ended");

    // process-a dies; its lease runs out.
    ok(pool, "UPDATE workflow_action_deliveries SET lease_until = now() - interval '1 second'").await;
    assert_eq!(outbox::housekeeping(pool, &cfg).await.unwrap().deliveries_released, 1);
    let (status, _, attempts, _, error) = state(pool, d).await;
    assert_eq!((status.as_str(), attempts), ("pending", 1), "the lost attempt counts");
    assert_eq!(error.as_deref(), Some("The attempt did not finish within its lease"));

    let b = claim(pool, "process-b").await;
    assert_eq!((b.len(), b[0].attempts, b[0].epoch), (1, 2, 2));
    assert!(!outbox::record(pool, &cfg, &a[0], delivered()).await.unwrap(), "process-a's late success is fenced");
    assert_eq!(state(pool, d).await.0, "sending");
    let late_failure = Outcome::Permanent { status_code: Some(550), reason: "rejected".into(), error: "late".into() };
    assert!(!outbox::record(pool, &cfg, &a[0], late_failure).await.unwrap(), "so is its late failure");
    assert!(outbox::record(pool, &cfg, &b[0], delivered()).await.unwrap());
    assert!(!outbox::record(pool, &cfg, &b[0], delivered()).await.unwrap(), "a second ack writes nothing");
    let (status, reason, attempts, _, error) = state(pool, d).await;
    assert_eq!((status.as_str(), reason, attempts, error), ("delivered", None, 2, None));
    assert_eq!(count(pool, "SELECT count(*) FROM audit_log WHERE action = 'workflow.action_dead'").await, 0);

    // The last attempt lost: dead as max_attempts, audited once; a late result changes nothing.
    let last = WorkflowActionsConfig { max_attempts: 1, ..cfg };
    let d2 = delivery_again(pool).await;
    let c = claim(pool, "process-c").await;
    assert_eq!((c.len(), c[0].id), (1, d2));
    ok(
        pool,
        &format!("UPDATE workflow_action_deliveries SET lease_until = now() - interval '1 second' WHERE id = '{d2}'"),
    )
    .await;
    let h = outbox::housekeeping(pool, &last).await.unwrap();
    assert_eq!(h.deliveries_released, 0);
    let (status, reason, attempts, ..) = state(pool, d2).await;
    assert_eq!((status.as_str(), reason.as_deref(), attempts), ("dead", Some("max_attempts"), 1));
    assert!(!outbox::record(pool, &last, &c[0], delivered()).await.unwrap(), "dead stays dead");
    assert_eq!(state(pool, d2).await.0, "dead");
    outbox::housekeeping(pool, &last).await.unwrap();
    assert_eq!(count(pool, "SELECT count(*) FROM audit_log WHERE action = 'workflow.action_dead'").await, 1);
    db.drop().await;
}

/// Eight senders claiming at once from 300 due deliveries: each delivery is
/// handed out once, with one attempt and epoch 1.
#[tokio::test]
async fn concurrent_senders_never_claim_one_delivery_twice() {
    let Some(db) = scratch::database("actions_edge_concurrent_claims").await else { return };
    let pool = &db.pool;
    delivery(pool).await;
    ok(
        pool,
        "INSERT INTO workflow_action_deliveries (run_id, recipient_key, status)
         SELECT (SELECT min(id) FROM workflow_action_runs), 'addr:' || g || '@b.example', 'pending'
         FROM generate_series(1, 299) g",
    )
    .await;
    let mut senders = tokio::task::JoinSet::new();
    for i in 0..8 {
        let pool = pool.clone();
        senders.spawn(async move {
            let mut mine = Vec::new();
            loop {
                let c = outbox::claim_deliveries(
                    &pool,
                    &format!("sender-{i}"),
                    WorkflowActionKind::Email,
                    Duration::from_secs(10),
                    7,
                )
                .await
                .unwrap();
                if c.is_empty() {
                    return mine;
                }
                mine.extend(c.into_iter().map(|c| (c.id, c.attempts, c.epoch)));
            }
        });
    }
    let mut all = Vec::new();
    while let Some(mine) = senders.join_next().await {
        all.extend(mine.unwrap());
    }
    let distinct: HashSet<Uuid> = all.iter().map(|c| c.0).collect();
    assert_eq!((all.len(), distinct.len()), (300, 300), "every delivery claimed exactly once");
    assert!(all.iter().all(|c| (c.1, c.2) == (1, 1)), "one attempt, epoch 1 each");
    assert_eq!(
        count(pool, "SELECT count(*) FROM workflow_action_deliveries WHERE status = 'sending' AND attempts = 1").await,
        300
    );
    db.drop().await;
}

/// The retry limits at their edges: with one attempt allowed the first
/// transient failure is dead; a Retry-After of zero makes the delivery due
/// at once; a Retry-After of a day waits an hour at most.
#[tokio::test]
async fn retry_limits_at_their_edges() {
    let Some(db) = scratch::database("actions_edge_retry_limits").await else { return };
    let pool = &db.pool;
    let transient = |after: Option<Duration>| Outcome::Transient {
        status_code: Some(503),
        error: "x".repeat(5000),
        retry_after: after,
    };

    let one = WorkflowActionsConfig { max_attempts: 1, ..cfg() };
    let d = delivery(pool).await;
    let c = claim(pool, "w").await;
    assert!(outbox::record(pool, &one, &c[0], transient(None)).await.unwrap());
    let (status, reason, attempts, _, error) = state(pool, d).await;
    assert_eq!((status.as_str(), reason.as_deref(), attempts), ("dead", Some("max_attempts"), 1));
    assert_eq!(error.map(|e| e.len()), Some(1024), "the error is capped, not refused");
    assert_eq!(count(pool, "SELECT count(*) FROM audit_log WHERE action = 'workflow.action_dead'").await, 1);

    let cfg = WorkflowActionsConfig { max_attempts: 3, ..cfg() };
    let d = delivery_again(pool).await;
    let c = claim(pool, "w").await;
    assert!(outbox::record(pool, &cfg, &c[0], transient(Some(Duration::ZERO))).await.unwrap());
    assert!(next_in(pool, d).await <= 0.5, "Retry-After: 0 is due now");
    let c = claim(pool, "w").await;
    assert_eq!((c.len(), c[0].attempts), (1, 2), "claimed again at once");
    assert!(outbox::record(pool, &cfg, &c[0], transient(Some(Duration::from_secs(86_400)))).await.unwrap());
    let wait = next_in(pool, d).await;
    assert!((3590.0..=3600.5).contains(&wait), "Retry-After capped at an hour: {wait} s");
    assert!(claim(pool, "w").await.is_empty());
    assert_eq!(state(pool, d).await.0, "pending");
    db.drop().await;
}

/// A database trigger that fails every notification of run `run`: a fan-out
/// that can never succeed (a poison run).
async fn poison(pool: &PgPool, run: i64) {
    ok(
        pool,
        &format!(
            "CREATE FUNCTION qa_poison() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN
               IF NEW.dedupe_key = 'action:{run}' THEN RAISE EXCEPTION 'poison run'; END IF;
               RETURN NEW;
             END $$;
             CREATE TRIGGER qa_poison BEFORE INSERT ON notifications FOR EACH ROW EXECUTE FUNCTION qa_poison();"
        ),
    )
    .await;
}

/// A run whose fan-out fails writes nothing, keeps its lease, and does not
/// hold up the runs claimed with it.
#[tokio::test]
async fn a_poison_run_does_not_hold_up_the_others() {
    let Some(db) = scratch::database("actions_edge_poison").await else { return };
    let pool = &db.pool;
    let cfg = cfg();
    let (f, alice) = queued(pool, 3).await;
    let first: i64 = sqlx::query_scalar("SELECT min(id) FROM workflow_action_runs").fetch_one(pool).await.unwrap();
    poison(pool, first).await;

    let ids = outbox::claim_runs(pool, "w", 10).await.unwrap();
    assert_eq!(ids.len(), 3);
    let mut done = 0;
    for id in ids {
        match outbox::fan_out(pool, &cfg, id, "w").await {
            Err(e) => {
                assert_eq!(id, first);
                assert!(e.to_string().contains("poison run"), "{e}");
            }
            Ok(r) => {
                assert_eq!(r, FanOut::Done);
                done += 1;
            }
        }
    }
    assert_eq!(done, 2);
    assert_eq!(notifications(pool, alice).await, 2);
    let poisoned: (String, Option<String>) =
        sqlx::query_as("SELECT status, lease_owner FROM workflow_action_runs WHERE id = $1")
            .bind(first)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(poisoned, ("fanning_out".to_owned(), Some("w".to_owned())), "kept leased; nothing written");
    assert_eq!(
        count(pool, &format!("SELECT count(*) FROM workflow_action_deliveries WHERE run_id = {first}")).await,
        0
    );
    let _ = f;
    db.drop().await;
}

/// A run whose fan-out fails every time should give up after the configured
/// attempts like a delivery does: cancelled or dead with a reason and
/// audited, not retried every lease for ever while it counts towards the
/// queue limit.
#[tokio::test]
async fn a_poison_run_gives_up_after_the_attempt_limit() {
    let Some(db) = scratch::database("actions_edge_poison_limit").await else { return };
    let pool = &db.pool;
    let cfg = WorkflowActionsConfig { max_attempts: 3, ..cfg() };
    queued(pool, 1).await;
    let run: i64 = sqlx::query_scalar("SELECT min(id) FROM workflow_action_runs").fetch_one(pool).await.unwrap();
    poison(pool, run).await;

    for _ in 0..10 {
        for id in outbox::claim_runs(pool, "w", 10).await.unwrap() {
            assert!(outbox::fan_out(pool, &cfg, id, "w").await.is_err());
        }
        ok(
            pool,
            "UPDATE workflow_action_runs SET lease_until = now() - interval '1 second' WHERE lease_until IS NOT NULL",
        )
        .await;
        outbox::housekeeping(pool, &cfg).await.unwrap();
    }
    let status: String = sqlx::query_scalar("SELECT status FROM workflow_action_runs WHERE id = $1")
        .bind(run)
        .fetch_one(pool)
        .await
        .unwrap();
    assert!(status != "pending" && status != "fanning_out", "still {status} after 10 failed fan-outs");
    assert!(
        count(pool, "SELECT count(*) FROM audit_log WHERE action LIKE 'workflow.action_%'").await >= 1,
        "the operator learns of it from the audit log"
    );
    db.drop().await;
}

/// WORKFLOW_ACTIONS_MAX_RECIPIENTS is the number of users one run
/// notifies at most: users who would be skipped (disabled, no view) should
/// not use up the cap and leave the users who may be told without a word.
#[tokio::test]
async fn the_recipient_cap_counts_only_users_who_are_told() {
    let Some(db) = scratch::database("actions_edge_recipient_cap").await else { return };
    let pool = &db.pool;
    let cfg = WorkflowActionsConfig { max_recipients: 1, ..cfg() };
    let (f, alice) = queued(pool, 0).await;
    // A disabled account whose id sorts before alice's, named first.
    ok(
        pool,
        "INSERT INTO users (id, username, display_name, password_hash, is_active)
         VALUES ('00000000-0000-4000-8000-000000000001', 'gone', 'Gone', '$argon2id$v=19$test', false);
         INSERT INTO workflow_action_recipients (action_id, position, source, user_id)
         SELECT id, 2, 'user', '00000000-0000-4000-8000-000000000001' FROM workflow_actions WHERE key = 'tell'",
    )
    .await;
    event(pool, &f).await;
    assert_eq!(drain(pool, &cfg).await, 1);
    assert_eq!(
        notifications(pool, alice).await,
        1,
        "alice may view the CI and is the one user to tell: {:?}",
        latest(pool).await
    );
    db.drop().await;
}
