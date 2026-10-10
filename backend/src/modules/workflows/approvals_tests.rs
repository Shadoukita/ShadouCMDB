//! Approvals slice A2 (SHAA-1894, design SHAA-1869 §3.1, §6.2, §10.1, §11)
//! through the real router against PostgreSQL: the draft `approval` block,
//! the publish lint, approver assignments and their preview, IN_USE for a
//! field that names approvers, and configuration format 9.

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
    lifecycle: Uuid,
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

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter().map(|(a, b)| ((*a).to_owned(), (*b).to_owned())).collect()
}

fn problem_codes(v: &Value) -> Vec<(String, String, String)> {
    v["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["path"].as_str().unwrap().to_owned(),
                p["code"].as_str().unwrap().to_owned(),
                p["severity"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
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
        let mut w = World { app, pool: db.pool.clone(), admin, password, server: Uuid::nil(), lifecycle: Uuid::nil() };
        w.server = id(&w.ok("POST", "/api/v1/ci-classes", json!({ "key": "server", "name": "Server" }), 201).await);
        let list = id(&w
            .ok("POST", "/api/v1/lookup-lists", json!({ "key": "server_state", "name": "Server state" }), 201)
            .await);
        for key in ["planned", "approved", "live"] {
            w.ok("POST", "/api/v1/lookup-list-values", json!({ "listId": list, "key": key, "name": key }), 201).await;
        }
        let field = |key: &str, extra: Value| {
            let mut b = json!({ "classId": w.server, "key": key, "label": key });
            b.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            b
        };
        w.lifecycle = id(&w
            .ok(
                "POST",
                "/api/v1/attribute-definitions",
                field("lifecycle", json!({ "dataType": "lookup", "lookupListId": list })),
                201,
            )
            .await);
        let person = w.person_class().await;
        w.ok(
            "POST",
            "/api/v1/attribute-definitions",
            field("owner", json!({ "label": "Owner", "dataType": "reference", "referenceClassId": person })),
            201,
        )
        .await;
        w.ok(
            "POST",
            "/api/v1/attribute-definitions",
            field("runs_on", json!({ "dataType": "reference", "referenceClassId": w.server })),
            201,
        )
        .await;
        w.ok("POST", "/api/v1/attribute-definitions", field("owner_team", json!({ "dataType": "text" })), 201).await;
        w
    }

    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let (status, v, _) = call(&self.app, method, path, &self.admin, body).await;
        (status, v)
    }

    async fn ok(&self, method: &str, path: &str, body: Value, expected: u16) -> Value {
        let (status, v) = self.call(method, path, Some(body)).await;
        assert_eq!(status, expected, "{method} {path}: {v}");
        v
    }

    async fn person_class(&self) -> Uuid {
        sqlx::query_scalar("SELECT id FROM ci_classes WHERE system_role = 'person'")
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    /// A profile with view (and, when asked, edit) on the server type, and these global rights.
    async fn profile(&self, name: &str, view: bool) -> Uuid {
        let profile: Uuid = sqlx::query_scalar("INSERT INTO permission_profiles (name) VALUES ($1) RETURNING id")
            .bind(name)
            .fetch_one(&self.pool)
            .await
            .unwrap();
        if view {
            sqlx::query(
                "INSERT INTO permission_profile_class_permissions (profile_id, class_id, can_view, can_create, can_edit, can_delete)
                 VALUES ($1, $2, true, false, false, false)",
            )
            .bind(profile)
            .bind(self.server)
            .execute(&self.pool)
            .await
            .unwrap();
        }
        profile
    }

    async fn user(&self, name: &str, profiles: &[Uuid]) -> Uuid {
        let body = json!({ "username": name, "email": format!("{name}@example.test"), "displayName": name.to_uppercase(),
            "password": self.password, "profileIds": profiles });
        id(&self.ok("POST", "/api/v1/admin/users", body, 201).await)
    }

    async fn group(&self, name: &str, members: &[Uuid]) -> Uuid {
        let group = self.ok("POST", "/api/v1/admin/groups", json!({ "name": name }), 201).await;
        let g = id(&group);
        if !members.is_empty() {
            let body = json!({ "version": group["version"], "userIds": members });
            let (status, v) = self.call("PUT", &format!("/api/v1/admin/groups/{g}/members"), Some(body)).await;
            assert_eq!(status, 200, "{v}");
        }
        g
    }

    async fn workflow(&self, key: &str, graph: Value) -> Uuid {
        let body = json!({ "key": key, "name": key, "classId": self.server, "stateAttributeId": self.lifecycle,
            "isActive": true });
        let def = id(&self.ok("POST", BASE, body, 201).await);
        self.ok("PUT", &format!("{BASE}/{def}/draft"), graph, 200).await;
        def
    }

    async fn publish(&self, def: Uuid) -> Value {
        let (_, d) = self.call("GET", &format!("{BASE}/{def}"), None).await;
        let body = json!({ "expectedDraftChecksum": d["draftChecksum"] });
        self.ok("POST", &format!("{BASE}/{def}/draft/publish"), body, 201).await
    }

    async fn set_approvers(&self, def: Uuid, approvers: Value) -> (u16, Value) {
        let (_, d) = self.call("GET", &format!("{BASE}/{def}"), None).await;
        let body = json!({ "version": d["version"], "approvers": approvers });
        self.call("PUT", &format!("{BASE}/{def}/approvers"), Some(body)).await
    }

    async fn count(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned())).fetch_one(&self.pool).await.unwrap()
    }
}

/// The change workflow of the design (§10.1): `approve` needs a technical
/// review, then two CAB approvals.
fn gated_graph() -> Value {
    json!({
        "initialState": "planned",
        "states": [
            { "key": "planned", "name": "Planned", "category": "open", "stateValue": "planned" },
            { "key": "approved", "name": "Approved", "category": "active", "stateValue": "approved" },
            { "key": "done", "name": "Live", "category": "done", "terminal": true, "stateValue": "live" }
        ],
        "transitions": [
            { "key": "approve", "name": "Approve", "from": "planned", "to": "approved",
              "approval": { "steps": [
                { "key": "tech", "name": "Technical review", "requiredApprovals": 1, "dueAfter": "PT48H" },
                { "key": "cab", "name": "CAB", "requiredApprovals": 2, "dueAfter": "P5D", "onOverdue": "reject",
                  "distinctFromEarlier": true, "excludeActorsOf": ["go_live"], "allowApiTokens": false } ] } },
            { "key": "go_live", "name": "Go live", "from": "approved", "to": "done" }
        ]
    })
}

/// Draft → lint → approvers → publish: the `approval` block is stored and
/// read back canonically, refused when malformed, linted at publish (errors
/// and warnings of §6.2), and staffing is checked on PUT approvers. A field
/// that names approvers cannot be archived (IN_USE); assignments of a step
/// only the draft had go with the draft; a gated transition creates an
/// approval request instead of moving the instance.
#[tokio::test]
async fn approval_policies_are_drafted_linted_staffed_and_published() {
    let Some(db) = scratch::database("approvals_design_time").await else { return };
    let w = World::new(&db).await;
    let def = w.workflow("change", gated_graph()).await;
    let draft = format!("{BASE}/{def}/draft");
    let approvers = format!("{BASE}/{def}/approvers");

    // Stored and read back, the interval in canonical form, defaults filled in.
    let (status, v) = w.call("GET", &draft, None).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        v["transitions"][0]["approval"],
        json!({ "steps": [
            { "key": "tech", "name": "Technical review", "requiredApprovals": 1, "dueAfter": "P2D", "onOverdue": "flag",
              "distinctFromEarlier": true, "excludeActorsOf": [], "allowApiTokens": false },
            { "key": "cab", "name": "CAB", "requiredApprovals": 2, "dueAfter": "P5D", "onOverdue": "reject",
              "distinctFromEarlier": true, "excludeActorsOf": ["go_live"], "allowApiTokens": false } ] })
    );
    assert!(v["transitions"][1].get("approval").is_none(), "no policy, no key: {v}");

    // Malformed policies are 400 with paths.
    let mut bad = gated_graph();
    bad["transitions"][0]["approval"]["steps"][1]["key"] = json!("tech");
    bad["transitions"][0]["approval"]["steps"][1]["excludeActorsOf"] = json!(["go_live", "go_live"]);
    let (status, v) = w.call("PUT", &draft, Some(bad)).await;
    assert_eq!(
        (status, details(&v)),
        (
            400,
            pairs(&[
                ("transitions[0].approval.steps[1].key", "duplicate"),
                ("transitions[0].approval.steps[1].excludeActorsOf[1]", "duplicate"),
            ])
        ),
        "{v}"
    );
    let mut bad = gated_graph();
    bad["transitions"][0]["approval"]["steps"][0]["onOverdue"] = json!("reject");
    bad["transitions"][0]["approval"]["steps"][0].as_object_mut().unwrap().remove("dueAfter");
    let (status, v) = w.call("PUT", &draft, Some(bad)).await;
    assert_eq!(
        (status, details(&v)),
        (400, pairs(&[("transitions[0].approval.steps[0].dueAfter", "required")])),
        "{v}"
    );
    for (due, code) in [("PT10M", "range"), ("P91D", "range"), ("P1M", "invalid_format"), ("PT", "invalid_format")] {
        let mut bad = gated_graph();
        bad["transitions"][0]["approval"]["steps"][0]["dueAfter"] = json!(due);
        let (status, v) = w.call("PUT", &draft, Some(bad)).await;
        assert_eq!(status, 400, "{due}: {v}");
        let (field, found) = details(&v).into_iter().next().unwrap();
        assert!(field.contains("dueAfter") && found == code, "{due}: {v}");
    }
    let mut bad = gated_graph();
    bad["transitions"][0]["approval"]["steps"][0]["requiredApprovals"] = json!(21);
    bad["transitions"][0]["approval"]["steps"] =
        json!((0..6).map(|n| json!({ "key": format!("s{n}"), "name": "S" })).collect::<Vec<_>>());
    let (status, v) = w.call("PUT", &draft, Some(bad)).await;
    assert_eq!(status, 400, "{v}");
    assert!(details(&v).iter().any(|(f, c)| f == "transitions.0.approval.steps" && c == "too_big"), "{v}");

    // The lint: unknown excluded transition is an error; unstaffed steps are warnings.
    let mut ghost = gated_graph();
    ghost["transitions"][0]["approval"]["steps"][1]["excludeActorsOf"] = json!(["ghost"]);
    w.ok("PUT", &draft, ghost, 200).await;
    let (status, v) = w.call("POST", &format!("{draft}/validate"), None).await;
    assert_eq!((status, v["valid"].as_bool()), (200, Some(false)), "{v}");
    let codes = problem_codes(&v);
    for expected in [
        ("transitions[0].approval.steps[1].excludeActorsOf[0]", "unknown_transition", "error"),
        ("transitions[0].approval.steps[0]", "no_approvers", "warning"),
        ("transitions[0].approval.steps[1]", "no_approvers", "warning"),
    ] {
        let e = (expected.0.to_owned(), expected.1.to_owned(), expected.2.to_owned());
        assert!(codes.contains(&e), "{expected:?} in {codes:?}");
    }
    let (status, d) = w.call("GET", &format!("{BASE}/{def}"), None).await;
    assert_eq!(status, 200);
    let (status, v) =
        w.call("POST", &format!("{draft}/publish"), Some(json!({ "expectedDraftChecksum": d["draftChecksum"] }))).await;
    assert_eq!(
        (status, details(&v)),
        (400, pairs(&[("transitions[0].approval.steps[1].excludeActorsOf[0]", "unknown_transition")])),
        "{v}"
    );
    w.ok("PUT", &draft, gated_graph(), 200).await;

    // People: a CAB group of two who may view servers, a profile nobody with view holds, a named user.
    let viewers = w.profile("Server viewers", true).await;
    let blind = w.profile("Blind", false).await;
    let alice = w.user("alice", &[viewers]).await;
    let bob = w.user("bob", &[viewers]).await;
    w.user("carol", &[blind]).await;
    w.group("CAB", &[alice, bob]).await;
    w.group("Empty group", &[]).await;

    // PUT approvers: what cannot be resolved is 400, nothing stored.
    let (status, v) = w
        .set_approvers(
            def,
            json!([
                { "transitionKey": "approve", "stepKey": "nope", "source": "group", "group": "CAB" },
                { "transitionKey": "approve", "stepKey": "cab", "source": "group", "group": "CAB", "user": "alice" },
                { "transitionKey": "approve", "stepKey": "cab", "source": "profile" },
                { "transitionKey": "approve", "stepKey": "cab", "source": "group", "group": "No such group" },
                { "transitionKey": "approve", "stepKey": "tech", "source": "ci_attribute", "attribute": "owner_team" },
                { "transitionKey": "approve", "stepKey": "tech", "source": "ci_attribute", "attribute": "runs_on" },
                { "transitionKey": "approve", "stepKey": "tech", "source": "ci_attribute", "attribute": "missing" }
            ]),
        )
        .await;
    assert_eq!(status, 400, "{v}");
    assert_eq!(
        details(&v),
        pairs(&[("approvers[1].user", "source_mismatch"), ("approvers[2].profile", "required"),]),
        "the shape is checked first: {v}"
    );
    let (status, v) = w
        .set_approvers(
            def,
            json!([
                { "transitionKey": "approve", "stepKey": "nope", "source": "group", "group": "CAB" },
                { "transitionKey": "approve", "stepKey": "cab", "source": "group", "group": "No such group" },
                { "transitionKey": "approve", "stepKey": "tech", "source": "ci_attribute", "attribute": "owner_team" },
                { "transitionKey": "approve", "stepKey": "tech", "source": "ci_attribute", "attribute": "runs_on" },
                { "transitionKey": "approve", "stepKey": "tech", "source": "ci_attribute", "attribute": "missing" },
                { "transitionKey": "approve", "stepKey": "cab", "source": "user", "user": "ALICE" },
                { "transitionKey": "approve", "stepKey": "cab", "source": "user", "user": alice }
            ]),
        )
        .await;
    assert_eq!(status, 400, "{v}");
    assert_eq!(
        details(&v),
        pairs(&[
            ("approvers[0].stepKey", "unknown_step"),
            ("approvers[1].group", "not_found"),
            ("approvers[2].attribute", "attribute_type"),
            ("approvers[3].attribute", "attribute_type"),
            ("approvers[4].attribute", "unknown_attribute"),
            ("approvers[6]", "duplicate"),
        ]),
        "{v}"
    );
    assert_eq!(w.count("SELECT count(*) FROM workflow_approval_assignments").await, 0);

    // Every source; the lint warns about the blind profile, the empty group and a short CAB.
    let (status, v) = w
        .set_approvers(
            def,
            json!([
                { "transitionKey": "approve", "stepKey": "tech", "source": "ci_attribute", "attribute": "owner" },
                { "transitionKey": "approve", "stepKey": "tech", "source": "profile", "profile": "blind" },
                { "transitionKey": "approve", "stepKey": "cab", "source": "group", "group": "cab" },
                { "transitionKey": "approve", "stepKey": "cab", "source": "group", "group": "Empty group" },
                { "transitionKey": "approve", "stepKey": "cab", "role": "escalation", "source": "user", "user": "carol" },
                { "transitionKey": "approve", "stepKey": "cab", "role": "escalation", "source": "service_owner",
                  "serviceOwnerRole": "business" }
            ]),
        )
        .await;
    assert_eq!(status, 200, "{v}");
    let version = v["version"].as_i64().unwrap();
    assert_eq!(v["approvers"].as_array().unwrap().len(), 6, "{v}");
    assert_eq!(v["approvers"][0]["stepKey"], "cab", "sorted by transition, step, role, source: {v}");
    assert_eq!(v["approvers"][0]["group"]["name"], "CAB", "{v}");
    let attribute = v["approvers"].as_array().unwrap().iter().find(|a| a["source"] == "ci_attribute").unwrap();
    assert_eq!(
        (&attribute["attribute"]["key"], &attribute["attribute"]["classKey"]),
        (&json!("owner"), &json!("server"))
    );
    let codes = problem_codes(&v);
    for expected in [
        ("transitions.approve.steps.tech", "approvers_cannot_view"),
        ("transitions.approve.steps.cab", "approvers_cannot_view"),
    ] {
        assert!(codes.iter().any(|c| (c.0.as_str(), c.1.as_str()) == expected), "{expected:?} in {codes:?}");
    }
    assert!(!codes.iter().any(|c| c.1 == "understaffed"), "CAB has two viewers for a quorum of 2: {codes:?}");
    // The same set again changes nothing: no version bump, no audit row.
    let audits = w.count("SELECT count(*) FROM audit_log WHERE entity_type = 'workflow_definitions'").await;
    let (status, same) = w.call("GET", &approvers, None).await;
    assert_eq!((status, same["version"].as_i64()), (200, Some(version)));
    let again: Vec<Value> = same["approvers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            let mut b = json!({ "transitionKey": a["transitionKey"], "stepKey": a["stepKey"], "role": a["role"],
                "source": a["source"] });
            for (k, v) in [("profile", "profile"), ("group", "group"), ("user", "user"), ("attribute", "attribute")] {
                if let Some(id) = a[k]["id"].as_str() {
                    b[v] = json!(id);
                }
            }
            if !a["serviceOwnerRole"].is_null() {
                b["serviceOwnerRole"] = a["serviceOwnerRole"].clone();
            }
            b
        })
        .collect();
    let (status, v) = w.set_approvers(def, json!(again)).await;
    assert_eq!((status, v["version"].as_i64()), (200, Some(version)), "{v}");
    assert_eq!(w.count("SELECT count(*) FROM audit_log WHERE entity_type = 'workflow_definitions'").await, audits);
    let (old, new): (Value, Value) = sqlx::query_as(
        "SELECT old_value, new_value FROM audit_log WHERE entity_type = 'workflow_definitions' AND entity_id = $1
         AND new_value ? 'approvers' AND action = 'update' ORDER BY id DESC LIMIT 1",
    )
    .bind(def)
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!(old["approvers"], json!([]));
    assert!(
        new["approvers"].as_array().unwrap().contains(&json!({ "transition": "approve", "step": "cab",
            "role": "escalation", "user": "carol" })),
        "audited by name: {new}"
    );
    // A short CAB: two viewers for a quorum of three.
    let mut short = gated_graph();
    short["transitions"][0]["approval"]["steps"][1]["requiredApprovals"] = json!(3);
    w.ok("PUT", &draft, short, 200).await;
    let (_, v) = w.call("GET", &approvers, None).await;
    assert!(problem_codes(&v).iter().any(|c| c.0 == "transitions.approve.steps.cab" && c.1 == "understaffed"), "{v}");
    w.ok("PUT", &draft, gated_graph(), 200).await;

    // Publish: warnings do not refuse it; the version carries the policy, and its checksum covers it.
    let (status, v) = w.call("POST", &format!("{draft}/validate"), None).await;
    assert_eq!((status, v["valid"].as_bool()), (200, Some(true)), "{v}");
    let v1 = w.publish(def).await;
    assert_eq!(v1["transitions"][0]["approval"]["steps"][1]["requiredApprovals"], 2);
    let mut plain = gated_graph();
    plain["transitions"][0].as_object_mut().unwrap().remove("approval");
    let (_, p) = w.call("PUT", &draft, Some(plain)).await;
    assert_ne!(p["checksum"], v1["checksum"], "the policy is part of the checksum");
    let (status, _) = w.call("DELETE", &draft, None).await;
    assert_eq!(status, 204);

    // A field that names approvers cannot be archived or purged.
    let owner: Uuid = sqlx::query_scalar("SELECT id FROM ci_attribute_definitions WHERE key = 'owner'")
        .fetch_one(&w.pool)
        .await
        .unwrap();
    let (status, v) = w.call("DELETE", &format!("/api/v1/attribute-definitions/{owner}"), None).await;
    assert_eq!((status, code(&v)), (409, "IN_USE"), "{v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("change (approvers of approve.tech)"), "{v}");

    // A gated transition creates an approval request; the instance stays where it is.
    let (status, ci) = w.call("POST", "/api/v1/configuration-items", Some(json!({ "classId": w.server }))).await;
    assert_eq!(status, 201, "{ci}");
    let (status, inst) =
        w.call("POST", "/api/v1/workflow-instances", Some(json!({ "ciId": id(&ci), "definitionId": def }))).await;
    assert!(status == 201 || status == 409, "{inst}");
    let instance: (Uuid, i32) = sqlx::query_as("SELECT id, version FROM workflow_instances WHERE ci_id = $1")
        .bind(id(&ci))
        .fetch_one(&w.pool)
        .await
        .unwrap();
    let body = json!({ "transitionKey": "approve", "expectedVersion": instance.1 });
    let (status, v) =
        w.call("POST", &format!("/api/v1/workflow-instances/{}/transitions", instance.0), Some(body)).await;
    assert_eq!((status, v["pendingApproval"]["stepKey"].as_str()), (202, Some("tech")), "{v}");
    let state: String = sqlx::query_scalar(
        "SELECT s.key FROM workflow_instances i JOIN workflow_states s ON s.id = i.current_state_id WHERE i.id = $1",
    )
    .bind(instance.0)
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!(state, "planned");

    // Assignments of a step that only the draft had go with the draft, audited.
    let mut extra = gated_graph();
    extra["transitions"][1]["approval"] = json!({ "steps": [ { "key": "ops", "name": "Operations" } ] });
    w.ok("PUT", &draft, extra, 200).await;
    let (_, now) = w.call("GET", &approvers, None).await;
    let mut list = again.clone();
    list.push(json!({ "transitionKey": "go_live", "stepKey": "ops", "source": "user", "user": "alice" }));
    let (status, v) = w.set_approvers(def, json!(list)).await;
    assert_eq!((status, v["approvers"].as_array().map(Vec::len)), (200, Some(7)), "{v}");
    assert!(v["version"].as_i64() > now["version"].as_i64());
    let (status, _) = w.call("DELETE", &draft, None).await;
    assert_eq!(status, 204);
    let (_, v) = w.call("GET", &approvers, None).await;
    assert_eq!(v["approvers"].as_array().map(Vec::len), Some(6), "{v}");
    assert!(!v["approvers"].to_string().contains("\"ops\""), "{v}");
    let (status, v) = w
        .set_approvers(
            def,
            json!([ { "transitionKey": "go_live", "stepKey": "ops", "source": "user", "user": "alice" } ]),
        )
        .await;
    assert_eq!((status, details(&v)), (400, pairs(&[("approvers[0].stepKey", "unknown_step")])), "{v}");

    // Approvers need workflows.manage, like everything here.
    let profile = w.profile("Readers", true).await;
    w.user("reader", &[profile]).await;
    let body = json!({ "username": "reader", "password": w.password });
    let (status, me, headers) = call(&w.app, "POST", "/api/v1/auth/login", &Creds::default(), Some(body)).await;
    assert_eq!(status, 200, "{me}");
    let reader = session_of(&me, &headers);
    let (status, _, _) = call(&w.app, "GET", &approvers, &reader, None).await;
    assert_eq!(status, 403);
    let (status, _, _) =
        call(&w.app, "GET", &format!("{approvers}/preview?transition=approve&step=cab"), &reader, None).await;
    assert_eq!(status, 403);
    db.drop().await;
}

/// The preview explains, per user, why they are in or out: eligible, no view
/// right, disabled, the requester (four-eyes), escalation only; the field and
/// service owner sources resolve on the CI given.
#[tokio::test]
async fn the_approver_preview_explains_who_is_in_and_out() {
    let Some(db) = scratch::database("approvals_preview").await else { return };
    let w = World::new(&db).await;
    let def = w.workflow("change", gated_graph()).await;
    w.publish(def).await;
    let viewers = w.profile("Server viewers", true).await;
    let blind = w.profile("Blind", false).await;
    let alice = w.user("alice", &[viewers]).await;
    let bob = w.user("bob", &[blind]).await;
    let carol = w.user("carol", &[viewers]).await;
    w.user("dave", &[viewers]).await;
    let erin = w.user("erin", &[viewers]).await;
    let frank = w.user("frank", &[viewers]).await;
    sqlx::query("UPDATE users SET is_active = false WHERE id = $1").bind(carol).execute(&w.pool).await.unwrap();
    w.group("CAB", &[alice, bob, carol]).await;
    let (status, v) = w
        .set_approvers(
            def,
            json!([
                { "transitionKey": "approve", "stepKey": "cab", "source": "group", "group": "CAB" },
                { "transitionKey": "approve", "stepKey": "cab", "source": "ci_attribute", "attribute": "owner" },
                { "transitionKey": "approve", "stepKey": "cab", "source": "service_owner", "serviceOwnerRole": "technical" },
                { "transitionKey": "approve", "stepKey": "cab", "role": "escalation", "source": "user", "user": "dave" }
            ]),
        )
        .await;
    assert_eq!(status, 200, "{v}");

    // A CI owned by erin's Person, member of a business service that frank owns technically.
    let person: Option<Uuid> =
        sqlx::query_scalar("SELECT person_ci_id FROM users WHERE id = $1").bind(erin).fetch_one(&w.pool).await.unwrap();
    let person = person.expect("a user gets a Person");
    let ci = id(&w
        .ok(
            "POST",
            "/api/v1/configuration-items",
            json!({ "classId": w.server, "attributes": { "owner": person } }),
            201,
        )
        .await);
    let service_class: Uuid = sqlx::query_scalar("SELECT id FROM ci_classes WHERE system_role = 'business_service'")
        .fetch_one(&w.pool)
        .await
        .unwrap();
    let service = id(&w
        .ok(
            "POST",
            "/api/v1/configuration-items",
            json!({ "classId": service_class, "attributes": { "name": "Payments" } }),
            201,
        )
        .await);
    let (status, v) = w
        .call("POST", &format!("/api/v1/business-services/{service}/members"), Some(json!({ "memberIds": [ci] })))
        .await;
    assert!(status == 200 || status == 201 || status == 204, "{v}");
    sqlx::query(
        "INSERT INTO business_service_owners (service_ci_id, role, user_id, position) VALUES ($1, 'technical', $2, 0)",
    )
    .bind(service)
    .bind(frank)
    .execute(&w.pool)
    .await
    .unwrap();

    let preview = format!("{BASE}/{def}/approvers/preview");
    let (status, v) =
        w.call("GET", &format!("{preview}?transition=approve&step=cab&ciId={ci}&requestedBy={alice}"), None).await;
    assert_eq!(status, 200, "{v}");
    let users: Vec<(String, String, bool)> = v["users"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| {
            (
                u["username"].as_str().unwrap().to_owned(),
                u["reason"].as_str().unwrap().to_owned(),
                u["eligible"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        users,
        [
            ("erin", "eligible", true),
            ("frank", "eligible", true),
            ("dave", "escalation_only", false),
            ("alice", "excluded", false),
            ("bob", "no_view_right", false),
            ("carol", "inactive", false),
        ]
        .map(|(a, b, c)| (a.to_owned(), b.to_owned(), c)),
        "{v}"
    );
    assert_eq!(
        (v["eligibleCount"].as_i64(), v["requiredApprovals"].as_i64(), v["truncated"].as_bool()),
        (Some(2), Some(2), Some(false))
    );
    let bob_row = v["users"].as_array().unwrap().iter().find(|u| u["username"] == "bob").unwrap();
    assert!(bob_row["message"].as_str().unwrap().contains("view type server"), "{bob_row}");
    assert_eq!(bob_row["via"], json!(["approver: group CAB"]));
    let erin_row = v["users"].as_array().unwrap().iter().find(|u| u["username"] == "erin").unwrap();
    assert_eq!(erin_row["via"], json!(["approver: field server.owner"]));
    let sources: Vec<(String, i64)> = v["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["source"].as_str().unwrap().to_owned(), s["userCount"].as_i64().unwrap()))
        .collect();
    assert_eq!(
        sources,
        [("group", 3), ("ci_attribute", 1), ("service_owner", 1), ("user", 1)].map(|(a, b)| (a.to_owned(), b))
    );

    // Without a CI: the CI-dependent sources are not resolved, and say so; nobody is excluded.
    let (status, v) = w.call("GET", &format!("{preview}?transition=approve&step=cab"), None).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["eligibleCount"].as_i64(), Some(1), "alice only: {v}");
    let notes: Vec<&Value> = v["sources"].as_array().unwrap().iter().map(|s| &s["note"]).collect();
    assert!(notes[1].as_str().unwrap().contains("ciId") && notes[2].as_str().unwrap().contains("ciId"), "{v}");

    // A CI without an owner or service: the sources tell why they are empty.
    let bare = id(&w.ok("POST", "/api/v1/configuration-items", json!({ "classId": w.server }), 201).await);
    let (_, v) = w.call("GET", &format!("{preview}?transition=approve&step=cab&ciId={bare}"), None).await;
    let notes: Vec<String> =
        v["sources"].as_array().unwrap().iter().map(|s| s["note"].as_str().unwrap_or("").to_owned()).collect();
    assert!(notes[1].contains("field owner is empty") && notes[2].contains("not a direct member"), "{notes:?}");

    // Refusals: an unknown step, a CI of another type, a missing CI.
    let (status, v) = w.call("GET", &format!("{preview}?transition=approve&step=nope"), None).await;
    assert_eq!((status, details(&v)), (400, pairs(&[("step", "unknown_step")])), "{v}");
    let (status, v) = w.call("GET", &format!("{preview}?transition=approve&step=cab&ciId={service}"), None).await;
    assert_eq!((status, details(&v)), (400, pairs(&[("ciId", "not_covered")])), "{v}");
    let (status, v) =
        w.call("GET", &format!("{preview}?transition=approve&step=cab&ciId={}", Uuid::new_v4()), None).await;
    assert_eq!((status, code(&v)), (404, "NOT_FOUND"), "{v}");
    db.drop().await;
}

async fn export(app: &Router, creds: &Creds) -> Value {
    let (status, v, _) = call(app, "GET", "/api/v1/admin/config/export", creds, None).await;
    assert_eq!(status, 200, "{v}");
    v
}

async fn import(app: &Router, creds: &Creds, file: &Value) -> (u16, Value) {
    let (status, v, _) = call(app, "POST", "/api/v1/admin/config/import?mode=apply", creds, Some(file.clone())).await;
    (status, v)
}

/// Format 9 (§11): export → import into an empty install round-trips the
/// approval policy and the approvers; an equal file is a no-op; a changed
/// policy publishes a new version; a missing group fails the import with
/// nothing written; a format 8 file still imports.
#[tokio::test]
async fn approvals_round_trip_through_the_configuration_file() {
    let Some(src) = scratch::database("approvals_config_src").await else { return };
    let Some(dst) = scratch::database("approvals_config_dst").await else { return };
    let w = World::new(&src).await;
    let def = w.workflow("change", gated_graph()).await;
    w.publish(def).await;
    let engineers = w.profile("Platform engineers", true).await;
    let head = w.user("head.of.ops", &[engineers]).await;
    w.group("CAB", &[head]).await;
    let (status, v) = w
        .set_approvers(
            def,
            json!([
                { "transitionKey": "approve", "stepKey": "tech", "source": "profile", "profile": "Platform engineers" },
                { "transitionKey": "approve", "stepKey": "tech", "source": "ci_attribute", "attribute": "owner" },
                { "transitionKey": "approve", "stepKey": "cab", "source": "group", "group": "CAB" },
                { "transitionKey": "approve", "stepKey": "cab", "role": "escalation", "source": "user", "user": "head.of.ops" },
                { "transitionKey": "approve", "stepKey": "cab", "source": "service_owner", "serviceOwnerRole": "business" }
            ]),
        )
        .await;
    assert_eq!(status, 200, "{v}");

    let file = export(&w.app, &w.admin).await;
    assert_eq!(file["formatVersion"], crate::modules::config_transfer::format::FORMAT_VERSION);
    let flow = &file["workflows"][0];
    assert_eq!(flow["graph"]["transitions"][0]["approval"]["steps"][0]["dueAfter"], "P2D");
    assert_eq!(
        flow["approvers"],
        json!([
            { "transition": "approve", "step": "cab", "role": "approver", "group": "CAB" },
            { "transition": "approve", "step": "cab", "role": "approver", "serviceOwner": "business" },
            { "transition": "approve", "step": "cab", "role": "escalation", "user": "head.of.ops" },
            { "transition": "approve", "step": "tech", "role": "approver", "profile": "Platform engineers" },
            { "transition": "approve", "step": "tech", "role": "approver", "attribute": "server.owner" }
        ])
    );
    let v1_sum: Vec<u8> = sqlx::query_scalar("SELECT checksum FROM workflow_versions WHERE status = 'published'")
        .fetch_one(&w.pool)
        .await
        .unwrap();

    // Into an empty install: the group and the user are not in the file, so the import fails and writes nothing.
    let dw = {
        let app = app(dst.pool.clone());
        let password = format!("test passphrase {}", Uuid::new_v4());
        let setup = json!({ "username": "admin", "email": "admin@example.test", "displayName": "Admin",
            "password": password, "setupToken": crate::auth::setup_token::TEST_TOKEN });
        let (status, me, headers) = call(&app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
        assert_eq!(status, 201, "{me}");
        World {
            app,
            pool: dst.pool.clone(),
            admin: session_of(&me, &headers),
            password,
            server: Uuid::nil(),
            lifecycle: Uuid::nil(),
        }
    };
    let tables = "SELECT (SELECT count(*) FROM ci_classes) + (SELECT count(*) FROM workflow_definitions)
                  + (SELECT count(*) FROM permission_profiles) + (SELECT count(*) FROM audit_log)";
    let before = dw.count(tables).await;
    let (status, v) = import(&dw.app, &dw.admin, &file).await;
    assert_eq!((status, code(&v)), (400, "VALIDATION_ERROR"), "{v}");
    assert_eq!(
        details(&v),
        pairs(&[("workflows.0.approvers.0.group", "not_found"), ("workflows.0.approvers.2.user", "not_found")]),
        "{v}"
    );
    assert!(v["error"]["message"].as_str().unwrap().contains("nothing was imported"), "{v}");
    assert_eq!(dw.count(tables).await, before, "nothing written");

    // With the group and the user in place, the import creates the workflow, its policy and its approvers.
    let ops = dw.profile("Ops", false).await;
    let head2 = dw.user("Head.Of.Ops", &[ops]).await;
    dw.group("cab", &[head2]).await;
    let (status, res) = import(&dw.app, &dw.admin, &file).await;
    assert_eq!(status, 200, "{res}");
    let sum: Vec<u8> = sqlx::query_scalar("SELECT checksum FROM workflow_versions WHERE status = 'published'")
        .fetch_one(&dst.pool)
        .await
        .unwrap();
    assert_eq!(sum, v1_sum, "the same policy has the same checksum on both installs");
    let back = export(&dw.app, &dw.admin).await;
    let names = |f: &Value| -> Value {
        // Names are matched regardless of case; the install's own spelling is exported.
        let mut f = f["workflows"].clone();
        for a in f[0]["approvers"].as_array_mut().unwrap() {
            for k in ["group", "user"] {
                if let Some(n) = a[k].as_str() {
                    a[k] = json!(n.to_lowercase());
                }
            }
        }
        f
    };
    assert_eq!(names(&back), names(&file), "round trip");
    assert_eq!(back["workflows"][0]["approvers"][0]["group"], "cab");

    // An equal file is a no-op.
    let counts = "SELECT (SELECT count(*) FROM audit_log) * 1000 + (SELECT count(*) FROM workflow_versions)";
    let n = dw.count(counts).await;
    let (status, res) = import(&dw.app, &dw.admin, &file).await;
    assert_eq!(status, 200, "{res}");
    assert_eq!(dw.count(counts).await, n);
    assert!(res["changes"].as_array().unwrap().iter().all(|c| c["section"] != "workflows"), "{res}");

    // A changed policy publishes a new version; changed approvers alone do not.
    let mut changed = file.clone();
    changed["workflows"][0]["graph"]["transitions"][0]["approval"]["steps"][1]["requiredApprovals"] = json!(3);
    let (status, res) = import(&dw.app, &dw.admin, &changed).await;
    assert_eq!(status, 200, "{res}");
    let change = res["changes"].as_array().unwrap().iter().find(|c| c["section"] == "workflows").unwrap().clone();
    let fields: Vec<&str> = change["fields"].as_array().unwrap().iter().map(|f| f["field"].as_str().unwrap()).collect();
    assert_eq!(fields, ["graph"], "{change}");
    assert_eq!(change["fields"][0]["to"]["versionNo"], 2);
    let mut staffed = changed.clone();
    staffed["workflows"][0]["approvers"].as_array_mut().unwrap().remove(1);
    let versions = dw.count("SELECT count(*) FROM workflow_versions").await;
    let (status, res) = import(&dw.app, &dw.admin, &staffed).await;
    assert_eq!(status, 200, "{res}");
    let change = res["changes"].as_array().unwrap().iter().find(|c| c["section"] == "workflows").unwrap().clone();
    assert_eq!(change["fields"][0]["field"], "approvers", "{change}");
    assert_eq!(dw.count("SELECT count(*) FROM workflow_versions").await, versions);
    assert_eq!(dw.count("SELECT count(*) FROM workflow_approval_assignments").await, 4);

    // An approver on a step no version has, and a field that is not a Person reference, fail the import.
    let mut stray = staffed.clone();
    stray["workflows"][0]["approvers"][0]["step"] = json!("ghost");
    let (status, v) = import(&dw.app, &dw.admin, &stray).await;
    assert_eq!((status, details(&v)), (400, pairs(&[("workflows.0.approvers.0.step", "unknown_step")])), "{v}");
    let mut typed = staffed.clone();
    typed["workflows"][0]["approvers"][0] =
        json!({ "transition": "approve", "step": "cab", "attribute": "server.owner_team" });
    let (status, v) = import(&dw.app, &dw.admin, &typed).await;
    assert_eq!((status, details(&v)), (400, pairs(&[("workflows.0.approvers.0.attribute", "attribute_type")])), "{v}");
    let mut two = staffed.clone();
    two["workflows"][0]["approvers"][0]["user"] = json!("head.of.ops");
    let (status, v) = import(&dw.app, &dw.admin, &two).await;
    assert_eq!((status, details(&v)), (400, pairs(&[("workflows.0.approvers.0", "source")])), "{v}");
    assert_eq!(dw.count("SELECT count(*) FROM workflow_approval_assignments").await, 4, "nothing changed");

    // A format 8 file (no policy, no approvers) still imports, and its graph keeps the format 8 checksum.
    let mut v8 = file.clone();
    v8["formatVersion"] = json!(8);
    v8["workflows"][0]["key"] = json!("plain");
    v8["workflows"][0]["stateAttribute"] = Value::Null;
    v8["workflows"][0]["isActive"] = json!(false);
    for s in v8["workflows"][0]["graph"]["states"].as_array_mut().unwrap() {
        s["stateValue"] = Value::Null;
    }
    v8["workflows"][0]["graph"]["transitions"][0].as_object_mut().unwrap().remove("approval");
    v8["workflows"][0].as_object_mut().unwrap().remove("approvers");
    v8.as_object_mut().unwrap().remove("uiSettings");
    let (status, res) = import(&dw.app, &dw.admin, &v8).await;
    assert_eq!((status, res["applied"].as_bool()), (200, Some(true)), "{res}");
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM workflow_transition_approval_steps s JOIN workflow_transitions t ON t.id = s.transition_id
         JOIN workflow_versions v ON v.id = t.version_id JOIN workflow_definitions d ON d.id = v.definition_id
         WHERE d.key = 'plain'",
    )
    .fetch_one(&dst.pool)
    .await
    .unwrap();
    assert_eq!(n, 0);

    src.drop().await;
    dst.drop().await;
}

/// QA edge cases of the approvals designer's API (SHAA-2849, #837): a policy
/// without steps or with a zero quorum is refused; one person named through
/// a user, a group and a profile counts once towards a quorum; approvers who
/// may not view the type, disabled ones and deleted ones are warned about
/// by the lint and explained by the preview; a deleted group or user takes
/// its assignments with it and the step is reported unstaffed.
#[tokio::test]
async fn the_approver_lint_and_preview_at_their_edges() {
    let Some(db) = scratch::database("approvals_designer_edges").await else { return };
    let w = World::new(&db).await;
    let def = w.workflow("change", gated_graph()).await;
    let draft = format!("{BASE}/{def}/draft");
    let approvers = format!("{BASE}/{def}/approvers");
    let preview = format!("{approvers}/preview");

    // Policies the designer must never save.
    for (steps, field) in [
        (json!([]), "transitions.0.approval.steps"),
        (json!([{ "key": "tech", "name": "Tech", "requiredApprovals": 0 }]), "requiredApprovals"),
        (json!([{ "key": "tech", "name": "Tech", "requiredApprovals": -1 }]), "requiredApprovals"),
        (json!([{ "key": "tech", "name": "Tech", "requiredApprovals": "2" }]), ""),
        (json!([{ "key": "", "name": "Tech" }]), "key"),
        (json!([{ "key": "tech", "name": "" }]), "name"),
    ] {
        let mut bad = gated_graph();
        bad["transitions"][0]["approval"]["steps"] = steps.clone();
        let (status, v) = w.call("PUT", &draft, Some(bad)).await;
        assert_eq!(status, 400, "{steps}: {v}");
        assert!(details(&v).iter().any(|(f, _)| f.contains(field)), "{steps}: {field} in {v}");
    }
    let (_, v) = w.call("GET", &draft, None).await;
    assert_eq!(v["transitions"][0]["approval"]["steps"].as_array().map(Vec::len), Some(2), "nothing stored: {v}");
    w.publish(def).await;

    let solo = w.profile("Solo", true).await;
    let blind = w.profile("Blind", false).await;
    let viewers = w.profile("Viewers", true).await;
    let alice = w.user("alice", &[solo]).await;
    let bob = w.user("bob", &[blind]).await;
    let carol = w.user("carol", &[viewers]).await;
    let dave = w.user("dave", &[viewers]).await;
    w.group("CAB", &[alice]).await;
    let ops = w.group("Ops", &[dave]).await;
    sqlx::query("UPDATE users SET is_active = false WHERE id = $1").bind(carol).execute(&w.pool).await.unwrap();

    // alice three times over: one approver for a quorum of 2. bob may not view servers; carol is disabled.
    let (status, v) = w
        .set_approvers(
            def,
            json!([
                { "transitionKey": "approve", "stepKey": "cab", "source": "user", "user": "alice" },
                { "transitionKey": "approve", "stepKey": "cab", "source": "group", "group": "CAB" },
                { "transitionKey": "approve", "stepKey": "cab", "source": "profile", "profile": "Solo" },
                { "transitionKey": "approve", "stepKey": "tech", "source": "user", "user": "bob" },
                { "transitionKey": "approve", "stepKey": "tech", "source": "user", "user": "carol" },
                { "transitionKey": "go_live", "stepKey": "tech", "source": "user", "user": "dave" }
            ]),
        )
        .await;
    assert_eq!(status, 400, "a step go_live does not have: {v}");
    assert_eq!(details(&v), pairs(&[("approvers[5].stepKey", "unknown_step")]), "{v}");
    let (status, v) = w
        .set_approvers(
            def,
            json!([
                { "transitionKey": "approve", "stepKey": "cab", "source": "user", "user": "alice" },
                { "transitionKey": "approve", "stepKey": "cab", "source": "group", "group": "CAB" },
                { "transitionKey": "approve", "stepKey": "cab", "source": "profile", "profile": "Solo" },
                { "transitionKey": "approve", "stepKey": "tech", "source": "user", "user": "bob" },
                { "transitionKey": "approve", "stepKey": "tech", "source": "user", "user": "carol" },
                { "transitionKey": "approve", "stepKey": "tech", "role": "escalation", "source": "group", "group": "Ops" }
            ]),
        )
        .await;
    assert_eq!(status, 200, "warnings never refuse a save: {v}");
    let codes = problem_codes(&v);
    let has = |path: &str, code: &str| codes.iter().any(|c| c.0 == path && c.1 == code && c.2 == "warning");
    assert!(has("transitions.approve.steps.cab", "understaffed"), "alice counts once: {codes:?}");
    assert!(!has("transitions.approve.steps.cab", "approvers_cannot_view"), "{codes:?}");
    assert!(has("transitions.approve.steps.tech", "approvers_cannot_view"), "bob and carol: {codes:?}");
    assert!(has("transitions.approve.steps.tech", "understaffed"), "an escalation never staffs a step: {codes:?}");
    let tech_warnings = codes.iter().filter(|c| c.0 == "transitions.approve.steps.tech").count();
    assert_eq!(tech_warnings, 3, "one per blind source and one understaffed: {codes:?}");

    let names = |v: &Value| -> Vec<(String, String)> {
        v["users"]
            .as_array()
            .unwrap()
            .iter()
            .map(|u| (u["username"].as_str().unwrap().to_owned(), u["reason"].as_str().unwrap().to_owned()))
            .collect()
    };
    let (status, v) = w.call("GET", &format!("{preview}?transition=approve&step=cab"), None).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(names(&v), pairs(&[("alice", "eligible")]), "listed once: {v}");
    assert_eq!((v["eligibleCount"].as_i64(), v["requiredApprovals"].as_i64()), (Some(1), Some(2)));
    let mut via: Vec<&str> = v["users"][0]["via"].as_array().unwrap().iter().map(|s| s.as_str().unwrap()).collect();
    via.sort_unstable();
    assert_eq!(via.len(), 3, "every source that names her: {via:?}");
    let (status, v) = w.call("GET", &format!("{preview}?transition=approve&step=tech"), None).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        names(&v),
        pairs(&[("dave", "escalation_only"), ("bob", "no_view_right"), ("carol", "inactive")]),
        "{v}"
    );
    assert_eq!(v["eligibleCount"].as_i64(), Some(0));

    // Preview refusals: a requester who does not exist or is not an id, a missing step.
    let (status, v) =
        w.call("GET", &format!("{preview}?transition=approve&step=cab&requestedBy={}", Uuid::new_v4()), None).await;
    assert!(status == 200 || status == 400 || status == 404, "{status} {v}");
    assert_ne!(status, 500, "{v}");
    let (status, v) = w.call("GET", &format!("{preview}?transition=approve&step=cab&requestedBy=alice"), None).await;
    assert_eq!(status, 400, "{v}");
    let (status, v) = w.call("GET", &format!("{preview}?transition=approve"), None).await;
    assert_eq!(status, 400, "{v}");
    let (status, v) = w.call("GET", &format!("{preview}?transition=nope&step=cab"), None).await;
    assert_eq!((status, details(&v)), (400, pairs(&[("step", "unknown_step")])), "{v}");

    // Deleting the escalation group and alice takes their assignments with them; the lint says what is left.
    let (status, v) = w.call("DELETE", &format!("/api/v1/admin/groups/{ops}"), None).await;
    assert!(status == 200 || status == 204, "{v}");
    let (status, v) = w.call("DELETE", &format!("/api/v1/admin/users/{alice}"), None).await;
    assert!(status == 200 || status == 204, "{v}");
    let (status, v) = w.call("GET", &approvers, None).await;
    assert_eq!(status, 200, "{v}");
    let mut left: Vec<(&str, &str)> = v["approvers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| (a["stepKey"].as_str().unwrap(), a["source"].as_str().unwrap()))
        .collect();
    left.sort_unstable();
    assert_eq!(left, [("cab", "group"), ("cab", "profile"), ("tech", "user"), ("tech", "user")], "{v}");
    let codes = problem_codes(&v);
    assert!(
        codes.iter().any(|c| c.0 == "transitions.approve.steps.cab" && c.1 == "approvers_cannot_view"),
        "CAB and Solo are empty now: {codes:?}"
    );
    let (status, v) = w.call("GET", &format!("{preview}?transition=approve&step=cab"), None).await;
    assert_eq!((status, v["eligibleCount"].as_i64()), (200, Some(0)), "{v}");
    let _ = (bob, dave);
    db.drop().await;
}
