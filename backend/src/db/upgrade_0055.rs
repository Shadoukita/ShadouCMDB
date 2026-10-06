//! Migration 0055 (layout windows as tall as the inline inputs, SHAA-1833, GH#621) against stored settings
//! and CI layouts: a tab whose windows are all still where the old height estimate put them from the grid
//! gets the frames of the new one, exactly as the API converts a document of an older layout format; a tab
//! arranged in the layout editor is left alone, the old versions stay in the history and every change is
//! audited.

use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::db::{MIGRATOR, scratch};
use crate::modules::ui_settings::document::{
    FRAME_RECORD_PX, FRAME_SEPARATOR_PX, LAYOUT_FORMAT, UiLayout, UiSettingsDocument, resize_default_frames,
};

fn frame(x: f64, y: u32, w: f64, h: u32, z: u32) -> Value {
    json!({ "x": x, "y": y, "w": w, "h": h, "z": z })
}

/// Placed from the grid with the old estimate (48 px title bar, 48 px per row): two field sections side by
/// side (one row; two rows with a separator), a note below, then the record details and the relationships.
fn default_tabs() -> Value {
    json!([
        { "key": "general", "label": "General", "placement": "free", "sections": [
            { "key": "net", "label": "Network", "columns": 3, "width": 6, "collapsed": false,
              "fields": [{ "field": "attributes.hostname", "width": 1 }, { "field": "attributes.ip_address", "width": 1 }],
              "frame": frame(0.0, 0, 0.5, 96, 1) },
            { "key": "life", "label": "Lifecycle", "columns": 2, "width": 6, "collapsed": true,
              "fields": [{ "field": "validFrom", "width": 1 }, { "separator": true, "label": "Until", "width": 2 },
                         { "field": "validUntil", "width": 1 }],
              "frame": frame(0.5, 0, 0.5, 192, 2) },
            { "key": "note", "label": "Read me", "kind": "note", "text": "Patch on *Sundays*", "columns": 3,
              "width": 12, "collapsed": false, "fields": [], "frame": frame(0.0, 208, 1.0, 144, 3) },
            { "key": "record", "label": "Record", "kind": "record", "columns": 3, "width": 12, "collapsed": false,
              "fields": [], "frame": frame(0.0, 368, 1.0, 144, 4) },
            { "key": "relations", "label": "Relationships", "kind": "relations", "columns": 3, "width": 12,
              "collapsed": false, "fields": [] }
        ] }
    ])
}

/// The same tabs with the new estimate: Network 88 + 82 = 170, Lifecycle 88 + 2 × 82 + 28 = 280 (the row
/// is as tall as it), the note and the record details below.
fn default_tabs_after() -> Value {
    let mut t = default_tabs();
    let s = &mut t[0]["sections"];
    s[0]["frame"] = frame(0.0, 0, 0.5, 170, 1);
    s[1]["frame"] = frame(0.5, 0, 0.5, 88 + 2 * 82 + FRAME_SEPARATOR_PX, 2);
    s[2]["frame"] = frame(0.0, 296, 1.0, 144, 3);
    s[3]["frame"] = frame(0.0, 456, 1.0, FRAME_RECORD_PX, 4);
    t
}

/// Arranged in the layout editor: the second window was moved down.
fn arranged_tabs() -> Value {
    json!([{ "key": "main", "label": "Main", "placement": "free", "sections": [
        { "key": "core", "label": "Core", "columns": 2, "width": 12, "collapsed": false,
          "fields": [{ "field": "ident", "width": 1 }], "frame": frame(0.0, 0, 1.0, 96, 1) },
        { "key": "more", "label": "More", "columns": 2, "width": 12, "collapsed": false,
          "fields": [{ "field": "label", "width": 1 }], "frame": frame(0.0, 400, 1.0, 96, 2) }
    ] }])
}

fn settings_before() -> Value {
    let empty = json!({ "tabs": [], "hiddenFields": [], "readOnlyFields": [] });
    json!({
        "branding": { "appName": "Acme CMDB" },
        "layoutFormat": 3,
        "layouts": [
            { "classKey": "server", "templateKey": "server" },
            { "classKey": "application", "templateKey": "app" }
        ],
        "layoutTemplates": [
            { "key": "standard", "name": "Standard", "layout": empty },
            { "key": "server", "name": "Server layout", "layout": {
                "tabs": default_tabs(), "hiddenFields": ["attributes.notes"], "readOnlyFields": ["label"] } },
            { "key": "app", "name": "Application layout", "layout": {
                "tabs": arranged_tabs(), "hiddenFields": [], "readOnlyFields": [] } }
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
async fn windows_placed_from_the_grid_get_the_new_heights() {
    let Some(db) = scratch::empty("windows_placed_from_the_grid_get_the_new_heights").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(54, pool).await.expect("migrations up to 0054");
    store(pool, &settings_before()).await;
    let (before_version, _) = current(pool).await;
    let (own, arranged) = (ci(pool, "CI-1").await, ci(pool, "CI-2").await);
    let own_layout = json!({ "tabs": default_tabs(), "hiddenFields": [], "readOnlyFields": ["ident"] });
    let arranged_layout = json!({ "tabs": arranged_tabs(), "hiddenFields": [], "readOnlyFields": [] });
    for (id, layout) in [(own, &own_layout), (arranged, &arranged_layout)] {
        sqlx::query("INSERT INTO ci_layout_overrides (ci_id, layout, updated_by_type, updated_by_name) VALUES ($1, $2, 'user', 'Owner')")
            .bind(id)
            .bind(layout)
            .execute(pool)
            .await
            .unwrap();
    }

    MIGRATOR.run_to(55, pool).await.expect("migration 0055");

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
    assert_eq!(by, "migration 0055");
    assert!(comment.contains("layout format 4"), "{comment}");
    let (actor, old, new): (String, Value, Value) = sqlx::query_as(
        "SELECT actor_name, old_value, new_value FROM audit_log WHERE entity_type = 'ui_settings' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(actor, "migration 0055");
    assert_eq!(old, json!({ "version": before_version, "settings": settings_before() }));
    assert_eq!((new["version"].as_i64(), &new["settings"]), (Some(i64::from(version)), &s));

    // Only the frames of the default tab change: sections, separators, the note, hidden and read-only fields stay.
    let mut expected = settings_before();
    expected["layoutTemplates"][1]["layout"]["tabs"] = default_tabs_after();
    expected["layoutFormat"] = json!(4);
    assert_eq!(s, expected);

    // The API converts the old document the same way, and the result is valid and stays as it is.
    let stored: UiSettingsDocument = serde_json::from_value(s.clone()).expect("the new format");
    let old: UiSettingsDocument = serde_json::from_value(settings_before()).expect("the old format");
    assert_eq!(stored.layout_format, Some(LAYOUT_FORMAT));
    assert_eq!(old.upgraded(), stored);
    assert_eq!(stored.clone().upgraded(), stored, "format 4 is left alone");
    assert!(stored.problems("").is_empty(), "{:?}", stored.problems(""));
    assert_eq!(
        stored.clone().normalized().layout_templates[1].layout.tabs[0].sections[4].frame.map(|f| f.y),
        Some(456 + FRAME_RECORD_PX + 16)
    );

    // The CIs' own layouts: the default one resized, with a new version and audited; the arranged one kept.
    type Row = (Uuid, Value, i32, Option<String>);
    let rows: Vec<Row> = sqlx::query_as("SELECT ci_id, layout, version, updated_by_name FROM ci_layout_overrides")
        .fetch_all(pool)
        .await
        .unwrap();
    let row = |id: Uuid| rows.iter().find(|r| r.0 == id).unwrap().clone();
    let mut own_after = own_layout.clone();
    own_after["tabs"] = default_tabs_after();
    assert_eq!(row(own), (own, own_after.clone(), 2, Some("migration 0055".into())));
    let mut api: UiLayout = serde_json::from_value(own_layout.clone()).unwrap();
    assert!(resize_default_frames(&mut api.tabs[0]));
    assert_eq!(serde_json::to_value(&api).unwrap(), own_after, "as the API resizes them");
    assert_eq!(row(arranged), (arranged, arranged_layout, 1, Some("Owner".into())));
    let audits: Vec<(Uuid, Value, Value)> = sqlx::query_as(
        "SELECT entity_id, old_value, new_value FROM audit_log WHERE entity_type = 'ci_layout_overrides' AND actor_name = 'migration 0055'",
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
async fn arranged_layouts_get_no_new_version() {
    let Some(db) = scratch::empty("arranged_layouts_get_no_new_version_0055").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(54, pool).await.expect("migrations up to 0054");
    let layout = json!({ "tabs": arranged_tabs(), "hiddenFields": [], "readOnlyFields": [] });
    store(
        pool,
        &json!({ "layoutFormat": 3, "layoutTemplates": [{ "key": "standard", "name": "Standard", "layout": layout }] }),
    )
    .await;
    let before = current(pool).await;
    MIGRATOR.run_to(55, pool).await.expect("migration 0055");
    assert_eq!(current(pool).await, before);
    MIGRATOR.run_to(55, pool).await.expect("re-run");
    db.drop().await;
}
