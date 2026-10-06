//! Migration 0048 (record details and relationships as layout sections, SHAA-1643) against stored
//! settings and CI layouts: every layout with tabs gets the "Record" and "Relationships" sections it
//! showed without placing them, exactly as the API converts a document of an older layout format; the
//! old versions stay in the history and every change is audited.

use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::db::{MIGRATOR, scratch};
use crate::modules::ui_settings::document::{UiLayout, UiSettingsDocument, place_implicit_panels};

fn record(key: &str) -> Value {
    json!({ "key": key, "label": "Record", "kind": "record", "columns": 3, "width": 12, "fields": [], "collapsed": false })
}
fn relations(key: &str) -> Value {
    json!({ "key": key, "label": "Relationships", "kind": "relations", "columns": 3, "width": 12, "fields": [], "collapsed": false })
}

/// Two windows on the first tab, the relationships already placed on the second; "record" is a section key.
fn server_tabs() -> Value {
    json!([
        { "key": "general", "label": "General", "placement": "free", "sections": [
            { "key": "record", "label": "Main", "columns": 3, "width": 6, "collapsed": false,
              "fields": [{ "field": "attributes.hostname", "width": 2 }, { "field": "ident", "width": 1 }],
              "frame": { "x": 0.0, "y": 0, "w": 0.5, "h": 96, "z": 1 } },
            { "key": "note", "label": "Read me", "kind": "note", "text": "Patch on *Sundays*", "columns": 3,
              "width": 6, "collapsed": true, "fields": [],
              "frame": { "x": 0.5, "y": 0, "w": 0.5, "h": 144, "z": 2 } }
        ] },
        { "key": "context", "label": "Context", "placement": "free", "sections": [
            { "key": "rel", "label": "Links", "kind": "relations", "columns": 3, "width": 12,
              "collapsed": false, "fields": [], "frame": { "x": 0.0, "y": 0, "w": 1.0, "h": 320, "z": 1 } }
        ] }
    ])
}

/// One tab on the earlier grid, nothing placed.
fn app_tabs() -> Value {
    json!([{ "key": "main", "label": "Main", "sections": [
        { "key": "core", "label": "Core", "columns": 2, "width": 12, "collapsed": false,
          "fields": [{ "field": "ident", "width": 1 }] }
    ] }])
}

fn settings_before() -> Value {
    let empty = json!({ "tabs": [], "hiddenFields": [], "readOnlyFields": [] });
    json!({
        "branding": { "appName": "Acme CMDB" },
        "layouts": [
            { "classKey": "server", "templateKey": "server" },
            { "classKey": "application", "templateKey": "app" },
            { "classKey": "vm", "templateKey": "standard" }
        ],
        "layoutTemplates": [
            { "key": "standard", "name": "Standard", "layout": empty },
            { "key": "server", "name": "Server layout", "layout": {
                "tabs": server_tabs(), "hiddenFields": ["attributes.notes"], "readOnlyFields": [] } },
            { "key": "app", "name": "Application layout", "description": "Apps", "layout": {
                "tabs": app_tabs(), "hiddenFields": [], "readOnlyFields": ["label"] } }
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

async fn ci(pool: &PgPool, ident: &str) -> Uuid {
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO configuration_items (class_id, ident, label) SELECT id, $1, $1 FROM ci_classes WHERE key = 'business_service' RETURNING id",
    )
    .bind(ident)
    .fetch_one(pool)
    .await
    .unwrap();
    id
}

#[tokio::test]
async fn layouts_get_the_record_and_relations_sections_they_showed() {
    let Some(db) = scratch::empty("layouts_get_the_record_and_relations_sections_they_showed").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(47, pool).await.expect("migrations up to 0047");
    store(pool, &settings_before()).await;
    let (before_version, _) = current(pool).await;
    // CIs' own layouts: one with tabs, one without, one using a template.
    let (own, plain, by_template) = (ci(pool, "CI-1").await, ci(pool, "CI-2").await, ci(pool, "CI-3").await);
    let own_layout = json!({ "tabs": app_tabs(), "hiddenFields": [], "readOnlyFields": [] });
    let plain_layout = json!({ "tabs": [], "hiddenFields": ["attributes.notes"], "readOnlyFields": [] });
    for (id, template, layout) in [
        (own, None, Some(own_layout.clone())),
        (plain, None, Some(plain_layout.clone())),
        (by_template, Some("app"), None),
    ] {
        sqlx::query("INSERT INTO ci_layout_overrides (ci_id, template_key, layout, updated_by_type, updated_by_name) VALUES ($1, $2, $3, 'user', 'Owner')")
            .bind(id)
            .bind(template)
            .bind(layout)
            .execute(pool)
            .await
            .unwrap();
    }

    MIGRATOR.run_to(48, pool).await.expect("migration 0048");

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
    assert_eq!(by, "migration 0048");
    assert!(comment.contains("layout format 3"), "{comment}");

    let (actor, action, old, new): (String, String, Value, Value) = sqlx::query_as(
        "SELECT actor_name, action::text, old_value, new_value FROM audit_log WHERE entity_type = 'ui_settings' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!((actor.as_str(), action.as_str()), ("migration 0048", "update"));
    assert_eq!(old, json!({ "version": before_version, "settings": settings_before() }));
    assert_eq!((new["version"].as_i64(), &new["settings"]), (Some(i64::from(version)), &s));

    // Record last on the first tab ("record" is taken: record_2); the server template places the
    // relationships already, the app template gets them after the record details.
    let mut server = server_tabs();
    server[0]["sections"].as_array_mut().unwrap().push(record("record_2"));
    let mut app = app_tabs();
    app[0]["sections"].as_array_mut().unwrap().extend([record("record"), relations("relations")]);
    let mut expected = settings_before();
    expected["layoutTemplates"][1]["layout"]["tabs"] = server;
    expected["layoutTemplates"][2]["layout"]["tabs"] = app.clone();
    expected["layoutFormat"] = json!(3);
    assert_eq!(s, expected, "only the layouts with tabs change, and the format");

    // The API converts the old document the same way, and the result is valid for a save.
    let stored: UiSettingsDocument = serde_json::from_value(s.clone()).expect("the new format");
    let old: UiSettingsDocument = serde_json::from_value(settings_before()).expect("the old format");
    assert_eq!(stored.layout_format, Some(3));
    assert_eq!(old.clone().upgraded(), stored.clone().upgraded());
    assert!(stored.problems("").is_empty(), "{:?}", stored.problems(""));
    // Read back, the new sections are windows below the first tab's others.
    let read = stored.upgraded().normalized();
    let tab = &read.layout_templates[1].layout.tabs[0];
    let rec = tab.sections.last().unwrap();
    assert_eq!(rec.key, "record_2");
    // The first tab's windows are where the grid put them, so format 4 makes "Main" taller (one row of
    // fields: 170 px); the note stays the taller one.
    assert_eq!(rec.frame.map(|f| (f.x, f.y, f.w)), Some((0.0, 170 + 16, 1.0)), "below the windows");

    // The CIs' own layouts: the one with tabs converted, with a new version, audited like the API's writes.
    type Row = (Uuid, Option<Value>, i32, String, Option<String>);
    let rows: Vec<Row> =
        sqlx::query_as("SELECT ci_id, layout, version, updated_by_type, updated_by_name FROM ci_layout_overrides")
            .fetch_all(pool)
            .await
            .unwrap();
    let row = |id: Uuid| rows.iter().find(|r| r.0 == id).unwrap().clone();
    let mut own_after = own_layout.clone();
    own_after["tabs"] = app;
    assert_eq!(row(own).1, Some(own_after.clone()));
    assert_eq!((row(own).2, row(own).3.as_str(), row(own).4.as_deref()), (2, "system", Some("migration 0048")));
    let mut api: UiLayout = serde_json::from_value(own_layout.clone()).unwrap();
    place_implicit_panels(&mut api.tabs);
    assert_eq!(serde_json::to_value(&api).unwrap(), own_after, "as the API places them");
    assert_eq!((row(plain).1, row(plain).2, row(plain).4.as_deref()), (Some(plain_layout), 1, Some("Owner")));
    assert_eq!((row(by_template).1, row(by_template).2), (None, 1));
    let audits: Vec<(Uuid, Value, Value)> = sqlx::query_as(
        "SELECT entity_id, old_value, new_value FROM audit_log WHERE entity_type = 'ci_layout_overrides' AND actor_name = 'migration 0048'",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(
        audits,
        [(
            own,
            json!({ "ciId": own, "templateKey": null, "layout": own_layout, "version": 1 }),
            json!({ "ciId": own, "templateKey": null, "layout": own_after, "version": 2 })
        )]
    );

    db.drop().await;
}

#[tokio::test]
async fn settings_without_tabs_get_no_new_version() {
    let Some(db) = scratch::empty("settings_without_tabs_get_no_new_version_0048").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(47, pool).await.expect("migrations up to 0047");
    let empty = json!({ "tabs": [], "hiddenFields": [], "readOnlyFields": [] });
    store(pool, &json!({ "layoutTemplates": [{ "key": "standard", "name": "Standard", "layout": empty }] })).await;
    let before = current(pool).await;
    MIGRATOR.run_to(48, pool).await.expect("migration 0048");
    assert_eq!(current(pool).await, before);
    MIGRATOR.run_to(48, pool).await.expect("re-run");
    db.drop().await;
}
