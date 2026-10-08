//! Notifications through the real router on a scratch database (SHAA-2356):
//! the import trigger, list, count, marking read, dismissing, the owner-only
//! and class rules, session-only access and retention. The workflow and
//! approval triggers are tested with the approvals in
//! `workflows::notifications_tests`.

use axum::Router;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::service::{self, MAX_PER_USER, Swept};
use crate::db::scratch;
use crate::modules::api_tokens::tests::{Creds, app, call, code, session_of};

const LIST: &str = "/api/v1/notifications";
const COUNT: &str = "/api/v1/notifications/unread-count";

struct World {
    app: Router,
    pool: PgPool,
    admin: (Creds, Uuid),
    password: String,
}

async fn world(db: &scratch::Scratch) -> World {
    let app = app(db.pool.clone());
    let password = format!("test passphrase {}", Uuid::new_v4());
    let setup = json!({ "username": "admin", "email": "admin@example.test", "displayName": "Admin", "password": password,
        "setupToken": crate::auth::setup_token::TEST_TOKEN });
    let (status, me, headers) = call(&app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
    assert_eq!(status, 201, "{me}");
    let admin = (session_of(&me, &headers), me["user"]["id"].as_str().unwrap().parse().unwrap());
    crate::seed::install_template(&db.pool, "it_infrastructure").await.unwrap();
    World { app, pool: db.pool.clone(), admin, password }
}

impl World {
    async fn call(&self, who: &Creds, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let (status, v, _) = call(&self.app, method, path, who, body).await;
        (status, v)
    }

    async fn class(&self, key: &str) -> Uuid {
        sqlx::query_scalar("SELECT id FROM ci_classes WHERE key = $1").bind(key).fetch_one(&self.pool).await.unwrap()
    }

    /// A signed-in user who may view these classes.
    async fn user(&self, name: &str, view: &[&str]) -> (Creds, Uuid) {
        let mut classes = Vec::new();
        for k in view {
            classes.push(json!({ "classId": self.class(k).await, "view": true, "create": false, "edit": false, "delete": false }));
        }
        let body = json!({ "name": format!("{name} profile"), "globalPermissions": [], "classPermissions": classes });
        let (status, p) = self.call(&self.admin.0, "POST", "/api/v1/admin/profiles", Some(body)).await;
        assert_eq!(status, 201, "{p}");
        let body = json!({ "username": name, "email": format!("{name}@example.test"), "displayName": name,
            "password": self.password, "profileIds": [p["id"]] });
        let (status, u) = self.call(&self.admin.0, "POST", "/api/v1/admin/users", Some(body)).await;
        assert_eq!(status, 201, "{u}");
        self.login(name).await
    }

    async fn login(&self, name: &str) -> (Creds, Uuid) {
        let login = json!({ "username": name, "password": self.password });
        let (status, me, headers) = call(&self.app, "POST", "/api/v1/auth/login", &Creds::default(), Some(login)).await;
        assert_eq!(status, 200, "{me}");
        (session_of(&me, &headers), me["user"]["id"].as_str().unwrap().parse().unwrap())
    }

    /// A CI of class `key`.
    async fn ci(&self, key: &str) -> Uuid {
        let in_service: Uuid = sqlx::query_scalar(
            "SELECT v.id FROM lookup_list_values v JOIN lookup_lists l ON l.id = v.list_id
             WHERE l.key = 'status' AND v.key = 'in_service'",
        )
        .fetch_one(&self.pool)
        .await
        .unwrap();
        let name = format!("ci-{}", Uuid::new_v4().simple());
        let body = json!({ "classId": self.class(key).await, "attributes": { "name": name, "status": in_service } });
        let (status, v) = self.call(&self.admin.0, "POST", "/api/v1/configuration-items", Some(body)).await;
        assert_eq!(status, 201, "{v}");
        v["id"].as_str().unwrap().parse().unwrap()
    }

    /// A notification as the triggers write one, `age_days` old.
    async fn notify(&self, user: Uuid, ci: Option<Uuid>, age_days: i32) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO notifications (user_id, kind, entity_type, entity_id, ci_id, data, dedupe_key, created_at)
             VALUES ($1, 'workflow_transition', 'workflow_instances', gen_random_uuid(), $2, '{\"ciLabel\": \"x\"}',
                     gen_random_uuid()::text, now() - make_interval(days => $3))
             RETURNING id",
        )
        .bind(user)
        .bind(ci)
        .bind(age_days)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn unread(&self, who: &Creds) -> i64 {
        let (status, v) = self.call(who, "GET", COUNT, None).await;
        assert_eq!(status, 200, "{v}");
        v["unread"].as_i64().unwrap()
    }

    async fn ids(&self, who: &Creds, query: &str) -> (i64, Vec<Uuid>) {
        let (status, v) = self.call(who, "GET", &format!("{LIST}{query}"), None).await;
        assert_eq!(status, 200, "{v}");
        let ids = v["data"].as_array().unwrap().iter().map(|n| n["id"].as_str().unwrap().parse().unwrap()).collect();
        (v["page"]["total"].as_i64().unwrap(), ids)
    }
}

/// An import that ends notifies its creator once; one that is cancelled or
/// expires does not, and a status change that is not an end does not either.
#[tokio::test]
async fn an_import_that_ends_notifies_its_creator() {
    let Some(db) = scratch::database("notifications_import").await else { return };
    let w = world(&db).await;
    let (alice, alice_id) = w.user("alice", &[]).await;
    let job = |status: &'static str| {
        let pool = w.pool.clone();
        async move {
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO import_jobs (created_by_id, created_by_name, status, file_name, file_format, file_size,
                                          expires_at)
                 VALUES ($1, 'alice', 'uploading', 'servers.csv', 'csv', 0, now() + interval '1 day') RETURNING id",
            )
            .bind(alice_id)
            .fetch_one(&pool)
            .await
            .unwrap();
            sqlx::query(
                "UPDATE import_jobs SET status = $2, file_sha256 = repeat('0', 64), file_size = 1,
                        finished_at = now(), error = CASE WHEN $2 = 'failed' THEN '{\"code\": \"parse_error\"}'::jsonb END
                  WHERE id = $1",
            )
            .bind(id)
            .bind(status)
            .execute(&pool)
            .await
            .unwrap();
            id
        }
    };
    let failed = job("failed").await;
    job("cancelled").await;
    assert_eq!(w.unread(&alice).await, 1);
    let (_, v) = w.call(&alice, "GET", LIST, None).await;
    let n = &v["data"][0];
    assert_eq!(n["kind"], "import_finished");
    assert_eq!(n["entityType"], "import_jobs");
    assert_eq!(n["entityId"], failed.to_string());
    assert_eq!(n["ciId"], Value::Null);
    assert_eq!(
        n["data"],
        json!({ "fileName": "servers.csv", "classKey": null, "status": "failed", "errorCode": "parse_error" })
    );
    assert_eq!(n["readAt"], Value::Null);
    // The same end written twice (a retried worker) notifies once.
    sqlx::query("UPDATE import_jobs SET status = 'completed', error = NULL WHERE id = $1")
        .bind(failed)
        .execute(&w.pool)
        .await
        .unwrap();
    assert_eq!(w.unread(&alice).await, 1);
    // Nobody else hears of it, the administrator included.
    assert_eq!(w.unread(&w.admin.0).await, 0);
}

/// List, filters, count, marking read and unread, mark-all up to a time,
/// dismissing; another user's notification is 404 to everyone else.
#[tokio::test]
async fn a_user_reads_marks_and_dismisses_only_their_own() {
    let Some(db) = scratch::database("notifications_inbox").await else { return };
    let w = world(&db).await;
    let (alice, alice_id) = w.user("alice", &[]).await;
    let (bob, bob_id) = w.user("bob", &[]).await;
    let old = w.notify(alice_id, None, 2).await;
    let mid = w.notify(alice_id, None, 1).await;
    let new = w.notify(alice_id, None, 0).await;
    let bobs = w.notify(bob_id, None, 0).await;

    assert_eq!(w.ids(&alice, "").await, (3, vec![new, mid, old]));
    assert_eq!(w.ids(&alice, "?limit=1&offset=1").await, (3, vec![mid]));
    assert_eq!(w.ids(&alice, "?kind=import_finished").await, (0, vec![]));
    assert_eq!(w.unread(&alice).await, 3);

    // One read, then unread again; reading twice keeps the first time.
    let (status, v) = w.call(&alice, "PATCH", &format!("{LIST}/{mid}"), Some(json!({ "read": true }))).await;
    assert_eq!(status, 200, "{v}");
    let first = v["readAt"].clone();
    assert!(first.is_string());
    let (_, v) = w.call(&alice, "PATCH", &format!("{LIST}/{mid}"), Some(json!({ "read": true }))).await;
    assert_eq!(v["readAt"], first);
    assert_eq!(w.unread(&alice).await, 2);
    assert_eq!(w.ids(&alice, "?unread=true").await, (2, vec![new, old]));
    assert_eq!(w.ids(&alice, "?unread=false").await, (1, vec![mid]));
    let (_, v) = w.call(&alice, "PATCH", &format!("{LIST}/{mid}"), Some(json!({ "read": false }))).await;
    assert_eq!(v["readAt"], Value::Null);

    // Mark all up to what the user saw: the newer one stays unread.
    let up_to: String =
        sqlx::query_scalar("SELECT to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') FROM notifications WHERE id = $1")
            .bind(mid)
            .fetch_one(&w.pool)
            .await
            .unwrap();
    let (status, v) = w.call(&alice, "POST", &format!("{LIST}/mark-read"), Some(json!({ "upTo": up_to }))).await;
    assert_eq!((status, v), (200, json!({ "updated": 2 })));
    assert_eq!(w.ids(&alice, "?unread=true").await, (1, vec![new]));
    let (_, v) = w.call(&alice, "POST", &format!("{LIST}/mark-read"), Some(json!({}))).await;
    assert_eq!(v, json!({ "updated": 1 }));
    assert_eq!(w.unread(&alice).await, 0);
    // Bob's is untouched.
    assert_eq!(w.unread(&bob).await, 1);

    // Another user's notification: 404 on every route, administrators included.
    for who in [&alice, &w.admin.0] {
        let (status, v) = w.call(who, "PATCH", &format!("{LIST}/{bobs}"), Some(json!({ "read": true }))).await;
        assert_eq!((status, code(&v)), (404, "NOT_FOUND"), "{v}");
        let (status, v) = w.call(who, "DELETE", &format!("{LIST}/{bobs}"), None).await;
        assert_eq!((status, code(&v)), (404, "NOT_FOUND"), "{v}");
    }
    assert_eq!(w.unread(&bob).await, 1);

    // Dismiss.
    let (status, _) = w.call(&alice, "DELETE", &format!("{LIST}/{old}"), None).await;
    assert_eq!(status, 204);
    assert_eq!(w.ids(&alice, "").await, (2, vec![new, mid]));
    let (status, _) = w.call(&alice, "DELETE", &format!("{LIST}/{old}"), None).await;
    assert_eq!(status, 404);

    // Validation at the boundary.
    let (status, v) = w.call(&alice, "PATCH", &format!("{LIST}/{new}"), Some(json!({ "read": "yes" }))).await;
    assert_eq!((status, code(&v)), (400, "VALIDATION_ERROR"), "{v}");
    let (status, v) = w.call(&alice, "GET", &format!("{LIST}?limit=101"), None).await;
    assert_eq!((status, code(&v)), (400, "VALIDATION_ERROR"), "{v}");
    let (status, v) = w.call(&alice, "GET", &format!("{LIST}?kind=nope"), None).await;
    assert_eq!((status, code(&v)), (400, "VALIDATION_ERROR"), "{v}");

    // A deleted account takes its notifications with it.
    sqlx::query("DELETE FROM users WHERE id = $1").bind(bob_id).execute(&w.pool).await.unwrap();
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM notifications WHERE id = $1")
        .bind(bobs)
        .fetch_one(&w.pool)
        .await
        .unwrap();
    assert_eq!(left, 0);
}

/// A notification about a CI is shown only while the reader may view its
/// class; it comes back when the right does.
#[tokio::test]
async fn a_notification_about_a_ci_follows_the_class_right() {
    let Some(db) = scratch::database("notifications_class").await else { return };
    let w = world(&db).await;
    let (carol, carol_id) = w.user("carol", &["server"]).await;
    let server = w.ci("server").await;
    let switch = w.ci("application").await;
    let seen = w.notify(carol_id, Some(server), 0).await;
    let hidden = w.notify(carol_id, Some(switch), 0).await;
    let plain = w.notify(carol_id, None, 0).await;

    let (total, mut ids) = w.ids(&carol, "").await;
    ids.sort();
    let mut want = vec![seen, plain];
    want.sort();
    assert_eq!((total, ids), (2, want));
    assert_eq!(w.unread(&carol).await, 2);
    let (status, _) = w.call(&carol, "PATCH", &format!("{LIST}/{hidden}"), Some(json!({ "read": true }))).await;
    assert_eq!(status, 404);
    let (status, _) = w.call(&carol, "DELETE", &format!("{LIST}/{hidden}"), None).await;
    assert_eq!(status, 404);
    let (_, v) = w.call(&carol, "POST", &format!("{LIST}/mark-read"), Some(json!({}))).await;
    assert_eq!(v, json!({ "updated": 2 }));
    let read: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT read_at FROM notifications WHERE id = $1")
            .bind(hidden)
            .fetch_one(&w.pool)
            .await
            .unwrap();
    assert_eq!(read, None, "mark-all leaves what the reader cannot see");
    // Given the right, the reader sees it again.
    let profile: Uuid = sqlx::query_scalar("SELECT profile_id FROM user_permission_profiles WHERE user_id = $1")
        .bind(carol_id)
        .fetch_one(&w.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO permission_profile_class_permissions (profile_id, class_id, can_view) VALUES ($1, $2, true)",
    )
    .bind(profile)
    .bind(w.class("application").await)
    .execute(&w.pool)
    .await
    .unwrap();
    let (carol, _) = w.login("carol").await;
    assert_eq!(w.unread(&carol).await, 1);

    // A purged CI takes its notifications with it.
    sqlx::query("DELETE FROM configuration_items WHERE id = $1").bind(switch).execute(&w.pool).await.unwrap();
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM notifications WHERE id = $1")
        .bind(hidden)
        .fetch_one(&w.pool)
        .await
        .unwrap();
    assert_eq!(left, 0);
}

/// Session only: an API token is refused, and so is no session at all.
#[tokio::test]
async fn notifications_need_a_browser_session() {
    let Some(db) = scratch::database("notifications_session").await else { return };
    let w = world(&db).await;
    let (status, v) = w.call(&Creds::default(), "GET", COUNT, None).await;
    assert_eq!((status, code(&v)), (401, "UNAUTHENTICATED"), "{v}");
    let profile: Uuid = sqlx::query_scalar("SELECT id FROM permission_profiles WHERE is_builtin LIMIT 1")
        .fetch_one(&w.pool)
        .await
        .unwrap();
    let expires = (chrono::Utc::now() + chrono::Duration::days(30)).to_rfc3339();
    let body = json!({ "name": "script", "userId": w.admin.1, "profileId": profile, "expiresAt": expires });
    let (status, t) = w.call(&w.admin.0, "POST", "/api/v1/admin/api-tokens", Some(body)).await;
    assert_eq!(status, 201, "{t}");
    let token = Creds { bearer: Some(t["secret"].as_str().unwrap().to_owned()), ..Default::default() };
    for (method, path) in [("GET", LIST), ("GET", COUNT)] {
        let (status, v) = w.call(&token, method, path, None).await;
        assert_eq!(status, 403, "{method} {path}: {v}");
    }
    let (status, _) = w.call(&token, "POST", &format!("{LIST}/mark-read"), Some(json!({}))).await;
    assert_eq!(status, 403);
}

/// Retention: older than the period goes, and so does everything beyond a
/// user's newest 500; a second sweep finds nothing.
#[tokio::test]
async fn the_sweep_keeps_the_retention_period_and_the_per_user_cap() {
    let Some(db) = scratch::database("notifications_retention").await else { return };
    let w = world(&db).await;
    let (_, dave) = w.user("dave", &[]).await;
    let (_, erin) = w.user("erin", &[]).await;
    let expired = w.notify(dave, None, 31).await;
    let kept = w.notify(dave, None, 29).await;
    // Erin: 3 more than the cap, all recent; the oldest three go.
    sqlx::query(
        "INSERT INTO notifications (user_id, kind, entity_type, entity_id, dedupe_key, created_at)
         SELECT $1, 'import_finished', 'import_jobs', gen_random_uuid(), 'k' || g, now() - make_interval(secs => g)
           FROM generate_series(1, $2) g",
    )
    .bind(erin)
    .bind(MAX_PER_USER + 3)
    .execute(&w.pool)
    .await
    .unwrap();
    let mut conn = w.pool.acquire().await.unwrap();
    assert_eq!(service::sweep(&mut conn, 30).await.unwrap(), Swept { expired: 1, over_limit: 3 });
    assert_eq!(service::sweep(&mut conn, 30).await.unwrap(), Swept::default());
    let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM notifications WHERE user_id = $1")
        .bind(dave)
        .fetch_all(&w.pool)
        .await
        .unwrap();
    assert_eq!(ids, vec![kept]);
    assert!(!ids.contains(&expired));
    let oldest: i64 =
        sqlx::query_scalar("SELECT max(substr(dedupe_key, 2)::bigint) FROM notifications WHERE user_id = $1")
            .bind(erin)
            .fetch_one(&w.pool)
            .await
            .unwrap();
    assert_eq!(oldest, MAX_PER_USER);
}
