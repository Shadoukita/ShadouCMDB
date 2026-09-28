//! Migration 0018 (multi-line text) against an install upgraded through 0016:
//! the notes fields 0016 made of the old column become multi-line, fields an
//! administrator created are left alone, and every change is audited.

use serde_json::{Value, json};
use sqlx::{Executor, PgPool};

use crate::db::{MIGRATOR, scratch};

/// Classes and CIs holding notes before 0016; "app" already has a field
/// "notes" of its own, so 0016 names the migrated one "notes_2".
const BEFORE: &str = r#"
INSERT INTO areas (key, name) VALUES ('infrastruktur', 'Infrastruktur');
CREATE SCHEMA infrastruktur;
INSERT INTO ci_classes (id, key, name, area_id, is_abstract, parent_id) VALUES
  ('00000000-0000-0000-0000-00000000c002', 'server', 'Server', (SELECT id FROM areas), false, NULL),
  ('00000000-0000-0000-0000-00000000c003', 'vm', 'VM', (SELECT id FROM areas), false, NULL),
  ('00000000-0000-0000-0000-00000000c004', 'app', 'App', (SELECT id FROM areas), false, NULL);
INSERT INTO ci_attribute_definitions (class_id, key, label, data_type)
VALUES ('00000000-0000-0000-0000-00000000c004', 'notes', 'Notes', 'text');
CREATE TABLE infrastruktur.server (id uuid PRIMARY KEY REFERENCES configuration_items (id) ON DELETE CASCADE);
CREATE TABLE infrastruktur.vm (id uuid PRIMARY KEY REFERENCES configuration_items (id) ON DELETE CASCADE);
CREATE TABLE infrastruktur.app (id uuid PRIMARY KEY REFERENCES configuration_items (id) ON DELETE CASCADE, notes text);
INSERT INTO statuses (id, key, name, is_operational, sort_order) VALUES
  ('00000000-0000-0000-0000-00000000a001', 'in_service', 'In service', true, 10);
INSERT INTO configuration_items (id, class_id, name, status_id, notes)
SELECT v.id::uuid, v.class_id::uuid, v.name, '00000000-0000-0000-0000-00000000a001', v.notes FROM (VALUES
  ('00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-00000000c002', 'srv-1', E'Rack A01\nSlot 4'),
  ('00000000-0000-0000-0000-000000000003', '00000000-0000-0000-0000-00000000c003', 'vm-1', NULL),
  ('00000000-0000-0000-0000-000000000004', '00000000-0000-0000-0000-00000000c004', 'CRM', E'Line 1\r\nLine 2')
) AS v (id, class_id, name, notes);
INSERT INTO infrastruktur.server (id) VALUES ('00000000-0000-0000-0000-000000000001');
INSERT INTO infrastruktur.vm (id) VALUES ('00000000-0000-0000-0000-000000000003');
INSERT INTO infrastruktur.app (id, notes) VALUES ('00000000-0000-0000-0000-000000000004', 'own field');
"#;

/// (class key, field key) → validation
async fn text_fields(pool: &PgPool) -> Vec<(String, String, Option<Value>)> {
    sqlx::query_as(
        "SELECT c.key, d.key, d.validation FROM ci_attribute_definitions d JOIN ci_classes c ON c.id = d.class_id
         WHERE d.key LIKE 'notes%' ORDER BY c.key, d.key",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn notes_fields_from_0016_become_multiline() {
    let Some(db) = scratch::empty("notes_fields_from_0016_become_multiline").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(15, pool).await.expect("migrations up to 0015");
    pool.execute(sqlx::AssertSqlSafe(BEFORE)).await.expect("data before the upgrade");
    MIGRATOR.run_to(17, pool).await.expect("migrations up to 0017");

    // After 0016: an administrator cleared the rules of one migrated field and
    // created a text field "notes" and a number field "notes_3" of their own.
    pool.execute(
        "UPDATE ci_attribute_definitions SET validation = NULL
           WHERE key = 'notes_2' AND class_id = '00000000-0000-0000-0000-00000000c004';
         INSERT INTO ci_attribute_definitions (class_id, key, label, data_type, validation) VALUES
           ('00000000-0000-0000-0000-00000000c003', 'notes', 'Notes', 'text', '{\"maxLength\": 4000}'),
           ('00000000-0000-0000-0000-00000000c002', 'notes_3', 'Notes count', 'number', NULL);",
    )
    .await
    .unwrap();
    assert_eq!(
        text_fields(pool).await,
        [
            ("app".into(), "notes".into(), None),
            ("app".into(), "notes_2".into(), None),
            ("server".into(), "notes".into(), Some(json!({ "maxLength": 4000 }))),
            ("server".into(), "notes_3".into(), None),
            ("vm".into(), "notes".into(), Some(json!({ "maxLength": 4000 }))),
        ]
    );

    MIGRATOR.run(pool).await.expect("migration 0018");

    assert_eq!(
        text_fields(pool).await,
        [
            ("app".into(), "notes".into(), None),
            ("app".into(), "notes_2".into(), Some(json!({ "multiline": true }))),
            ("server".into(), "notes".into(), Some(json!({ "maxLength": 4000, "multiline": true }))),
            ("server".into(), "notes_3".into(), None),
            ("vm".into(), "notes".into(), Some(json!({ "maxLength": 4000 }))),
        ],
        "only the fields 0016 created from the notes column"
    );

    // The line breaks came through 0016 unchanged.
    let (server, app): (String, String) =
        sqlx::query_as("SELECT (SELECT notes FROM infrastruktur.server), (SELECT notes_2 FROM infrastruktur.app)")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!((server.as_str(), app.as_str()), ("Rack A01\nSlot 4", "Line 1\r\nLine 2"));

    let audit: Vec<(String, String, Value, Value)> = sqlx::query_as(
        "SELECT l.actor_name, c.key, l.old_value, l.new_value
         FROM audit_log l JOIN ci_attribute_definitions d ON d.id = l.entity_id JOIN ci_classes c ON c.id = d.class_id
         WHERE l.entity_type = 'ci_attribute_definitions' AND l.action = 'update' ORDER BY c.key",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(
        audit,
        [
            (
                "migration 0018".into(),
                "app".into(),
                json!({ "validation": null }),
                json!({ "validation": { "multiline": true } })
            ),
            (
                "migration 0018".into(),
                "server".into(),
                json!({ "validation": { "maxLength": 4000 } }),
                json!({ "validation": { "maxLength": 4000, "multiline": true } })
            ),
        ]
    );
    db.drop().await;
}

#[tokio::test]
async fn fresh_install_is_unchanged_by_0018() {
    let Some(db) = scratch::empty("fresh_install_is_unchanged_by_0018").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(17, pool).await.expect("migrations up to 0017");
    MIGRATOR.run(pool).await.expect("migration 0018");
    let audited: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE actor_name = 'migration 0018'")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(audited, 0);
    db.drop().await;
}
