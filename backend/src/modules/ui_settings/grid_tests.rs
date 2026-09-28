//! The layout grid (section widths, rows, field grids up to 12 columns) through the real router.

use axum::http::header;
use serde_json::{Value, json};

use crate::db::scratch;
use crate::modules::api_tokens::tests::{Creds, app, call, code};

fn layout(sections: Value) -> Value {
    json!({"layouts": [{"classKey": "server", "tabs": [{"key": "t", "label": "T", "sections": sections}]}]})
}

fn errors(v: &Value) -> Vec<&str> {
    v["error"]["details"].as_array().unwrap().iter().map(|d| d["field"].as_str().unwrap()).collect()
}

#[tokio::test]
async fn sections_side_by_side_are_validated_and_stored() {
    let Some(db) = scratch::database("sections_side_by_side_are_validated_and_stored").await else { return };
    let app = app(db.pool.clone());

    let setup = json!({ "username": "owner", "displayName": "Owner", "password": "correct horse battery" });
    let (status, me, headers) = call(&app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
    assert_eq!(status, 201, "{me}");
    let cookie = headers
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned())
        .collect::<Vec<_>>()
        .join("; ");
    let s = Creds { cookie: Some(cookie), csrf: me["csrfToken"].as_str().map(str::to_owned), bearer: None };
    let (_, current, _) = call(&app, "GET", "/api/v1/ui-settings", &s, None).await;
    let mut version = current["version"].as_i64().unwrap();

    // Out of range on the schema, and a field wider than its section's grid.
    let bad = layout(json!([
        {"key": "a", "label": "A", "width": 13},
        {"key": "b", "label": "B", "width": 0, "columns": 13},
        {"key": "c", "label": "C", "minHeight": 0, "fields": [{"field": "ident", "width": 13}]},
    ]));
    let (status, v, _) =
        call(&app, "PUT", "/api/v1/ui-settings", &s, Some(json!({"version": version, "settings": bad}))).await;
    assert_eq!((status, code(&v)), (400, "VALIDATION_ERROR"), "{v}");
    let mut got = errors(&v);
    got.sort_unstable();
    assert_eq!(
        got,
        [
            "settings.layouts.0.tabs.0.sections.0.width",
            "settings.layouts.0.tabs.0.sections.1.columns",
            "settings.layouts.0.tabs.0.sections.1.width",
            "settings.layouts.0.tabs.0.sections.2.fields.0.width",
            "settings.layouts.0.tabs.0.sections.2.minHeight",
        ],
        "{v}"
    );
    let bad = layout(json!([{"key": "a", "label": "A", "columns": 6, "fields": [{"field": "ident", "width": 7}]}]));
    let (status, v, _) =
        call(&app, "PUT", "/api/v1/ui-settings", &s, Some(json!({"version": version, "settings": bad}))).await;
    assert_eq!((status, errors(&v)), (400, vec!["settings.layouts.0.tabs.0.sections.0.fields.0.width"]), "{v}");

    // Two half-width sections, then one forced onto a new row; an old-style section keeps its meaning.
    let good = layout(json!([
        {"key": "a", "label": "A", "width": 6, "columns": 12, "minHeight": 3,
         "fields": [{"field": "ident", "width": 12}]},
        {"key": "b", "label": "B", "width": 6, "columns": 4, "fields": [{"field": "label", "width": 4}]},
        {"key": "c", "label": "C", "width": 6, "newRow": true},
        {"key": "d", "label": "D", "columns": 2, "fields": [{"field": "validFrom"}]},
    ]));
    let (status, v, _) =
        call(&app, "PUT", "/api/v1/ui-settings", &s, Some(json!({"version": version, "settings": good}))).await;
    assert_eq!(status, 200, "{v}");
    version = v["version"].as_i64().unwrap();
    let (status, stored, _) = call(&app, "GET", &format!("/api/v1/ui-settings/versions/{version}"), &s, None).await;
    assert_eq!(status, 200, "{stored}");
    assert_eq!(
        stored["settings"]["layouts"][0]["tabs"][0]["sections"],
        json!([
            {"key": "a", "label": "A", "columns": 12, "width": 6, "minHeight": 3, "collapsed": false,
             "fields": [{"field": "ident", "width": 12}]},
            {"key": "b", "label": "B", "columns": 4, "width": 6, "collapsed": false,
             "fields": [{"field": "label", "width": 4}]},
            {"key": "c", "label": "C", "columns": 3, "width": 6, "newRow": true, "collapsed": false, "fields": []},
            {"key": "d", "label": "D", "columns": 2, "width": 12, "collapsed": false,
             "fields": [{"field": "validFrom", "width": 1}]},
        ])
    );
    // The row holds the same document the version endpoint shows.
    let raw: Value = sqlx::query_scalar("SELECT settings FROM ui_settings").fetch_one(&db.pool).await.unwrap();
    assert_eq!(raw["layouts"][0]["tabs"], stored["settings"]["layouts"][0]["tabs"]);

    db.drop().await;
}
