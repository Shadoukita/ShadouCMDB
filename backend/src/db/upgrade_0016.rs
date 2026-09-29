//! Migration 0016 (barebone CI core) against a database holding data in the
//! old fixed columns: every value must arrive in a class field, nothing lost.

use serde_json::{Value, json};
use sqlx::{Executor, PgPool};
use uuid::Uuid;

use crate::api::context::RequestContext;
use crate::db::{MIGRATOR, scratch};

/// The data model and CIs of an install at migration 0015, in its own SQL.
const BEFORE: &str = r#"
INSERT INTO areas (key, name) VALUES ('infrastruktur', 'Infrastruktur');
CREATE SCHEMA infrastruktur;
INSERT INTO ci_classes (id, key, name, area_id, is_abstract, parent_id) VALUES
  ('00000000-0000-0000-0000-00000000c001', 'hardware', 'Hardware', (SELECT id FROM areas), true, NULL),
  ('00000000-0000-0000-0000-00000000c002', 'server', 'Server', (SELECT id FROM areas), false, '00000000-0000-0000-0000-00000000c001'),
  ('00000000-0000-0000-0000-00000000c003', 'vm', 'VM', (SELECT id FROM areas), false, NULL),
  ('00000000-0000-0000-0000-00000000c004', 'app', 'App', (SELECT id FROM areas), false, NULL),
  ('00000000-0000-0000-0000-00000000c005', 'empty_root', 'Empty', (SELECT id FROM areas), false, NULL);
-- A field that already uses one of the migrated keys.
INSERT INTO ci_attribute_definitions (class_id, key, label, data_type)
VALUES ('00000000-0000-0000-0000-00000000c003', 'hostname', 'Custom hostname', 'text');
CREATE TABLE infrastruktur.hardware (id uuid PRIMARY KEY REFERENCES configuration_items (id) ON DELETE CASCADE);
CREATE TABLE infrastruktur.server (id uuid PRIMARY KEY REFERENCES configuration_items (id) ON DELETE CASCADE);
CREATE TABLE infrastruktur.vm (id uuid PRIMARY KEY REFERENCES configuration_items (id) ON DELETE CASCADE, hostname text);
CREATE TABLE infrastruktur.app (id uuid PRIMARY KEY REFERENCES configuration_items (id) ON DELETE CASCADE);
CREATE TABLE infrastruktur.empty_root (id uuid PRIMARY KEY REFERENCES configuration_items (id) ON DELETE CASCADE);

INSERT INTO statuses (id, key, name, is_operational, sort_order) VALUES
  ('00000000-0000-0000-0000-00000000a001', 'in_service', 'In service', true, 10),
  ('00000000-0000-0000-0000-00000000a002', 'retired', 'Retired', false, 20);
INSERT INTO environments (id, key, name) VALUES ('00000000-0000-0000-0000-00000000b001', 'production', 'Production');
INSERT INTO owners (id, kind, name, email) VALUES
  ('00000000-0000-0000-0000-00000000d001', 'team', 'Ops Team', 'ops@example.com'),
  ('00000000-0000-0000-0000-00000000d002', 'person', 'Ops-Team!', NULL),
  ('00000000-0000-0000-0000-00000000d003', 'person', '42 Ümit', NULL);
INSERT INTO locations (id, key, name, location_type, address) VALUES
  ('00000000-0000-0000-0000-00000000e001', 'fra1', 'Frankfurt DC 1', 'site', 'Frankfurt am Main');
-- A list an administrator made earlier takes the key "status".
INSERT INTO lookup_lists (key, name) VALUES ('status', 'Ticket status');

INSERT INTO configuration_items (id, class_id, name, status_id, environment_id, owner_id, location_id,
                                 hostname, ip_address, serial_number, notes, created_at, deleted_at) VALUES
  ('00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-00000000c002', 'srv-1',
   '00000000-0000-0000-0000-00000000a001', '00000000-0000-0000-0000-00000000b001', '00000000-0000-0000-0000-00000000d001',
   '00000000-0000-0000-0000-00000000e001', 'srv1.example.com', '10.0.0.1', 'SN-1', 'Rack A01', '2024-01-02T03:04:05Z', NULL),
  ('00000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-00000000c002', 'srv-2 (old)',
   '00000000-0000-0000-0000-00000000a002', NULL, '00000000-0000-0000-0000-00000000d003', NULL,
   NULL, '10.0.0.0/24', NULL, NULL, '2023-05-06T07:08:09Z', now()),
  ('00000000-0000-0000-0000-000000000003', '00000000-0000-0000-0000-00000000c003', 'vm-1',
   '00000000-0000-0000-0000-00000000a001', NULL, NULL, NULL, 'vm1', NULL, NULL, NULL, now(), NULL),
  ('00000000-0000-0000-0000-000000000004', '00000000-0000-0000-0000-00000000c004', 'CRM',
   '00000000-0000-0000-0000-00000000a002', NULL, NULL, NULL, NULL, NULL, NULL, 'Customer relationship management', now(), NULL);
INSERT INTO infrastruktur.hardware (id) VALUES ('00000000-0000-0000-0000-000000000001'), ('00000000-0000-0000-0000-000000000002');
INSERT INTO infrastruktur.server (id) VALUES ('00000000-0000-0000-0000-000000000001'), ('00000000-0000-0000-0000-000000000002');
INSERT INTO infrastruktur.vm (id, hostname) VALUES ('00000000-0000-0000-0000-000000000003', 'custom');
INSERT INTO infrastruktur.app (id) VALUES ('00000000-0000-0000-0000-000000000004');
"#;

fn settings_before() -> Value {
    json!({
        "branding": { "appName": "Acme CMDB" },
        "dashboard": { "widgets": [
            { "id": "by_status", "type": "count_by_status", "size": "small" },
            { "id": "by_env", "type": "count_by_environment" },
            { "id": "prod", "type": "saved_search", "search": {
                "classKeys": ["server"], "includeSubclasses": true,
                "filters": { "q": "srv", "environmentKeys": ["production"], "statusKeys": [] },
                "sort": { "field": "serialNumber", "direction": "desc" } } }
        ] },
        "listViews": [{
            "classKey": "server",
            "columns": ["name", "status", "hostname", "ipAddress", "createdAt"],
            "defaultSort": { "field": "hostname", "direction": "asc" },
            "defaultFilters": { "statusKeys": ["in_service"], "locationKeys": ["fra1"] }
        }],
        "layouts": [{
            "classKey": "server",
            "panels": [{ "key": "main", "label": "Main", "fields": ["name", "attributes.name", "hostname", "serialNumber"] }],
            "hiddenFields": ["notes"],
            "readOnlyFields": ["owner"]
        }]
    })
}

async fn scalar<T>(pool: &PgPool, sql: &str) -> T
where
    T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin,
{
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

/// (class key, field key, data type, lookup list key, required)
async fn fields(pool: &PgPool) -> Vec<(String, String, String, Option<String>, bool)> {
    sqlx::query_as(
        "SELECT c.key, d.key, d.data_type, l.key, d.is_required
         FROM ci_attribute_definitions d JOIN ci_classes c ON c.id = d.class_id
         LEFT JOIN lookup_lists l ON l.id = d.lookup_list_id
         ORDER BY c.key, d.sort_order, d.key",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn fixed_columns_become_class_fields_without_losing_a_value() {
    let Some(db) = scratch::empty("fixed_columns_become_class_fields_without_losing_a_value").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(15, pool).await.expect("migrations up to 0015");
    pool.execute(sqlx::AssertSqlSafe(BEFORE)).await.expect("data before the upgrade");
    sqlx::query(
        "INSERT INTO ui_settings_versions (version, settings, actor_type) VALUES (2, $1, 'user');
         ",
    )
    .bind(settings_before())
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("UPDATE ui_settings SET version = 2, settings = $1")
        .bind(settings_before())
        .execute(pool)
        .await
        .unwrap();

    let stamps = "SELECT string_agg(id::text || updated_at::text, ',' ORDER BY id) FROM (
                    SELECT id, updated_at FROM configuration_items UNION ALL SELECT id, updated_at FROM ci_classes) x";
    let before: String = scalar(pool, stamps).await;
    // The operator's check, part 1 (sql/checks/).
    pool.execute(sqlx::AssertSqlSafe(include_str!("../../../sql/checks/core_ci_upgrade_1_before.sql"))).await.unwrap();

    MIGRATOR.run_to(16, pool).await.expect("migration 0016");

    let after: String = scalar(pool, stamps).await;
    assert_eq!(before, after, "the backfills are not edits: updated_at is kept");

    // Part 2 finds every recorded value in a class field and every former name as the label.
    let check = include_str!("../../../sql/checks/core_ci_upgrade_2_after.sql");
    let (setup, _) = check.split_once("SELECT (SELECT count(*)").unwrap();
    let mut c = pool.acquire().await.unwrap();
    c.execute(sqlx::AssertSqlSafe(setup)).await.unwrap();
    let (recorded, missing, wrong_labels): (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM public.core_ci_upgrade_before),
                (SELECT count(*) FROM (SELECT * FROM public.core_ci_upgrade_before EXCEPT SELECT * FROM core_ci_upgrade_after) m),
                (SELECT count(*) FROM public.core_ci_upgrade_before b JOIN configuration_items ci ON ci.id = b.ci_id
                  WHERE b.field = 'name' AND ci.label IS DISTINCT FROM b.value)",
    )
    .fetch_one(&mut *c)
    .await
    .unwrap();
    assert_eq!((recorded, missing, wrong_labels), (19, 0, 0));
    c.execute("DROP TABLE core_ci_upgrade_after").await.unwrap();
    c.execute(check).await.expect("the whole check script runs");
    drop(c);

    // Old columns gone; new core columns filled.
    let old: i64 = scalar(
        pool,
        "SELECT count(*) FROM information_schema.columns WHERE table_schema = 'cmdb' AND table_name = 'configuration_items'
           AND column_name IN ('name', 'status_id', 'environment_id', 'owner_id', 'location_id', 'hostname',
                               'ip_address', 'serial_number', 'notes')",
    )
    .await;
    assert_eq!(old, 0);
    let rows: Vec<(Uuid, String, String, bool, bool)> = sqlx::query_as(
        "SELECT id, ident, label, valid_from = created_at, valid_until IS NULL FROM configuration_items ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let ident = regex::Regex::new("^CI-[0-9A-HJKMNP-TV-Z]{8}$").unwrap();
    assert_eq!(rows.len(), 4);
    for (id, i, _, from_created, open) in &rows {
        assert!(ident.is_match(i), "{id}: {i}");
        assert!(*from_created && *open, "{id}: validity");
    }
    let labels: Vec<&str> = rows.iter().map(|r| r.2.as_str()).collect();
    assert_eq!(labels, ["srv-1", "srv-2 (old)", "vm-1", "CRM"]);
    let distinct: i64 = scalar(pool, "SELECT count(DISTINCT lower(ident)) FROM configuration_items").await;
    assert_eq!(distinct, 4);

    // Fields: name on every root class; the others on the topmost classes holding values.
    let f = fields(pool).await;
    let has = |class: &str, key: &str| f.iter().find(|x| x.0 == class && x.1 == key).cloned();
    for root in ["hardware", "vm", "app", "empty_root"] {
        let name = has(root, "name").unwrap_or_else(|| panic!("{root}.name: {f:?}"));
        assert_eq!((name.2.as_str(), name.4), ("text", true));
    }
    assert!(has("server", "name").is_none(), "inherited from hardware");
    for (class, key) in [("server", "status"), ("vm", "status"), ("app", "status")] {
        let s = has(class, key).unwrap_or_else(|| panic!("{class}.{key}: {f:?}"));
        assert_eq!((s.2.as_str(), s.3.as_deref(), s.4), ("lookup", Some("status_2"), true), "{class}");
    }
    assert!(has("hardware", "status").is_none() && has("empty_root", "status").is_none(), "{f:?}");
    for key in ["environment", "owner", "location", "hostname", "ip_address", "serial_number", "notes"] {
        let holders: Vec<&str> =
            f.iter().filter(|x| x.1 == key || x.1 == format!("{key}_2")).map(|x| x.0.as_str()).collect();
        let expected: &[&str] = match key {
            "hostname" => &["server", "vm", "vm"],
            "notes" => &["app", "server"],
            _ => &["server"],
        };
        assert_eq!(holders, expected, "{key}");
    }
    assert_eq!(has("vm", "hostname_2").map(|x| x.2), Some("text".into()), "clash with the existing field");
    assert_eq!(has("server", "owner").and_then(|x| x.3), Some("owner".into()));

    // Values, the soft-deleted CI's included.
    type ServerRow = (Uuid, String, Uuid, Option<Uuid>, Option<Uuid>, Option<Uuid>, Option<String>, Option<String>);
    let server: Vec<ServerRow> =
        sqlx::query_as(
            "SELECT h.id, h.name, s.status, s.environment, s.owner, s.location, s.hostname, host(s.ip_address) || '/' || masklen(s.ip_address)
             FROM infrastruktur.hardware h JOIN infrastruktur.server s ON s.id = h.id ORDER BY h.id",
        )
        .fetch_all(pool)
        .await
        .unwrap();
    let id = |n: &str| Uuid::parse_str(n).unwrap();
    assert_eq!(server.len(), 2);
    assert_eq!(server[0].1, "srv-1");
    assert_eq!(server[0].2, id("00000000-0000-0000-0000-00000000a001"), "same id as the statuses row");
    assert_eq!(server[0].3, Some(id("00000000-0000-0000-0000-00000000b001")));
    assert_eq!(server[0].4, Some(id("00000000-0000-0000-0000-00000000d001")));
    assert_eq!(server[0].5, Some(id("00000000-0000-0000-0000-00000000e001")));
    assert_eq!(server[0].6.as_deref(), Some("srv1.example.com"));
    assert_eq!(server[0].7.as_deref(), Some("10.0.0.1/32"));
    assert_eq!((server[1].1.as_str(), server[1].2), ("srv-2 (old)", id("00000000-0000-0000-0000-00000000a002")));
    assert_eq!(server[1].7.as_deref(), Some("10.0.0.0/24"));
    let (serial, notes): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT serial_number, notes FROM infrastruktur.server WHERE id = '00000000-0000-0000-0000-000000000001'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!((serial.as_deref(), notes.as_deref()), (Some("SN-1"), Some("Rack A01")));
    let vm: (String, Option<String>, Option<String>) =
        sqlx::query_as("SELECT name, hostname, hostname_2 FROM infrastruktur.vm").fetch_one(pool).await.unwrap();
    assert_eq!(vm, ("vm-1".into(), Some("custom".into()), Some("vm1".into())));
    let app: (String, Option<String>) =
        sqlx::query_as("SELECT name, notes FROM infrastruktur.app").fetch_one(pool).await.unwrap();
    assert_eq!(app, ("CRM".into(), Some("Customer relationship management".into())));

    // Lookup lists keep the ids; owners get keys.
    let owners: Vec<(Uuid, String, String, Option<String>)> = sqlx::query_as(
        "SELECT v.id, v.key, v.name, v.description FROM lookup_list_values v JOIN lookup_lists l ON l.id = v.list_id
         WHERE l.key = 'owner' ORDER BY v.sort_order",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let keys: Vec<&str> = owners.iter().map(|o| o.1.as_str()).collect();
    assert_eq!(keys, ["owner_42_mit", "ops_team", "ops_team_2"]);
    assert_eq!(owners[1].3.as_deref(), Some("Team, ops@example.com"));
    let statuses: i64 = scalar(
        pool,
        "SELECT count(*) FROM lookup_list_values v JOIN lookup_lists l ON l.id = v.list_id
         WHERE l.key = 'status_2' AND v.id IN (SELECT id FROM statuses)",
    )
    .await;
    assert_eq!(statuses, 2);
    let old_tables: i64 = scalar(pool, "SELECT (SELECT count(*) FROM statuses) + (SELECT count(*) FROM owners)").await;
    assert_eq!(old_tables, 5, "the old tables are kept");

    // Title attribute: the root's name, on every class of its lineage.
    let titles: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT c.key, d.key FROM ci_classes c LEFT JOIN ci_attribute_definitions d ON d.id = c.title_attribute_id ORDER BY c.key",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert!(titles.iter().all(|(_, t)| t.as_deref() == Some("name")), "{titles:?}");
    let server_title: String = scalar(
        pool,
        "SELECT c.key FROM ci_classes s JOIN ci_attribute_definitions d ON d.id = s.title_attribute_id
         JOIN ci_classes c ON c.id = d.class_id WHERE s.key = 'server'",
    )
    .await;
    assert_eq!(server_title, "hardware");

    // Search vector over label and ident.
    let found: i64 =
        scalar(pool, "SELECT count(*) FROM configuration_items WHERE search_vector @@ to_tsquery('simple', 'crm')")
            .await;
    assert_eq!(found, 1);

    // Recorded in the schema history, clash included.
    let impact: Value = scalar(pool, "SELECT impact FROM schema_changes WHERE actor_name = 'migration 0016'").await;
    let text = impact.to_string();
    assert!(text.contains("hostname_2") && text.contains("data_moved"), "{text}");

    // UI settings: a new version with the new field names.
    let (version, s): (i32, Value) =
        sqlx::query_as("SELECT version, settings FROM ui_settings").fetch_one(pool).await.unwrap();
    assert_eq!(version, 3);
    assert_eq!(s["branding"], json!({ "appName": "Acme CMDB" }));
    assert_eq!(
        s["listViews"][0]["columns"],
        json!(["label", "attributes.status", "attributes.hostname", "attributes.ip_address", "createdAt"])
    );
    assert_eq!(s["listViews"][0]["defaultSort"], json!({ "field": "label", "direction": "asc" }));
    assert_eq!(
        s["listViews"][0]["defaultFilters"],
        json!({ "lookups": { "status_2": ["in_service"], "location": ["fra1"] } })
    );
    let widgets = &s["dashboard"]["widgets"];
    assert_eq!(
        widgets[0],
        json!({ "id": "by_status", "type": "count_by_lookup", "lookupListKey": "status_2", "size": "small" })
    );
    assert_eq!(widgets[1]["lookupListKey"], json!("environment"));
    assert_eq!(widgets[2]["search"]["filters"], json!({ "q": "srv", "lookups": { "environment": ["production"] } }));
    assert_eq!(widgets[2]["search"]["sort"]["field"], json!("label"));
    let layout = &s["layouts"][0];
    assert_eq!(
        layout["panels"][0]["fields"],
        json!(["attributes.name", "attributes.hostname", "attributes.serial_number"])
    );
    assert_eq!(layout["hiddenFields"], json!(["attributes.notes"]));
    assert_eq!(layout["readOnlyFields"], json!(["attributes.owner"]));

    // The engine rebuilds the reporting views on the new registry columns (after
    // the newer migrations, as `shadoucmdb migrate` does: it records into their columns).
    MIGRATOR.run(pool).await.expect("the newer migrations");
    // 0028 (GH#252): the record gets a variant without its counts, shown to readers who may not view every type.
    let (summary, redacted, classes): (String, Value, Option<Vec<uuid::Uuid>>) = sqlx::query_as(
        "SELECT redacted_summary, redacted_impact, count_classes FROM schema_changes WHERE actor_name = 'migration 0016'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(summary, "Migration 0016: fixed CI columns moved into class fields");
    assert_eq!(classes, None);
    let moved: Vec<&Value> = redacted.as_array().unwrap().iter().filter(|i| i["kind"] == "data_moved").collect();
    assert!(!moved.is_empty(), "{redacted}");
    for i in moved {
        assert_eq!(i["rows"], Value::Null, "{i}");
        let message = i["message"].as_str().unwrap();
        assert!(message.starts_with("The values of configuration_items.") && message.ends_with("and verified"), "{i}");
    }
    let ctx = RequestContext::system("test", "test");
    let mut tx = pool.begin().await.unwrap();
    crate::schema::reconcile(&mut tx, &ctx, "after 0016").await.expect("reconcile");
    tx.commit().await.unwrap();
    let view: (String, String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT ident, label, name, (SELECT key FROM lookup_list_values WHERE key = v.status) FROM infrastruktur.v_server v
         WHERE id = '00000000-0000-0000-0000-000000000001'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!((view.1.as_str(), view.2.as_deref(), view.3.as_deref()), ("srv-1", Some("srv-1"), Some("in_service")));
    db.drop().await;
}
