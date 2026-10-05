//! Migrations 0051 and 0052 (approvals schema, SHAA-1871; design on SHAA-1869
//! §3): the upgrade of a populated workflow install rewrites no table and
//! validates its constraints apart, a published version's approval steps
//! never change, decisions are append-only (for the API role by privilege as
//! well), the database enforces one pending request per instance, one vote
//! per actor and per principal per step, the delegation window and exactly
//! one approver source, a CI purge archives the approvals with the instance,
//! and deleting a user for good keeps the delegation history with its names.

use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use super::upgrade_0029::{constraint_def, literals};
use super::upgrade_0046::{Fixture, id, ok, refused, workflow_fixture};
use crate::data::crud::AuditAction;
use crate::db::{MIGRATOR, scratch};

const APPROVAL_ACTIONS: [AuditAction; 4] = [
    AuditAction::WorkflowApprovalRequest,
    AuditAction::WorkflowApprovalDecide,
    AuditAction::WorkflowApprovalClose,
    AuditAction::WorkflowApprovalOverdue,
];

/// The new tables, each holding one row after [`approval_fixture`].
pub(crate) const TABLES: [&str; 7] = [
    "workflow_transition_approval_steps",
    "workflow_approval_assignments",
    "workflow_approval_delegations",
    "workflow_approval_requests",
    "workflow_approval_request_steps",
    "workflow_approval_eligibility",
    "workflow_approval_decisions",
];

const IMMUTABLE: &str = "55000";
const APPEND_ONLY: &str = "42501";
const CHECK: &str = "23514";
const UNIQUE: &str = "23505";

async fn validated(pool: &PgPool, table: &str, name: &str) -> bool {
    sqlx::query_scalar(
        "SELECT c.convalidated FROM pg_constraint c WHERE c.conrelid = $1::text::regclass AND c.conname = $2",
    )
    .bind(format!("cmdb.{table}"))
    .bind(name)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn filenode(pool: &PgPool, table: &str) -> i64 {
    sqlx::query_scalar("SELECT pg_relation_filenode($1::text::regclass)::bigint")
        .bind(format!("cmdb.{table}"))
        .fetch_one(pool)
        .await
        .unwrap()
}

pub(crate) async fn user(pool: &PgPool, name: &str) -> Uuid {
    id(
        pool,
        &format!(
            "INSERT INTO users (username, display_name, password_hash)
             VALUES ('{name}', '{name}', '$argon2id$v=19$test') RETURNING id"
        ),
    )
    .await
}

/// The approval rows on top of a [`workflow_fixture`]: a published version 2
/// whose `finish` has a one-step policy, an assignment, a delegation from
/// `pat` to `dee`, and a pending request on the fixture's instance whose
/// active step has one eligible principal (`pat`) and one decision (`dee`
/// for `pat`, through the delegation), plus its `approval_request` event.
pub(crate) struct Approval {
    pub request: Uuid,
    pub requester: Uuid,
    pub principal: Uuid,
    pub delegate: Uuid,
    pub delegation: Uuid,
    /// The published version 2 with the approval step.
    pub version: Uuid,
}

pub(crate) async fn approval_fixture(pool: &PgPool, f: &Fixture) -> Approval {
    let requester = user(pool, "rita").await;
    let principal = user(pool, "pat").await;
    let delegate = user(pool, "dee").await;
    let def = f.definition;
    let v2 = id(
        pool,
        &format!("INSERT INTO workflow_versions (definition_id, version_no, status) VALUES ('{def}', 2, 'draft') RETURNING id"),
    )
    .await;
    let state = |k: &str, category: &str, terminal: bool| {
        format!(
            "INSERT INTO workflow_states (version_id, key, name, category, is_terminal)
             VALUES ('{v2}', '{k}', '{k}', '{category}', {terminal}) RETURNING id"
        )
    };
    let planned = id(pool, &state("planned", "open", false)).await;
    let done = id(pool, &state("done", "done", true)).await;
    let finish = id(
        pool,
        &format!(
            "INSERT INTO workflow_transitions (version_id, key, name, from_state_id, to_state_id)
             VALUES ('{v2}', 'finish', 'Finish', '{planned}', '{done}') RETURNING id"
        ),
    )
    .await;
    ok(
        pool,
        &format!(
            "INSERT INTO workflow_transition_approval_steps
               (transition_id, step_no, key, name, required_approvals, due_after, on_overdue, exclude_actors_of)
               VALUES ('{finish}', 1, 'cab', 'CAB', 2, interval '2 days', 'reject', ARRAY['implement']);
             UPDATE workflow_versions SET initial_state_id = '{planned}', status = 'published', published_at = now(),
               published_by_name = 'test', checksum = sha256('graph v2') WHERE id = '{v2}';
             UPDATE workflow_definitions SET current_version_id = '{v2}' WHERE id = '{def}';
             INSERT INTO workflow_approval_assignments (definition_id, transition_key, step_key, source, profile_id)
               SELECT '{def}', 'finish', 'cab', 'profile', id FROM permission_profiles WHERE is_builtin ORDER BY name LIMIT 1"
        ),
    )
    .await;
    let delegation = id(
        pool,
        &format!(
            "INSERT INTO workflow_approval_delegations
               (principal_id, principal_name, delegate_id, delegate_name, starts_at, ends_at, reason, created_by_id,
                created_by_name)
             VALUES ('{principal}', 'pat', '{delegate}', 'dee', now() - interval '1 day', now() + interval '6 days',
                     'Leave', '{principal}', 'pat') RETURNING id"
        ),
    )
    .await;
    // The request is on the instance's pinned version 1, whose `finish` the fixture made.
    let request = id(
        pool,
        &format!(
            "INSERT INTO workflow_approval_requests
               (instance_id, version_id, transition_key, request_no, status, requested_by_id, requested_by_name,
                excluded_user_ids, comment, staged_fields, field_baseline)
             VALUES ('{}', '{}', 'finish', 1, 'pending', '{requester}', 'rita', ARRAY['{requester}'::uuid],
                     'Please', '{{\"owner\": \"x\"}}', '{{\"owner\": null}}') RETURNING id",
            f.instance, f.version
        ),
    )
    .await;
    ok(
        pool,
        &format!(
            "INSERT INTO workflow_approval_request_steps
               (request_id, step_no, step_key, required_approvals, status, activated_at, due_at, eligible_count, resolved_at)
               VALUES ('{request}', 1, 'cab', 2, 'active', now(), now() + interval '2 days', 2, now());
             INSERT INTO workflow_approval_eligibility (request_id, step_no, role, principal_kind, principal_id, via)
               VALUES ('{request}', 1, 'approver', 'user', '{principal}', '{{\"source\": \"user\", \"label\": \"pat\"}}');
             INSERT INTO workflow_approval_decisions
               (request_id, step_no, decision, actor_id, actor_name, credential, on_behalf_of_id, on_behalf_of_name,
                delegation_id, via, comment, http_request_id)
               VALUES ('{request}', 1, 'approve', '{delegate}', 'dee', 'session', '{principal}', 'pat', '{delegation}',
                       '[{{\"source\": \"user\", \"label\": \"pat\"}}]', 'Fine', 'req-2');
             INSERT INTO workflow_instance_events
               (instance_id, kind, from_state_key, to_state_key, to_version_no, actor_type, actor_id, actor_name,
                approval_request_id, approval_step_no)
               VALUES ('{}', 'approval_request', 'planned', 'planned', 1, 'user', '{requester}', 'rita', '{request}', 1)",
            f.instance
        ),
    )
    .await;
    Approval { request, requester, principal, delegate, delegation, version: v2 }
}

/// A v0.4.0-S6 install with 100,000 workflow events: 0051 changes no table
/// file (no rewrite), adds its CHECK constraints without validating them, and
/// 0052 validates them apart. The audit actions match the server's.
#[tokio::test]
async fn the_upgrade_rewrites_nothing_and_validates_apart() {
    let Some(db) = scratch::empty("approvals_upgrade_rewrites_nothing").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(50, pool).await.expect("migrations up to 0050");
    let class = id(pool, "SELECT id FROM ci_classes WHERE system_role = 'business_service'").await;
    let ci =
        id(pool, &format!("INSERT INTO configuration_items (class_id, label) VALUES ('{class}', 'one') RETURNING id"))
            .await;
    let f = workflow_fixture(pool, "lifecycle", class, ci, None).await;
    ok(
        pool,
        &format!(
            "INSERT INTO workflow_instance_events (instance_id, kind, transition_key, from_state_key, to_state_key,
               to_version_no, actor_type, actor_name)
             SELECT '{}', 'transition', 'finish', 'planned', 'done', 1, 'user', 'bulk' FROM generate_series(1, 100000);
             INSERT INTO audit_log (actor_type, actor_name, action, entity_type, entity_id, old_value, new_value)
             VALUES ('user', 'alice', 'workflow.transition', 'configuration_items', '{ci}', '{{}}', '{{}}')",
            f.instance
        ),
    )
    .await;
    let events = filenode(pool, "workflow_instance_events").await;
    let audit = filenode(pool, "audit_log").await;
    let archive = filenode(pool, "workflow_instance_archive").await;

    MIGRATOR.run_to(51, pool).await.expect("migration 0051");
    for (table, c) in [
        ("workflow_instance_events", "workflow_instance_events_kind_check"),
        ("workflow_instance_events", "workflow_instance_events_approval"),
        ("audit_log", "audit_log_action_valid"),
        ("audit_log", "audit_log_values_present"),
    ] {
        assert!(!validated(pool, table, c).await, "{c} is NOT VALID after 0051");
    }
    MIGRATOR.run_to(52, pool).await.expect("migration 0052");
    for (table, c) in [
        ("workflow_instance_events", "workflow_instance_events_kind_check"),
        ("workflow_instance_events", "workflow_instance_events_approval"),
        ("audit_log", "audit_log_action_valid"),
        ("audit_log", "audit_log_values_present"),
    ] {
        assert!(validated(pool, table, c).await, "{c} is validated by 0052");
    }
    assert_eq!(filenode(pool, "workflow_instance_events").await, events, "the events table was not rewritten");
    assert_eq!(filenode(pool, "audit_log").await, audit, "the audit log was not rewritten");
    assert_eq!(filenode(pool, "workflow_instance_archive").await, archive, "the archive was not rewritten");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_instance_events").fetch_one(pool).await.unwrap();
    assert_eq!(n, 100_002);
    let kept: (Option<Uuid>, Option<i16>, Option<String>) = sqlx::query_as(
        "SELECT approval_request_id, approval_step_no, on_behalf_of_name FROM workflow_instance_events LIMIT 1",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(kept, (None, None, None), "existing events have no approval columns");

    // The audit constraint accepts exactly the server's actions, approvals as events (new value only).
    let actions = literals(&constraint_def(pool, "audit_log", "audit_log_action_valid").await);
    for a in APPROVAL_ACTIONS {
        assert!(actions.contains(a.as_str()), "{}", a.as_str());
        let row = |old: &str| {
            format!(
                "INSERT INTO audit_log (actor_type, actor_name, action, entity_type, entity_id, old_value, new_value)
                 VALUES ('user', 'alice', '{}', 'configuration_items', '{ci}', {old}, '{{}}')",
                a.as_str()
            )
        };
        ok(pool, &row("NULL")).await;
        refused(pool, &row("'{}'"), CHECK).await;
    }
    for kept in ["create", "export", "workflow.force", "workflow.publish", "session.reauthenticate"] {
        assert!(actions.contains(kept), "{kept}");
    }
    let prune: String = sqlx::query_scalar("SELECT prosrc FROM pg_proc WHERE proname = 'prune_audit_log'")
        .fetch_one(pool)
        .await
        .unwrap();
    assert!(APPROVAL_ACTIONS.iter().all(|a| prune.contains(a.as_str())), "the approval actions are in a prune scope");
    for scope in ["auth", "changes"] {
        sqlx::query("SELECT * FROM prune_audit_log(interval '400 days', $1, true)")
            .bind(scope)
            .fetch_all(pool)
            .await
            .unwrap_or_else(|e| panic!("{scope}: {e}"));
    }

    // The approval kinds need their request; the others are unchanged.
    let event = |kind: &str, request: &str| {
        format!(
            "INSERT INTO workflow_instance_events (instance_id, kind, to_state_key, to_version_no, actor_type,
               approval_request_id)
             VALUES ('{}', '{kind}', 'planned', 1, 'user', {request})",
            f.instance
        )
    };
    for kind in ["approval_request", "approval_decision", "approval_withdraw", "approval_close", "approval_overdue"] {
        refused(pool, &event(kind, "NULL"), CHECK).await;
        ok(pool, &event(kind, "gen_random_uuid()")).await;
    }
    refused(pool, &event("approval_bogus", "gen_random_uuid()"), CHECK).await;
    ok(pool, &event("cancel", "NULL")).await;

    // Running the migrations again is a no-op.
    MIGRATOR.run(pool).await.expect("re-run");
    db.drop().await;
}

#[tokio::test]
async fn the_approval_tables_enforce_their_rules() {
    let Some(db) = scratch::database("approval_tables_enforce_their_rules").await else { return };
    let pool = &db.pool;
    let class = id(pool, "SELECT id FROM ci_classes WHERE system_role = 'business_service'").await;
    let ci = |label: &str| {
        format!("INSERT INTO configuration_items (class_id, label) VALUES ('{class}', '{label}') RETURNING id")
    };
    let ci1 = id(pool, &ci("one")).await;
    let f = workflow_fixture(pool, "lifecycle", class, ci1, None).await;
    let a = approval_fixture(pool, &f).await;
    let (request, v2) = (a.request, a.version);

    // A published version's approval steps never change; a draft's do.
    let step = |transition: Uuid, no: i32, key: &str| {
        format!(
            "INSERT INTO workflow_transition_approval_steps (transition_id, step_no, key, name)
             VALUES ('{transition}', {no}, '{key}', 'Step')"
        )
    };
    let msg = refused(pool, &step(f.finish, 1, "tech"), IMMUTABLE).await;
    assert!(msg.contains("only a draft"), "{msg}");
    let v2_finish = id(pool, &format!("SELECT id FROM workflow_transitions WHERE version_id = '{v2}'")).await;
    refused(pool, &step(v2_finish, 2, "tech"), IMMUTABLE).await;
    refused(
        pool,
        &format!(
            "UPDATE workflow_transition_approval_steps SET required_approvals = 1 WHERE transition_id = '{v2_finish}'"
        ),
        IMMUTABLE,
    )
    .await;
    refused(
        pool,
        &format!("DELETE FROM workflow_transition_approval_steps WHERE transition_id = '{v2_finish}'"),
        IMMUTABLE,
    )
    .await;
    refused(pool, "TRUNCATE workflow_transition_approval_steps", APPEND_ONLY).await;
    let v3 = id(
        pool,
        &format!(
            "INSERT INTO workflow_versions (definition_id, version_no, status) VALUES ('{}', 3, 'draft') RETURNING id",
            f.definition
        ),
    )
    .await;
    let s = |k: &str| {
        format!(
            "INSERT INTO workflow_states (version_id, key, name, category) VALUES ('{v3}', '{k}', '{k}', 'open') RETURNING id"
        )
    };
    let (s1, s2) = (id(pool, &s("a")).await, id(pool, &s("b")).await);
    let t3 = id(
        pool,
        &format!(
            "INSERT INTO workflow_transitions (version_id, key, name, from_state_id, to_state_id)
             VALUES ('{v3}', 'go', 'Go', '{s1}', '{s2}') RETURNING id"
        ),
    )
    .await;
    ok(pool, &step(t3, 1, "tech")).await;
    ok(pool, &step(t3, 2, "cab")).await;
    refused(pool, &step(t3, 3, "cab"), UNIQUE).await;
    refused(pool, &step(t3, 6, "six"), CHECK).await;
    refused(pool, &step(t3, 3, "Bad Key"), CHECK).await;
    // A rejecting step needs a due date; the due interval is bounded.
    let policy = |set: &str| {
        format!("UPDATE workflow_transition_approval_steps SET {set} WHERE transition_id = '{t3}' AND step_no = 1")
    };
    refused(pool, &policy("on_overdue = 'reject'"), CHECK).await;
    refused(pool, &policy("due_after = interval '5 minutes'"), CHECK).await;
    refused(pool, &policy("required_approvals = 21"), CHECK).await;
    ok(pool, &policy("due_after = interval '1 day', on_overdue = 'reject'")).await;
    ok(pool, &format!("DELETE FROM workflow_versions WHERE id = '{v3}'")).await;

    // Approver assignments: exactly one source, and it matches `source`.
    let assignment = |source: &str, cols: &str, vals: &str| {
        format!(
            "INSERT INTO workflow_approval_assignments (definition_id, transition_key, step_key, source{cols})
             VALUES ('{}', 'finish', 'cab', '{source}'{vals})",
            f.definition
        )
    };
    let user = a.requester;
    refused(pool, &assignment("user", "", ""), CHECK).await;
    refused(pool, &assignment("profile", ", user_id", &format!(", '{user}'")), CHECK).await;
    refused(pool, &assignment("user", ", user_id, service_owner_role", &format!(", '{user}', 'business'")), CHECK)
        .await;
    refused(pool, &assignment("service_owner", ", service_owner_role", ", 'janitor'"), CHECK).await;
    ok(pool, &assignment("user", ", user_id", &format!(", '{user}'"))).await;
    refused(pool, &assignment("user", ", user_id", &format!(", '{user}'")), UNIQUE).await;
    ok(pool, &assignment("service_owner", ", service_owner_role", ", 'business'")).await;
    ok(pool, &assignment("service_owner", ", service_owner_role", ", 'technical'")).await;
    refused(pool, &assignment("service_owner", ", service_owner_role", ", 'business'"), UNIQUE).await;

    // Delegations: not to oneself, a window of at most 90 days, revoked with a name.
    let delegation = |principal: Uuid, delegate: Uuid, window: &str| {
        format!(
            "INSERT INTO workflow_approval_delegations
               (principal_id, principal_name, delegate_id, delegate_name, starts_at, ends_at, created_by_name)
             VALUES ('{principal}', 'p', '{delegate}', 'd', {window}, 'test')"
        )
    };
    refused(pool, &delegation(a.principal, a.principal, "now(), now() + interval '1 day'"), CHECK).await;
    refused(pool, &delegation(a.principal, a.delegate, "now(), now()"), CHECK).await;
    refused(pool, &delegation(a.principal, a.delegate, "now(), now() - interval '1 day'"), CHECK).await;
    refused(pool, &delegation(a.principal, a.delegate, "now(), now() + interval '91 days'"), CHECK).await;
    ok(pool, &delegation(a.principal, a.delegate, "now(), now() + interval '90 days'")).await;
    // Created by the principal or a third user, never by the delegate for someone else (SHAA-1872 C2).
    let created_by = |by: Uuid| {
        delegation(a.principal, a.delegate, "now(), now() + interval '1 day'")
            .replace(", created_by_name)", ", created_by_name, created_by_id)")
            .replace("'test')", &format!("'test', '{by}')"))
    };
    refused(pool, &created_by(a.delegate), CHECK).await;
    ok(pool, &created_by(a.principal)).await;
    ok(pool, &created_by(a.requester)).await;
    refused(
        pool,
        &format!("UPDATE workflow_approval_delegations SET revoked_at = now() WHERE id = '{}'", a.delegation),
        CHECK,
    )
    .await;

    // Requests: at most one pending per instance; a closed one carries its reason.
    let req = |no: i32, status: &str, closed: &str| {
        format!(
            "INSERT INTO workflow_approval_requests (instance_id, version_id, transition_key, request_no, status,
               close_reason, closed_at, requested_by_name, excluded_user_ids)
             VALUES ('{}', '{}', 'finish', {no}, '{status}', {closed}, 'rita', ARRAY[gen_random_uuid()])",
            f.instance, f.version
        )
    };
    let msg = refused(pool, &req(2, "pending", "NULL, NULL"), UNIQUE).await;
    assert!(msg.contains("workflow_approval_requests_one_pending"), "{msg}");
    refused(pool, &req(1, "rejected", "'rejected', now()"), UNIQUE).await;
    refused(pool, &req(2, "rejected", "NULL, now()"), CHECK).await;
    refused(pool, &req(2, "rejected", "'rejected', NULL"), CHECK).await;
    ok(pool, &req(2, "rejected", "'rejected', now()")).await;
    refused(
        pool,
        &format!(
            "INSERT INTO workflow_approval_requests (instance_id, version_id, transition_key, request_no, status,
               close_reason, closed_at, requested_by_name, excluded_user_ids)
             VALUES ('{}', '{}', 'nope', 3, 'withdrawn', 'withdrawn', now(), 'rita', ARRAY[gen_random_uuid()])",
            f.instance, f.version
        ),
        "23503",
    )
    .await;
    refused(
        pool,
        &format!("UPDATE workflow_approval_requests SET excluded_user_ids = '{{}}' WHERE id = '{request}'"),
        CHECK,
    )
    .await;

    // Decisions: one vote per actor and per principal per step.
    let decide = |actor: Uuid, on_behalf: Option<(Uuid, Uuid)>| {
        let (who, delegation, name) = match on_behalf {
            Some((p, d)) => (format!("'{p}'"), format!("'{d}'"), "'p'".to_owned()),
            None => ("NULL".to_owned(), "NULL".to_owned(), "NULL".to_owned()),
        };
        format!(
            "INSERT INTO workflow_approval_decisions (request_id, step_no, decision, actor_id, actor_name, credential,
               on_behalf_of_id, on_behalf_of_name, delegation_id, via)
             VALUES ('{request}', 1, 'approve', '{actor}', 'x', 'session', {who}, {name}, {delegation}, '{{}}')"
        )
    };
    // `dee` already voted for `pat`: neither may vote again on this step, in person or for them.
    let msg = refused(pool, &decide(a.delegate, None), UNIQUE).await;
    assert!(msg.contains("workflow_approval_decisions_actor_uq"), "{msg}");
    let msg = refused(pool, &decide(a.principal, None), UNIQUE).await;
    assert!(msg.contains("workflow_approval_decisions_principal_uq"), "{msg}");
    // A delegated vote names its principal and delegation together.
    refused(pool, &decide(a.requester, Some((a.principal, a.delegation))).replace("'p', '", "NULL, '"), CHECK).await;
    ok(pool, &decide(Uuid::new_v4(), None)).await;
    // A token decision names its token; a session decision has none (SHAA-1872 C3).
    let by = |credential: &str, token: &str, creator: &str| {
        decide(Uuid::new_v4(), None)
            .replace("credential,", "credential, token_id, token_creator_id,")
            .replace("'session',", &format!("'{credential}', {token}, {creator},"))
    };
    refused(pool, &by("token", "NULL", "NULL"), CHECK).await;
    refused(pool, &by("session", "gen_random_uuid()", "NULL"), CHECK).await;
    refused(pool, &by("session", "NULL", "gen_random_uuid()"), CHECK).await;
    ok(pool, &by("token", "gen_random_uuid()", "NULL")).await;
    ok(pool, &by("token", "gen_random_uuid()", "gen_random_uuid()")).await;
    // A decision on a step that does not exist is refused.
    refused(pool, &decide(Uuid::new_v4(), None).replace(", 1, 'approve'", ", 2, 'approve'"), "23503").await;

    // Decisions are append-only for everyone, the owner included.
    for sql in [
        format!("UPDATE workflow_approval_decisions SET comment = 'edited' WHERE request_id = '{request}'"),
        format!("DELETE FROM workflow_approval_decisions WHERE request_id = '{request}'"),
        "TRUNCATE workflow_approval_decisions".to_owned(),
    ] {
        let msg = refused(pool, &sql, APPEND_ONLY).await;
        assert!(msg.contains("workflow_approval_decisions is append-only"), "{msg}");
    }
    // The events keep their own message.
    let msg = refused(pool, "UPDATE workflow_instance_events SET comment = 'x'", APPEND_ONLY).await;
    assert!(msg.contains("workflow_instance_events is append-only"), "{msg}");
    // Nor do the request, its steps or the instance go while decisions refer to them.
    refused(pool, &format!("DELETE FROM workflow_approval_request_steps WHERE request_id = '{request}'"), "23503")
        .await;
    refused(pool, &format!("DELETE FROM workflow_approval_requests WHERE id = '{request}'"), "23503").await;
    refused(pool, &format!("DELETE FROM workflow_instances WHERE id = '{}'", f.instance), "23503").await;
    // A delegation a decision was made through cannot be deleted (it is revoked instead).
    refused(pool, &format!("DELETE FROM workflow_approval_delegations WHERE id = '{}'", a.delegation), "23503").await;
    db.drop().await;
}

/// Deleting a user for good (`delete_user`) keeps every delegation they were
/// part of, used or not, with their name; the decision made through one
/// stays as it was.
#[tokio::test]
async fn deleting_a_user_keeps_the_delegation_history() {
    let Some(db) = scratch::database("deleting_a_user_keeps_the_delegation_history").await else { return };
    let pool = &db.pool;
    let class = id(pool, "SELECT id FROM ci_classes WHERE system_role = 'business_service'").await;
    let ci =
        id(pool, &format!("INSERT INTO configuration_items (class_id, label) VALUES ('{class}', 'one') RETURNING id"))
            .await;
    let f = workflow_fixture(pool, "lifecycle", class, ci, None).await;
    let a = approval_fixture(pool, &f).await;
    // An unused delegation: `una` to `ulf`, never acted on. `ulf` goes.
    let (una, ulf) = (user(pool, "una").await, user(pool, "ulf").await);
    let unused = id(
        pool,
        &format!(
            "INSERT INTO workflow_approval_delegations
               (principal_id, principal_name, delegate_id, delegate_name, starts_at, ends_at, created_by_id, created_by_name)
             VALUES ('{una}', 'una', '{ulf}', 'ulf', now(), now() + interval '30 days', '{una}', 'una') RETURNING id"
        ),
    )
    .await;
    // `pat`, the principal of the used delegation, is assigned as an approver too.
    ok(
        pool,
        &format!(
            "INSERT INTO workflow_approval_assignments (definition_id, transition_key, step_key, source, user_id)
             VALUES ('{}', 'finish', 'cab', 'user', '{}')",
            f.definition, a.principal
        ),
    )
    .await;

    let mut c = pool.acquire().await.unwrap();
    crate::data::auth::delete_user(&mut c, a.principal).await.expect("the principal of a used delegation is deleted");
    crate::data::auth::delete_user(&mut c, ulf).await.expect("the delegate of an unused delegation is deleted");
    drop(c);

    type Row = (Option<Uuid>, String, Option<Uuid>, String, Option<Uuid>, String);
    let row = |d: Uuid| async move {
        sqlx::query_as::<_, Row>(
            "SELECT principal_id, principal_name, delegate_id, delegate_name, created_by_id, created_by_name
             FROM workflow_approval_delegations WHERE id = $1",
        )
        .bind(d)
        .fetch_one(pool)
        .await
        .unwrap()
    };
    assert_eq!(
        row(a.delegation).await,
        (None, "pat".into(), Some(a.delegate), "dee".into(), None, "pat".into()),
        "the used delegation keeps the deleted principal's name"
    );
    assert_eq!(
        row(unused).await,
        (Some(una), "una".into(), None, "ulf".into(), Some(una), "una".into()),
        "the unused delegation keeps the deleted delegate's name"
    );
    let decision: (Uuid, Option<Uuid>, Option<String>, Option<Uuid>) = sqlx::query_as(
        "SELECT actor_id, on_behalf_of_id, on_behalf_of_name, delegation_id FROM workflow_approval_decisions
         WHERE request_id = $1",
    )
    .bind(a.request)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(decision, (a.delegate, Some(a.principal), Some("pat".into()), Some(a.delegation)));
    // Their assignment went with them, like a business service owner.
    let assigned: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_approval_assignments WHERE source = 'user'")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(assigned, 0);
    // The requester can go as well; the request keeps their name.
    let mut c = pool.acquire().await.unwrap();
    crate::data::auth::delete_user(&mut c, a.requester).await.expect("the requester is deleted");
    drop(c);
    let requester: (Option<Uuid>, String) =
        sqlx::query_as("SELECT requested_by_id, requested_by_name FROM workflow_approval_requests WHERE id = $1")
            .bind(a.request)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(requester, (None, "rita".into()));
    db.drop().await;
}

/// On a three-role install the API role may read and append decisions but not
/// change or remove them, and the other approval tables are ordinary DML
/// tables. Deleting a CI row as the API role still archives the approvals of
/// its instances (the trigger runs as the owner) and leaves none behind.
#[tokio::test]
async fn the_api_role_only_appends_decisions_and_a_purge_archives_approvals() {
    let Some(roles) = scratch::Roles::create("the_api_role_only_appends_decisions").await else { return };
    let db = roles.database().await;
    let owner = &db.pool;
    let privilege = |table: &str, privilege: &str| {
        let (role, table, privilege) = (roles.app.clone(), table.to_owned(), privilege.to_owned());
        async move {
            sqlx::query_scalar::<_, bool>("SELECT has_table_privilege($1, $2, $3)")
                .bind(role)
                .bind(format!("cmdb.{table}"))
                .bind(privilege)
                .fetch_one(owner)
                .await
                .unwrap()
        }
    };
    for p in ["SELECT", "INSERT"] {
        assert!(privilege("workflow_approval_decisions", p).await, "{p}");
    }
    for p in ["UPDATE", "DELETE", "TRUNCATE"] {
        assert!(!privilege("workflow_approval_decisions", p).await, "{p}");
    }
    for t in TABLES.iter().filter(|t| **t != "workflow_approval_decisions") {
        for p in ["SELECT", "INSERT", "UPDATE", "DELETE"] {
            assert!(privilege(t, p).await, "{t} {p}");
        }
    }

    let class = id(owner, "SELECT id FROM ci_classes WHERE system_role = 'business_service'").await;
    let ci =
        id(owner, &format!("INSERT INTO configuration_items (class_id, label) VALUES ('{class}', 'one') RETURNING id"))
            .await;
    let f = workflow_fixture(owner, "lifecycle", class, ci, None).await;
    let a = approval_fixture(owner, &f).await;
    let api = roles.api_pool(&db).await;
    let request = a.request;
    let (otto, token) = (Uuid::new_v4(), Uuid::new_v4());
    ok(
        &api,
        &format!(
            "INSERT INTO cmdb.workflow_approval_decisions (request_id, step_no, decision, actor_id, actor_name,
               credential, token_id, token_creator_id, via, comment)
             VALUES ('{request}', 1, 'reject', '{otto}', 'otto', 'token', '{token}', '{otto}', '{{}}', 'No')"
        ),
    )
    .await;
    for sql in [
        format!("UPDATE cmdb.workflow_approval_decisions SET comment = 'x' WHERE request_id = '{request}'"),
        format!("DELETE FROM cmdb.workflow_approval_decisions WHERE request_id = '{request}'"),
        "TRUNCATE cmdb.workflow_approval_decisions".to_owned(),
    ] {
        refused(&api, &sql, APPEND_ONLY).await;
    }
    // Setting the archive variable does not open the decisions to the API role.
    let mut c = api.acquire().await.unwrap();
    sqlx::query("BEGIN").execute(&mut *c).await.unwrap();
    sqlx::query("SELECT set_config('shadoucmdb.workflow_archive', 'on', true)").execute(&mut *c).await.unwrap();
    let err = sqlx::query("DELETE FROM cmdb.workflow_approval_decisions WHERE request_id = $1")
        .bind(request)
        .execute(&mut *c)
        .await
        .unwrap_err();
    assert_eq!(err.as_database_error().and_then(|d| d.code()).as_deref(), Some(APPEND_ONLY));
    sqlx::query("ROLLBACK").execute(&mut *c).await.unwrap();

    // Deleting the CI row moves the instance with its events and approvals.
    sqlx::query("DELETE FROM cmdb.configuration_items WHERE id = $1").bind(ci).execute(&mut *c).await.unwrap();
    let (events, approvals): (Value, Value) =
        sqlx::query_as("SELECT events, approvals FROM cmdb.workflow_instance_archive WHERE instance_id = $1")
            .bind(f.instance)
            .fetch_one(&mut *c)
            .await
            .unwrap();
    let kinds: Vec<&str> = events.as_array().unwrap().iter().map(|e| e["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["start", "transition", "approval_request"]);
    assert_eq!(events[2]["approvalRequestId"], request.to_string());
    assert_eq!(events[2]["approvalStepNo"], 1);
    let requests = approvals.as_array().unwrap();
    assert_eq!(requests.len(), 1);
    let r = &requests[0];
    assert_eq!((r["id"].as_str(), r["status"].as_str()), (Some(request.to_string().as_str()), Some("pending")));
    assert_eq!(r["requestedByName"], "rita");
    assert_eq!(r["stagedFields"], serde_json::json!({"owner": "x"}));
    assert_eq!(r["excludedUserIds"], serde_json::json!([a.requester]));
    let step = &r["steps"][0];
    assert_eq!((step["stepKey"].as_str(), step["requiredApprovals"].as_i64()), (Some("cab"), Some(2)));
    let decisions = step["decisions"].as_array().unwrap();
    let who: Vec<(&str, &str, Option<&str>)> = decisions
        .iter()
        .map(|d| (d["decision"].as_str().unwrap(), d["actorName"].as_str().unwrap(), d["onBehalfOfName"].as_str()))
        .collect();
    assert_eq!(who, [("approve", "dee", Some("pat")), ("reject", "otto", None)]);
    assert_eq!(decisions[0]["delegationId"], a.delegation.to_string());
    assert_eq!(decisions[0]["requestId"], "req-2");
    assert_eq!((decisions[0]["tokenId"].is_null(), decisions[1]["credential"].as_str()), (true, Some("token")));
    assert_eq!(decisions[1]["tokenId"], token.to_string());
    assert_eq!(decisions[1]["tokenCreatorId"], otto.to_string());
    let left: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM cmdb.workflow_approval_requests)
              + (SELECT count(*) FROM cmdb.workflow_approval_request_steps)
              + (SELECT count(*) FROM cmdb.workflow_approval_eligibility)
              + (SELECT count(*) FROM cmdb.workflow_approval_decisions)
              + (SELECT count(*) FROM cmdb.workflow_instance_events)
              + (SELECT count(*) FROM cmdb.workflow_instances)",
    )
    .fetch_one(&mut *c)
    .await
    .unwrap();
    assert_eq!(left, 0, "nothing of the instance is left behind");
    // The delegation is not the CI's: it stays.
    let kept: i64 =
        sqlx::query_scalar("SELECT count(*) FROM cmdb.workflow_approval_delegations").fetch_one(&mut *c).await.unwrap();
    assert_eq!(kept, 1);
    drop(c);
    api.close().await;
    db.drop().await;
    roles.drop().await;
}
