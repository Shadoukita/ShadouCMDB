//! The workflow and approval notification triggers of migration 0072
//! (SHAA-2356) through the real router: who hears of a request, of each step,
//! of the outcome and of a transition, cancel or force, and who does not.

use serde_json::{Value, json};
use uuid::Uuid;

use super::approvals_runtime_tests::{REQUESTS, decide, pending, request, setup, started};
use super::runtime_tests::{RUN, World, id, world};
use crate::db::scratch;
use crate::modules::api_tokens::tests::Creds;

/// `(kind, data)` of a user's notifications, oldest first, straight from the table.
async fn inbox(w: &World, user: Uuid) -> Vec<(String, Value)> {
    sqlx::query_as("SELECT kind, data FROM notifications WHERE user_id = $1 ORDER BY created_at, id")
        .bind(user)
        .fetch_all(&w.pool)
        .await
        .unwrap()
}

fn kinds(n: &[(String, Value)]) -> Vec<&str> {
    n.iter().map(|(k, _)| k.as_str()).collect()
}

async fn unread(w: &World, who: &Creds) -> i64 {
    let (status, v) = w.call(who, "GET", "/api/v1/notifications/unread-count", None).await;
    assert_eq!(status, 200, "{v}");
    v["unread"].as_i64().unwrap()
}

async fn admin_id(w: &World) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE username = 'admin'").fetch_one(&w.pool).await.unwrap()
}

/// Each step notifies its approvers when it becomes active, never the
/// requester; the outcome notifies the requester; the transition it applies
/// notifies whoever started the instance. A refused request notifies no one.
#[tokio::test]
async fn approvals_notify_each_step_the_requester_and_the_starter() {
    let Some(db) = scratch::database("workflow_notifications_approvals").await else { return };
    let w = world(&db).await;
    let p = setup(&w).await;
    let admin = admin_id(&w).await;
    let (ci, instance) = started(&w).await;

    // Refused at validation: rolled back, nobody hears of it.
    let (status, _) = request(&w, &p.req.0, instance, "approve", json!({})).await;
    assert_eq!(status, 422);
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM notifications").fetch_one(&w.pool).await.unwrap();
    assert_eq!(total, 0);

    // Step 1 (tech): the tech reviewer; not the requester, who is in Tech too.
    let (status, v) = request(&w, &p.req.0, instance, "approve", json!({ "owner_team": "ops" })).await;
    assert_eq!(status, 202, "{v}");
    let (request_id, _, _) = pending(&w, instance).await;
    let tech = inbox(&w, p.tech.1).await;
    assert_eq!(kinds(&tech), ["approval_requested"]);
    let d = &tech[0].1;
    assert_eq!(
        (d["stepKey"].as_str(), d["stepName"].as_str(), d["transitionName"].as_str(), d["requestedByName"].as_str()),
        (Some("tech"), Some("Technical review"), Some("Approve"), Some("req"))
    );
    assert_eq!(d["ciId"], json!(ci));
    assert_eq!(d["instanceId"], json!(instance));
    let row: (String, Uuid, Option<Uuid>) =
        sqlx::query_as("SELECT entity_type, entity_id, ci_id FROM notifications WHERE user_id = $1")
            .bind(p.tech.1)
            .fetch_one(&w.pool)
            .await
            .unwrap();
    assert_eq!(row, ("workflow_approval_requests".to_owned(), request_id, Some(ci)));
    for u in [p.req.1, p.a1.1, p.blind.1, admin] {
        assert_eq!(inbox(&w, u).await, vec![], "{u}");
    }
    assert_eq!(unread(&w, &p.tech.0).await, 1);

    // Step 2 (CAB): a1, a2, a3 and blind; blind may not view servers, so the API hides it.
    let (status, v) = decide(&w, &p.tech.0, instance, "approve", None).await;
    assert_eq!(status, 200, "{v}");
    for u in [p.a1.1, p.a2.1, p.a3.1, p.blind.1] {
        let n = inbox(&w, u).await;
        assert_eq!(kinds(&n), ["approval_requested"], "{u}");
        assert_eq!(n[0].1["stepKey"], "cab");
    }
    assert_eq!(kinds(&inbox(&w, p.tech.1).await), ["approval_requested"], "step 1's approver is not told again");
    assert_eq!(inbox(&w, p.req.1).await, vec![]);
    assert_eq!(unread(&w, &p.a1.0).await, 1);
    assert_eq!(unread(&w, &p.blind.0).await, 0, "the class check on read");

    // The quorum applies the transition: the requester hears the outcome, the starter the transition.
    decide(&w, &p.a1.0, instance, "approve", None).await;
    let (status, v) = decide(&w, &p.a2.0, instance, "approve", None).await;
    assert_eq!((status, v["request"]["status"].as_str()), (200, Some("approved")), "{v}");
    let req = inbox(&w, p.req.1).await;
    assert_eq!(kinds(&req), ["approval_closed"]);
    assert_eq!(
        (req[0].1["status"].as_str(), req[0].1["closeReason"].as_str(), req[0].1["closedByName"].as_str()),
        (Some("approved"), Some("approved"), Some("a2"))
    );
    let started_by = inbox(&w, admin).await;
    assert_eq!(kinds(&started_by), ["workflow_transition"]);
    let d = &started_by[0].1;
    assert_eq!(
        (d["event"].as_str(), d["transitionKey"].as_str(), d["fromStateName"].as_str(), d["toStateName"].as_str()),
        (Some("transition"), Some("approve"), Some("Planned"), Some("Approved"))
    );
    assert_eq!(d["actorName"], "a2");
    assert_eq!(kinds(&inbox(&w, p.a3.1).await), ["approval_requested"], "no outcome for an approver");

    // A rejection is an outcome too.
    let (_, other) = started(&w).await;
    request(&w, &p.req.0, other, "approve", json!({ "owner_team": "ops" })).await;
    decide(&w, &p.tech.0, other, "reject", Some("not now")).await;
    let req = inbox(&w, p.req.1).await;
    assert_eq!(kinds(&req), ["approval_closed", "approval_closed"]);
    assert_eq!(req[1].1["status"], "rejected");
    db.drop().await;
}

/// The requester's own withdrawal tells nobody; a manager's cancel tells the
/// requester. Someone else's cancel or force tells the starter, their own
/// does not, and nor does a transition the starter requested themselves.
#[tokio::test]
async fn own_actions_notify_no_one_and_others_notify_the_starter() {
    let Some(db) = scratch::database("workflow_notifications_own").await else { return };
    let w = world(&db).await;
    let p = setup(&w).await;
    let admin = admin_id(&w).await;
    let fields = json!({ "owner_team": "ops" });

    // Withdrawn by the requester: no outcome notification.
    let (_, instance) = started(&w).await;
    request(&w, &p.req.0, instance, "approve", fields.clone()).await;
    let (first, version, _) = pending(&w, instance).await;
    let (status, v) = w
        .call(&p.req.0, "POST", &format!("{REQUESTS}/{first}/withdraw"), Some(json!({ "expectedVersion": version })))
        .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(inbox(&w, p.req.1).await, vec![]);

    // Cancelled by a manager: the requester hears of it.
    request(&w, &p.req.0, instance, "approve", fields.clone()).await;
    let (second, version, _) = pending(&w, instance).await;
    let body = json!({ "expectedVersion": version, "comment": "superseded" });
    let (status, v) = w.call(&p.req2.0, "POST", &format!("{REQUESTS}/{second}/cancel"), Some(body)).await;
    assert_eq!(status, 200, "{v}");
    let req = inbox(&w, p.req.1).await;
    assert_eq!(kinds(&req), ["approval_closed"]);
    assert_eq!((req[0].1["status"].as_str(), req[0].1["closedByName"].as_str()), (Some("cancelled"), Some("req2")));

    // The administrator cancels and forces their own instances: nothing for them.
    let (_, v) = w.call(&w.admin, "GET", &format!("{RUN}/{instance}"), None).await;
    let body = json!({ "expectedVersion": v["instance"]["version"], "reason": "not needed" });
    let (status, v) = w.call(&w.admin, "POST", &format!("{RUN}/{instance}/cancel"), Some(body)).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(inbox(&w, admin).await, vec![]);

    // req2 starts two instances; the administrator cancels one and forces the other.
    let cancelled = start_as(&w, &p.req2.0).await;
    let forced = start_as(&w, &p.req2.0).await;
    let (_, v) = w.call(&w.admin, "GET", &format!("{RUN}/{cancelled}"), None).await;
    let body = json!({ "expectedVersion": v["instance"]["version"], "reason": "duplicate" });
    let (status, v) = w.call(&w.admin, "POST", &format!("{RUN}/{cancelled}/cancel"), Some(body)).await;
    assert_eq!(status, 200, "{v}");
    let (_, v) = w.call(&w.admin, "GET", &format!("{RUN}/{forced}"), None).await;
    let body = json!({ "expectedVersion": v["instance"]["version"], "stateKey": "approved", "reason": "emergency" });
    let (status, v) = w.call(&w.admin, "POST", &format!("{RUN}/{forced}/force"), Some(body)).await;
    assert_eq!(status, 200, "{v}");
    let req2 = inbox(&w, p.req2.1).await;
    let events: Vec<(&str, &str)> = req2.iter().map(|(k, d)| (k.as_str(), d["event"].as_str().unwrap())).collect();
    assert_eq!(events, [("workflow_transition", "cancel"), ("workflow_transition", "force")]);
    assert_eq!(req2[1].1["toStateKey"], "approved");
    assert_eq!(req2[1].1["actorName"], "admin");

    // req starts an instance and requests a transition; its approval tells req
    // the outcome only, not the transition as well.
    let own = start_as(&w, &p.req.0).await;
    request(&w, &p.req.0, own, "approve", fields).await;
    decide(&w, &p.tech.0, own, "approve", None).await;
    decide(&w, &p.a1.0, own, "approve", None).await;
    let (status, v) = decide(&w, &p.a2.0, own, "approve", None).await;
    assert_eq!((status, v["instance"]["state"]["key"].as_str()), (200, Some("approved")), "{v}");
    assert_eq!(kinds(&inbox(&w, p.req.1).await), ["approval_closed", "approval_closed"]);

    // A deactivated account gets nothing new.
    sqlx::query("UPDATE users SET is_active = false WHERE id = $1").bind(p.req2.1).execute(&w.pool).await.unwrap();
    let late = start_as_admin_for(&w, p.req2.1).await;
    let (_, v) = w.call(&w.admin, "GET", &format!("{RUN}/{late}"), None).await;
    let body = json!({ "expectedVersion": v["instance"]["version"], "reason": "inactive owner" });
    let (status, v) = w.call(&w.admin, "POST", &format!("{RUN}/{late}/cancel"), Some(body)).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(inbox(&w, p.req2.1).await.len(), 2);
    db.drop().await;
}

/// An instance `creds` starts on a new server.
async fn start_as(w: &World, creds: &Creds) -> Uuid {
    let ci = w.ci(w.server).await;
    let (status, v) = w.start(creds, ci).await;
    assert_eq!(status, 201, "{v}");
    id(&v["instance"])
}

/// An instance the administrator starts, recorded as started by `user`.
async fn start_as_admin_for(w: &World, user: Uuid) -> Uuid {
    let (_, instance) = started(w).await;
    sqlx::query("UPDATE workflow_instances SET started_by_id = $2 WHERE id = $1")
        .bind(instance)
        .bind(user)
        .execute(&w.pool)
        .await
        .unwrap();
    instance
}
