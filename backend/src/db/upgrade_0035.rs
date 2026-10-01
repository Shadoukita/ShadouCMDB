//! Migration 0035 (GH#354) against an install that has the IT infrastructure
//! template's Application field "criticality" with values: values that match
//! the criticality list move onto the core field (audited), the core field
//! wins where it is already set, nothing is deleted, and the field is archived
//! with a report of what did not map. Other classes' fields of that key stay.

use serde_json::{Value, json};
use sqlx::{Executor, PgPool};
use uuid::Uuid;

use crate::db::{MIGRATOR, scratch};

const APP: &str = "00000000-0000-0000-0000-00000000c001";
const BIZ: &str = "00000000-0000-0000-0000-00000000c002";

/// Application, a subclass and a server (with a "criticality" field of its own)
/// as the template and an administrator left them before 0035. The enum check
/// is left out of the tables so a value an administrator added can be stored.
const BEFORE: &str = r#"
INSERT INTO areas (key, name) VALUES ('infrastruktur', 'Infrastruktur');
CREATE SCHEMA infrastruktur;
INSERT INTO ci_classes (id, key, name, area_id, is_abstract, parent_id) VALUES
  ('00000000-0000-0000-0000-00000000c001', 'application', 'Application', (SELECT id FROM areas WHERE key = 'infrastruktur'), false, NULL),
  ('00000000-0000-0000-0000-00000000c002', 'business_app', 'Business application', (SELECT id FROM areas WHERE key = 'infrastruktur'), false,
   '00000000-0000-0000-0000-00000000c001'),
  ('00000000-0000-0000-0000-00000000c003', 'server', 'Server', (SELECT id FROM areas WHERE key = 'infrastruktur'), false, NULL);
INSERT INTO ci_attribute_definitions (class_id, key, label, data_type, enum_values) VALUES
  ('00000000-0000-0000-0000-00000000c001', 'criticality', 'Criticality', 'enum', '["low","medium","high","critical","urgent"]'),
  ('00000000-0000-0000-0000-00000000c003', 'criticality', 'Criticality', 'enum', '["low","high"]');
CREATE TABLE infrastruktur.application (id uuid PRIMARY KEY REFERENCES configuration_items (id) ON DELETE CASCADE, criticality text);
CREATE TABLE infrastruktur.business_app (id uuid PRIMARY KEY REFERENCES configuration_items (id) ON DELETE CASCADE);
CREATE TABLE infrastruktur.server (id uuid PRIMARY KEY REFERENCES configuration_items (id) ON DELETE CASCADE, criticality text);
INSERT INTO configuration_items (id, label, class_id, criticality_value_id) VALUES
  ('00000000-0000-0000-0000-000000000001', 'ci-1', '00000000-0000-0000-0000-00000000c001', NULL),
  ('00000000-0000-0000-0000-000000000002', 'ci-2', '00000000-0000-0000-0000-00000000c002', NULL),
  ('00000000-0000-0000-0000-000000000003', 'ci-3', '00000000-0000-0000-0000-00000000c001',
   (SELECT v.id FROM lookup_list_values v JOIN lookup_lists l ON l.id = v.list_id WHERE l.system_role = 'criticality' AND v.key = 'medium')),
  ('00000000-0000-0000-0000-000000000004', 'ci-4', '00000000-0000-0000-0000-00000000c001',
   (SELECT v.id FROM lookup_list_values v JOIN lookup_lists l ON l.id = v.list_id WHERE l.system_role = 'criticality' AND v.key = 'high')),
  ('00000000-0000-0000-0000-000000000005', 'ci-5', '00000000-0000-0000-0000-00000000c001', NULL),
  ('00000000-0000-0000-0000-000000000006', 'ci-6', '00000000-0000-0000-0000-00000000c001', NULL),
  ('00000000-0000-0000-0000-000000000007', 'ci-7', '00000000-0000-0000-0000-00000000c003', NULL);
INSERT INTO infrastruktur.application (id, criticality) VALUES
  ('00000000-0000-0000-0000-000000000001', 'high'),
  ('00000000-0000-0000-0000-000000000002', 'critical'),
  ('00000000-0000-0000-0000-000000000003', 'medium'),
  ('00000000-0000-0000-0000-000000000004', 'low'),
  ('00000000-0000-0000-0000-000000000005', 'urgent'),
  ('00000000-0000-0000-0000-000000000006', NULL);
INSERT INTO infrastruktur.business_app (id) VALUES ('00000000-0000-0000-0000-000000000002');
INSERT INTO infrastruktur.server (id, criticality) VALUES ('00000000-0000-0000-0000-000000000007', 'high');
"#;

fn ci(n: u8) -> Uuid {
    format!("00000000-0000-0000-0000-{n:012}").parse().unwrap()
}

/// CI number -> (core criticality key, version)
async fn core(pool: &PgPool) -> Vec<(Uuid, Option<String>, i32)> {
    sqlx::query_as(
        "SELECT ci.id, v.key, ci.version FROM configuration_items ci
         LEFT JOIN lookup_list_values v ON v.id = ci.criticality_value_id ORDER BY ci.id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn application_criticality_moves_to_the_core_field() {
    let Some(db) = scratch::empty("application_criticality_moves_to_the_core_field").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(34, pool).await.expect("migrations up to 0034");
    pool.execute(sqlx::AssertSqlSafe(BEFORE)).await.expect("data before the upgrade");

    MIGRATOR.run(pool).await.expect("migration 0035");

    let s = |k: &str| Some(k.to_owned());
    assert_eq!(
        core(pool).await,
        [
            (ci(1), s("high"), 2),
            (ci(2), s("critical"), 2),
            (ci(3), s("medium"), 1),
            (ci(4), s("high"), 1),
            (ci(5), None, 1),
            (ci(6), None, 1),
            (ci(7), None, 1),
        ],
        "mapped where empty, the core value wins, other classes untouched"
    );

    // Nothing is deleted: every value is still in its column.
    let kept: Vec<(Uuid, Option<String>)> =
        sqlx::query_as("SELECT id, criticality FROM infrastruktur.application ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap();
    assert_eq!(kept.iter().filter(|(_, v)| v.is_some()).count(), 5);
    let server: Option<String> =
        sqlx::query_scalar("SELECT criticality FROM infrastruktur.server").fetch_one(pool).await.unwrap();
    assert_eq!(server.as_deref(), Some("high"));

    let active: Vec<(String, bool)> = sqlx::query_as(
        "SELECT c.key, d.is_active FROM ci_attribute_definitions d JOIN ci_classes c ON c.id = d.class_id
         WHERE d.key = 'criticality' ORDER BY c.key",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(active, [("application".into(), false), ("server".into(), true)]);

    let ci_audit: Vec<(Uuid, String, Value, Value)> = sqlx::query_as(
        "SELECT entity_id, actor_name, old_value, new_value - 'criticality' || jsonb_build_object('key', new_value -> 'criticality' ->> 'key')
         FROM audit_log WHERE entity_type = 'configuration_items' AND action = 'update' ORDER BY entity_id",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let entry = |n: u8, class: &str, key: &str| {
        (
            ci(n),
            "migration 0035".to_owned(),
            json!({ "classId": class, "criticality": null }),
            json!({ "classId": class, "key": key, "from": { "field": "attributes.criticality", "value": key } }),
        )
    };
    assert_eq!(ci_audit, [entry(1, APP, "high"), entry(2, BIZ, "critical")]);

    let field_audit: Vec<(Value, Value)> = sqlx::query_as(
        "SELECT old_value, new_value FROM audit_log WHERE entity_type = 'ci_attribute_definitions' AND action = 'update'",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(
        field_audit,
        [(
            json!({ "isActive": true }),
            json!({
                "isActive": false,
                "reason": "Superseded by the core Criticality field of every CI",
                "criticalitySet": 2,
                "alreadySet": 1,
                "keptCoreValue": 1,
                "notMapped": 1,
                "notMappedValues": ["urgent"],
            })
        )]
    );
    db.drop().await;
}

/// A lookup field (an administrator re-typed it) maps by its value's key or
/// name; an install without the field is left alone.
#[tokio::test]
async fn lookup_typed_field_maps_by_value_key_or_name() {
    let Some(db) = scratch::empty("lookup_typed_field_maps_by_value_key_or_name").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(34, pool).await.expect("migrations up to 0034");
    pool.execute(
        r#"
INSERT INTO areas (key, name) VALUES ('apps', 'Apps');
CREATE SCHEMA apps;
INSERT INTO lookup_lists (id, key, name) VALUES ('00000000-0000-0000-0000-00000000f001', 'app_crit', 'Application criticality');
INSERT INTO lookup_list_values (id, list_id, key, name, sort_order) VALUES
  ('00000000-0000-0000-0000-00000000f011', '00000000-0000-0000-0000-00000000f001', 'p1', ' CRITICAL ', 10),
  ('00000000-0000-0000-0000-00000000f012', '00000000-0000-0000-0000-00000000f001', 'low', 'Not important', 20);
INSERT INTO ci_classes (id, key, name, area_id, is_abstract) VALUES
  ('00000000-0000-0000-0000-00000000c001', 'application', 'Application', (SELECT id FROM areas WHERE key = 'apps'), false);
INSERT INTO ci_attribute_definitions (class_id, key, label, data_type, lookup_list_id) VALUES
  ('00000000-0000-0000-0000-00000000c001', 'criticality', 'Criticality', 'lookup', '00000000-0000-0000-0000-00000000f001');
CREATE TABLE apps.application (id uuid PRIMARY KEY REFERENCES configuration_items (id) ON DELETE CASCADE, criticality uuid);
INSERT INTO configuration_items (id, label, class_id) VALUES
  ('00000000-0000-0000-0000-000000000001', 'ci-1', '00000000-0000-0000-0000-00000000c001'),
  ('00000000-0000-0000-0000-000000000002', 'ci-2', '00000000-0000-0000-0000-00000000c001');
INSERT INTO apps.application (id, criticality) VALUES
  ('00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-00000000f011'),
  ('00000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-00000000f012');
"#,
    )
    .await
    .expect("data before the upgrade");

    MIGRATOR.run(pool).await.expect("migration 0035");

    let s = |k: &str| Some(k.to_owned());
    assert_eq!(core(pool).await, [(ci(1), s("critical"), 2), (ci(2), s("low"), 2)]);
    let active: bool = sqlx::query_scalar("SELECT is_active FROM ci_attribute_definitions WHERE key = 'criticality'")
        .fetch_one(pool)
        .await
        .unwrap();
    assert!(!active);
    db.drop().await;
}
