//! The layout grid (section widths, rows, field grids up to 12 columns) through the real router.

use axum::http::header;
use serde_json::{Value, json};

use crate::db::scratch;
use crate::modules::api_tokens::tests::{Creds, app, call, code};

/// One tab, in the current layout format (it shows the panels it places, and no others).
fn layout(sections: Value) -> Value {
    json!({"layoutFormat": 3, "layouts": [{"classKey": "server", "tabs": [{"key": "t", "label": "T", "sections": sections}]}]})
}

fn errors(v: &Value) -> Vec<&str> {
    v["error"]["details"].as_array().unwrap().iter().map(|d| d["field"].as_str().unwrap()).collect()
}

#[tokio::test]
async fn sections_side_by_side_are_validated_and_stored() {
    let Some(db) = scratch::database("sections_side_by_side_are_validated_and_stored").await else { return };
    let app = app(db.pool.clone());

    let setup = json!({ "username": "owner", "email": "owner@example.test", "displayName": "Owner", "password": "correct horse battery", "setupToken": crate::auth::setup_token::TEST_TOKEN });
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

    // Two half-width sections, then one forced onto a new row; an old-style section keeps its meaning. Each
    // becomes a window where the grid puts it (SHAA-1471: every tab is free).
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
             "fields": [{"field": "ident", "width": 12}], "frame": {"x": 0.0, "y": 0, "w": 0.5, "h": 192, "z": 1}},
            {"key": "b", "label": "B", "columns": 4, "width": 6, "collapsed": false,
             "fields": [{"field": "label", "width": 4}], "frame": {"x": 0.5, "y": 0, "w": 0.5, "h": 96, "z": 2}},
            {"key": "c", "label": "C", "columns": 3, "width": 6, "newRow": true, "collapsed": false, "fields": [],
             "frame": {"x": 0.0, "y": 208, "w": 0.5, "h": 96, "z": 3}},
            {"key": "d", "label": "D", "columns": 2, "width": 12, "collapsed": false,
             "fields": [{"field": "validFrom", "width": 1}], "frame": {"x": 0.0, "y": 320, "w": 1.0, "h": 96, "z": 4}},
        ])
    );
    assert_eq!(stored["settings"]["layouts"][0]["tabs"][0]["placement"], "free");
    // The row holds the same layout the version endpoint shows, in the class's template (SHAA-1472).
    let raw: Value = sqlx::query_scalar("SELECT settings FROM ui_settings").fetch_one(&db.pool).await.unwrap();
    assert_eq!(raw["layouts"][0], json!({"classKey": "server", "templateKey": "server"}));
    assert_eq!(raw["layoutTemplates"][1]["layout"]["tabs"], stored["settings"]["layouts"][0]["tabs"]);

    db.drop().await;
}

/// A saved version as stored (the effective settings leave out layouts of classes that do not exist).
async fn stored(app: &axum::Router, s: &Creds, version: i64) -> Value {
    let (status, v, _) = call(app, "GET", &format!("/api/v1/ui-settings/versions/{version}"), s, None).await;
    assert_eq!(status, 200, "{v}");
    v
}

/// Free tabs (SHAA-361): junk frames are refused, a converted tab is stored normalised, and the frames
/// are in the audit trail and survive export and import.
#[tokio::test]
async fn free_tabs_are_validated_normalised_audited_and_exported() {
    let Some(db) = scratch::database("free_tabs_are_validated_normalised_audited_and_exported").await else { return };
    let app = app(db.pool.clone());

    let setup = json!({ "username": "owner", "email": "owner@example.test", "displayName": "Owner", "password": "correct horse battery", "setupToken": crate::auth::setup_token::TEST_TOKEN });
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
    let version = current["version"].as_i64().unwrap();
    let free = |sections: Value| {
        json!({"layoutFormat": 3, "layouts": [{"classKey": "server", "tabs": [
            {"key": "t", "label": "T", "placement": "free", "sections": sections}]}]})
    };

    // Out of range on the schema, and windows that do not fit the tab.
    let bad = free(json!([
        {"key": "a", "label": "A", "frame": {"x": -0.1, "y": 0, "w": 0.01, "h": 20, "z": 1}},
        {"key": "b", "label": "B", "frame": {"x": 0.5, "y": 1.5, "w": 1.2, "h": 100, "z": -1, "minH": 10}},
        {"key": "c", "label": "C", "frame": {"x": 0.6, "y": 0, "w": 0.5, "h": 100, "z": 1, "minH": 200}},
    ]));
    let (status, v, _) =
        call(&app, "PUT", "/api/v1/ui-settings", &s, Some(json!({"version": version, "settings": bad}))).await;
    assert_eq!((status, code(&v)), (400, "VALIDATION_ERROR"), "{v}");
    let mut got = errors(&v);
    got.sort_unstable();
    assert_eq!(
        got,
        [
            "settings.layouts.0.tabs.0.sections.0.frame.h",
            "settings.layouts.0.tabs.0.sections.0.frame.w",
            "settings.layouts.0.tabs.0.sections.0.frame.x",
            "settings.layouts.0.tabs.0.sections.1.frame.minH",
            "settings.layouts.0.tabs.0.sections.1.frame.w",
            "settings.layouts.0.tabs.0.sections.1.frame.y",
            "settings.layouts.0.tabs.0.sections.1.frame.z",
        ],
        "{v}"
    );
    let bad =
        free(json!([{"key": "c", "label": "C", "frame": {"x": 0.6, "y": 0, "w": 0.5, "h": 100, "z": 1, "minH": 200}}]));
    let (status, v, _) =
        call(&app, "PUT", "/api/v1/ui-settings", &s, Some(json!({"version": version, "settings": bad}))).await;
    assert_eq!(
        (status, errors(&v)),
        (400, vec!["settings.layouts.0.tabs.0.sections.0.frame.w", "settings.layouts.0.tabs.0.sections.0.frame.minH"]),
        "{v}"
    );

    // A grid tab switched to free without frames: frames from the grid positions, in reading order.
    let good = free(json!([
        {"key": "a", "label": "A", "width": 6},
        {"key": "b", "label": "B", "width": 6, "frame": {"x": 0.25, "y": 40, "w": 0.75, "h": 300, "z": 9, "minH": 100}},
        {"key": "r", "label": "Relationships", "kind": "relations"},
    ]));
    let (status, v, _) =
        call(&app, "PUT", "/api/v1/ui-settings", &s, Some(json!({"version": version, "settings": good}))).await;
    assert_eq!(status, 200, "{v}");
    let version = v["version"].as_i64().unwrap();
    let v = stored(&app, &s, version).await;
    let tab = &v["settings"]["layouts"][0]["tabs"][0];
    assert_eq!(tab["placement"], "free");
    let got: Vec<(&str, &Value)> =
        tab["sections"].as_array().unwrap().iter().map(|s| (s["key"].as_str().unwrap(), &s["frame"])).collect();
    assert_eq!(
        got,
        [
            ("b", &json!({"x": 0.25, "y": 40, "w": 0.75, "h": 300, "z": 1, "minH": 100})),
            ("a", &json!({"x": 0.0, "y": 356, "w": 0.5, "h": 96, "z": 2})),
            ("r", &json!({"x": 0.0, "y": 468, "w": 1.0, "h": 320, "z": 3})),
        ],
        "{v}"
    );
    let saved = v["settings"].clone();

    // The audit row holds the stored form: the layout is in the class's template (SHAA-1472).
    let (status, log, _) = call(&app, "GET", "/api/v1/audit-log?entityType=ui_settings&limit=1", &s, None).await;
    assert_eq!(status, 200, "{log}");
    let audited = &log["data"][0]["newValue"]["settings"];
    assert_eq!(audited["layoutTemplates"][1]["layout"]["tabs"], saved["layouts"][0]["tabs"], "{log}");

    // Export, reset, import: the free tab comes back as it was.
    let (status, file, _) = call(&app, "GET", "/api/v1/admin/config/export", &s, None).await;
    assert_eq!(status, 200, "{file}");
    assert_eq!(file["uiSettings"]["settings"]["layoutTemplates"][1]["layout"]["tabs"], saved["layouts"][0]["tabs"]);
    let (status, v, _) =
        call(&app, "PUT", "/api/v1/ui-settings", &s, Some(json!({"version": version, "settings": {}}))).await;
    assert_eq!(status, 200, "{v}");
    let (status, v, _) = call(&app, "POST", "/api/v1/admin/config/import?mode=apply", &s, Some(file)).await;
    assert_eq!(status, 200, "{v}");
    let (_, current, _) = call(&app, "GET", "/api/v1/ui-settings", &s, None).await;
    let version = current["version"].as_i64().unwrap();
    assert_eq!(stored(&app, &s, version).await["settings"]["layouts"], saved["layouts"]);

    // "grid" is still accepted (SHAA-1471): the tab stays free with its windows where they were.
    let mut grid = saved.clone();
    grid["layouts"][0]["tabs"][0]["placement"] = json!("grid");
    let (status, v, _) =
        call(&app, "PUT", "/api/v1/ui-settings", &s, Some(json!({"version": version, "settings": grid}))).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["version"].as_i64().unwrap(), version, "the same document: no new version");
    assert_eq!(stored(&app, &s, version).await["settings"]["layouts"], saved["layouts"]);

    // A grid layout stored before free placement was the only one reads back free, every section a window
    // where it was on the grid; so does an older export holding one when it is imported.
    let old = layout(json!([
        {"key": "a", "label": "A", "width": 6, "fields": [{"field": "ident"}]},
        {"key": "b", "label": "B", "width": 6, "fields": [{"field": "label"}]},
        {"key": "c", "label": "C", "fields": [{"field": "validFrom"}]},
    ]));
    let expected = json!([
        {"key": "a", "frame": {"x": 0.0, "y": 0, "w": 0.5, "h": 96, "z": 1}},
        {"key": "b", "frame": {"x": 0.5, "y": 0, "w": 0.5, "h": 96, "z": 2}},
        {"key": "c", "frame": {"x": 0.0, "y": 112, "w": 1.0, "h": 96, "z": 3}},
    ]);
    let frames_of = |settings: &Value| {
        let tab = &settings["layouts"][0]["tabs"][0];
        assert_eq!(tab["placement"], "free", "{tab}");
        Value::Array(
            tab["sections"].as_array().unwrap().iter().map(|s| json!({"key": s["key"], "frame": s["frame"]})).collect(),
        )
    };
    sqlx::query(
        "INSERT INTO ui_settings_versions (version, settings, actor_type) \
         SELECT max(version) + 1, $1, 'user' FROM ui_settings_versions",
    )
    .bind(&old)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE ui_settings SET version = (SELECT max(version) FROM ui_settings_versions), settings = $1")
        .bind(&old)
        .execute(&db.pool)
        .await
        .unwrap();
    let version = version + 1;
    assert_eq!(frames_of(&stored(&app, &s, version).await["settings"]), expected);
    let (status, mut file, _) = call(&app, "GET", "/api/v1/admin/config/export", &s, None).await;
    assert_eq!(status, 200, "{file}");
    assert_eq!(frames_of(&file["uiSettings"]["settings"]), expected);

    file["uiSettings"]["settings"] = old.clone();
    let (status, v, _) =
        call(&app, "PUT", "/api/v1/ui-settings", &s, Some(json!({"version": version, "settings": {}}))).await;
    assert_eq!(status, 200, "{v}");
    let (status, v, _) = call(&app, "POST", "/api/v1/admin/config/import?mode=apply", &s, Some(file)).await;
    assert_eq!(status, 200, "{v}");
    let (_, current, _) = call(&app, "GET", "/api/v1/ui-settings", &s, None).await;
    let version = current["version"].as_i64().unwrap();
    let (raw,): (Value,) = sqlx::query_as("SELECT settings FROM ui_settings").fetch_one(&db.pool).await.unwrap();
    // Stored in the class's template (SHAA-1472).
    let template = raw["layoutTemplates"].as_array().unwrap().iter().find(|t| t["key"] == "server").unwrap();
    assert_eq!(frames_of(&json!({"layouts": [template["layout"]]})), expected, "stored free");
    assert_eq!(frames_of(&stored(&app, &s, version).await["settings"]), expected);

    db.drop().await;
}
