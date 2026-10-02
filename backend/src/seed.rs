//! `shadoucmdb seed`: the rows every installation needs, optionally a starter
//! template and a small demo inventory.
//!
//! Since SHAA-31 a fresh install starts bare: no CI classes, attributes,
//! relationship types or business lookups. The administrator builds the data
//! model or installs a starter template (Administration > Templates, or
//! `seed --template it_infrastructure`). `--demo` installs the IT
//! infrastructure template and loads a sample inventory into a database that
//! has no CIs yet. Everything here is idempotent.

use std::collections::HashMap;

use anyhow::{Context, anyhow, bail};
use serde_json::{Map, Value, json};
use sqlx::postgres::{PgConnection, PgPool};
use uuid::Uuid;

use crate::api::context::RequestContext;
use crate::config::DatabaseConfig;
use crate::data::items;
use crate::modules::templates;
use crate::schema::model::{Field, Model};

/// Demo owners (values of the template's "owner" list): key, name, description
const OWNERS: &[(&str, &str, &str)] = &[
    ("infrastructure", "Infrastructure", "Team, infra@example.com"),
    ("platform", "Platform Engineering", "Team, platform@example.com"),
    ("dba", "Database Administration", "Team, dba@example.com"),
];

type KeyMap = HashMap<String, Uuid>;

/// `key -> id` for one of the keyed tables (the name is always a constant).
async fn key_map(c: &mut PgConnection, table: &'static str) -> sqlx::Result<KeyMap> {
    let rows: Vec<(String, Uuid)> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT key, id FROM {table}"))).fetch_all(c).await?;
    Ok(rows.into_iter().collect())
}

fn must(map: &KeyMap, key: &str) -> anyhow::Result<Uuid> {
    map.get(key).copied().ok_or_else(|| anyhow!("seed: unknown key \"{key}\""))
}

/// Rows the application itself relies on. The built-in Administrator profile
/// is created by migration 0003; this checks it is there. New system rows
/// (never business content) belong here.
pub async fn seed_system_rows(pool: &PgPool) -> anyhow::Result<()> {
    let builtin: i64 =
        sqlx::query_scalar("SELECT count(*) FROM permission_profiles WHERE is_builtin").fetch_one(pool).await?;
    if builtin != 1 {
        bail!("the built-in Administrator profile is missing; run `shadoucmdb migrate` first");
    }
    Ok(())
}

/// Installs a starter template as the system actor (audited as "seed").
pub async fn install_template(pool: &PgPool, key: &str) -> anyhow::Result<templates::TemplateInstallResult> {
    let template = templates::find(key).ok_or_else(|| {
        let known: Vec<&str> = templates::TEMPLATES.iter().map(|t| t.key).collect();
        anyhow!("unknown template \"{key}\" (available: {})", known.join(", "))
    })?;
    let ctx = RequestContext::system("seed", format!("seed-{}", Uuid::new_v4()));
    let mut tx = pool.begin().await?;
    let result = templates::install(&mut tx, &ctx, template).await.map_err(|e| anyhow!("{}", e.message))?;
    tx.commit().await?;
    Ok(result)
}

/// The fields every demo CI may have (lookup values by key); `None` is left empty.
#[derive(Default)]
struct DemoCi<'a> {
    class: &'a str,
    name: &'a str,
    status: Option<&'a str>,
    environment: Option<&'a str>,
    owner: Option<&'a str>,
    location: Option<&'a str>,
    hostname: Option<&'a str>,
    ip_address: Option<&'a str>,
    serial_number: Option<&'a str>,
    /// Key of a value of the criticality system list (the core field)
    criticality: Option<&'a str>,
}

enum Val<'a> {
    Text(&'a str),
    Number(&'a str),
    Bool(bool),
    Date(&'a str),
    Ip(&'a str),
    Cidr(&'a str),
    Ref(Uuid),
    Lookup(Uuid),
}

impl Val<'_> {
    fn text(&self) -> String {
        match self {
            Val::Text(v) | Val::Number(v) | Val::Date(v) | Val::Ip(v) | Val::Cidr(v) => (*v).to_owned(),
            Val::Bool(b) => b.to_string(),
            Val::Ref(id) | Val::Lookup(id) => id.to_string(),
        }
    }
}

/// Small sample inventory (and the owners it uses) on top of the IT
/// infrastructure template; only loaded into a database with no CIs.
pub async fn seed_demo_data(pool: &PgPool) -> anyhow::Result<bool> {
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM configuration_items").fetch_one(pool).await?;
    if n > 0 {
        return Ok(false);
    }

    let mut tx = pool.begin().await?;
    for (i, (key, name, description)) in OWNERS.iter().enumerate() {
        sqlx::query(
            "INSERT INTO lookup_list_values (list_id, key, name, description, sort_order)
             SELECT id, $1, $2, $3, $4 FROM lookup_lists WHERE key = 'owner'
             ON CONFLICT DO NOTHING",
        )
        .bind(key)
        .bind(name)
        .bind(description)
        .bind(i as i32 * 10)
        .execute(&mut *tx)
        .await?;
    }
    let cls = crate::modules::templates::template_classes(&mut tx).await?;
    let types = key_map(&mut tx, "relationship_types").await?;
    // "list.value" -> id
    let lookups: KeyMap = sqlx::query_as::<_, (String, Uuid)>(
        "SELECT l.key || '.' || v.key, v.id FROM lookup_list_values v JOIN lookup_lists l ON l.id = v.list_id",
    )
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .collect();
    let lookup = |list: &str, key: &str| must(&lookups, &format!("{list}.{key}"));
    // The criticality list is found by its system role: its key may differ (0031).
    let criticality: KeyMap = sqlx::query_as::<_, (String, Uuid)>(
        "SELECT v.key, v.id FROM lookup_list_values v JOIN lookup_lists l ON l.id = v.list_id
         WHERE l.system_role = 'criticality'",
    )
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .collect();

    let mut created: Vec<(Uuid, Uuid)> = Vec::new();
    let mut values: Vec<(Uuid, &str, Val)> = Vec::new();
    let mut insert = async |ci: DemoCi<'static>| -> anyhow::Result<Uuid> {
        let class_id = must(&cls, ci.class)?;
        let new = items::NewItem {
            id: None,
            class_id,
            ident: None,
            valid_from: None,
            valid_until: None,
            criticality_value_id: ci.criticality.map(|k| must(&criticality, k)).transpose()?,
        };
        let id = items::insert(&mut tx, &new).await?;
        values.push((id, "name", Val::Text(ci.name)));
        values.push((id, "status", Val::Lookup(lookup("status", ci.status.unwrap_or("in_service"))?)));
        for (key, list, v) in [
            ("environment", "environment", ci.environment),
            ("owner", "owner", ci.owner),
            ("location", "location", ci.location),
        ] {
            if let Some(v) = v {
                values.push((id, key, Val::Lookup(lookup(list, v)?)));
            }
        }
        if let Some(h) = ci.hostname {
            values.push((id, "hostname", Val::Text(h)));
        }
        if let Some(ip) = ci.ip_address {
            values.push((id, "ip_address", Val::Ip(ip)));
        }
        if let Some(sn) = ci.serial_number {
            values.push((id, "serial_number", Val::Text(sn)));
        }
        created.push((id, class_id));
        Ok(id)
    };

    let infra = Some("infrastructure");
    let platform = Some("platform");
    let prod = Some("production");
    let rack = insert(DemoCi { class: "location", name: "FRA1 Rack A01", ..Default::default() }).await?;
    let srv = insert(DemoCi {
        class: "server",
        name: "fra1-esx-01",
        hostname: Some("fra1-esx-01.example.internal"),
        ip_address: Some("10.10.1.11"),
        serial_number: Some("SN-DL380-0001"),
        environment: prod,
        owner: infra,
        location: Some("fra1_rack_a01"),
        ..Default::default()
    })
    .await?;
    let sw = insert(DemoCi {
        class: "network_device",
        name: "fra1-tor-a01",
        hostname: Some("fra1-tor-a01.example.internal"),
        ip_address: Some("10.10.0.2"),
        serial_number: Some("SN-N9K-0042"),
        environment: prod,
        owner: infra,
        location: Some("fra1_rack_a01"),
        ..Default::default()
    })
    .await?;
    let vm = insert(DemoCi {
        class: "virtual_machine",
        name: "crm-app-01",
        hostname: Some("crm-app-01.example.internal"),
        ip_address: Some("10.20.5.21"),
        environment: prod,
        owner: platform,
        ..Default::default()
    })
    .await?;
    let db1 = insert(DemoCi {
        class: "database",
        name: "crm-db",
        environment: prod,
        owner: Some("dba"),
        ..Default::default()
    })
    .await?;
    let app = insert(DemoCi {
        class: "application",
        name: "CRM",
        environment: prod,
        owner: platform,
        criticality: Some("high"),
        ..Default::default()
    })
    .await?;
    let svc = insert(DemoCi {
        class: "service",
        name: "Customer Relationship Management",
        environment: prod,
        owner: platform,
        ..Default::default()
    })
    .await?;
    insert(DemoCi {
        class: "server",
        name: "nyc1-old-01",
        status: Some("retired"),
        location: Some("nyc1"),
        serial_number: Some("SN-OLD-0007"),
        ..Default::default()
    })
    .await?;

    values.extend([
        (srv, "manufacturer", Val::Text("HPE")),
        (srv, "model", Val::Text("ProLiant DL380 Gen10")),
        (srv, "warranty_end", Val::Date("2028-03-31")),
        (srv, "cpu_cores", Val::Number("32")),
        (srv, "memory_gb", Val::Number("512")),
        (srv, "os_family", Val::Text("other")),
        (srv, "management_ip", Val::Ip("10.10.100.11")),
        (sw, "manufacturer", Val::Text("Cisco")),
        (sw, "device_role", Val::Text("switch")),
        (sw, "port_count", Val::Number("48")),
        (sw, "management_subnet", Val::Cidr("10.10.0.0/24")),
        (vm, "vcpu", Val::Number("8")),
        (vm, "memory_gb", Val::Number("32")),
        (vm, "os_family", Val::Text("linux")),
        (vm, "platform", Val::Text("vmware")),
        (db1, "engine", Val::Text("postgresql")),
        (db1, "engine_version", Val::Text("17.2")),
        (db1, "port", Val::Number("5432")),
        (db1, "backup_enabled", Val::Bool(true)),
        (app, "version", Val::Text("4.2.0")),
        (app, "primary_database", Val::Ref(db1)),
        (svc, "service_tier", Val::Text("tier_1")),
        (svc, "sla_uptime_percent", Val::Number("99.9")),
    ]);
    // One row per CI in the table of its class and of every ancestor, with its values.
    let model = Model::load(&mut tx).await?;
    for (ci, class_id) in &created {
        let mut written = Vec::new();
        for class in model.lineage(*class_id) {
            let table = model.table(class.id).context("seed: type without a table")?;
            let row: Vec<(&Field, Option<String>)> = model
                .own_fields(class.id)
                .filter_map(|f| {
                    values.iter().find(|(c, k, _)| c == ci && *k == f.key).map(|(_, _, v)| (f, Some(v.text())))
                })
                .collect();
            written.extend(row.iter().map(|(f, v)| (f.key.clone(), json!(v))));
            items::insert_type_row(&mut tx, &table, *ci, &row).await?;
        }
        items::refresh_labels(&mut tx, &model, &[*class_id], Some(&[*ci])).await?;
        let (ident, label): (String, String) =
            sqlx::query_as("SELECT ident, label FROM configuration_items WHERE id = $1")
                .bind(ci)
                .fetch_one(&mut *tx)
                .await?;
        // The inserted fields, as the API names them.
        let new_value = json!({
            "id": ci, "ident": ident, "label": label, "classId": class_id,
            "attributes": written.into_iter().collect::<Map<String, Value>>(),
        });
        sqlx::query(
            "INSERT INTO audit_log (actor_type, actor_name, action, entity_type, entity_id, new_value)
             VALUES ('system', 'seed', 'create', 'configuration_items', $1, $2)",
        )
        .bind(ci)
        .bind(new_value)
        .execute(&mut *tx)
        .await?;
    }

    let edges = [
        ("depends_on", svc, app),  // Service -> Application
        ("runs_on", app, vm),      // Application -> VM
        ("depends_on", app, db1),  // Application -> Database
        ("runs_on", vm, srv),      // VM -> Server
        ("runs_on", db1, vm),      // Database -> VM
        ("located_in", srv, rack), // Device -> Location
        ("located_in", sw, rack),
        ("connected_to", srv, sw),
    ];
    for (ty, source, target) in edges {
        sqlx::query(
            "INSERT INTO ci_relationships (relationship_type_id, source_ci_id, target_ci_id) VALUES ($1, $2, $3)",
        )
        .bind(must(&types, ty)?)
        .bind(source)
        .bind(target)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(true)
}

/// Rows per data-model table: `(table, total, built-in)`. Built-in rows carry
/// a `system_role` (the Business service class and member type, migration
/// 0033; the criticality list, 0031) and exist on every install. Attributes,
/// rules and list values have no such marker: the administrator and the
/// templates may add their own to a built-in class or list.
async fn data_model_counts(pool: &PgPool) -> sqlx::Result<Vec<(String, i64, i64)>> {
    sqlx::query_as(
        "SELECT 'ci_classes', count(*), count(*) FILTER (WHERE system_role IS NOT NULL) FROM ci_classes
         UNION ALL SELECT 'ci_attribute_definitions', count(*), 0 FROM ci_attribute_definitions
         UNION ALL SELECT 'relationship_types', count(*), count(*) FILTER (WHERE system_role IS NOT NULL)
           FROM relationship_types
         UNION ALL SELECT 'relationship_type_rules', count(*), 0 FROM relationship_type_rules
         UNION ALL SELECT 'lookup_lists', count(*), count(*) FILTER (WHERE system_role IS NOT NULL) FROM lookup_lists
         UNION ALL SELECT 'lookup_list_values', count(*), 0 FROM lookup_list_values",
    )
    .fetch_all(pool)
    .await
}

/// The data model is empty while no class without a system role exists: the
/// same rule as `dataModelEmpty()` in the web UI (SHAA-961).
fn data_model_empty(counts: &[(String, i64, i64)]) -> bool {
    counts.iter().any(|(table, total, builtin)| table == "ci_classes" && total == builtin)
}

/// The "Data model:" block `seed` prints, one line per table
/// (`  <table>: <total>`, plus ` (built-in: <n>)` when some rows are built
/// in), followed by the first-run hint while the data model is empty.
fn data_model_report(counts: &[(String, i64, i64)]) -> Vec<String> {
    let mut lines = vec!["Data model:".to_owned()];
    for (table, total, builtin) in counts {
        lines.push(if *builtin > 0 {
            format!("  {table}: {total} (built-in: {builtin})")
        } else {
            format!("  {table}: {total}")
        });
    }
    if data_model_empty(counts) {
        lines.push(
            "The data model is empty. Build it under Administration, or install a starter template: \
             shadoucmdb seed --template it_infrastructure"
                .to_owned(),
        );
    }
    lines
}

pub async fn run(cfg: &DatabaseConfig, template_keys: &[String], demo: bool) -> anyhow::Result<()> {
    let pool = crate::db::connect(cfg).await?;
    let result = async {
        seed_system_rows(&pool).await?;
        println!("System rows: ok");
        let mut keys: Vec<String> = template_keys.to_vec();
        if demo && !keys.iter().any(|k| k == "it_infrastructure") {
            keys.push("it_infrastructure".into());
        }
        for key in &keys {
            let r = install_template(&pool, key).await?;
            let c = &r.created;
            println!(
                "Template {key}: created {} classes, {} attributes, {} relationship types, {} rules, {} lookup lists, {} list values",
                c.classes,
                c.attribute_definitions,
                c.relationship_types,
                c.relationship_rules,
                c.lookup_lists,
                c.lookup_list_values
            );
            for s in &r.skipped {
                println!("  skipped {s}");
            }
        }
        for line in data_model_report(&data_model_counts(&pool).await?) {
            println!("{line}");
        }
        if demo {
            let loaded = seed_demo_data(&pool).await?;
            println!(
                "{}",
                if loaded {
                    "Demo inventory loaded."
                } else {
                    "Demo inventory skipped: database already contains CIs."
                }
            );
        }
        anyhow::Ok(())
    }
    .await;
    pool.close().await;
    result
}

#[cfg(test)]
mod tests {
    use super::{data_model_counts, data_model_report, install_template};
    use crate::db::scratch;

    const HINT: &str = "The data model is empty.";

    /// GH#386: on a fresh install only built-in rows exist (migrations 0031,
    /// 0033), and the first-run hint is shown until a template or a class of
    /// the administrator's own exists.
    #[tokio::test]
    async fn empty_data_model_hint_ignores_built_in_rows() {
        let Some(db) = scratch::database("seed_empty_data_model_hint").await else { return };

        let fresh = data_model_report(&data_model_counts(&db.pool).await.unwrap());
        assert!(fresh.iter().any(|l| l.starts_with(HINT)), "{fresh:#?}");
        assert!(fresh.contains(&"  ci_classes: 2 (built-in: 2)".to_owned()), "{fresh:#?}");

        install_template(&db.pool, "it_infrastructure").await.unwrap();
        let seeded = data_model_report(&data_model_counts(&db.pool).await.unwrap());
        assert!(!seeded.iter().any(|l| l.starts_with(HINT)), "{seeded:#?}");

        db.drop().await;
    }

    #[tokio::test]
    async fn a_class_of_your_own_ends_the_empty_data_model_hint() {
        let Some(db) = scratch::database("seed_own_class_ends_hint").await else { return };
        sqlx::query(
            "WITH a AS (INSERT INTO areas (key, name) VALUES ('network', 'Network') RETURNING id)
             INSERT INTO ci_classes (key, name, area_id) SELECT 'load_balancer', 'Load Balancer', id FROM a",
        )
        .execute(&db.pool)
        .await
        .unwrap();
        let report = data_model_report(&data_model_counts(&db.pool).await.unwrap());
        assert!(!report.iter().any(|l| l.starts_with(HINT)), "{report:#?}");
        assert!(report.contains(&"  ci_classes: 3 (built-in: 2)".to_owned()), "{report:#?}");
        db.drop().await;
    }
}
