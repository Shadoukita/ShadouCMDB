//! GH#839: approver assignments and action recipients are no way round
//! `GET /principals`. A `workflows.manage` holder who may not look up users
//! and groups gets one answer for a user or group given by name, whether or
//! not it exists, and previews with counts but no users.

use serde_json::{Value, json};
use uuid::Uuid;

use super::runtime_tests::{DEFS, World, world};
use crate::db::scratch;
use crate::modules::api_tokens::tests::Creds;

/// What a refusal tells: the status and the error without its request id.
fn answer(status: u16, v: &Value) -> (u16, Value) {
    let mut e = v["error"].clone();
    if let Some(o) = e.as_object_mut() {
        o.remove("requestId");
    }
    (status, e)
}

async fn grant(w: &World, profile: Uuid, permission: &str) {
    sqlx::query("INSERT INTO permission_profile_global_permissions (profile_id, permission) VALUES ($1, $2)")
        .bind(profile)
        .bind(permission)
        .execute(&w.pool)
        .await
        .unwrap();
}

async fn version(w: &World) -> Value {
    w.ok("GET", &format!("{DEFS}/{}", w.definition), json!(null)).await["version"].clone()
}

async fn put_approvers(w: &World, creds: &Creds, approvers: Value) -> (u16, Value) {
    let body = json!({ "version": version(w).await, "approvers": approvers });
    w.call(creds, "PUT", &format!("{DEFS}/{}/approvers", w.definition), Some(body)).await
}

async fn put_actions(w: &World, creds: &Creds, recipients: Value) -> (u16, Value) {
    let action = json!({ "key": "tell", "name": "Tell", "kind": "inbox", "trigger": "transition",
        "transition": "approve", "recipients": recipients });
    let body = json!({ "version": version(w).await, "actions": [action] });
    w.call(creds, "PUT", &format!("{DEFS}/{}/actions", w.definition), Some(body)).await
}

fn usernames(v: &Value) -> Vec<String> {
    v["users"].as_array().unwrap().iter().map(|u| u["username"].as_str().unwrap().to_owned()).collect()
}

#[tokio::test]
async fn directory_lookups_need_the_principals_right() {
    let Some(db) = scratch::database("workflow_directory_gate").await else { return };
    let w = world(&db).await;
    // A step to staff: approve gets an approval policy in the draft.
    let draft = json!({
        "initialState": "planned",
        "states": [
            { "key": "planned", "name": "Planned", "category": "open", "stateValue": "planned" },
            { "key": "approved", "name": "Approved", "category": "active", "stateValue": "approved" },
            { "key": "done", "name": "In production", "category": "done", "terminal": true, "stateValue": "live" }
        ],
        "transitions": [
            { "key": "approve", "name": "Approve", "from": "planned", "to": "approved",
              "approval": { "steps": [ { "key": "cab", "name": "CAB", "requiredApprovals": 1 } ] } },
            { "key": "go_live", "name": "Go live", "from": "approved", "to": "done" }
        ]
    });
    w.ok("PUT", &format!("{DEFS}/{}/draft", w.definition), draft).await;

    let (_, alice) = w.user("alice", &[w.approvers]).await;
    w.user("bob", &[w.approvers]).await;
    let cab = w.ok("POST", "/api/v1/admin/groups", json!({ "name": "CAB" })).await;
    let cab_id = cab["id"].as_str().unwrap().to_owned();

    // workflows.manage with view and edit on servers, nothing else.
    let wf = w.profile("WF admins", &[(w.server, true)]).await;
    grant(&w, wf, "workflows.manage").await;
    let (wfadmin, _) = w.user("wfadmin", &[wf]).await;
    // The same, with users.manage.
    let full = w.profile("WF and users", &[(w.server, true)]).await;
    grant(&w, full, "workflows.manage").await;
    grant(&w, full, "users.manage").await;
    let (useradmin, _) = w.user("useradmin", &[full]).await;

    let (status, v) = w.call(&wfadmin, "GET", "/api/v1/principals?q=al", None).await;
    assert_eq!(status, 403, "{v}");

    // Approvers by name: the same answer for a user or group that exists and one that does not.
    let step = |source: &str, field: &str, given: &str| {
        let mut a = json!({ "transitionKey": "approve", "stepKey": "cab", "source": source });
        a[field] = json!(given);
        json!([a])
    };
    for (source, field, known, unknown) in
        [("user", "user", "alice", "nosuchuser"), ("group", "group", "CAB", "nosuch")]
    {
        let (s1, v1) = put_approvers(&w, &wfadmin, step(source, field, known)).await;
        let (s2, v2) = put_approvers(&w, &wfadmin, step(source, field, unknown)).await;
        let (known, unknown) = (answer(s1, &v1), answer(s2, &v2));
        assert_eq!(known.0, 400, "{v1}");
        assert_eq!(known.1["details"][0]["code"], "directory_lookup_forbidden", "{v1}");
        assert_eq!(known.1["details"][0]["field"], format!("approvers[0].{field}"), "{v1}");
        assert_eq!(known, unknown);
    }
    // By id it works, and the list it stored saves again by name.
    let by_id = json!([
        { "transitionKey": "approve", "stepKey": "cab", "source": "user", "user": alice },
        { "transitionKey": "approve", "stepKey": "cab", "source": "group", "group": cab_id },
        { "transitionKey": "approve", "stepKey": "cab", "source": "profile", "profile": "Approvers" }
    ]);
    let (status, v) = put_approvers(&w, &wfadmin, by_id).await;
    assert_eq!(status, 200, "{v}");
    let unchanged = json!([
        { "transitionKey": "approve", "stepKey": "cab", "source": "user", "user": "Alice" },
        { "transitionKey": "approve", "stepKey": "cab", "source": "group", "group": "cab" },
        { "transitionKey": "approve", "stepKey": "cab", "source": "profile", "profile": "Approvers" }
    ]);
    let (status, v) = put_approvers(&w, &wfadmin, unchanged).await;
    assert_eq!(status, 200, "{v}");
    // A caller with users.manage still looks up by name, and is told what does not exist.
    let (status, v) = put_approvers(&w, &useradmin, step("user", "user", "nosuchuser")).await;
    assert_eq!((status, v["error"]["details"][0]["code"].clone()), (400, json!("not_found")), "{v}");

    // The preview: counts only without the right, the users with it.
    let preview = format!("{DEFS}/{}/approvers/preview?transition=approve&step=cab", w.definition);
    let (status, hidden) = w.call(&wfadmin, "GET", &preview, None).await;
    assert_eq!(status, 200, "{hidden}");
    assert_eq!((hidden["users"].clone(), hidden["usersHidden"].clone()), (json!([]), json!(true)), "{hidden}");
    let (status, shown) = w.call(&useradmin, "GET", &preview, None).await;
    assert_eq!(status, 200, "{shown}");
    assert_eq!(shown["usersHidden"], false);
    assert_eq!(usernames(&shown), ["alice", "bob"], "{shown}");
    assert_eq!(hidden["eligibleCount"], shown["eligibleCount"]);
    assert_eq!(hidden["eligibleCount"], 2);
    assert_eq!(hidden["sources"], shown["sources"]);

    // Action recipients by name: the same answer for one that exists and one that does not.
    for (source, known, unknown) in [("user", "alice", "nosuchuser"), ("group", "CAB", "nosuch")] {
        let (s1, v1) = put_actions(&w, &wfadmin, json!([{ "source": source, source: known }])).await;
        let (s2, v2) = put_actions(&w, &wfadmin, json!([{ "source": source, source: unknown }])).await;
        let (known, unknown) = (answer(s1, &v1), answer(s2, &v2));
        assert_eq!(known.0, 400, "{v1}");
        assert_eq!(known.1["details"][0]["code"], "directory_lookup_forbidden", "{v1}");
        assert_eq!(known.1["details"][0]["field"], format!("actions[0].recipients[0].{source}"), "{v1}");
        assert_eq!(known, unknown);
    }
    let by_id = json!([{ "source": "user", "user": alice }, { "source": "group", "group": cab_id },
        { "source": "profile", "profile": "Approvers" }]);
    let (status, v) = put_actions(&w, &wfadmin, by_id).await;
    assert_eq!(status, 200, "{v}");
    let unchanged = json!([{ "source": "user", "user": "alice" }, { "source": "group", "group": "CAB" },
        { "source": "profile", "profile": "Approvers" }]);
    let (status, v) = put_actions(&w, &wfadmin, unchanged).await;
    assert_eq!(status, 200, "{v}");

    let preview = format!("{DEFS}/{}/actions/tell/preview", w.definition);
    let (status, hidden) = w.call(&wfadmin, "GET", &preview, None).await;
    assert_eq!(status, 200, "{hidden}");
    assert_eq!((hidden["users"].clone(), hidden["usersHidden"].clone()), (json!([]), json!(true)), "{hidden}");
    let (status, shown) = w.call(&useradmin, "GET", &preview, None).await;
    assert_eq!(status, 200, "{shown}");
    assert_eq!(shown["usersHidden"], false);
    assert_eq!(usernames(&shown), ["alice", "bob"], "{shown}");
    assert_eq!((hidden["included"].clone(), shown["included"].clone()), (json!(2), json!(2)));
}

const CANDIDATES: &str = "/api/v1/me/approval-delegations/candidates";

async fn get(w: &World, creds: &Creds, q: &str) -> (u16, Value) {
    let path = if q.is_empty() { CANDIDATES.to_owned() } else { format!("{CANDIDATES}?q={q}") };
    w.call(creds, "GET", &path, None).await
}

fn ids(v: &Value) -> Vec<String> {
    v["data"].as_array().unwrap().iter().map(|u| u["username"].as_str().unwrap().to_owned()).collect()
}

/// SHAA-2927: anyone may find the delegate for "Delegate my approvals", but
/// without the `/principals` right only by the exact username, with one
/// answer for an account that does not exist, is disabled or is the caller's.
#[tokio::test]
async fn delegate_candidates_list_only_with_the_principals_right() {
    let Some(db) = scratch::database("workflow_delegate_candidates").await else { return };
    let w = world(&db).await;
    let (_, alice) = w.user("alice", &[w.approvers]).await;
    w.user("alicia", &[w.approvers]).await;
    let (_, carol) = w.user("carol", &[w.approvers]).await;
    sqlx::query("UPDATE users SET is_active = false WHERE id = $1").bind(carol).execute(&w.pool).await.unwrap();
    let viewers = w.profile("Server viewers", &[(w.server, false)]).await;
    let (pat, _) = w.user("pat", &[viewers]).await;
    let full = w.profile("User admins", &[]).await;
    grant(&w, full, "users.manage").await;
    let (useradmin, _) = w.user("useradmin", &[full]).await;

    for (q, code) in [("", "required"), ("a", "too_small")] {
        let (status, v) = get(&w, &pat, q).await;
        assert_eq!((status, v["error"]["details"][0]["code"].clone()), (400, json!(code)), "{v}");
    }

    // Without the right: the exact username, any case, and nothing more.
    let (status, v) = get(&w, &pat, "ALICE").await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["exactMatchOnly"], true);
    assert_eq!(v["data"], json!([{ "id": alice, "username": "alice", "displayName": "alice" }]));
    let (_, v) = get(&w, &pat, "ali").await;
    assert_eq!(v, json!({ "data": [], "exactMatchOnly": true }));
    for q in ["carol", "pat", "nosuchuser"] {
        assert_eq!(get(&w, &pat, q).await, (200, json!({ "data": [], "exactMatchOnly": true })), "{q}");
    }

    // With it: a search, without the caller or disabled accounts.
    let (status, v) = get(&w, &useradmin, "ali").await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["exactMatchOnly"], false);
    assert_eq!(ids(&v), ["alice", "alicia"], "{v}");
    let (_, v) = get(&w, &useradmin, "a").await;
    assert_eq!(v["error"]["details"][0]["code"], "too_small", "{v}");
    let (_, v) = get(&w, &useradmin, "car").await;
    assert_eq!(ids(&v), Vec::<String>::new(), "{v}");
    let (_, v) = get(&w, &useradmin, "useradmin").await;
    assert_eq!(ids(&v), Vec::<String>::new(), "{v}");

    // Exact lookups are counted: 30 a minute, then 429 with Retry-After.
    let mut used = 5;
    while used < 30 {
        assert_eq!(get(&w, &pat, "alice").await.0, 200);
        used += 1;
    }
    let (status, v) = get(&w, &pat, "alice").await;
    assert_eq!((status, v["error"]["code"].clone()), (429, json!("RATE_LIMITED")), "{v}");
    // The search is not counted.
    assert_eq!(get(&w, &useradmin, "ali").await.0, 200);
}
