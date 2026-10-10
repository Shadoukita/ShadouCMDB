//! QA edge tests (SHAA-2979): the action API the S6 designer (PR #847) is
//! built on, at its edges. Unknown and unreachable recipients (refused on
//! save, warned by the lint, explained by the preview), empty or misplaced
//! texts, and a designer with `workflows.manage` but not `webhooks.manage`
//! who picks an endpoint, sees only its key, name and status, and cannot
//! touch the endpoint itself.

use std::sync::Arc;

use serde_json::{Value, json};
use uuid::Uuid;

use super::runtime_tests::{DEFS, World, details, pairs, world, world_with_app};
use crate::config::WebhooksConfig;
use crate::db::scratch;
use crate::modules::api_tokens::tests::{Creds, app_with_webhooks, code};
use crate::modules::webhooks::Webhooks;
use crate::modules::webhooks::ssrf::tests::StaticResolver;
use crate::secrets::Keyring;

const ENDPOINTS: &str = "/api/v1/admin/webhook-endpoints";

fn actions(w: &World) -> String {
    format!("{DEFS}/{}/actions", w.definition)
}

async fn version(w: &World) -> Value {
    w.ok("GET", &actions(w), json!(null)).await["version"].clone()
}

fn inbox(key: &str, recipients: Value) -> Value {
    json!({ "key": key, "name": key, "kind": "inbox", "trigger": "transition", "transition": "approve",
        "recipients": recipients })
}

fn sorted(v: &Value) -> Vec<(String, String)> {
    let mut d = details(v);
    d.sort();
    d
}

/// (path, code) of the lint's warnings.
fn lint(v: &Value) -> Vec<(String, String)> {
    v["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["path"].as_str().unwrap().to_owned(), p["code"].as_str().unwrap().to_owned()))
        .collect()
}

async fn grant(w: &World, profile: Uuid, permission: &str) {
    sqlx::query("INSERT INTO permission_profile_global_permissions (profile_id, permission) VALUES ($1, $2)")
        .bind(profile)
        .bind(permission)
        .execute(&w.pool)
        .await
        .unwrap();
}

/// A designer: `workflows.manage` and edit on the workflow's type (needed to
/// change a workflow), nothing else.
async fn designer(w: &World) -> Creds {
    let p = w.profile("Designers", &[(w.server, true)]).await;
    grant(w, p, "workflows.manage").await;
    w.user("designer", &[p]).await.0
}

/// Recipients that do not exist are refused on save, each at its own path,
/// with nothing saved; recipients that exist but reach nobody are saved with
/// a lint warning; the preview says why each user is out.
#[tokio::test]
async fn qa_s6_unknown_and_unreachable_recipients() {
    let Some(db) = scratch::database("qa_s6_recipients").await else { return };
    let w = world(&db).await;
    let ops = w.profile("Ops", &[(w.server, false)]).await;
    w.profile("Empty", &[(w.server, false)]).await;
    let (_, o1) = w.user("o1", &[ops]).await;
    let (_, gone) = w.user("gone", &[ops]).await;
    let v0 = version(&w).await;

    // Every unknown principal named, one problem each, nothing saved.
    let body = json!({ "version": v0, "actions": [inbox("a", json!([
        { "source": "profile", "profile": "No such profile" },
        { "source": "group", "group": "no-such-group" },
        { "source": "user", "user": "nobody" },
        { "source": "user", "user": "o1" }
    ]))] });
    let (status, v) = w.call(&w.admin, "PUT", &actions(&w), Some(body)).await;
    assert_eq!(
        (status, sorted(&v)),
        (
            400,
            pairs(&[
                ("actions[0].recipients[0].profile", "not_found"),
                ("actions[0].recipients[1].group", "not_found"),
                ("actions[0].recipients[2].user", "not_found"),
            ])
        ),
        "{v}"
    );
    assert_eq!(version(&w).await, v0, "nothing saved");

    // A source without its principal, and one naming two.
    let body = json!({ "version": v0, "actions": [inbox("a", json!([
        { "source": "profile" },
        { "source": "user", "user": "o1", "profile": "Ops" }
    ]))] });
    let (status, v) = w.call(&w.admin, "PUT", &actions(&w), Some(body)).await;
    assert_eq!(
        (status, sorted(&v)),
        (
            400,
            pairs(&[
                ("actions[0].recipients[0].profile", "required"),
                ("actions[0].recipients[1].profile", "source_mismatch")
            ])
        ),
        "{v}"
    );

    // A profile with no user, and a user then disabled: saved, with warnings.
    let body = json!({ "version": v0, "actions": [inbox("a", json!([
        { "source": "profile", "profile": "Empty" },
        { "source": "user", "user": "gone" },
        { "source": "user", "user": "o1" }
    ]))] });
    w.ok("PUT", &actions(&w), body).await;
    w.ok("PATCH", &format!("/api/v1/admin/users/{gone}"), json!({ "isActive": false })).await;
    let v = w.ok("GET", &actions(&w), json!(null)).await;
    assert_eq!(
        lint(&v),
        pairs(&[
            ("actions[0].recipients[0]", "recipients_cannot_view"),
            ("actions[0].recipients[1]", "recipients_cannot_view")
        ]),
        "{v}"
    );

    // The preview: o1 in, gone out as inactive; nothing sent.
    let ci = w.ci(w.server).await;
    let v = w.ok("GET", &format!("{}/a/preview?ciId={ci}", actions(&w)), json!(null)).await;
    let mut users: Vec<(&str, &str)> = v["users"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| (u["username"].as_str().unwrap(), u["reason"].as_str().unwrap()))
        .collect();
    users.sort();
    assert_eq!(users, [("gone", "inactive"), ("o1", "included")], "{v}");
    assert_eq!(v["included"].as_i64(), Some(1), "{v}");
    let sent: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_action_runs").fetch_one(&w.pool).await.unwrap();
    assert_eq!(sent, 0, "a preview queues nothing");

    // A CI that does not exist, or is of a type nobody may preview with: 404, never 500.
    let (status, v) =
        w.call(&w.admin, "GET", &format!("{}/a/preview?ciId={}", actions(&w), Uuid::new_v4()), None).await;
    assert_eq!((status, code(&v)), (404, "NOT_FOUND"), "{v}");
    let (status, v) = w.call(&w.admin, "GET", &format!("{}/a/preview?ciId=not-a-uuid", actions(&w)), None).await;
    assert_eq!(status, 400, "{v}");
    let _ = o1;
    db.drop().await;
}

/// Texts: a blank name is required; an empty text is refused by the schema;
/// e-mail texts on an inbox or webhook
/// action are refused as not applicable, as are webhook
/// fields on an inbox action; e-mail itself is not yet available.
#[tokio::test]
async fn qa_s6_empty_and_misplaced_texts() {
    let Some(db) = scratch::database("qa_s6_texts").await else { return };
    let w = world(&db).await;
    let ops = w.profile("Ops", &[(w.server, false)]).await;
    w.user("o1", &[ops]).await;
    let v0 = version(&w).await;
    let o1 = json!([{ "source": "user", "user": "o1" }]);

    let mut blank = inbox("a", o1.clone());
    blank["name"] = json!("   ");
    let mut subject = inbox("b", o1.clone());
    subject["settings"] = json!({ "subject": { "en": "Done" }, "intro": {} });
    let mut fields = inbox("c", o1.clone());
    fields["settings"] = json!({ "includeAttributes": [] });
    let (status, v) = w
        .call(&w.admin, "PUT", &actions(&w), Some(json!({ "version": v0, "actions": [blank, subject, fields] })))
        .await;
    assert_eq!(
        (status, sorted(&v)),
        (
            400,
            pairs(&[
                ("actions[0].name", "required"),
                ("actions[1].settings.intro", "not_applicable"),
                ("actions[1].settings.subject", "not_applicable"),
                ("actions[2].settings.includeAttributes", "not_applicable"),
            ])
        ),
        "{v}"
    );

    // An e-mail action: e-mail is the unavailable part, not a 500.
    let mail = json!({ "key": "m", "name": "Mail", "kind": "email", "trigger": "transition", "transition": "approve",
        "recipients": o1, "settings": { "subject": { "en": "{{ci.label}}" } } });
    let (status, v) = w.call(&w.admin, "PUT", &actions(&w), Some(json!({ "version": v0, "actions": [mail] }))).await;
    assert_eq!(status, 400, "{v}");
    assert!(sorted(&v).contains(&("actions[0].kind".into(), "kind_unavailable".into())), "{v}");
    assert_eq!(version(&w).await, v0, "nothing saved");

    // An empty text is no text: refused by the schema before any rule runs.
    let mut empty = inbox("e", o1.clone());
    empty["settings"] = json!({ "subject": { "en": "", "de": "" } });
    let (status, v) = w.call(&w.admin, "PUT", &actions(&w), Some(json!({ "version": v0, "actions": [empty] }))).await;
    assert_eq!(
        (status, sorted(&v)),
        (400, pairs(&[("actions.0.settings.subject.de", "too_small"), ("actions.0.settings.subject.en", "too_small")])),
        "{v}"
    );
    db.drop().await;
}

/// A designer with `workflows.manage` only: may list endpoints (key, name and
/// status, nothing else), choose one for an action and see the lint about
/// it, but not read, create, change, ping or delete an endpoint; the action
/// is audited as theirs.
#[tokio::test]
async fn qa_s6_a_designer_without_webhooks_manage_picks_an_endpoint_only() {
    let Some(db) = scratch::database("qa_s6_webhook_designer").await else { return };
    let resolver = Arc::new(StaticResolver::default());
    resolver.set("hook.example.test", &["192.0.2.10"]);
    let cfg = WebhooksConfig { allowed: true, ..WebhooksConfig::default() };
    let hooks = Arc::new(Webhooks::for_tests(cfg, Keyring::for_tests(), resolver, None));
    let w = world_with_app(&db, app_with_webhooks(db.pool.clone(), hooks)).await;
    w.ok("POST", "/api/v1/admin/webhook-allowed-hosts", json!({ "hostPattern": "hook.example.test" })).await;
    let v = w
        .ok(
            "POST",
            ENDPOINTS,
            json!({ "key": "itsm", "name": "ITSM", "url": "https://hook.example.test/in",
                "authHeader": { "name": "Authorization", "value": "Bearer qa-header-secret" } }),
        )
        .await;
    let endpoint = v["endpoint"]["id"].as_str().unwrap().to_owned();
    let d = designer(&w).await;

    // The list: key, name and status; no URL, header, secret or counters.
    let (status, v) = w.call(&d, "GET", ENDPOINTS, None).await;
    assert_eq!(status, 200, "{v}");
    let text = v.to_string();
    assert!(!text.contains("hook.example.test") && !text.contains("qa-header-secret"), "{v}");
    let first = &v["data"][0];
    assert_eq!(
        (first["key"].as_str(), first["name"].as_str(), first["status"].as_str()),
        (Some("itsm"), Some("ITSM"), Some("active"))
    );
    assert_eq!(first["url"], Value::Null, "{first}");

    // Nothing on the endpoint itself.
    for (method, path, body) in [
        ("GET", format!("{ENDPOINTS}/{endpoint}"), None),
        ("POST", ENDPOINTS.to_owned(), Some(json!({ "key": "x", "name": "x", "url": "https://hook.example.test/x" }))),
        ("PATCH", format!("{ENDPOINTS}/{endpoint}"), Some(json!({ "name": "Renamed" }))),
        ("POST", format!("{ENDPOINTS}/{endpoint}/ping"), None),
        ("POST", format!("{ENDPOINTS}/{endpoint}/pause"), None),
        ("POST", format!("{ENDPOINTS}/{endpoint}/rotate-secret"), Some(json!({}))),
        ("DELETE", format!("{ENDPOINTS}/{endpoint}"), None),
    ] {
        let (status, v) = w.call(&d, method, &path, body).await;
        assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{method} {path}: {v}");
    }

    // Choosing it for an action is allowed, and the save is the designer's.
    let v0 = version(&w).await;
    let body = json!({ "version": v0, "actions": [{ "key": "sync", "name": "Sync", "kind": "webhook",
        "trigger": "transition", "transition": "approve", "endpoint": "itsm",
        "settings": { "includeAttributes": ["owner_team"] } }] });
    let (status, v) = w.call(&d, "PUT", &actions(&w), Some(body)).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["actions"][0]["endpoint"]["key"], "itsm", "{v}");
    assert_eq!(lint(&v), pairs(&[]), "{v}");
    let actor: String = sqlx::query_scalar(
        "SELECT actor_name FROM audit_log WHERE entity_type = 'workflow_definitions' AND action = 'update'
         AND new_value ? 'actions' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!(actor, "designer");

    // A webhook action with recipients, or without its endpoint; an endpoint on an inbox action.
    let v1 = version(&w).await;
    let bad = json!({ "version": v1, "actions": [
        { "key": "w1", "name": "W1", "kind": "webhook", "trigger": "transition", "transition": "approve",
          "endpoint": "itsm", "recipients": [{ "source": "user", "user": "designer" }] },
        { "key": "w2", "name": "W2", "kind": "webhook", "trigger": "transition", "transition": "approve" },
        { "key": "i1", "name": "I1", "kind": "inbox", "trigger": "transition", "transition": "approve",
          "endpoint": "itsm", "recipients": [{ "source": "user", "user": "designer" }] }
    ] });
    let (status, v) = w.call(&d, "PUT", &actions(&w), Some(bad)).await;
    assert_eq!(
        (status, sorted(&v)),
        (
            400,
            pairs(&[
                ("actions[0].recipients", "not_applicable"),
                ("actions[1].endpoint", "required"),
                ("actions[2].endpoint", "not_applicable"),
            ])
        ),
        "{v}"
    );

    // The administrator pauses the endpoint: the designer's lint says so; the action stays.
    w.ok("POST", &format!("{ENDPOINTS}/{endpoint}/pause"), json!(null)).await;
    let (status, v) = w.call(&d, "GET", &actions(&w), None).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(lint(&v), pairs(&[("actions[0].endpoint", "endpoint_not_active")]), "{v}");
    db.drop().await;
}

/// With webhooks switched off on the server, a saved webhook action is kept
/// and the lint warns `webhooks_disabled`; the preview of a webhook action
/// does not fail.
#[tokio::test]
async fn qa_s6_webhooks_switched_off_are_a_warning() {
    let Some(db) = scratch::database("qa_s6_webhooks_off").await else { return };
    let resolver = Arc::new(StaticResolver::default());
    resolver.set("hook.example.test", &["192.0.2.10"]);
    let on = WebhooksConfig { allowed: true, ..WebhooksConfig::default() };
    let hooks = Arc::new(Webhooks::for_tests(on, Keyring::for_tests(), resolver.clone(), None));
    let w = world_with_app(&db, app_with_webhooks(db.pool.clone(), hooks)).await;
    w.ok("POST", "/api/v1/admin/webhook-allowed-hosts", json!({ "hostPattern": "hook.example.test" })).await;
    w.ok("POST", ENDPOINTS, json!({ "key": "itsm", "name": "ITSM", "url": "https://hook.example.test/in" })).await;
    let body = json!({ "version": version(&w).await, "actions": [{ "key": "sync", "name": "Sync", "kind": "webhook",
        "trigger": "transition", "transition": "approve", "endpoint": "itsm" }] });
    w.ok("PUT", &actions(&w), body).await;

    // The same database behind a server with webhooks off.
    let off = Arc::new(Webhooks::for_tests(WebhooksConfig::default(), Keyring::for_tests(), resolver, None));
    let w2 = World { app: app_with_webhooks(db.pool.clone(), off), ..w };
    let v = w2.ok("GET", &actions(&w2), json!(null)).await;
    assert_eq!(lint(&v), pairs(&[("actions[0].endpoint", "webhooks_disabled")]), "{v}");
    let ci = w2.ci(w2.server).await;
    let (status, v) = w2.call(&w2.admin, "GET", &format!("{}/sync/preview?ciId={ci}", actions(&w2)), None).await;
    assert!(status == 200 || status == 400, "a webhook preview is answered, never 500: {status} {v}");
    db.drop().await;
}
