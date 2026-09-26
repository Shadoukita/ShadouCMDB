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
use crate::modules::templates;

/// Demo owners: kind, name, email, external ref
const OWNERS: &[(&str, &str, &str, &str)] = &[
    ("team", "Infrastructure", "infra@example.com", "seed:team:infrastructure"),
    ("team", "Platform Engineering", "platform@example.com", "seed:team:platform"),
    ("team", "Database Administration", "dba@example.com", "seed:team:dba"),
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

/// Core fields of a demo CI; `None` columns are left NULL.
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
}

enum Val<'a> {
    Text(&'a str),
    Number(&'a str),
    Bool(bool),
    Date(&'a str),
    Ip(&'a str),
    Cidr(&'a str),
    Ref(Uuid),
}

/// Small sample inventory (and the owners it uses) on top of the IT
/// infrastructure template; only loaded into a database with no CIs.
pub async fn seed_demo_data(pool: &PgPool) -> anyhow::Result<bool> {
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM configuration_items").fetch_one(pool).await?;
    if n > 0 {
        return Ok(false);
    }

    let mut tx = pool.begin().await?;
    for (kind, name, email, external_ref) in OWNERS {
        sqlx::query(
            "INSERT INTO owners (kind, name, email, external_ref) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
        )
        .bind(kind)
        .bind(name)
        .bind(email)
        .bind(external_ref)
        .execute(&mut *tx)
        .await?;
    }
    let cls = key_map(&mut tx, "ci_classes").await?;
    let st = key_map(&mut tx, "statuses").await?;
    let envs = key_map(&mut tx, "environments").await?;
    let locs = key_map(&mut tx, "locations").await?;
    let types = key_map(&mut tx, "relationship_types").await?;
    let owners: KeyMap =
        sqlx::query_as::<_, (String, Uuid)>("SELECT external_ref, id FROM owners WHERE external_ref IS NOT NULL")
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .collect();

    let mut insert = async |ci: DemoCi<'_>| -> anyhow::Result<Uuid> {
        let class_id = must(&cls, ci.class)?;
        let status_id = must(&st, ci.status.unwrap_or("in_service"))?;
        let environment_id = ci.environment.map(|k| must(&envs, k)).transpose()?;
        let owner_id = ci.owner.map(|k| must(&owners, k)).transpose()?;
        let location_id = ci.location.map(|k| must(&locs, k)).transpose()?;
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO configuration_items
               (class_id, name, status_id, environment_id, owner_id, location_id, hostname, ip_address, serial_number)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8::inet, $9) RETURNING id",
        )
        .bind(class_id)
        .bind(ci.name)
        .bind(status_id)
        .bind(environment_id)
        .bind(owner_id)
        .bind(location_id)
        .bind(ci.hostname)
        .bind(ci.ip_address)
        .bind(ci.serial_number)
        .fetch_one(&mut *tx)
        .await?;

        // Same shape the API writes: the inserted fields plus the id.
        let mut new_value = Map::new();
        new_value.insert("id".into(), json!(id));
        new_value.insert("classId".into(), json!(class_id));
        new_value.insert("name".into(), json!(ci.name));
        new_value.insert("statusId".into(), json!(status_id));
        for (k, v) in [
            ("environmentId", environment_id.map(|u| u.to_string())),
            ("ownerId", owner_id.map(|u| u.to_string())),
            ("locationId", location_id.map(|u| u.to_string())),
            ("hostname", ci.hostname.map(str::to_owned)),
            ("ipAddress", ci.ip_address.map(str::to_owned)),
            ("serialNumber", ci.serial_number.map(str::to_owned)),
        ] {
            if let Some(v) = v {
                new_value.insert(k.into(), json!(v));
            }
        }
        sqlx::query(
            "INSERT INTO audit_log (actor_type, actor_name, action, entity_type, entity_id, new_value)
             VALUES ('system', 'seed', 'create', 'configuration_items', $1, $2)",
        )
        .bind(id)
        .bind(Value::Object(new_value))
        .execute(&mut *tx)
        .await?;
        Ok(id)
    };

    let infra = Some("seed:team:infrastructure");
    let platform = Some("seed:team:platform");
    let prod = Some("production");
    let rack = insert(DemoCi {
        class: "location",
        name: "FRA1 Rack A01",
        location: Some("fra1_rack_a01"),
        owner: infra,
        ..Default::default()
    })
    .await?;
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
        location: Some("fra1"),
        ..Default::default()
    })
    .await?;
    let db1 = insert(DemoCi {
        class: "database",
        name: "crm-db",
        hostname: Some("crm-db-01.example.internal"),
        ip_address: Some("10.20.6.31"),
        environment: prod,
        owner: Some("seed:team:dba"),
        location: Some("fra1"),
        ..Default::default()
    })
    .await?;
    let app =
        insert(DemoCi { class: "application", name: "CRM", environment: prod, owner: platform, ..Default::default() })
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

    let defs: HashMap<(String, String), Uuid> = sqlx::query_as::<_, (String, String, Uuid)>(
        "SELECT k.key, d.key, d.id FROM ci_attribute_definitions d JOIN ci_classes k ON k.id = d.class_id",
    )
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(|(c, a, id)| ((c, a), id))
    .collect();
    let values = [
        (srv, "hardware", "manufacturer", Val::Text("HPE")),
        (srv, "hardware", "model", Val::Text("ProLiant DL380 Gen10")),
        (srv, "hardware", "warranty_end", Val::Date("2028-03-31")),
        (srv, "server", "cpu_cores", Val::Number("32")),
        (srv, "server", "memory_gb", Val::Number("512")),
        (srv, "server", "os_family", Val::Text("other")),
        (srv, "server", "management_ip", Val::Ip("10.10.100.11")),
        (sw, "hardware", "manufacturer", Val::Text("Cisco")),
        (sw, "network_device", "device_role", Val::Text("switch")),
        (sw, "network_device", "port_count", Val::Number("48")),
        (sw, "network_device", "management_subnet", Val::Cidr("10.10.0.0/24")),
        (vm, "virtual_machine", "vcpu", Val::Number("8")),
        (vm, "virtual_machine", "memory_gb", Val::Number("32")),
        (vm, "virtual_machine", "os_family", Val::Text("linux")),
        (vm, "virtual_machine", "platform", Val::Text("vmware")),
        (db1, "database", "engine", Val::Text("postgresql")),
        (db1, "database", "engine_version", Val::Text("17.2")),
        (db1, "database", "port", Val::Number("5432")),
        (db1, "database", "backup_enabled", Val::Bool(true)),
        (app, "application", "version", Val::Text("4.2.0")),
        (app, "application", "criticality", Val::Text("high")),
        (app, "application", "primary_database", Val::Ref(db1)),
        (svc, "service", "service_tier", Val::Text("tier_1")),
        (svc, "service", "sla_uptime_percent", Val::Number("99.9")),
    ];
    for (ci, class, key, val) in values {
        let attribute_id = *defs
            .get(&(class.to_owned(), key.to_owned()))
            .with_context(|| format!("seed: unknown attribute {class}.{key}"))?;
        let (mut text, mut num, mut boolean, mut date, mut ip, mut cidr, mut reference) =
            (None, None, None, None, None, None, None);
        match val {
            Val::Text(v) => text = Some(v),
            Val::Number(v) => num = Some(v),
            Val::Bool(v) => boolean = Some(v),
            Val::Date(v) => date = Some(v),
            Val::Ip(v) => ip = Some(v),
            Val::Cidr(v) => cidr = Some(v),
            Val::Ref(v) => reference = Some(v),
        }
        sqlx::query(
            "INSERT INTO ci_attribute_values
               (ci_id, attribute_id, value_text, value_number, value_boolean, value_date, value_ip, value_cidr, value_ref_ci_id)
             VALUES ($1, $2, $3, $4::numeric, $5, $6::date, $7::inet, $8::cidr, $9)",
        )
        .bind(ci)
        .bind(attribute_id)
        .bind(text)
        .bind(num)
        .bind(boolean)
        .bind(date)
        .bind(ip)
        .bind(cidr)
        .bind(reference)
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

pub async fn run(cfg: &DatabaseConfig, template_keys: &[String], demo: bool) -> anyhow::Result<()> {
    let pool = crate::db::connect(cfg).await?;
    let result = async {
        seed_system_rows(&pool).await?;
        let mut keys: Vec<String> = template_keys.to_vec();
        if demo && !keys.iter().any(|k| k == "it_infrastructure") {
            keys.push("it_infrastructure".into());
        }
        for key in &keys {
            let r = install_template(&pool, key).await?;
            let c = &r.created;
            println!(
                "Template {key}: created {} classes, {} attributes, {} relationship types, {} rules, {} statuses, {} environments, {} locations",
                c.classes,
                c.attribute_definitions,
                c.relationship_types,
                c.relationship_rules,
                c.statuses,
                c.environments,
                c.locations
            );
            for s in &r.skipped {
                println!("  skipped {s}");
            }
        }
        let counts: Vec<(String, i64)> = sqlx::query_as(
            "SELECT 'ci_classes', count(*) FROM ci_classes
             UNION ALL SELECT 'ci_attribute_definitions', count(*) FROM ci_attribute_definitions
             UNION ALL SELECT 'relationship_types', count(*) FROM relationship_types
             UNION ALL SELECT 'relationship_type_rules', count(*) FROM relationship_type_rules
             UNION ALL SELECT 'statuses', count(*) FROM statuses
             UNION ALL SELECT 'environments', count(*) FROM environments
             UNION ALL SELECT 'locations', count(*) FROM locations
             UNION ALL SELECT 'owners', count(*) FROM owners
             UNION ALL SELECT 'lookup_lists', count(*) FROM lookup_lists",
        )
        .fetch_all(&pool)
        .await?;
        println!("System rows: ok");
        println!("Data model:");
        for (table, n) in &counts {
            println!("  {table}: {n}");
        }
        if counts.first().is_some_and(|(_, n)| *n == 0) {
            println!(
                "The data model is empty. Build it under Administration, or install a starter template: \
                 shadoucmdb seed --template it_infrastructure"
            );
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
