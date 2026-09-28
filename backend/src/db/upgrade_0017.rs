//! Migration 0017 (layout format v2) against stored settings in the old
//! format: every panel, field and flag arrives in the new format, exactly as
//! the API converts a v1 layout, and the old version stays in the history.

use serde_json::{Value, json};
use sqlx::PgPool;

use crate::db::{MIGRATOR, scratch};
use crate::modules::ui_settings::document::UiSettingsDocument;

fn settings_before() -> Value {
    json!({
        "branding": { "appName": "Acme CMDB" },
        "listViews": [{ "classKey": "server", "columns": ["label", "attributes.hostname"] }],
        "layouts": [
            {
                "classKey": "server",
                "panels": [
                    { "key": "main", "label": "Main", "fields": ["attributes.name", "validFrom", "attributes.hostname"] },
                    { "key": "hw", "label": "Hardware", "collapsed": true, "fields": ["attributes.cpu_cores"] },
                    { "key": "empty", "label": "Nothing yet" }
                ],
                "hiddenFields": ["ident", "attributes.notes", "validUntil"],
                "readOnlyFields": ["ident", "attributes.owner"]
            },
            { "classKey": "vm", "hiddenFields": ["attributes.notes"] },
            { "classKey": "app", "panels": [], "readOnlyFields": ["validFrom"] }
        ]
    })
}

async fn current(pool: &PgPool) -> (i32, Value) {
    sqlx::query_as("SELECT version, settings FROM ui_settings").fetch_one(pool).await.unwrap()
}

#[tokio::test]
async fn v1_layouts_become_tabs_as_the_api_converts_them() {
    let Some(db) = scratch::empty("v1_layouts_become_tabs_as_the_api_converts_them").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(16, pool).await.expect("migrations up to 0016");
    sqlx::query("INSERT INTO ui_settings_versions (version, settings, actor_type) SELECT max(version) + 1, $1, 'user' FROM ui_settings_versions")
        .bind(settings_before())
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("UPDATE ui_settings SET version = (SELECT max(version) FROM ui_settings_versions), settings = $1")
        .bind(settings_before())
        .execute(pool)
        .await
        .unwrap();
    let (before_version, _) = current(pool).await;

    MIGRATOR.run(pool).await.expect("migration 0017");

    let (version, s) = current(pool).await;
    assert_eq!(version, before_version + 1, "saved as a new version");
    let (kept,): (Value,) = sqlx::query_as("SELECT settings FROM ui_settings_versions WHERE version = $1")
        .bind(before_version)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(kept, settings_before(), "the old version stays as it was");
    let (by, comment): (String, String) =
        sqlx::query_as("SELECT actor_name, comment FROM ui_settings_versions WHERE version = $1")
            .bind(version)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(by, "migration 0017");
    assert!(comment.contains("layout format v2"), "{comment}");

    let server = &s["layouts"][0];
    assert!(server.get("panels").is_none(), "{server}");
    assert_eq!(
        server["tabs"],
        json!([{ "key": "general", "label": "General", "sections": [
            { "key": "main", "label": "Main", "columns": 3, "collapsed": false, "fields": [
                { "field": "attributes.name", "width": 1 },
                { "field": "validFrom", "width": 1 },
                { "field": "attributes.hostname", "width": 1 } ] },
            { "key": "hw", "label": "Hardware", "columns": 3, "collapsed": true, "fields": [
                { "field": "attributes.cpu_cores", "width": 1 } ] },
            { "key": "empty", "label": "Nothing yet", "columns": 3, "collapsed": false, "fields": [] }
        ] }])
    );
    assert_eq!(server["hiddenFields"], json!(["attributes.notes"]), "core fields are no longer hidden");
    assert_eq!(server["readOnlyFields"], json!(["ident", "attributes.owner"]));
    assert_eq!(s["layouts"][1], json!({ "classKey": "vm", "hiddenFields": ["attributes.notes"] }));
    assert_eq!(s["layouts"][2], json!({ "classKey": "app", "readOnlyFields": ["validFrom"] }));
    assert_eq!(s["branding"], settings_before()["branding"]);
    assert_eq!(s["listViews"], settings_before()["listViews"]);

    // The same document the API makes of the old one (apart from the hidden core fields, which the
    // API reports instead of dropping), and valid for a save.
    let stored: UiSettingsDocument = serde_json::from_value(s.clone()).expect("the new format");
    let mut api: UiSettingsDocument = serde_json::from_value(settings_before()).expect("the old format");
    api.layouts[0].hidden_fields.retain(|f| f.starts_with("attributes."));
    assert_eq!(stored, api);
    assert!(stored.problems("").is_empty(), "{:?}", stored.problems(""));

    db.drop().await;
}

#[tokio::test]
async fn settings_without_layouts_get_no_new_version() {
    let Some(db) = scratch::empty("settings_without_layouts_get_no_new_version").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(16, pool).await.expect("migrations up to 0016");
    let before = current(pool).await;
    MIGRATOR.run(pool).await.expect("migration 0017");
    assert_eq!(current(pool).await, before);
    db.drop().await;
}
