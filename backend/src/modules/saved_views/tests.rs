//! Saved views through the real router on a scratch database (SHAA-578 §5.1,
//! §5.2): CRUD, every limit at its boundary, validation parity with the list
//! endpoint, the permission cases, defaults, audit, resolution and the
//! configuration file.

use axum::Router;
use axum::http::header;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::definition::SavedViewDefinition;
use crate::db::scratch;
use crate::modules::api_tokens::tests::{Creds, app, call, code, session_of};

const VIEWS: &str = "/api/v1/saved-views";

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
    let setup = json!({ "username": "admin", "displayName": "Admin", "password": password,
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

    /// A profile with these global rights that may view these classes.
    async fn profile(&self, name: &str, global: &[&str], view: &[&str]) -> String {
        let mut classes = Vec::new();
        for k in view {
            classes.push(
                json!({ "classId": self.class(k).await, "view": true, "create": true, "edit": true, "delete": true }),
            );
        }
        let body = json!({ "name": name, "globalPermissions": global, "classPermissions": classes });
        let (status, p) = self.call(&self.admin, "POST", "/api/v1/admin/profiles", Some(body)).await;
        assert_eq!(status, 201, "{p}");
        p["id"].as_str().unwrap().to_owned()
    }

    /// A signed-in user holding these profiles.
    async fn user(&self, name: &str, profiles: &[&str]) -> (Creds, String) {
        let body = json!({ "username": name, "displayName": name, "password": self.password, "profileIds": profiles });
        let (status, u) = self.call(&self.admin, "POST", "/api/v1/admin/users", Some(body)).await;
        assert_eq!(status, 201, "{u}");
        let login = json!({ "username": name, "password": self.password });
        let (status, me, headers) = call(&self.app, "POST", "/api/v1/auth/login", &Creds::default(), Some(login)).await;
        assert_eq!(status, 200, "{me}");
        (session_of(&me, &headers), u["id"].as_str().unwrap().to_owned())
    }

    async fn create(&self, who: &Creds, body: Value) -> (u16, Value) {
        self.call(who, "POST", VIEWS, Some(body)).await
    }

    async fn created(&self, who: &Creds, body: Value) -> Value {
        let (status, v) = self.create(who, body).await;
        assert_eq!(status, 201, "{v}");
        v
    }

    async fn stored(&self, id: &str) -> Value {
        sqlx::query_scalar("SELECT definition FROM saved_views WHERE id = $1::uuid")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn audit_rows(&self, id: &str) -> Vec<(String, String, Option<Value>, Option<Value>)> {
        sqlx::query_as(
            "SELECT action, actor_type, old_value, new_value FROM audit_log
             WHERE entity_type = 'saved_views' AND entity_id = $1::uuid ORDER BY id",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }
}

fn details(v: &Value) -> Vec<(String, String)> {
    v["error"]["details"]
        .as_array()
        .map(|d| {
            d.iter()
                .map(|e| (e["field"].as_str().unwrap_or("").to_owned(), e["code"].as_str().unwrap_or("").to_owned()))
                .collect()
        })
        .unwrap_or_default()
}

fn has(v: &Value, field: &str, code: &str) -> bool {
    details(v).iter().any(|(f, c)| f == field && c == code)
}

fn view(context: &str, name: &str, visibility: &str, definition: Value) -> Value {
    json!({ "context": context, "name": name, "visibility": visibility, "definition": definition })
}

/// The resolved query as a query string.
fn query_string(q: &Value) -> String {
    q.as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| format!("{k}={}", v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string())))
        .collect::<Vec<_>>()
        .join("&")
}

async fn ci(w: &World, class: &str, name: &str, env: &str) -> String {
    let body = json!({ "classId": w.class(class).await, "attributes": {
        "name": name, "status": w.value("status", "in_service").await, "environment": w.value("environment", env).await } });
    let (status, v) = w.call(&w.admin, "POST", "/api/v1/configuration-items", Some(body)).await;
    assert_eq!(status, 201, "{v}");
    v["id"].as_str().unwrap().to_owned()
}

/// §5.1 CRUD and validation parity, §2.5 every limit at its boundary.
#[tokio::test]
async fn personal_views_round_trip_and_hold_every_limit() {
    let Some((db, w)) = world("saved_views_round_trip").await else { return };
    let admin = w.admin.clone();
    let prod = ci(&w, "server", "web-1", "production").await;
    ci(&w, "server", "web-2", "staging").await;
    ci(&w, "virtual_machine", "vm-1", "production").await;

    // Create: 201 with Location, resolved into the list's own parameters.
    let definition = json!({ "classKeys": ["server"], "filters": { "lookups": { "environment": ["production"] } },
        "sort": { "field": "attributes.cpu_cores", "direction": "desc" },
        "columns": ["label", "attributes.os_family"], "pageSize": 50 });
    let (status, v, headers) =
        call(&w.app, "POST", VIEWS, &admin, Some(view("inventory", "Production servers", "personal", definition)))
            .await;
    assert_eq!(status, 201, "{v}");
    let id = v["id"].as_str().unwrap().to_owned();
    assert_eq!(headers.get(header::LOCATION).unwrap().to_str().unwrap(), format!("{VIEWS}/{id}"));
    assert_eq!(
        (v["visibility"].as_str(), v["home"].as_str(), v["canEdit"].as_bool(), v["version"].as_i64()),
        (Some("personal"), Some("server"), Some(true), Some(1))
    );
    assert_eq!(v["resolved"]["state"], "ok");
    let q = &v["resolved"]["query"];
    assert_eq!(q["classId"].as_str().unwrap(), w.class("server").await.to_string());
    assert_eq!(q["lookupValueId"].as_str().unwrap(), w.value("environment", "production").await.to_string());
    assert_eq!((q["sort"].as_str(), q["limit"].as_i64()), (Some("-attributes.cpu_cores"), Some(50)));
    assert_eq!(v["resolved"]["columns"], json!(["label", "attributes.os_family"]));
    assert!(v.get("defaultCount").is_none(), "personal views have no default count");

    // The resolved query is a valid list request and returns exactly the rows a direct call does.
    let (status, listed) =
        w.call(&admin, "GET", &format!("/api/v1/configuration-items?{}", query_string(q)), None).await;
    assert_eq!(status, 200, "{listed}");
    let ids: Vec<&str> = listed["data"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert_eq!(ids, [prod.as_str()]);

    // Names are unique per owner and context, ignoring case; the same name in another context is fine.
    let (status, dup) =
        w.create(&admin, view("inventory", "PRODUCTION servers", "personal", json!({ "classKeys": ["server"] }))).await;
    assert_eq!((status, code(&dup)), (409, "CONFLICT"));
    assert!(has(&dup, "name", "duplicate_name"), "{dup}");
    let search =
        w.created(&admin, view("search", "Production servers", "personal", json!({ "filters": { "q": "web" } }))).await;
    assert_eq!((search["home"].clone(), search["resolved"]["query"]["q"].as_str()), (Value::Null, Some("web")));

    let (status, list) = w.call(&admin, "GET", &format!("{VIEWS}?context=inventory"), None).await;
    assert_eq!(status, 200);
    assert_eq!(list["data"].as_array().unwrap().len(), 1);
    assert_eq!(list["limits"], json!({ "personal": { "used": 2, "max": 200 }, "shared": { "used": 0, "max": 500 } }));
    let (status, v) = w.call(&admin, "GET", &format!("{VIEWS}?context=report"), None).await;
    assert_eq!(status, 400, "{v}");
    let (status, v) = w.call(&admin, "GET", &format!("{VIEWS}?nope=1"), None).await;
    assert_eq!(status, 400, "unknown query parameters are refused: {v}");

    // PATCH: version check, rename, duplicate, nothing to change.
    let by_id = format!("{VIEWS}/{id}");
    let (status, v) = w.call(&admin, "PATCH", &by_id, Some(json!({ "version": 7, "name": "x" }))).await;
    assert_eq!((status, code(&v)), (409, "VERSION_CONFLICT"), "{v}");
    let (status, v) = w.call(&admin, "PATCH", &by_id, Some(json!({ "version": 1 }))).await;
    assert_eq!(status, 400, "{v}");
    let (status, v) =
        w.call(&admin, "PATCH", &by_id, Some(json!({ "version": 1, "name": "Prod", "description": "Mine" }))).await;
    assert_eq!((status, v["version"].as_i64(), v["description"].as_str()), (200, Some(2), Some("Mine")), "{v}");
    w.created(&admin, view("inventory", "Other", "personal", json!({}))).await;
    let (status, v) = w.call(&admin, "PATCH", &by_id, Some(json!({ "version": 2, "name": "other" }))).await;
    assert!(status == 409 && has(&v, "name", "duplicate_name"), "{v}");

    // DELETE: version check, then gone.
    let (status, v) = w.call(&admin, "DELETE", &format!("{by_id}?version=1"), None).await;
    assert_eq!((status, code(&v)), (409, "VERSION_CONFLICT"));
    let (status, _) = w.call(&admin, "DELETE", &format!("{by_id}?version=2"), None).await;
    assert_eq!(status, 204);
    let (status, _) = w.call(&admin, "GET", &by_id, None).await;
    assert_eq!(status, 404);

    // Validation parity with the list endpoint (§5.1).
    let refused = |definition: Value, context: &'static str| {
        let w = &w;
        let admin = admin.clone();
        async move {
            let (status, v) = w.create(&admin, view(context, "Refused", "personal", definition)).await;
            assert_eq!(status, 400, "{v}");
            details(&v)
        }
    };
    let field = |f: &str, c: &str| vec![(f.to_owned(), c.to_owned())];
    assert_eq!(
        refused(json!({ "sort": { "field": "attributes.hostname" } }), "inventory").await,
        field("definition.sort.field", "class_required")
    );
    assert_eq!(
        refused(
            json!({ "classKeys": ["application"], "sort": { "field": "attributes.primary_database" } }),
            "inventory"
        )
        .await,
        field("definition.sort.field", "not_sortable")
    );
    assert_eq!(
        refused(
            json!({ "classKeys": ["server", "virtual_machine"], "sort": { "field": "attributes.cpu_cores" } }),
            "inventory"
        )
        .await,
        field("definition.sort.field", "unknown_attribute")
    );
    assert_eq!(
        refused(
            json!({ "classKeys": ["server", "virtual_machine"], "sort": { "field": "attributes.name" } }),
            "inventory"
        )
        .await,
        field("definition.sort.field", "ambiguous_attribute")
    );
    assert_eq!(
        refused(json!({ "classKeys": ["server"], "columns": ["attributes.vcpu"] }), "inventory").await,
        field("definition.columns.0", "unknown_attribute")
    );
    assert_eq!(
        refused(json!({ "filters": { "lookups": { "nope": ["a"] } } }), "inventory").await,
        field("definition.filters.lookups.nope", "unknown_lookup")
    );
    assert_eq!(
        refused(json!({ "filters": { "lookups": { "environment": ["moon"] } } }), "inventory").await,
        field("definition.filters.lookups.environment.0", "unknown_lookup")
    );
    assert_eq!(
        refused(json!({ "classKeys": ["nope"] }), "inventory").await,
        field("definition.classKeys.0", "unknown_class")
    );
    assert_eq!(refused(json!({}), "search").await, field("definition.filters.q", "required"));
    assert_eq!(
        refused(json!({ "filters": { "q": "x" }, "sort": { "field": "label" } }), "search").await,
        field("definition.sort", "not_allowed")
    );
    assert!(has(
        &json!({ "error": { "details": refused(json!({ "nope": 1 }), "inventory").await.iter()
        .map(|(f, c)| json!({ "field": f, "code": c })).collect::<Vec<_>>() } }),
        "definition",
        "unrecognized_keys"
    ));

    // §2.5 limits, n accepted (or refused for another reason) and n + 1 refused.
    let keys = |n: usize| (0..n).map(|i| format!("k{i}")).collect::<Vec<_>>();
    let codes = refused(json!({ "classKeys": keys(100) }), "inventory").await;
    assert!(codes.iter().all(|(_, c)| c == "unknown_class") && codes.len() == 100);
    assert_eq!(refused(json!({ "classKeys": keys(101) }), "inventory").await, field("definition.classKeys", "too_big"));
    let codes = refused(json!({ "filters": { "lookups": { "a": keys(50), "b": keys(50) } } }), "inventory").await;
    assert!(codes.iter().all(|(_, c)| c == "unknown_lookup"), "{codes:?}");
    assert_eq!(
        refused(json!({ "filters": { "lookups": { "a": keys(50), "b": keys(51) } } }), "inventory").await,
        field("definition.filters.lookups", "too_big")
    );
    let attrs = |n: usize| (0..n).map(|i| format!("attributes.a{i}")).collect::<Vec<_>>();
    let codes = refused(json!({ "columns": attrs(50) }), "inventory").await;
    assert!(codes.iter().all(|(_, c)| c == "class_required") && codes.len() == 50);
    assert_eq!(refused(json!({ "columns": attrs(51) }), "inventory").await, field("definition.columns", "too_big"));
    for (size, ok) in [(9, false), (10, true), (200, true), (201, false)] {
        let (status, v) =
            w.create(&admin, view("inventory", &format!("Page {size}"), "personal", json!({ "pageSize": size }))).await;
        assert_eq!(status == 201, ok, "{size}: {v}");
    }
    for (len, ok) in [(100, true), (101, false)] {
        let (status, v) = w.create(&admin, view("inventory", &"n".repeat(len), "personal", json!({}))).await;
        assert_eq!(status == 201, ok, "name {len}: {v}");
        let body = json!({ "context": "search", "name": format!("Description {len}"), "visibility": "personal",
            "description": "d".repeat(len * 5), "definition": { "filters": { "q": "x" } } });
        let (status, v) = w.create(&admin, body).await;
        assert_eq!(status == 201, ok, "description {}: {v}", len * 5);
    }
    let (status, v) = w.create(&admin, view("inventory", "Control", "personal", json!({}))).await;
    assert_eq!(status, 201, "{v}");
    let (status, v) = w.create(&admin, view("inventory", "Ctl\u{7}", "personal", json!({}))).await;
    assert_eq!(status, 400, "control characters are refused: {v}");
    let (status, v) = w.create(&admin, view("inventory", "   ", "personal", json!({}))).await;
    assert_eq!(status, 400, "blank names are refused: {v}");

    // 16 KiB of compact JSON: exactly at the limit passes the size check, one byte more does not.
    for (size, too_large) in [(16 * 1024, false), (16 * 1024 + 1, true)] {
        let d = sized_definition(size);
        let (status, v) =
            w.create(&admin, view("inventory", "Big", "personal", serde_json::to_value(&d).unwrap())).await;
        assert_eq!(status, 400);
        assert_eq!(has(&v, "definition", "too_large"), too_large, "{size}: {:?}", details(&v));
    }

    // 200 personal views per user; 500 shared views per instance.
    let me: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE username = 'admin'").fetch_one(&w.pool).await.unwrap();
    let used: i64 = sqlx::query_scalar("SELECT count(*) FROM saved_views WHERE owner_id = $1")
        .bind(me)
        .fetch_one(&w.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO saved_views (owner_id, context, name, definition, created_by_name, updated_by_name)
         SELECT $1, 'inventory', 'Filler ' || n, '{}', 'admin', 'admin' FROM generate_series(1, $2) n",
    )
    .bind(me)
    .bind(199 - used as i32)
    .execute(&w.pool)
    .await
    .unwrap();
    w.created(&admin, view("inventory", "Number 200", "personal", json!({}))).await;
    let (status, v) = w.create(&admin, view("inventory", "Number 201", "personal", json!({}))).await;
    assert!(status == 409 && has(&v, "(root)", "limit_reached"), "{v}");
    let (status, v) = w
        .call(
            &admin,
            "POST",
            &format!("{VIEWS}/{}/copy", search["id"].as_str().unwrap()),
            Some(json!({ "name": "Copy 201", "visibility": "personal" })),
        )
        .await;
    assert!(status == 409 && has(&v, "(root)", "limit_reached"), "copies count too: {v}");
    sqlx::query(
        "INSERT INTO saved_views (owner_id, context, name, definition, created_by_name, updated_by_name)
         SELECT NULL, 'inventory', 'Shared ' || n, '{}', 'admin', 'admin' FROM generate_series(1, 499) n",
    )
    .execute(&w.pool)
    .await
    .unwrap();
    w.created(&admin, view("inventory", "Shared 500", "shared", json!({}))).await;
    let (status, v) = w.create(&admin, view("inventory", "Shared 501", "shared", json!({}))).await;
    assert!(status == 409 && has(&v, "(root)", "limit_reached"), "{v}");

    db.drop().await;
}

/// A definition of exactly `bytes` bytes of compact JSON, with keys the
/// schema accepts (none of which exist).
fn sized_definition(bytes: usize) -> SavedViewDefinition {
    let long = |prefix: &str, i: usize| format!("{prefix}{i:03}{}", "x".repeat(58));
    let mut d = SavedViewDefinition { class_keys: (0..100).map(|i| long("k", i)).collect(), ..Default::default() };
    d.filters.lookups.insert("l".into(), (0..100).map(|i| long("v", i)).collect());
    d.columns = (0..50).map(|i| format!("attributes.{}", long("c", i))).collect();
    let mut excess = d.json_bytes() - bytes;
    for c in d.columns.iter_mut().rev() {
        let cut = excess.min(58);
        c.truncate(c.len() - cut);
        excess -= cut;
    }
    assert_eq!(d.json_bytes(), bytes);
    d
}

/// §5.2 permission cases 1–9, 11 and 12, defaults, copies, audit and session-only access.
#[tokio::test]
async fn views_never_reveal_or_widen_beyond_the_callers_rights() {
    let Some((db, w)) = world("saved_views_permissions").await else { return };
    let admin = w.admin.clone();
    let both = w.profile("Servers and VMs", &[], &["server", "virtual_machine"]).await;
    let servers = w.profile("Servers", &[], &["server"]).await;
    let sharers = w.profile("Server curators", &["views.share"], &["server"]).await;
    let auditors = w.profile("Server auditors", &["audit.view"], &["server"]).await;
    let (a, _) = w.user("alice", &[&both]).await;
    let (b, b_id) = w.user("bob", &[&servers]).await;
    let (s, _) = w.user("sam", &[&sharers]).await;
    let (r, _) = w.user("rita", &[&auditors]).await;
    let srv = ci(&w, "server", "srv-1", "production").await;
    ci(&w, "virtual_machine", "vm-1", "production").await;

    // 1. A class B cannot view is refused exactly like one that does not exist.
    let (status, hidden) =
        w.create(&b, view("inventory", "VMs", "personal", json!({ "classKeys": ["virtual_machine"] }))).await;
    let (_, unknown) =
        w.create(&b, view("inventory", "VMs", "personal", json!({ "classKeys": ["doesnotexist"] }))).await;
    assert_eq!(status, 400);
    assert_eq!(details(&hidden), details(&unknown));
    assert_eq!(
        hidden["error"]["details"][0]["message"].as_str().unwrap().replace("virtual_machine", "K"),
        unknown["error"]["details"][0]["message"].as_str().unwrap().replace("doesnotexist", "K")
    );
    // 2. A server attribute column is fine; 3. a VM attribute sort is not.
    w.created(
        &b,
        view(
            "inventory",
            "Server cores",
            "personal",
            json!({ "classKeys": ["server"], "columns": ["label", "attributes.cpu_cores"] }),
        ),
    )
    .await;
    let (status, v) = w
        .create(
            &b,
            view(
                "inventory",
                "By vCPU",
                "personal",
                json!({ "classKeys": ["server"], "sort": { "field": "attributes.vcpu" } }),
            ),
        )
        .await;
    assert!(status == 400 && has(&v, "definition.sort.field", "unknown_attribute"), "{v}");

    // 4. A shared view on servers and VMs: B sees the server part only, degraded, the VM key never named.
    let mixed = w
        .created(
            &admin,
            view(
                "inventory",
                "Fleet",
                "shared",
                json!({ "classKeys": ["server", "virtual_machine"], "sort": { "field": "label" } }),
            ),
        )
        .await;
    let mixed_id = mixed["id"].as_str().unwrap().to_owned();
    assert_eq!((mixed["defaultCount"].as_i64(), mixed["canEdit"].as_bool()), (Some(0), Some(true)));
    let (_, list) = w.call(&b, "GET", VIEWS, None).await;
    assert!(!list.to_string().contains("virtual_machine"), "{list}");
    let fleet = list["data"].as_array().unwrap().iter().find(|v| v["id"] == mixed["id"]).unwrap().clone();
    assert_eq!(fleet["definition"]["classKeys"], json!(["server"]));
    assert_eq!((fleet["resolved"]["state"].as_str(), fleet["canEdit"].as_bool()), (Some("degraded"), Some(false)));
    assert!(fleet.get("defaultCount").is_none());
    let issues = fleet["resolved"]["issues"].as_array().unwrap();
    assert_eq!(issues.len(), 1);
    assert_eq!(
        (issues[0]["code"].as_str(), issues[0]["path"].as_str()),
        (Some("not_available"), Some("definition.classKeys"))
    );
    assert_eq!(fleet["resolved"]["query"]["classId"].as_str().unwrap(), w.class("server").await.to_string());
    let (_, rows) = w
        .call(&b, "GET", &format!("/api/v1/configuration-items?{}", query_string(&fleet["resolved"]["query"])), None)
        .await;
    let rows: Vec<&str> = rows["data"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert_eq!(rows, [srv.as_str()], "running it as B returns only servers");
    // A sees both classes.
    let (_, seen) = w.call(&a, "GET", &format!("{VIEWS}/{mixed_id}"), None).await;
    assert_eq!(
        (seen["definition"]["classKeys"].as_array().unwrap().len(), seen["resolved"]["state"].as_str()),
        (2, Some("ok"))
    );

    // 5. A shared view on VMs only: not in B's list, and 404 by id.
    let vms =
        w.created(&admin, view("inventory", "VMs only", "shared", json!({ "classKeys": ["virtual_machine"] }))).await;
    let vms_id = vms["id"].as_str().unwrap().to_owned();
    let (_, list) = w.call(&b, "GET", VIEWS, None).await;
    assert!(list["data"].as_array().unwrap().iter().all(|v| v["id"] != vms["id"]));
    let (status, _) = w.call(&b, "GET", &format!("{VIEWS}/{vms_id}"), None).await;
    assert_eq!(status, 404);

    // 6. A's personal view is 404 to B whatever B does.
    let mine = w.created(&a, view("inventory", "Alice's", "personal", json!({ "classKeys": ["server"] }))).await;
    let mine_path = format!("{VIEWS}/{}", mine["id"].as_str().unwrap());
    for (method, path, body) in [
        ("GET", mine_path.clone(), None),
        ("PATCH", mine_path.clone(), Some(json!({ "version": 1, "name": "Taken" }))),
        ("DELETE", format!("{mine_path}?version=1"), None),
        ("POST", format!("{mine_path}/copy"), Some(json!({ "name": "Stolen", "visibility": "personal" }))),
    ] {
        let (status, v) = w.call(&b, method, &path, body).await;
        assert_eq!((status, code(&v)), (404, "NOT_FOUND"), "{method} {path}");
    }
    let (status, _) = w.call(&admin, "GET", &mine_path, None).await;
    assert_eq!(status, 404, "administrators do not read personal views either");

    // 7. B may not create, change or delete a shared view, but may copy it and make it a default.
    let (status, v) =
        w.create(&b, view("inventory", "Mine for all", "shared", json!({ "classKeys": ["server"] }))).await;
    assert_eq!((status, code(&v)), (403, "FORBIDDEN"));
    let (status, _) =
        w.call(&b, "PATCH", &format!("{VIEWS}/{mixed_id}"), Some(json!({ "version": 1, "name": "x" }))).await;
    assert_eq!(status, 403);
    let (status, _) = w.call(&b, "DELETE", &format!("{VIEWS}/{mixed_id}?version=1"), None).await;
    assert_eq!(status, 403);
    let (status, copy) = w
        .call(
            &b,
            "POST",
            &format!("{VIEWS}/{mixed_id}/copy"),
            Some(json!({ "name": "My fleet", "visibility": "personal" })),
        )
        .await;
    assert_eq!(status, 201, "{copy}");
    assert_eq!(
        (copy["visibility"].as_str(), copy["definition"]["classKeys"].clone()),
        (Some("personal"), json!(["server"]))
    );
    assert!(
        !w.stored(copy["id"].as_str().unwrap()).await.to_string().contains("virtual_machine"),
        "only what B sees is copied"
    );
    let (status, v) = w
        .call(&b, "POST", &format!("{VIEWS}/{mixed_id}/copy"), Some(json!({ "name": "x", "visibility": "shared" })))
        .await;
    assert_eq!(status, 403, "{v}");
    let set = |class: Value, view: Value| json!({ "context": "inventory", "classKey": class, "viewId": view });
    let (status, v) = w.call(&b, "PUT", &format!("{VIEWS}/defaults"), Some(set(Value::Null, json!(mixed_id)))).await;
    assert_eq!((status, v), (200, json!({ "context": "inventory", "home": null, "viewId": mixed_id })));
    let (status, v) =
        w.call(&b, "PUT", &format!("{VIEWS}/defaults"), Some(set(json!("server"), json!(mixed_id)))).await;
    assert!(
        status == 400 && has(&v, "classKey", "home_mismatch"),
        "a two-class view belongs to the unscoped list: {v}"
    );
    let (status, _) = w.call(&b, "PUT", &format!("{VIEWS}/defaults"), Some(set(Value::Null, json!(vms_id)))).await;
    assert_eq!(status, 404);
    let search = w.created(&b, view("search", "Find web", "personal", json!({ "filters": { "q": "web" } }))).await;
    let (status, v) =
        w.call(&b, "PUT", &format!("{VIEWS}/defaults"), Some(set(Value::Null, search["id"].clone()))).await;
    assert!(status == 400 && has(&v, "viewId", "not_inventory"), "D7: {v}");
    let (status, v) = w
        .call(
            &b,
            "PUT",
            &format!("{VIEWS}/defaults"),
            Some(json!({ "context": "search", "classKey": null, "viewId": null })),
        )
        .await;
    assert_eq!(status, 400, "{v}");
    let (_, fleet) = w.call(&b, "GET", &format!("{VIEWS}/{mixed_id}"), None).await;
    assert_eq!(fleet["isDefault"], true);
    let (_, seen) = w.call(&admin, "GET", &format!("{VIEWS}/{mixed_id}"), None).await;
    assert_eq!((seen["isDefault"].as_bool(), seen["defaultCount"].as_i64()), (Some(false), Some(1)));

    // 8. S edits the shared view: the VM key S cannot see stays stored.
    let (status, edited) = w
        .call(
            &s,
            "PATCH",
            &format!("{VIEWS}/{mixed_id}"),
            Some(json!({ "version": 1,
        "definition": { "classKeys": ["server"], "sort": { "field": "ident", "direction": "desc" } } })),
        )
        .await;
    assert_eq!(status, 200, "{edited}");
    assert_eq!(edited["definition"]["classKeys"], json!(["server"]));
    assert!(!edited.to_string().contains("virtual_machine"));
    assert_eq!(w.stored(&mixed_id).await["classKeys"], json!(["server", "virtual_machine"]));
    assert_eq!(w.stored(&mixed_id).await["sort"], json!({ "field": "ident", "direction": "desc" }));
    let (_, fleet) = w.call(&b, "GET", &format!("{VIEWS}/{mixed_id}"), None).await;
    assert_eq!(fleet["isDefault"], true, "the home did not move, so B's default stays");

    // 9. S loses views.share through the profile API (where the right is visible) and is refused at once.
    let (_, p) = w.call(&admin, "GET", &format!("/api/v1/admin/profiles/{sharers}"), None).await;
    assert_eq!(p["globalPermissions"], json!(["views.share"]));
    let (status, p) = w
        .call(&admin, "PATCH", &format!("/api/v1/admin/profiles/{sharers}"), Some(json!({ "globalPermissions": [] })))
        .await;
    assert_eq!(status, 200, "{p}");
    let (status, v) =
        w.call(&s, "PATCH", &format!("{VIEWS}/{mixed_id}"), Some(json!({ "version": 2, "name": "Renamed" }))).await;
    assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{v}");

    // 11. An auditor limited to servers never sees the VM key in saved_views entries.
    let share_copy = w
        .call(
            &admin,
            "POST",
            &format!("{VIEWS}/{}/copy", copy["id"].as_str().unwrap()),
            Some(json!({ "name": "B's", "visibility": "shared" })),
        )
        .await;
    assert_eq!(share_copy.0, 404, "the admin cannot read B's personal view");
    let personal = w
        .created(
            &admin,
            view("inventory", "Admin fleet", "personal", json!({ "classKeys": ["server", "virtual_machine"] })),
        )
        .await;
    let (status, shared_copy) = w
        .call(
            &admin,
            "POST",
            &format!("{VIEWS}/{}/copy", personal["id"].as_str().unwrap()),
            Some(json!({ "name": "Fleet copy", "visibility": "shared" })),
        )
        .await;
    assert_eq!(status, 201, "{shared_copy}");
    let (status, log) = w.call(&r, "GET", "/api/v1/audit-log?entityType=saved_views&limit=200", None).await;
    assert_eq!(status, 200, "{log}");
    assert!(!log.to_string().contains("virtual_machine"), "{log}");
    let entries = log["data"].as_array().unwrap();
    assert!(entries.iter().any(|e| e["newValue"]["hiddenClassKeyCount"] == 1), "{log}");
    let (_, full) = w.call(&admin, "GET", "/api/v1/audit-log?entityType=saved_views&limit=200", None).await;
    assert!(full.to_string().contains("virtual_machine"));
    assert_eq!(full["page"]["total"], log["page"]["total"], "the entries themselves are not hidden");

    // §3.4: one row per shared change with the right values; personal changes write none.
    let rows = w.audit_rows(&mixed_id).await;
    let summary: Vec<(&str, &str)> = rows.iter().map(|(a, t, _, _)| (a.as_str(), t.as_str())).collect();
    assert_eq!(summary, [("create", "user"), ("update", "user")]);
    let (_, _, old, new) = &rows[1];
    assert_eq!(old.as_ref().unwrap()["definition"]["sort"], json!({ "field": "label", "direction": "asc" }));
    assert_eq!(new.as_ref().unwrap()["definition"]["sort"]["field"], "ident");
    assert_eq!(new.as_ref().unwrap()["visibility"], "shared");
    let rows = w.audit_rows(shared_copy["id"].as_str().unwrap()).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].3.as_ref().unwrap()["copiedFrom"], personal["id"]);
    let personal_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE entity_type = 'saved_views' AND entity_id IN
           (SELECT id FROM saved_views WHERE owner_id IS NOT NULL)",
    )
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!(personal_rows, 0, "personal views are not audited");
    let (status, _) = w.call(&admin, "DELETE", &format!("{VIEWS}/{vms_id}?version=1"), None).await;
    assert_eq!(status, 204);
    let rows = w.audit_rows(&vms_id).await;
    assert_eq!(rows.iter().map(|(a, ..)| a.as_str()).collect::<Vec<_>>(), ["create", "delete"]);
    assert!(rows[1].2.as_ref().unwrap()["name"] == "VMs only" && rows[1].3.is_none());

    // 12. Resolution never widens: with every value of its filter archived the view is unavailable.
    let staging = w
        .created(
            &admin,
            view(
                "inventory",
                "Staging servers",
                "shared",
                json!({ "classKeys": ["server"], "filters": { "lookups": { "environment": ["staging", "test"] } } }),
            ),
        )
        .await;
    let staging_path = format!("{VIEWS}/{}", staging["id"].as_str().unwrap());
    for (value, state, count) in [("staging", "degraded", 1), ("test", "unavailable", 0)] {
        let id = w.value("environment", value).await;
        let (status, v) = w
            .call(&admin, "PATCH", &format!("/api/v1/lookup-list-values/{id}"), Some(json!({ "isActive": false })))
            .await;
        assert_eq!(status, 200, "{v}");
        let (_, v) = w.call(&b, "GET", &staging_path, None).await;
        assert_eq!(v["resolved"]["state"], state, "{v}");
        let ids = v["resolved"]["query"]["lookupValueId"].as_str().map_or(0, |s| s.split(',').count());
        assert_eq!(ids, count, "{v}");
    }
    let (_, v) = w.call(&b, "GET", &staging_path, None).await;
    assert!(v["resolved"]["query"].is_null(), "an unavailable view has no query to run: {v}");
    let (status, v) =
        w.call(&b, "PUT", &format!("{VIEWS}/defaults"), Some(set(json!("server"), staging["id"].clone()))).await;
    assert!(status == 400 && has(&v, "viewId", "unavailable"), "{v}");
    assert_eq!(
        w.stored(staging["id"].as_str().unwrap()).await["filters"]["lookups"]["environment"],
        json!(["staging", "test"])
    );

    // Defaults go with their view; a user's personal views and defaults go with the user, shared views stay.
    let server_view =
        w.created(&b, view("inventory", "Only servers", "personal", json!({ "classKeys": ["server"] }))).await;
    let (status, v) =
        w.call(&b, "PUT", &format!("{VIEWS}/defaults"), Some(set(json!("server"), server_view["id"].clone()))).await;
    assert_eq!(status, 200, "{v}");
    let (status, _) =
        w.call(&b, "DELETE", &format!("{VIEWS}/{}?version=1", server_view["id"].as_str().unwrap()), None).await;
    assert_eq!(status, 204);
    let defaults = |user: String| {
        let pool = w.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM saved_view_defaults WHERE user_id = $1::uuid")
                .bind(user)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    assert_eq!(defaults(b_id.clone()).await, 1, "only the unscoped default is left");
    let (status, v) = w.call(&b, "PUT", &format!("{VIEWS}/defaults"), Some(set(Value::Null, Value::Null))).await;
    assert_eq!((status, v["viewId"].clone()), (200, Value::Null));
    assert_eq!(defaults(b_id.clone()).await, 0);
    w.call(&b, "PUT", &format!("{VIEWS}/defaults"), Some(set(Value::Null, json!(mixed_id)))).await;
    let (status, v) = w.call(&admin, "DELETE", &format!("/api/v1/admin/users/{b_id}"), None).await;
    assert!(status == 204 || status == 200, "{v}");
    let (personal_left, defaults_left, shared_left): (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM saved_views WHERE owner_id = $1::uuid),
                (SELECT count(*) FROM saved_view_defaults WHERE user_id = $1::uuid),
                (SELECT count(*) FROM saved_views WHERE owner_id IS NULL)",
    )
    .bind(&b_id)
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!((personal_left, defaults_left, shared_left), (0, 0, 3));
    let creator: String = sqlx::query_scalar("SELECT created_by_name FROM saved_views WHERE id = $1::uuid")
        .bind(&mixed_id)
        .fetch_one(&w.pool)
        .await
        .unwrap();
    assert_eq!(creator, "admin");

    // D8: an API token gets 403 on every saved-view operation.
    let readers = w.profile("Token scope", &["views.share"], &["server"]).await;
    let expires = (chrono::Utc::now() + chrono::Duration::days(30)).to_rfc3339();
    let (status, t) = w
        .call(
            &admin,
            "POST",
            "/api/v1/admin/api-tokens",
            Some(json!({ "name": "views", "profileId": readers, "expiresAt": expires })),
        )
        .await;
    assert_eq!(status, 201, "{t}");
    let token = Creds { bearer: t["secret"].as_str().map(str::to_owned), ..Creds::default() };
    let one = format!("{VIEWS}/{mixed_id}");
    for (method, path, body) in [
        ("GET", VIEWS.to_owned(), None),
        ("POST", VIEWS.to_owned(), Some(view("inventory", "Token", "personal", json!({})))),
        ("PUT", format!("{VIEWS}/defaults"), Some(set(Value::Null, Value::Null))),
        ("GET", one.clone(), None),
        ("PATCH", one.clone(), Some(json!({ "version": 2, "name": "T" }))),
        ("DELETE", format!("{one}?version=2"), None),
        ("POST", format!("{one}/copy"), Some(json!({ "name": "T", "visibility": "personal" }))),
    ] {
        let (status, v) = w.call(&token, method, &path, body).await;
        assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{method} {path}");
    }
    // Writes need the CSRF token like every session write.
    let no_csrf = Creds { csrf: None, ..a.clone() };
    let (status, v) = w.create(&no_csrf, view("inventory", "No CSRF", "personal", json!({}))).await;
    assert_eq!((status, code(&v)), (403, "CSRF_TOKEN_INVALID"));

    db.drop().await;
}

/// §2.6, one case per row: archive and restore, dropped columns and sorts,
/// lookup values, purged classes; the stored definition never changes.
#[tokio::test]
async fn resolution_follows_the_data_model_without_rewriting_the_view() {
    let Some((db, w)) = world("saved_views_resolution").await else { return };
    let admin = w.admin.clone();
    let definition = json!({ "classKeys": ["server", "network_device"], "includeSubclasses": false,
        "filters": { "lookups": { "environment": ["production", "staging"] }, "active": "all", "deleted": "include",
                     "ipWithin": "10.0.0.0/8", "q": "web" },
        "sort": { "field": "attributes.hostname" }, "columns": ["label", "attributes.hostname"], "pageSize": 25 });
    let v = w.created(&admin, view("inventory", "Fleet", "personal", definition.clone())).await;
    let id = v["id"].as_str().unwrap().to_owned();
    let path = format!("{VIEWS}/{id}");
    let q = &v["resolved"]["query"];
    assert_eq!(v["resolved"]["state"], "ok", "{v}");
    assert_eq!(q["includeSubclasses"], "false");
    assert_eq!(
        (q["active"].as_str(), q["deleted"].as_str(), q["ipWithin"].as_str()),
        (Some("all"), Some("include"), Some("10.0.0.0/8"))
    );
    assert_eq!(
        (q["sort"].as_str(), q["q"].as_str(), q["limit"].as_i64()),
        (Some("attributes.hostname"), Some("web"), Some(25))
    );
    assert_eq!(v["home"], Value::Null, "two classes: the unscoped inventory");
    let (status, rows) = w.call(&admin, "GET", &format!("/api/v1/configuration-items?{}", query_string(q)), None).await;
    assert_eq!(status, 200, "the resolved query is a valid list request: {rows}");
    let stored = w.stored(&id).await;
    let get = || async { w.call(&admin, "GET", &path, None).await.1 };
    let issue = |v: &Value| {
        v["resolved"]["issues"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| (i["path"].as_str().unwrap().to_owned(), i["code"].as_str().unwrap().to_owned()))
            .collect::<Vec<_>>()
    };

    // A class archived: still applied, with an info issue; restored: as before.
    let device = w.class("network_device").await;
    let archive = |id: Uuid, active: bool| {
        let w = &w;
        let admin = admin.clone();
        async move {
            let (status, v) =
                w.call(&admin, "PATCH", &format!("/api/v1/ci-classes/{id}"), Some(json!({ "isActive": active }))).await;
            assert_eq!(status, 200, "{v}");
        }
    };
    archive(device, false).await;
    let v = get().await;
    assert_eq!(
        (v["resolved"]["state"].as_str(), issue(&v)),
        (Some("ok"), vec![("definition.classKeys.1".to_owned(), "class_archived".to_owned())])
    );
    assert_eq!(v["resolved"]["issues"][0]["severity"], "info");
    assert_eq!(v["resolved"]["query"]["classId"].as_str().unwrap().split(',').count(), 2);
    archive(device, true).await;
    assert!(issue(&get().await).is_empty());

    // An attribute archived: the column goes, the sort falls back to label; both only present results.
    let hostname: Uuid = sqlx::query_scalar(
        "SELECT a.id FROM ci_attribute_definitions a JOIN ci_classes c ON c.id = a.class_id WHERE c.key = 'hardware' AND a.key = 'hostname'",
    )
    .fetch_one(&w.pool)
    .await
    .unwrap();
    let (status, v) = w
        .call(&admin, "PATCH", &format!("/api/v1/attribute-definitions/{hostname}"), Some(json!({ "isActive": false })))
        .await;
    assert_eq!(status, 200, "{v}");
    let v = get().await;
    assert_eq!(v["resolved"]["state"], "degraded");
    assert_eq!(
        issue(&v),
        [
            ("definition.sort.field".to_owned(), "unknown_attribute".to_owned()),
            ("definition.columns.1".to_owned(), "unknown_attribute".to_owned())
        ]
    );
    assert!(v["resolved"]["query"].get("sort").is_none(), "the list sorts by label: {v}");
    assert_eq!(v["resolved"]["columns"], json!(["label"]));
    // The messages name the attribute by its label, as operators know it (§1.5), not by its key.
    let label: String = sqlx::query_scalar("SELECT label FROM ci_attribute_definitions WHERE id = $1")
        .bind(hostname)
        .fetch_one(&w.pool)
        .await
        .unwrap();
    assert_eq!(v["resolved"]["issues"][1]["message"], format!("Column \"{label}\" no longer exists and is left out"));
    assert_eq!(
        v["resolved"]["issues"][0]["message"],
        format!("Sort by \"{label}\" is no longer possible; the list sorts by label")
    );
    w.call(&admin, "PATCH", &format!("/api/v1/attribute-definitions/{hostname}"), Some(json!({ "isActive": true })))
        .await;

    // A lookup value archived: dropped from the OR; the other stays.
    let staging = w.value("environment", "staging").await;
    w.call(&admin, "PATCH", &format!("/api/v1/lookup-list-values/{staging}"), Some(json!({ "isActive": false }))).await;
    let v = get().await;
    assert_eq!(
        (v["resolved"]["state"].as_str(), issue(&v)),
        (
            Some("degraded"),
            vec![("definition.filters.lookups.environment.1".to_owned(), "unknown_lookup_value".to_owned())]
        )
    );
    let (list, value): (String, String) = sqlx::query_as(
        "SELECT l.name, v.name FROM lookup_list_values v JOIN lookup_lists l ON l.id = v.list_id WHERE v.id = $1",
    )
    .bind(staging)
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!(
        v["resolved"]["issues"][0]["message"],
        format!("\"{value}\" is no longer an active value of lookup list \"{list}\" and is left out")
    );
    assert_eq!(
        v["resolved"]["query"]["lookupValueId"].as_str().unwrap(),
        w.value("environment", "production").await.to_string()
    );
    w.call(&admin, "PATCH", &format!("/api/v1/lookup-list-values/{staging}"), Some(json!({ "isActive": true }))).await;
    assert_eq!(get().await["resolved"]["state"], "ok");

    // A purged class (here: a key no class has) among several: dropped; the only one: unavailable.
    let mut gone = stored.clone();
    gone["classKeys"] = json!(["server", "gone_class"]);
    gone["columns"] = json!(["label"]);
    gone.as_object_mut().unwrap().remove("sort");
    sqlx::query("UPDATE saved_views SET definition = $2 WHERE id = $1::uuid")
        .bind(&id)
        .bind(&gone)
        .execute(&w.pool)
        .await
        .unwrap();
    let v = get().await;
    assert_eq!(
        (v["resolved"]["state"].as_str(), issue(&v)),
        (Some("degraded"), vec![("definition.classKeys.1".to_owned(), "unknown_class".to_owned())])
    );
    assert_eq!(v["resolved"]["query"]["classId"].as_str().unwrap(), w.class("server").await.to_string());
    gone["classKeys"] = json!(["gone_class"]);
    sqlx::query("UPDATE saved_views SET definition = $2 WHERE id = $1::uuid")
        .bind(&id)
        .bind(&gone)
        .execute(&w.pool)
        .await
        .unwrap();
    let v = get().await;
    assert_eq!(v["resolved"]["state"], "unavailable");
    assert!(v["resolved"]["query"].is_null(), "never treated as every class: {v}");
    assert_eq!(w.stored(&id).await, gone, "resolution never rewrites the stored definition");

    // Saving the cleaned definition is how the owner fixes it; a home change drops defaults for the old home.
    let (status, v) =
        w.call(&admin, "PATCH", &path, Some(json!({ "version": 1, "definition": { "classKeys": ["server"] } }))).await;
    assert_eq!((status, v["resolved"]["state"].as_str(), v["home"].as_str()), (200, Some("ok"), Some("server")), "{v}");
    let set = json!({ "context": "inventory", "classKey": "server", "viewId": id });
    let (status, _) = w.call(&admin, "PUT", &format!("{VIEWS}/defaults"), Some(set)).await;
    assert_eq!(status, 200);
    let (status, v) = w
        .call(&admin, "PATCH", &path, Some(json!({ "version": 2, "definition": { "classKeys": ["database"] } })))
        .await;
    assert_eq!((status, v["isDefault"].as_bool()), (200, Some(false)), "{v}");
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM saved_view_defaults").fetch_one(&w.pool).await.unwrap();
    assert_eq!(left, 0);
    db.drop().await;
}

/// §4: config export v6 carries shared views only; import merges them by
/// context and name, never deletes, warns about unknown keys on a dry run,
/// needs views.share; files of versions 1 to 5 still import.
#[tokio::test]
async fn shared_views_travel_with_the_configuration_file() {
    use crate::modules::config_transfer::format::{ConfigFile, FORMAT_VERSION};
    use crate::modules::config_transfer::{ImportMode, export, import};

    let Some((src_db, src)) = world("saved_views_config_src").await else { return };
    let Some((dst_db, dst)) = world("saved_views_config_dst").await else { return };
    let admin = src.admin.clone();
    src.created(
        &admin,
        view(
            "inventory",
            "Production",
            "shared",
            json!({ "classKeys": ["server"], "filters": { "lookups": { "environment": ["production"] } } }),
        ),
    )
    .await;
    src.created(&admin, view("search", "Web", "shared", json!({ "filters": { "q": "web" } }))).await;
    src.created(&admin, view("inventory", "Private", "personal", json!({}))).await;
    src.profile("Curators", &["views.share", "customization.manage"], &["server"]).await;

    let system = crate::api::context::RequestContext::system("test", "test");
    let file = export(&src.pool, &system).await.unwrap();
    assert_eq!(file.format_version, FORMAT_VERSION);
    let views = file.saved_views.clone().unwrap();
    let names: Vec<&str> = views.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(names, ["Production", "Web"], "shared views only, by context and name");
    let curators = file.permission_profiles.as_ref().unwrap().iter().find(|p| p.name == "Curators").unwrap();
    assert!(curators.global_permissions.contains(&crate::auth::permissions::GlobalPermission::ViewsShare));
    let json = serde_json::to_string(&file).unwrap();
    assert!(!json.contains("Private"));

    // A view already on the target is updated in place, another is kept, and nothing is deleted.
    let keep = dst.created(&dst.admin, view("inventory", "Kept", "shared", json!({}))).await;
    let old =
        dst.created(&dst.admin, view("inventory", "PRODUCTION", "shared", json!({ "classKeys": ["database"] }))).await;
    let only_views = ConfigFile {
        data_model: None,
        lookups: None,
        ui_settings: None,
        permission_profiles: None,
        import_mappings: None,
        ..file.clone()
    };
    let res = import(&dst.pool, &system, &only_views, ImportMode::Apply).await.unwrap();
    let section = res.summary.iter().find(|s| s.section == "savedViews").unwrap();
    assert_eq!((section.created, section.updated, section.not_in_file), (1, 1, 1), "{:?}", res.summary);
    let (_, list) = dst.call(&dst.admin, "GET", VIEWS, None).await;
    let names: Vec<&str> = list["data"].as_array().unwrap().iter().map(|v| v["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Kept", "PRODUCTION", "Web"]);
    assert_eq!(dst.stored(old["id"].as_str().unwrap()).await["classKeys"], json!(["server"]));
    assert!(keep["id"].is_string());
    let actors: Vec<String> = sqlx::query_scalar(
        "SELECT actor_type FROM audit_log WHERE entity_type = 'saved_views' AND actor_type = 'import'",
    )
    .fetch_all(&dst.pool)
    .await
    .unwrap();
    assert_eq!(actors.len(), 2, "one import row per view written");
    let again = import(&dst.pool, &system, &only_views, ImportMode::Apply).await.unwrap();
    let section = again.summary.iter().find(|s| s.section == "savedViews").unwrap();
    assert_eq!((section.created, section.updated, section.unchanged), (0, 0, 2));

    // Keys the target lacks are warnings on a dry run, and the view is still accepted.
    let mut odd = only_views.clone();
    odd.saved_views.as_mut().unwrap()[0].definition =
        serde_json::from_value(json!({ "classKeys": ["warp_core"], "columns": ["attributes.flux"], "filters": { "lookups": { "galaxy": ["andromeda"] } } })).unwrap();
    let res = import(&dst.pool, &system, &odd, ImportMode::DryRun).await.unwrap();
    let paths: Vec<&str> = res.warnings.iter().map(|w| w.path.as_str()).collect();
    assert!(paths.contains(&"savedViews.0.definition.classKeys.0"), "{paths:?}");
    assert!(paths.contains(&"savedViews.0.definition.filters.lookups.galaxy"), "{paths:?}");
    assert!(!res.applied);

    // The section needs views.share (dry run included); an unknown version is refused clearly; v5 files import.
    let (customiser, _) = dst.user("carol", &[&dst.profile("Exporters", &["config.export_import"], &[]).await]).await;
    let (status, v) = dst
        .call(
            &customiser,
            "POST",
            "/api/v1/admin/config/import?mode=dry_run",
            Some(serde_json::to_value(&only_views).unwrap()),
        )
        .await;
    assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{v}");
    let (status, v) = dst.call(&customiser, "GET", "/api/v1/admin/config/export", None).await;
    assert_eq!(status, 200);
    assert!(v.get("savedViews").is_none(), "left out of the export without views.share");
    let err = import(
        &dst.pool,
        &system,
        &ConfigFile { format_version: FORMAT_VERSION + 1, ..only_views.clone() },
        ImportMode::DryRun,
    )
    .await
    .unwrap_err();
    assert!(err.message.contains("versions 1 to 6") || format!("{err:?}").contains("versions 1 to 6"), "{err:?}");
    let v5 = ConfigFile { format_version: 5, saved_views: None, ..only_views };
    import(&dst.pool, &system, &v5, ImportMode::DryRun).await.unwrap();

    src_db.drop().await;
    dst_db.drop().await;
}

/// GH#475/#476: a restricted importer neither sees nor rewrites a shared view
/// that the API answers 404 to them, and a diff of a view they do see never
/// names a class they cannot view; on a dry run and on apply alike.
#[tokio::test]
async fn config_import_respects_what_the_importer_may_see() {
    let Some((db, w)) = world("saved_views_config_hidden").await else { return };
    let admin = w.admin.clone();
    let mut hidden = view("inventory", "Hidden", "shared", json!({ "classKeys": ["database"] }));
    hidden["description"] = json!("Secret notes");
    let hidden = w.created(&admin, hidden).await;
    let mixed =
        w.created(&admin, view("inventory", "Mixed", "shared", json!({ "classKeys": ["server", "database"] }))).await;
    let profile = w.profile("Importers", &["views.share", "config.export_import"], &["server"]).await;
    let (dave, _) = w.user("dave", &[&profile]).await;
    let file = json!({ "format": "shadoucmdb.config", "formatVersion": 6, "savedViews": [
        { "context": "inventory", "name": "hidden", "description": "Mine now", "definition": { "classKeys": ["server"] } },
        { "context": "inventory", "name": "Mixed", "description": "Changed", "definition": { "classKeys": ["server"], "columns": ["label"] } }
    ] });
    let before = (w.stored(hidden["id"].as_str().unwrap()).await, w.stored(mixed["id"].as_str().unwrap()).await);

    for mode in ["dry_run", "apply"] {
        let (status, v) =
            w.call(&dave, "POST", &format!("/api/v1/admin/config/import?mode={mode}"), Some(file.clone())).await;
        assert_eq!(status, 200, "{v}");
        let body = v.to_string();
        assert!(!body.contains("database") && !body.contains("Secret notes"), "{mode}: {body}");
        let warned: Vec<&str> = v["warnings"].as_array().unwrap().iter().map(|w| w["path"].as_str().unwrap()).collect();
        assert_eq!(warned, ["savedViews.0"], "{mode}: the hidden view is skipped with a warning");
        let section = v["summary"].as_array().unwrap().iter().find(|s| s["section"] == "savedViews").unwrap();
        assert_eq!((section["created"].as_i64(), section["updated"].as_i64()), (Some(0), Some(1)), "{mode}: {v}");
    }

    let hidden_row: (Option<String>, Value) =
        sqlx::query_as("SELECT description, definition FROM saved_views WHERE id = $1::uuid")
            .bind(hidden["id"].as_str().unwrap())
            .fetch_one(&w.pool)
            .await
            .unwrap();
    assert_eq!(hidden_row, (Some("Secret notes".to_owned()), before.0), "the hidden view is untouched");
    let after = w.stored(mixed["id"].as_str().unwrap()).await;
    assert_eq!(after["classKeys"], json!(["server", "database"]), "the hidden class stays in the visible view");
    assert_eq!(after["columns"], json!(["label"]));
    assert_ne!(after, before.1);

    db.drop().await;
}
