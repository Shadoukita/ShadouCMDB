//! Migrations 0073/0074 (workflow actions and notifications, SHAA-2731, design
//! SHAA-2725 slice S1): the upgrade of a populated install rewrites no table
//! and enqueues nothing for what happened before it; every constraint of the
//! new tables refuses what it should; the enqueue trigger writes one run per
//! matching action in the event's own transaction, and none otherwise.

use sqlx::PgPool;
use uuid::Uuid;

use super::upgrade_0046::{Fixture, assert_permissions_match, id, ok, refused, validated, workflow_fixture};
use super::upgrade_0051::approval_fixture;
use crate::db::{MIGRATOR, reconcile_and_link, scratch};

const CHECK: &str = "23514";
const UNIQUE: &str = "23505";

async fn count(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

async fn filenode(pool: &PgPool, table: &str) -> i64 {
    sqlx::query_scalar("SELECT pg_relation_filenode($1::regclass)::bigint")
        .bind(format!("cmdb.{table}"))
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A business service CI with the published `lifecycle` workflow of
/// [`workflow_fixture`] running on it (transition `finish`).
async fn workflow(pool: &PgPool) -> (Uuid, Fixture) {
    let class = id(pool, "SELECT id FROM ci_classes WHERE system_role = 'business_service'").await;
    let ci =
        id(pool, &format!("INSERT INTO configuration_items (class_id, label) VALUES ('{class}', 'one') RETURNING id"))
            .await;
    (ci, workflow_fixture(pool, "lifecycle", class, ci, None).await)
}

async fn action(
    pool: &PgPool,
    definition: Uuid,
    key: &str,
    kind: &str,
    trigger: &str,
    transition: Option<&str>,
) -> Uuid {
    let transition = transition.map_or("NULL".to_owned(), |t| format!("'{t}'"));
    id(
        pool,
        &format!(
            "INSERT INTO workflow_actions (definition_id, key, name, kind, trigger, transition_key)
             VALUES ('{definition}', '{key}', '{key}', '{kind}', '{trigger}', {transition}) RETURNING id"
        ),
    )
    .await
}

fn transition_event(instance: Uuid) -> String {
    format!(
        "INSERT INTO workflow_instance_events
           (instance_id, kind, transition_key, from_state_key, to_state_key, to_version_no, actor_type, actor_name,
            request_id)
         VALUES ('{instance}', 'transition', 'finish', 'planned', 'done', 1, 'user', 'test', 'req-t')"
    )
}

/// The runs written for events of `instance`, as (action key, status, reason).
async fn runs(pool: &PgPool, instance: Uuid) -> Vec<(String, String, Option<String>)> {
    sqlx::query_as(
        "SELECT action_key, status, status_reason FROM workflow_action_runs WHERE instance_id = $1
         ORDER BY event_id, action_key",
    )
    .bind(instance)
    .fetch_all(pool)
    .await
    .unwrap()
}

/// A v0.4.0 install at 0072 with 100,000 workflow events: 0073 changes no
/// table file (the events, the CIs, the users), enqueues nothing for those
/// events, adds the audit checks unvalidated, and 0074 validates them. The
/// rights match the server's; webhooks.manage is held by no profile.
#[tokio::test]
async fn the_upgrade_rewrites_nothing_and_enqueues_nothing_for_old_events() {
    const TEST: &str = "workflow_actions_upgrade";
    let Some(roles) = scratch::Roles::create(TEST).await else { return };
    let db = roles.empty().await;
    let pool = &db.pool;
    let mut migrator = pool.acquire().await.unwrap();
    MIGRATOR.run_to(72, &mut *migrator).await.expect("migrations up to 0072");
    reconcile_and_link(pool).await.expect("reconcile at 0072");

    let (_, f) = workflow(pool).await;
    ok(
        pool,
        &format!(
            "INSERT INTO workflow_instance_events (instance_id, kind, transition_key, from_state_key, to_state_key,
               to_version_no, actor_type, actor_name)
             SELECT '{}', 'transition', 'finish', 'planned', 'done', 1, 'user', 'bulk' FROM generate_series(1, 100000);
             INSERT INTO users (username, display_name, password_hash) VALUES ('alice', 'Alice', '$argon2id$v=19$test')",
            f.instance
        ),
    )
    .await;
    let tables = ["workflow_instance_events", "configuration_items", "users", "notifications"];
    let mut before = Vec::new();
    for t in tables {
        before.push(filenode(pool, t).await);
    }
    let events = count(pool, "SELECT count(*) FROM workflow_instance_events").await;
    assert_eq!(events, 100_002);

    MIGRATOR.run_to(73, &mut *migrator).await.expect("migration 0073");
    assert!(!validated(pool, "audit_log_action_valid").await, "0073 adds the audit checks NOT VALID");
    assert!(!validated(pool, "audit_log_values_present").await);
    MIGRATOR.run(&mut *migrator).await.expect("upgrade to the latest version");
    drop(migrator);
    reconcile_and_link(pool).await.expect("reconcile after the upgrade");
    assert!(validated(pool, "audit_log_action_valid").await, "0074 validates them");
    assert!(validated(pool, "audit_log_values_present").await);

    let mut after = Vec::new();
    for t in tables {
        after.push(filenode(pool, t).await);
    }
    assert_eq!(after, before, "no table rewritten: {tables:?}");
    assert_eq!(count(pool, "SELECT count(*) FROM workflow_instance_events").await, events);
    assert_eq!(count(pool, "SELECT count(*) FROM workflow_action_runs").await, 0, "nothing backfilled");
    assert_eq!(count(pool, "SELECT count(*) FROM users WHERE locale IS NOT NULL").await, 0);
    assert_eq!(
        count(pool, "SELECT count(*) FROM permission_profile_global_permissions WHERE permission = 'webhooks.manage'")
            .await,
        0,
        "only the Administrator holds webhooks.manage, implicitly"
    );
    assert_permissions_match(pool).await;
    for action in [
        "webhook_endpoint.rotate_secret",
        "webhook_endpoint.suspend",
        "webhook_endpoint.resume",
        "workflow.action_dead",
        "workflow.action_retry",
        "workflow.action_discard",
        "workflow.action_suppressed",
        "mail.test",
    ] {
        ok(
            pool,
            &format!(
                "INSERT INTO audit_log (actor_type, actor_name, action, entity_type, entity_id, new_value)
                 VALUES ('system', 'test', '{action}', 'webhook_endpoints', gen_random_uuid(), '{{}}')"
            ),
        )
        .await;
        refused(
            pool,
            &format!(
                "INSERT INTO audit_log (actor_type, actor_name, action, entity_type, entity_id, old_value, new_value)
                 VALUES ('system', 'test', '{action}', 'webhook_endpoints', gen_random_uuid(), '{{}}', '{{}}')"
            ),
            CHECK,
        )
        .await;
    }
    let pruned: Vec<String> = sqlx::query_scalar(
        "SELECT category FROM prune_audit_log(interval '400 days', 'changes', true)
         WHERE category LIKE 'workflow.action%' OR category LIKE 'webhook%' OR category = 'mail.test'",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert!(pruned.is_empty(), "nothing that old, but the scope accepts them: {pruned:?}");

    // The API role, as the running server: an action on `finish` fires for a
    // new transition, once.
    let api = roles.api_pool(&db).await;
    action(&api, f.definition, "tell_ops", "inbox", "transition", Some("finish")).await;
    ok(&api, &transition_event(f.instance)).await;
    assert_eq!(runs(&api, f.instance).await, vec![("tell_ops".into(), "pending".into(), None)]);
    sqlx::query("UPDATE users SET locale = 'de' WHERE username = 'alice'").execute(&api).await.unwrap();
    api.close().await;

    db.drop().await;
    roles.drop().await;
}

/// Each CHECK and unique constraint of the new tables, proven by an insert
/// it refuses next to one it accepts.
#[tokio::test]
async fn each_constraint_refuses_what_it_should() {
    let Some(db) = scratch::database("workflow_actions_constraints").await else { return };
    let pool = &db.pool;
    let (_, f) = workflow(pool).await;
    let def = f.definition;
    let profile = id(pool, "SELECT id FROM permission_profiles WHERE is_builtin").await;

    // Recipients: exactly one source, and the column of that source.
    let inbox = action(pool, def, "inbox", "inbox", "transition", Some("finish")).await;
    let recipient = |position: i32, source: &str, columns: &str, values: &str| {
        format!(
            "INSERT INTO workflow_action_recipients (action_id, position, source{columns})
             VALUES ('{inbox}', {position}, '{source}'{values})"
        )
    };
    ok(pool, &recipient(1, "profile", ", profile_id", &format!(", '{profile}'"))).await;
    ok(pool, &recipient(2, "ci_owner", "", "")).await;
    ok(pool, &recipient(3, "participant", ", participant", ", 'starter'")).await;
    ok(pool, &recipient(4, "address", ", address", ", 'cab@corp.example'")).await;
    ok(pool, &recipient(5, "service_owner", ", service_owner_role", ", 'technical'")).await;
    refused(pool, &recipient(6, "profile", "", ""), CHECK).await;
    refused(pool, &recipient(6, "group", ", profile_id", &format!(", '{profile}'")), CHECK).await;
    refused(pool, &recipient(6, "ci_owner", ", participant", ", 'actor'"), CHECK).await;
    refused(pool, &recipient(6, "participant", ", participant, address", ", 'actor', 'cab@corp.example'"), CHECK).await;
    refused(pool, &recipient(6, "address", ", address", ", 'not an address'"), CHECK).await;
    refused(pool, &recipient(21, "ci_owner", "", ""), CHECK).await;

    // Actions: an endpoint exactly for webhooks; a transition key exactly for
    // the triggers that have one; at most 10 per trigger and transition.
    let endpoint = id(
        pool,
        "INSERT INTO webhook_endpoints (key, name, url, secret_ciphertext, secret_key_id)
         VALUES ('itsm-prod', 'ITSM', 'https://itsm.corp.example/hook', '\\x01', '\\x02') RETURNING id",
    )
    .await;
    let insert_action = |key: &str, kind: &str, trigger: &str, transition: &str, endpoint: &str| {
        format!(
            "INSERT INTO workflow_actions (definition_id, key, name, kind, trigger, transition_key, endpoint_id)
             VALUES ('{def}', '{key}', '{key}', '{kind}', '{trigger}', {transition}, {endpoint})"
        )
    };
    let ep = format!("'{endpoint}'");
    ok(pool, &insert_action("sync", "webhook", "transition", "'finish'", &ep)).await;
    refused(pool, &insert_action("a1", "webhook", "transition", "'finish'", "NULL"), CHECK).await;
    refused(pool, &insert_action("a2", "inbox", "transition", "'finish'", &ep), CHECK).await;
    refused(pool, &insert_action("a3", "email", "instance_cancelled", "'finish'", "NULL"), CHECK).await;
    refused(pool, &insert_action("a4", "email", "approval_closed", "NULL", "NULL"), CHECK).await;
    refused(pool, &insert_action("sync", "email", "transition", "'finish'", "NULL"), UNIQUE).await;
    for n in 3..=10 {
        ok(pool, &insert_action(&format!("more_{n}"), "email", "transition", "'finish'", "NULL")).await;
    }
    let message = refused(pool, &insert_action("eleventh", "email", "transition", "'finish'", "NULL"), CHECK).await;
    assert!(message.contains("at most 10"), "{message}");
    ok(pool, &insert_action("other_trigger", "email", "instance_cancelled", "NULL", "NULL")).await;
    refused(pool, &format!("DELETE FROM webhook_endpoints WHERE id = '{endpoint}'"), "23503").await;

    // Endpoints: the previous secret, the auth header and the suspension each
    // come as a whole.
    let endpoint_row = |key: &str, columns: &str, values: &str| {
        format!(
            "INSERT INTO webhook_endpoints (key, name, url, secret_ciphertext, secret_key_id{columns})
             VALUES ('{key}', 'E', 'https://e.corp.example/', '\\x01', '\\x02'{values})"
        )
    };
    ok(
        pool,
        &endpoint_row(
            "rotated",
            ", previous_secret_ciphertext, previous_secret_key_id, previous_secret_until",
            ", '\\x03', '\\x04', now()",
        ),
    )
    .await;
    refused(pool, &endpoint_row("e1", ", previous_secret_ciphertext", ", '\\x03'"), CHECK).await;
    refused(
        pool,
        &endpoint_row("e2", ", previous_secret_ciphertext, previous_secret_until", ", '\\x03', now()"),
        CHECK,
    )
    .await;
    refused(pool, &endpoint_row("e3", ", previous_secret_until", ", now()"), CHECK).await;
    refused(pool, &endpoint_row("e4", ", auth_header_name", ", 'Authorization'"), CHECK).await;
    refused(pool, &endpoint_row("e5", ", status", ", 'suspended'"), CHECK).await;
    refused(pool, &endpoint_row("e6", ", suspended_reason", ", 'breaker'"), CHECK).await;
    refused(pool, &endpoint_row("e7", ", timeout_ms", ", 60000"), CHECK).await;
    refused(
        pool,
        "INSERT INTO webhook_endpoints (key, name, url, secret_ciphertext, secret_key_id)
         VALUES ('ftp', 'E', 'ftp://e.corp.example/', '\\x01', '\\x02')",
        CHECK,
    )
    .await;

    // Allowlist: one entry per host and port, a NULL port included.
    let host = |pattern: &str, port: &str| {
        format!(
            "INSERT INTO webhook_allowed_hosts (host_pattern, port, created_by_name) VALUES ('{pattern}', {port}, 'admin')"
        )
    };
    ok(pool, &host("*.corp.example", "NULL")).await;
    ok(pool, &host("*.corp.example", "8443")).await;
    refused(pool, &host("*.corp.example", "NULL"), UNIQUE).await;
    refused(pool, &host("*", "NULL"), CHECK).await;
    refused(pool, &host("Upper.example", "NULL"), CHECK).await;

    // Runs: one per event and action. Deliveries: one per run and recipient.
    let run = |event: i64| {
        format!(
            "INSERT INTO workflow_action_runs (event_id, action_id, action_key, kind, definition_id, instance_id, ci_id,
               status)
             SELECT {event}, '{inbox}', 'inbox', 'inbox', '{def}', '{}', ci_id, 'pending'
             FROM workflow_instances WHERE id = '{}' RETURNING id",
            f.instance, f.instance
        )
    };
    let run_id: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(run(1))).fetch_one(pool).await.unwrap();
    refused(pool, &run(1), UNIQUE).await;
    ok(pool, &run(2)).await;
    refused(
        pool,
        &format!(
            "INSERT INTO workflow_action_runs (event_id, action_key, kind, definition_id, instance_id, ci_id, status)
             VALUES (3, 'x', 'inbox', '{def}', '{}', gen_random_uuid(), 'suppressed')",
            f.instance
        ),
        CHECK,
    )
    .await;
    let delivery = |key: &str, status: &str, extra: &str| {
        format!(
            "INSERT INTO workflow_action_deliveries (run_id, recipient_key, status{extra})
             VALUES ({run_id}, '{key}', '{status}'{})",
            if extra.is_empty() { "" } else { ", now()" }
        )
    };
    ok(pool, &delivery("addr:cab@corp.example", "pending", "")).await;
    refused(pool, &delivery("addr:cab@corp.example", "pending", ""), UNIQUE).await;
    refused(pool, &delivery("addr:ops@corp.example", "sending", ""), CHECK).await;
    refused(pool, &delivery("addr:ops@corp.example", "pending", ", lease_until"), CHECK).await;
    refused(pool, &delivery("addr:ops@corp.example", "dead", ""), CHECK).await;
    ok(pool, &delivery("addr:ops@corp.example", "sending", ", lease_until")).await;

    // Users: a language the server writes in, or none.
    ok(pool, "INSERT INTO users (username, display_name, password_hash, locale) VALUES ('dora', 'Dora', '$argon2id$v=19$test', 'de')").await;
    refused(pool, "INSERT INTO users (username, display_name, password_hash, locale) VALUES ('fred', 'Fred', '$argon2id$v=19$test', 'fr')", CHECK)
        .await;

    // Notifications: the two new kinds and the endpoint entity.
    let user = id(pool, "SELECT id FROM users WHERE username = 'dora'").await;
    ok(
        pool,
        &format!(
            "INSERT INTO notifications (user_id, kind, entity_type, entity_id, dedupe_key)
             VALUES ('{user}', 'webhook_suspended', 'webhook_endpoints', '{endpoint}', 'webhook_suspended:1'),
                    ('{user}', 'workflow_action', 'workflow_instances', '{}', 'action:1')",
            f.instance
        ),
    )
    .await;

    // Attribute actions: part of the graph, so refused on a published version.
    let attribute = id(pool, "SELECT id FROM ci_attribute_definitions ORDER BY key LIMIT 1").await;
    let message = refused(
        pool,
        &format!(
            "INSERT INTO workflow_transition_set_attributes (transition_id, position, attribute_id, value_from)
             VALUES ('{}', 1, '{attribute}', 'clear')",
            f.finish
        ),
        "55000",
    )
    .await;
    assert!(message.contains("only a draft"), "{message}");
    let draft = id(
        pool,
        &format!("INSERT INTO workflow_versions (definition_id, version_no, status) VALUES ('{def}', 2, 'draft') RETURNING id"),
    )
    .await;
    let (a, b) = (
        id(pool, &format!("INSERT INTO workflow_states (version_id, key, name, category) VALUES ('{draft}', 'a', 'A', 'open') RETURNING id")).await,
        id(pool, &format!("INSERT INTO workflow_states (version_id, key, name, category) VALUES ('{draft}', 'b', 'B', 'done') RETURNING id")).await,
    );
    let t = id(
        pool,
        &format!(
            "INSERT INTO workflow_transitions (version_id, key, name, from_state_id, to_state_id)
             VALUES ('{draft}', 'go', 'Go', '{a}', '{b}') RETURNING id"
        ),
    )
    .await;
    let set = |position: i32, value_from: &str, value: &str| {
        format!(
            "INSERT INTO workflow_transition_set_attributes (transition_id, position, attribute_id, value_from, value)
             VALUES ('{t}', {position}, '{attribute}', '{value_from}', {value})"
        )
    };
    refused(pool, &set(1, "literal", "NULL"), CHECK).await;
    refused(pool, &set(1, "now", "'\"x\"'"), CHECK).await;
    ok(pool, &set(1, "literal", "'\"retired\"'")).await;
    refused(pool, &set(2, "clear", "NULL"), UNIQUE).await;
    refused(pool, "TRUNCATE workflow_transition_set_attributes", "42501").await;

    db.drop().await;
}

/// The enqueue trigger: nothing when no action matches; one run per matching,
/// enabled action, written at commit in the event's transaction and rolled
/// back with it; `suppressed` / `queue_full` while the queue is flagged; the
/// approval triggers on the request's transition.
#[tokio::test]
async fn the_enqueue_trigger_writes_one_run_per_matching_action_in_the_events_transaction() {
    let Some(db) = scratch::database("workflow_actions_enqueue").await else { return };
    let pool = &db.pool;
    let (ci, f) = workflow(pool).await;
    let def = f.definition;

    // No action at all, then only actions that do not match.
    ok(pool, &transition_event(f.instance)).await;
    action(pool, def, "on_other", "inbox", "transition", Some("other")).await;
    action(pool, def, "on_cancel", "inbox", "instance_cancelled", None).await;
    let off = action(pool, def, "disabled", "inbox", "transition", Some("finish")).await;
    ok(pool, &format!("UPDATE workflow_actions SET enabled = false WHERE id = '{off}'")).await;
    ok(pool, &transition_event(f.instance)).await;
    assert_eq!(runs(pool, f.instance).await, vec![], "no matching action, no run");

    // Two matching actions: two runs for the next transition, with ids only.
    let inbox = action(pool, def, "tell_ops", "inbox", "transition", Some("finish")).await;
    action(pool, def, "mail_cab", "email", "transition", Some("finish")).await;
    ok(pool, &transition_event(f.instance)).await;
    let event: i64 = sqlx::query_scalar("SELECT max(id) FROM workflow_instance_events").fetch_one(pool).await.unwrap();
    assert_eq!(
        runs(pool, f.instance).await,
        vec![("mail_cab".into(), "pending".into(), None), ("tell_ops".into(), "pending".into(), None)]
    );
    let row: (i64, Option<Uuid>, String, Uuid, Uuid, Option<String>, i16) = sqlx::query_as(
        "SELECT event_id, action_id, kind, definition_id, ci_id, http_request_id, depth FROM workflow_action_runs
         WHERE action_key = 'tell_ops'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(row, (event, Some(inbox), "inbox".into(), def, ci, Some("req-t".into()), 0));

    // In the event's transaction: at commit, or earlier when constraints are
    // made immediate, and gone with a rollback.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(transition_event(f.instance))).execute(&mut *tx).await.unwrap();
    let pending = "SELECT count(*) FROM workflow_action_runs";
    let n: i64 = sqlx::query_scalar(pending).fetch_one(&mut *tx).await.unwrap();
    assert_eq!(n, 2, "deferred to the end of the transaction");
    sqlx::query("SET CONSTRAINTS ALL IMMEDIATE").execute(&mut *tx).await.unwrap();
    let n: i64 = sqlx::query_scalar(pending).fetch_one(&mut *tx).await.unwrap();
    assert_eq!(n, 4, "written inside the event's transaction");
    tx.rollback().await.unwrap();
    assert_eq!(count(pool, pending).await, 2, "a rolled-back transition enqueues nothing");

    // A cancel fires the instance trigger only.
    ok(
        pool,
        &format!(
            "INSERT INTO workflow_instance_events (instance_id, kind, from_state_key, to_state_key, to_version_no,
               actor_type, actor_name)
             VALUES ('{}', 'cancel', 'planned', 'planned', 1, 'user', 'test')",
            f.instance
        ),
    )
    .await;
    assert_eq!(count(pool, "SELECT count(*) FROM workflow_action_runs WHERE action_key = 'on_cancel'").await, 1);

    // Overloaded: written, but suppressed.
    ok(pool, "UPDATE workflow_action_queue_state SET overloaded = true").await;
    ok(pool, &transition_event(f.instance)).await;
    assert_eq!(
        count(
            pool,
            "SELECT count(*) FROM workflow_action_runs WHERE status = 'suppressed' AND status_reason = 'queue_full'"
        )
        .await,
        2
    );
    ok(pool, "UPDATE workflow_action_queue_state SET overloaded = false").await;

    // Approvals: requested and step 1 when the request is made; closed, through
    // the status filter, by the request's last event once it is closed.
    action(pool, def, "requested", "inbox", "approval_requested", Some("finish")).await;
    action(pool, def, "step", "inbox", "approval_step", Some("finish")).await;
    let closed_any = action(pool, def, "closed_any", "inbox", "approval_closed", Some("finish")).await;
    let closed_ok = action(pool, def, "closed_ok", "inbox", "approval_closed", Some("finish")).await;
    ok(
        pool,
        &format!("UPDATE workflow_actions SET settings = '{{\"statuses\": [\"approved\"]}}' WHERE id = '{closed_ok}'"),
    )
    .await;
    let approval = approval_fixture(pool, &f).await;
    let fired = |kind: &'static str| async move {
        let keys: Vec<String> = sqlx::query_scalar(
            "SELECT r.action_key FROM workflow_action_runs r JOIN workflow_instance_events e ON e.id = r.event_id
             WHERE e.kind = $1 ORDER BY r.action_key",
        )
        .bind(kind)
        .fetch_all(pool)
        .await
        .unwrap();
        keys
    };
    assert_eq!(fired("approval_request").await, ["requested", "step"]);
    ok(
        pool,
        &format!(
            "INSERT INTO workflow_instance_events (instance_id, kind, from_state_key, to_state_key, to_version_no,
               actor_type, actor_name, approval_request_id)
             VALUES ('{}', 'approval_close', 'planned', 'planned', 1, 'user', 'admin', '{r}');
             UPDATE workflow_approval_requests SET status = 'cancelled', close_reason = 'withdrawn', closed_at = now(),
               closed_by_name = 'admin' WHERE id = '{r}'",
            f.instance,
            r = approval.request
        ),
    )
    .await;
    assert_eq!(fired("approval_close").await, ["closed_any"], "the `statuses` filter leaves out closed_ok");
    let fired_for: i64 =
        count(pool, &format!("SELECT count(*) FROM workflow_action_runs WHERE action_id = '{closed_any}'")).await;
    assert_eq!(fired_for, 1);

    db.drop().await;
}
