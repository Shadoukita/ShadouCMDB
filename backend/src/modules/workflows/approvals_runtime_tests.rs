//! Run-time approvals through the real router against PostgreSQL (approvals
//! design SHAA-1869 slice A3, SHAA-1880): requests on gated transitions,
//! quorum over two steps, the final apply with its stale and tamper checks,
//! every four-eyes vector and the token rules of SHAA-1872 (C1, C3, C4), a
//! restricted approver, and the closures by veto, withdrawal, manager cancel,
//! instance cancel, force, CI deletion, migration and bulk runs.

use serde_json::{Value, json};
use uuid::Uuid;

use super::runtime_tests::{DEFS, RUN, World, details, id, pairs, world};
use crate::db::scratch;
use crate::modules::api_tokens::tests::{Creds, code};

pub(super) const REQUESTS: &str = "/api/v1/workflow-approval-requests";

pub(super) struct People {
    /// Requests: granted every transition, and also a member of Tech and CAB.
    pub(super) req: (Creds, Uuid),
    /// A second requester (granted, in no approver profile).
    pub(super) req2: (Creds, Uuid),
    /// Tech reviewer.
    pub(super) tech: (Creds, Uuid),
    /// CAB members.
    pub(super) a1: (Creds, Uuid),
    pub(super) a2: (Creds, Uuid),
    pub(super) a3: (Creds, Uuid),
    /// Eligible for the CAB step through a profile that may not view servers.
    pub(super) blind: (Creds, Uuid),
}

/// Version 2 of the world's workflow: `approve` needs a technical review and
/// then two CAB approvals; `implement` one check; `review` one approval by
/// someone who took no part in `implement`.
pub(super) async fn setup(w: &World) -> People {
    w.ok(
        "POST",
        "/api/v1/attribute-definitions",
        json!({ "classId": w.server, "key": "notes", "label": "notes", "dataType": "text" }),
    )
    .await;
    let list: Uuid = sqlx::query_scalar("SELECT list_id FROM lookup_list_values WHERE id = $1")
        .bind(w.value("planned"))
        .fetch_one(&w.pool)
        .await
        .unwrap();
    w.ok("POST", "/api/v1/lookup-list-values", json!({ "listId": list, "key": "implemented", "name": "implemented" }))
        .await;
    publish(w, graph()).await;
    let def = w.ok("GET", &format!("{DEFS}/{}", w.definition), json!(null)).await;
    let v = w
        .ok(
            "PUT",
            &format!("{DEFS}/{}/grants", w.definition),
            json!({ "version": def["version"], "grants": [
                { "transitionKey": "approve", "profiles": ["Approvers"] },
                { "transitionKey": "implement", "profiles": ["Approvers"] },
                { "transitionKey": "review", "profiles": ["Approvers"] },
                { "transitionKey": "_cancel", "profiles": ["Approvers"] }
            ] }),
        )
        .await;
    let tech = w.profile("Tech", &[(w.server, false)]).await;
    let cab = w.profile("CAB", &[(w.server, false)]).await;
    let blind = w.profile("Blind", &[(w.network, false)]).await;
    let a = |t: &str, s: &str, profile: &str| json!({ "transitionKey": t, "stepKey": s, "source": "profile", "profile": profile });
    w.ok(
        "PUT",
        &format!("{DEFS}/{}/approvers", w.definition),
        json!({ "version": v["version"], "approvers": [
            a("approve", "tech", "Tech"), a("approve", "cab", "CAB"), a("approve", "cab", "Blind"),
            a("implement", "check", "CAB"), a("review", "review", "CAB")
        ] }),
    )
    .await;
    People {
        req: w.user("req", &[w.approvers, tech, cab]).await,
        req2: w.user("req2", &[w.approvers]).await,
        tech: w.user("tech", &[tech]).await,
        a1: w.user("a1", &[cab]).await,
        a2: w.user("a2", &[cab]).await,
        a3: w.user("a3", &[cab]).await,
        blind: w.user("blind", &[blind]).await,
    }
}

/// Version 2's graph.
fn graph() -> Value {
    json!({
        "initialState": "planned",
        "states": [
            { "key": "planned", "name": "Planned", "category": "open", "stateValue": "planned" },
            { "key": "approved", "name": "Approved", "category": "active", "stateValue": "approved" },
            { "key": "implemented", "name": "Implemented", "category": "active", "stateValue": "implemented" },
            { "key": "done", "name": "Done", "category": "done", "terminal": true, "stateValue": "live" }
        ],
        "transitions": [
            { "key": "approve", "name": "Approve", "from": "planned", "to": "approved",
              "fields": [ { "attribute": "owner_team", "required": true }, { "attribute": "risk", "required": false } ],
              "approval": { "steps": [
                { "key": "tech", "name": "Technical review", "requiredApprovals": 1 },
                { "key": "cab", "name": "CAB", "requiredApprovals": 2, "allowApiTokens": true } ] } },
            { "key": "implement", "name": "Implement", "from": "approved", "to": "implemented",
              "approval": { "steps": [ { "key": "check", "name": "Check", "requiredApprovals": 1, "allowApiTokens": true } ] } },
            { "key": "review", "name": "Review", "from": "implemented", "to": "done",
              "approval": { "steps": [ { "key": "review", "name": "Review", "requiredApprovals": 1,
                                          "excludeActorsOf": ["implement"] } ] } }
        ]
    })
}

async fn publish(w: &World, graph: Value) {
    let draft = w.ok("PUT", &format!("{DEFS}/{}/draft", w.definition), graph).await;
    w.ok(
        "POST",
        &format!("{DEFS}/{}/draft/publish", w.definition),
        json!({ "expectedDraftChecksum": draft["checksum"], "changeNote": "approvals" }),
    )
    .await;
}

/// An API token of `owner` narrowed to `profile`, recorded as minted by
/// `minter` (None: unknown, as a CLI-minted token).
async fn token(w: &World, owner: Uuid, profile: Uuid, minter: Option<Uuid>) -> (Creds, Uuid) {
    let expires = (chrono::Utc::now() + chrono::Duration::days(30)).to_rfc3339();
    let body = json!({ "name": format!("t-{}", Uuid::new_v4().simple()), "userId": owner, "profileId": profile,
        "expiresAt": expires });
    let v = w.ok("POST", "/api/v1/admin/api-tokens", body).await;
    let token = id(&v["token"]);
    sqlx::query("UPDATE api_tokens SET created_by_user_id = $2 WHERE id = $1")
        .bind(token)
        .bind(minter)
        .execute(&w.pool)
        .await
        .unwrap();
    (Creds { bearer: Some(v["secret"].as_str().unwrap().to_owned()), ..Default::default() }, token)
}

async fn profile_id(w: &World, name: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM permission_profiles WHERE name = $1")
        .bind(name)
        .fetch_one(&w.pool)
        .await
        .unwrap()
}

/// A CI with a running instance of the current version; returns (CI, instance).
async fn started(w: &World) -> (Uuid, Uuid) {
    let ci = w.ci(w.server).await;
    let (status, v) = w.start(&w.admin, ci).await;
    assert_eq!(status, 201, "{v}");
    (ci, id(&v["instance"]))
}

async fn request(w: &World, creds: &Creds, instance: Uuid, key: &str, fields: Value) -> (u16, Value) {
    let (_, v) = w.call(&w.admin, "GET", &format!("{RUN}/{instance}"), None).await;
    let version = v["instance"]["version"].clone();
    w.transition(creds, instance, json!({ "transitionKey": key, "expectedVersion": version, "fields": fields })).await
}

/// The pending request of an instance, as `(id, version, stepKey)`.
async fn pending(w: &World, instance: Uuid) -> (Uuid, i64, String) {
    let (_, v) = w.call(&w.admin, "GET", &format!("{RUN}/{instance}"), None).await;
    let p = &v["instance"]["pendingApproval"];
    assert!(p.is_object(), "no pending approval: {v}");
    (id(&json!({ "id": p["requestId"] })), p["version"].as_i64().unwrap(), p["stepKey"].as_str().unwrap().to_owned())
}

async fn decide(w: &World, creds: &Creds, instance: Uuid, decision: &str, comment: Option<&str>) -> (u16, Value) {
    let (request, version, step) = pending(w, instance).await;
    let body = json!({ "stepKey": step, "decision": decision, "expectedVersion": version, "comment": comment });
    w.call(creds, "POST", &format!("{REQUESTS}/{request}/decisions"), Some(body)).await
}

fn reason(v: &Value) -> (String, String) {
    (code(v).to_owned(), v["error"]["details"][0]["code"].as_str().unwrap_or("").to_owned())
}

fn refused(code: &str, reason: &str) -> (String, String) {
    (code.to_owned(), reason.to_owned())
}

async fn count(w: &World, sql: &str, id: Uuid) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned())).bind(id).fetch_one(&w.pool).await.unwrap()
}

async fn audit_ok(w: &World) {
    let problems: Vec<(i64, String)> =
        sqlx::query_as("SELECT chain_seq, problem FROM audit_log_verify()").fetch_all(&w.pool).await.unwrap();
    assert_eq!(problems, vec![]);
}

/// Request → technical review → two CAB approvals → the transition applies.
/// Refusals on the way change nothing; a stale or tampered request is refused
/// at the final approval and writes nothing, not even the decision.
#[tokio::test]
async fn approval_requests_run_to_quorum_and_apply_the_transition() {
    let Some(db) = scratch::database("workflow_approvals_run").await else { return };
    let w = world(&db).await;
    let p = setup(&w).await;
    let (ci, instance) = started(&w).await;

    // The requester sees that approve needs approval, and in which steps.
    let (_, v) = w.call(&p.req.0, "GET", &format!("{RUN}/{instance}"), None).await;
    let approve = v["availableTransitions"].as_array().unwrap().iter().find(|t| t["key"] == "approve").unwrap();
    assert_eq!(approve["requiresApproval"], true);
    let steps: Vec<(&str, i64)> = approve["approvalSteps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["key"].as_str().unwrap(), s["requiredApprovals"].as_i64().unwrap()))
        .collect();
    assert_eq!(steps, [("tech", 1), ("cab", 2)]);

    // The request is validated as a transition is, then 202 with nothing written to the CI.
    let (status, v) = request(&w, &p.req.0, instance, "approve", json!({})).await;
    assert_eq!((status, code(&v)), (422, "WORKFLOW_CONDITION_FAILED"), "{v}");
    let (status, v) = request(&w, &p.req.0, instance, "approve", json!({ "owner_team": "ops", "risk": 2 })).await;
    assert_eq!(status, 202, "{v}");
    assert_eq!(v["state"]["key"], "planned");
    assert_eq!(v["version"], 2);
    let pa = &v["pendingApproval"];
    assert_eq!(
        (pa["stepNo"].as_i64(), pa["stepKey"].as_str(), pa["stepCount"].as_i64()),
        (Some(1), Some("tech"), Some(2))
    );
    assert_eq!(
        (pa["approvals"].as_i64(), pa["required"].as_i64(), pa["toState"].as_str()),
        (Some(0), Some(1), Some("approved"))
    );
    assert_eq!(w.ci_values(ci).await["attributes"]["owner_team"], Value::Null, "nothing written yet");
    let request_id = id(&json!({ "id": pa["requestId"] }));

    // While it is pending, no transition runs and the transitions say why.
    let (status, v) = request(&w, &p.req.0, instance, "approve", json!({ "owner_team": "x" })).await;
    assert_eq!((status, code(&v)), (409, "WORKFLOW_APPROVAL_PENDING"), "{v}");
    assert_eq!(v["error"]["details"][0]["message"], json!(request_id.to_string()));
    let (_, v) = w.call(&p.req.0, "GET", &format!("{RUN}/{instance}"), None).await;
    assert_eq!(v["availableTransitions"][0]["blockedBy"][0]["code"], "approval_pending", "{v}");

    // The request as each one sees it.
    let path = format!("{REQUESTS}/{request_id}");
    let (status, v) = w.call(&p.req.0, "GET", &path, None).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["requester"], json!({ "id": p.req.1, "name": "req", "active": true, "stillAuthorized": true }));
    assert_eq!(v["stagedFields"], json!({ "owner_team": "ops", "risk": 2 }));
    assert_eq!(
        (v["myEligibility"]["canDecide"].as_bool(), v["myEligibility"]["reason"].as_str()),
        (Some(false), Some("requester"))
    );
    assert_eq!(v["approvers"], Value::Null, "the requester does not see who decides");
    assert_eq!(v["steps"][0]["eligibleCount"], 1, "tech alone: the requester never counts");
    let (status, v) = w.call(&p.tech.0, "GET", &path, None).await;
    assert_eq!((status, v["myEligibility"]["canDecide"].as_bool()), (200, Some(true)), "{v}");
    assert_eq!(v["approvers"][0]["label"], "profile Tech");
    // A user eligible through a profile that may not view servers never sees it.
    let (status, v) = w.call(&p.blind.0, "GET", &path, None).await;
    assert_eq!((status, code(&v)), (404, "NOT_FOUND"), "{v}");
    let (status, v) = decide(&w, &p.blind.0, instance, "approve", None).await;
    assert_eq!((status, code(&v)), (404, "NOT_FOUND"), "{v}");

    // Refusals at step 1.
    let (status, v) = decide(&w, &p.a1.0, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, refused("FORBIDDEN", "not_eligible")), "{v}");
    let body = json!({ "stepKey": "cab", "decision": "approve", "expectedVersion": 1 });
    let (status, v) = w.call(&p.tech.0, "POST", &format!("{path}/decisions"), Some(body)).await;
    assert_eq!((status, details(&v)), (409, pairs(&[("stepKey", "step_not_active")])), "{v}");
    let body = json!({ "stepKey": "tech", "decision": "approve", "expectedVersion": 9 });
    let (status, v) = w.call(&p.tech.0, "POST", &format!("{path}/decisions"), Some(body)).await;
    assert_eq!((status, code(&v)), (409, "VERSION_CONFLICT"), "{v}");
    let body = json!({ "stepKey": "tech", "decision": "reject", "expectedVersion": 1 });
    let (status, v) = w.call(&p.tech.0, "POST", &format!("{path}/decisions"), Some(body)).await;
    assert_eq!((status, details(&v)), (400, pairs(&[("comment", "required")])), "a rejection says why: {v}");

    // Step 1 reaches its quorum; step 2 is active.
    let (status, v) = decide(&w, &p.tech.0, instance, "approve", Some("looks fine")).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        (v["request"]["currentStepNo"].as_i64(), v["request"]["steps"][0]["status"].as_str()),
        (Some(2), Some("approved"))
    );
    assert_eq!(v["instance"]["state"]["key"], "planned");
    assert_eq!(v["instance"]["pendingApproval"]["stepKey"], "cab");
    assert_eq!(v["request"]["steps"][1]["eligibleCount"], 3, "a1, a2, a3; not the requester, not blind");
    // Whoever approved step 1 does not approve step 2 (distinctFromEarlier).
    let (status, v) = decide(&w, &p.tech.0, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, refused("WORKFLOW_APPROVAL_SELF", "earlier_step")), "{v}");

    // 2 of 3: one approval is not enough, and one vote per person.
    let (status, v) = decide(&w, &p.a1.0, instance, "approve", None).await;
    assert_eq!((status, v["request"]["status"].as_str()), (200, Some("pending")), "{v}");
    assert_eq!(v["instance"]["pendingApproval"]["approvals"], 1);
    let (status, v) = decide(&w, &p.a1.0, instance, "approve", None).await;
    assert_eq!((status, details(&v)), (409, pairs(&[("decision", "already_decided")])), "{v}");

    // Stale: a staged field changed since the request. Nothing is recorded.
    let decisions = "SELECT count(*) FROM workflow_approval_decisions WHERE request_id = $1";
    let ci_version = w.ci_values(ci).await["version"].clone();
    w.ok(
        "PATCH",
        &format!("/api/v1/configuration-items/{ci}"),
        json!({ "version": ci_version, "attributes": { "owner_team": "someone else" } }),
    )
    .await;
    let (status, v) = decide(&w, &p.a2.0, instance, "approve", None).await;
    assert_eq!((status, code(&v)), (409, "WORKFLOW_APPROVAL_STALE"), "{v}");
    assert_eq!(details(&v), pairs(&[("fields.owner_team", "changed")]));
    assert_eq!(count(&w, decisions, request_id).await, 2, "the refused approval is not recorded");
    assert_eq!(pending(&w, instance).await.2, "cab");
    let ci_version = w.ci_values(ci).await["version"].clone();
    w.ok(
        "PATCH",
        &format!("/api/v1/configuration-items/{ci}"),
        json!({ "version": ci_version, "attributes": { "owner_team": null } }),
    )
    .await;

    // Tampered: a key that is not a transition field is refused at apply (A-Q7.1).
    sqlx::query(
        "UPDATE workflow_approval_requests SET staged_fields = staged_fields || '{\"notes\": \"x\"}' WHERE id = $1",
    )
    .bind(request_id)
    .execute(&w.pool)
    .await
    .unwrap();
    let (status, v) = decide(&w, &p.a2.0, instance, "approve", None).await;
    assert_eq!((status, code(&v)), (409, "WORKFLOW_APPROVAL_STALE"), "{v}");
    assert_eq!(details(&v), pairs(&[("fields.notes", "not_a_transition_field")]));
    assert_eq!(count(&w, decisions, request_id).await, 2);
    let after = w.ci_values(ci).await;
    assert_eq!(
        (after["attributes"]["notes"].clone(), after["attributes"]["owner_team"].clone()),
        (Value::Null, Value::Null)
    );
    sqlx::query("UPDATE workflow_approval_requests SET staged_fields = staged_fields - 'notes' WHERE id = $1")
        .bind(request_id)
        .execute(&w.pool)
        .await
        .unwrap();

    // The second CAB approval applies the transition, with the decider as actor.
    let (status, v) = decide(&w, &p.a2.0, instance, "approve", None).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        (v["request"]["status"].as_str(), v["request"]["closeReason"].as_str()),
        (Some("approved"), Some("approved"))
    );
    assert_eq!(v["instance"]["state"]["key"], "approved");
    assert_eq!(v["instance"]["pendingApproval"], Value::Null);
    let after = w.ci_values(ci).await;
    assert_eq!(after["attributes"]["owner_team"], "ops");
    assert_eq!(after["attributes"]["risk"], 2);
    assert_eq!(after["attributes"]["lifecycle"], json!(w.value("approved").to_string()));

    // Audit: both rows of the final approval name the request and the requester (SHAA-1872).
    let rows: Vec<(String, Option<String>, Value)> = sqlx::query_as(
        "SELECT action, actor_name, new_value FROM audit_log
         WHERE entity_type = 'configuration_items' AND entity_id = $1 ORDER BY id",
    )
    .bind(ci)
    .fetch_all(&w.pool)
    .await
    .unwrap();
    let actions: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(
        actions,
        [
            "create",
            "update",
            "workflow.start",
            "workflow.approval_request",
            "workflow.approval_decide",
            "workflow.approval_decide",
            "update",
            "update",
            "workflow.approval_decide",
            "update",
            "workflow.transition"
        ]
    );
    for (action, actor, new) in rows.iter().rev().take(2) {
        assert_eq!(actor.as_deref(), Some("a2"), "{action}");
        assert_eq!(new["approvalRequestId"], json!(request_id.to_string()), "{action}: {new}");
        assert_eq!(new["requestedBy"], json!({ "id": p.req.1, "name": "req" }), "{action}: {new}");
    }
    let transition = &rows.last().unwrap().2;
    let approvers: Vec<&str> =
        transition["approvers"].as_array().unwrap().iter().map(|a| a["approvedBy"].as_str().unwrap()).collect();
    assert_eq!(approvers, ["tech", "a1", "a2"]);
    let events: Vec<(String, Option<Uuid>)> = sqlx::query_as(
        "SELECT kind, approval_request_id FROM workflow_instance_events WHERE instance_id = $1 ORDER BY id",
    )
    .bind(instance)
    .fetch_all(&w.pool)
    .await
    .unwrap();
    let kinds: Vec<&str> = events.iter().map(|e| e.0.as_str()).collect();
    assert_eq!(
        kinds,
        ["start", "approval_request", "approval_decision", "approval_decision", "approval_decision", "transition"]
    );
    assert_eq!(events.last().unwrap().1, Some(request_id));
    let (status, v) = w.call(&p.req.0, "GET", &format!("{RUN}/{instance}/events"), None).await;
    assert_eq!((status, v["data"][1]["kind"].as_str()), (200, Some("approval_request")), "{v}");

    // A-Q7.2: the requester's standing is read live, and does not block anything.
    sqlx::query("DELETE FROM workflow_transition_grants WHERE definition_id = $1 AND transition_key = 'approve'")
        .bind(w.definition)
        .execute(&w.pool)
        .await
        .unwrap();
    let (_, v) = w.call(&w.admin, "GET", &path, None).await;
    assert_eq!(
        (v["requester"]["active"].as_bool(), v["requester"]["stillAuthorized"].as_bool()),
        (Some(true), Some(false))
    );
    audit_ok(&w).await;
    db.drop().await;
}

/// §13: every identity of the requester is refused with
/// WORKFLOW_APPROVAL_SELF; tokens decide only when self-minted on a step that
/// allows them (C1) and are recorded (C3); `excludeActorsOf` refuses the
/// requester and the approver of the excluded transition by user id (C4).
#[tokio::test]
async fn four_eyes_holds_for_every_identity_of_the_requester() {
    let Some(db) = scratch::database("workflow_approvals_four_eyes").await else { return };
    let w = world(&db).await;
    let p = setup(&w).await;
    let cab = profile_id(&w, "CAB").await;
    let (_, instance) = started(&w).await;
    let (status, v) = request(&w, &p.req.0, instance, "approve", json!({ "owner_team": "ops" })).await;
    assert_eq!(status, 202, "{v}");
    let self_refusal = |v: &Value| reason(v);

    // 1. The requester, signed in (eligible for tech through a second profile).
    let (status, v) = decide(&w, &p.req.0, instance, "approve", None).await;
    assert_eq!((status, self_refusal(&v)), (403, refused("WORKFLOW_APPROVAL_SELF", "requester")), "{v}");
    // A session-only step refuses even a self-minted token.
    let tech_profile = profile_id(&w, "Tech").await;
    let (tech_token, _) = token(&w, p.tech.1, tech_profile, Some(p.tech.1)).await;
    let (status, v) = decide(&w, &tech_token, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, refused("FORBIDDEN", "session_required")), "{v}");
    let (status, v) = decide(&w, &p.tech.0, instance, "approve", None).await;
    assert_eq!(status, 200, "{v}");

    // 2. The requester through their CAB profile on the CAB step.
    let (status, v) = decide(&w, &p.req.0, instance, "approve", None).await;
    assert_eq!((status, self_refusal(&v)), (403, refused("WORKFLOW_APPROVAL_SELF", "requester")), "{v}");
    // 3. Through a token the requester owns and minted.
    let (own, _) = token(&w, p.req.1, cab, Some(p.req.1)).await;
    let (status, v) = decide(&w, &own, instance, "approve", None).await;
    assert_eq!((status, self_refusal(&v)), (403, refused("WORKFLOW_APPROVAL_SELF", "requester")), "{v}");
    // 4. Through a token the requester minted for someone else.
    let (lent, _) = token(&w, p.a1.1, cab, Some(p.req.1)).await;
    let (status, v) = decide(&w, &lent, instance, "approve", None).await;
    assert_eq!((status, self_refusal(&v)), (403, refused("WORKFLOW_APPROVAL_SELF", "token_creator")), "{v}");
    // C1: a token not minted by its owner never decides: another creator, or an unknown one.
    let (by_admin, _) = token(&w, p.a1.1, cab, Some(admin_id(&w).await)).await;
    let (status, v) = decide(&w, &by_admin, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, refused("FORBIDDEN", "token_not_self_minted")), "{v}");
    let (unknown, _) = token(&w, p.a1.1, cab, None).await;
    let (status, v) = decide(&w, &unknown, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, refused("FORBIDDEN", "token_not_self_minted")), "{v}");
    let decisions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM workflow_approval_decisions").fetch_one(&w.pool).await.unwrap();
    assert_eq!(decisions, 1, "no refusal recorded a decision");

    // A self-minted token on a step that allows tokens decides, and is recorded (C3).
    let (mine, mine_id) = token(&w, p.a1.1, cab, Some(p.a1.1)).await;
    let (status, v) = decide(&w, &mine, instance, "approve", None).await;
    assert_eq!(status, 200, "{v}");
    let decision = &v["request"]["steps"][1]["decisions"][0];
    assert_eq!((decision["actorName"].as_str(), decision["credential"].as_str()), (Some("a1"), Some("token")));
    let row: (String, Option<Uuid>, Option<Uuid>) = sqlx::query_as(
        "SELECT credential, token_id, token_creator_id FROM workflow_approval_decisions WHERE actor_id = $1",
    )
    .bind(p.a1.1)
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!(row, ("token".to_owned(), Some(mine_id), Some(p.a1.1)));
    let audit: Value = sqlx::query_scalar(
        "SELECT new_value FROM audit_log WHERE action = 'workflow.approval_decide' AND actor_name = 'a1'",
    )
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!((audit["tokenId"].clone(), audit["tokenCreatorId"].clone()), (json!(mine_id), json!(p.a1.1)));
    let (status, v) = decide(&w, &p.a2.0, instance, "approve", None).await;
    assert_eq!((status, v["instance"]["state"]["key"].as_str()), (200, Some("approved")), "{v}");

    // C3/§4.3: a request through a token minted by someone else excludes its creator too; an unknown creator only the owner.
    let approvers_profile = w.approvers;
    let (_, other) = started(&w).await;
    let admin = admin_id(&w).await;
    let (lent_to_req2, lent_id) = token(&w, p.req2.1, approvers_profile, Some(admin)).await;
    let (status, v) = request(&w, &lent_to_req2, other, "approve", json!({ "owner_team": "ops" })).await;
    assert_eq!(status, 202, "{v}");
    let row: (Vec<Uuid>, Option<Uuid>, Option<Uuid>) = sqlx::query_as(
        "SELECT excluded_user_ids, token_id, token_creator_id FROM workflow_approval_requests WHERE instance_id = $1",
    )
    .bind(other)
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!(row, (vec![p.req2.1, admin], Some(lent_id), Some(admin)));
    let (_, third) = started(&w).await;
    let (cli, cli_id) = token(&w, p.req2.1, approvers_profile, None).await;
    let (status, v) = request(&w, &cli, third, "approve", json!({ "owner_team": "ops" })).await;
    assert_eq!(status, 202, "{v}");
    let row: (Vec<Uuid>, Option<Uuid>, Option<Uuid>) = sqlx::query_as(
        "SELECT excluded_user_ids, token_id, token_creator_id FROM workflow_approval_requests WHERE instance_id = $1",
    )
    .bind(third)
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!(row, (vec![p.req2.1], Some(cli_id), None));

    // C4: R requests implement and A approves it; on review (excludeActorsOf implement) both are refused.
    let (status, v) = request(&w, &p.req.0, instance, "implement", json!({})).await;
    assert_eq!(status, 202, "{v}");
    let (status, v) = decide(&w, &p.a3.0, instance, "approve", None).await;
    assert_eq!((status, v["instance"]["state"]["key"].as_str()), (200, Some("implemented")), "{v}");
    let (status, v) = request(&w, &p.req2.0, instance, "review", json!({})).await;
    assert_eq!(status, 202, "{v}");
    let (status, v) = decide(&w, &p.req.0, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, refused("WORKFLOW_APPROVAL_SELF", "actor_of:implement")), "R: {v}");
    let (status, v) = decide(&w, &p.a3.0, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, refused("WORKFLOW_APPROVAL_SELF", "actor_of:implement")), "A: {v}");
    let (status, v) = decide(&w, &p.a1.0, instance, "approve", None).await;
    assert_eq!((status, v["instance"]["state"]["key"].as_str()), (200, Some("done")), "{v}");
    assert_eq!(v["instance"]["status"], "completed");
    audit_ok(&w).await;
    db.drop().await;
}

/// A request's status and close reason.
async fn closed(w: &World, id: Uuid) -> (String, Option<String>) {
    sqlx::query_as("SELECT status, close_reason FROM workflow_approval_requests WHERE id = $1")
        .bind(id)
        .fetch_one(&w.pool)
        .await
        .unwrap()
}

async fn admin_id(w: &World) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE username = 'admin'").fetch_one(&w.pool).await.unwrap()
}

/// Veto and re-request, withdrawal, manager cancel, and every engine path
/// that closes a pending request (§5.3), with the right reason.
#[tokio::test]
async fn pending_requests_close_with_the_right_reason() {
    let Some(db) = scratch::database("workflow_approvals_close").await else { return };
    let w = world(&db).await;
    let p = setup(&w).await;
    let closed = |id: Uuid| closed(&w, id);
    let (_, instance) = started(&w).await;
    let fields = json!({ "owner_team": "ops" });

    // Veto: one rejection rejects the request; the instance can request again.
    let (status, _) = request(&w, &p.req.0, instance, "approve", fields.clone()).await;
    assert_eq!(status, 202);
    let (first, _, _) = pending(&w, instance).await;
    let (status, v) = decide(&w, &p.tech.0, instance, "reject", Some("not like this")).await;
    assert_eq!((status, v["request"]["status"].as_str()), (200, Some("rejected")), "{v}");
    assert_eq!(v["request"]["steps"][1]["status"], "closed");
    assert_eq!(
        (v["instance"]["state"]["key"].as_str(), v["instance"]["pendingApproval"].clone()),
        (Some("planned"), Value::Null)
    );
    let (status, v) = request(&w, &p.req.0, instance, "approve", fields.clone()).await;
    assert_eq!((status, v["pendingApproval"]["requestNo"].as_i64()), (202, Some(2)), "{v}");
    let (second, version, _) = pending(&w, instance).await;
    assert_ne!(first, second);

    // Withdraw: only the requester.
    let body = json!({ "expectedVersion": version });
    let (status, v) = w.call(&p.a1.0, "POST", &format!("{REQUESTS}/{second}/withdraw"), Some(body.clone())).await;
    assert_eq!((status, reason(&v)), (403, refused("FORBIDDEN", "not_requester")), "{v}");
    let (status, v) = w.call(&p.req.0, "POST", &format!("{REQUESTS}/{second}/withdraw"), Some(body.clone())).await;
    assert_eq!((status, v["request"]["status"].as_str()), (200, Some("withdrawn")), "{v}");
    let (status, v) = w.call(&p.req.0, "POST", &format!("{REQUESTS}/{second}/withdraw"), Some(body)).await;
    assert_eq!((status, details(&v)), (409, pairs(&[("id", "not_pending")])), "{v}");

    // Manager cancel: workflows.manage or the _cancel grant with edit; a comment is required.
    request(&w, &p.req.0, instance, "approve", fields.clone()).await;
    let (third, version, _) = pending(&w, instance).await;
    let (status, v) = w
        .call(
            &p.a1.0,
            "POST",
            &format!("{REQUESTS}/{third}/cancel"),
            Some(json!({ "expectedVersion": version, "comment": "no" })),
        )
        .await;
    assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{v}");
    let (status, v) = w
        .call(&w.admin, "POST", &format!("{REQUESTS}/{third}/cancel"), Some(json!({ "expectedVersion": version })))
        .await;
    assert_eq!((status, code(&v)), (400, "VALIDATION_ERROR"), "{v}");
    let (status, v) = w
        .call(
            &p.req2.0,
            "POST",
            &format!("{REQUESTS}/{third}/cancel"),
            Some(json!({ "expectedVersion": version, "comment": "superseded" })),
        )
        .await;
    assert_eq!((status, v["request"]["status"].as_str()), (200, Some("cancelled")), "the _cancel grant holder: {v}");
    assert_eq!(closed(third).await, ("cancelled".to_owned(), Some("withdrawn".to_owned())));

    // Instance cancel closes the request.
    request(&w, &p.req.0, instance, "approve", fields.clone()).await;
    let (fourth, _, _) = pending(&w, instance).await;
    let (_, v) = w.call(&w.admin, "GET", &format!("{RUN}/{instance}"), None).await;
    let body = json!({ "expectedVersion": v["instance"]["version"], "reason": "no longer needed" });
    let (status, v) = w.call(&w.admin, "POST", &format!("{RUN}/{instance}/cancel"), Some(body)).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(closed(fourth).await, ("cancelled".to_owned(), Some("instance_cancelled".to_owned())));

    // Force closes it and names it as overridden.
    let (_, forced) = started(&w).await;
    request(&w, &p.req.0, forced, "approve", fields.clone()).await;
    let (fifth, _, _) = pending(&w, forced).await;
    let (_, v) = w.call(&w.admin, "GET", &format!("{RUN}/{forced}"), None).await;
    let body =
        json!({ "expectedVersion": v["instance"]["version"], "stateKey": "approved", "reason": "emergency change" });
    let (status, v) = w.call(&w.admin, "POST", &format!("{RUN}/{forced}/force"), Some(body)).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(closed(fifth).await, ("cancelled".to_owned(), Some("instance_forced".to_owned())));
    let force: Value = sqlx::query_scalar("SELECT new_value FROM audit_log WHERE action = 'workflow.force'")
        .fetch_one(&w.pool)
        .await
        .unwrap();
    assert_eq!(force["overriddenApprovalRequestId"], json!(fifth));

    // A CI soft delete closes it.
    let (deleted_ci, deleted) = started(&w).await;
    request(&w, &p.req.0, deleted, "approve", fields.clone()).await;
    let (sixth, _, _) = pending(&w, deleted).await;
    let (status, v) = w.call(&w.admin, "DELETE", &format!("/api/v1/configuration-items/{deleted_ci}"), None).await;
    assert_eq!(status, 204, "{v}");
    assert_eq!(closed(sixth).await, ("cancelled".to_owned(), Some("ci_deleted".to_owned())));
    let events: Vec<(String, String)> =
        sqlx::query_as("SELECT kind, actor_type FROM workflow_instance_events WHERE instance_id = $1 ORDER BY id")
            .bind(deleted)
            .fetch_all(&w.pool)
            .await
            .unwrap();
    assert_eq!(
        events[events.len() - 2..],
        [("approval_close".into(), "system".into()), ("cancel".into(), "system".into())]
    );

    // Bulk: a gated item creates a request.
    let (_, bulk) = started(&w).await;
    let body = json!({ "items": [ { "instanceId": bulk, "transitionKey": "approve", "expectedVersion": 1, "fields": fields } ] });
    let (status, v) = w.call(&p.req.0, "POST", &format!("{RUN}/bulk-transitions"), Some(body)).await;
    assert_eq!((status, v["succeeded"].as_i64()), (200, Some(1)), "{v}");
    let (bulk_request, _, _) = pending(&w, bulk).await;
    assert_eq!(v["results"][0]["approvalRequestId"], json!(bulk_request));
    assert_eq!(v["results"][0]["instance"]["state"]["key"], "planned");

    // Migration: pending requests stay with `skip` (the default) and close with `cancel`.
    let (_, waiting) = started(&w).await;
    request(&w, &p.req.0, waiting, "approve", fields.clone()).await;
    let (_, free) = started(&w).await;
    publish(&w, graph()).await;
    let path = format!("{DEFS}/{}/instance-migrations", w.definition);
    let body = |dry: bool, mode: Option<&str>| {
        let mut b = json!({ "fromVersionNo": 2, "toVersionNo": 3, "dryRun": dry });
        if let Some(m) = mode {
            b["pendingApprovals"] = json!(m);
        }
        b
    };
    let v = w.ok("POST", &path, body(true, None)).await;
    assert_eq!((v["pendingApprovals"].as_i64(), v["total"].as_i64()), (Some(2), Some(4)), "{v}");
    let v = w.ok("POST", &path, body(false, None)).await;
    assert_eq!((v["migrated"].as_i64(), v["skipped"].as_i64()), (Some(2), Some(2)), "{v}");
    let on_version = "SELECT v.version_no::bigint FROM workflow_instances i JOIN workflow_versions v ON v.id = i.version_id WHERE i.id = $1";
    assert_eq!((count(&w, on_version, waiting).await, count(&w, on_version, free).await), (2, 3));
    let (waiting_request, _, _) = pending(&w, waiting).await;
    let v = w.ok("POST", &path, body(false, Some("cancel"))).await;
    assert_eq!((v["migrated"].as_i64(), v["skipped"].as_i64()), (Some(2), Some(0)), "{v}");
    assert_eq!(count(&w, on_version, waiting).await, 3);
    assert_eq!(closed(waiting_request).await, ("cancelled".to_owned(), Some("instance_migrated".to_owned())));
    audit_ok(&w).await;
    db.drop().await;
}

/// `(total, ids)` of a list page as `creds` sees it.
async fn listed(w: &World, creds: &Creds, query: &str) -> (i64, Vec<Uuid>) {
    let (status, v) = w.call(creds, "GET", &format!("{REQUESTS}?{query}"), None).await;
    assert_eq!(status, 200, "{query}: {v}");
    let ids = v["data"].as_array().unwrap().iter().map(|r| id(&json!({ "id": r["id"] }))).collect();
    (v["page"]["total"].as_i64().unwrap(), ids)
}

/// Slice A3b: the inbox lists exactly what each caller may decide now, the
/// other views and filters, the totals leave out what the caller may not view,
/// the instance's request history, `awaitingApproval`, and refresh.
#[tokio::test]
async fn approval_lists_show_what_each_caller_may_decide() {
    let Some(db) = scratch::database("workflow_approvals_lists").await else { return };
    let w = world(&db).await;
    let p = setup(&w).await;
    let tech_profile = profile_id(&w, "Tech").await;
    let cab = profile_id(&w, "CAB").await;
    // In both approver profiles: may decide step 1 or step 2, never both of one request.
    let both = w.user("both", &[tech_profile, cab]).await;
    let fields = json!({ "owner_team": "ops" });
    let (c1, i1) = started(&w).await;
    let (_, i2) = started(&w).await;
    let (_, i3) = started(&w).await;
    let (c_idle, _) = started(&w).await;
    for (creds, instance) in [(&p.req.0, i1), (&p.req.0, i2), (&p.req2.0, i3)] {
        let (status, v) = request(&w, creds, instance, "approve", fields.clone()).await;
        assert_eq!(status, 202, "{v}");
    }
    let (r1, _, _) = pending(&w, i1).await;
    let (r2, _, _) = pending(&w, i2).await;
    let (r3, _, _) = pending(&w, i3).await;

    // Step 1 (tech) of all three: tech and both see them; the requester and CAB do not.
    assert_eq!(listed(&w, &p.tech.0, "").await.0, 3);
    assert_eq!(listed(&w, &both.0, "view=actionable").await.0, 3);
    assert_eq!(listed(&w, &p.req.0, "").await, (1, vec![r3]), "a tech member: req2's request, never their own");
    assert_eq!(listed(&w, &p.a1.0, "").await.0, 0, "CAB's step is not active yet");

    // tech decides i1, both decides i2: both moves to CAB.
    assert_eq!(decide(&w, &p.tech.0, i1, "approve", None).await.0, 200);
    assert_eq!(decide(&w, &both.0, i2, "approve", None).await.0, 200);
    let (total, ids) = listed(&w, &p.tech.0, "").await;
    assert_eq!((total, ids), (1, vec![r3]));
    assert_eq!(listed(&w, &p.tech.0, "view=decided").await, (1, vec![r1]));
    let (total, mut ids) = listed(&w, &p.a1.0, "").await;
    ids.sort();
    let mut expected = vec![r1, r2];
    expected.sort();
    assert_eq!((total, ids), (2, expected));
    // distinctFromEarlier: both approved step 1 of r2, so only r1 waits for them at CAB (and r3 at tech).
    let (total, mut ids) = listed(&w, &both.0, "").await;
    ids.sort();
    let mut expected = vec![r1, r3];
    expected.sort();
    assert_eq!((total, ids), (2, expected));
    // A user eligible for CAB through a profile that may not view servers: nothing, in no view, in no total.
    assert_eq!(listed(&w, &p.blind.0, "").await, (0, vec![]));
    assert_eq!(listed(&w, &p.blind.0, "view=all").await, (0, vec![]));
    assert_eq!(listed(&w, &w.admin, "view=all").await.0, 3);

    // Tokens (C1): a token someone else minted has no inbox; a self-minted one lists only steps that allow tokens.
    let (lent, _) = token(&w, p.a1.1, cab, Some(admin_id(&w).await)).await;
    assert_eq!(listed(&w, &lent, "").await.0, 0);
    let (own, _) = token(&w, p.a1.1, cab, Some(p.a1.1)).await;
    assert_eq!(listed(&w, &own, "").await.0, 2, "the CAB step allows tokens");
    let (tech_token, _) = token(&w, p.tech.1, tech_profile, Some(p.tech.1)).await;
    assert_eq!(listed(&w, &tech_token, "").await.0, 0, "the tech step needs a session");
    // What the inbox lists, the request detail lets decide.
    for r in [r1, r2] {
        let (_, v) = w.call(&p.a1.0, "GET", &format!("{REQUESTS}/{r}"), None).await;
        assert_eq!(v["myEligibility"]["canDecide"], true, "{v}");
    }

    // One vote per person: after a1 approves r1, it leaves a1's inbox and stays in a2's.
    assert_eq!(decide(&w, &p.a1.0, i1, "approve", None).await.0, 200);
    assert_eq!(listed(&w, &p.a1.0, "").await, (1, vec![r2]));
    assert_eq!(listed(&w, &p.a2.0, "").await.0, 2);

    // The item, as a list shows it.
    let (_, v) = w.call(&p.a2.0, "GET", &format!("{REQUESTS}?ciId={c1}"), None).await;
    let item = &v["data"][0];
    assert_eq!(item["id"], json!(r1), "{v}");
    assert_eq!(item["requestedBy"], json!({ "id": p.req.1, "name": "req" }));
    assert_eq!((item["transitionKey"].as_str(), item["toState"].as_str()), (Some("approve"), Some("approved")));
    assert_eq!(item["stepCount"], 2);
    let step = &item["currentStep"];
    assert_eq!(
        (step["key"].as_str(), step["approvals"].as_i64(), step["requiredApprovals"].as_i64(), step["status"].as_str()),
        (Some("cab"), Some(1), Some(2), Some("active"))
    );
    assert!(item.get("stagedFields").is_none(), "a list carries no staged values");

    // Views and filters.
    assert_eq!(listed(&w, &p.req.0, "view=requested").await.0, 2);
    assert_eq!(listed(&w, &p.req2.0, "view=requested").await, (1, vec![r3]));
    let runbook = format!("view=all&status=pending&requestedBy={}", p.req.1);
    assert_eq!(listed(&w, &w.admin, &runbook).await.0, 2, "the incident runbook's query");
    assert_eq!(listed(&w, &w.admin, "view=all&status=approved").await.0, 0);
    assert_eq!(listed(&w, &w.admin, "view=all&overdue=true").await.0, 0);
    assert_eq!(listed(&w, &w.admin, "view=all&overdue=false").await.0, 3);
    assert_eq!(listed(&w, &w.admin, "view=all&definitionKey=nope").await.0, 0);
    let (_, sorted) = listed(&w, &w.admin, "view=all&sort=-requestedAt").await;
    assert_eq!(sorted, vec![r3, r2, r1]);
    let (status, v) = w.call(&w.admin, "GET", &format!("{REQUESTS}?view=mine"), None).await;
    assert_eq!((status, code(&v)), (400, "VALIDATION_ERROR"), "{v}");

    // The instance list and summary know who waits for approval.
    let (_, v) = w.call(&w.admin, "GET", &format!("{RUN}?awaitingApproval=true"), None).await;
    assert_eq!(v["page"]["total"], 3, "{v}");
    let (_, v) = w.call(&w.admin, "GET", &format!("{RUN}?awaitingApproval=false&ciId={c_idle}"), None).await;
    assert_eq!(v["page"]["total"], 1, "{v}");
    let (_, v) = w.call(&w.admin, "GET", &format!("{RUN}/summary"), None).await;
    let planned = v["data"].as_array().unwrap().iter().find(|s| s["stateKey"] == "planned").unwrap().clone();
    assert_eq!((planned["count"].as_i64(), planned["awaitingApproval"].as_i64()), (Some(4), Some(3)), "{v}");

    // History of an instance: newest first, whatever became of each.
    assert_eq!(decide(&w, &p.tech.0, i3, "reject", Some("no")).await.0, 200);
    assert_eq!(request(&w, &p.req2.0, i3, "approve", fields.clone()).await.0, 202);
    let (status, v) = w.call(&p.a1.0, "GET", &format!("{RUN}/{i3}/approval-requests"), None).await;
    assert_eq!(status, 200, "{v}");
    let history: Vec<(i64, &str)> = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["requestNo"].as_i64().unwrap(), r["status"].as_str().unwrap()))
        .collect();
    assert_eq!((v["page"]["total"].as_i64(), history), (Some(2), vec![(2, "pending"), (1, "rejected")]));
    let (status, _) = w.call(&p.blind.0, "GET", &format!("{RUN}/{i3}/approval-requests"), None).await;
    assert_eq!(status, 404);

    // Refresh: managers only, pending only; it picks up a change of approvers.
    let (status, v) = w.call(&p.a1.0, "POST", &format!("{REQUESTS}/{r2}/refresh"), None).await;
    assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{v}");
    let (_, v) = w.call(&w.admin, "GET", &format!("{REQUESTS}/{r2}"), None).await;
    assert_eq!(v["steps"][1]["eligibleCount"], 4, "a1, a2, a3 and both: {v}");
    sqlx::query("DELETE FROM user_permission_profiles WHERE user_id = $1 AND profile_id = $2")
        .bind(p.a3.1)
        .bind(cab)
        .execute(&w.pool)
        .await
        .unwrap();
    let (status, v) = w.call(&w.admin, "POST", &format!("{REQUESTS}/{r2}/refresh"), None).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["steps"][1]["eligibleCount"], 3, "a3 left CAB: {v}");
    let (status, v) = w.call(&w.admin, "POST", &format!("{REQUESTS}/{r1}/refresh"), None).await;
    assert_eq!(status, 200, "{v}");
    let (status, v) = w.call(&w.admin, "POST", &format!("{REQUESTS}/{r2}/refresh"), None).await;
    assert_eq!(status, 200, "{v}");
    // GH#663: each refresh is audited on the CI in its transaction, the
    // approvers before and after, also when nothing changed.
    let rows: Vec<(String, Uuid, Value, Value)> = sqlx::query_as(
        "SELECT actor_name, entity_id, old_value, new_value FROM audit_log
         WHERE action = 'workflow.approval_refresh' ORDER BY chain_seq",
    )
    .fetch_all(&w.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 3, "{rows:?}");
    let (actor, ci, old, new) = &rows[0];
    assert_eq!((actor.as_str(), new["requestId"].as_str()), ("admin", Some(r2.to_string().as_str())), "{new}");
    let ci_of_r2: Uuid = sqlx::query_scalar(
        "SELECT i.ci_id FROM workflow_approval_requests r JOIN workflow_instances i ON i.id = r.instance_id WHERE r.id = $1",
    )
    .bind(r2)
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!(*ci, ci_of_r2);
    assert_eq!((old["eligibleCount"].as_i64(), new["eligibleCount"].as_i64()), (Some(4), Some(3)), "{old} {new}");
    assert_eq!((new["changed"].as_bool(), new["stepNo"].as_i64()), (Some(true), Some(2)), "{new}");
    assert_eq!(old["approvers"], new["approvers"], "the CAB profile stays the principal; its members changed");
    assert!(
        new["approvers"].as_array().is_some_and(|a| a.iter().any(|p| p["kind"] == "profile" && p["id"] == json!(cab)))
    );
    // r1 sits at the same CAB step, so a3 leaving changed it as well.
    let (_, _, _, new) = &rows[1];
    assert_eq!((new["requestId"].as_str(), new["changed"].as_bool()), (Some(r1.to_string().as_str()), Some(true)));
    // r2 again: nothing changed, still recorded.
    let (_, _, old, new) = &rows[2];
    assert_eq!((new["requestId"].as_str(), new["changed"].as_bool()), (Some(r2.to_string().as_str()), Some(false)));
    assert_eq!((&old["approvers"], old["eligibleCount"].as_i64()), (&new["approvers"], Some(3)), "{old} {new}");
    audit_ok(&w).await;
    let (_, v) = w.call(&w.admin, "GET", &format!("{RUN}/{i3}/approval-requests"), None).await;
    let rejected = id(&json!({ "id": v["data"][1]["id"] }));
    let (status, v) = w.call(&w.admin, "POST", &format!("{REQUESTS}/{rejected}/refresh"), None).await;
    assert_eq!((status, details(&v)), (409, pairs(&[("id", "not_pending")])), "{v}");
    audit_ok(&w).await;
    db.drop().await;
}

/// GH#635: `excludeActorsOf` refuses every approver of the excluded
/// transition, not only the one whose vote completed its quorum.
#[tokio::test]
async fn exclude_actors_of_refuses_every_approver_of_the_excluded_transition() {
    let Some(db) = scratch::database("workflow_approvals_actors").await else { return };
    let w = world(&db).await;
    let p = setup(&w).await;
    let mut g = graph();
    g["transitions"][1]["approval"]["steps"][0]["requiredApprovals"] = json!(2);
    publish(&w, g).await;
    let (_, instance) = started(&w).await;
    assert_eq!(request(&w, &p.req.0, instance, "approve", json!({ "owner_team": "ops" })).await.0, 202);
    assert_eq!(decide(&w, &p.tech.0, instance, "approve", None).await.0, 200);
    assert_eq!(decide(&w, &p.a3.0, instance, "approve", None).await.0, 200);
    assert_eq!(decide(&w, &p.a2.0, instance, "approve", None).await.0, 200);
    // implement: a1 votes first, a2 completes the quorum.
    assert_eq!(request(&w, &p.req.0, instance, "implement", json!({})).await.0, 202);
    assert_eq!(decide(&w, &p.a1.0, instance, "approve", None).await.0, 200);
    let (status, v) = decide(&w, &p.a2.0, instance, "approve", None).await;
    assert_eq!((status, v["instance"]["state"]["key"].as_str()), (200, Some("implemented")), "{v}");
    assert_eq!(request(&w, &p.req2.0, instance, "review", json!({})).await.0, 202);
    let (review, _, _) = pending(&w, instance).await;
    for (who, creds) in [("a2", &p.a2.0), ("a1", &p.a1.0)] {
        let (status, v) = decide(&w, creds, instance, "approve", None).await;
        assert_eq!((status, reason(&v)), (403, refused("WORKFLOW_APPROVAL_SELF", "actor_of:implement")), "{who}: {v}");
        assert_eq!(listed(&w, creds, "").await.0, 0, "{who}'s inbox agrees");
    }
    assert_eq!(listed(&w, &p.a3.0, "").await, (1, vec![review]));
    let (status, v) = decide(&w, &p.a3.0, instance, "approve", None).await;
    assert_eq!((status, v["instance"]["state"]["key"].as_str()), (200, Some("done")), "{v}");
    audit_ok(&w).await;
    db.drop().await;
}

/// GH#635: so does an approver of an earlier step of the excluded transition,
/// not only the approvers of its final step.
#[tokio::test]
async fn exclude_actors_of_refuses_an_approver_of_an_earlier_step() {
    let Some(db) = scratch::database("workflow_approvals_actors_steps").await else { return };
    let w = world(&db).await;
    let p = setup(&w).await;
    let mut g = graph();
    g["transitions"][2]["approval"]["steps"][0]["excludeActorsOf"] = json!(["implement", "approve"]);
    publish(&w, g).await;
    let (tech, cab) = (profile_id(&w, "Tech").await, profile_id(&w, "CAB").await);
    // t2 does the technical review of `approve` and, as a CAB member, could decide `review`.
    let t2 = w.user("t2", &[tech, cab]).await;
    let a4 = w.user("a4", &[cab]).await;
    let (_, instance) = started(&w).await;
    assert_eq!(request(&w, &p.req.0, instance, "approve", json!({ "owner_team": "ops" })).await.0, 202);
    assert_eq!(decide(&w, &t2.0, instance, "approve", None).await.0, 200);
    assert_eq!(decide(&w, &p.a1.0, instance, "approve", None).await.0, 200);
    let (status, v) = decide(&w, &p.a2.0, instance, "approve", None).await;
    assert_eq!((status, v["instance"]["state"]["key"].as_str()), (200, Some("approved")), "{v}");
    assert_eq!(request(&w, &p.req.0, instance, "implement", json!({})).await.0, 202);
    assert_eq!(decide(&w, &p.a3.0, instance, "approve", None).await.0, 200);
    assert_eq!(request(&w, &p.req2.0, instance, "review", json!({})).await.0, 202);
    let (review, _, _) = pending(&w, instance).await;
    for (who, creds, key) in [("t2", &t2.0, "approve"), ("a1", &p.a1.0, "approve"), ("a3", &p.a3.0, "implement")] {
        let (status, v) = decide(&w, creds, instance, "approve", None).await;
        let expected = refused("WORKFLOW_APPROVAL_SELF", &format!("actor_of:{key}"));
        assert_eq!((status, reason(&v)), (403, expected), "{who}: {v}");
        assert_eq!(listed(&w, creds, "").await.0, 0, "{who}'s inbox agrees");
    }
    assert_eq!(listed(&w, &a4.0, "").await, (1, vec![review]));
    let (status, v) = decide(&w, &a4.0, instance, "approve", None).await;
    assert_eq!((status, v["instance"]["state"]["key"].as_str()), (200, Some("done")), "{v}");
    audit_ok(&w).await;
    db.drop().await;
}

/// GH#668 (SHAA-2176): a request staged while its field was nobody's state
/// field is refused at the final approval once an active workflow drives that
/// field, as a direct transition is: 409 WORKFLOW_APPROVAL_STALE with
/// `state_field`, and the CI keeps its value.
#[tokio::test]
async fn the_final_approval_refuses_a_field_that_became_a_state_field() {
    let Some(db) = scratch::database("workflow_approval_state_field").await else { return };
    let w = world(&db).await;
    let set_active = |def: Uuid, active: bool| {
        let w = &w;
        async move {
            let d = w.ok("GET", &format!("{DEFS}/{def}"), json!(null)).await;
            w.ok("PATCH", &format!("{DEFS}/{def}"), json!({ "version": d["version"], "isActive": active })).await;
        }
    };

    // A review workflow on servers, without a state field, whose gated
    // transition takes `lifecycle`: publishable while server_lifecycle is inactive.
    let review =
        id(&w.ok("POST", DEFS, json!({ "key": "server_review", "name": "Server review", "classId": w.server })).await);
    let graph = json!({
        "initialState": "open",
        "states": [
            { "key": "open", "name": "Open", "category": "open" },
            { "key": "closed", "name": "Closed", "category": "done", "terminal": true }
        ],
        "transitions": [
            { "key": "close", "name": "Close", "from": "open", "to": "closed",
              "fields": [ { "attribute": "lifecycle", "required": true } ],
              "approval": { "steps": [ { "key": "cab", "name": "CAB", "requiredApprovals": 1 } ] } }
        ]
    });
    w.ok("PUT", &format!("{DEFS}/{review}/draft"), graph).await;
    let cab = w.profile("CAB", &[(w.server, false)]).await;
    let cab = w.user("cab", &[cab]).await;
    let d = w.ok("GET", &format!("{DEFS}/{review}"), json!(null)).await;
    let step = json!({ "transitionKey": "close", "stepKey": "cab", "source": "profile", "profile": "CAB" });
    w.ok("PUT", &format!("{DEFS}/{review}/approvers"), json!({ "version": d["version"], "approvers": [step] })).await;
    set_active(w.definition, false).await;
    let draft = w.ok("GET", &format!("{DEFS}/{review}/draft"), json!(null)).await;
    let (status, v) = w
        .call(
            &w.admin,
            "POST",
            &format!("{DEFS}/{review}/draft/publish"),
            Some(json!({ "expectedDraftChecksum": draft["checksum"] })),
        )
        .await;
    assert_eq!(status, 201, "{v}");
    set_active(review, true).await;

    // The request stages a lifecycle value while no active workflow drives it.
    let ci = w.ci(w.server).await;
    let before = w.ci_values(ci).await["attributes"]["lifecycle"].clone();
    let (status, v) =
        w.call(&w.admin, "POST", RUN, Some(json!({ "definitionKey": "server_review", "ciId": ci }))).await;
    assert_eq!(status, 201, "{v}");
    let instance = id(&v["instance"]);
    let (status, v) = request(&w, &w.admin, instance, "close", json!({ "lifecycle": w.value("live") })).await;
    assert_eq!(status, 202, "the transition waits for approval: {v}");

    // server_lifecycle is active again: lifecycle is its state field now. Activating it is
    // refused while the review can still take the field (GH#698); data from before that
    // check (forced here) still has the request refused at the final approval.
    let d = w.ok("GET", &format!("{DEFS}/{}", w.definition), json!(null)).await;
    let body = json!({ "version": d["version"], "isActive": true });
    let (status, v) = w.call(&w.admin, "PATCH", &format!("{DEFS}/{}", w.definition), Some(body)).await;
    assert_eq!((status, details(&v)), (409, pairs(&[("stateAttributeId", "state_field_in_transition")])), "{v}");
    sqlx::query("UPDATE workflow_definitions SET is_active = true WHERE id = $1")
        .bind(w.definition)
        .execute(&w.pool)
        .await
        .unwrap();
    let (status, v) = decide(&w, &cab.0, instance, "approve", None).await;
    assert_eq!((status, code(&v)), (409, "WORKFLOW_APPROVAL_STALE"), "{v}");
    assert_eq!(details(&v), pairs(&[("fields.lifecycle", "state_field")]), "{v}");
    assert_eq!(w.ci_values(ci).await["attributes"]["lifecycle"], before);
    assert_eq!(pending(&w, instance).await.2, "cab", "the request is still pending");
    db.drop().await;
}

// ---------------------------------------------------------------------------
// GH#664: a requester does not pick the approver of a field step
// ---------------------------------------------------------------------------

/// A world whose `approve` (ready → approved) needs one approval by the Person
/// in the server's `owner` field, and whose `go_live` (ready → done) needs a
/// technical check and then the owner. `prepare` (planned → ready) and
/// `finish` (approved → done) are not gated.
struct Owners {
    w: World,
    /// Granted every transition; may edit servers and view people.
    req: (Creds, Uuid),
    /// The same rights as req.
    req2: (Creds, Uuid),
    /// Edits servers and views people; granted nothing.
    ed: (Creds, Uuid),
    /// The technical check of go_live.
    tech: (Creds, Uuid),
    /// Server viewers whose Persons can be named in `owner`.
    owner: (Creds, Uuid),
    pal: (Creds, Uuid),
    owner_person: Uuid,
    pal_person: Uuid,
    /// Edits servers and views people (for a token of req).
    editor_profile: Uuid,
}

async fn owners(db: &scratch::Scratch) -> Owners {
    let w = world(db).await;
    let person: Uuid =
        sqlx::query_scalar("SELECT id FROM ci_classes WHERE system_role = 'person'").fetch_one(&w.pool).await.unwrap();
    w.ok(
        "POST",
        "/api/v1/attribute-definitions",
        json!({ "classId": w.server, "key": "owner", "label": "Owner", "dataType": "reference",
            "referenceClassId": person }),
    )
    .await;
    let list: Uuid = sqlx::query_scalar("SELECT list_id FROM lookup_list_values WHERE id = $1")
        .bind(w.value("planned"))
        .fetch_one(&w.pool)
        .await
        .unwrap();
    w.ok("POST", "/api/v1/lookup-list-values", json!({ "listId": list, "key": "ready", "name": "ready" })).await;
    let step = |key: &str| json!({ "key": key, "name": key, "requiredApprovals": 1 });
    publish(
        &w,
        json!({
            "initialState": "planned",
            "states": [
                { "key": "planned", "name": "Planned", "category": "open", "stateValue": "planned" },
                { "key": "ready", "name": "Ready", "category": "active", "stateValue": "ready" },
                { "key": "approved", "name": "Approved", "category": "active", "stateValue": "approved" },
                { "key": "done", "name": "Done", "category": "done", "terminal": true, "stateValue": "live" }
            ],
            "transitions": [
                { "key": "prepare", "name": "Prepare", "from": "planned", "to": "ready" },
                { "key": "approve", "name": "Approve", "from": "ready", "to": "approved",
                  "approval": { "steps": [ step("owner") ] } },
                { "key": "go_live", "name": "Go live", "from": "ready", "to": "done",
                  "approval": { "steps": [ step("check"), step("owner") ] } },
                { "key": "finish", "name": "Finish", "from": "approved", "to": "done" }
            ]
        }),
    )
    .await;
    let def = w.ok("GET", &format!("{DEFS}/{}", w.definition), json!(null)).await;
    let v = w
        .ok(
            "PUT",
            &format!("{DEFS}/{}/grants", w.definition),
            json!({ "version": def["version"], "grants": [
                { "transitionKey": "prepare", "profiles": ["Approvers"] },
                { "transitionKey": "approve", "profiles": ["Approvers"] },
                { "transitionKey": "go_live", "profiles": ["Approvers"] },
                { "transitionKey": "_cancel", "profiles": ["Approvers"] }
            ] }),
        )
        .await;
    let tech = w.profile("Tech", &[(w.server, false)]).await;
    let viewers = w.profile("Viewers", &[(w.server, false)]).await;
    let people = w.profile("People", &[(person, false)]).await;
    let editor_profile = w.profile("Server and people editors", &[(w.server, true), (person, false)]).await;
    let owner = json!({ "source": "ci_attribute", "attribute": "owner" });
    let field = |t: &str| {
        let mut a = owner.clone();
        a["transitionKey"] = json!(t);
        a["stepKey"] = json!("owner");
        a
    };
    w.ok(
        "PUT",
        &format!("{DEFS}/{}/approvers", w.definition),
        json!({ "version": v["version"], "approvers": [
            field("approve"), field("go_live"),
            { "transitionKey": "go_live", "stepKey": "check", "source": "profile", "profile": "Tech" }
        ] }),
    )
    .await;
    let req = w.user("req", &[w.approvers, people]).await;
    let req2 = w.user("req2", &[w.approvers, people]).await;
    let ed = w.user("ed", &[w.editors, people]).await;
    let tech = w.user("tech", &[tech]).await;
    let owner = w.user("owner", &[viewers]).await;
    let pal = w.user("pal", &[viewers]).await;
    let person_of = |u: Uuid| {
        let pool = w.pool.clone();
        async move {
            let p: Option<Uuid> = sqlx::query_scalar("SELECT person_ci_id FROM users WHERE id = $1")
                .bind(u)
                .fetch_one(&pool)
                .await
                .unwrap();
            p.expect("a user gets a Person")
        }
    };
    let (owner_person, pal_person) = (person_of(owner.1).await, person_of(pal.1).await);
    Owners { w, req, req2, ed, tech, owner, pal, owner_person, pal_person, editor_profile }
}

impl Owners {
    /// Sets the server's owner as `creds`.
    async fn set_owner(&self, creds: &Creds, ci: Uuid, person: Uuid) {
        self.patch(creds, ci, json!({ "owner": person })).await;
    }

    async fn patch(&self, creds: &Creds, ci: Uuid, attributes: Value) {
        let version = self.w.ci_values(ci).await["version"].clone();
        let body = json!({ "version": version, "attributes": attributes });
        let (status, v) = self.w.call(creds, "PATCH", &format!("/api/v1/configuration-items/{ci}"), Some(body)).await;
        assert_eq!(status, 200, "{v}");
    }

    /// A server owned by owner's Person (set by the administrator), with an
    /// instance in state `ready`; returns (CI, instance).
    async fn ready(&self) -> (Uuid, Uuid) {
        let (ci, instance) = started(&self.w).await;
        self.set_owner(&self.w.admin, ci, self.owner_person).await;
        self.prepare(&self.w.admin, instance).await;
        (ci, instance)
    }

    async fn prepare(&self, creds: &Creds, instance: Uuid) {
        let (status, v) = request(&self.w, creds, instance, "prepare", json!({})).await;
        assert_eq!((status, v["state"]["key"].as_str()), (200, Some("ready")), "{v}");
    }

    /// The pending request of `instance` as the administrator sees it.
    async fn view(&self, instance: Uuid) -> Value {
        let (request, _, _) = pending(&self.w, instance).await;
        let (status, v) = self.w.call(&self.w.admin, "GET", &format!("{REQUESTS}/{request}"), None).await;
        assert_eq!(status, 200, "{v}");
        v
    }
}

/// The `(reason, actorName)` of each source a step dropped.
fn dropped(step: &Value) -> Vec<(String, String)> {
    step["droppedSources"]
        .as_array()
        .unwrap_or_else(|| panic!("no droppedSources: {step}"))
        .iter()
        .map(|d| {
            (
                d["reason"].as_str().unwrap().to_owned(),
                d["fieldLastChanged"]["actorName"].as_str().unwrap_or("").to_owned(),
            )
        })
        .collect()
}

fn by(reason: &str, actor: &str) -> Vec<(String, String)> {
    vec![(reason.to_owned(), actor.to_owned())]
}

/// GH#664 as reported: the requester points the owner field at a colleague,
/// then requests. The colleague may not decide, and the request and the
/// approver preview say why.
#[tokio::test]
async fn a_requester_who_set_the_owner_field_does_not_pick_the_approver() {
    let Some(db) = scratch::database("workflow_approvals_field_repro").await else { return };
    let o = owners(&db).await;
    let w = &o.w;
    let (ci, instance) = o.ready().await;
    o.set_owner(&o.req.0, ci, o.pal_person).await;
    let (status, v) = request(w, &o.req.0, instance, "approve", json!({})).await;
    assert_eq!(status, 202, "{v}");
    let (status, v) = decide(w, &o.pal.0, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, refused("FORBIDDEN", "not_eligible")), "{v}");
    let v = o.view(instance).await;
    let step = &v["steps"][0];
    assert_eq!(dropped(step), by("field_set_by_requester", "req"), "{v}");
    let d = &step["droppedSources"][0];
    assert_eq!((d["source"].as_str(), d["label"].as_str()), (Some("ci_attribute"), Some("field server.owner")));
    assert_eq!(
        (d["fieldLastChanged"]["actorType"].as_str(), d["fieldLastChanged"]["actorId"].clone()),
        (Some("user"), json!(o.req.1))
    );
    assert!(d["message"].as_str().unwrap().contains("req set field server.owner"), "{d}");
    assert_eq!(v["approvers"], json!([]), "nobody is left: {v}");

    // The preview for req tells the same; for anyone else the field resolves to pal.
    let preview = format!("{DEFS}/{}/approvers/preview?transition=approve&step=owner&ciId={ci}", w.definition);
    let (status, v) = w.call(&w.admin, "GET", &format!("{preview}&requestedBy={}", o.req.1), None).await;
    assert_eq!(status, 200, "{v}");
    let source = &v["sources"][0];
    assert_eq!(
        (source["dropped"].as_str(), source["userCount"].as_i64(), v["eligibleCount"].as_i64()),
        (Some("field_set_by_requester"), Some(0), Some(0)),
        "{v}"
    );
    assert!(source["note"].as_str().unwrap().contains("Separation of duties"), "{source}");
    assert_eq!(source["fieldLastChanged"]["actorName"], "req");
    let (_, v) = w.call(&w.admin, "GET", &format!("{preview}&requestedBy={}", o.req2.1), None).await;
    assert_eq!((v["sources"][0]["dropped"].clone(), v["eligibleCount"].as_i64()), (Value::Null, Some(1)), "{v}");
    audit_ok(w).await;
    db.drop().await;
}

/// Changing state between the edit and the request does not launder it: who
/// set the field counts, however long ago.
#[tokio::test]
async fn moving_the_ci_on_before_requesting_does_not_launder_the_edit() {
    let Some(db) = scratch::database("workflow_approvals_field_state").await else { return };
    let o = owners(&db).await;
    let w = &o.w;
    let (ci, instance) = started(w).await;
    o.set_owner(&w.admin, ci, o.owner_person).await;
    o.set_owner(&o.req.0, ci, o.pal_person).await;
    o.prepare(&o.req.0, instance).await;
    assert_eq!(request(w, &o.req.0, instance, "approve", json!({})).await.0, 202);
    let (status, v) = decide(w, &o.pal.0, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, refused("FORBIDDEN", "not_eligible")), "{v}");
    assert_eq!(dropped(&o.view(instance).await["steps"][0]), by("field_set_by_requester", "req"));
    db.drop().await;
}

/// An edit through the requester's API token is theirs; so is an edit by the
/// creator of the token a request is made with.
#[tokio::test]
async fn an_edit_through_a_token_counts_as_its_owner_and_its_creator() {
    let Some(db) = scratch::database("workflow_approvals_field_token").await else { return };
    let o = owners(&db).await;
    let w = &o.w;
    let (ci, instance) = o.ready().await;
    let (own, _) = token(w, o.req.1, o.editor_profile, Some(o.req.1)).await;
    o.set_owner(&own, ci, o.pal_person).await;
    assert_eq!(request(w, &o.req.0, instance, "approve", json!({})).await.0, 202);
    let (status, v) = decide(w, &o.pal.0, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, refused("FORBIDDEN", "not_eligible")), "{v}");
    let v = o.view(instance).await;
    assert_eq!(dropped(&v["steps"][0]), by("field_set_by_requester", "req"));
    assert_eq!(v["steps"][0]["droppedSources"][0]["fieldLastChanged"]["actorType"], "api_client");

    // req sets the field, req2 requests through a token req minted for them.
    let (ci, instance) = o.ready().await;
    o.set_owner(&o.req.0, ci, o.pal_person).await;
    let (lent, _) = token(w, o.req2.1, w.approvers, Some(o.req.1)).await;
    assert_eq!(request(w, &lent, instance, "approve", json!({})).await.0, 202);
    let (status, v) = decide(w, &o.pal.0, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, refused("FORBIDDEN", "not_eligible")), "{v}");
    assert_eq!(dropped(&o.view(instance).await["steps"][0]), by("field_set_by_requester", "req"));
    db.drop().await;
}

/// A bulk import writes as its owner, so it counts as theirs; an import that
/// recorded no user (discovery) counts as nobody's, and the field is not used.
#[tokio::test]
async fn an_edit_through_an_import_counts_and_an_unattributed_one_is_not_trusted() {
    use crate::api::context::RequestContext;
    use crate::auth::{Credential, Principal};
    let Some(db) = scratch::database("workflow_approvals_field_import").await else { return };
    let o = owners(&db).await;
    let w = &o.w;
    let body: crate::modules::items::schemas::UpdateItemBody =
        serde_json::from_value(json!({ "attributes": { "owner": o.pal_person } })).unwrap();

    // As the import commit writes a row (imports::dry_run::owner_context).
    let (ci, instance) = o.ready().await;
    let mut conn = w.pool.acquire().await.unwrap();
    let permissions = crate::data::auth::load_permissions(&mut conn, o.req.1).await.unwrap();
    drop(conn);
    let principal = Principal {
        user_id: o.req.1,
        username: "req".into(),
        credential: Credential::Token { profile_id: None, creator_id: None, token_id: None, minted_by: None },
        permissions,
    };
    let ctx = RequestContext::import_for_user(std::sync::Arc::new(principal), Uuid::new_v4());
    crate::modules::items::service::update(&w.pool, &ctx, ci, &body).await.unwrap();
    assert_eq!(request(w, &o.req.0, instance, "approve", json!({})).await.0, 202);
    let (status, v) = decide(w, &o.pal.0, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, refused("FORBIDDEN", "not_eligible")), "{v}");
    let v = o.view(instance).await;
    assert_eq!(dropped(&v["steps"][0]), by("field_set_by_requester", "req"));
    assert_eq!(v["steps"][0]["droppedSources"][0]["fieldLastChanged"]["actorType"], "import");

    // A discovery run: no user recorded.
    let (ci, instance) = o.ready().await;
    let ctx = RequestContext::import("discovery", "run-1");
    crate::modules::items::service::update(&w.pool, &ctx, ci, &body).await.unwrap();
    assert_eq!(request(w, &o.req.0, instance, "approve", json!({})).await.0, 202);
    let (status, v) = decide(w, &o.pal.0, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, refused("FORBIDDEN", "not_eligible")), "{v}");
    let v = o.view(instance).await;
    assert_eq!(dropped(&v["steps"][0]), by("field_set_by_unattributed", "discovery"), "{v}");
    assert_eq!(v["steps"][0]["droppedSources"][0]["fieldLastChanged"]["actorId"], Value::Null);
    db.drop().await;
}

/// Someone else saving the CI, even with the same owner sent again, does not
/// hide who set the field; an administrator's refresh judges it the same way.
#[tokio::test]
async fn a_later_save_that_keeps_the_value_does_not_hide_who_set_it() {
    let Some(db) = scratch::database("workflow_approvals_field_resave").await else { return };
    let o = owners(&db).await;
    let w = &o.w;
    let (ci, instance) = o.ready().await;
    o.set_owner(&o.req.0, ci, o.pal_person).await;
    o.patch(&o.ed.0, ci, json!({ "owner_team": "ops" })).await;
    o.patch(&o.ed.0, ci, json!({ "owner": o.pal_person, "owner_team": "ops2" })).await;
    assert_eq!(request(w, &o.req.0, instance, "approve", json!({})).await.0, 202);
    let (status, v) = decide(w, &o.pal.0, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, refused("FORBIDDEN", "not_eligible")), "{v}");
    assert_eq!(dropped(&o.view(instance).await["steps"][0]), by("field_set_by_requester", "req"));
    let (request_id, _, _) = pending(w, instance).await;
    let (status, v) = w.call(&w.admin, "POST", &format!("{REQUESTS}/{request_id}/refresh"), None).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(dropped(&v["steps"][0]), by("field_set_by_requester", "req"), "{v}");
    let (status, v) = decide(w, &o.pal.0, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, refused("FORBIDDEN", "not_eligible")), "{v}");
    let refresh: Value = sqlx::query_scalar(
        "SELECT new_value FROM audit_log WHERE action = 'workflow.approval_refresh' AND entity_id = $1",
    )
    .bind(ci)
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!(refresh["droppedSources"][0]["reason"], "field_set_by_requester", "{refresh}");
    audit_ok(w).await;
    db.drop().await;
}

/// Every step's approvers are fixed when the request is made: pointing the
/// field at someone else while step 1 runs changes nothing for step 2.
#[tokio::test]
async fn an_edit_while_the_request_is_pending_does_not_change_a_later_step() {
    let Some(db) = scratch::database("workflow_approvals_field_pending").await else { return };
    let o = owners(&db).await;
    let w = &o.w;
    let (ci, instance) = o.ready().await;
    assert_eq!(request(w, &o.req.0, instance, "go_live", json!({})).await.0, 202);
    let v = o.view(instance).await;
    assert_eq!(v["steps"][1]["eligibleCount"], 1, "step 2 is resolved with the request: {v}");
    o.set_owner(&o.req.0, ci, o.pal_person).await;
    let (status, v) = decide(w, &o.tech.0, instance, "approve", None).await;
    assert_eq!((status, v["instance"]["pendingApproval"]["stepKey"].as_str()), (200, Some("owner")), "{v}");
    assert_eq!(v["request"]["steps"][1]["droppedSources"], json!([]), "{v}");
    let (status, v) = decide(w, &o.pal.0, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, refused("FORBIDDEN", "not_eligible")), "{v}");
    let (_, v) = w.call(&o.owner.0, "GET", &format!("{REQUESTS}/{}", pending(w, instance).await.0), None).await;
    assert_eq!(v["approvers"][0]["fieldLastChanged"]["actorName"], "admin", "{v}");
    let (status, v) = decide(w, &o.owner.0, instance, "approve", None).await;
    assert_eq!((status, v["instance"]["state"]["key"].as_str()), (200, Some("done")), "{v}");
    audit_ok(w).await;
    db.drop().await;
}

/// When the dropped field was the step's only source, the request still
/// exists but nobody can decide it: the step is understaffed, and a manager
/// can cancel it.
#[tokio::test]
async fn a_step_left_without_approvers_is_understaffed() {
    let Some(db) = scratch::database("workflow_approvals_field_empty").await else { return };
    let o = owners(&db).await;
    let w = &o.w;
    let (ci, instance) = o.ready().await;
    o.set_owner(&o.req.0, ci, o.pal_person).await;
    let (status, v) = request(w, &o.req.0, instance, "approve", json!({})).await;
    assert_eq!(status, 202, "{v}");
    let v = o.view(instance).await;
    let step = &v["steps"][0];
    assert_eq!(
        (step["eligibleCount"].as_i64(), step["understaffed"].as_bool(), step["status"].as_str()),
        (Some(0), Some(true), Some("active")),
        "{v}"
    );
    for (who, creds) in [("pal", &o.pal.0), ("owner", &o.owner.0), ("req", &o.req.0)] {
        let (status, _) = decide(w, creds, instance, "approve", None).await;
        assert_eq!(status, 403, "{who}");
    }
    let (request_id, version, _) = pending(w, instance).await;
    let body = json!({ "expectedVersion": version, "comment": "owner set by the requester" });
    let (status, v) = w.call(&w.admin, "POST", &format!("{REQUESTS}/{request_id}/cancel"), Some(body)).await;
    assert_eq!((status, v["request"]["status"].as_str()), (200, Some("cancelled")), "{v}");
    let audit: Value = sqlx::query_scalar(
        "SELECT new_value FROM audit_log WHERE action = 'workflow.approval_request' AND entity_id = $1",
    )
    .bind(ci)
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!(audit["steps"][0]["eligibleCount"], 0, "{audit}");
    assert_eq!(audit["steps"][0]["droppedSources"][0]["reason"], "field_set_by_requester", "{audit}");
    audit_ok(w).await;
    db.drop().await;
}

/// Control: a field someone else set names the approver as before, and the
/// request says who set it.
#[tokio::test]
async fn a_field_set_by_someone_else_still_names_the_approver() {
    let Some(db) = scratch::database("workflow_approvals_field_other").await else { return };
    let o = owners(&db).await;
    let w = &o.w;
    let (ci, instance) = o.ready().await;
    o.set_owner(&o.ed.0, ci, o.pal_person).await;
    assert_eq!(request(w, &o.req.0, instance, "approve", json!({})).await.0, 202);
    let v = o.view(instance).await;
    assert_eq!(v["steps"][0]["droppedSources"], json!([]), "{v}");
    let changed = &v["approvers"][0]["fieldLastChanged"];
    assert_eq!((changed["actorName"].as_str(), changed["actorId"].clone()), (Some("ed"), json!(o.ed.1)), "{v}");
    let (status, v) = decide(w, &o.pal.0, instance, "approve", None).await;
    assert_eq!((status, v["instance"]["state"]["key"].as_str()), (200, Some("approved")), "{v}");
    audit_ok(w).await;
    db.drop().await;
}

/// Control: a field with no audit history (older than the log, or pruned)
/// keeps its approvers, so approvals on older CIs go on working.
#[tokio::test]
async fn a_field_without_audit_history_keeps_its_approvers() {
    let Some(db) = scratch::database("workflow_approvals_field_history").await else { return };
    let o = owners(&db).await;
    let w = &o.w;
    let (ci, instance) = started(w).await;
    o.prepare(&w.admin, instance).await;
    // Written straight into the type table, as a value older than the log.
    let (schema, table): (String, String) = sqlx::query_as(
        "SELECT table_schema::text, table_name::text FROM information_schema.columns
         WHERE column_name = 'owner' AND table_schema <> 'cmdb' AND table_name NOT LIKE 'v\\_%'",
    )
    .fetch_one(&w.pool)
    .await
    .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(r#"UPDATE "{schema}"."{table}" SET owner = $2 WHERE id = $1"#)))
        .bind(ci)
        .bind(o.pal_person)
        .execute(&w.pool)
        .await
        .unwrap();
    assert_eq!(w.ci_values(ci).await["attributes"]["owner"], json!(o.pal_person.to_string()));
    assert_eq!(request(w, &o.req.0, instance, "approve", json!({})).await.0, 202);
    let v = o.view(instance).await;
    assert_eq!(
        (v["steps"][0]["droppedSources"].clone(), v["approvers"][0]["fieldLastChanged"].clone()),
        (json!([]), Value::Null),
        "{v}"
    );
    let (status, v) = decide(w, &o.pal.0, instance, "approve", None).await;
    assert_eq!((status, v["instance"]["state"]["key"].as_str()), (200, Some("approved")), "{v}");
    db.drop().await;
}

// ---------------------------------------------------------------------------
// GH#709: an edit through a token the requester minted for someone else
// ---------------------------------------------------------------------------

/// GH#709 as reported: req mints a token for ed, sets the owner field with
/// it, and requests in their own session. The edit is req's too.
#[tokio::test]
async fn an_edit_through_a_token_the_requester_minted_for_someone_else_is_theirs() {
    let Some(db) = scratch::database("workflow_approvals_field_lent_token").await else { return };
    let o = owners(&db).await;
    let w = &o.w;
    let (ci, instance) = o.ready().await;
    let (lent, _) = token(w, o.ed.1, o.editor_profile, Some(o.req.1)).await;
    o.set_owner(&lent, ci, o.pal_person).await;
    assert_eq!(request(w, &o.req.0, instance, "approve", json!({})).await.0, 202);
    let (status, v) = decide(w, &o.pal.0, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, refused("FORBIDDEN", "not_eligible")), "{v}");
    let v = o.view(instance).await;
    assert_eq!(dropped(&v["steps"][0]), by("field_set_by_requester", "ed"), "{v}");
    let d = &v["steps"][0]["droppedSources"][0];
    assert_eq!(
        (d["fieldLastChanged"]["actorType"].as_str(), d["fieldLastChanged"]["tokenCreatedBy"].clone()),
        (Some("api_client"), json!([o.req.1])),
        "{d}"
    );
    assert!(d["message"].as_str().unwrap().contains("with an API token minted for them"), "{d}");

    // The approver preview for req says the same; for req2 the field names pal.
    let preview = format!("{DEFS}/{}/approvers/preview?transition=approve&step=owner&ciId={ci}", w.definition);
    let (_, v) = w.call(&w.admin, "GET", &format!("{preview}&requestedBy={}", o.req.1), None).await;
    assert_eq!(
        (v["sources"][0]["dropped"].as_str(), v["eligibleCount"].as_i64()),
        (Some("field_set_by_requester"), Some(0))
    );
    let (_, v) = w.call(&w.admin, "GET", &format!("{preview}&requestedBy={}", o.req2.1), None).await;
    assert_eq!((v["sources"][0]["dropped"].clone(), v["eligibleCount"].as_i64()), (Value::Null, Some(1)), "{v}");
    audit_ok(w).await;
    db.drop().await;
}

/// The token is the one the edit's request used: another token of the same
/// owner that req minted does not taint an edit made with a token req did not
/// mint. Without the `token.use` row (pruned), every minter of the owner's
/// tokens counts.
#[tokio::test]
async fn the_token_used_decides_and_a_pruned_token_use_row_fails_closed() {
    use crate::api::context::RequestContext;
    use crate::auth::{Credential, Principal};
    let Some(db) = scratch::database("workflow_approvals_field_token_pick").await else { return };
    let o = owners(&db).await;
    let w = &o.w;
    let (_lent, lent_id) = token(w, o.ed.1, o.editor_profile, Some(o.req.1)).await;
    let (own, _) = token(w, o.ed.1, o.editor_profile, None).await;

    // Through ed's other token: the token.use row names it, so req is not involved.
    let (ci, instance) = o.ready().await;
    o.set_owner(&own, ci, o.pal_person).await;
    assert_eq!(request(w, &o.req.0, instance, "approve", json!({})).await.0, 202);
    let v = o.view(instance).await;
    assert_eq!(v["steps"][0]["droppedSources"], json!([]), "{v}");
    assert_eq!(v["approvers"][0]["fieldLastChanged"].get("tokenCreatedBy"), None, "{v}");
    let (status, v) = decide(w, &o.pal.0, instance, "approve", None).await;
    assert_eq!((status, v["instance"]["state"]["key"].as_str()), (200, Some("approved")), "{v}");

    // A token edit whose token.use row is gone (as after `prune-audit --scope auth`).
    let (ci, instance) = o.ready().await;
    let mut conn = w.pool.acquire().await.unwrap();
    let permissions = crate::data::auth::load_permissions(&mut conn, o.ed.1).await.unwrap();
    drop(conn);
    let principal = Principal {
        user_id: o.ed.1,
        username: "ed".into(),
        credential: Credential::Token {
            profile_id: Some(o.editor_profile),
            creator_id: Some(o.req.1),
            token_id: Some(lent_id),
            minted_by: Some(o.req.1),
        },
        permissions,
    };
    let ctx = RequestContext::token(std::sync::Arc::new(principal), "no-token-use-row".into());
    let body: crate::modules::items::schemas::UpdateItemBody =
        serde_json::from_value(json!({ "attributes": { "owner": o.pal_person } })).unwrap();
    crate::modules::items::service::update(&w.pool, &ctx, ci, &body).await.unwrap();
    assert_eq!(request(w, &o.req.0, instance, "approve", json!({})).await.0, 202);
    let (status, v) = decide(w, &o.pal.0, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, refused("FORBIDDEN", "not_eligible")), "{v}");
    let v = o.view(instance).await;
    assert_eq!(dropped(&v["steps"][0]), by("field_set_by_requester", "ed"), "{v}");
    assert_eq!(v["steps"][0]["droppedSources"][0]["fieldLastChanged"]["tokenCreatedBy"], json!([o.req.1]));
    db.drop().await;
}

// ---------------------------------------------------------------------------
// GH#708: a requester does not pick the approver of a service owner step
// ---------------------------------------------------------------------------

/// Staffs `approve`'s owner step with the technical owners of the server's
/// business services and lets req edit business services; returns the
/// business service type.
async fn service_owner_step(o: &Owners) -> Uuid {
    let w = &o.w;
    let bs: Uuid = sqlx::query_scalar("SELECT id FROM ci_classes WHERE system_role = 'business_service'")
        .fetch_one(&w.pool)
        .await
        .unwrap();
    let def = w.ok("GET", &format!("{DEFS}/{}", w.definition), json!(null)).await;
    w.ok(
        "PUT",
        &format!("{DEFS}/{}/approvers", w.definition),
        json!({ "version": def["version"], "approvers": [
            { "transitionKey": "approve", "stepKey": "owner", "source": "service_owner", "serviceOwnerRole": "technical" }
        ] }),
    )
    .await;
    let managers = w.profile("Service managers", &[(bs, true)]).await;
    sqlx::query("INSERT INTO user_permission_profiles (user_id, profile_id) VALUES ($1, $2)")
        .bind(o.req.1)
        .bind(managers)
        .execute(&w.pool)
        .await
        .unwrap();
    bs
}

impl Owners {
    /// A business service, created by the administrator.
    async fn service(&self, bs: Uuid) -> Uuid {
        let body = json!({ "classId": bs, "attributes": { "name": "Shop" } });
        id(&self.w.ok("POST", "/api/v1/configuration-items", body).await)
    }

    /// Sets the technical owners of business service `service` as `creds`.
    async fn set_owners(&self, creds: &Creds, service: Uuid, users: &[Uuid]) {
        let w = &self.w;
        let path = format!("/api/v1/business-services/{service}");
        let version = w.ok("GET", &path, json!(null)).await["version"].clone();
        let technical: Vec<Value> = users.iter().map(|u| json!({ "kind": "user", "id": u })).collect();
        let body = json!({ "version": version, "technical": technical, "business": [] });
        let (status, v) = w.call(creds, "PUT", &format!("{path}/owners"), Some(body)).await;
        assert_eq!(status, 200, "{v}");
    }

    /// Adds CI `ci` to business service `service` as `creds`.
    async fn add_member(&self, creds: &Creds, service: Uuid, ci: Uuid) {
        let path = format!("/api/v1/business-services/{service}/members");
        let (status, v) = self.w.call(creds, "POST", &path, Some(json!({ "memberIds": [ci] }))).await;
        assert_eq!(status, 200, "{v}");
    }
}

/// The `(reason, label)` of each source or part a step dropped.
fn dropped_labels(step: &Value) -> Vec<(String, String)> {
    step["droppedSources"]
        .as_array()
        .unwrap_or_else(|| panic!("no droppedSources: {step}"))
        .iter()
        .map(|d| (d["reason"].as_str().unwrap().to_owned(), d["label"].as_str().unwrap().to_owned()))
        .collect()
}

/// GH#708 as reported, first half: req names pal the technical owner of a
/// service the CI is in. pal may not decide; the request and the preview say
/// why.
#[tokio::test]
async fn a_requester_who_named_the_service_owner_does_not_pick_the_approver() {
    let Some(db) = scratch::database("workflow_approvals_owner_named").await else { return };
    let o = owners(&db).await;
    let w = &o.w;
    let bs = service_owner_step(&o).await;
    let (ci, instance) = o.ready().await;
    let service = o.service(bs).await;
    let name = w.ci_values(service).await["label"].as_str().unwrap().to_owned();
    o.add_member(&w.admin, service, ci).await;
    o.set_owners(&o.req.0, service, &[o.pal.1]).await;
    assert_eq!(request(w, &o.req.0, instance, "approve", json!({})).await.0, 202);
    let (status, v) = decide(w, &o.pal.0, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, refused("FORBIDDEN", "not_eligible")), "{v}");
    let v = o.view(instance).await;
    let step = &v["steps"][0];
    assert_eq!(
        dropped_labels(step),
        vec![("field_set_by_requester".to_owned(), format!("technical owner pal of business service {name}"))],
        "{v}"
    );
    assert_eq!(dropped(step), by("field_set_by_requester", "req"));
    assert_eq!(step["droppedSources"][0]["source"], "service_owner");
    assert!(step["droppedSources"][0]["message"].as_str().unwrap().contains("req made pal a technical owner"), "{v}");
    assert_eq!((step["eligibleCount"].as_i64(), v["approvers"].clone()), (Some(0), json!([])), "{v}");

    let preview = format!("{DEFS}/{}/approvers/preview?transition=approve&step=owner&ciId={ci}", w.definition);
    let (status, v) = w.call(&w.admin, "GET", &format!("{preview}&requestedBy={}", o.req.1), None).await;
    assert_eq!(status, 200, "{v}");
    let source = &v["sources"][0];
    assert_eq!(
        (source["dropped"].as_str(), source["userCount"].as_i64(), v["eligibleCount"].as_i64()),
        (Some("field_set_by_requester"), Some(0), Some(0)),
        "{v}"
    );
    assert_eq!(source["droppedParts"].as_array().map(Vec::len), Some(1), "{v}");
    assert!(source["note"].as_str().unwrap().contains("Separation of duties"), "{source}");
    let (_, v) = w.call(&w.admin, "GET", &format!("{preview}&requestedBy={}", o.req2.1), None).await;
    assert_eq!(
        (v["sources"][0]["dropped"].clone(), v["sources"][0]["droppedParts"].clone(), v["eligibleCount"].as_i64()),
        (Value::Null, json!([]), Some(1)),
        "{v}"
    );
    audit_ok(w).await;
    db.drop().await;
}

/// GH#708 second half: pal owns a service (set by the administrator) and req
/// adds the CI to it. The service's owners are not used.
#[tokio::test]
async fn a_requester_who_added_the_ci_to_a_service_does_not_pick_its_owners() {
    let Some(db) = scratch::database("workflow_approvals_owner_member").await else { return };
    let o = owners(&db).await;
    let w = &o.w;
    let bs = service_owner_step(&o).await;
    let (ci, instance) = o.ready().await;
    let service = o.service(bs).await;
    let name = w.ci_values(service).await["label"].as_str().unwrap().to_owned();
    o.set_owners(&w.admin, service, &[o.pal.1]).await;
    o.add_member(&o.req.0, service, ci).await;
    assert_eq!(request(w, &o.req.0, instance, "approve", json!({})).await.0, 202);
    let (status, v) = decide(w, &o.pal.0, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, refused("FORBIDDEN", "not_eligible")), "{v}");
    let v = o.view(instance).await;
    assert_eq!(
        dropped_labels(&v["steps"][0]),
        vec![("field_set_by_requester".to_owned(), format!("technical owners of business service {name}"))],
        "{v}"
    );
    assert!(
        v["steps"][0]["droppedSources"][0]["message"]
            .as_str()
            .unwrap()
            .contains("req added the CI to business service"),
        "{v}"
    );

    // The same through a token req minted for ed (GH#709 on the membership).
    let (ci, instance) = o.ready().await;
    let both = w.profile("Servers and services", &[(w.server, true), (bs, true)]).await;
    sqlx::query("INSERT INTO user_permission_profiles (user_id, profile_id) VALUES ($1, $2)")
        .bind(o.ed.1)
        .bind(both)
        .execute(&w.pool)
        .await
        .unwrap();
    let (lent, _) = token(w, o.ed.1, both, Some(o.req.1)).await;
    o.add_member(&lent, service, ci).await;
    assert_eq!(request(w, &o.req.0, instance, "approve", json!({})).await.0, 202);
    let (status, v) = decide(w, &o.pal.0, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, refused("FORBIDDEN", "not_eligible")), "{v}");
    assert_eq!(dropped(&o.view(instance).await["steps"][0]), by("field_set_by_requester", "ed"));
    audit_ok(w).await;
    db.drop().await;
}

/// Control: owners and memberships the administrator set name the approver
/// as before; an owner req adds next to them is dropped alone.
#[tokio::test]
async fn service_owners_and_memberships_set_by_someone_else_still_name_the_approver() {
    let Some(db) = scratch::database("workflow_approvals_owner_other").await else { return };
    let o = owners(&db).await;
    let w = &o.w;
    let bs = service_owner_step(&o).await;
    let (ci, instance) = o.ready().await;
    let service = o.service(bs).await;
    o.set_owners(&w.admin, service, &[o.pal.1]).await;
    o.add_member(&w.admin, service, ci).await;
    o.set_owners(&o.req.0, service, &[o.pal.1, o.owner.1]).await;
    assert_eq!(request(w, &o.req.0, instance, "approve", json!({})).await.0, 202);
    let v = o.view(instance).await;
    let step = &v["steps"][0];
    assert_eq!(step["eligibleCount"], 1, "pal only: {v}");
    let labels = dropped_labels(step);
    assert_eq!(labels.len(), 1, "{v}");
    assert!(labels[0].1.starts_with("technical owner owner of business service"), "{v}");
    let (status, v) = decide(w, &o.owner.0, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, refused("FORBIDDEN", "not_eligible")), "{v}");
    let (status, v) = decide(w, &o.pal.0, instance, "approve", None).await;
    assert_eq!((status, v["instance"]["state"]["key"].as_str()), (200, Some("approved")), "{v}");
    audit_ok(w).await;
    db.drop().await;
}

/// GH#708 edges (SHAA-2282): a requester who is a service owner (named by the
/// administrator) is refused on their own request while another owner still
/// decides, and an owner group set through a token the requester minted
/// (GH#709) is not used either.
#[tokio::test]
async fn a_requester_who_owns_the_service_is_refused_and_a_lent_token_group_owner_is_dropped() {
    let Some(db) = scratch::database("workflow_approvals_owner_self_group").await else { return };
    let o = owners(&db).await;
    let w = &o.w;
    let bs = service_owner_step(&o).await;
    let service = o.service(bs).await;
    let name = w.ci_values(service).await["label"].as_str().unwrap().to_owned();
    o.set_owners(&w.admin, service, &[o.req.1, o.pal.1]).await;

    // req owns the service: four-eyes refuses them, pal still approves.
    let (ci, instance) = o.ready().await;
    o.add_member(&w.admin, service, ci).await;
    assert_eq!(request(w, &o.req.0, instance, "approve", json!({})).await.0, 202);
    let (status, v) = decide(w, &o.req.0, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, refused("WORKFLOW_APPROVAL_SELF", "requester")), "{v}");
    let v = o.view(instance).await;
    assert_eq!(v["steps"][0]["droppedSources"], json!([]), "nothing set by req: {v}");
    let (status, v) = decide(w, &o.pal.0, instance, "approve", None).await;
    assert_eq!((status, v["instance"]["state"]["key"].as_str()), (200, Some("approved")), "{v}");

    // req mints a token for ed and makes a group with pal in it the owner.
    let (status, g) = w.call(&w.admin, "POST", "/api/v1/admin/groups", Some(json!({ "name": "Shop team" }))).await;
    assert_eq!(status, 201, "{g}");
    let group = id(&g);
    let body = json!({ "version": g["version"], "userIds": [o.pal.1] });
    let (status, v) = w.call(&w.admin, "PUT", &format!("/api/v1/admin/groups/{group}/members"), Some(body)).await;
    assert_eq!(status, 200, "{v}");
    let both = w.profile("Servers and services", &[(w.server, true), (bs, true)]).await;
    sqlx::query("INSERT INTO user_permission_profiles (user_id, profile_id) VALUES ($1, $2)")
        .bind(o.ed.1)
        .bind(both)
        .execute(&w.pool)
        .await
        .unwrap();
    let (lent, _) = token(w, o.ed.1, both, Some(o.req.1)).await;
    let path = format!("/api/v1/business-services/{service}");
    let version = w.ok("GET", &path, json!(null)).await["version"].clone();
    let body = json!({ "version": version, "technical": [{ "kind": "group", "id": group }], "business": [] });
    let (status, v) = w.call(&lent, "PUT", &format!("{path}/owners"), Some(body)).await;
    assert_eq!(status, 200, "{v}");

    let (ci, instance) = o.ready().await;
    o.add_member(&w.admin, service, ci).await;
    assert_eq!(request(w, &o.req.0, instance, "approve", json!({})).await.0, 202);
    let (status, v) = decide(w, &o.pal.0, instance, "approve", None).await;
    assert_eq!((status, reason(&v)), (403, refused("FORBIDDEN", "not_eligible")), "{v}");
    let v = o.view(instance).await;
    let step = &v["steps"][0];
    assert_eq!(
        dropped_labels(step),
        vec![(
            "field_set_by_requester".to_owned(),
            format!("technical owner group Shop team of business service {name}")
        )],
        "{v}"
    );
    assert_eq!(step["droppedSources"][0]["fieldLastChanged"]["tokenCreatedBy"], json!([o.req.1]), "{v}");
    assert_eq!(step["eligibleCount"].as_i64(), Some(0), "{v}");

    // req2 did not mint the token: the group names pal for their request.
    let (ci, instance) = o.ready().await;
    o.add_member(&w.admin, service, ci).await;
    assert_eq!(request(w, &o.req2.0, instance, "approve", json!({})).await.0, 202);
    let (status, v) = decide(w, &o.pal.0, instance, "approve", None).await;
    assert_eq!((status, v["instance"]["state"]["key"].as_str()), (200, Some("approved")), "{v}");
    audit_ok(w).await;
    db.drop().await;
}
