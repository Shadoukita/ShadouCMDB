//! The design-time API through the real router against PostgreSQL (SHAA-1423).

use axum::Router;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::db::scratch;
use crate::modules::api_tokens::tests::{Creds, app, call, code, session_of};

const BASE: &str = "/api/v1/admin/workflow-definitions";

struct World {
    app: Router,
    pool: PgPool,
    admin: Creds,
    password: String,
    server: Uuid,
    /// Lookup field `lifecycle` of the server type, on list `server_state`.
    lifecycle: Uuid,
    /// Value ids of `server_state` by key.
    values: Vec<(String, Uuid)>,
}

async fn post(app: &Router, creds: &Creds, path: &str, body: Value) -> Value {
    let (status, v, _) = call(app, "POST", path, creds, Some(body)).await;
    assert_eq!(status, 201, "POST {path}: {v}");
    v
}

fn id(v: &Value) -> Uuid {
    v["id"].as_str().unwrap().parse().unwrap()
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

async fn world(db: &scratch::Scratch) -> World {
    let app = app(db.pool.clone());
    // Random per test run, so no hard-coded credential reaches the hasher or verifier.
    let password = format!("test passphrase {}", Uuid::new_v4());
    let setup = json!({ "username": "admin", "email": "admin@example.test", "displayName": "Admin",
        "password": password, "setupToken": crate::auth::setup_token::TEST_TOKEN });
    let (status, me, headers) = call(&app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
    assert_eq!(status, 201, "{me}");
    let admin = session_of(&me, &headers);
    let server = id(&post(&app, &admin, "/api/v1/ci-classes", json!({ "key": "server", "name": "Server" })).await);
    let list =
        id(&post(&app, &admin, "/api/v1/lookup-lists", json!({ "key": "server_state", "name": "Server state" })).await);
    let mut values = Vec::new();
    for key in ["planned", "approved", "live", "decommissioned"] {
        let v =
            post(&app, &admin, "/api/v1/lookup-list-values", json!({ "listId": list, "key": key, "name": key })).await;
        values.push((key.to_owned(), id(&v)));
    }
    let field =
        |key: &str, data_type: &str| json!({ "classId": server, "key": key, "label": key, "dataType": data_type });
    let lifecycle = id(&post(
        &app,
        &admin,
        "/api/v1/attribute-definitions",
        json!({ "classId": server, "key": "lifecycle", "label": "Lifecycle", "dataType": "lookup", "lookupListId": list }),
    )
    .await);
    post(&app, &admin, "/api/v1/attribute-definitions", field("owner_team", "text")).await;
    post(&app, &admin, "/api/v1/attribute-definitions", field("risk", "integer")).await;
    post(&app, &admin, "/api/v1/attribute-definitions", field("notes", "text")).await;
    let mut env = field("environment", "enum");
    env["enumValues"] = json!(["prod", "test"]);
    post(&app, &admin, "/api/v1/attribute-definitions", env).await;
    World { app, pool: db.pool.clone(), admin, password, server, lifecycle, values }
}

impl World {
    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let (status, v, _) = call(&self.app, method, path, &self.admin, body).await;
        (status, v)
    }

    /// A signed-in user holding a profile with these global rights and no class rights.
    async fn user(&self, name: &str, globals: &[&str]) -> Creds {
        self.scoped_user(name, globals, &[]).await
    }

    /// A signed-in user holding a profile with these global rights, and view
    /// (with edit when `true`) on these types.
    async fn scoped_user(&self, name: &str, globals: &[&str], classes: &[(Uuid, bool)]) -> Creds {
        let profile: Uuid = sqlx::query_scalar("INSERT INTO permission_profiles (name) VALUES ($1) RETURNING id")
            .bind(format!("{name} profile"))
            .fetch_one(&self.pool)
            .await
            .unwrap();
        for g in globals {
            sqlx::query("INSERT INTO permission_profile_global_permissions (profile_id, permission) VALUES ($1, $2)")
                .bind(profile)
                .bind(g)
                .execute(&self.pool)
                .await
                .unwrap();
        }
        for (class, edit) in classes {
            sqlx::query(
                "INSERT INTO permission_profile_class_permissions
                   (profile_id, class_id, can_view, can_create, can_edit, can_delete)
                 VALUES ($1, $2, true, $3, $3, $3)",
            )
            .bind(profile)
            .bind(class)
            .bind(edit)
            .execute(&self.pool)
            .await
            .unwrap();
        }
        let body = json!({ "username": name, "email": format!("{name}@example.test"), "displayName": name,
            "password": self.password, "profileIds": [profile] });
        let (status, v) = self.call("POST", "/api/v1/admin/users", Some(body)).await;
        assert_eq!(status, 201, "{v}");
        let body = json!({ "username": name, "password": self.password });
        let (status, me, headers) = call(&self.app, "POST", "/api/v1/auth/login", &Creds::default(), Some(body)).await;
        assert_eq!(status, 200, "{me}");
        session_of(&me, &headers)
    }

    async fn ci(&self) -> Uuid {
        let (status, v) =
            self.call("POST", "/api/v1/configuration-items", Some(json!({ "classId": self.server }))).await;
        assert_eq!(status, 201, "{v}");
        id(&v)
    }

    fn value(&self, key: &str) -> Uuid {
        self.values.iter().find(|(k, _)| k == key).unwrap().1
    }

    async fn audit(&self, entity: Uuid) -> Vec<(String, Option<Value>, Option<Value>)> {
        sqlx::query_as(
            "SELECT action, old_value, new_value FROM audit_log
             WHERE entity_type = 'workflow_definitions' AND entity_id = $1 ORDER BY id",
        )
        .bind(entity)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }
}

/// The design example of §6.1, valid against the world's server type.
fn lifecycle_graph() -> Value {
    json!({
        "initialState": "planned",
        "states": [
            { "key": "planned", "name": "Planned", "category": "open", "terminal": false, "stateValue": "planned" },
            { "key": "approved", "name": "Approved", "category": "active", "terminal": false, "stateValue": "approved" },
            { "key": "done", "name": "In production", "category": "done", "terminal": true, "stateValue": "live" }
        ],
        "transitions": [
            { "key": "approve", "name": "Approve", "from": "planned", "to": "approved", "requiresComment": true,
              "fields": [ { "attribute": "owner_team", "required": true } ],
              "conditions": { "all": [
                  { "field": "environment", "op": "in", "value": ["prod"] },
                  { "any": [ { "field": "owner_team", "op": "isSet" }, { "field": "risk", "op": "lte", "value": 2 } ] }
              ] } },
            { "key": "go_live", "name": "Go live", "from": "approved", "to": "done", "requiresComment": false, "fields": [] }
        ],
        "layout": { "planned": { "x": 0, "y": 0 }, "approved": { "x": 240, "y": 0 } }
    })
}

/// Create → draft PUT → validate → publish → retire, with every refusal on
/// the way: 409 VERSION_CONFLICT on a stale version or checksum, lint
/// failures as 400 with one detail per problem, IN_USE from the field and
/// lookup value deletion paths, the activation warning, grants, and audit.
#[tokio::test]
async fn workflow_definitions_are_designed_published_and_retired() {
    let Some(db) = scratch::database("workflow_definitions_lifecycle").await else { return };
    let w = world(&db).await;

    // Create: inactive by default, with an empty draft v1.
    let body = json!({ "key": "server_lifecycle", "name": "Server lifecycle", "classId": w.server,
        "stateAttributeId": w.lifecycle });
    let (status, d) = w.call("POST", BASE, Some(body.clone())).await;
    assert_eq!(status, 201, "{d}");
    assert_eq!(
        (d["isActive"].as_bool(), d["draftVersionNo"].as_i64(), d["currentVersionNo"].is_null(), d["version"].as_i64()),
        (Some(false), Some(1), true, Some(1)),
        "{d}"
    );
    assert_eq!(d["stateAttributeKey"], "lifecycle");
    assert_eq!(d["warnings"], json!([]));
    let def = id(&d);
    let by_id = format!("{BASE}/{def}");
    let draft = format!("{by_id}/draft");

    // Refusals on create.
    let mut dup = body.clone();
    dup["key"] = json!("SERVER_lifecycle".to_lowercase());
    let (status, v) = w.call("POST", BASE, Some(dup)).await;
    assert_eq!((status, code(&v)), (409, "CONFLICT"), "{v}");
    assert_eq!(details(&v), [("key".to_owned(), "unique".to_owned())]);
    let (status, v) = w.call("POST", BASE, Some(json!({ "key": "x", "name": "X", "classId": Uuid::new_v4() }))).await;
    assert_eq!((status, details(&v)), (400, vec![("classId".to_owned(), "not_found".to_owned())]), "{v}");
    let risk: Uuid = sqlx::query_scalar("SELECT id FROM ci_attribute_definitions WHERE key = 'risk'")
        .fetch_one(&w.pool)
        .await
        .unwrap();
    let (status, v) = w
        .call("POST", BASE, Some(json!({ "key": "y", "name": "Y", "classId": w.server, "stateAttributeId": risk })))
        .await;
    assert_eq!(
        (status, details(&v)),
        (400, vec![("stateAttributeId".to_owned(), "invalid_state_attribute".to_owned())])
    );

    // The empty draft lints as unpublishable.
    let (status, v) = w.call("GET", &draft, None).await;
    assert_eq!((status, v["states"].as_array().map(Vec::len), v["status"].as_str()), (200, Some(0), Some("draft")));
    let (status, v) = w.call("POST", &format!("{draft}/validate"), None).await;
    assert_eq!((status, v["valid"].as_bool()), (200, Some(false)), "{v}");
    assert_eq!(v["problems"][0]["code"], "no_states");

    // Draft PUT: what cannot be resolved is 400 with paths.
    let mut bad = lifecycle_graph();
    bad["states"][0]["stateValue"] = json!("unknown_value");
    bad["transitions"][0]["fields"][0]["attribute"] = json!("nope");
    bad["transitions"][0]["conditions"]["all"][1]["any"][1]["value"] = json!("two");
    bad["transitions"][1]["conditions"] = json!({ "field": "owner_team", "op": "gt", "value": "a" });
    let (status, v) = w.call("PUT", &draft, Some(bad)).await;
    assert_eq!(status, 400, "{v}");
    assert_eq!(
        details(&v),
        [
            ("states[0].stateValue", "unknown_value"),
            ("transitions[0].fields[0].attribute", "unknown_attribute"),
            ("transitions[0].conditions.all[1].any[1].value", "type"),
            ("transitions[1].conditions.op", "op_type"),
        ]
        .map(|(a, b)| (a.to_owned(), b.to_owned()))
    );
    let mut bad = lifecycle_graph();
    bad["states"][1]["key"] = json!("planned");
    bad["transitions"][1]["to"] = json!("approved");
    bad["initialState"] = json!("ghost");
    let (status, v) = w.call("PUT", &draft, Some(bad)).await;
    assert_eq!(status, 400, "{v}");
    let found = details(&v);
    for expected in [("states[1].key", "duplicate"), ("initialState", "unknown_state"), ("transitions[1].to", "loop")] {
        assert!(found.contains(&(expected.0.to_owned(), expected.1.to_owned())), "{expected:?} in {found:?}");
    }

    // A graph the lint refuses: an unreachable state and a dead end.
    let mut broken = lifecycle_graph();
    broken["states"].as_array_mut().unwrap().push(json!({ "key": "limbo", "name": "Limbo", "category": "active" }));
    let (status, v) = w.call("PUT", &draft, Some(broken)).await;
    assert_eq!(status, 200, "{v}");
    let broken_sum = v["checksum"].as_str().unwrap().to_owned();
    let (status, v) = w.call("POST", &format!("{draft}/validate"), None).await;
    assert_eq!((status, v["valid"].as_bool(), v["checksum"].as_str()), (200, Some(false), Some(broken_sum.as_str())));
    let codes: Vec<(&str, &str, &str)> = v["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["path"].as_str().unwrap(), p["code"].as_str().unwrap(), p["severity"].as_str().unwrap()))
        .collect();
    assert!(codes.contains(&("states[3]", "unreachable_state", "error")), "{codes:?}");
    assert!(codes.contains(&("states[3]", "dead_end", "error")), "{codes:?}");
    assert!(codes.contains(&("transitions[0]", "ungranted_transition", "warning")), "{codes:?}");
    let (status, v) =
        w.call("POST", &format!("{draft}/publish"), Some(json!({ "expectedDraftChecksum": broken_sum }))).await;
    assert_eq!((status, code(&v)), (400, "VALIDATION_ERROR"), "{v}");
    assert_eq!(
        details(&v),
        [("states[3]", "unreachable_state"), ("states[3]", "dead_end")].map(|(a, b)| (a.to_owned(), b.to_owned()))
    );

    // The good graph; a stale expectedChecksum on PUT is a version conflict.
    let mut stale = lifecycle_graph();
    stale["expectedChecksum"] = json!("0".repeat(64));
    let (status, v) = w.call("PUT", &draft, Some(stale)).await;
    assert_eq!((status, code(&v)), (409, "VERSION_CONFLICT"), "{v}");
    let mut good = lifecycle_graph();
    good["expectedChecksum"] = json!(broken_sum);
    let (status, v) = w.call("PUT", &draft, Some(good)).await;
    assert_eq!(status, 200, "{v}");
    let sum = v["checksum"].as_str().unwrap().to_owned();
    assert_ne!(sum, broken_sum);
    // What was stored reads back in the API form, keys and all.
    let (status, back) = w.call("GET", &draft, None).await;
    assert_eq!(status, 200);
    let expected = lifecycle_graph();
    for k in ["initialState", "states", "transitions", "layout"] {
        assert_eq!(back[k], expected[k], "{k}");
    }
    assert_eq!(back["checksum"].as_str(), Some(sum.as_str()));
    let (status, v) = w.call("POST", &format!("{draft}/validate"), None).await;
    assert_eq!((status, v["valid"].as_bool()), (200, Some(true)), "{v}");

    // Publish: a stale checksum conflicts; the right one publishes v1.
    let (status, v) =
        w.call("POST", &format!("{draft}/publish"), Some(json!({ "expectedDraftChecksum": broken_sum }))).await;
    assert_eq!((status, code(&v)), (409, "VERSION_CONFLICT"), "{v}");
    assert_eq!(details(&v), [("expectedDraftChecksum".to_owned(), "stale".to_owned())]);
    let (status, v) = w
        .call(
            "POST",
            &format!("{draft}/publish"),
            Some(json!({ "expectedDraftChecksum": sum, "changeNote": "First cut" })),
        )
        .await;
    assert_eq!(status, 201, "{v}");
    assert_eq!(
        (v["versionNo"].as_i64(), v["status"].as_str(), v["isCurrent"].as_bool(), v["checksum"].as_str()),
        (Some(1), Some("published"), Some(true), Some(sum.as_str()))
    );
    let (status, d) = w.call("GET", &by_id, None).await;
    assert_eq!(status, 200);
    assert_eq!(
        (
            d["currentVersionNo"].as_i64(),
            d["draftVersionNo"].is_null(),
            d["draftChecksum"].is_null(),
            d["version"].as_i64()
        ),
        (Some(1), true, true, Some(2)),
        "{d}"
    );
    let (status, _) = w.call("GET", &draft, None).await;
    assert_eq!(status, 404);
    let refs: Vec<String> = sqlx::query_scalar(
        "SELECT a.key FROM workflow_version_attribute_refs r JOIN ci_attribute_definitions a ON a.id = r.attribute_id
         ORDER BY a.key",
    )
    .fetch_all(&w.pool)
    .await
    .unwrap();
    assert_eq!(refs, ["environment", "lifecycle", "owner_team", "risk"]);
    // The database keeps the published graph as it is.
    let err = sqlx::query("UPDATE workflow_states SET name = 'x' WHERE key = 'planned'").execute(&w.pool).await;
    assert!(err.is_err());

    // IN_USE from the field paths: archive, retype, enum value removal.
    let (status, v) = w.call("DELETE", &format!("/api/v1/attribute-definitions/{risk}"), None).await;
    assert_eq!((status, code(&v)), (409, "IN_USE"), "{v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("server_lifecycle v1 (published)"), "{v}");
    assert_eq!(details(&v), [("id".to_owned(), "workflow_reference".to_owned())]);
    let (status, v) =
        w.call("PATCH", &format!("/api/v1/attribute-definitions/{risk}"), Some(json!({ "dataType": "text" }))).await;
    assert_eq!((status, code(&v)), (409, "IN_USE"), "{v}");
    let env: Uuid = sqlx::query_scalar("SELECT id FROM ci_attribute_definitions WHERE key = 'environment'")
        .fetch_one(&w.pool)
        .await
        .unwrap();
    let (status, v) =
        w.call("PATCH", &format!("/api/v1/attribute-definitions/{env}"), Some(json!({ "enumValues": ["prod"] }))).await;
    assert_eq!((status, code(&v)), (409, "IN_USE"), "{v}");
    let (status, v) = w.call("DELETE", &format!("/api/v1/attribute-definitions/{}", w.lifecycle), None).await;
    assert_eq!((status, code(&v)), (409, "IN_USE"), "{v}");
    // ... and from the lookup value deletion path.
    let (status, v) = w.call("DELETE", &format!("/api/v1/lookup-list-values/{}", w.value("planned")), None).await;
    assert_eq!((status, code(&v)), (409, "IN_USE"), "{v}");
    assert_eq!(details(&v), [("workflowStates".to_owned(), "in_use".to_owned())]);
    // A value no state maps to still goes.
    let (status, v) =
        w.call("DELETE", &format!("/api/v1/lookup-list-values/{}", w.value("decommissioned")), None).await;
    assert_eq!(status, 204, "{v}");
    // A field only a draft uses can be archived, but not purged.
    let mut next = lifecycle_graph();
    next["transitions"][1]["fields"] = json!([{ "attribute": "notes", "required": false }]);
    let (status, v) = w.call("PUT", &draft, Some(next)).await;
    assert_eq!((status, v["versionNo"].as_i64()), (200, Some(2)), "{v}");
    let notes: Uuid = sqlx::query_scalar("SELECT id FROM ci_attribute_definitions WHERE key = 'notes'")
        .fetch_one(&w.pool)
        .await
        .unwrap();
    let (status, v) = w.call("DELETE", &format!("/api/v1/attribute-definitions/{notes}"), None).await;
    assert_eq!(status, 204, "{v}");
    let (status, v) = w
        .call("POST", &format!("/api/v1/attribute-definitions/{notes}/purge"), Some(json!({ "confirm": "notes" })))
        .await;
    assert_eq!((status, code(&v)), (409, "IN_USE"), "{v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("server_lifecycle v2 (draft)"), "{v}");
    // The lint now flags the archived field in the draft.
    let (_, v) = w.call("POST", &format!("{draft}/validate"), None).await;
    assert!(
        v["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["code"] == "inactive_attribute" && p["path"] == "transitions[1].fields[0].attribute"),
        "{v}"
    );

    // PATCH: stale version; activation warns about CIs without an instance.
    let cis = [w.ci().await, w.ci().await, w.ci().await];
    let gone = w.ci().await;
    let (status, _) = w.call("DELETE", &format!("/api/v1/configuration-items/{gone}"), None).await;
    assert_eq!(status, 204);
    let (status, v) = w.call("PATCH", &by_id, Some(json!({ "version": 1, "isActive": true }))).await;
    assert_eq!((status, code(&v)), (409, "VERSION_CONFLICT"), "{v}");
    assert_eq!(details(&v), [("version".to_owned(), "stale".to_owned())]);
    let (status, v) = w.call("PATCH", &by_id, Some(json!({ "version": 2, "isActive": true }))).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["warnings"].as_array().map(Vec::len), Some(1), "{v}");
    assert_eq!(
        (v["warnings"][0]["code"].as_str(), v["warnings"][0]["count"].as_i64()),
        (Some("UNINSTANCED_CIS"), Some(3))
    );
    assert_eq!(v["version"].as_i64(), Some(3));
    // A rename of the active workflow warns about nothing.
    let (status, v) = w.call("PATCH", &by_id, Some(json!({ "version": 3, "name": "Server lifecycle (EU)" }))).await;
    assert_eq!((status, v["warnings"].clone()), (200, json!([])), "{v}");
    // The state field is fixed once published.
    let (status, v) = w.call("PATCH", &by_id, Some(json!({ "version": 4, "stateAttributeId": null }))).await;
    assert_eq!((status, details(&v)), (409, vec![("stateAttributeId".to_owned(), "published".to_owned())]), "{v}");
    // A second active workflow cannot drive the same field.
    let other = json!({ "key": "other", "name": "Other", "classId": w.server, "stateAttributeId": w.lifecycle,
        "isActive": true });
    let (status, v) = w.call("POST", BASE, Some(other)).await;
    assert_eq!(
        (status, details(&v)),
        (409, vec![("stateAttributeId".to_owned(), "state_attribute_driven".to_owned())])
    );

    // Grants: by name or id; unknown profiles and stale versions refused.
    let profile: Uuid =
        sqlx::query_scalar("INSERT INTO permission_profiles (name) VALUES ('Change managers') RETURNING id")
            .fetch_one(&w.pool)
            .await
            .unwrap();
    let grants = format!("{by_id}/grants");
    let (status, v) = w.call("GET", &grants, None).await;
    assert_eq!((status, v), (200, json!({ "version": 4, "grants": [] })));
    let (status, v) = w
        .call(
            "PUT",
            &grants,
            Some(json!({ "version": 4, "grants": [{ "transitionKey": "approve", "profiles": ["Nobody"] }] })),
        )
        .await;
    assert_eq!((status, details(&v)), (400, vec![("grants[0].profiles[0]".to_owned(), "not_found".to_owned())]));
    let (status, v) = w
        .call(
            "PUT",
            &grants,
            Some(json!({ "version": 3, "grants": [{ "transitionKey": "approve", "profiles": [profile] }] })),
        )
        .await;
    assert_eq!((status, code(&v)), (409, "VERSION_CONFLICT"), "{v}");
    // GH#587: a key in no version or draft is refused, not stored.
    let (status, v) = w
        .call(
            "PUT",
            &grants,
            Some(json!({ "version": 4, "grants": [{ "transitionKey": "teleport", "profiles": [profile] }] })),
        )
        .await;
    assert_eq!(
        (status, details(&v)),
        (400, vec![("grants[0].transitionKey".to_owned(), "unknown_transition".to_owned())])
    );
    let body = json!({ "version": 4, "grants": [
        { "transitionKey": "approve", "profiles": ["change MANAGERS"] },
        { "transitionKey": "_cancel", "profiles": [profile] } ] });
    let (status, v) = w.call("PUT", &grants, Some(body)).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        v,
        json!({ "version": 5, "grants": [
            { "transitionKey": "_cancel", "profiles": [{ "id": profile, "name": "Change managers" }] },
            { "transitionKey": "approve", "profiles": [{ "id": profile, "name": "Change managers" }] } ] })
    );

    // Versions: the draft v2 and the published v1; retire v1.
    let (status, v) = w.call("GET", &format!("{by_id}/versions"), None).await;
    assert_eq!(status, 200, "{v}");
    let rows: Vec<(i64, &str, bool)> = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["versionNo"].as_i64().unwrap(), r["status"].as_str().unwrap(), r["isCurrent"].as_bool().unwrap()))
        .collect();
    assert_eq!(rows, [(2, "draft", false), (1, "published", true)]);
    assert_eq!(v["page"]["total"].as_i64(), Some(2));
    let (status, v) = w.call("GET", &format!("{by_id}/versions/1"), None).await;
    assert_eq!(
        (status, v["changeNote"].as_str(), v["transitions"].as_array().map(Vec::len)),
        (200, Some("First cut"), Some(2))
    );
    let (status, _) = w.call("GET", &format!("{by_id}/versions/9"), None).await;
    assert_eq!(status, 404);
    let (status, v) = w.call("POST", &format!("{by_id}/versions/2/retire"), None).await;
    assert_eq!((status, code(&v)), (409, "CONFLICT"), "{v}");
    let (status, v) = w.call("POST", &format!("{by_id}/versions/1/retire"), None).await;
    assert_eq!(
        (status, v["status"].as_str(), v["activeInstanceCount"].as_i64()),
        (200, Some("retired"), Some(0)),
        "{v}"
    );
    let (status, v) = w.call("POST", &format!("{by_id}/versions/1/retire"), None).await;
    assert_eq!((status, code(&v)), (409, "CONFLICT"), "{v}");
    let (_, d) = w.call("GET", &by_id, None).await;
    assert!(d["currentVersionNo"].is_null(), "{d}");
    // Retired and not running anywhere: its fields are free again.
    let (status, v) = w.call("DELETE", &format!("/api/v1/attribute-definitions/{risk}"), None).await;
    assert_eq!(status, 204, "{v}");

    // Draft DELETE.
    let (status, _) = w.call("DELETE", &draft, None).await;
    assert_eq!(status, 204);
    let (status, _) = w.call("DELETE", &draft, None).await;
    assert_eq!(status, 404);

    // Delete: refused once an instance ran; allowed for a workflow that never ran.
    let v1: Uuid = sqlx::query_scalar("SELECT id FROM workflow_versions WHERE definition_id = $1 AND version_no = 1")
        .bind(def)
        .fetch_one(&w.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO workflow_instances (definition_id, version_id, ci_id, current_state_id, status, started_by_name)
         SELECT $1, $2, $3, s.id, 'active', 'test' FROM workflow_states s WHERE s.version_id = $2 AND s.key = 'planned'",
    )
    .bind(def)
    .bind(v1)
    .bind(cis[0])
    .execute(&w.pool)
    .await
    .unwrap();
    let (status, v) = w.call("DELETE", &by_id, None).await;
    assert_eq!((status, code(&v)), (409, "IN_USE"), "{v}");
    assert_eq!(details(&v), [("id".to_owned(), "has_instances".to_owned())]);
    let (status, v) =
        w.call("POST", BASE, Some(json!({ "key": "scratch", "name": "Scratch", "classId": w.server }))).await;
    assert_eq!(status, 201, "{v}");
    let scratch = id(&v);
    let mut g = lifecycle_graph();
    for s in g["states"].as_array_mut().unwrap() {
        s.as_object_mut().unwrap().remove("stateValue");
    }
    g["transitions"][0]["conditions"] = json!({ "field": "owner_team", "op": "isSet" });
    let (status, v) = w.call("PUT", &format!("{BASE}/{scratch}/draft"), Some(g)).await;
    assert_eq!(status, 200, "{v}");
    let sum = v["checksum"].clone();
    let (status, v) =
        w.call("POST", &format!("{BASE}/{scratch}/draft/publish"), Some(json!({ "expectedDraftChecksum": sum }))).await;
    assert_eq!(status, 201, "{v}");
    let (status, _) = w.call("DELETE", &format!("{BASE}/{scratch}"), None).await;
    assert_eq!(status, 204);
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_versions WHERE definition_id = $1")
        .bind(scratch)
        .fetch_one(&w.pool)
        .await
        .unwrap();
    assert_eq!(left, 0);

    // List with filters.
    let (status, v) = w.call("GET", &format!("{BASE}?classKey=server&active=true"), None).await;
    assert_eq!(status, 200, "{v}");
    let keys: Vec<&str> = v["data"].as_array().unwrap().iter().map(|d| d["key"].as_str().unwrap()).collect();
    assert_eq!(keys, ["server_lifecycle"]);
    let (status, v) = w.call("GET", &format!("{BASE}?q=nothing-like-this"), None).await;
    assert_eq!((status, v["page"]["total"].as_i64()), (200, Some(0)));

    // Audit: create, publish (with the graph), updates, retire, grants; delete of the scratch workflow.
    let rows = w.audit(def).await;
    let actions: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(actions, ["create", "workflow.publish", "update", "update", "update", "update"], "{rows:?}");
    let publish = rows[1].2.as_ref().unwrap();
    assert_eq!((publish["versionNo"].as_i64(), publish["changeNote"].as_str()), (Some(1), Some("First cut")));
    assert_eq!(publish["graph"]["states"], lifecycle_graph()["states"]);
    assert_eq!(publish["graph"]["transitions"], lifecycle_graph()["transitions"]);
    let grants_row = rows[4].2.as_ref().unwrap();
    assert_eq!(grants_row["grants"], json!({ "_cancel": ["Change managers"], "approve": ["Change managers"] }));
    let retire = rows[5].2.as_ref().unwrap();
    assert_eq!(retire, &json!({ "versionNo": 1, "status": "retired", "currentVersionNo": null }));
    let scratch_rows = w.audit(scratch).await;
    let actions: Vec<&str> = scratch_rows.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(actions, ["create", "workflow.publish", "delete"]);

    db.drop().await;
}

/// Every route needs `workflows.manage` (403 FORBIDDEN otherwise), and a
/// holder who may view the workflow's type, without other rights, can read it.
#[tokio::test]
async fn workflow_definitions_need_workflows_manage() {
    let Some(db) = scratch::database("workflow_definitions_need_manage").await else { return };
    let w = world(&db).await;
    let (status, d) = w.call("POST", BASE, Some(json!({ "key": "flow", "name": "Flow", "classId": w.server }))).await;
    assert_eq!(status, 201, "{d}");
    let by_id = format!("{BASE}/{}", id(&d));
    let modeller = w.user("modeller", &["datamodel.manage"]).await;
    // 0046 grants workflows.manage to data-model managers; this profile was made after it.
    let routes: Vec<(&str, String, Option<Value>)> = vec![
        ("GET", BASE.to_owned(), None),
        ("POST", BASE.to_owned(), Some(json!({ "key": "flow2", "name": "Flow", "classId": w.server }))),
        ("GET", by_id.clone(), None),
        ("PATCH", by_id.clone(), Some(json!({ "version": 1, "name": "X" }))),
        ("DELETE", by_id.clone(), None),
        ("GET", format!("{by_id}/versions"), None),
        ("GET", format!("{by_id}/versions/1"), None),
        ("POST", format!("{by_id}/versions/1/retire"), None),
        ("GET", format!("{by_id}/draft"), None),
        ("PUT", format!("{by_id}/draft"), Some(json!({ "states": [], "transitions": [] }))),
        ("DELETE", format!("{by_id}/draft"), None),
        ("POST", format!("{by_id}/draft/validate"), None),
        ("POST", format!("{by_id}/draft/publish"), Some(json!({ "expectedDraftChecksum": "0".repeat(64) }))),
        ("GET", format!("{by_id}/grants"), None),
        ("PUT", format!("{by_id}/grants"), Some(json!({ "version": 1, "grants": [] }))),
    ];
    for (method, path, body) in &routes {
        let (status, v, _) = call(&w.app, method, path, &modeller, body.clone()).await;
        assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{method} {path}: {v}");
    }
    let designer = w.scoped_user("designer", &["workflows.manage"], &[(w.server, false)]).await;
    let (status, v, _) = call(&w.app, "GET", BASE, &designer, None).await;
    assert_eq!((status, v["page"]["total"].as_i64()), (200, Some(1)), "{v}");
    let (status, v, _) = call(&w.app, "GET", &format!("{by_id}/draft"), &designer, None).await;
    assert_eq!(status, 200, "{v}");
    let (status, v, _) =
        call(&w.app, "GET", "/api/v1/admin/workflow-definitions/not-a-uuid/versions/0", &designer, None).await;
    assert_eq!(status, 400, "{v}");
    db.drop().await;
}

/// A signed-in first administrator on an empty install.
async fn admin_of(db: &scratch::Scratch) -> (Router, Creds) {
    let app = app(db.pool.clone());
    let password = format!("test passphrase {}", Uuid::new_v4());
    let setup = json!({ "username": "admin", "email": "admin@example.test", "displayName": "Admin",
        "password": password, "setupToken": crate::auth::setup_token::TEST_TOKEN });
    let (status, me, headers) = call(&app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
    assert_eq!(status, 201, "{me}");
    let admin = session_of(&me, &headers);
    (app, admin)
}

async fn export_config(app: &Router, creds: &Creds) -> Value {
    let (status, v, _) = call(app, "GET", "/api/v1/admin/config/export", creds, None).await;
    assert_eq!(status, 200, "{v}");
    v
}

async fn import_config(app: &Router, creds: &Creds, file: &Value) -> (u16, Value) {
    let (status, v, _) = call(app, "POST", "/api/v1/admin/config/import?mode=apply", creds, Some(file.clone())).await;
    (status, v)
}

async fn count(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned())).fetch_one(pool).await.unwrap()
}

/// SHAA-1425 (design §7): config format 8 carries each workflow's current
/// published version by key. Export → import into an empty install → export
/// gives the same `workflows` section; an unchanged file is a no-op; a
/// changed graph publishes a new version while running instances keep
/// theirs; a broken graph fails the whole import; a version 6 file still
/// imports.
#[tokio::test]
async fn workflows_round_trip_through_the_configuration_file() {
    let Some(src) = scratch::database("workflows_config_src").await else { return };
    let Some(dst) = scratch::database("workflows_config_dst").await else { return };
    let w = world(&src).await;

    // Source: an active workflow with v1 published, grants, and a later draft.
    sqlx::query("INSERT INTO permission_profiles (name) VALUES ('Change managers')").execute(&w.pool).await.unwrap();
    let body = json!({ "key": "server_lifecycle", "name": "Server lifecycle", "classId": w.server,
        "stateAttributeId": w.lifecycle, "isActive": true, "description": "Servers from order to retirement" });
    let def = id(&post(&w.app, &w.admin, BASE, body).await);
    let draft = format!("{BASE}/{def}/draft");
    let (status, v) = w.call("PUT", &draft, Some(lifecycle_graph())).await;
    assert_eq!(status, 200, "{v}");
    let v1_sum = v["checksum"].as_str().unwrap().to_owned();
    let (status, v) =
        w.call("POST", &format!("{draft}/publish"), Some(json!({ "expectedDraftChecksum": v1_sum }))).await;
    assert_eq!(status, 201, "{v}");
    let (status, g) = w.call("GET", &format!("{BASE}/{def}/grants"), None).await;
    assert_eq!(status, 200);
    let grants = json!({ "version": g["version"], "grants": [
        { "transitionKey": "approve", "profiles": ["Change managers"] },
        { "transitionKey": "_cancel", "profiles": ["Change managers"] } ] });
    let (status, v) = w.call("PUT", &format!("{BASE}/{def}/grants"), Some(grants)).await;
    assert_eq!(status, 200, "{v}");
    let mut unpublished = lifecycle_graph();
    unpublished["states"][0]["name"] = json!("Draft only");
    let (status, v) = w.call("PUT", &draft, Some(unpublished)).await;
    assert_eq!(status, 200, "{v}");
    // A workflow never published is not exported either.
    post(&w.app, &w.admin, BASE, json!({ "key": "never_published", "name": "Never", "classId": w.server })).await;

    let file = export_config(&w.app, &w.admin).await;
    assert_eq!(file["formatVersion"], 9);
    let flows = file["workflows"].as_array().unwrap();
    assert_eq!(flows.len(), 1, "{flows:?}");
    let flow = &flows[0];
    let expected = lifecycle_graph();
    assert_eq!(
        (&flow["key"], &flow["class"], &flow["stateAttribute"], &flow["isActive"], &flow["autoStart"]),
        (&json!("server_lifecycle"), &json!("server"), &json!("lifecycle"), &json!(true), &json!(false))
    );
    for k in ["initialState", "states", "transitions", "layout"] {
        assert_eq!(flow["graph"][k], expected[k], "{k}");
    }
    // GH#641: the cancel grant goes into the file as `_cancel`, and the import below takes it back.
    assert_eq!(
        flow["grants"],
        json!([ { "transition": "_cancel", "profiles": ["Change managers"] },
                { "transition": "approve", "profiles": ["Change managers"] } ])
    );
    let text = flow.to_string();
    assert!(!text.contains("Draft only") && !text.contains(&def.to_string()), "{text}");
    // The source's draft is still in the way of a changed graph; it stays out of the no-op below.
    let (status, _) = w.call("DELETE", &draft, None).await;
    assert_eq!(status, 204);

    // Into an empty install: the workflow is created and v1 published, by the importer.
    let (app2, admin2) = admin_of(&dst).await;
    let (status, res) = import_config(&app2, &admin2, &file).await;
    assert_eq!(status, 200, "{res}");
    let summary = res["summary"].as_array().unwrap().iter().find(|s| s["section"] == "workflows").unwrap().clone();
    assert_eq!((summary["created"].as_i64(), summary["updated"].as_i64()), (Some(1), Some(0)), "{summary}");
    let (note, by, sum): (Option<String>, Option<String>, Vec<u8>) = sqlx::query_as(
        "SELECT v.change_note, v.published_by_name, v.checksum FROM workflow_versions v
         JOIN workflow_definitions d ON d.id = v.definition_id
         WHERE d.key = 'server_lifecycle' AND v.status = 'published'",
    )
    .fetch_one(&dst.pool)
    .await
    .unwrap();
    assert_eq!((note.as_deref(), by.as_deref()), (Some("Imported from configuration file"), Some("admin")));
    assert_eq!(hex::encode(sum), v1_sum, "the same graph has the same checksum on both installs");
    let def_dst: Uuid = sqlx::query_scalar("SELECT id FROM workflow_definitions WHERE key = 'server_lifecycle'")
        .fetch_one(&dst.pool)
        .await
        .unwrap();
    let (status, g, _) = call(&app2, "GET", &format!("{BASE}/{def_dst}/grants"), &admin2, None).await;
    assert_eq!(status, 200, "{g}");
    let keys: Vec<&str> =
        g["grants"].as_array().unwrap().iter().map(|g| g["transitionKey"].as_str().unwrap()).collect();
    assert!(keys.contains(&"_cancel") && keys.contains(&"approve"), "{g}");
    let back = export_config(&app2, &admin2).await;
    assert_eq!(back["workflows"], file["workflows"], "round trip");
    let actions: Vec<String> =
        sqlx::query_scalar("SELECT action FROM audit_log WHERE entity_type = 'workflow_definitions' ORDER BY id")
            .fetch_all(&dst.pool)
            .await
            .unwrap();
    assert_eq!(actions, ["create", "workflow.publish", "update"], "audited like a manual change");

    // Re-importing the unchanged file changes nothing, on either install.
    for (pool, app, creds) in [(&dst.pool, &app2, &admin2), (&w.pool, &w.app, &w.admin)] {
        let audit = "SELECT count(*) FROM audit_log WHERE entity_type = 'workflow_definitions'";
        let versions = "SELECT count(*) FROM workflow_versions";
        let (a, n) = (count(pool, audit).await, count(pool, versions).await);
        let (status, res, _) =
            call(app, "POST", "/api/v1/admin/config/import?mode=dry_run", creds, Some(file.clone())).await;
        assert_eq!(status, 200, "{res}");
        let (status, res) = import_config(app, creds, &file).await;
        assert_eq!(status, 200, "{res}");
        assert_eq!((count(pool, audit).await, count(pool, versions).await), (a, n));
        let changes: Vec<&Value> =
            res["changes"].as_array().unwrap().iter().filter(|c| c["section"] == "workflows").collect();
        assert!(changes.is_empty(), "{changes:?}");
    }

    // A changed graph publishes v2; the instance running on v1 stays on v1.
    let server2: Uuid =
        sqlx::query_scalar("SELECT id FROM ci_classes WHERE key = 'server'").fetch_one(&dst.pool).await.unwrap();
    let (status, ci, _) =
        call(&app2, "POST", "/api/v1/configuration-items", &admin2, Some(json!({ "classId": server2 }))).await;
    assert_eq!(status, 201, "{ci}");
    let (def2, v1): (Uuid, Uuid) = sqlx::query_as(
        "SELECT d.id, d.current_version_id FROM workflow_definitions d WHERE d.key = 'server_lifecycle'",
    )
    .fetch_one(&dst.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workflow_instances (definition_id, version_id, ci_id, current_state_id, status, started_by_name)
         SELECT $1, $2, $3, s.id, 'active', 'test' FROM workflow_states s WHERE s.version_id = $2 AND s.key = 'planned'",
    )
    .bind(def2)
    .bind(v1)
    .bind(id(&ci))
    .execute(&dst.pool)
    .await
    .unwrap();
    // GH#587: a grant for a key in no version (here or in the file) fails the import.
    let mut typo = file.clone();
    typo["workflows"][0]["grants"] = json!([
        { "transition": "_cancel", "profiles": ["Change managers"] },
        { "transition": "approve", "profiles": ["Change managers"] },
        { "transition": "go_lve", "profiles": ["Change managers"] } ]);
    let (status, v) = import_config(&app2, &admin2, &typo).await;
    assert_eq!(
        (status, details(&v)),
        (400, vec![("workflows.0.grants.2.transition".to_owned(), "unknown_transition".to_owned())])
    );
    let mut changed = file.clone();
    changed["workflows"][0]["graph"]["states"][1]["name"] = json!("Approved by CAB");
    changed["workflows"][0]["grants"] = json!([]);
    changed["workflows"][0]["autoStart"] = json!(true);
    let (status, res) = import_config(&app2, &admin2, &changed).await;
    assert_eq!(status, 200, "{res}");
    let change = res["changes"].as_array().unwrap().iter().find(|c| c["section"] == "workflows").unwrap().clone();
    let fields: Vec<&str> = change["fields"].as_array().unwrap().iter().map(|f| f["field"].as_str().unwrap()).collect();
    assert_eq!((change["action"].as_str(), fields), (Some("update"), vec!["autoStart", "graph", "grants"]), "{change}");
    assert_eq!(change["fields"][1]["to"]["versionNo"], 2);
    let (status, d, _) = call(&app2, "GET", &format!("{BASE}/{def2}"), &admin2, None).await;
    assert_eq!((status, d["currentVersionNo"].as_i64(), d["autoStart"].as_bool()), (200, Some(2), Some(true)), "{d}");
    let (status, v, _) = call(&app2, "GET", &format!("{BASE}/{def2}/versions/1"), &admin2, None).await;
    assert_eq!(
        (status, v["status"].as_str(), v["states"][1]["name"].as_str()),
        (200, Some("published"), Some("Approved"))
    );
    let running: Uuid = sqlx::query_scalar("SELECT version_id FROM workflow_instances WHERE definition_id = $1")
        .bind(def2)
        .fetch_one(&dst.pool)
        .await
        .unwrap();
    assert_eq!(running, v1);
    let (status, g, _) = call(&app2, "GET", &format!("{BASE}/{def2}/grants"), &admin2, None).await;
    assert_eq!((status, &g["grants"]), (200, &json!([])), "grants are replaced");

    // A graph the publish lint refuses fails the whole import, with paths into the file.
    let mut broken = changed.clone();
    broken["workflows"][0]["graph"]["states"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "key": "limbo", "name": "Limbo", "category": "active" }));
    broken["workflows"][0]["name"] = json!("Renamed");
    let versions = count(&dst.pool, "SELECT count(*) FROM workflow_versions").await;
    let (status, v) = import_config(&app2, &admin2, &broken).await;
    assert_eq!((status, code(&v)), (400, "VALIDATION_ERROR"), "{v}");
    assert_eq!(
        details(&v),
        [("workflows.0.graph.states[3]", "unreachable_state"), ("workflows.0.graph.states[3]", "dead_end")]
            .map(|(a, b)| (a.to_owned(), b.to_owned()))
    );
    assert_eq!(count(&dst.pool, "SELECT count(*) FROM workflow_versions").await, versions);
    let name: String = sqlx::query_scalar("SELECT name FROM workflow_definitions WHERE id = $1")
        .bind(def2)
        .fetch_one(&dst.pool)
        .await
        .unwrap();
    assert_eq!(name, "Server lifecycle");

    // The section needs workflows.manage, on export and on import.
    let modeller = w
        .user("modeller", &["config.export_import", "datamodel.manage", "profiles.manage", "customization.manage"])
        .await;
    let partial = export_config(&w.app, &modeller).await;
    assert!(partial.get("workflows").is_none(), "{partial}");
    let (status, v, _) =
        call(&w.app, "POST", "/api/v1/admin/config/import?mode=dry_run", &modeller, Some(file.clone())).await;
    assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{v}");

    // A version 6 file (no workflows, no layout templates) still imports.
    let mut v6 = file.clone();
    let o = v6.as_object_mut().unwrap();
    o.insert("formatVersion".into(), json!(6));
    o.remove("workflows");
    o.remove("uiSettings");
    let (status, res) = import_config(&app2, &admin2, &v6).await;
    assert_eq!((status, res["applied"].as_bool()), (200, Some(true)), "{res}");

    src.drop().await;
    dst.drop().await;
}

/// GH#573: a small invalid body cannot answer with tens of thousands of
/// details. Both bodies are the reproductions of the issue; each answers 100
/// details plus one `truncated` entry counting the rest.
#[tokio::test]
async fn validation_details_are_capped() {
    use crate::http::error::MAX_DETAILS;

    let Some(db) = scratch::database("workflow_validation_details_capped").await else { return };
    let w = world(&db).await;
    let (status, d) = w.call("POST", BASE, Some(json!({ "key": "flow", "name": "Flow", "classId": w.server }))).await;
    assert_eq!(status, 201, "{d}");
    let by_id = format!("{BASE}/{}", id(&d));
    let capped = |v: &Value| {
        let found = details(v);
        assert_eq!(found.len(), MAX_DETAILS + 1, "{found:?}");
        assert_eq!(found[MAX_DETAILS], (String::new(), "truncated".to_owned()));
        assert!(serde_json::to_vec(v).unwrap().len() < 32 * 1024, "{v}");
        v["error"]["details"][MAX_DETAILS]["message"].as_str().unwrap().to_owned()
    };

    // 301 grants with unknown keys × 100 unknown profiles: 30,401 problems.
    let grants: Vec<Value> =
        (0..301).map(|i| json!({ "transitionKey": format!("t{i}"), "profiles": vec!["a"; 100] })).collect();
    let (status, v) = w.call("PUT", &format!("{by_id}/grants"), Some(json!({ "version": 1, "grants": grants }))).await;
    assert_eq!((status, code(&v)), (400, "VALIDATION_ERROR"), "{v}");
    assert_eq!(details(&v)[0], ("grants[0].transitionKey".to_owned(), "unknown_transition".to_owned()));
    assert_eq!(capped(&v), "30301 more problems not shown");

    // 300 transitions with a condition of 200 leaves naming an unknown field each.
    let states = json!([
        { "key": "a", "name": "A", "category": "open", "terminal": false },
        { "key": "b", "name": "B", "category": "done", "terminal": true }
    ]);
    let transitions: Vec<Value> = (0..300)
        .map(|i| {
            json!({ "key": format!("t{i}"), "name": "T", "from": "a", "to": "b", "requiresComment": false,
                "fields": [], "conditions": { "any": vec![json!({ "field": "x" }); 200] } })
        })
        .collect();
    let body = json!({ "initialState": "a", "states": states, "transitions": transitions });
    let (status, v) = w.call("PUT", &format!("{by_id}/draft"), Some(body)).await;
    assert_eq!((status, code(&v)), (400, "VALIDATION_ERROR"), "{v}");
    assert!(capped(&v).ends_with(" more problems not shown"));

    // At the cap nothing is truncated.
    let grants = json!([{ "transitionKey": "_cancel", "profiles": vec!["a"; 100] }]);
    let (status, v) = w.call("PUT", &format!("{by_id}/grants"), Some(json!({ "version": 1, "grants": grants }))).await;
    assert_eq!(status, 400, "{v}");
    let found = details(&v);
    assert_eq!(found.len(), MAX_DETAILS);
    assert!(found.iter().all(|(_, c)| c == "not_found"), "{found:?}");
    db.drop().await;
}

/// GH#617: a grant on a transition only the draft has goes when the draft
/// saves without it or is deleted, audited, so the stored grants re-save
/// unchanged and the instance's own export re-imports.
#[tokio::test]
async fn grants_on_draft_only_transitions_go_with_the_draft() {
    let Some(db) = scratch::database("workflow_draft_only_grants").await else { return };
    let w = world(&db).await;
    let body = json!({ "key": "server_lifecycle", "name": "Server lifecycle", "classId": w.server,
        "stateAttributeId": w.lifecycle, "isActive": true });
    let def = id(&post(&w.app, &w.admin, BASE, body).await);
    let draft = format!("{BASE}/{def}/draft");
    let grants = format!("{BASE}/{def}/grants");
    let (status, v) = w.call("PUT", &draft, Some(lifecycle_graph())).await;
    assert_eq!(status, 200, "{v}");
    let sum = v["checksum"].clone();
    let (status, v) = w.call("POST", &format!("{draft}/publish"), Some(json!({ "expectedDraftChecksum": sum }))).await;
    assert_eq!(status, 201, "{v}");

    let mut fast = lifecycle_graph();
    fast["transitions"].as_array_mut().unwrap().push(json!({ "key": "fast_track", "name": "Fast track",
        "from": "planned", "to": "done", "requiresComment": false, "fields": [] }));
    let grant_fast = |version: &Value| {
        json!({ "version": version, "grants": [
            { "transitionKey": "approve", "profiles": ["Administrator"] },
            { "transitionKey": "fast_track", "profiles": ["Administrator"] }
        ] })
    };
    let keys = |v: &Value| -> Vec<String> {
        v["grants"].as_array().unwrap().iter().map(|g| g["transitionKey"].as_str().unwrap().to_owned()).collect()
    };

    // Renamed in the draft: the grant on the old key goes.
    let (status, v) = w.call("PUT", &draft, Some(fast.clone())).await;
    assert_eq!(status, 200, "{v}");
    let (_, g) = w.call("GET", &grants, None).await;
    let (status, v) = w.call("PUT", &grants, Some(grant_fast(&g["version"]))).await;
    assert_eq!((status, keys(&v)), (200, vec!["approve".to_owned(), "fast_track".to_owned()]), "{v}");
    let mut renamed = fast.clone();
    renamed["transitions"][2]["key"] = json!("express");
    let (status, v) = w.call("PUT", &draft, Some(renamed)).await;
    assert_eq!(status, 200, "{v}");
    let (_, g) = w.call("GET", &grants, None).await;
    assert_eq!(keys(&g), vec!["approve".to_owned()]);

    // Deleted with the draft (the GH#617 repro).
    let (status, v) = w.call("PUT", &draft, Some(fast)).await;
    assert_eq!(status, 200, "{v}");
    let (status, v) = w.call("PUT", &grants, Some(grant_fast(&g["version"]))).await;
    assert_eq!(status, 200, "{v}");
    let (status, _) = w.call("DELETE", &draft, None).await;
    assert_eq!(status, 204);
    let (_, g) = w.call("GET", &grants, None).await;
    assert_eq!(keys(&g), vec!["approve".to_owned()]);
    let audit = w.audit(def).await;
    let (action, old, new) = audit.last().unwrap();
    assert_eq!(action, "update");
    assert_eq!(
        old.as_ref().unwrap()["grants"],
        json!({ "approve": ["Administrator"], "fast_track": ["Administrator"] })
    );
    assert_eq!(new.as_ref().unwrap()["grants"], json!({ "approve": ["Administrator"] }));
    assert_eq!(new.as_ref().unwrap()["version"], g["version"]);

    // The stored set re-saves unchanged, and the export re-imports.
    let same =
        json!({ "version": g["version"], "grants": [{ "transitionKey": "approve", "profiles": ["Administrator"] }] });
    let (status, v) = w.call("PUT", &grants, Some(same)).await;
    assert_eq!(status, 200, "{v}");
    let file = export_config(&w.app, &w.admin).await;
    assert_eq!(file["workflows"][0]["grants"], json!([{ "transition": "approve", "profiles": ["Administrator"] }]));
    for mode in ["dry_run", "apply"] {
        let (status, v, _) =
            call(&w.app, "POST", &format!("/api/v1/admin/config/import?mode={mode}"), &w.admin, Some(file.clone()))
                .await;
        assert_eq!(status, 200, "{mode}: {v}");
    }
    db.drop().await;
}

/// GH#667: `workflows.manage` reaches only the workflows on types the manager
/// may view, every one of them, to read them, and edit as well, to change
/// them. One on a type they may not view answers 404 on every route, as at run
/// time, and is left out of the list, the export and the import.
#[tokio::test]
async fn workflow_definitions_follow_the_managers_class_scope() {
    let Some(db) = scratch::database("workflow_definitions_class_scope").await else { return };
    let w = world(&db).await;
    let network =
        id(&post(&w.app, &w.admin, "/api/v1/ci-classes", json!({ "key": "network", "name": "Network" })).await);
    let body = json!({ "key": "server_lifecycle", "name": "Server lifecycle", "classId": w.server,
        "stateAttributeId": w.lifecycle });
    let server_flow = id(&post(&w.app, &w.admin, BASE, body).await);
    let draft = format!("{BASE}/{server_flow}/draft");
    let (status, v) = w.call("PUT", &draft, Some(lifecycle_graph())).await;
    assert_eq!(status, 200, "{v}");
    let sum = v["checksum"].as_str().unwrap().to_owned();
    let (status, v) = w.call("POST", &format!("{draft}/publish"), Some(json!({ "expectedDraftChecksum": sum }))).await;
    assert_eq!(status, 201, "{v}");
    let net_flow =
        id(&post(&w.app, &w.admin, BASE, json!({ "key": "net_review", "name": "Network review", "classId": network }))
            .await);

    let globals = ["workflows.manage", "config.export_import"];
    let net_manager = w.scoped_user("net_manager", &globals, &[(network, true)]).await;
    let by_id = format!("{BASE}/{server_flow}");

    // The list holds only the network workflow.
    let (status, v, _) = call(&w.app, "GET", BASE, &net_manager, None).await;
    assert_eq!((status, v["page"]["total"].as_i64()), (200, Some(1)), "{v}");
    assert_eq!(v["data"][0]["key"], "net_review");

    // Every route on the server workflow answers as for a missing one.
    let routes: Vec<(&str, String, Option<Value>)> = vec![
        ("GET", by_id.clone(), None),
        ("PATCH", by_id.clone(), Some(json!({ "version": 2, "isActive": false }))),
        ("DELETE", by_id.clone(), None),
        ("GET", format!("{by_id}/versions"), None),
        ("GET", format!("{by_id}/versions/1"), None),
        ("POST", format!("{by_id}/versions/1/retire"), None),
        ("GET", format!("{by_id}/draft"), None),
        ("PUT", format!("{by_id}/draft"), Some(lifecycle_graph())),
        ("DELETE", format!("{by_id}/draft"), None),
        ("POST", format!("{by_id}/draft/validate"), None),
        ("POST", format!("{by_id}/draft/publish"), Some(json!({ "expectedDraftChecksum": "0".repeat(64) }))),
        ("GET", format!("{by_id}/grants"), None),
        ("PUT", format!("{by_id}/grants"), Some(json!({ "version": 2, "grants": [] }))),
        ("GET", format!("{by_id}/approvers"), None),
        ("PUT", format!("{by_id}/approvers"), Some(json!({ "version": 2, "approvers": [] }))),
        ("GET", format!("{by_id}/approvers/preview?transition=approve&step=cab"), None),
        // GH#695: not 409 `inactive` / `unpublished` naming the workflow.
        ("POST", format!("{by_id}/bootstrap"), Some(json!({ "stateFromAttribute": true }))),
        ("POST", format!("{by_id}/bootstrap"), Some(json!({ "stateFromAttribute": true, "dryRun": true }))),
        (
            "POST",
            format!("{by_id}/instance-migrations"),
            Some(json!({ "fromVersionNo": 1, "toVersionNo": 2, "dryRun": false })),
        ),
    ];
    let (_, hidden, _) = call(&w.app, "GET", &by_id, &net_manager, None).await;
    for (method, path, body) in &routes {
        let (status, v, _) = call(&w.app, method, path, &net_manager, body.clone()).await;
        assert_eq!((status, code(&v)), (404, "NOT_FOUND"), "{method} {path}: {v}");
        assert!(!v.to_string().contains("server_lifecycle"), "{method} {path} names the workflow: {v}");
        assert_eq!(v["error"]["message"], hidden["error"]["message"], "{method} {path}: as the read route");
    }
    let (_, v) = w.call("GET", &by_id, None).await;
    assert_eq!((v["isActive"].as_bool(), v["version"].as_i64()), (Some(false), Some(2)), "unchanged: {v}");

    // Creating one needs view and edit on the type.
    let flow = |key: &str, class: Uuid| json!({ "key": key, "name": key, "classId": class });
    let (status, v, _) = call(&w.app, "POST", BASE, &net_manager, Some(flow("server_two", w.server))).await;
    assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{v}");
    let (status, v, _) = call(&w.app, "POST", BASE, &net_manager, Some(flow("net_two", network))).await;
    assert_eq!(status, 201, "{v}");
    let (status, v, _) = call(&w.app, "GET", &format!("{BASE}/{net_flow}/grants"), &net_manager, None).await;
    assert_eq!(status, 200, "{v}");

    // View without edit: it reads the workflow, and changes nothing.
    let server_viewer = w.scoped_user("server_viewer", &["workflows.manage"], &[(w.server, false)]).await;
    let (status, v, _) = call(&w.app, "GET", &by_id, &server_viewer, None).await;
    assert_eq!(status, 200, "{v}");
    let patch = json!({ "version": 2, "isActive": true });
    let (status, v, _) = call(&w.app, "PATCH", &by_id, &server_viewer, Some(patch)).await;
    assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{v}");

    // A subtype it may not view hides a workflow that includes subtypes.
    post(&w.app, &w.admin, "/api/v1/ci-classes", json!({ "key": "router", "name": "Router", "parentId": network }))
        .await;
    let (status, v, _) = call(&w.app, "GET", &format!("{BASE}/{net_flow}"), &net_manager, None).await;
    assert_eq!((status, code(&v)), (404, "NOT_FOUND"), "{v}");

    // The configuration file: the export leaves the server workflow out, and
    // an import that changes its grants skips it with a warning.
    sqlx::query("INSERT INTO permission_profiles (name) VALUES ('Network approvers')").execute(&w.pool).await.unwrap();
    let mine = export_config(&w.app, &net_manager).await;
    assert_eq!(mine["workflows"], json!([]), "{mine}");
    let full = export_config(&w.app, &w.admin).await;
    let mut file = json!({ "format": full["format"], "formatVersion": full["formatVersion"],
        "workflows": full["workflows"] });
    assert_eq!(file["workflows"][0]["key"], "server_lifecycle");
    file["workflows"][0]["grants"] = json!([{ "transition": "approve", "profiles": ["Network approvers"] }]);
    for mode in ["dry_run", "apply"] {
        let path = format!("/api/v1/admin/config/import?mode={mode}");
        let (status, res, _) = call(&w.app, "POST", &path, &net_manager, Some(file.clone())).await;
        assert_eq!(status, 200, "{res}");
        assert_eq!(res["changes"], json!([]), "{res}");
        assert_eq!(res["warnings"][0]["path"], "workflows.0", "{res}");
        let section = res["summary"].as_array().unwrap().iter().find(|s| s["section"] == "workflows").unwrap();
        assert_eq!(section["notInFile"], 0, "the hidden workflows are not counted: {res}");
    }
    let grants = count(&w.pool, "SELECT count(*) FROM workflow_transition_grants").await;
    assert_eq!(grants, 0);
    db.drop().await;
}

/// GH#667 (SHAA-2176): the types a workflow covers include the subtypes when
/// `includeSubclasses` is on, which is the default. A manager with view and
/// edit on a type, but not on a subtype below it, can neither create a
/// workflow that includes subtypes nor turn subtypes on for one they manage.
#[tokio::test]
async fn including_subtypes_needs_the_rights_on_every_subtype() {
    let Some(db) = scratch::database("workflow_definitions_subtype_scope").await else { return };
    let w = world(&db).await;
    post(&w.app, &w.admin, "/api/v1/ci-classes", json!({ "key": "blade", "name": "Blade", "parentId": w.server }))
        .await;
    let editor = w.scoped_user("server_editor", &["workflows.manage"], &[(w.server, true)]).await;

    // The default includes the subtypes, so the hidden Blade type is refused.
    let body = json!({ "key": "server_default", "name": "Server default", "classId": w.server });
    let (status, v, _) = call(&w.app, "POST", BASE, &editor, Some(body)).await;
    assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{v}");
    let body =
        json!({ "key": "server_alone", "name": "Server alone", "classId": w.server, "includeSubclasses": false });
    let (status, v, _) = call(&w.app, "POST", BASE, &editor, Some(body)).await;
    assert_eq!(status, 201, "{v}");
    let flow = id(&v);
    let by_id = format!("{BASE}/{flow}");
    let (status, v, _) = call(&w.app, "GET", BASE, &editor, None).await;
    assert_eq!((status, v["page"]["total"].as_i64()), (200, Some(1)), "{v}");

    // Turning the subtypes on would cover Blade: refused, and nothing changes.
    // GH#686: 403, not 404, since the editor may read this workflow.
    let (status, v, _) =
        call(&w.app, "PATCH", &by_id, &editor, Some(json!({ "version": 1, "includeSubclasses": true }))).await;
    assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{v}");
    let (_, v) = w.call("GET", &by_id, None).await;
    assert_eq!((v["includeSubclasses"].as_bool(), v["version"].as_i64()), (Some(false), Some(1)), "{v}");

    // With the rights on Blade as well, both are allowed.
    let blade: Uuid =
        sqlx::query_scalar("SELECT id FROM ci_classes WHERE key = 'blade'").fetch_one(&w.pool).await.unwrap();
    let manager = w.scoped_user("fleet_manager", &["workflows.manage"], &[(w.server, true), (blade, true)]).await;
    let (status, v, _) =
        call(&w.app, "PATCH", &by_id, &manager, Some(json!({ "version": 1, "includeSubclasses": true }))).await;
    assert_eq!((status, v["includeSubclasses"].as_bool()), (200, Some(true)), "{v}");
    let body = json!({ "key": "server_default", "name": "Server default", "classId": w.server });
    let (status, v, _) = call(&w.app, "POST", BASE, &manager, Some(body)).await;
    assert_eq!(status, 201, "{v}");
    // The editor no longer sees the workflow that now covers Blade.
    let (status, v, _) = call(&w.app, "GET", &by_id, &editor, None).await;
    assert_eq!((status, code(&v)), (404, "NOT_FOUND"), "{v}");
    db.drop().await;
}

/// GH#668 (SHAA-2176): the configuration import lints a workflow's graph as
/// publishing does, so a file whose transition takes the workflow's own state
/// field is refused with `state_field` and changes nothing.
#[tokio::test]
async fn the_import_refuses_a_state_field_as_a_transition_field() {
    let Some(db) = scratch::database("workflow_import_state_field").await else { return };
    let w = world(&db).await;
    let body = json!({ "key": "server_lifecycle", "name": "Server lifecycle", "classId": w.server,
        "stateAttributeId": w.lifecycle });
    let flow = id(&post(&w.app, &w.admin, BASE, body).await);
    let (status, v) = w.call("PUT", &format!("{BASE}/{flow}/draft"), Some(lifecycle_graph())).await;
    assert_eq!(status, 200, "{v}");
    let sum = v["checksum"].as_str().unwrap().to_owned();
    let (status, v) =
        w.call("POST", &format!("{BASE}/{flow}/draft/publish"), Some(json!({ "expectedDraftChecksum": sum }))).await;
    assert_eq!(status, 201, "{v}");

    let full = export_config(&w.app, &w.admin).await;
    let mut file = json!({ "format": full["format"], "formatVersion": full["formatVersion"],
        "workflows": full["workflows"] });
    assert_eq!(file["workflows"][0]["graph"]["transitions"][0]["key"], "approve", "{file}");
    file["workflows"][0]["graph"]["transitions"][0]["fields"] = json!([{ "attribute": "lifecycle", "required": true }]);
    for mode in ["dry_run", "apply"] {
        let path = format!("/api/v1/admin/config/import?mode={mode}");
        let (status, v, _) = call(&w.app, "POST", &path, &w.admin, Some(file.clone())).await;
        assert_eq!(status, 400, "{mode}: {v}");
        assert!(details(&v).iter().any(|(_, c)| c == "state_field"), "{mode}: {v}");
    }
    let versions = count(&w.pool, "SELECT count(*) FROM workflow_versions WHERE status = 'published'").await;
    assert_eq!(versions, 1, "no version was published by the refused import");
    db.drop().await;
}
