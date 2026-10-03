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

    /// A signed-in user holding a profile with these global rights (and view on every type).
    async fn user(&self, name: &str, globals: &[&str]) -> Creds {
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
/// holder without other rights can use them.
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
    let designer = w.user("designer", &["workflows.manage"]).await;
    let (status, v, _) = call(&w.app, "GET", BASE, &designer, None).await;
    assert_eq!((status, v["page"]["total"].as_i64()), (200, Some(1)), "{v}");
    let (status, v, _) = call(&w.app, "GET", &format!("{by_id}/draft"), &designer, None).await;
    assert_eq!(status, 200, "{v}");
    let (status, v, _) =
        call(&w.app, "GET", "/api/v1/admin/workflow-definitions/not-a-uuid/versions/0", &designer, None).await;
    assert_eq!(status, 400, "{v}");
    db.drop().await;
}
