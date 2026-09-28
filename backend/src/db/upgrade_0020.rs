//! Migration 0020 (attribute sorts) against an install upgraded through 0016:
//! the stored sorts 0016 turned into label sorts go back to the attributes
//! 0016 made of the old columns, unless they were changed since or the class
//! has no such attribute.

use serde_json::{Value, json};
use sqlx::{Executor, PgPool};

use crate::db::{MIGRATOR, scratch};

/// Server (below the abstract hardware) and vm hold hostnames; vm already
/// has a field "hostname", so 0016 names the migrated one "hostname_2".
const BEFORE: &str = r#"
INSERT INTO areas (key, name) VALUES ('infrastruktur', 'Infrastruktur');
CREATE SCHEMA infrastruktur;
INSERT INTO ci_classes (id, key, name, area_id, is_abstract, parent_id) VALUES
  ('00000000-0000-0000-0000-00000000c001', 'hardware', 'Hardware', (SELECT id FROM areas), true, NULL),
  ('00000000-0000-0000-0000-00000000c002', 'server', 'Server', (SELECT id FROM areas), false, '00000000-0000-0000-0000-00000000c001'),
  ('00000000-0000-0000-0000-00000000c003', 'vm', 'VM', (SELECT id FROM areas), false, NULL),
  ('00000000-0000-0000-0000-00000000c004', 'app', 'App', (SELECT id FROM areas), false, NULL);
INSERT INTO ci_attribute_definitions (class_id, key, label, data_type)
VALUES ('00000000-0000-0000-0000-00000000c003', 'hostname', 'Custom hostname', 'text');
CREATE TABLE infrastruktur.hardware (id uuid PRIMARY KEY REFERENCES configuration_items (id) ON DELETE CASCADE);
CREATE TABLE infrastruktur.server (id uuid PRIMARY KEY REFERENCES configuration_items (id) ON DELETE CASCADE);
CREATE TABLE infrastruktur.vm (id uuid PRIMARY KEY REFERENCES configuration_items (id) ON DELETE CASCADE, hostname text);
CREATE TABLE infrastruktur.app (id uuid PRIMARY KEY REFERENCES configuration_items (id) ON DELETE CASCADE);
INSERT INTO statuses (id, key, name, is_operational, sort_order) VALUES
  ('00000000-0000-0000-0000-00000000a001', 'in_service', 'In service', true, 10);
INSERT INTO configuration_items (id, class_id, name, status_id, hostname, ip_address, serial_number) VALUES
  ('00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-00000000c002', 'srv-1',
   '00000000-0000-0000-0000-00000000a001', 'srv1.example.com', '10.0.0.1', 'SN-1'),
  ('00000000-0000-0000-0000-000000000003', '00000000-0000-0000-0000-00000000c003', 'vm-1',
   '00000000-0000-0000-0000-00000000a001', 'vm1', NULL, NULL),
  ('00000000-0000-0000-0000-000000000004', '00000000-0000-0000-0000-00000000c004', 'CRM',
   '00000000-0000-0000-0000-00000000a001', NULL, NULL, NULL);
INSERT INTO infrastruktur.hardware (id) VALUES ('00000000-0000-0000-0000-000000000001');
INSERT INTO infrastruktur.server (id) VALUES ('00000000-0000-0000-0000-000000000001');
INSERT INTO infrastruktur.vm (id, hostname) VALUES ('00000000-0000-0000-0000-000000000003', 'custom');
INSERT INTO infrastruktur.app (id) VALUES ('00000000-0000-0000-0000-000000000004');
"#;

fn search(id: &str, class: &str, field: &str, direction: &str) -> Value {
    json!({ "id": id, "type": "saved_search", "search": {
        "classKeys": [class], "includeSubclasses": true, "sort": { "field": field, "direction": direction } } })
}

fn view(class: &str, field: &str, direction: &str) -> Value {
    json!({ "classKey": class, "defaultSort": { "field": field, "direction": direction } })
}

fn settings_before() -> Value {
    json!({
        "dashboard": { "widgets": [
            search("serials", "server", "serialNumber", "desc"),
            search("ips", "server", "ipAddress", "asc"),
            { "id": "both", "type": "saved_search", "search": {
                "classKeys": ["server", "vm"], "sort": { "field": "hostname", "direction": "asc" } } },
        ] },
        "listViews": [
            view("server", "hostname", "asc"),
            view("vm", "hostname", "desc"),
            // status: every class holding one has the attribute
            view("app", "statusName", "asc"),
            // 0016 put serial_number on server, below hardware
            view("hardware", "serialNumber", "asc"),
        ]
    })
}

async fn settings(pool: &PgPool) -> (i32, Value) {
    sqlx::query_as("SELECT version, settings FROM ui_settings").fetch_one(pool).await.unwrap()
}

#[tokio::test]
async fn sorts_0016_turned_into_label_sorts_are_restored() {
    let Some(db) = scratch::empty("sorts_0016_turned_into_label_sorts_are_restored").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(15, pool).await.expect("migrations up to 0015");
    pool.execute(sqlx::AssertSqlSafe(BEFORE)).await.expect("data before the upgrade");
    sqlx::query("INSERT INTO ui_settings_versions (version, settings, actor_type) VALUES (2, $1, 'user')")
        .bind(settings_before())
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("UPDATE ui_settings SET version = 2, settings = $1")
        .bind(settings_before())
        .execute(pool)
        .await
        .unwrap();
    MIGRATOR.run_to(19, pool).await.expect("migrations up to 0019");

    let (version, s) = settings(pool).await;
    assert_eq!(version, 3, "0016 wrote a version");
    assert_eq!(s["listViews"][0]["defaultSort"], json!({ "field": "label", "direction": "asc" }));

    // After 0016 an administrator changed the IP search to a descending label sort.
    let mut changed = s.clone();
    changed["dashboard"]["widgets"][1]["search"]["sort"] = json!({ "field": "label", "direction": "desc" });
    sqlx::query(
        "INSERT INTO ui_settings_versions (version, settings, actor_type) VALUES (4, $1, 'user');
         ",
    )
    .bind(&changed)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("UPDATE ui_settings SET version = 4, settings = $1").bind(&changed).execute(pool).await.unwrap();

    MIGRATOR.run(pool).await.expect("migration 0020");

    let (version, s) = settings(pool).await;
    assert_eq!(version, 5);
    let sort = |field: &str, direction: &str| json!({ "field": field, "direction": direction });
    let views: Vec<&Value> = s["listViews"].as_array().unwrap().iter().map(|v| &v["defaultSort"]).collect();
    assert_eq!(
        views,
        [
            &sort("attributes.hostname", "asc"),
            &sort("attributes.hostname_2", "desc"),
            &sort("attributes.status", "asc"),
            &sort("label", "asc"),
        ]
    );
    let widgets: Vec<&Value> =
        s["dashboard"]["widgets"].as_array().unwrap().iter().map(|w| &w["search"]["sort"]).collect();
    assert_eq!(
        widgets,
        [
            &sort("attributes.serial_number", "desc"),
            &sort("label", "desc"),
            // Two classes: not restored.
            &sort("label", "asc"),
        ]
    );
    let (actor, comment): (String, String) =
        sqlx::query_as("SELECT actor_name, comment FROM ui_settings_versions WHERE version = 5")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(actor, "migration 0020");
    assert!(comment.contains("restored"), "{comment}");

    // The restored settings are valid and resolve without issues.
    let doc: crate::modules::ui_settings::document::UiSettingsDocument = serde_json::from_value(s).unwrap();
    assert!(crate::api::route::Check::check(&doc).is_empty());
    db.drop().await;
}

#[tokio::test]
async fn fresh_install_is_unchanged_by_0020() {
    let Some(db) = scratch::empty("fresh_install_is_unchanged_by_0020").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(19, pool).await.expect("migrations up to 0019");
    let before = settings(pool).await;
    MIGRATOR.run(pool).await.expect("migration 0020");
    assert_eq!(settings(pool).await, before);
    db.drop().await;
}
