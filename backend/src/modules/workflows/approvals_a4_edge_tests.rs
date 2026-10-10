//! Edge cases of approval delegation and the SLA sweep (slice A4, SHAA-2643)
//! beyond `approvals_a4_tests.rs`, from the QA pass SHAA-2724: the window at
//! its boundaries, a revocation while a request is pending, a delegate who is
//! the requester, disabled accounts at creation, the sweep racing itself and
//! a decision, and the overdue backlog an install has when it first runs the
//! sweep after the upgrade.

use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use super::approvals_runtime_tests::{
    REQUESTS, audit_ok, decide, graph, pending, publish, reason, refused, request, setup, started,
};
use super::runtime::sweep;
use super::runtime_tests::{World, details, id, pairs, world};
use crate::db::scratch;
use crate::modules::api_tokens::tests::Creds;

const MINE: &str = "/api/v1/me/approval-delegations";
const ADMIN: &str = "/api/v1/admin/approval-delegations";

async fn post_window(w: &World, creds: &Creds, to: Uuid, starts: String, ends: String) -> (u16, Value) {
    w.call(creds, "POST", MINE, Some(json!({ "delegateUserId": to, "startsAt": starts, "endsAt": ends }))).await
}

async fn delegated(w: &World, creds: &Creds, to: Uuid) -> Uuid {
    let now = Utc::now();
    let (status, v) =
        post_window(w, creds, to, (now - Duration::hours(1)).to_rfc3339(), (now + Duration::hours(24)).to_rfc3339())
            .await;
    assert_eq!(status, 201, "{v}");
    id(&v)
}

async fn decide_for(w: &World, creds: &Creds, instance: Uuid, on_behalf_of: Option<Uuid>) -> (u16, Value) {
    let (request, version, step) = pending(w, instance).await;
    let body =
        json!({ "stepKey": step, "decision": "approve", "expectedVersion": version, "onBehalfOf": on_behalf_of });
    w.call(creds, "POST", &format!("{REQUESTS}/{request}/decisions"), Some(body)).await
}

async fn inbox_total(w: &World, creds: &Creds) -> i64 {
    let (status, v) = w.call(creds, "GET", &format!("{REQUESTS}?view=actionable"), None).await;
    assert_eq!(status, 200, "{v}");
    v["page"]["total"].as_i64().unwrap()
}

async fn scalar_i64(w: &World, sql: &str, id: Uuid) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned())).bind(id).fetch_one(&w.pool).await.unwrap()
}

async fn exec(w: &World, sql: &str, id: Uuid) {
    sqlx::query(sqlx::AssertSqlSafe(sql.to_owned())).bind(id).execute(&w.pool).await.unwrap();
}

/// The `status` a delegation shows its principal.
async fn status_of(w: &World, creds: &Creds, delegation: Uuid) -> String {
    let (_, v) = w.call(creds, "GET", &format!("{MINE}?role=principal"), None).await;
    let row = v["data"].as_array().unwrap().iter().find(|d| id(d) == delegation).expect("listed");
    row["status"].as_str().unwrap().to_owned()
}

/// The window is `startsAt <= now < endsAt`, at most 90 days long, checked
/// to the microsecond the database stores; a request never gets a 500.
#[tokio::test]
async fn delegation_windows_hold_at_their_boundaries() {
    let Some(db) = scratch::database("approval_delegation_boundaries").await else { return };
    let w = world(&db).await;
    let p = setup(&w).await;
    let viewers = w.profile("Viewers", &[(w.server, false)]).await;
    let dee = w.user("dee", &[viewers]).await;
    let out_of_range = pairs(&[("endsAt", "out_of_range")]);
    let start = Utc::now() + Duration::days(1);

    // Exactly 90 days is allowed, a second more is not; nor an empty window.
    let (status, v) =
        post_window(&w, &p.a1.0, dee.1, start.to_rfc3339(), (start + Duration::days(90)).to_rfc3339()).await;
    assert_eq!((status, v["status"].as_str()), (201, Some("scheduled")), "{v}");
    let (status, v) = post_window(
        &w,
        &p.a2.0,
        dee.1,
        start.to_rfc3339(),
        (start + Duration::days(90) + Duration::seconds(1)).to_rfc3339(),
    )
    .await;
    assert_eq!((status, details(&v)), (400, out_of_range.clone()), "{v}");
    let (status, v) = post_window(&w, &p.a2.0, dee.1, start.to_rfc3339(), start.to_rfc3339()).await;
    assert_eq!((status, details(&v)), (400, out_of_range.clone()), "{v}");
    // Ends after it starts by less than the microsecond PostgreSQL keeps: refused, never a 500.
    // GH#822: today the refusal names the database constraint instead of `endsAt`/`out_of_range`.
    let base = DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z").unwrap();
    let (status, v) = post_window(
        &w,
        &p.a2.0,
        dee.1,
        base.format("%Y-%m-%dT%H:%M:%S.000000100Z").to_string(),
        base.format("%Y-%m-%dT%H:%M:%S.000000200Z").to_string(),
    )
    .await;
    assert_eq!(status, 400, "{v}");

    // tech -> dee, live: dee may decide step 1 for tech.
    let (_, instance) = started(&w).await;
    let (status, v) = request(&w, &p.req.0, instance, "approve", json!({ "owner_team": "ops" })).await;
    assert_eq!(status, 202, "{v}");
    let live = delegated(&w, &p.tech.0, dee.1).await;

    // A window that started at this very instant already applies.
    exec(&w, "UPDATE workflow_approval_delegations SET starts_at = clock_timestamp() WHERE id = $1", live).await;
    assert_eq!(status_of(&w, &p.tech.0, live).await, "active");
    assert_eq!(inbox_total(&w, &dee.0).await, 1);

    // A window that ended at this very instant no longer does, and cannot be revoked.
    exec(&w, "UPDATE workflow_approval_delegations SET ends_at = clock_timestamp() WHERE id = $1", live).await;
    assert_eq!(status_of(&w, &p.tech.0, live).await, "ended");
    assert_eq!(inbox_total(&w, &dee.0).await, 0);
    let (status, v) = decide(&w, &dee.0, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, refused("FORBIDDEN", "not_eligible")), "{v}");
    let (status, v) = w.call(&p.tech.0, "POST", &format!("{MINE}/{live}/revoke"), None).await;
    assert_eq!((status, details(&v)), (409, pairs(&[("id", "ended")])), "{v}");
    // An ended delegation frees its place under the cap.
    let (_, v) = w.call(&p.tech.0, "GET", &format!("{MINE}?role=principal&active=true"), None).await;
    assert_eq!(v["page"]["total"], 0, "{v}");
    audit_ok(&w).await;
}

/// Revoking a delegation while its request is pending: a vote already cast
/// through it stands and still binds the principal, the delegate can do no
/// more, and the request completes with others.
#[tokio::test]
async fn revoking_a_delegation_mid_request_keeps_the_votes_it_cast() {
    let Some(db) = scratch::database("approval_delegation_revoke_pending").await else { return };
    let w = world(&db).await;
    let p = setup(&w).await;
    let viewers = w.profile("Viewers", &[(w.server, false)]).await;
    let dan = w.user("dan", &[viewers]).await;
    let (_, instance) = started(&w).await;
    let (status, v) = request(&w, &p.req.0, instance, "approve", json!({ "owner_team": "ops" })).await;
    assert_eq!(status, 202, "{v}");
    let (request_id, _, _) = pending(&w, instance).await;
    let (status, v) = decide(&w, &p.tech.0, instance, "approve", None).await;
    assert_eq!(status, 200, "{v}");

    // CAB needs two: dan votes for a1, through a delegation a1 then revokes.
    let a1_to_dan = delegated(&w, &p.a1.0, dan.1).await;
    delegated(&w, &p.a2.0, dan.1).await;
    let (status, v) = decide_for(&w, &dan.0, instance, Some(p.a1.1)).await;
    assert_eq!((status, v["instance"]["pendingApproval"]["approvals"].as_i64()), (200, Some(1)), "{v}");
    let (status, v) = w.call(&p.a1.0, "POST", &format!("{MINE}/{a1_to_dan}/revoke"), None).await;
    assert_eq!((status, v["status"].as_str()), (200, Some("revoked")), "{v}");

    // The vote stands: the request still counts it, and a1 has had their say.
    let (status, v) = w.call(&w.admin, "GET", &format!("{REQUESTS}/{request_id}"), None).await;
    assert_eq!(status, 200, "{v}");
    let cast = &v["steps"][1]["decisions"][0];
    assert_eq!((cast["actorName"].as_str(), cast["onBehalfOfName"].as_str()), (Some("dan"), Some("a1")), "{v}");
    let (status, v) = decide(&w, &p.a1.0, instance, "approve", None).await;
    assert_eq!((status, details(&v)), (409, pairs(&[("decision", "already_decided")])), "{v}");
    // dan may no longer act for a1, and the inbox says so, while a2's delegation is untouched.
    let (status, v) = w.call(&dan.0, "GET", &format!("{REQUESTS}/{request_id}"), None).await;
    assert_eq!(status, 200, "{v}");
    let for_whom: Vec<&str> = v["myEligibility"]["onBehalfOf"]
        .as_array()
        .map(|a| a.iter().filter_map(|o| o["name"].as_str()).collect())
        .unwrap_or_default();
    assert!(!for_whom.contains(&"a1"), "{v}");
    let (status, v) = decide_for(&w, &dan.0, instance, Some(p.a1.1)).await;
    assert_eq!((status, details(&v)), (403, pairs(&[("onBehalfOf", "no_delegation")])), "{v}");
    // dan already cast a vote on this step, so not for a2 either: one vote per person.
    let (status, v) = decide_for(&w, &dan.0, instance, Some(p.a2.1)).await;
    assert_eq!((status, details(&v)), (409, pairs(&[("decision", "already_decided")])), "{v}");

    // a3 in person completes it; the transition still names who approved for whom.
    let (status, v) = decide(&w, &p.a3.0, instance, "approve", None).await;
    assert_eq!((status, v["instance"]["state"]["key"].as_str()), (200, Some("approved")), "{v}");
    let decided_through: Option<Uuid> = sqlx::query_scalar(
        "SELECT delegation_id FROM workflow_approval_decisions WHERE request_id = $1 AND actor_id = $2",
    )
    .bind(request_id)
    .bind(dan.1)
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!(decided_through, Some(a1_to_dan));
    audit_ok(&w).await;
}

/// `myEligibility` agrees with a decision once votes are cast on the active
/// step (GH#887): whoever cast a vote, and whoever one was cast for, may not
/// decide it again, and a principal already voted for is no longer offered to
/// their delegates. The inbox agrees with both.
#[tokio::test]
async fn my_eligibility_drops_whoever_already_decided_the_active_step() {
    let Some(db) = scratch::database("approval_eligibility_already_decided").await else { return };
    let w = world(&db).await;
    let p = setup(&w).await;
    let viewers = w.profile("Viewers", &[(w.server, false)]).await;
    let dan = w.user("dan", &[viewers]).await;
    let (_, instance) = started(&w).await;
    let (status, v) = request(&w, &p.req.0, instance, "approve", json!({ "owner_team": "ops" })).await;
    assert_eq!(status, 202, "{v}");
    let (request_id, _, _) = pending(&w, instance).await;
    let path = format!("{REQUESTS}/{request_id}");
    let eligibility = |v: &Value| {
        let mine = &v["myEligibility"];
        let for_whom: Vec<String> = mine["onBehalfOf"]
            .as_array()
            .map(|a| a.iter().filter_map(|o| o["name"].as_str().map(str::to_owned)).collect())
            .unwrap_or_default();
        (mine["canDecide"].as_bool(), mine["inPerson"].as_bool(), for_whom, mine["reason"].as_str().map(str::to_owned))
    };
    let already = (Some(false), Some(false), Vec::<String>::new(), Some("already_decided".to_owned()));

    // In person: tech decides step 1 (one approval), then CAB is active; tech is
    // done with step 1 but step 2 has its own rules (earlier_step).
    let (status, v) = w.call(&p.tech.0, "GET", &path, None).await;
    assert_eq!((status, eligibility(&v).0), (200, Some(true)), "{v}");
    let (status, v) = decide(&w, &p.tech.0, instance, "approve", None).await;
    assert_eq!(status, 200, "{v}");

    // Delegated: a1 and a2 lend dan their CAB approval; a2 lends a3 theirs too.
    delegated(&w, &p.a1.0, dan.1).await;
    delegated(&w, &p.a2.0, dan.1).await;
    delegated(&w, &p.a2.0, p.a3.1).await;
    let (_, v) = w.call(&dan.0, "GET", &path, None).await;
    assert_eq!(eligibility(&v), (Some(true), Some(false), vec!["a1".into(), "a2".into()], None), "{v}");
    let (_, v) = w.call(&p.a3.0, "GET", &path, None).await;
    assert_eq!(eligibility(&v), (Some(true), Some(true), vec!["a2".into()], None), "{v}");

    // a3 votes for a2: 1 of 2.
    let (status, v) = decide_for(&w, &p.a3.0, instance, Some(p.a2.1)).await;
    assert_eq!((status, v["instance"]["pendingApproval"]["approvals"].as_i64()), (200, Some(1)), "{v}");

    // a3 cast a vote: done, in person and for anyone; the decision path agrees.
    let (status, v) = w.call(&p.a3.0, "GET", &path, None).await;
    assert_eq!((status, eligibility(&v)), (200, already.clone()), "{v}");
    assert_eq!(v["myEligibility"]["message"], "You already decided step cab of this request", "{v}");
    let (status, v) = decide(&w, &p.a3.0, instance, "approve", None).await;
    assert_eq!((status, details(&v)), (409, pairs(&[("decision", "already_decided")])), "{v}");
    assert_eq!(inbox_total(&w, &p.a3.0).await, 0);
    // a2 had a vote cast for them: done as well.
    let (_, v) = w.call(&p.a2.0, "GET", &path, None).await;
    assert_eq!(eligibility(&v), already.clone(), "{v}");
    let (status, v) = decide(&w, &p.a2.0, instance, "approve", None).await;
    assert_eq!((status, details(&v)), (409, pairs(&[("decision", "already_decided")])), "{v}");
    assert_eq!(inbox_total(&w, &p.a2.0).await, 0);
    // dan may still act for a1, no longer for a2.
    let (_, v) = w.call(&dan.0, "GET", &path, None).await;
    assert_eq!(eligibility(&v), (Some(true), Some(false), vec!["a1".into()], None), "{v}");
    let (status, v) = decide_for(&w, &dan.0, instance, Some(p.a2.1)).await;
    assert_eq!((status, details(&v)), (409, pairs(&[("onBehalfOf", "already_decided")])), "{v}");
    assert_eq!(inbox_total(&w, &dan.0).await, 1);
    // a1 is untouched.
    let (_, v) = w.call(&p.a1.0, "GET", &path, None).await;
    assert_eq!(eligibility(&v), (Some(true), Some(true), Vec::<String>::new(), None), "{v}");

    // dan votes for a1: the quorum completes and the request closes.
    let (status, v) = decide_for(&w, &dan.0, instance, Some(p.a1.1)).await;
    assert_eq!((status, v["instance"]["state"]["key"].as_str()), (200, Some("approved")), "{v}");
    let (_, v) = w.call(&dan.0, "GET", &path, None).await;
    assert_eq!(v["myEligibility"]["reason"], "not_pending", "{v}");
    audit_ok(&w).await;
}

/// Four-eyes binds a delegate who made the request: they decide it neither
/// in person nor for anyone, see it in no inbox, and the reason says so.
/// Accounts disabled at creation time are refused on either side.
#[tokio::test]
async fn a_requester_never_decides_their_request_as_a_delegate() {
    let Some(db) = scratch::database("approval_delegation_requester").await else { return };
    let w = world(&db).await;
    let p = setup(&w).await;
    // req2 may request but approves nothing in person; tech and a1 lend req2 their approvals.
    let (_, instance) = started(&w).await;
    delegated(&w, &p.tech.0, p.req2.1).await;
    delegated(&w, &p.a1.0, p.req2.1).await;
    let (status, v) = request(&w, &p.req2.0, instance, "approve", json!({ "owner_team": "ops" })).await;
    assert_eq!(status, 202, "{v}");
    let (request_id, _, _) = pending(&w, instance).await;

    let (status, v) = w.call(&p.req2.0, "GET", &format!("{REQUESTS}/{request_id}"), None).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["myEligibility"]["canDecide"], false, "{v}");
    assert_eq!(inbox_total(&w, &p.req2.0).await, 0);
    let self_approval = refused("WORKFLOW_APPROVAL_SELF", "requester");
    let (status, v) = decide(&w, &p.req2.0, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, self_approval.clone()), "{v}");
    let (status, v) = decide_for(&w, &p.req2.0, instance, Some(p.tech.1)).await;
    assert_eq!((status, reason(&v)), (403, self_approval.clone()), "{v}");
    // Rejecting for someone is deciding too.
    let (request_id2, version, step) = pending(&w, instance).await;
    let body = json!({ "stepKey": step, "decision": "reject", "expectedVersion": version, "onBehalfOf": p.tech.1,
                       "comment": "Not ready" });
    let (status, v) = w.call(&p.req2.0, "POST", &format!("{REQUESTS}/{request_id2}/decisions"), Some(body)).await;
    assert_eq!((status, reason(&v)), (403, self_approval.clone()), "{v}");
    assert_eq!(
        scalar_i64(&w, "SELECT count(*) FROM workflow_approval_decisions WHERE request_id = $1", request_id).await,
        0
    );

    // tech in person moves it on; the CAB step binds req2 as well.
    let (status, v) = decide(&w, &p.tech.0, instance, "approve", None).await;
    assert_eq!(status, 200, "{v}");
    let (status, v) = decide_for(&w, &p.req2.0, instance, Some(p.a1.1)).await;
    assert_eq!((status, reason(&v)), (403, self_approval), "{v}");
    assert_eq!(inbox_total(&w, &p.req2.0).await, 0);

    // Disabled accounts: no delegation to or from them.
    let viewers = w.profile("Viewers", &[(w.server, false)]).await;
    let gone = w.user("gone", &[viewers]).await;
    sqlx::query("UPDATE users SET is_active = false WHERE id = $1").bind(gone.1).execute(&w.pool).await.unwrap();
    let now = Utc::now();
    let (starts, ends) = ((now + Duration::hours(1)).to_rfc3339(), (now + Duration::hours(24)).to_rfc3339());
    let (status, v) = post_window(&w, &p.a2.0, gone.1, starts.clone(), ends.clone()).await;
    assert_eq!((status, details(&v)), (400, pairs(&[("delegateUserId", "inactive")])), "{v}");
    let body = json!({ "principalUserId": gone.1, "delegateUserId": p.a2.1, "startsAt": starts, "endsAt": ends });
    let (status, v) = w.call(&w.admin, "POST", ADMIN, Some(body)).await;
    assert_eq!((status, details(&v)), (400, pairs(&[("principalUserId", "inactive")])), "{v}");
    let made = scalar_i64(
        &w,
        "SELECT count(*) FROM workflow_approval_delegations WHERE principal_id = $1 OR delegate_id = $1",
        gone.1,
    )
    .await;
    assert_eq!(made, 0);
    audit_ok(&w).await;
}

/// Step 1 due in 15 minutes and rejected when overdue.
fn reject_graph() -> Value {
    let mut g = graph();
    let steps = &mut g["transitions"][0]["approval"]["steps"];
    steps[0]["dueAfter"] = json!("PT15M");
    steps[0]["onOverdue"] = json!("reject");
    steps[1]["dueAfter"] = json!("PT15M");
    g
}

async fn make_due(w: &World, request: Uuid) {
    exec(
        w,
        "UPDATE workflow_approval_request_steps SET due_at = now() - interval '1 minute'
         WHERE request_id = $1 AND status = 'active'",
        request,
    )
    .await;
}

const OVERDUE_EVENTS: &str =
    "SELECT count(*) FROM workflow_instance_events WHERE kind = 'approval_overdue' AND approval_request_id = $1";
const CLOSES: &str = "SELECT count(*) FROM audit_log WHERE action = 'workflow.approval_close'
                      AND new_value->>'requestId' = $1::text AND new_value->>'reason' = 'overdue'";

/// Several sweeps racing over a `reject` step close the request once; a
/// sweep racing a decision on the same step leaves exactly one outcome.
#[tokio::test]
async fn the_sweep_never_double_rejects_or_races_a_decision_into_two_outcomes() {
    let Some(db) = scratch::database("approval_sweep_races").await else { return };
    let w = world(&db).await;
    let p = setup(&w).await;
    publish(&w, reject_graph()).await;
    let other =
        PgPoolOptions::new().max_connections(8).connect_with((*w.pool.connect_options()).clone()).await.unwrap();

    // Four sweeps at once over one overdue reject step.
    let (_, instance) = started(&w).await;
    let (status, v) = request(&w, &p.req.0, instance, "approve", json!({ "owner_team": "ops" })).await;
    assert_eq!(status, 202, "{v}");
    let (request_id, _, _) = pending(&w, instance).await;
    make_due(&w, request_id).await;
    let (a, b, c, d) =
        tokio::join!(sweep::tick(&w.pool), sweep::tick(&other), sweep::tick(&other), sweep::tick(&w.pool));
    let reports = [a.unwrap(), b.unwrap(), c.unwrap(), d.unwrap()];
    assert_eq!(reports.iter().map(|r| r.rejected).sum::<u64>(), 1, "{reports:?}");
    assert_eq!(reports.iter().map(|r| r.flagged + r.failed).sum::<u64>(), 0, "{reports:?}");
    assert_eq!(scalar_i64(&w, OVERDUE_EVENTS, request_id).await, 1);
    assert_eq!(scalar_i64(&w, CLOSES, request_id).await, 1);
    // And again, later: nothing more.
    assert_eq!(sweep::tick(&other).await.unwrap(), sweep::Report::default());
    assert_eq!(scalar_i64(&w, CLOSES, request_id).await, 1);

    // A decision and the sweep at once, a few times: approved xor rejected, never both, never a 500.
    for round in 0..5 {
        let (_, instance) = started(&w).await;
        let (status, v) = request(&w, &p.req.0, instance, "approve", json!({ "owner_team": "ops" })).await;
        assert_eq!(status, 202, "{v}");
        let (request_id, version, step) = pending(&w, instance).await;
        make_due(&w, request_id).await;
        let body = json!({ "stepKey": step, "decision": "approve", "expectedVersion": version });
        let path = format!("{REQUESTS}/{request_id}/decisions");
        let (decided, swept) = tokio::join!(w.call(&p.tech.0, "POST", &path, Some(body)), sweep::tick(&other));
        let swept = swept.unwrap();
        let (_, v) = w.call(&w.admin, "GET", &format!("{REQUESTS}/{request_id}"), None).await;
        let step1 = v["steps"][0]["status"].as_str().unwrap().to_owned();
        let closes = scalar_i64(&w, CLOSES, request_id).await;
        match decided.0 {
            200 => {
                assert_eq!((swept.rejected, step1.as_str(), closes), (0, "approved", 0), "round {round}: {v}");
                assert_eq!(v["status"], "pending", "round {round}: {v}");
            }
            s => {
                assert!((400..500).contains(&s), "round {round}: {s} {}", decided.1);
                assert_eq!((swept.rejected, step1.as_str(), closes), (1, "rejected", 1), "round {round}: {v}");
                assert_eq!(
                    scalar_i64(
                        &w,
                        "SELECT count(*) FROM workflow_approval_decisions WHERE request_id = $1",
                        request_id
                    )
                    .await,
                    0
                );
            }
        }
    }
    other.close().await;
    audit_ok(&w).await;
}

/// An install upgraded to the sweep (slice A4 adds no migration) may hold more
/// overdue steps than one tick handles, left by a version without a sweep:
/// the first ticks work through them a batch at a time, each step once, with
/// `flag` steps flagged and `reject` steps closed, and nothing approved.
#[tokio::test]
async fn the_first_sweeps_after_the_upgrade_work_through_an_overdue_backlog_once() {
    let Some(db) = scratch::database("approval_sweep_backlog").await else { return };
    let w = world(&db).await;
    let p = setup(&w).await;
    // Version 3: flag steps. Requests made against it stay pinned to it.
    let mut flag = graph();
    flag["transitions"][0]["approval"]["steps"][0]["dueAfter"] = json!("PT15M");
    publish(&w, flag).await;
    let total = usize::try_from(sweep::BATCH).unwrap() + 20;
    let rejecting = 15;
    let mut requests = Vec::with_capacity(total);
    for n in 0..total {
        if n == total - rejecting {
            // Version 4: reject steps, for the last few requests.
            publish(&w, reject_graph()).await;
        }
        let (_, instance) = started(&w).await;
        let (status, v) = request(&w, &p.req.0, instance, "approve", json!({ "owner_team": "ops" })).await;
        assert_eq!(status, 202, "{v}");
        requests.push(pending(&w, instance).await.0);
    }
    // As a version without a sweep left them: due weeks ago, never marked.
    let backdated = sqlx::query(
        "UPDATE workflow_approval_request_steps SET due_at = now() - interval '21 days' + step_no * interval '1 second'
         WHERE status = 'active' AND overdue_at IS NULL",
    )
    .execute(&w.pool)
    .await
    .unwrap()
    .rows_affected();
    assert_eq!(backdated, total as u64);

    let first = sweep::tick(&w.pool).await.unwrap();
    assert_eq!(first.flagged + first.rejected, sweep::BATCH as u64, "{first:?}");
    let second = sweep::tick(&w.pool).await.unwrap();
    assert_eq!(second.flagged + second.rejected, 20, "{second:?}");
    assert_eq!(sweep::tick(&w.pool).await.unwrap(), sweep::Report::default());
    assert_eq!(
        (first.flagged + second.flagged, first.rejected + second.rejected),
        ((total - rejecting) as u64, rejecting as u64)
    );
    for (n, request_id) in requests.iter().enumerate() {
        assert_eq!(scalar_i64(&w, OVERDUE_EVENTS, *request_id).await, 1, "request {n}");
    }
    let statuses: Vec<(String, i64)> =
        sqlx::query_as("SELECT status, count(*) FROM workflow_approval_requests GROUP BY status ORDER BY status")
            .fetch_all(&w.pool)
            .await
            .unwrap();
    assert_eq!(
        statuses,
        [("pending".to_owned(), (total - rejecting) as i64), ("rejected".to_owned(), rejecting as i64)]
    );
    let approved: i64 =
        sqlx::query_scalar("SELECT count(*) FROM workflow_approval_decisions").fetch_one(&w.pool).await.unwrap();
    assert_eq!(approved, 0, "the sweep never approves");
    audit_ok(&w).await;
}
