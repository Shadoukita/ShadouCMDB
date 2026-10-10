//! QA edge cases for workflow actions slice S1 (SHAA-2849, #830) through the
//! real router against PostgreSQL: the e-mail language on PATCH
//! /api/v1/auth/me refuses every value but `en`, `de` and null, whatever its
//! JSON type or spelling, and only a signed-in session may set it; the new
//! `webhooks.manage` right is granted only by whoever holds it, shows in the
//! grantee's session, and leaves the other rights alone.

use serde_json::{Value, json};
use uuid::Uuid;

use crate::db::scratch;
use crate::modules::api_tokens::tests::{Creds, app, call, code, session_of};

const ME: &str = "/api/v1/auth/me";
const PROFILES: &str = "/api/v1/admin/profiles";

struct World {
    app: axum::Router,
    pool: sqlx::PgPool,
    admin: Creds,
    password: String,
}

impl World {
    async fn new(db: &scratch::Scratch) -> World {
        let app = app(db.pool.clone());
        let password = format!("test passphrase {}", Uuid::new_v4());
        let setup = json!({ "username": "admin", "email": "admin@example.test", "displayName": "Admin",
            "password": password, "setupToken": crate::auth::setup_token::TEST_TOKEN });
        let (status, me, headers) = call(&app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
        assert_eq!(status, 201, "{me}");
        let admin = session_of(&me, &headers);
        World { app, pool: db.pool.clone(), admin, password }
    }

    async fn ok(&self, creds: &Creds, method: &str, path: &str, body: Value, expected: u16) -> Value {
        let (status, v, _) = call(&self.app, method, path, creds, Some(body)).await;
        assert_eq!(status, expected, "{method} {path}: {v}");
        v
    }

    async fn profile(&self, name: &str, rights: &[&str]) -> Uuid {
        let v = self.ok(&self.admin, "POST", PROFILES, json!({ "name": name, "globalPermissions": rights }), 201).await;
        v["id"].as_str().unwrap().parse().unwrap()
    }

    /// A user with these profiles, signed in: their session and its body.
    async fn sign_in(&self, name: &str, profiles: &[Uuid]) -> (Creds, Value) {
        let body = json!({ "username": name, "email": format!("{name}@example.test"), "displayName": name,
            "password": self.password, "profileIds": profiles });
        self.ok(&self.admin, "POST", "/api/v1/admin/users", body, 201).await;
        let body = json!({ "username": name, "password": self.password });
        let (status, me, headers) = call(&self.app, "POST", "/api/v1/auth/login", &Creds::default(), Some(body)).await;
        assert_eq!(status, 200, "{me}");
        (session_of(&me, &headers), me)
    }

    async fn locale(&self, username: &str) -> Option<String> {
        sqlx::query_scalar("SELECT locale FROM users WHERE username = $1")
            .bind(username)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }
}

fn rights(me: &Value) -> Vec<String> {
    let mut r: Vec<String> =
        me["permissions"]["global"].as_array().unwrap().iter().map(|p| p.as_str().unwrap().to_owned()).collect();
    r.sort();
    r
}

/// Every value but `en`, `de` and null is a 400 that changes nothing: other
/// spellings, regions, padding, other JSON types, an empty string, a body
/// that is not an object. A request without a session, or with an API
/// token, cannot set it.
#[tokio::test]
async fn the_e_mail_language_refuses_everything_but_en_de_and_null() {
    let Some(db) = scratch::database("s1_edge_locale").await else { return };
    let w = World::new(&db).await;
    let (session, me) = w.sign_in("operator", &[]).await;
    assert_eq!(me["locale"], json!(null), "a new account follows the server default: {me}");

    let v = w.ok(&session, "PATCH", ME, json!({ "locale": "en" }), 200).await;
    assert_eq!(v["locale"], "en");
    for bad in [
        json!({ "locale": "" }),
        json!({ "locale": " de" }),
        json!({ "locale": "de " }),
        json!({ "locale": "De" }),
        json!({ "locale": "de-DE" }),
        json!({ "locale": "de_DE" }),
        json!({ "locale": "en-GB" }),
        json!({ "locale": "eng" }),
        json!({ "locale": "x" }),
        json!({ "locale": "d".repeat(10_000) }),
        json!({ "locale": 1 }),
        json!({ "locale": true }),
        json!({ "locale": ["de"] }),
        json!({ "locale": { "code": "de" } }),
        json!({ "locale": "de", "theme": "dark" }),
        json!("de"),
        json!(["de"]),
        json!(null),
    ] {
        let (status, v, _) = call(&w.app, "PATCH", ME, &session, Some(bad.clone())).await;
        assert_eq!((status, code(&v)), (400, "VALIDATION_ERROR"), "{bad}: {v}");
        assert_eq!(w.locale("operator").await.as_deref(), Some("en"), "{bad} changed nothing");
    }

    // Nobody signed in: 401. An API token, even its owner's: refused, nothing stored.
    let (status, v, _) = call(&w.app, "PATCH", ME, &Creds::default(), Some(json!({ "locale": "de" }))).await;
    assert_eq!(status, 401, "{v}");
    let admin_id: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE username = 'admin'").fetch_one(&w.pool).await.unwrap();
    let builtin: Uuid =
        sqlx::query_scalar("SELECT id FROM permission_profiles WHERE is_builtin").fetch_one(&w.pool).await.unwrap();
    let expires = (chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339();
    let t = w
        .ok(
            &w.admin,
            "POST",
            "/api/v1/admin/api-tokens",
            json!({ "name": "t", "userId": admin_id, "profileId": builtin, "expiresAt": expires }),
            201,
        )
        .await;
    let token = Creds { bearer: Some(t["secret"].as_str().unwrap().to_owned()), ..Default::default() };
    let (status, v, _) = call(&w.app, "PATCH", ME, &token, Some(json!({ "locale": "de" }))).await;
    assert!(status == 401 || status == 403, "an API token has no session to set it on: {status} {v}");
    assert_eq!(w.locale("admin").await, None, "the token's owner is unchanged");

    // One user's choice is theirs alone, and survives signing in again.
    let v = w.ok(&session, "PATCH", ME, json!({ "locale": "de" }), 200).await;
    assert_eq!(v["locale"], "de");
    let (status, admin_me, _) = call(&w.app, "GET", ME, &w.admin, None).await;
    assert_eq!((status, &admin_me["locale"]), (200, &json!(null)));
    let body = json!({ "username": "operator", "password": w.password });
    let (status, again, _) = call(&w.app, "POST", "/api/v1/auth/login", &Creds::default(), Some(body)).await;
    assert_eq!((status, &again["locale"]), (200, &json!("de")), "{again}");
    db.drop().await;
}

/// `webhooks.manage` is a right of its own: no profile holds it after
/// setup; a profile manager who does not hold it cannot grant it, not even
/// to their own profile, nor clone a profile that grants it; one who holds it
/// can. The grantee's session lists it and nothing more.
#[tokio::test]
async fn webhooks_manage_is_granted_only_by_whoever_holds_it() {
    let Some(db) = scratch::database("s1_edge_webhooks_manage").await else { return };
    let w = World::new(&db).await;
    let held: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM permission_profile_global_permissions WHERE permission = 'webhooks.manage'",
    )
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!(held, 0, "only the Administrator holds it, implicitly");
    let (_, admin_me, _) = call(&w.app, "GET", ME, &w.admin, None).await;
    assert!(rights(&admin_me).contains(&"webhooks.manage".to_owned()), "{admin_me}");

    let managers = w.profile("Profile managers", &["profiles.manage", "users.manage"]).await;
    let (manager, me) = w.sign_in("manager", &[managers]).await;
    assert!(!rights(&me).contains(&"webhooks.manage".to_owned()), "{me}");

    // Without it: refused everywhere it could be handed out, nothing written.
    let (status, v, _) = call(
        &w.app,
        "POST",
        PROFILES,
        &manager,
        Some(json!({ "name": "Hooks", "globalPermissions": ["webhooks.manage"] })),
    )
    .await;
    assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{v}");
    let (status, v, _) = call(
        &w.app,
        "PATCH",
        &format!("{PROFILES}/{managers}"),
        &manager,
        Some(json!({ "globalPermissions": ["profiles.manage", "users.manage", "webhooks.manage"] })),
    )
    .await;
    assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "not even onto their own profile: {v}");
    let hooks = w.profile("Webhook admins", &["webhooks.manage"]).await;
    let (status, v, _) =
        call(&w.app, "POST", &format!("{PROFILES}/{hooks}/clone"), &manager, Some(json!({ "name": "Hooks copy" })))
            .await;
    assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "nor by cloning a profile that grants it: {v}");
    let held: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM permission_profile_global_permissions WHERE permission = 'webhooks.manage'",
    )
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!(held, 1, "the Webhook admins profile the Administrator made, only");

    // A grantee: the right and nothing it does not imply.
    let (_, me) = w.sign_in("integrator", &[hooks]).await;
    assert_eq!(rights(&me), ["webhooks.manage"], "{me}");

    // A manager who holds it may hand it on.
    let both = w.profile("Managers with hooks", &["profiles.manage", "users.manage", "webhooks.manage"]).await;
    let (manager, _) = w.sign_in("manager2", &[both]).await;
    let (status, v, _) = call(
        &w.app,
        "POST",
        PROFILES,
        &manager,
        Some(json!({ "name": "Hooks 2", "globalPermissions": ["webhooks.manage"] })),
    )
    .await;
    assert_eq!(status, 201, "{v}");
    assert_eq!(v["globalPermissions"], json!(["webhooks.manage"]));
    db.drop().await;
}
