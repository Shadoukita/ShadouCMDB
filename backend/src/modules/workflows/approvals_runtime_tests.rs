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

const REQUESTS: &str = "/api/v1/workflow-approval-requests";

struct People {
    /// Requests: granted every transition, and also a member of Tech and CAB.
    req: (Creds, Uuid),
    /// A second requester (granted, in no approver profile).
    req2: (Creds, Uuid),
    /// Tech reviewer.
    tech: (Creds, Uuid),
    /// CAB members.
    a1: (Creds, Uuid),
    a2: (Creds, Uuid),
    a3: (Creds, Uuid),
    /// Eligible for the CAB step through a profile that may not view servers.
    blind: (Creds, Uuid),
}

/// Version 2 of the world's workflow: `approve` needs a technical review and
/// then two CAB approvals; `implement` one check; `review` one approval by
/// someone who took no part in `implement`.
async fn setup(w: &World) -> People {
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
