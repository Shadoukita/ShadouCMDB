//! Approval delegation and the SLA sweep through the real router against
//! PostgreSQL (approvals design SHAA-1869 §6, §7; slice A4, SHAA-2643): when a
//! delegation qualifies and what it never lends, one vote per principal,
//! administrator delegations (SHAA-1872 C2), the overdue sweep with two
//! "server processes" on one database, escalation, `onOverdue: reject`,
//! understaffed steps and re-resolution after a staffing change.

use chrono::{Duration, Utc};
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use super::approvals_runtime_tests::{
    REQUESTS, audit_ok, decide, graph, pending, publish, reason, refused, request, setup, started, token,
};
use super::runtime::sweep;
use super::runtime_tests::{DEFS, RUN, World, details, id, pairs, world};
use crate::db::scratch;
use crate::modules::api_tokens::tests::{Creds, code};

const MINE: &str = "/api/v1/me/approval-delegations";
const ADMIN: &str = "/api/v1/admin/approval-delegations";

/// A delegation window from `from` to `to` hours from now.
fn window(from: i64, to: i64) -> (String, String) {
    let now = Utc::now();
    ((now + Duration::hours(from)).to_rfc3339(), (now + Duration::hours(to)).to_rfc3339())
}

/// Delegates `creds`' approvals to `to` for the window `from`..`to` hours from now.
async fn delegate(w: &World, creds: &Creds, to: Uuid, from: i64, until: i64) -> (u16, Value) {
    let (starts, ends) = window(from, until);
    w.call(creds, "POST", MINE, Some(json!({ "delegateUserId": to, "startsAt": starts, "endsAt": ends }))).await
}

async fn delegated(w: &World, creds: &Creds, to: Uuid) -> Uuid {
    let (status, v) = delegate(w, creds, to, -1, 24).await;
    assert_eq!(status, 201, "{v}");
    id(&v)
}

/// A decision on the active step for `on_behalf_of`.
async fn decide_for(w: &World, creds: &Creds, instance: Uuid, on_behalf_of: Option<Uuid>) -> (u16, Value) {
    let (request, version, step) = pending(w, instance).await;
    let body =
        json!({ "stepKey": step, "decision": "approve", "expectedVersion": version, "onBehalfOf": on_behalf_of });
    w.call(creds, "POST", &format!("{REQUESTS}/{request}/decisions"), Some(body)).await
}

/// The caller's inbox: the request ids and `page.total`.
async fn inbox(w: &World, creds: &Creds) -> (Vec<Uuid>, i64) {
    let (status, v) = w.call(creds, "GET", &format!("{REQUESTS}?view=actionable"), None).await;
    assert_eq!(status, 200, "{v}");
    (v["data"].as_array().unwrap().iter().map(id).collect(), v["page"]["total"].as_i64().unwrap())
}

async fn scalar_i64(w: &World, sql: &str, id: Uuid) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned())).bind(id).fetch_one(&w.pool).await.unwrap()
}

async fn set_active(w: &World, user: Uuid, active: bool) {
    sqlx::query("UPDATE users SET is_active = $2 WHERE id = $1")
        .bind(user)
        .bind(active)
        .execute(&w.pool)
        .await
        .unwrap();
}

/// A delegation qualifies only in its window, unrevoked, with both accounts
/// active; it lends the principal's own eligibility and nothing else: no
/// visibility, no way around four-eyes, and no second vote.
#[tokio::test]
async fn delegations_lend_a_principals_approvals_and_nothing_more() {
    let Some(db) = scratch::database("approval_delegations").await else { return };
    let w = world(&db).await;
    let p = setup(&w).await;
    let viewers = w.profile("Viewers", &[(w.server, false)]).await;
    let networks = w.profile("Networks", &[(w.network, false)]).await;
    let dee = w.user("dee", &[viewers]).await;
    let dan = w.user("dan", &[viewers]).await;
    let eve = w.user("eve", &[viewers]).await;
    let nov = w.user("nov", &[networks]).await;
    let (_, instance) = started(&w).await;
    let (status, v) = request(&w, &p.req.0, instance, "approve", json!({ "owner_team": "ops" })).await;
    assert_eq!(status, 202, "{v}");
    let (request_id, _, _) = pending(&w, instance).await;
    let path = format!("{REQUESTS}/{request_id}");
    let not_eligible = refused("FORBIDDEN", "not_eligible");

    // Scheduled: not yet.
    let (status, v) = delegate(&w, &p.tech.0, dee.1, 24, 48).await;
    assert_eq!((status, v["status"].as_str()), (201, Some("scheduled")), "{v}");
    let (status, v) = decide(&w, &dee.0, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, not_eligible.clone()), "{v}");
    assert_eq!(inbox(&w, &dee.0).await, (vec![], 0));

    // Active: the delegate sees that they may decide for tech, and the inbox lists it.
    let live = delegated(&w, &p.tech.0, dee.1).await;
    let (status, v) = w.call(&dee.0, "GET", &path, None).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        v["myEligibility"],
        json!({ "canDecide": true, "inPerson": false, "reason": null, "message": null,
                "onBehalfOf": [ { "userId": p.tech.1, "name": "tech", "delegationId": live } ] })
    );
    assert_eq!(inbox(&w, &dee.0).await, (vec![request_id], 1));

    // Revoked: no longer. Revoking twice is a conflict.
    let (status, v) = w.call(&p.tech.0, "POST", &format!("{MINE}/{live}/revoke"), None).await;
    assert_eq!((status, v["status"].as_str(), v["revokedByName"].as_str()), (200, Some("revoked"), Some("tech")));
    let (status, v) = w.call(&p.tech.0, "POST", &format!("{MINE}/{live}/revoke"), None).await;
    assert_eq!((status, details(&v)), (409, pairs(&[("id", "revoked")])), "{v}");
    let (status, v) = decide(&w, &dee.0, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, not_eligible.clone()), "{v}");

    // Ended: no longer either.
    let ended = delegated(&w, &p.tech.0, dee.1).await;
    sqlx::query(
        "UPDATE workflow_approval_delegations SET starts_at = now() - interval '2 days', ends_at = now() - interval '1 \
         day' WHERE id = $1",
    )
    .bind(ended)
    .execute(&w.pool)
    .await
    .unwrap();
    let (status, v) = decide(&w, &dee.0, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, not_eligible.clone()), "{v}");

    // A disabled principal lends nothing; a disabled delegate cannot even ask.
    let live = delegated(&w, &p.tech.0, dee.1).await;
    set_active(&w, p.tech.1, false).await;
    let (status, v) = decide(&w, &dee.0, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, not_eligible.clone()), "{v}");
    assert_eq!(inbox(&w, &dee.0).await.1, 0);
    set_active(&w, p.tech.1, true).await;
    set_active(&w, dee.1, false).await;
    let (status, _) = w.call(&dee.0, "GET", &path, None).await;
    assert_eq!(status, 401);
    set_active(&w, dee.1, true).await;

    // In its window: dee decides step 1 for tech (the only principal, so no onBehalfOf needed).
    let (status, v) = decide(&w, &dee.0, instance, "approve", Some("for tech, on leave")).await;
    assert_eq!(status, 200, "{v}");
    let cast = &v["request"]["steps"][0]["decisions"][0];
    assert_eq!((cast["actorName"].as_str(), cast["onBehalfOfName"].as_str()), (Some("dee"), Some("tech")));
    let row: (Uuid, Option<Uuid>, Option<String>, Option<Uuid>) = sqlx::query_as(
        "SELECT actor_id, on_behalf_of_id, on_behalf_of_name, delegation_id FROM workflow_approval_decisions
         WHERE request_id = $1",
    )
    .bind(request_id)
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!(row, (dee.1, Some(p.tech.1), Some("tech".into()), Some(live)));
    let event: Option<String> = sqlx::query_scalar(
        "SELECT on_behalf_of_name FROM workflow_instance_events WHERE instance_id = $1 AND kind = 'approval_decision'",
    )
    .bind(instance)
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!(event.as_deref(), Some("tech"));
    let (_, v) = w.call(&w.admin, "GET", &format!("{RUN}/{instance}/events"), None).await;
    let shown = v["data"].as_array().unwrap().iter().find(|e| e["kind"] == "approval_decision").unwrap();
    assert_eq!(shown["onBehalfOfName"], "tech", "{v}");
    let audit: Value = sqlx::query_scalar(
        "SELECT new_value FROM audit_log WHERE action = 'workflow.approval_decide' AND new_value->>'requestId' = $1",
    )
    .bind(request_id.to_string())
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!(
        (&audit["onBehalfOf"], &audit["delegationId"]),
        (&json!({ "id": p.tech.1, "name": "tech" }), &json!(live))
    );

    // Step 2 (CAB, two approvals). dee approved step 1, for tech, so dee may not approve step 2 for a3.
    delegated(&w, &p.a3.0, dee.1).await;
    let (status, v) = decide_for(&w, &dee.0, instance, Some(p.a3.1)).await;
    assert_eq!((status, reason(&v)), (403, refused("WORKFLOW_APPROVAL_SELF", "earlier_step")), "{v}");
    // dan holds two delegations: say for whom.
    delegated(&w, &p.a1.0, dan.1).await;
    delegated(&w, &p.a2.0, dan.1).await;
    let (status, v) = decide_for(&w, &dan.0, instance, None).await;
    assert_eq!((status, details(&v)), (400, pairs(&[("onBehalfOf", "required")])), "{v}");
    let (status, v) = decide_for(&w, &dan.0, instance, Some(p.a1.1)).await;
    assert_eq!((status, v["instance"]["pendingApproval"]["approvals"].as_i64()), (200, Some(1)), "{v}");
    // One vote per principal and one per person, in person or delegated.
    let (status, v) = decide(&w, &p.a1.0, instance, "approve", None).await;
    assert_eq!((status, details(&v)), (409, pairs(&[("decision", "already_decided")])), "{v}");
    let (status, v) = decide_for(&w, &dan.0, instance, Some(p.a2.1)).await;
    assert_eq!((status, details(&v)), (409, pairs(&[("decision", "already_decided")])), "{v}");
    let unique = sqlx::query(
        "INSERT INTO workflow_approval_decisions (request_id, step_no, decision, actor_id, actor_name, credential, via)
         VALUES ($1, 2, 'approve', $2, 'a1', 'session', '{}')",
    )
    .bind(request_id)
    .bind(p.a1.1)
    .execute(&w.pool)
    .await
    .unwrap_err();
    assert!(unique.to_string().contains("workflow_approval_decisions_principal_uq"), "{unique}");

    // A delegate who may not view the type never sees the request.
    delegated(&w, &p.a2.0, nov.1).await;
    let (status, v) = w.call(&nov.0, "GET", &path, None).await;
    assert_eq!((status, code(&v)), (404, "NOT_FOUND"), "{v}");
    let (status, v) = decide_for(&w, &nov.0, instance, Some(p.a2.1)).await;
    assert_eq!((status, code(&v)), (404, "NOT_FOUND"), "{v}");
    assert_eq!(inbox(&w, &nov.0).await, (vec![], 0));
    // A principal who may not view it (blind is a CAB approver through a profile without servers) lends nothing.
    delegated(&w, &p.blind.0, eve.1).await;
    let (status, v) = decide_for(&w, &eve.0, instance, Some(p.blind.1)).await;
    assert_eq!((status, reason(&v)), (403, not_eligible.clone()), "{v}");
    assert_eq!(inbox(&w, &eve.0).await, (vec![], 0));
    // The requester's delegate never decides for the requester (req is also a CAB member).
    delegated(&w, &p.req.0, eve.1).await;
    let (status, v) = decide_for(&w, &eve.0, instance, Some(p.req.1)).await;
    assert_eq!((status, reason(&v)), (403, refused("WORKFLOW_APPROVAL_SELF", "on_behalf_of_requester")), "{v}");
    assert_eq!(inbox(&w, &eve.0).await, (vec![], 0));
    // Nor for someone no delegation covers.
    let (status, v) = decide_for(&w, &eve.0, instance, Some(p.a3.1)).await;
    assert_eq!((status, details(&v)), (403, pairs(&[("onBehalfOf", "no_delegation")])), "{v}");

    // a2 in person completes the quorum; the transition names who approved for whom.
    let (status, v) = decide(&w, &p.a2.0, instance, "approve", None).await;
    assert_eq!((status, v["instance"]["state"]["key"].as_str()), (200, Some("approved")), "{v}");
    let approvers: Value = sqlx::query_scalar(
        "SELECT new_value->'approvers' FROM audit_log WHERE action = 'workflow.transition'
         AND new_value->>'approvalRequestId' = $1",
    )
    .bind(request_id.to_string())
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!(
        approvers,
        json!([
            { "stepKey": "tech", "approvedBy": "dee", "onBehalfOf": "tech" },
            { "stepKey": "cab", "approvedBy": "dan", "onBehalfOf": "a1" },
            { "stepKey": "cab", "approvedBy": "a2", "onBehalfOf": null }
        ])
    );
    audit_ok(&w).await;
}

/// Administrators delegate for an absent user, never to themselves (SHAA-1872
/// C2, refused by the API and by the table), within the window and the cap;
/// either side may revoke, nobody else.
#[tokio::test]
async fn administrator_delegations_never_name_their_creator() {
    let Some(db) = scratch::database("approval_delegations_admin").await else { return };
    let w = world(&db).await;
    let p = setup(&w).await;
    let admin: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE username = 'admin'").fetch_one(&w.pool).await.unwrap();
    let (starts, ends) = window(-1, 24);
    let body = |principal: Uuid, delegate: Uuid| {
        json!({ "principalUserId": principal, "delegateUserId": delegate, "startsAt": starts, "endsAt": ends,
                "reason": "on sick leave" })
    };

    // Never to the administrator who makes it, and the table refuses such a row as well.
    let (status, v) = w.call(&w.admin, "POST", ADMIN, Some(body(p.a1.1, admin))).await;
    assert_eq!((status, details(&v)), (400, pairs(&[("delegateUserId", "creator")])), "{v}");
    let err = sqlx::query(
        "INSERT INTO workflow_approval_delegations
           (principal_id, principal_name, delegate_id, delegate_name, starts_at, ends_at, created_by_id, created_by_name)
         VALUES ($1, 'a1', $2, 'admin', now(), now() + interval '1 day', $2, 'admin')",
    )
    .bind(p.a1.1)
    .bind(admin)
    .execute(&w.pool)
    .await
    .unwrap_err();
    assert!(err.to_string().contains("workflow_approval_delegations_not_creator"), "{err}");

    // Bad bodies.
    let (status, v) = w.call(&w.admin, "POST", ADMIN, Some(body(p.a1.1, p.a1.1))).await;
    assert_eq!((status, details(&v)), (400, pairs(&[("delegateUserId", "self")])), "{v}");
    let (status, v) = w.call(&w.admin, "POST", ADMIN, Some(body(Uuid::new_v4(), p.a2.1))).await;
    assert_eq!((status, details(&v)), (400, pairs(&[("principalUserId", "unknown")])), "{v}");
    let (long_start, long_end) = window(0, 91 * 24);
    let mut b = body(p.a1.1, p.a2.1);
    (b["startsAt"], b["endsAt"]) = (json!(long_start), json!(long_end));
    let (status, v) = w.call(&w.admin, "POST", ADMIN, Some(b)).await;
    assert_eq!((status, details(&v)), (400, pairs(&[("endsAt", "out_of_range")])), "{v}");
    let (past_start, past_end) = window(-48, -24);
    let mut b = body(p.a1.1, p.a2.1);
    (b["startsAt"], b["endsAt"]) = (json!(past_start), json!(past_end));
    let (status, v) = w.call(&w.admin, "POST", ADMIN, Some(b)).await;
    assert_eq!((status, details(&v)), (400, pairs(&[("endsAt", "in_past")])), "{v}");
    let mut b = body(p.a1.1, p.a2.1);
    b["definitionKey"] = json!("no_such_workflow");
    let (status, v) = w.call(&w.admin, "POST", ADMIN, Some(b)).await;
    assert_eq!((status, details(&v)), (400, pairs(&[("definitionKey", "unknown")])), "{v}");
    // Only users.manage holders, and only in a signed-in session.
    let (status, _) = w.call(&p.a1.0, "POST", ADMIN, Some(body(p.a2.1, p.a3.1))).await;
    assert_eq!(status, 403);
    let (a1_token, _) = token(&w, p.a1.1, w.approvers, Some(p.a1.1)).await;
    let (status, v) = delegate(&w, &a1_token, p.a2.1, 0, 24).await;
    assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{v}");

    // Made for a1, to a2, limited to the workflow: both sides see it, with who made it.
    let mut b = body(p.a1.1, p.a2.1);
    b["definitionKey"] = json!("server_lifecycle");
    let (status, made) = w.call(&w.admin, "POST", ADMIN, Some(b)).await;
    assert_eq!(status, 201, "{made}");
    assert_eq!(
        (&made["createdBy"]["name"], &made["definitionKey"], &made["status"], &made["scoped"]),
        (&json!("admin"), &json!("server_lifecycle"), &json!("active"), &json!(true))
    );
    let made_id = id(&made);
    let (_, v) = w.call(&p.a1.0, "GET", &format!("{MINE}?role=principal"), None).await;
    assert_eq!((v["page"]["total"].as_i64(), id(&v["data"][0])), (Some(1), made_id), "{v}");
    let (_, v) = w.call(&p.a2.0, "GET", &format!("{MINE}?role=delegate&active=true"), None).await;
    assert_eq!(v["page"]["total"], 1, "{v}");
    let (_, v) = w.call(&p.a3.0, "GET", MINE, None).await;
    assert_eq!(v["page"]["total"], 0, "{v}");
    let (_, v) = w.call(&w.admin, "GET", &format!("{ADMIN}?principal={}", p.a1.1), None).await;
    assert_eq!(v["page"]["total"], 1, "{v}");

    // At most five scheduled or active per principal.
    for to in [p.a3.1, p.tech.1, p.req2.1, p.blind.1] {
        let (status, v) = delegate(&w, &p.a1.0, to, 1, 24).await;
        assert_eq!(status, 201, "{v}");
    }
    let (status, v) = delegate(&w, &p.a1.0, p.req.1, 0, 24).await;
    assert_eq!((status, details(&v)), (409, pairs(&[("delegateUserId", "limit")])), "{v}");

    // Revoke: not by a stranger (it does not exist for them); the delegate may decline; then it is done.
    let (status, _) = w.call(&p.a3.0, "POST", &format!("{MINE}/{made_id}/revoke"), None).await;
    assert_eq!(status, 404);
    let (status, v) = w.call(&p.a2.0, "POST", &format!("{MINE}/{made_id}/revoke"), None).await;
    assert_eq!((status, v["status"].as_str()), (200, Some("revoked")), "{v}");
    let (status, v) = w.call(&w.admin, "POST", &format!("{ADMIN}/{made_id}/revoke"), None).await;
    assert_eq!((status, details(&v)), (409, pairs(&[("id", "revoked")])), "{v}");
    let (status, v) = delegate(&w, &p.a1.0, p.req.1, 0, 24).await;
    assert_eq!(status, 201, "a revoked one frees a place: {v}");

    // Audited: the creation by the administrator and the revocation, by name.
    let rows: Vec<(String, String, Option<Value>)> = sqlx::query_as(
        "SELECT action, actor_name, new_value FROM audit_log WHERE entity_type = 'workflow_approval_delegations'
         AND entity_id = $1 ORDER BY id",
    )
    .bind(made_id)
    .fetch_all(&w.pool)
    .await
    .unwrap();
    let summary: Vec<(&str, &str, &Value, &Value)> = rows
        .iter()
        .map(|(a, actor, v)| {
            let v = v.as_ref().unwrap();
            (a.as_str(), actor.as_str(), &v["delegate"]["name"], &v["revokedByName"])
        })
        .collect();
    assert_eq!(
        summary,
        [("create", "admin", &json!("a2"), &Value::Null), ("update", "a2", &json!("a2"), &json!("a2"))]
    );
    assert_eq!(rows[0].2.as_ref().unwrap()["definition"]["key"], "server_lifecycle");
    audit_ok(&w).await;
}

/// `approve`: technical review due in 15 minutes and flagged when overdue,
/// with an escalation approver; CAB rejected when overdue.
fn sla_graph() -> Value {
    let mut g = graph();
    let steps = &mut g["transitions"][0]["approval"]["steps"];
    steps[0]["dueAfter"] = json!("PT15M");
    steps[1]["dueAfter"] = json!("PT15M");
    steps[1]["onOverdue"] = json!("reject");
    g
}

async fn put_approvers(w: &World, approvers: Value) -> Value {
    let def = w.ok("GET", &format!("{DEFS}/{}", w.definition), json!(null)).await;
    w.ok(
        "PUT",
        &format!("{DEFS}/{}/approvers", w.definition),
        json!({ "version": def["version"], "approvers": approvers }),
    )
    .await
}

fn assignment(step: &str, role: &str, source: &str, name: &str) -> Value {
    json!({ "transitionKey": "approve", "stepKey": step, "role": role, "source": source, source: name })
}

/// Makes the active step of `request` due a minute ago.
async fn make_due(w: &World, request: Uuid) {
    sqlx::query(
        "UPDATE workflow_approval_request_steps SET due_at = now() - interval '1 minute'
         WHERE request_id = $1 AND status = 'active'",
    )
    .bind(request)
    .execute(&w.pool)
    .await
    .unwrap();
}

/// Two server processes sweep one database at once: an overdue step gets
/// exactly one `approval_overdue` event and audit row. Escalation approvers
/// may decide only then; `onOverdue: reject` closes the request.
#[tokio::test]
async fn the_sweep_marks_a_step_overdue_once_across_processes() {
    let Some(db) = scratch::database("approval_sweep").await else { return };
    let w = world(&db).await;
    let p = setup(&w).await;
    publish(&w, sla_graph()).await;
    let viewers = w.profile("Viewers", &[(w.server, false)]).await;
    let esc = w.user("esc", &[viewers]).await;
    put_approvers(
        &w,
        json!([
            assignment("tech", "approver", "profile", "Tech"),
            assignment("tech", "escalation", "user", "esc"),
            assignment("cab", "approver", "profile", "CAB"),
        ]),
    )
    .await;
    let (_, flagged) = started(&w).await;
    let (status, v) = request(&w, &p.req.0, flagged, "approve", json!({ "owner_team": "ops" })).await;
    assert_eq!(status, 202, "{v}");
    let (flagged_request, _, _) = pending(&w, flagged).await;
    let path = format!("{REQUESTS}/{flagged_request}");

    // Not overdue yet: the escalation approver may not decide, and the sweep does nothing.
    let (status, v) = decide(&w, &esc.0, flagged, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, refused("FORBIDDEN", "not_eligible")), "{v}");
    assert_eq!(inbox(&w, &esc.0).await.1, 0);
    assert_eq!(sweep::tick(&w.pool).await.unwrap().flagged, 0);

    // Due: two pools on the same database, sweeping at once, mark it once.
    make_due(&w, flagged_request).await;
    let other =
        PgPoolOptions::new().max_connections(4).connect_with((*w.pool.connect_options()).clone()).await.unwrap();
    let (a, b, c) = tokio::join!(sweep::tick(&w.pool), sweep::tick(&other), sweep::tick(&other));
    let flagged_count = a.unwrap().flagged + b.unwrap().flagged + c.unwrap().flagged;
    assert_eq!(flagged_count, 1);
    let events =
        "SELECT count(*) FROM workflow_instance_events WHERE kind = 'approval_overdue' AND approval_request_id = $1";
    let audits = "SELECT count(*) FROM audit_log WHERE action = 'workflow.approval_overdue' AND actor_type = 'system'
                  AND new_value->>'requestId' = $1::text";
    assert_eq!(scalar_i64(&w, events, flagged_request).await, 1);
    assert_eq!(scalar_i64(&w, audits, flagged_request).await, 1);
    let overdue: Value = sqlx::query_scalar(
        "SELECT new_value FROM audit_log WHERE action = 'workflow.approval_overdue' AND new_value->>'requestId' = $1",
    )
    .bind(flagged_request.to_string())
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!(
        (&overdue["reason"], &overdue["onOverdue"], &overdue["escalatedTo"], &overdue["stepKey"]),
        (&json!("due"), &json!("flag"), &json!(["user esc"]), &json!("tech"))
    );
    let (_, v) = w.call(&w.admin, "GET", &path, None).await;
    assert_eq!((v["status"].as_str(), v["steps"][0]["overdue"].as_bool()), (Some("pending"), Some(true)), "{v}");
    // Sweeping again changes nothing.
    assert_eq!(sweep::tick(&other).await.unwrap().flagged, 0);
    assert_eq!(scalar_i64(&w, events, flagged_request).await, 1);

    // Now the escalation approver may decide, and finds it in the inbox.
    assert_eq!(inbox(&w, &esc.0).await, (vec![flagged_request], 1));
    let (status, v) = decide(&w, &esc.0, flagged, "approve", None).await;
    assert_eq!((status, v["request"]["currentStepNo"].as_i64()), (200, Some(2)), "{v}");

    // onOverdue reject: the CAB step was not decided in time.
    make_due(&w, flagged_request).await;
    let r = sweep::tick(&other).await.unwrap();
    assert_eq!((r.flagged, r.rejected), (0, 1));
    let (_, v) = w.call(&w.admin, "GET", &path, None).await;
    assert_eq!(
        (v["status"].as_str(), v["closeReason"].as_str(), v["closedByName"].as_str(), v["steps"][1]["status"].as_str()),
        (Some("rejected"), Some("overdue"), Some("system"), Some("rejected")),
        "{v}"
    );
    let (_, v) = w.call(&w.admin, "GET", &format!("{RUN}/{flagged}"), None).await;
    assert_eq!(
        (v["instance"]["state"]["key"].as_str(), &v["instance"]["pendingApproval"]),
        (Some("planned"), &Value::Null)
    );
    let closes =
        "SELECT count(*) FROM audit_log WHERE action = 'workflow.approval_close' AND new_value->>'requestId' = $1::text
                  AND new_value->>'reason' = 'overdue'";
    assert_eq!(scalar_i64(&w, closes, flagged_request).await, 1);
    assert_eq!(scalar_i64(&w, events, flagged_request).await, 2, "one per step");
    other.close().await;
    audit_ok(&w).await;
}

/// A step nobody can staff raises the escalation seam once; changing the
/// workflow's approvers re-resolves pending steps at once, and the sweep
/// finishes what a cut-short re-resolution left.
#[tokio::test]
async fn staffing_changes_re_resolve_pending_steps() {
    let Some(db) = scratch::database("approval_staffing").await else { return };
    let w = world(&db).await;
    let p = setup(&w).await;
    let (_, instance) = started(&w).await;
    let (status, v) = request(&w, &p.req.0, instance, "approve", json!({ "owner_team": "ops" })).await;
    assert_eq!(status, 202, "{v}");
    let (request_id, _, _) = pending(&w, instance).await;
    let path = format!("{REQUESTS}/{request_id}");
    let base = |tech: &str| {
        json!([
            assignment("tech", "approver", "profile", tech),
            assignment("cab", "approver", "profile", "CAB"),
            assignment("cab", "approver", "profile", "Blind"),
        ])
    };

    // Only Blind, who may not view servers, for the technical review: understaffed.
    put_approvers(&w, base("Blind")).await;
    let (_, v) = w.call(&w.admin, "GET", &path, None).await;
    assert_eq!(
        (v["steps"][0]["eligibleCount"].as_i64(), v["steps"][0]["understaffed"].as_bool()),
        (Some(0), Some(true))
    );
    let (status, v) = decide(&w, &p.tech.0, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, refused("FORBIDDEN", "not_eligible")), "{v}");
    let refresh: Value = sqlx::query_scalar(
        "SELECT new_value FROM audit_log WHERE action = 'workflow.approval_refresh' AND actor_type = 'system'
         AND new_value->>'requestId' = $1",
    )
    .bind(request_id.to_string())
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!((&refresh["trigger"], &refresh["eligibleCount"]), (&json!("sweep"), &json!(0)));
    let raised = "SELECT count(*) FROM audit_log WHERE action = 'workflow.approval_overdue'
                  AND new_value->>'reason' = 'understaffed' AND new_value->>'requestId' = $1::text";
    assert_eq!(scalar_i64(&w, raised, request_id).await, 1);
    // The sweep looks at it again, but raises it only once.
    let r = sweep::tick(&w.pool).await.unwrap();
    assert_eq!((r.reresolved, r.changed, r.understaffed), (1, 0, 0));
    assert_eq!(scalar_i64(&w, raised, request_id).await, 1);

    // Staffed again: tech may decide.
    put_approvers(&w, base("Tech")).await;
    let (_, v) = w.call(&w.admin, "GET", &path, None).await;
    assert_eq!(
        (v["steps"][0]["eligibleCount"].as_i64(), v["steps"][0]["understaffed"].as_bool()),
        (Some(1), Some(false))
    );

    // A change whose re-resolution was cut short (here: written without it) is finished by the sweep.
    sqlx::query(
        "UPDATE workflow_approval_assignments SET profile_id = (SELECT id FROM permission_profiles WHERE name = 'CAB')
         WHERE definition_id = $1 AND step_key = 'tech'",
    )
    .bind(w.definition)
    .execute(&w.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE workflow_definitions SET version = version + 1 WHERE id = $1")
        .bind(w.definition)
        .execute(&w.pool)
        .await
        .unwrap();
    let r = sweep::tick(&w.pool).await.unwrap();
    assert_eq!((r.reresolved, r.changed), (1, 1));
    let (_, v) = w.call(&w.admin, "GET", &path, None).await;
    assert_eq!(v["steps"][0]["eligibleCount"], 3, "a1, a2, a3: {v}");
    let (status, v) = decide(&w, &p.a1.0, instance, "approve", None).await;
    assert_eq!(status, 200, "{v}");
    audit_ok(&w).await;
}
