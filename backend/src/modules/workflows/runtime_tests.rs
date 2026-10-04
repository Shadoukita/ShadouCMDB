//! The runtime API through the real router against PostgreSQL (SHAA-1424):
//! every endpoint and error code of §6.3, restricted readers, grants and the
//! token's profile (Q6), the CI delete cascade, the CI → instance lock order
//! under concurrency, and the audit chain after a run.

use axum::Router;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::db::scratch;
use crate::modules::api_tokens::tests::{Creds, app, call, code, session_of};

const DEFS: &str = "/api/v1/admin/workflow-definitions";
const RUN: &str = "/api/v1/workflow-instances";

pub(super) struct World {
    pub(super) app: Router,
    pub(super) pool: PgPool,
    pub(super) admin: Creds,
    pub(super) password: String,
    pub(super) server: Uuid,
    pub(super) network: Uuid,
    /// Value ids of list `server_state` by key.
    pub(super) values: Vec<(String, Uuid)>,
    pub(super) definition: Uuid,
    /// Profile granted approve, go_live and _cancel, with view and edit on servers.
    pub(super) approvers: Uuid,
    /// View and edit on servers, no grant.
    pub(super) editors: Uuid,
}

fn id(v: &Value) -> Uuid {
    v["id"].as_str().unwrap_or_else(|| panic!("no id in {v}")).parse().unwrap()
}

fn details(v: &Value) -> Vec<(String, String)> {
    let mut d: Vec<(String, String)> = v["error"]["details"]
        .as_array()
        .map(|d| {
            d.iter()
                .map(|e| (e["field"].as_str().unwrap_or("").to_owned(), e["code"].as_str().unwrap_or("").to_owned()))
                .collect()
        })
        .unwrap_or_default();
    d.sort();
    d
}

fn pairs(p: &[(&str, &str)]) -> Vec<(String, String)> {
    p.iter().map(|(a, b)| ((*a).to_owned(), (*b).to_owned())).collect()
}

impl World {
    pub(super) async fn call(&self, creds: &Creds, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let (status, v, _) = call(&self.app, method, path, creds, body).await;
        (status, v)
    }

    pub(super) async fn ok(&self, method: &str, path: &str, body: Value) -> Value {
        let (status, v) = self.call(&self.admin, method, path, Some(body)).await;
        assert!(status == 200 || status == 201, "{method} {path}: {status} {v}");
        v
    }

    pub(super) fn value(&self, key: &str) -> Uuid {
        self.values.iter().find(|(k, _)| k == key).unwrap().1
    }

    async fn profile(&self, name: &str, classes: &[(Uuid, bool)]) -> Uuid {
        let profile: Uuid = sqlx::query_scalar("INSERT INTO permission_profiles (name) VALUES ($1) RETURNING id")
            .bind(name)
            .fetch_one(&self.pool)
            .await
            .unwrap();
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
        profile
    }

    /// A signed-in user holding these profiles; returns the session and the user's id.
    async fn user(&self, name: &str, profiles: &[Uuid]) -> (Creds, Uuid) {
        let body = json!({ "username": name, "email": format!("{name}@example.test"), "displayName": name,
            "password": self.password, "profileIds": profiles });
        let v = self.ok("POST", "/api/v1/admin/users", body).await;
        let user = id(&v);
        let body = json!({ "username": name, "password": self.password });
        let (status, me, headers) = call(&self.app, "POST", "/api/v1/auth/login", &Creds::default(), Some(body)).await;
        assert_eq!(status, 200, "{me}");
        (session_of(&me, &headers), user)
    }

    /// An API token of `owner` narrowed to `profile`, minted by the administrator.
    async fn token(&self, owner: Uuid, profile: Uuid) -> Creds {
        let expires = (chrono::Utc::now() + chrono::Duration::days(30)).to_rfc3339();
        let body = json!({ "name": format!("t-{}", Uuid::new_v4().simple()), "userId": owner, "profileId": profile,
            "expiresAt": expires });
        let v = self.ok("POST", "/api/v1/admin/api-tokens", body).await;
        Creds { bearer: Some(v["secret"].as_str().unwrap().to_owned()), ..Default::default() }
    }

    pub(super) async fn ci(&self, class: Uuid) -> Uuid {
        let attributes = if class == self.server { json!({ "environment": "test" }) } else { json!({}) };
        id(&self.ok("POST", "/api/v1/configuration-items", json!({ "classId": class, "attributes": attributes })).await)
    }

    async fn ci_values(&self, ci: Uuid) -> Value {
        let (status, v) = self.call(&self.admin, "GET", &format!("/api/v1/configuration-items/{ci}"), None).await;
        assert_eq!(status, 200, "{v}");
        v
    }

    pub(super) async fn start(&self, creds: &Creds, ci: Uuid) -> (u16, Value) {
        self.call(creds, "POST", RUN, Some(json!({ "definitionKey": "server_lifecycle", "ciId": ci }))).await
    }

    pub(super) async fn transition(&self, creds: &Creds, instance: Uuid, body: Value) -> (u16, Value) {
        self.call(creds, "POST", &format!("{RUN}/{instance}/transitions"), Some(body)).await
    }

    async fn audit_rows(&self, ci: Uuid) -> Vec<(String, Option<String>)> {
        sqlx::query_as(
            "SELECT action, request_id FROM audit_log WHERE entity_type = 'configuration_items' AND entity_id = $1
             ORDER BY id",
        )
        .bind(ci)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }
}

pub(super) async fn world(db: &scratch::Scratch) -> World {
    let app = app(db.pool.clone());
    // Random per test run, so no hard-coded credential reaches the hasher or verifier.
    let password = format!("test passphrase {}", Uuid::new_v4());
    let setup = json!({ "username": "admin", "email": "admin@example.test", "displayName": "Admin",
        "password": password, "setupToken": crate::auth::setup_token::TEST_TOKEN });
    let (status, me, headers) = call(&app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
    assert_eq!(status, 201, "{me}");
    let admin = session_of(&me, &headers);
    let mut w = World {
        app,
        pool: db.pool.clone(),
        admin,
        password,
        server: Uuid::nil(),
        network: Uuid::nil(),
        values: Vec::new(),
        definition: Uuid::nil(),
        approvers: Uuid::nil(),
        editors: Uuid::nil(),
    };
    w.server = id(&w.ok("POST", "/api/v1/ci-classes", json!({ "key": "server", "name": "Server" })).await);
    w.network = id(&w.ok("POST", "/api/v1/ci-classes", json!({ "key": "network", "name": "Network" })).await);
    let list =
        id(&w.ok("POST", "/api/v1/lookup-lists", json!({ "key": "server_state", "name": "Server state" })).await);
    for key in ["planned", "approved", "live"] {
        let v = w.ok("POST", "/api/v1/lookup-list-values", json!({ "listId": list, "key": key, "name": key })).await;
        w.values.push((key.to_owned(), id(&v)));
    }
    let attr =
        |key: &str, data_type: &str| json!({ "classId": w.server, "key": key, "label": key, "dataType": data_type });
    let mut lifecycle = attr("lifecycle", "lookup");
    lifecycle["lookupListId"] = json!(list);
    let lifecycle = id(&w.ok("POST", "/api/v1/attribute-definitions", lifecycle).await);
    w.ok("POST", "/api/v1/attribute-definitions", attr("owner_team", "text")).await;
    w.ok("POST", "/api/v1/attribute-definitions", attr("risk", "integer")).await;
    let mut env = attr("environment", "enum");
    env["enumValues"] = json!(["prod", "test"]);
    w.ok("POST", "/api/v1/attribute-definitions", env).await;

    // The design example: approve needs a comment, owner_team, and environment prod with an owner or a low risk.
    let d = w
        .ok(
            "POST",
            DEFS,
            json!({ "key": "server_lifecycle", "name": "Server lifecycle", "classId": w.server,
                "stateAttributeId": lifecycle }),
        )
        .await;
    w.definition = id(&d);
    let graph = json!({
        "initialState": "planned",
        "states": [
            { "key": "planned", "name": "Planned", "category": "open", "stateValue": "planned" },
            { "key": "approved", "name": "Approved", "category": "active", "stateValue": "approved" },
            { "key": "done", "name": "In production", "category": "done", "terminal": true, "stateValue": "live" }
        ],
        "transitions": [
            { "key": "approve", "name": "Approve", "from": "planned", "to": "approved", "requiresComment": true,
              "fields": [ { "attribute": "owner_team", "required": true }, { "attribute": "risk", "required": false } ],
              "conditions": { "all": [
                  { "field": "environment", "op": "in", "value": ["prod"] },
                  { "any": [ { "field": "owner_team", "op": "isSet" }, { "field": "risk", "op": "lte", "value": 2 } ] }
              ] } },
            { "key": "go_live", "name": "Go live", "from": "approved", "to": "done" }
        ]
    });
    let draft = w.ok("PUT", &format!("{DEFS}/{}/draft", w.definition), graph).await;
    w.ok(
        "POST",
        &format!("{DEFS}/{}/draft/publish", w.definition),
        json!({ "expectedDraftChecksum": draft["checksum"], "changeNote": "v1" }),
    )
    .await;
    w.approvers = w.profile("Approvers", &[(w.server, true)]).await;
    w.editors = w.profile("Editors", &[(w.server, true)]).await;
    let def = w.ok("GET", &format!("{DEFS}/{}", w.definition), json!(null)).await;
    let v = w
        .ok(
            "PUT",
            &format!("{DEFS}/{}/grants", w.definition),
            json!({ "version": def["version"], "grants": [
                { "transitionKey": "approve", "profiles": ["Approvers"] },
                { "transitionKey": "go_live", "profiles": ["Approvers"] },
                { "transitionKey": "_cancel", "profiles": ["Approvers"] }
            ] }),
        )
        .await;
    w.ok("PATCH", &format!("{DEFS}/{}", w.definition), json!({ "version": v["version"], "isActive": true })).await;
    w
}

/// Start, read, transition (with every refusal on the way), complete, the
/// lists and the CI's workflows; cancel and force; audit and events.
#[tokio::test]
async fn workflow_instances_run_with_conditions_grants_and_audit() {
    let Some(db) = scratch::database("workflow_instances_run").await else { return };
    let w = world(&db).await;
    let (approver, _) = w.user("approver", &[w.approvers]).await;
    let (editor, _) = w.user("editor", &[w.editors]).await;
    let viewers = w.profile("Viewers", &[(w.server, false)]).await;
    let (viewer, _) = w.user("viewer", &[viewers]).await;
    let ci = w.ci(w.server).await;

    // Start: needs edit (viewer 403), sets the state field, one running instance per workflow and CI.
    let (status, v) = w.start(&viewer, ci).await;
    assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{v}");
    let (status, v) = w.start(&editor, ci).await;
    assert_eq!(status, 201, "{v}");
    let instance = id(&v["instance"]);
    assert_eq!(v["instance"]["state"]["key"], "planned");
    assert_eq!(v["instance"]["version"], 1);
    assert_eq!(v["graph"]["initialState"], "planned");
    assert_eq!(v["graph"]["transitions"].as_array().unwrap().len(), 2);
    assert_eq!(v["availableTransitions"], json!([]), "the editor is granted nothing: {v}");
    assert_eq!(w.ci_values(ci).await["attributes"]["lifecycle"], json!(w.value("planned").to_string()));
    let (status, v) = w.start(&w.admin, ci).await;
    assert_eq!((status, details(&v)), (409, pairs(&[("ciId", "already_running")])), "{v}");
    let (status, v) = w.call(&w.admin, "POST", RUN, Some(json!({ "definitionKey": "nope", "ciId": ci }))).await;
    assert_eq!((status, code(&v)), (404, "NOT_FOUND"), "{v}");
    let (status, v) = w
        .call(&w.admin, "POST", RUN, Some(json!({ "definitionKey": "x", "definitionId": w.definition, "ciId": ci })))
        .await;
    assert_eq!((status, details(&v)), (400, pairs(&[("definitionId", "one_of")])), "{v}");
    let other = w.ci(w.network).await;
    let (status, v) = w.start(&w.admin, other).await;
    assert_eq!((status, details(&v)), (400, pairs(&[("ciId", "not_covered")])), "{v}");

    // The approver sees approve, blocked by its conditions on the CI as it stands.
    let (status, v) = w.call(&approver, "GET", &format!("{RUN}/{instance}"), None).await;
    assert_eq!(status, 200, "{v}");
    let approve = &v["availableTransitions"][0];
    assert_eq!((approve["key"].as_str(), approve["requiresComment"].as_bool()), (Some("approve"), Some(true)));
    assert_eq!(approve["toState"]["key"], "approved");
    let fields: Vec<(&str, bool)> = approve["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (f["key"].as_str().unwrap(), f["required"].as_bool().unwrap()))
        .collect();
    assert_eq!(fields, [("owner_team", true), ("risk", false)]);
    let blocked: Vec<&str> =
        approve["blockedBy"].as_array().unwrap().iter().map(|b| b["field"].as_str().unwrap()).collect();
    assert_eq!(blocked, ["fields.environment", "fields.owner_team", "fields.risk"], "{approve}");
    assert_eq!(v["canCancel"], true);

    // Refusals, none of which changes anything.
    let run = |key: &str, version: i64, fields: Value, comment: Option<&str>| json!({ "transitionKey": key, "expectedVersion": version, "fields": fields, "comment": comment });
    let (status, v) = w.transition(&approver, instance, run("approve", 1, json!({}), None)).await;
    assert_eq!((status, code(&v)), (422, "WORKFLOW_CONDITION_FAILED"), "{v}");
    assert_eq!(
        details(&v),
        pairs(&[
            ("comment", "comment_required"),
            ("fields.environment", "condition"),
            ("fields.owner_team", "condition"),
            ("fields.owner_team", "required"),
            ("fields.risk", "condition"),
        ])
    );
    let (status, v) = w.transition(&approver, instance, run("approve", 1, json!({ "notes": "x" }), Some("ok"))).await;
    assert_eq!((status, details(&v)), (400, pairs(&[("fields.notes", "not_a_transition_field")])), "{v}");
    let (status, v) = w.transition(&approver, instance, run("approve", 1, json!({ "risk": "high" }), Some("ok"))).await;
    assert_eq!((status, code(&v)), (400, "VALIDATION_ERROR"), "{v}");
    assert_eq!(details(&v)[0].0, "fields.risk");
    let (status, v) = w.transition(&approver, instance, run("approve", 7, json!({}), Some("ok"))).await;
    assert_eq!((status, code(&v)), (409, "VERSION_CONFLICT"), "{v}");
    let (status, v) = w.transition(&approver, instance, run("nope", 1, json!({}), None)).await;
    assert_eq!((status, details(&v)), (400, pairs(&[("transitionKey", "unknown_transition")])), "{v}");
    let (status, v) = w.transition(&approver, instance, run("go_live", 1, json!({}), None)).await;
    assert_eq!((status, details(&v)), (409, pairs(&[("transitionKey", "not_from_current_state")])), "{v}");
    let (status, v) = w.transition(&editor, instance, run("approve", 1, json!({}), Some("ok"))).await;
    assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "not granted: {v}");
    let (status, v) = w.transition(&viewer, instance, run("approve", 1, json!({}), Some("ok"))).await;
    assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "no edit right: {v}");
    let ci_version = w.ci_values(ci).await["version"].clone();

    // A low risk sent with the transition meets the `any`; the environment comes from the CI.
    w.ok("PATCH", &format!("/api/v1/configuration-items/{ci}"), json!({ "attributes": { "environment": "prod" } }))
        .await;
    let (status, v) = w
        .transition(&approver, instance, run("approve", 1, json!({ "owner_team": "ops", "risk": 1 }), Some("CAB ok")))
        .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        (v["state"]["key"].as_str(), v["version"].as_i64(), v["status"].as_str()),
        (Some("approved"), Some(2), Some("active"))
    );
    let item = w.ci_values(ci).await;
    assert_eq!(item["attributes"]["owner_team"], "ops");
    assert_eq!(item["attributes"]["lifecycle"], json!(w.value("approved").to_string()));
    assert_eq!(v["ciVersion"], item["version"]);
    assert_ne!(item["version"], ci_version);

    // Go live completes it; nothing runs on a completed instance.
    let (status, v) = w.transition(&approver, instance, run("go_live", 2, json!({}), None)).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!((v["status"].as_str(), v["state"]["terminal"].as_bool()), (Some("completed"), Some(true)));
    assert!(v["endedAt"].is_string());
    let (status, v) = w.transition(&approver, instance, run("go_live", 3, json!({}), None)).await;
    assert_eq!((status, details(&v)), (409, pairs(&[("id", "not_active")])), "{v}");

    // History: the events, and the audit rows on the CI with the CI update of the same request.
    let (status, v) = w.call(&viewer, "GET", &format!("{RUN}/{instance}/events"), None).await;
    assert_eq!(status, 200, "{v}");
    let kinds: Vec<(&str, &str)> = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["kind"].as_str().unwrap(), e["toStateKey"].as_str().unwrap()))
        .collect();
    assert_eq!(kinds, [("start", "planned"), ("transition", "approved"), ("transition", "done")]);
    assert_eq!(v["data"][1]["comment"], "CAB ok");
    assert_eq!(v["data"][1]["fieldChanges"]["owner_team"], json!({ "old": null, "new": "ops" }));
    assert_eq!(v["data"][1]["actorName"], "approver");
    let rows = w.audit_rows(ci).await;
    let actions: Vec<&str> = rows.iter().map(|(a, _)| a.as_str()).collect();
    assert_eq!(
        actions,
        [
            "create",
            "update",
            "workflow.start",
            "update",
            "update",
            "workflow.transition",
            "update",
            "workflow.transition"
        ]
    );
    assert_eq!(rows[4].1, rows[5].1, "the CI write and the transition are one request");
    let transition: Value = sqlx::query_scalar(
        "SELECT new_value FROM audit_log WHERE entity_id = $1 AND action = 'workflow.transition' ORDER BY id LIMIT 1",
    )
    .bind(ci)
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!(
        (transition["transitionKey"].as_str(), transition["stateKey"].as_str()),
        (Some("approve"), Some("approved"))
    );
    assert_eq!(transition["comment"], "CAB ok");

    // Cancel: the editor has no _cancel grant; the approver has it; a second cancel is 409.
    let ci2 = w.ci(w.server).await;
    let (_, v) = w.start(&w.admin, ci2).await;
    let second = id(&v["instance"]);
    let cancel = |version: i64| json!({ "expectedVersion": version, "reason": "Server order withdrawn" });
    let (status, v) = w.call(&editor, "POST", &format!("{RUN}/{second}/cancel"), Some(cancel(1))).await;
    assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{v}");
    let (status, v) = w
        .call(
            &approver,
            "POST",
            &format!("{RUN}/{second}/force"),
            Some(json!({
        "expectedVersion": 1, "stateKey": "done", "reason": "x" })),
        )
        .await;
    assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "force needs workflows.manage: {v}");
    let (status, v) = w
        .call(
            &w.admin,
            "POST",
            &format!("{RUN}/{second}/force"),
            Some(json!({
        "expectedVersion": 1, "stateKey": "nope", "reason": "x" })),
        )
        .await;
    assert_eq!((status, details(&v)), (400, pairs(&[("stateKey", "unknown_state")])), "{v}");
    let (status, v) = w
        .call(
            &w.admin,
            "POST",
            &format!("{RUN}/{second}/force"),
            Some(json!({
        "expectedVersion": 1, "stateKey": "planned", "reason": "x" })),
        )
        .await;
    assert_eq!((status, details(&v)), (409, pairs(&[("stateKey", "same_state")])), "{v}");
    let (status, v) = w
        .call(
            &w.admin,
            "POST",
            &format!("{RUN}/{second}/force"),
            Some(json!({
        "expectedVersion": 1, "stateKey": "approved", "reason": "Approved on paper" })),
        )
        .await;
    assert_eq!((status, v["state"]["key"].as_str()), (200, Some("approved")), "{v}");
    assert_eq!(w.ci_values(ci2).await["attributes"]["lifecycle"], json!(w.value("approved").to_string()));
    let (status, v) = w.call(&approver, "POST", &format!("{RUN}/{second}/cancel"), Some(cancel(1))).await;
    assert_eq!((status, code(&v)), (409, "VERSION_CONFLICT"), "{v}");
    let (status, v) = w.call(&approver, "POST", &format!("{RUN}/{second}/cancel"), Some(cancel(2))).await;
    assert_eq!((status, v["status"].as_str()), (200, Some("cancelled")), "{v}");
    let (status, v) = w.call(&approver, "POST", &format!("{RUN}/{second}/cancel"), Some(cancel(3))).await;
    assert_eq!((status, details(&v)), (409, pairs(&[("id", "not_active")])), "{v}");
    let force_row: (Value, Value) =
        sqlx::query_as("SELECT old_value, new_value FROM audit_log WHERE entity_id = $1 AND action = 'workflow.force'")
            .bind(ci2)
            .fetch_one(&w.pool)
            .await
            .unwrap();
    assert_eq!(
        (force_row.0["stateKey"].as_str(), force_row.1["reason"].as_str()),
        (Some("planned"), Some("Approved on paper"))
    );

    // Lists, summary and the CI's workflows.
    let ci3 = w.ci(w.server).await;
    w.start(&w.admin, ci3).await;
    let (status, v) =
        w.call(&viewer, "GET", &format!("{RUN}?status=active&definitionKey=server_lifecycle"), None).await;
    assert_eq!((status, v["page"]["total"].as_i64()), (200, Some(1)), "{v}");
    assert_eq!(v["data"][0]["ciId"], json!(ci3));
    let (_, v) = w.call(&viewer, "GET", &format!("{RUN}?stateKey=done"), None).await;
    assert_eq!(v["page"]["total"], 1);
    let (_, v) = w.call(&viewer, "GET", &format!("{RUN}?ciId={ci2}"), None).await;
    assert_eq!(v["data"][0]["status"], "cancelled");
    let (status, v) = w.call(&viewer, "GET", &format!("{RUN}/summary"), None).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        v["data"],
        json!([{ "definitionId": w.definition, "definitionKey": "server_lifecycle", "stateKey": "planned",
            "stateName": "Planned", "category": "open", "count": 1 }])
    );
    let (status, v) = w.call(&approver, "GET", &format!("/api/v1/configuration-items/{ci2}/workflows"), None).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["data"][0]["instance"]["status"], "cancelled");
    assert_eq!(v["data"][0]["availableTransitions"], json!([]));
    assert_eq!(v["startable"][0]["definitionKey"], "server_lifecycle");
    let (_, v) = w.call(&viewer, "GET", &format!("/api/v1/configuration-items/{ci2}/workflows"), None).await;
    assert_eq!(v["startable"], json!([]), "no edit right, nothing to start");

    // A sequence of steps leaves the audit chain intact.
    let problems: Vec<(i64, String)> =
        sqlx::query_as("SELECT chain_seq, problem FROM audit_log_verify()").fetch_all(&w.pool).await.unwrap();
    assert_eq!(problems, vec![]);
    db.drop().await;
}

/// §4.2: an instance on a CI of a type the caller may not view does not
/// exist for them, on every endpoint; lists and counts leave it out.
#[tokio::test]
async fn hidden_instances_answer_404_and_are_not_counted() {
    let Some(db) = scratch::database("workflow_instances_hidden").await else { return };
    let w = world(&db).await;
    let network_only = w.profile("Network only", &[(w.network, true)]).await;
    let (outsider, _) = w.user("outsider", &[network_only]).await;
    let ci = w.ci(w.server).await;
    let (_, v) = w.start(&w.admin, ci).await;
    let instance = id(&v["instance"]);

    let missing = |status: u16, v: &Value| status == 404 && code(v) == "NOT_FOUND";
    for (method, path, body) in [
        ("GET", format!("{RUN}/{instance}"), None),
        ("GET", format!("{RUN}/{instance}/events"), None),
        ("GET", format!("/api/v1/configuration-items/{ci}/workflows"), None),
        (
            "POST",
            format!("{RUN}/{instance}/transitions"),
            Some(json!({ "transitionKey": "approve", "expectedVersion": 1, "comment": "x" })),
        ),
        ("POST", format!("{RUN}/{instance}/cancel"), Some(json!({ "expectedVersion": 1, "reason": "x" }))),
        ("POST", RUN.to_owned(), Some(json!({ "definitionKey": "server_lifecycle", "ciId": ci }))),
    ] {
        let (status, v) = w.call(&outsider, method, &path, body).await;
        assert!(missing(status, &v), "{method} {path}: {status} {v}");
    }
    // The same answer as for an instance that does not exist.
    let (status, v) = w.call(&outsider, "GET", &format!("{RUN}/{}", Uuid::new_v4()), None).await;
    assert!(missing(status, &v));
    let (_, v) = w.call(&outsider, "GET", RUN, None).await;
    assert_eq!((v["page"]["total"].as_i64(), v["data"].clone()), (Some(0), json!([])), "{v}");
    let (_, v) = w.call(&outsider, "GET", &format!("{RUN}/summary"), None).await;
    assert_eq!(v["data"], json!([]));
    // The workflow rows of the CI's history are hidden like its other rows.
    let audit = |creds: Creds| {
        let w = &w;
        async move { w.call(&creds, "GET", &format!("/api/v1/audit-log?entityId={ci}"), None).await }
    };
    let audit_viewers = w.profile("Auditors", &[(w.network, false)]).await;
    sqlx::query("INSERT INTO permission_profile_global_permissions (profile_id, permission) VALUES ($1, 'audit.view')")
        .bind(audit_viewers)
        .execute(&w.pool)
        .await
        .unwrap();
    let (auditor, _) = w.user("auditor", &[audit_viewers]).await;
    let (status, v) = audit(auditor).await;
    assert_eq!((status, v["page"]["total"].as_i64()), (200, Some(0)), "{v}");
    let (_, v) = audit(w.admin.clone()).await;
    assert!(v["data"].as_array().unwrap().iter().any(|e| e["action"] == "workflow.start"), "{v}");
    db.drop().await;
}

/// Q6: a token may run a transition only when its owner and its narrowing
/// profile are both granted it (the Administrator profile is granted all).
#[tokio::test]
async fn tokens_run_a_transition_only_when_their_profile_is_granted_too() {
    let Some(db) = scratch::database("workflow_tokens_need_their_profile").await else { return };
    let w = world(&db).await;
    let (_, approver) = w.user("approver", &[w.approvers, w.editors]).await;
    let (_, editor) = w.user("editor", &[w.editors]).await;
    let admin_id: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE username = 'admin'").fetch_one(&w.pool).await.unwrap();
    let admin_profile: Uuid =
        sqlx::query_scalar("SELECT id FROM permission_profiles WHERE is_builtin").fetch_one(&w.pool).await.unwrap();

    // (token, expected status of an approve it is otherwise ready for)
    let cases = [
        ("approver's token narrowed to Approvers", w.token(approver, w.approvers).await, 200),
        ("approver's token narrowed to Editors", w.token(approver, w.editors).await, 403),
        ("editor's token narrowed to Editors", w.token(editor, w.editors).await, 403),
        ("administrator's token narrowed to Editors", w.token(admin_id, w.editors).await, 403),
        ("administrator's token narrowed to Approvers", w.token(admin_id, w.approvers).await, 200),
        ("administrator's token with the Administrator profile", w.token(admin_id, admin_profile).await, 200),
    ];
    for (what, token, expected) in cases {
        let ci = w.ci(w.server).await;
        w.ok("PATCH", &format!("/api/v1/configuration-items/{ci}"), json!({ "attributes": { "environment": "prod" } }))
            .await;
        let (status, v) = w.start(&token, ci).await;
        assert_eq!(status, 201, "{what}: {v}");
        let instance = id(&v["instance"]);
        let offered = v["availableTransitions"].as_array().unwrap().iter().any(|t| t["key"] == "approve");
        assert_eq!(offered, expected == 200, "{what}: {v}");
        let body = json!({ "transitionKey": "approve", "expectedVersion": 1, "fields": { "owner_team": "ops" },
            "comment": "ok" });
        let (status, v) = w.transition(&token, instance, body).await;
        assert_eq!(status, expected, "{what}: {v}");
        let actor: Option<String> =
            sqlx::query_scalar("SELECT actor_type FROM audit_log WHERE entity_id = $1 AND action = 'workflow.start'")
                .bind(ci)
                .fetch_one(&w.pool)
                .await
                .unwrap();
        assert_eq!(actor.as_deref(), Some("api_client"), "{what}");
    }
    db.drop().await;
}

/// Deleting a CI cancels its running instances in the same transaction.
#[tokio::test]
async fn deleting_a_ci_cancels_its_running_instances() {
    let Some(db) = scratch::database("workflow_ci_delete_cancels").await else { return };
    let w = world(&db).await;
    let ci = w.ci(w.server).await;
    let (_, v) = w.start(&w.admin, ci).await;
    let instance = id(&v["instance"]);
    let (status, v) = w.call(&w.admin, "DELETE", &format!("/api/v1/configuration-items/{ci}"), None).await;
    assert_eq!(status, 204, "{v}");
    let (status, v) = w.call(&w.admin, "GET", &format!("{RUN}/{instance}"), None).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!((v["instance"]["status"].as_str(), v["instance"]["version"].as_i64()), (Some("cancelled"), Some(2)));
    let (_, v) = w.call(&w.admin, "GET", &format!("{RUN}/{instance}/events"), None).await;
    assert_eq!((v["data"][1]["kind"].as_str(), v["data"][1]["actorType"].as_str()), (Some("cancel"), Some("system")));
    let rows = w.audit_rows(ci).await;
    let tail: Vec<&str> = rows.iter().rev().take(2).map(|(a, _)| a.as_str()).collect();
    assert_eq!(tail, ["delete", "workflow.cancel"]);
    assert_eq!(rows[rows.len() - 1].1, rows[rows.len() - 2].1, "one request");
    let (status, v) = w.start(&w.admin, ci).await;
    assert_eq!((status, details(&v)), (409, pairs(&[("ciId", "deleted")])), "{v}");
    db.drop().await;
}

/// A workflow starts only once it is published and while it is active.
#[tokio::test]
async fn unpublished_and_inactive_workflows_start_nothing() {
    let Some(db) = scratch::database("workflow_start_unpublished").await else { return };
    let w = world(&db).await;
    let draft_only =
        w.ok("POST", DEFS, json!({ "key": "network_review", "name": "Network review", "classId": w.network })).await;
    let network_ci = w.ci(w.network).await;
    let start = json!({ "definitionId": id(&draft_only), "ciId": network_ci });
    let (status, v) = w.call(&w.admin, "POST", RUN, Some(start)).await;
    assert_eq!((status, details(&v)), (409, pairs(&[("definitionId", "unpublished")])), "{v}");

    let def = w.ok("GET", &format!("{DEFS}/{}", w.definition), json!(null)).await;
    w.ok("PATCH", &format!("{DEFS}/{}", w.definition), json!({ "version": def["version"], "isActive": false })).await;
    let (status, v) = w.start(&w.admin, w.ci(w.server).await).await;
    assert_eq!((status, details(&v)), (409, pairs(&[("definitionId", "inactive")])), "{v}");
    db.drop().await;
}

/// Amendment 1: a transition and a delete of the same CI, in parallel, many
/// times. Both lock the CI row first, so neither deadlocks (no 40P01, which
/// would surface as a 500), and the end state is always consistent: the CI
/// is deleted and its instance cancelled, whichever ran first.
#[tokio::test]
async fn a_transition_racing_a_ci_delete_never_deadlocks() {
    let Some(db) = scratch::database("workflow_transition_delete_race").await else { return };
    let w = world(&db).await;
    let mut transitioned_first = 0;
    for i in 0..40 {
        let ci = w.ci(w.server).await;
        w.ok("PATCH", &format!("/api/v1/configuration-items/{ci}"), json!({ "attributes": { "environment": "prod" } }))
            .await;
        let (_, v) = w.start(&w.admin, ci).await;
        let instance = id(&v["instance"]);
        let body = json!({ "transitionKey": "approve", "expectedVersion": 1, "fields": { "owner_team": "ops" },
            "comment": "race" });
        let delete_path = format!("/api/v1/configuration-items/{ci}");
        let ((t_status, t), (d_status, d)) =
            tokio::join!(w.transition(&w.admin, instance, body), w.call(&w.admin, "DELETE", &delete_path, None));
        assert_eq!(d_status, 204, "delete {i}: {d}");
        match t_status {
            200 => transitioned_first += 1,
            409 => assert_eq!(details(&t), pairs(&[("id", "not_active")]), "transition {i}: {t}"),
            _ => panic!("transition {i}: {t_status} {t}"),
        }
        let (status, deleted): (String, bool) = sqlx::query_as(
            "SELECT wi.status, ci.deleted_at IS NOT NULL FROM workflow_instances wi
             JOIN configuration_items ci ON ci.id = wi.ci_id WHERE wi.id = $1",
        )
        .bind(instance)
        .fetch_one(&w.pool)
        .await
        .unwrap();
        assert_eq!((status.as_str(), deleted), ("cancelled", true), "race {i}");
        let events: Vec<String> =
            sqlx::query_scalar("SELECT kind FROM workflow_instance_events WHERE instance_id = $1 ORDER BY id")
                .bind(instance)
                .fetch_all(&w.pool)
                .await
                .unwrap();
        let expected: &[&str] = if t_status == 200 { &["start", "transition", "cancel"] } else { &["start", "cancel"] };
        assert_eq!(events, expected, "race {i}");
    }
    eprintln!("transition won the CI lock {transitioned_first} of 40 times");
    let problems: Vec<(i64, String)> =
        sqlx::query_as("SELECT chain_seq, problem FROM audit_log_verify()").fetch_all(&w.pool).await.unwrap();
    assert_eq!(problems, vec![]);
    db.drop().await;
}
