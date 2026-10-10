//! QA (SHAA-3062): a v0.4.0 install from before workflow e-mail (S4, #879)
//! and the test send (#892), at 0078, with actions, runs and deliveries in
//! every state, upgraded to the current schema (0079-0081). The upgrade
//! rewrites none of the action tables or the audit log, keeps every row and
//! its state, builds the two partial lookups of 0079 valid, leaves the audit
//! checks of 0080 unvalidated until 0081, and the new audit action and
//! notification entity type are accepted only from 0080 on.

use sqlx::PgPool;
use uuid::Uuid;

use super::upgrade_0046::{id, ok, refused, validated, workflow_fixture};
use crate::db::{MIGRATOR, reconcile_and_link, scratch};

const CHECK: &str = "23514";

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

/// Whether index `name` exists and is valid (not left behind by a failed build).
async fn index_valid(pool: &PgPool, name: &str) -> Option<bool> {
    sqlx::query_scalar(
        "SELECT i.indisvalid FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid
         JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = 'cmdb' AND c.relname = $1",
    )
    .bind(name)
    .fetch_optional(pool)
    .await
    .unwrap()
}

fn test_audit(entity: Uuid) -> String {
    format!(
        "INSERT INTO audit_log (actor_type, actor_name, action, entity_type, entity_id, new_value)
         VALUES ('user', 'designer', 'workflow.action_test', 'workflow_definitions', '{entity}',
                 '{{\"action\": \"tell\", \"kind\": \"email\", \"ok\": true}}')"
    )
}

fn test_notification(user: Uuid, entity: Uuid) -> String {
    format!(
        "INSERT INTO notifications (user_id, kind, entity_type, entity_id, data, dedupe_key)
         VALUES ('{user}', 'workflow_action', 'workflow_definitions', '{entity}', '{{\"test\": true}}', 'qa-3062')"
    )
}

#[tokio::test]
async fn a_pre_s4_install_with_queued_actions_upgrades_in_place() {
    const TEST: &str = "upgrade_0079_mail";
    let Some(roles) = scratch::Roles::create(TEST).await else { return };
    let db = roles.empty().await;
    let pool = &db.pool;
    let mut migrator = pool.acquire().await.unwrap();
    MIGRATOR.run_to(78, &mut *migrator).await.expect("migrations up to 0078 (before S4)");
    reconcile_and_link(pool).await.expect("reconcile at 0078");

    let class = id(pool, "SELECT id FROM ci_classes WHERE system_role = 'business_service'").await;
    let ci =
        id(pool, &format!("INSERT INTO configuration_items (class_id, label) VALUES ('{class}', 'one') RETURNING id"))
            .await;
    let f = workflow_fixture(pool, "lifecycle", class, ci, None).await;
    let user = id(
        pool,
        "INSERT INTO users (username, display_name, password_hash) VALUES ('alice', 'Alice', '$argon2id$v=19$test')
         RETURNING id",
    )
    .await;
    let endpoint = id(
        pool,
        "INSERT INTO webhook_endpoints (key, name, url, secret_ciphertext, secret_key_id)
         VALUES ('itsm', 'ITSM', 'https://itsm.corp.example/hook', decode(repeat('00', 60), 'hex'), 1)
         RETURNING id",
    )
    .await;
    // One action per kind; the e-mail one names two fixed addresses.
    let mut actions = Vec::new();
    for kind in ["inbox", "email", "webhook"] {
        let endpoint = if kind == "webhook" { format!("'{endpoint}'") } else { "NULL".to_owned() };
        actions.push(
            id(
                pool,
                &format!(
                    "INSERT INTO workflow_actions (definition_id, key, name, kind, trigger, transition_key, endpoint_id)
                     VALUES ('{}', 'tell_{kind}', 'Tell {kind}', '{kind}', 'transition', 'finish', {endpoint})
                     RETURNING id",
                    f.definition
                ),
            )
            .await,
        );
    }
    ok(
        pool,
        &format!(
            "INSERT INTO workflow_action_recipients (action_id, position, source, address)
               VALUES ('{email}', 1, 'address', 'carol@corp.example');
             INSERT INTO workflow_action_recipients (action_id, position, source, address)
               VALUES ('{email}', 2, 'address', 'cab@corp.example');",
            email = actions[1]
        ),
    )
    .await;
    // 2,000 events, each with a fanned-out e-mail run; deliveries in every state, with a pre-#881 raw address in
    // last_error; one run still pending with a request id (the 0079 lookup by action and request).
    ok(
        pool,
        &format!(
            "INSERT INTO workflow_instance_events (instance_id, kind, transition_key, from_state_key, to_state_key,
               to_version_no, actor_type, actor_name)
             SELECT '{instance}', 'transition', 'finish', 'planned', 'done', 1, 'user', 'bulk'
             FROM generate_series(1, 2000);
             INSERT INTO workflow_action_runs (event_id, action_id, action_key, kind, definition_id, instance_id, ci_id,
               http_request_id, status)
             SELECT e.id, '{email}', 'tell_email', 'email', '{definition}', '{instance}', '{ci}', 'req-' || e.id,
                    CASE WHEN e.id = (SELECT max(id) FROM workflow_instance_events) THEN 'pending' ELSE 'fanned_out' END
             FROM workflow_instance_events e WHERE e.actor_name = 'bulk';
             INSERT INTO workflow_action_deliveries (run_id, recipient_key, user_id, status, status_reason, attempts,
               last_status_code, last_error)
             SELECT r.id, 'address:carol@corp.example', NULL,
                    (ARRAY['pending', 'held', 'delivered', 'skipped', 'dead'])[1 + r.id % 5],
                    CASE WHEN r.id % 5 IN (3, 4) THEN 'max_attempts' END,
                    r.id % 3, 451, '451 4.2.0 <carol@corp.example> mailbox busy'
             FROM workflow_action_runs r WHERE r.status = 'fanned_out';",
            instance = f.instance,
            email = actions[1],
            definition = f.definition,
        ),
    )
    .await;
    let states = "SELECT string_agg(status || ':' || n, ',' ORDER BY status)
                  FROM (SELECT status, count(*) AS n FROM workflow_action_deliveries GROUP BY status) s";
    let states_before: String = sqlx::query_scalar(states).fetch_one(pool).await.unwrap();
    // The enqueue trigger also queued a run per event for the other actions.
    let runs = "SELECT string_agg(kind || '/' || status || ':' || n, ',' ORDER BY kind, status)
                FROM (SELECT kind, status, count(*) AS n FROM workflow_action_runs GROUP BY kind, status) s";
    let runs_before: String = sqlx::query_scalar(runs).fetch_one(pool).await.unwrap();
    let raw = "SELECT count(*) FROM workflow_action_deliveries WHERE last_error LIKE '%carol@corp.example%'";
    let raw_before = count(pool, raw).await;
    assert!(raw_before >= 1999, "{raw_before}");
    // The new action and entity type are refused before 0080.
    refused(pool, &test_audit(f.definition), CHECK).await;
    refused(pool, &test_notification(user, f.definition), CHECK).await;

    let tables = [
        "webhook_endpoints",
        "workflow_actions",
        "workflow_action_recipients",
        "workflow_action_runs",
        "workflow_action_deliveries",
        "audit_log",
        "notifications",
    ];
    let mut before = Vec::new();
    for t in tables {
        before.push(filenode(pool, t).await);
    }
    let audit_before = count(pool, "SELECT count(*) FROM audit_log").await;

    MIGRATOR.run_to(80, &mut *migrator).await.expect("migrations 0079 and 0080");
    assert!(!validated(pool, "audit_log_action_valid").await, "0080 re-adds the audit checks NOT VALID");
    assert!(!validated(pool, "audit_log_values_present").await);
    MIGRATOR.run(&mut *migrator).await.expect("upgrade to the latest version");
    drop(migrator);
    reconcile_and_link(pool).await.expect("reconcile after the upgrade");
    assert!(validated(pool, "audit_log_action_valid").await, "0081 validates them");
    assert!(validated(pool, "audit_log_values_present").await);

    let mut after = Vec::new();
    for t in tables {
        after.push(filenode(pool, t).await);
    }
    assert_eq!(after, before, "no table rewritten: {tables:?}");
    for index in ["workflow_action_runs_request_idx", "workflow_action_deliveries_pending_key_idx"] {
        assert_eq!(index_valid(pool, index).await, Some(true), "{index}");
    }

    // Every row and state is kept, the queue included; nothing is re-sent or re-enqueued.
    assert_eq!(count(pool, "SELECT count(*) FROM workflow_actions").await, 3);
    assert_eq!(
        count(pool, &format!("SELECT count(*) FROM workflow_actions WHERE endpoint_id = '{endpoint}'")).await,
        1,
        "the webhook action keeps its endpoint"
    );
    assert_eq!(count(pool, "SELECT count(*) FROM workflow_action_recipients").await, 2);
    let runs_after: String = sqlx::query_scalar(runs).fetch_one(pool).await.unwrap();
    assert_eq!(runs_after, runs_before);
    assert!(runs_after.contains("email/pending:1,"), "{runs_after}");
    let states_after: String = sqlx::query_scalar(states).fetch_one(pool).await.unwrap();
    assert_eq!(states_after, states_before);
    assert_eq!(count(pool, raw).await, raw_before, "stored errors are not rewritten (they are masked on read)");
    assert!(count(pool, "SELECT count(*) FROM audit_log").await >= audit_before, "no audit row removed");

    // From now on the test send can be audited and notified, and the prune scope takes it.
    ok(pool, &test_audit(f.definition)).await;
    ok(pool, &test_notification(user, f.definition)).await;
    refused(
        pool,
        &format!(
            "INSERT INTO audit_log (actor_type, actor_name, action, entity_type, entity_id, old_value, new_value)
             VALUES ('user', 'designer', 'workflow.action_test', 'workflow_definitions', '{}', '{{}}', '{{}}')",
            f.definition
        ),
        CHECK,
    )
    .await;
    let pruned: Vec<String> = sqlx::query_scalar(
        "SELECT category FROM prune_audit_log(interval '400 days', 'changes', true)
         WHERE category = 'workflow.action_test'",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert!(pruned.is_empty(), "nothing that old, but the scope accepts it: {pruned:?}");
    let problems: Vec<String> =
        sqlx::query_scalar("SELECT chain_seq || ' ' || problem || ': ' || detail FROM audit_log_verify()")
            .fetch_all(pool)
            .await
            .unwrap();
    assert_eq!(problems, Vec::<String>::new(), "the audit chain verifies");
    db.drop().await;
}
