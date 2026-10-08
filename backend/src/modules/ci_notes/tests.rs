//! CI notes through the real router on a scratch database (SHAA-2355): the
//! note stream, who may write, change and delete, the edit window, version
//! conflicts, the audit trail and its scoping, the policy and the retention
//! sweep.

use axum::Router;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::db::scratch;
use crate::modules::api_tokens::tests::{Creds, app, call, code, session_of};

struct World {
    app: Router,
    pool: PgPool,
    admin: Creds,
    password: String,
}

async fn world(name: &str) -> Option<(scratch::Scratch, World)> {
    let db = scratch::database(name).await?;
    let app = app(db.pool.clone());
    // Random per test run, so no hard-coded credential reaches the hasher or verifier.
    let password = format!("test passphrase {}", Uuid::new_v4());
    let setup = json!({ "username": "admin", "email": "admin@example.test", "displayName": "Admin", "password": password,
        "setupToken": crate::auth::setup_token::TEST_TOKEN });
    let (status, me, headers) = call(&app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
    assert_eq!(status, 201, "{me}");
    let admin = session_of(&me, &headers);
    crate::seed::install_template(&db.pool, "it_infrastructure").await.unwrap();
    let pool = db.pool.clone();
    Some((db, World { app, pool, admin, password }))
}

impl World {
    async fn call(&self, who: &Creds, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let (status, v, _) = call(&self.app, method, path, who, body).await;
        (status, v)
    }

    async fn class(&self, key: &str) -> Uuid {
        sqlx::query_scalar("SELECT id FROM ci_classes WHERE key = $1").bind(key).fetch_one(&self.pool).await.unwrap()
    }

    async fn value(&self, list: &str, key: &str) -> Uuid {
        sqlx::query_scalar(
            "SELECT v.id FROM lookup_list_values v JOIN lookup_lists l ON l.id = v.list_id WHERE l.key = $1 AND v.key = $2",
        )
        .bind(list)
        .bind(key)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    /// A signed-in user whose profile has these global rights and, per class, view and maybe edit.
    async fn user(&self, name: &str, global: &[&str], classes: &[(&str, bool)]) -> Creds {
        let mut perms = Vec::new();
        for (k, edit) in classes {
            perms.push(json!({ "classId": self.class(k).await, "view": true, "create": *edit, "edit": *edit, "delete": false }));
        }
        let body = json!({ "name": format!("{name} profile"), "globalPermissions": global, "classPermissions": perms });
        let (status, p) = self.call(&self.admin, "POST", "/api/v1/admin/profiles", Some(body)).await;
        assert_eq!(status, 201, "{p}");
        let body = json!({ "username": name, "email": format!("{name}@example.test"), "displayName": name,
            "password": self.password, "profileIds": [p["id"]] });
        let (status, u) = self.call(&self.admin, "POST", "/api/v1/admin/users", Some(body)).await;
        assert_eq!(status, 201, "{u}");
        let login = json!({ "username": name, "password": self.password });
        let (status, me, headers) = call(&self.app, "POST", "/api/v1/auth/login", &Creds::default(), Some(login)).await;
        assert_eq!(status, 200, "{me}");
        session_of(&me, &headers)
    }

    async fn ci(&self, class: &str, name: &str) -> Uuid {
        let body = json!({ "classId": self.class(class).await, "attributes": {
            "name": name, "status": self.value("status", "in_service").await,
            "environment": self.value("environment", "production").await } });
        let (status, v) = self.call(&self.admin, "POST", "/api/v1/configuration-items", Some(body)).await;
        assert_eq!(status, 201, "{v}");
        v["id"].as_str().unwrap().parse().unwrap()
    }

    async fn post(&self, who: &Creds, ci: Uuid, body: &str) -> (u16, Value) {
        self.call(who, "POST", &format!("/api/v1/configuration-items/{ci}/notes"), Some(json!({ "body": body }))).await
    }

    async fn list(&self, who: &Creds, ci: Uuid) -> (u16, Value) {
        self.call(who, "GET", &format!("/api/v1/configuration-items/{ci}/notes"), None).await
    }

    async fn policy(&self, window: Option<i32>, retention: Option<i32>) -> (u16, Value) {
        let body = json!({ "editWindowMinutes": window, "retentionDays": retention });
        self.call(&self.admin, "PUT", "/api/v1/ci-note-settings", Some(body)).await
    }

    async fn audit(&self, note: &str) -> Vec<(String, Option<Value>, Option<Value>)> {
        sqlx::query_as(
            "SELECT action::text, old_value, new_value FROM audit_log
             WHERE entity_type = 'ci_notes' AND entity_id = $1::uuid ORDER BY id",
        )
        .bind(note)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }
}

fn note_path(ci: Uuid, note: &Value) -> String {
    format!("/api/v1/configuration-items/{ci}/notes/{}", note["id"].as_str().unwrap())
}

fn detail(v: &Value) -> &str {
    v["error"]["details"][0]["code"].as_str().unwrap_or_default()
}

/// The stream, every permission case, version conflicts and the audit rows.
#[tokio::test]
async fn notes_follow_the_ci_and_only_the_author_changes_them() {
    let Some((db, w)) = world("ci_notes_crud").await else { return };
    let ci = w.ci("server", "web-01").await;
    let author = w.user("author", &[], &[("server", true)]).await;
    let colleague = w.user("colleague", &[], &[("server", true)]).await;
    let reader = w.user("reader", &[], &[("server", false)]).await;
    let outsider = w.user("outsider", &[], &[("virtual_machine", true)]).await;

    // Posting: edit on the class; the text is trimmed and validated.
    let (status, note) = w.post(&author, ci, "  Disk replaced in slot 3.\nNext check in May.  ").await;
    assert_eq!(status, 201, "{note}");
    assert_eq!(note["body"], "Disk replaced in slot 3.\nNext check in May.");
    assert_eq!((note["author"]["name"].as_str(), note["version"].as_i64()), (Some("author"), Some(1)));
    assert_eq!((note["canEdit"].as_bool(), note["canDelete"].as_bool()), (Some(true), Some(true)));
    assert!(note["editedAt"].is_null() && note["editableUntil"].is_string(), "{note}");
    let (status, v) = w.post(&author, ci, "   ").await;
    assert_eq!((status, code(&v)), (400, "VALIDATION_ERROR"), "{v}");
    let (status, v) = w.post(&author, ci, &"x".repeat(10_001)).await;
    assert_eq!((status, code(&v)), (400, "VALIDATION_ERROR"), "{v}");
    let (status, v) = w
        .call(
            &author,
            "POST",
            &format!("/api/v1/configuration-items/{ci}/notes"),
            Some(json!({ "body": "a", "pinned": true })),
        )
        .await;
    assert_eq!(status, 400, "{v}");
    let (status, v) = w.post(&reader, ci, "I may only read").await;
    assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{v}");
    let (status, v) = w.post(&outsider, ci, "Not my class").await;
    assert_eq!((status, code(&v)), (404, "NOT_FOUND"), "{v}");
    let (status, v) = w.post(&author, Uuid::new_v4(), "Nowhere").await;
    assert_eq!((status, code(&v)), (404, "NOT_FOUND"), "{v}");
    let (status, second) = w.post(&colleague, ci, "Second note").await;
    assert_eq!(status, 201, "{second}");

    // Reading: view on the class, newest first, with what each caller may do.
    let (status, page) = w.list(&reader, ci).await;
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["page"]["total"], 2);
    assert_eq!(page["data"][0]["id"], second["id"]);
    assert!(page["data"].as_array().unwrap().iter().all(|n| n["canEdit"] == false && n["canDelete"] == false));
    let (_, page) = w.list(&author, ci).await;
    assert_eq!((page["data"][1]["canEdit"].as_bool(), page["data"][0]["canEdit"].as_bool()), (Some(true), Some(false)));
    let (_, page) = w.list(&w.admin, ci).await;
    assert!(page["data"].as_array().unwrap().iter().all(|n| n["canEdit"] == false && n["canDelete"] == true));
    let (status, page) =
        w.call(&w.admin, "GET", &format!("/api/v1/configuration-items/{ci}/notes?limit=1&offset=1"), None).await;
    assert_eq!((status, page["data"][0]["id"].clone()), (200, note["id"].clone()), "{page}");
    let (status, v) = w.list(&outsider, ci).await;
    assert_eq!((status, code(&v)), (404, "NOT_FOUND"), "{v}");
    let (status, _) = w.call(&w.admin, "GET", &format!("/api/v1/configuration-items/{ci}/notes?limit=0"), None).await;
    assert_eq!(status, 400);

    // Changing: the author only, with the version they loaded.
    let path = note_path(ci, &note);
    let (status, v) = w.call(&colleague, "PATCH", &path, Some(json!({ "version": 1, "body": "Mine now" }))).await;
    assert_eq!((status, detail(&v)), (403, "not_author"), "{v}");
    let (status, v) = w.call(&w.admin, "PATCH", &path, Some(json!({ "version": 1, "body": "Admin edit" }))).await;
    assert_eq!((status, detail(&v)), (403, "not_author"), "{v}");
    let (status, v) = w.call(&outsider, "PATCH", &path, Some(json!({ "version": 1, "body": "x" }))).await;
    assert_eq!(status, 404, "{v}");
    let (status, v) =
        w.call(&author, "PATCH", &path, Some(json!({ "version": 1, "body": "Disk replaced in slot 4." }))).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!((v["version"].as_i64(), v["editedAt"].is_string()), (Some(2), true), "{v}");
    let (status, v) = w.call(&author, "PATCH", &path, Some(json!({ "version": 1, "body": "Stale" }))).await;
    assert_eq!((status, code(&v)), (409, "VERSION_CONFLICT"), "{v}");
    let (status, v) =
        w.call(&author, "PATCH", &path, Some(json!({ "version": 2, "body": "Disk replaced in slot 4." }))).await;
    assert_eq!((status, v["version"].as_i64()), (200, Some(2)), "unchanged text writes nothing: {v}");
    let wrong_ci = format!(
        "/api/v1/configuration-items/{}/notes/{}",
        w.ci("server", "web-02").await,
        note["id"].as_str().unwrap()
    );
    let (status, v) = w.call(&author, "PATCH", &wrong_ci, Some(json!({ "version": 2, "body": "x" }))).await;
    assert_eq!(status, 404, "a note is reached only through its CI: {v}");

    // Deleting: the author with the version, or an administrator.
    let (status, v) = w.call(&colleague, "DELETE", &format!("{path}?version=2"), None).await;
    assert_eq!((status, detail(&v)), (403, "not_author"), "{v}");
    let (status, v) = w.call(&author, "DELETE", &format!("{path}?version=1"), None).await;
    assert_eq!((status, code(&v)), (409, "VERSION_CONFLICT"), "{v}");
    let (status, v) = w.call(&author, "DELETE", &path, None).await;
    assert_eq!(status, 400, "the version is required: {v}");
    let (status, _) = w.call(&author, "DELETE", &format!("{path}?version=2"), None).await;
    assert_eq!(status, 204);
    let (status, _) = w.call(&author, "DELETE", &format!("{path}?version=2"), None).await;
    assert_eq!(status, 404);
    let (status, _) = w.call(&w.admin, "DELETE", &format!("{}?version=1", note_path(ci, &second)), None).await;
    assert_eq!(status, 204, "an administrator removes any note");

    // One audit row per write, in order, the CI named in every value.
    let rows = w.audit(note["id"].as_str().unwrap()).await;
    let actions: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(actions, ["create", "update", "delete"]);
    assert_eq!(rows[1].1.as_ref().unwrap()["body"], "Disk replaced in slot 3.\nNext check in May.");
    assert_eq!(rows[1].2.as_ref().unwrap()["body"], "Disk replaced in slot 4.");
    assert_eq!(rows[2].1.as_ref().unwrap()["ciId"], ci.to_string());
    assert!(rows[2].2.is_none());
    drop(w);
    db.drop().await;
}

/// The edit window closes changes and deletes for authors, also for old notes;
/// a deleted CI keeps its notes readable and its authors may only delete them.
#[tokio::test]
async fn the_edit_window_and_deleted_cis() {
    let Some((db, w)) = world("ci_notes_window").await else { return };
    let ci = w.ci("server", "db-01").await;
    let author = w.user("author", &[], &[("server", true)]).await;
    let (_, note) = w.post(&author, ci, "Patched to 17.2").await;
    let path = note_path(ci, &note);

    let (status, v) = w.policy(Some(0), None).await;
    assert_eq!(status, 200, "{v}");
    let (_, page) = w.list(&author, ci).await;
    assert_eq!(
        (page["data"][0]["canEdit"].as_bool(), page["data"][0]["canDelete"].as_bool()),
        (Some(false), Some(false))
    );
    let (status, v) = w.call(&author, "PATCH", &path, Some(json!({ "version": 1, "body": "Patched to 17.3" }))).await;
    assert_eq!((status, detail(&v)), (403, "edit_window_closed"), "{v}");
    let (status, v) = w.call(&author, "DELETE", &format!("{path}?version=1"), None).await;
    assert_eq!((status, detail(&v)), (403, "edit_window_closed"), "{v}");

    // Unlimited: no end time, and the author may act again.
    w.policy(None, None).await;
    let (_, page) = w.list(&author, ci).await;
    assert!(page["data"][0]["editableUntil"].is_null(), "{page}");
    assert_eq!(page["data"][0]["canEdit"], true);

    // A deleted CI: notes stay readable; no new notes, no changes, deletes still allowed.
    let (status, v) = w.call(&w.admin, "DELETE", &format!("/api/v1/configuration-items/{ci}?version=1"), None).await;
    assert!(status == 204 || status == 200, "{status} {v}");
    let (status, page) = w.list(&author, ci).await;
    assert_eq!((status, page["page"]["total"].as_i64()), (200, Some(1)), "{page}");
    assert_eq!(
        (page["data"][0]["canEdit"].as_bool(), page["data"][0]["canDelete"].as_bool()),
        (Some(false), Some(true))
    );
    let (status, _) = w.post(&author, ci, "Too late").await;
    assert_eq!(status, 404);
    let (status, _) = w.call(&author, "PATCH", &path, Some(json!({ "version": 1, "body": "Too late" }))).await;
    assert_eq!(status, 404);
    let (status, _) = w.call(&author, "DELETE", &format!("{path}?version=1"), None).await;
    assert_eq!(status, 204);
    drop(w);
    db.drop().await;
}

/// The audit log shows a note's entries only to callers who may view its CI.
#[tokio::test]
async fn note_audit_entries_are_scoped_to_the_ci() {
    let Some((db, w)) = world("ci_notes_audit").await else { return };
    let server = w.ci("server", "app-01").await;
    let (status, note) = w.post(&w.admin, server, "Owner: team blue").await;
    assert_eq!(status, 201, "{note}");
    let auditor = w.user("auditor", &["audit.view"], &[("virtual_machine", false)]).await;
    let full_auditor = w.user("full_auditor", &["audit.view"], &[("server", false)]).await;

    let total = |v: &Value| v["page"]["total"].as_i64().unwrap();
    let (status, log) = w.call(&auditor, "GET", "/api/v1/audit-log?entityType=ci_notes", None).await;
    assert_eq!((status, total(&log)), (200, 0), "{log}");
    assert!(!log.to_string().contains("team blue"), "{log}");
    let id = note["id"].as_str().unwrap();
    let (_, log) = w.call(&auditor, "GET", &format!("/api/v1/audit-log?entityType=ci_notes&entityId={id}"), None).await;
    assert_eq!(total(&log), 0, "{log}");
    let (status, log) = w.call(&full_auditor, "GET", "/api/v1/audit-log?entityType=ci_notes", None).await;
    assert_eq!((status, total(&log)), (200, 1), "{log}");
    assert_eq!(log["data"][0]["newValue"]["body"], "Owner: team blue");
    drop(w);
    db.drop().await;
}

/// The policy: administrators only, validated, audited; the sweep deletes old
/// notes and audits each without its text.
#[tokio::test]
async fn the_policy_and_the_retention_sweep() {
    let Some((db, w)) = world("ci_notes_policy").await else { return };
    let editor = w.user("editor", &[], &[("server", true)]).await;
    let (status, v) = w.call(&editor, "GET", "/api/v1/ci-note-settings", None).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!((v["editWindowMinutes"].as_i64(), v["retentionDays"].is_null()), (Some(1440), true), "{v}");
    let body = json!({ "editWindowMinutes": 60, "retentionDays": null });
    let (status, v) = w.call(&editor, "PUT", "/api/v1/ci-note-settings", Some(body)).await;
    assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{v}");
    for (window, retention) in [(Some(-1), None), (Some(525_601), None), (None, Some(29)), (None, Some(36_501))] {
        let (status, v) = w.policy(window, retention).await;
        assert_eq!((status, code(&v)), (400, "VALIDATION_ERROR"), "{window:?} {retention:?}: {v}");
    }
    let (status, v) =
        w.call(&w.admin, "PUT", "/api/v1/ci-note-settings", Some(json!({ "editWindowMinutes": 60 }))).await;
    assert_eq!(status, 400, "both fields are required: {v}");

    let ci = w.ci("server", "old-01").await;
    let (_, old) = w.post(&editor, ci, "Commissioned").await;
    let (_, new) = w.post(&editor, ci, "Rack moved").await;
    sqlx::query("UPDATE ci_notes SET created_at = now() - interval '40 days' WHERE id = $1::uuid")
        .bind(old["id"].as_str().unwrap())
        .execute(&w.pool)
        .await
        .unwrap();
    // No retention period: nothing goes.
    assert_eq!(super::service::sweep(&w.pool, chrono::Duration::zero()).await.unwrap(), 0);

    let (status, v) = w.policy(Some(60), Some(30)).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!((v["retentionDays"].as_i64(), v["updatedBy"].as_str()), (Some(30), Some("admin")), "{v}");
    let (status, _) = w.policy(Some(60), Some(30)).await;
    assert_eq!(status, 200);
    let changes: Vec<(Value, Value)> =
        sqlx::query_as("SELECT old_value, new_value FROM audit_log WHERE entity_type = 'ci_note_settings' ORDER BY id")
            .fetch_all(&w.pool)
            .await
            .unwrap();
    assert_eq!(changes.len(), 1, "an unchanged policy writes nothing");
    assert_eq!(changes[0].0, json!({ "editWindowMinutes": 1440, "retentionDays": null }));
    assert_eq!(changes[0].1, json!({ "editWindowMinutes": 60, "retentionDays": 30 }));

    assert_eq!(super::service::sweep(&w.pool, chrono::Duration::zero()).await.unwrap(), 1);
    let (_, page) = w.list(&editor, ci).await;
    assert_eq!((page["page"]["total"].as_i64(), page["data"][0]["id"].clone()), (Some(1), new["id"].clone()));
    let rows = w.audit(old["id"].as_str().unwrap()).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    let gone = rows[1].1.as_ref().unwrap();
    assert_eq!(
        (rows[1].0.as_str(), gone["ciId"].as_str(), gone["retentionDays"].as_i64()),
        ("delete", Some(ci.to_string().as_str()), Some(30))
    );
    assert!(gone.get("body").is_none(), "the sweep's own entry carries no text: {gone}");
    let actor: String = sqlx::query_scalar(
        "SELECT actor_name FROM audit_log WHERE entity_type = 'ci_notes' AND action = 'delete' AND entity_id = $1::uuid",
    )
    .bind(old["id"].as_str().unwrap())
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!(actor, "note retention");
    // 31 days on, the newer note goes as well.
    assert_eq!(super::service::sweep(&w.pool, chrono::Duration::days(31)).await.unwrap(), 1);
    drop(w);
    db.drop().await;
}

/// Who reaches the notes at all: nobody signed out; an API token by its
/// scope's class rights, writing as its owner; a caller who may not view the
/// class learns nothing, also on delete; the policy is for sessions only.
#[tokio::test]
async fn notes_without_a_session_and_through_an_api_token() {
    let Some((db, w)) = world("ci_notes_access").await else { return };
    let server = w.ci("server", "srv-01").await;
    let vm = w.ci("virtual_machine", "vm-01").await;
    let (_, note) = w.post(&w.admin, server, "Warranty until 2028").await;
    let (_, vm_note) = w.post(&w.admin, vm, "Snapshot policy: weekly").await;
    let outsider = w.user("outsider", &[], &[("virtual_machine", true)]).await;

    // Signed out: 401 on every route, nothing written.
    let none = Creds::default();
    for (method, path, body) in [
        ("GET", format!("/api/v1/configuration-items/{server}/notes"), None),
        ("POST", format!("/api/v1/configuration-items/{server}/notes"), Some(json!({ "body": "anon" }))),
        ("PATCH", note_path(server, &note), Some(json!({ "version": 1, "body": "anon" }))),
        ("DELETE", format!("{}?version=1", note_path(server, &note)), None),
        ("GET", "/api/v1/ci-note-settings".to_owned(), None),
    ] {
        let (status, v) = w.call(&none, method, &path, body).await;
        assert_eq!(status, 401, "{method} {path}: {v}");
    }

    // A caller who may not view servers cannot delete a server note or learn it exists.
    let (status, v) = w.call(&outsider, "DELETE", &format!("{}?version=1", note_path(server, &note)), None).await;
    assert_eq!((status, code(&v)), (404, "NOT_FOUND"), "{v}");
    let (status, v) = w.call(&outsider, "DELETE", &format!("{}?version=1", note_path(server, &vm_note)), None).await;
    assert_eq!(status, 404, "a note is reached only through its own CI: {v}");

    // A token of the administrator scoped to view and edit servers.
    let perms =
        json!([{ "classId": w.class("server").await, "view": true, "create": true, "edit": true, "delete": false }]);
    let body = json!({ "name": "Server notes", "globalPermissions": [], "classPermissions": perms });
    let (status, profile) = w.call(&w.admin, "POST", "/api/v1/admin/profiles", Some(body)).await;
    assert_eq!(status, 201, "{profile}");
    let expires = (chrono::Utc::now() + chrono::Duration::days(30)).to_rfc3339();
    let body = json!({ "name": "notes script", "profileId": profile["id"], "expiresAt": expires });
    let (status, created) = w.call(&w.admin, "POST", "/api/v1/admin/api-tokens", Some(body)).await;
    assert_eq!(status, 201, "{created}");
    let token = Creds { bearer: created["secret"].as_str().map(str::to_owned), ..Creds::default() };

    let (status, page) = w.list(&token, server).await;
    assert_eq!((status, page["page"]["total"].as_i64()), (200, Some(1)), "{page}");
    let (status, v) = w.list(&token, vm).await;
    assert_eq!((status, code(&v)), (404, "NOT_FOUND"), "the scope, not the owner's rights: {v}");
    let (status, mine) = w.post(&token, server, "Written by the script").await;
    assert_eq!(status, 201, "{mine}");
    assert_eq!(mine["author"]["name"], "admin", "{mine}");
    let (status, v) = w.post(&token, vm, "Not in scope").await;
    assert_eq!(status, 404, "{v}");
    let (status, v) = w
        .call(&token, "PATCH", &note_path(server, &mine), Some(json!({ "version": 1, "body": "Edited by the script" })))
        .await;
    assert_eq!((status, v["version"].as_i64()), (200, Some(2)), "{v}");
    let (status, v) = w
        .call(&token, "PUT", "/api/v1/ci-note-settings", Some(json!({ "editWindowMinutes": 5, "retentionDays": null })))
        .await;
    assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "the policy is for sessions only: {v}");
    let (_, policy) = w.call(&w.admin, "GET", "/api/v1/ci-note-settings", None).await;
    assert_eq!(policy["editWindowMinutes"], 1440, "{policy}");
    drop(w);
    db.drop().await;
}
