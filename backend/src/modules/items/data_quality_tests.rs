//! The data-quality checks (SHAA-2351) through the real router against
//! PostgreSQL: the per-type owner and end-of-life settings, the counts, their
//! drill-down filters and the caller's visibility. The pending_approval check
//! runs in `workflows::approvals_runtime_tests`, which raises a real request.

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::{TimeDelta, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

use super::schemas::DataQualityQuery;
use crate::api::context::RequestContext;
use crate::auth::permissions::{ClassRights, Permissions};
use crate::auth::{Credential, Principal};
use crate::db::scratch;
use crate::modules::api_tokens::tests::{Creds, app, call, session_of};

/// A user who may view only these classes.
fn viewer(classes: &[Uuid]) -> RequestContext {
    let permissions = Permissions {
        classes: classes.iter().map(|id| (*id, ClassRights { view: true, ..Default::default() })).collect(),
        ..Default::default()
    };
    let principal = Principal {
        user_id: Uuid::new_v4(),
        username: "viewer".into(),
        credential: Credential::Token { profile_id: None, creator_id: None, token_id: None, minted_by: None },
        permissions,
    };
    RequestContext::user(Arc::new(principal), "quality-viewer".into())
}

fn id(v: &Value) -> Uuid {
    v["id"].as_str().unwrap().parse().unwrap()
}

fn detail_fields(v: &Value) -> Vec<String> {
    v["error"]["details"]
        .as_array()
        .map(|d| d.iter().map(|e| e["field"].as_str().unwrap().to_owned()).collect())
        .unwrap_or_default()
}

/// (key, count, configured) per check.
fn counts(checks: &[super::schemas::DataQualityCheck]) -> Vec<(String, i64, bool)> {
    checks
        .iter()
        .map(|c| (serde_json::to_value(c.key).unwrap().as_str().unwrap().to_owned(), c.count, c.configured))
        .collect()
}

fn row(key: &str, count: i64, configured: bool) -> (String, i64, bool) {
    (key.to_owned(), count, configured)
}

#[tokio::test]
async fn data_quality_checks_count_and_drill_down() {
    let Some(db) = scratch::database("data_quality").await else { return };
    let pool = &db.pool;
    let app = app(pool.clone());
    let setup = json!({ "username": "owner", "email": "owner@example.test", "displayName": "Owner", "password": "correct horse battery", "setupToken": crate::auth::setup_token::TEST_TOKEN });
    let (status, me, headers) = call(&app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
    assert_eq!(status, 201, "{me}");
    let admin = session_of(&me, &headers);
    let post = |path: &'static str, body: Value| {
        let (app, admin) = (&app, &admin);
        async move {
            let (status, v, _) = call(app, "POST", path, admin, Some(body)).await;
            assert_eq!(status, 201, "{path}: {v}");
            id(&v)
        }
    };

    // srv (owner, eol) and its subtype vm (retire, its own end-of-life field); net has neither.
    let srv = post("/api/v1/ci-classes", json!({ "key": "srv", "name": "Srv" })).await;
    let vm = post("/api/v1/ci-classes", json!({ "key": "vm", "name": "VM", "parentId": srv })).await;
    let net = post("/api/v1/ci-classes", json!({ "key": "net", "name": "Net" })).await;
    let field =
        |class: Uuid, key: &str, ty: &str| json!({ "classId": class, "key": key, "label": key, "dataType": ty });
    let owner = post("/api/v1/attribute-definitions", field(srv, "owner", "text")).await;
    let eol = post("/api/v1/attribute-definitions", field(srv, "eol", "date")).await;
    let retire = post("/api/v1/attribute-definitions", field(vm, "retire", "datetime")).await;

    let today = Utc::now().date_naive();
    let date = |d: i64| (today + TimeDelta::days(d)).to_string();
    let datetime = |d: i64| format!("{}T12:00:00Z", today + TimeDelta::days(d));
    let ci = |class: Uuid, attrs: Value| {
        post("/api/v1/configuration-items", json!({ "classId": class, "attributes": attrs }))
    };
    let s1 = ci(srv, json!({ "owner": "ops", "eol": date(30) })).await;
    let s2 = ci(srv, json!({ "owner": "  ", "eol": date(200) })).await;
    let v1 = ci(vm, json!({ "eol": date(300), "retire": datetime(-5) })).await;
    let v2 = ci(vm, json!({ "owner": "dc", "eol": date(10) })).await;
    let n1 = ci(net, json!({})).await;
    let n2 = ci(net, json!({})).await;
    // A deleted CI without an owner counts nowhere.
    let gone = ci(srv, json!({})).await;
    let (status, v, _) = call(&app, "DELETE", &format!("/api/v1/configuration-items/{gone}"), &admin, None).await;
    assert!(status == 200 || status == 204, "{v}");

    // s1 -> n1.
    let rt: Uuid = sqlx::query_scalar(
        "INSERT INTO relationship_types (key, name, forward_label, reverse_label, is_directional)
         VALUES ('uses', 'Uses', 'uses', 'used by', true) RETURNING id",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO relationship_type_rules (relationship_type_id, source_class_id, target_class_id) VALUES ($1, $2, $3)")
        .bind(rt)
        .bind(srv)
        .bind(net)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO ci_relationships (relationship_type_id, source_ci_id, target_ci_id) VALUES ($1, $2, $3)")
        .bind(rt)
        .bind(s1)
        .bind(n1)
        .execute(pool)
        .await
        .unwrap();

    let all = viewer(&[srv, vm, net]);
    let q = DataQualityQuery { end_of_life_within_days: None };
    let dq = super::service::data_quality(pool, &all, &q).await.unwrap();
    // No owner or end-of-life field set yet: not configured, rather than a reassuring 0.
    assert_eq!(
        counts(&dq.checks),
        [
            row("no_owner", 0, false),
            row("end_of_life", 0, false),
            row("no_relationships", 4, true),
            row("pending_approval", 0, true)
        ]
    );

    // The settings refuse a field of the wrong type or outside the lineage.
    for (class, body, field) in [
        (srv, json!({ "ownerAttributeId": eol }), "ownerAttributeId"),
        (srv, json!({ "endOfLifeAttributeId": owner }), "endOfLifeAttributeId"),
        (net, json!({ "ownerAttributeId": owner }), "ownerAttributeId"),
        (srv, json!({ "endOfLifeAttributeId": retire }), "endOfLifeAttributeId"),
    ] {
        let (status, v, _) = call(&app, "PATCH", &format!("/api/v1/ci-classes/{class}"), &admin, Some(body)).await;
        assert!((400..500).contains(&status), "{status} {v}");
        assert_eq!(detail_fields(&v), [field], "{v}");
    }

    // Set on srv (vm inherits the owner field) and an end-of-life override on vm; audited.
    let body = json!({ "ownerAttributeId": owner, "endOfLifeAttributeId": eol });
    let (status, v, _) = call(&app, "PATCH", &format!("/api/v1/ci-classes/{srv}"), &admin, Some(body)).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!((v["ownerAttributeId"].clone(), v["endOfLifeAttributeId"].clone()), (json!(owner), json!(eol)));
    let body = json!({ "endOfLifeAttributeId": retire });
    let (status, v, _) = call(&app, "PATCH", &format!("/api/v1/ci-classes/{vm}"), &admin, Some(body)).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!((v["ownerAttributeId"].clone(), v["endOfLifeAttributeId"].clone()), (Value::Null, json!(retire)));
    let audited: Option<Value> = sqlx::query_scalar(
        "SELECT new_value FROM audit_log WHERE entity_id = $1 AND action = 'update' ORDER BY id DESC LIMIT 1",
    )
    .bind(srv)
    .fetch_optional(pool)
    .await
    .unwrap();
    let audited = audited.expect("an audit row for the class update");
    assert_eq!(
        (audited["ownerAttributeId"].clone(), audited["endOfLifeAttributeId"].clone()),
        (json!(owner), json!(eol)),
        "{audited}"
    );

    // no_owner: s2 (blank) and v1. end_of_life (90 days): s1 (eol) and v1 (retire, not its eol); v2's eol is
    // not vm's field. no_relationships: everything but s1 and n1.
    let dq = super::service::data_quality(pool, &all, &q).await.unwrap();
    assert_eq!(
        counts(&dq.checks),
        [
            row("no_owner", 2, true),
            row("end_of_life", 2, true),
            row("no_relationships", 4, true),
            row("pending_approval", 0, true)
        ]
    );
    let window = |days| DataQualityQuery { end_of_life_within_days: Some(days) };
    let dq = super::service::data_quality(pool, &all, &window(0)).await.unwrap();
    assert_eq!((dq.checks[1].count, dq.checks[1].filter.end_of_life_within_days), (1, Some(0)));
    let dq = super::service::data_quality(pool, &all, &window(365)).await.unwrap();
    assert_eq!(dq.checks[1].count, 3);

    // A viewer of srv and vm only: s1's one relationship leads to a CI it cannot see.
    let dq = super::service::data_quality(pool, &viewer(&[srv, vm]), &q).await.unwrap();
    assert_eq!(
        counts(&dq.checks),
        [
            row("no_owner", 2, true),
            row("end_of_life", 2, true),
            row("no_relationships", 4, true),
            row("pending_approval", 0, true)
        ]
    );
    // A viewer of net only: neither setting reaches a type it may view.
    let dq = super::service::data_quality(pool, &viewer(&[net]), &q).await.unwrap();
    assert_eq!(
        counts(&dq.checks),
        [
            row("no_owner", 0, false),
            row("end_of_life", 0, false),
            row("no_relationships", 2, true),
            row("pending_approval", 0, true)
        ]
    );

    // Through the router: each check's filter lists exactly the CIs it counted.
    let (status, v, _) = call(&app, "GET", "/api/v1/configuration-items/data-quality", &admin, None).await;
    assert_eq!(status, 200, "{v}");
    let mine: BTreeSet<Uuid> = [s1, s2, v1, v2, n1, n2].into();
    for check in v["checks"].as_array().unwrap() {
        let filter = check["filter"].as_object().unwrap();
        let mut path = format!("/api/v1/configuration-items?limit=200&quality={}", filter["quality"].as_str().unwrap());
        if let Some(days) = filter["endOfLifeWithinDays"].as_i64() {
            path.push_str(&format!("&endOfLifeWithinDays={days}"));
        }
        let (status, list, _) = call(&app, "GET", &path, &admin, None).await;
        assert_eq!(status, 200, "{list}");
        assert_eq!(list["page"]["total"], check["count"], "{path}: {list}");
        let listed: BTreeSet<Uuid> =
            list["data"].as_array().unwrap().iter().map(id).filter(|i| mine.contains(i)).collect();
        let expected: BTreeSet<Uuid> = match check["key"].as_str().unwrap() {
            "no_owner" => [s2, v1].into(),
            "end_of_life" => [s1, v1].into(),
            "no_relationships" => [s2, v1, v2, n2].into(),
            _ => BTreeSet::new(),
        };
        assert_eq!(listed, expected, "{path}");
    }
    let (status, v, _) =
        call(&app, "GET", "/api/v1/configuration-items/facets?quality=end_of_life&endOfLifeWithinDays=0", &admin, None)
            .await;
    assert_eq!(status, 200, "{v}");

    // Bad input is a 400 naming the parameter.
    for path in [
        "/api/v1/configuration-items/data-quality?endOfLifeWithinDays=-1",
        "/api/v1/configuration-items/data-quality?endOfLifeWithinDays=3651",
        "/api/v1/configuration-items?quality=end_of_life&endOfLifeWithinDays=-1",
        "/api/v1/configuration-items?quality=stale",
    ] {
        let (status, v, _) = call(&app, "GET", path, &admin, None).await;
        assert_eq!(status, 400, "{path}: {v}");
    }

    // A field named by a setting keeps a type that fits it.
    let body = json!({ "dataType": "number" });
    let (status, v, _) = call(&app, "PATCH", &format!("/api/v1/attribute-definitions/{eol}"), &admin, Some(body)).await;
    assert!((400..500).contains(&status), "{status} {v}");
}
