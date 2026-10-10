//! QA edge tests (SHAA-3062): the designer's test send of a saved action
//! (SHAA-3042, PR #892) at its permission edges, through the real router on a
//! scratch database. A manager who may view but not edit the workflow's types
//! may test (to themselves) but not save; one who may not view them gets 404
//! as for the workflow itself; an API token is refused (session only); a
//! disabled action can still be tested; refusals are neither sent nor audited.

use serde_json::json;
use uuid::Uuid;

use crate::db::scratch;
use crate::db::upgrade_0046::id;
use crate::modules::api_tokens::tests::{Creds, code};
use crate::modules::workflows::runtime_tests::{DEFS, World, world};

fn actions(w: &World) -> String {
    format!("{DEFS}/{}/actions", w.definition)
}

/// A profile with `workflows.manage` and the given rights on the classes.
async fn managers(w: &World, name: &str, classes: &[(Uuid, bool)]) -> Uuid {
    let profile = w.profile(name, classes).await;
    sqlx::query(
        "INSERT INTO permission_profile_global_permissions (profile_id, permission) VALUES ($1, 'workflows.manage')",
    )
    .bind(profile)
    .execute(&w.pool)
    .await
    .unwrap();
    profile
}

async fn count(w: &World, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned())).fetch_one(&w.pool).await.unwrap()
}

#[tokio::test]
async fn a_test_send_needs_view_not_edit_a_session_and_ignores_enabled() {
    let Some(db) = scratch::database("workflow_action_test_edges").await else { return };
    let w = world(&db).await;
    let version = w.ok("GET", &actions(&w), json!(null)).await["version"].clone();
    let inbox = |key: &str, enabled: bool| {
        json!({ "key": key, "name": key, "kind": "inbox", "trigger": "transition", "transition": "approve",
            "enabled": enabled, "recipients": [{ "source": "participant", "participant": "actor" }] })
    };
    w.ok("PUT", &actions(&w), json!({ "version": version, "actions": [inbox("box", true), inbox("off", false)] }))
        .await;
    let viewers = managers(&w, "Workflow viewers", &[(w.server, false)]).await;
    let blind = managers(&w, "Workflow blind", &[(w.network, true)]).await;
    let (viewer, viewer_id) = w.user("viewer", &[viewers]).await;
    let (blind_user, _) = w.user("blind", &[blind]).await;
    let test = |key: &str| format!("{}/{key}/test", actions(&w));
    let audited = "SELECT count(*) FROM audit_log WHERE action = 'workflow.action_test'";
    let notified = "SELECT count(*) FROM notifications WHERE kind = 'workflow_action'";

    // View without edit: may test, to themselves only, but not save.
    let (status, v) = w.call(&viewer, "POST", &test("box"), None).await;
    assert_eq!((status, v["ok"].as_bool(), v["to"].as_str()), (200, Some(true), Some("inbox")), "{v}");
    let to: Uuid = id(&w.pool, "SELECT user_id FROM notifications WHERE kind = 'workflow_action'").await;
    assert_eq!(to, viewer_id, "the caller is notified, nobody else");
    let (status, v) = w.call(&viewer, "PUT", &actions(&w), Some(json!({ "version": version, "actions": [] }))).await;
    assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "saving needs edit: {v}");

    // No view on the workflow's types: the workflow does not exist for them.
    let (status, v) = w.call(&blind_user, "POST", &test("box"), None).await;
    assert_eq!((status, code(&v)), (404, "NOT_FOUND"), "{v}");

    // A disabled action is still tested (that is how a designer checks it before enabling it).
    let v = w.ok("POST", &test("off"), json!(null)).await;
    assert_eq!((v["ok"].as_bool(), v["key"].as_str()), (Some(true), Some("off")), "{v}");

    // An API token, even the Administrator's: a session only.
    let builtin = id(&w.pool, "SELECT id FROM permission_profiles WHERE is_builtin").await;
    let created = w
        .ok(
            "POST",
            "/api/v1/admin/api-tokens",
            json!({ "name": "qa", "profileId": builtin,
                "expiresAt": (chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339() }),
        )
        .await;
    let token = Creds { bearer: created["secret"].as_str().map(str::to_owned), ..Creds::default() };
    let (status, v) = w.call(&token, "POST", &test("box"), None).await;
    assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{v}");
    let (status, _) = w.call(&token, "GET", &actions(&w), None).await;
    assert_eq!(status, 200, "the token may still read the actions");

    // A CI id that is not a UUID is a validation error, not a 500.
    let (status, v) = w.call(&w.admin, "POST", &format!("{}?ciId=nope", test("box")), None).await;
    assert_eq!((status, code(&v)), (400, "VALIDATION_ERROR"), "{v}");

    // Only the two tests that went through were notified and audited.
    assert_eq!(count(&w, notified).await, 2);
    assert_eq!(count(&w, audited).await, 2);
    assert_eq!(count(&w, "SELECT count(*) FROM workflow_action_runs").await, 0, "nothing queued");
    db.drop().await;
}
