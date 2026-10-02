//! Migration 0042 (layout templates, SHAA-1472) against stored settings with
//! class layouts: each becomes a template the class uses, with every tab,
//! section, field and flag, exactly as the API converts a document without
//! templates; the old version stays in the history and the change is audited.

use std::collections::HashMap;

use serde_json::{Value, json};
use sqlx::PgPool;

use crate::db::{MIGRATOR, scratch};
use crate::modules::ui_settings::document::{self, UiSettingsDocument};

fn server_tabs() -> Value {
    json!([
        { "key": "general", "label": "General", "placement": "free", "sections": [
            { "key": "main", "label": "Main", "columns": 3, "width": 6, "collapsed": false,
              "fields": [{ "field": "attributes.hostname", "width": 2 }, { "field": "ident", "width": 1 }],
              "frame": { "x": 0.0, "y": 0, "w": 0.5, "h": 96, "z": 1 } },
            { "key": "note", "label": "Read me", "kind": "note", "text": "Patch on *Sundays*", "columns": 3,
              "width": 6, "collapsed": true, "fields": [],
              "frame": { "x": 0.5, "y": 0, "w": 0.5, "h": 144, "z": 2, "minH": 96 } }
        ] },
        { "key": "context", "label": "Context", "sections": [
            { "key": "rel", "label": "Relationships", "kind": "relations", "columns": 3, "width": 12,
              "collapsed": false, "fields": [] }
        ] }
    ])
}

fn settings_before() -> Value {
    json!({
        "branding": { "appName": "Acme CMDB" },
        "listViews": [{ "classKey": "server", "columns": ["label", "attributes.hostname"] }],
        "layouts": [
            { "classKey": "server", "tabs": server_tabs(), "hiddenFields": ["attributes.notes"],
              "readOnlyFields": ["attributes.owner"] },
            { "classKey": "vm" },
            { "classKey": "business_service", "readOnlyFields": ["label"] },
            { "classKey": "standard", "hiddenFields": ["attributes.notes"] }
        ]
    })
}

async fn current(pool: &PgPool) -> (i32, Value) {
    sqlx::query_as("SELECT version, settings FROM ui_settings").fetch_one(pool).await.unwrap()
}

async fn store(pool: &PgPool, settings: &Value) {
    sqlx::query("INSERT INTO ui_settings_versions (version, settings, actor_type) SELECT max(version) + 1, $1, 'user' FROM ui_settings_versions")
        .bind(settings)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("UPDATE ui_settings SET version = (SELECT max(version) FROM ui_settings_versions), settings = $1")
        .bind(settings)
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn class_layouts_become_the_templates_their_classes_use() {
    let Some(db) = scratch::empty("class_layouts_become_the_templates_their_classes_use").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(41, pool).await.expect("migrations up to 0041");
    store(pool, &settings_before()).await;
    let (before_version, _) = current(pool).await;

    MIGRATOR.run(pool).await.expect("migration 0042");

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
    assert_eq!(by, "migration 0042");
    assert!(comment.contains("layout templates"), "{comment}");

    // Audited like a save through the API.
    let (actor, action, old, new): (String, String, Value, Value) = sqlx::query_as(
        "SELECT actor_name, action::text, old_value, new_value FROM audit_log WHERE entity_type = 'ui_settings' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!((actor.as_str(), action.as_str()), ("migration 0042", "update"));
    assert_eq!(old, json!({ "version": before_version, "settings": settings_before() }));
    assert_eq!((new["version"].as_i64(), &new["settings"]), (Some(i64::from(version)), &s));

    assert_eq!(
        s["layouts"],
        json!([
            { "classKey": "server", "templateKey": "server" },
            { "classKey": "vm", "templateKey": "standard" },
            { "classKey": "business_service", "templateKey": "business_service" },
            { "classKey": "standard", "templateKey": "standard_2" }
        ])
    );
    let empty = json!({ "tabs": [], "hiddenFields": [], "readOnlyFields": [] });
    assert_eq!(
        s["layoutTemplates"],
        json!([
            { "key": "standard", "name": "Standard", "layout": empty },
            { "key": "server", "name": "server layout", "layout": {
                "tabs": server_tabs(), "hiddenFields": ["attributes.notes"], "readOnlyFields": ["attributes.owner"] } },
            { "key": "business_service", "name": "Business service layout", "layout": {
                "tabs": [], "hiddenFields": [], "readOnlyFields": ["label"] } },
            { "key": "standard_2", "name": "standard layout", "layout": {
                "tabs": [], "hiddenFields": ["attributes.notes"], "readOnlyFields": [] } }
        ]),
        "every tab, section and field in the class's template"
    );
    assert_eq!(s["branding"], settings_before()["branding"]);
    assert_eq!(s["listViews"], settings_before()["listViews"]);

    // The document the API makes of the old one, valid for a save; and it shows each class as before.
    let stored: UiSettingsDocument = serde_json::from_value(s.clone()).expect("the new format");
    let old: UiSettingsDocument = serde_json::from_value(settings_before()).expect("the old format");
    let names = HashMap::from([("business_service".to_owned(), "Business service".to_owned())]);
    let api = document::contract(old.clone(), &[], &names).expect("converts");
    assert_eq!(stored, api);
    assert!(stored.problems("").is_empty(), "{:?}", stored.problems(""));
    let returned = stored.returned();
    for (was, now) in old.layouts.iter().zip(&returned.layouts) {
        assert_eq!(was.content(), now.content(), "{}", was.class_key);
    }

    // One source per CI layout.
    let ci: (uuid::Uuid,) = sqlx::query_as(
        "INSERT INTO configuration_items (class_id, ident, label) SELECT id, 'CI-1', 'CI-1' FROM ci_classes WHERE key = 'business_service' RETURNING id",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    let insert = |template: Option<&'static str>, layout: Option<Value>| {
        sqlx::query("INSERT INTO ci_layout_overrides (ci_id, template_key, layout, updated_by_type) VALUES ($1, $2, $3, 'user')")
            .bind(ci.0)
            .bind(template)
            .bind(layout)
            .execute(pool)
    };
    let err = insert(Some("server"), Some(json!({}))).await.unwrap_err();
    assert!(err.to_string().contains("ci_layout_overrides_one_source"), "{err}");
    let err = insert(None, None).await.unwrap_err();
    assert!(err.to_string().contains("ci_layout_overrides_one_source"), "{err}");
    insert(Some("server"), None).await.expect("a template");

    db.drop().await;
}

#[tokio::test]
async fn settings_without_layouts_get_no_new_version() {
    let Some(db) = scratch::empty("settings_without_layouts_get_no_new_version_0042").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(41, pool).await.expect("migrations up to 0041");
    let before = current(pool).await;
    MIGRATOR.run(pool).await.expect("migration 0042");
    assert_eq!(current(pool).await, before);
    db.drop().await;
}
