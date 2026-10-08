//! Bulk update of CIs through the real router (SHAA-2354).

use axum::Router;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::schemas::BULK_UPDATE_MAX;
use crate::db::scratch;
use crate::modules::api_tokens::tests::{Creds, app, call, code, session_of};

const BULK: &str = "/api/v1/configuration-items/bulk-update";

struct World {
    app: Router,
    admin: Creds,
    pool: PgPool,
    server: Uuid,
    switch: Uuid,
}

async fn class(app: &Router, admin: &Creds, key: &str, field: &str) -> Uuid {
    let (status, v, _) =
        call(app, "POST", "/api/v1/ci-classes", admin, Some(json!({ "key": key, "name": key.to_uppercase() }))).await;
    assert_eq!(status, 201, "{v}");
    let id: Uuid = v["id"].as_str().unwrap().parse().unwrap();
    let def = json!({ "classId": id, "key": field, "label": field, "dataType": "number" });
    let (status, v, _) = call(app, "POST", "/api/v1/attribute-definitions", admin, Some(def)).await;
    assert_eq!(status, 201, "{v}");
    id
}

async fn world(db: &scratch::Scratch) -> World {
    let app = app(db.pool.clone());
    let setup = json!({ "username": "owner", "email": "owner@example.test", "displayName": "Owner", "password": "correct horse battery", "setupToken": crate::auth::setup_token::TEST_TOKEN });
    let (status, me, headers) = call(&app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
    assert_eq!(status, 201, "{me}");
    let admin = session_of(&me, &headers);
    let server = class(&app, &admin, "server", "cpu_cores").await;
    let switch = class(&app, &admin, "switch", "port_count").await;
    World { app, admin, pool: db.pool.clone(), server, switch }
}

impl World {
    async fn ci(&self, class: Uuid) -> Uuid {
        let body = json!({ "classId": class });
        let (status, v, _) = call(&self.app, "POST", "/api/v1/configuration-items", &self.admin, Some(body)).await;
        assert_eq!(status, 201, "{v}");
        v["id"].as_str().unwrap().parse().unwrap()
    }

    async fn attribute(&self, id: Uuid, key: &str) -> Value {
        let (status, v, _) =
            call(&self.app, "GET", &format!("/api/v1/configuration-items/{id}"), &self.admin, None).await;
        assert_eq!(status, 200, "{v}");
        v["attributes"][key].clone()
    }

    async fn updates(&self, ids: &[Uuid]) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM audit_log
             WHERE entity_type = 'configuration_items' AND action = 'update' AND entity_id = ANY($1)",
        )
        .bind(ids)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    /// A signed-in user with these rights on the server class (view, edit) and none on the switch class.
    async fn user(&self, name: &str, edit: bool) -> Creds {
        let profile: Uuid = sqlx::query_scalar("INSERT INTO permission_profiles (name) VALUES ($1) RETURNING id")
            .bind(format!("profile {name}"))
            .fetch_one(&self.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO permission_profile_class_permissions (profile_id, class_id, can_view, can_edit)
             VALUES ($1, $2, true, $3)",
        )
        .bind(profile)
        .bind(self.server)
        .bind(edit)
        .execute(&self.pool)
        .await
        .unwrap();
        let body = json!({ "username": name, "email": format!("{name}@example.test"), "displayName": format!("User {name}"), "password": "a long enough password", "profileIds": [profile] });
        let (status, v, _) = call(&self.app, "POST", "/api/v1/admin/users", &self.admin, Some(body)).await;
        assert_eq!(status, 201, "{v}");
        let login = json!({ "username": name, "password": "a long enough password" });
        let (status, me, headers) = call(&self.app, "POST", "/api/v1/auth/login", &Creds::default(), Some(login)).await;
        assert_eq!(status, 200, "{me}");
        session_of(&me, &headers)
    }
}

/// The CIs that pass are written with one audit row each; the others are
/// reported in the request's order with what a single PATCH would answer.
/// `allOrNothing` writes nothing when one CI is refused.
#[tokio::test]
async fn bulk_update_reports_each_ci_and_commits_the_ones_that_pass() {
    let Some(db) = scratch::database("bulk_update_reports_each_ci_and_commits_the_ones_that_pass").await else {
        return;
    };
    let w = world(&db).await;
    let servers = [w.ci(w.server).await, w.ci(w.server).await, w.ci(w.server).await];
    let switch = w.ci(w.switch).await;
    let missing = Uuid::new_v4();
    let ids = json!([servers[0], switch, servers[1], missing, servers[2]]);

    let (status, v, _) =
        call(&w.app, "POST", BULK, &w.admin, Some(json!({ "ids": ids, "attributes": { "cpu_cores": 8 } }))).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        (v["succeeded"].as_i64(), v["failed"].as_i64(), v["committed"].as_bool()),
        (Some(3), Some(2), Some(true))
    );
    let results = v["results"].as_array().unwrap();
    let order: Vec<(i64, &str, bool)> = results
        .iter()
        .map(|r| (r["index"].as_i64().unwrap(), r["id"].as_str().unwrap(), r["ok"].as_bool().unwrap()))
        .collect();
    let expected: Vec<(i64, String, bool)> = vec![
        (0, servers[0].to_string(), true),
        (1, switch.to_string(), false),
        (2, servers[1].to_string(), true),
        (3, missing.to_string(), false),
        (4, servers[2].to_string(), true),
    ];
    let order: Vec<(i64, String, bool)> = order.into_iter().map(|(i, id, ok)| (i, id.to_owned(), ok)).collect();
    assert_eq!(order, expected, "{v}");
    assert_eq!(results[0]["item"]["id"].as_str(), Some(servers[0].to_string().as_str()));
    assert_eq!(results[0]["item"]["version"].as_i64(), Some(2), "{v}");
    assert!(results[0]["error"].is_null());
    assert_eq!(results[3]["error"]["code"], "NOT_FOUND", "{v}");
    assert!(results[1]["item"].is_null());

    // The refusal is the one a single PATCH of the switch answers.
    let (status, single, _) = call(
        &w.app,
        "PATCH",
        &format!("/api/v1/configuration-items/{switch}"),
        &w.admin,
        Some(json!({ "attributes": { "cpu_cores": 8 } })),
    )
    .await;
    assert_eq!(status, 400, "{single}");
    assert_eq!(results[1]["error"]["code"], single["error"]["code"]);
    assert_eq!(results[1]["error"]["message"], single["error"]["message"]);
    assert_eq!(results[1]["error"]["details"], single["error"]["details"], "{v}");

    for s in servers {
        assert_eq!(w.attribute(s, "cpu_cores").await, json!(8));
    }
    assert_eq!(w.updates(&servers).await, 3, "one audit row per CI written");
    assert_eq!(w.updates(&[switch]).await, 0);

    // All or nothing: one refused CI and nothing is written.
    let body =
        json!({ "ids": [servers[0], switch, servers[1]], "attributes": { "cpu_cores": 16 }, "allOrNothing": true });
    let (status, v, _) = call(&w.app, "POST", BULK, &w.admin, Some(body)).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        (v["succeeded"].as_i64(), v["failed"].as_i64(), v["committed"].as_bool()),
        (Some(2), Some(1), Some(false))
    );
    assert!(v["results"][0]["ok"].as_bool().unwrap() && v["results"][0]["item"].is_null(), "{v}");
    assert_eq!(w.attribute(servers[0], "cpu_cores").await, json!(8));
    assert_eq!(w.updates(&servers).await, 3, "no audit row from a rolled-back bulk update");

    // All or nothing with nothing refused commits; null clears, as in PATCH.
    let body = json!({ "ids": servers, "attributes": { "cpu_cores": null }, "allOrNothing": true });
    let (status, v, _) = call(&w.app, "POST", BULK, &w.admin, Some(body)).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!((v["succeeded"].as_i64(), v["committed"].as_bool()), (Some(3), Some(true)));
    assert_eq!(w.attribute(servers[1], "cpu_cores").await, Value::Null);
    assert_eq!(w.updates(&servers).await, 6);

    // A bad value is refused per CI like any other validation error.
    let body = json!({ "ids": [servers[0]], "attributes": { "cpu_cores": "many" } });
    let (status, v, _) = call(&w.app, "POST", BULK, &w.admin, Some(body)).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["results"][0]["error"]["code"], "VALIDATION_ERROR", "{v}");
    assert_eq!(v["results"][0]["error"]["details"][0]["field"], "attributes.cpu_cores", "{v}");
}

/// Each CI is checked against the caller's rights on its class: view-only is
/// 403, a class the caller may not view is 404 like a missing CI.
#[tokio::test]
async fn bulk_update_checks_the_rights_of_each_ci() {
    let Some(db) = scratch::database("bulk_update_checks_the_rights_of_each_ci").await else { return };
    let w = world(&db).await;
    let server = w.ci(w.server).await;
    let switch = w.ci(w.switch).await;
    let viewer = w.user("viewer", false).await;
    let editor = w.user("editor", true).await;
    let body = json!({ "ids": [server, switch], "attributes": { "cpu_cores": 4 } });

    let (status, v, _) = call(&w.app, "POST", BULK, &viewer, Some(body.clone())).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["results"][0]["error"]["code"], "FORBIDDEN", "{v}");
    assert_eq!(v["results"][1]["error"]["code"], "NOT_FOUND", "{v}");

    let (status, v, _) = call(&w.app, "POST", BULK, &editor, Some(body)).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        (v["results"][0]["ok"].as_bool(), v["results"][1]["error"]["code"].as_str()),
        (Some(true), Some("NOT_FOUND")),
        "{v}"
    );
    assert_eq!(w.attribute(server, "cpu_cores").await, json!(4));
    let actor: Option<String> =
        sqlx::query_scalar("SELECT actor_name FROM audit_log WHERE entity_id = $1 AND action = 'update'")
            .bind(server)
            .fetch_optional(&w.pool)
            .await
            .unwrap();
    assert!(actor.as_deref().is_some_and(|a| a.contains("editor")), "{actor:?}");
}

/// The body is checked before any CI is looked at.
#[tokio::test]
async fn bulk_update_refuses_a_malformed_body() {
    let Some(db) = scratch::database("bulk_update_refuses_a_malformed_body").await else { return };
    let w = world(&db).await;
    let id = w.ci(w.server).await;
    let too_many: Vec<Uuid> = (0..=BULK_UPDATE_MAX).map(|_| Uuid::new_v4()).collect();
    let cases = [
        (json!({ "ids": [], "attributes": { "cpu_cores": 1 } }), "ids"),
        (json!({ "ids": too_many, "attributes": { "cpu_cores": 1 } }), "ids"),
        (json!({ "ids": [id, id], "attributes": { "cpu_cores": 1 } }), "ids"),
        (json!({ "ids": [id] }), "(root)"),
        (json!({ "ids": [id], "attributes": {} }), "(root)"),
        (json!({ "ids": [id], "attributes": { "cpu_cores": 1 }, "classId": w.switch }), "classId"),
    ];
    for (body, field) in cases {
        let (status, v, _) = call(&w.app, "POST", BULK, &w.admin, Some(body.clone())).await;
        assert_eq!((status, code(&v)), (400, "VALIDATION_ERROR"), "{body}: {v}");
        let fields: Vec<&str> =
            v["error"]["details"].as_array().unwrap().iter().filter_map(|d| d["field"].as_str()).collect();
        assert!(fields.contains(&field), "{body}: {v}");
    }
    assert_eq!(w.updates(&[id]).await, 0);
}
