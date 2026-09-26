//! Reference data every installation needs, plus an optional demo inventory.
//!
//! Idempotent: rows are matched by their stable `key` and never overwritten, so
//! an operator's renames survive a re-run. `--demo` additionally loads a small
//! sample inventory into a database that has no CIs yet.

use std::collections::HashMap;

use anyhow::{Context, anyhow};
use serde_json::{Map, Value, json};
use sqlx::postgres::{PgConnection, PgPool};
use uuid::Uuid;

use crate::config::DatabaseConfig;

struct Status {
    key: &'static str,
    name: &'static str,
    is_operational: bool,
    description: &'static str,
}

const STATUSES: &[Status] = &[
    Status { key: "planned", name: "Planned", is_operational: false, description: "Approved but not yet deployed" },
    Status {
        key: "in_service",
        name: "In service",
        is_operational: true,
        description: "Deployed and serving its purpose",
    },
    Status {
        key: "maintenance",
        name: "Maintenance",
        is_operational: true,
        description: "Temporarily degraded or under maintenance",
    },
    Status { key: "retired", name: "Retired", is_operational: false, description: "Decommissioned, kept for records" },
    Status {
        key: "disposed",
        name: "Disposed",
        is_operational: false,
        description: "Physically disposed or destroyed",
    },
];

const ENVIRONMENTS: &[(&str, &str)] = &[
    ("production", "Production"),
    ("staging", "Staging"),
    ("test", "Test"),
    ("development", "Development"),
    ("disaster_recovery", "Disaster recovery"),
];

struct Location {
    key: &'static str,
    name: &'static str,
    location_type: &'static str,
    parent: Option<&'static str>,
    address: Option<&'static str>,
}

const LOCATIONS: &[Location] = &[
    Location { key: "emea", name: "EMEA", location_type: "region", parent: None, address: None },
    Location {
        key: "fra1",
        name: "Frankfurt DC 1",
        location_type: "site",
        parent: Some("emea"),
        address: Some("Frankfurt am Main, DE"),
    },
    Location {
        key: "fra1_room_101",
        name: "FRA1 Room 101",
        location_type: "room",
        parent: Some("fra1"),
        address: None,
    },
    Location {
        key: "fra1_rack_a01",
        name: "FRA1 Rack A01",
        location_type: "rack",
        parent: Some("fra1_room_101"),
        address: None,
    },
    Location { key: "amer", name: "Americas", location_type: "region", parent: None, address: None },
    Location {
        key: "nyc1",
        name: "New York DC 1",
        location_type: "site",
        parent: Some("amer"),
        address: Some("New York, NY, US"),
    },
    Location {
        key: "aws_eu_central_1",
        name: "AWS eu-central-1",
        location_type: "cloud_region",
        parent: Some("emea"),
        address: None,
    },
];

#[derive(Default)]
struct Attr {
    key: &'static str,
    label: &'static str,
    data_type: &'static str,
    enum_values: Option<&'static [&'static str]>,
    reference_class: Option<&'static str>,
    is_required: bool,
    group_name: Option<&'static str>,
    /// JSON object, e.g. `{"min":1}`.
    validation: Option<&'static str>,
}

struct Class {
    key: &'static str,
    name: &'static str,
    parent: Option<&'static str>,
    is_abstract: bool,
    description: &'static str,
    attributes: Vec<Attr>,
}

const OS_FAMILIES: &[&str] = &["linux", "windows", "bsd", "unix", "other"];

fn attr(key: &'static str, label: &'static str, data_type: &'static str) -> Attr {
    Attr { key, label, data_type, ..Attr::default() }
}

impl Attr {
    fn group(mut self, g: &'static str) -> Self {
        self.group_name = Some(g);
        self
    }
    fn valid(mut self, v: &'static str) -> Self {
        self.validation = Some(v);
        self
    }
    fn values(mut self, v: &'static [&'static str]) -> Self {
        self.enum_values = Some(v);
        self
    }
    fn required(mut self) -> Self {
        self.is_required = true;
        self
    }
    fn refers(mut self, class: &'static str) -> Self {
        self.reference_class = Some(class);
        self
    }
}

fn classes() -> Vec<Class> {
    vec![
        Class {
            key: "hardware",
            name: "Hardware",
            parent: None,
            is_abstract: true,
            description: "Any physical device",
            attributes: vec![
                attr("manufacturer", "Manufacturer", "text").group("Hardware"),
                attr("model", "Model", "text").group("Hardware"),
                attr("asset_tag", "Asset tag", "text").group("Asset"),
                attr("purchase_date", "Purchase date", "date").group("Asset"),
                attr("warranty_end", "Warranty end", "date").group("Asset"),
            ],
        },
        Class {
            key: "server",
            name: "Server",
            parent: Some("hardware"),
            is_abstract: false,
            description: "Physical server",
            attributes: vec![
                attr("cpu_cores", "CPU cores", "integer").group("Compute").valid(r#"{"min":1}"#),
                attr("memory_gb", "Memory (GB)", "number").group("Compute").valid(r#"{"min":0}"#),
                attr("os_family", "OS family", "enum").values(OS_FAMILIES).group("Software"),
                attr("os_version", "OS version", "text").group("Software"),
                attr("management_ip", "Management IP (BMC)", "ip").group("Network"),
            ],
        },
        Class {
            key: "network_device",
            name: "Network device",
            parent: Some("hardware"),
            is_abstract: false,
            description: "Switch, router, firewall, load balancer or access point",
            attributes: vec![
                attr("device_role", "Role", "enum")
                    .values(&["switch", "router", "firewall", "load_balancer", "wireless_ap", "other"])
                    .required()
                    .group("Network"),
                attr("port_count", "Port count", "integer").group("Network").valid(r#"{"min":0}"#),
                attr("firmware_version", "Firmware version", "text").group("Software"),
                attr("management_subnet", "Management subnet", "cidr").group("Network"),
            ],
        },
        Class {
            key: "virtual_machine",
            name: "Virtual machine",
            parent: None,
            is_abstract: false,
            description: "Virtual machine or cloud instance",
            attributes: vec![
                attr("vcpu", "vCPU", "integer").group("Compute").valid(r#"{"min":1}"#),
                attr("memory_gb", "Memory (GB)", "number").group("Compute").valid(r#"{"min":0}"#),
                attr("os_family", "OS family", "enum").values(OS_FAMILIES).group("Software"),
                attr("platform", "Platform", "enum")
                    .values(&["vmware", "hyper_v", "kvm", "aws", "azure", "gcp", "other"])
                    .group("Compute"),
                attr("instance_id", "Instance ID", "text").group("Compute"),
            ],
        },
        Class {
            key: "application",
            name: "Application",
            parent: None,
            is_abstract: false,
            description: "Deployed software application",
            attributes: vec![
                attr("version", "Version", "text"),
                attr("vendor", "Vendor", "text"),
                attr("url", "URL", "text").valid(r#"{"pattern":"^https?://"}"#),
                attr("criticality", "Criticality", "enum").values(&["low", "medium", "high", "critical"]),
                attr("primary_database", "Primary database", "reference").refers("database"),
            ],
        },
        Class {
            key: "database",
            name: "Database",
            parent: None,
            is_abstract: false,
            description: "Database instance or schema",
            attributes: vec![
                attr("engine", "Engine", "enum")
                    .values(&["postgresql", "mysql", "mariadb", "sql_server", "oracle", "mongodb", "redis", "other"])
                    .required(),
                attr("engine_version", "Engine version", "text"),
                attr("port", "Port", "integer").valid(r#"{"min":1,"max":65535}"#),
                attr("size_gb", "Size (GB)", "number").valid(r#"{"min":0}"#),
                attr("backup_enabled", "Backups enabled", "boolean"),
            ],
        },
        Class {
            key: "service",
            name: "Service",
            parent: None,
            is_abstract: false,
            description: "Business or technical service offered to users",
            attributes: vec![
                attr("service_tier", "Service tier", "enum").values(&["tier_1", "tier_2", "tier_3"]),
                attr("sla_uptime_percent", "SLA uptime (%)", "number").valid(r#"{"min":0,"max":100}"#),
                attr("support_url", "Support URL", "text"),
                attr("go_live_at", "Go-live", "datetime"),
            ],
        },
        Class {
            key: "location",
            name: "Location",
            parent: None,
            is_abstract: false,
            description: "A place that participates in the relationship graph; set the core location field to the matching locations row",
            attributes: vec![
                attr("rack_units", "Rack units", "integer").valid(r#"{"min":1}"#),
                attr("power_kw", "Power budget (kW)", "number").valid(r#"{"min":0}"#),
            ],
        },
    ]
}

/// key, name, forward label, reverse label, directional
const RELATIONSHIP_TYPES: &[(&str, &str, &str, &str, bool)] = &[
    ("runs_on", "Runs on", "runs on", "hosts", true),
    ("depends_on", "Depends on", "depends on", "is required by", true),
    ("located_in", "Located in", "is located in", "contains", true),
    ("connected_to", "Connected to", "is connected to", "is connected to", false),
];

/// [type, source class, target class]; rules also match descendant classes.
const RELATIONSHIP_RULES: &[(&str, &str, &str)] = &[
    ("runs_on", "application", "server"),
    ("runs_on", "application", "virtual_machine"),
    ("runs_on", "database", "server"),
    ("runs_on", "database", "virtual_machine"),
    ("runs_on", "virtual_machine", "server"),
    ("depends_on", "application", "database"),
    ("depends_on", "application", "application"),
    ("depends_on", "service", "application"),
    ("depends_on", "service", "service"),
    ("located_in", "hardware", "location"),
    ("located_in", "location", "location"),
    ("connected_to", "hardware", "hardware"),
];

/// kind, name, email, external ref
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

pub async fn seed_reference_data(pool: &PgPool) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;

    for (i, s) in STATUSES.iter().enumerate() {
        sqlx::query(
            "INSERT INTO statuses (key, name, is_operational, description, sort_order)
             VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
        )
        .bind(s.key)
        .bind(s.name)
        .bind(s.is_operational)
        .bind(s.description)
        .bind(i as i32 * 10)
        .execute(&mut *tx)
        .await?;
    }
    for (i, (key, name)) in ENVIRONMENTS.iter().enumerate() {
        sqlx::query("INSERT INTO environments (key, name, sort_order) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING")
            .bind(key)
            .bind(name)
            .bind(i as i32 * 10)
            .execute(&mut *tx)
            .await?;
    }

    // Locations and classes reference their parent, so parents are inserted first.
    for loc in LOCATIONS {
        let parent_id = match loc.parent {
            Some(p) => Some(must(&key_map(&mut tx, "locations").await?, p)?),
            None => None,
        };
        sqlx::query(
            "INSERT INTO locations (key, name, location_type, address, parent_id)
             VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
        )
        .bind(loc.key)
        .bind(loc.name)
        .bind(loc.location_type)
        .bind(loc.address)
        .bind(parent_id)
        .execute(&mut *tx)
        .await?;
    }

    let classes = classes();
    for cls in &classes {
        let parent_id = match cls.parent {
            Some(p) => Some(must(&key_map(&mut tx, "ci_classes").await?, p)?),
            None => None,
        };
        sqlx::query(
            "INSERT INTO ci_classes (key, name, description, is_abstract, parent_id)
             VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
        )
        .bind(cls.key)
        .bind(cls.name)
        .bind(cls.description)
        .bind(cls.is_abstract)
        .bind(parent_id)
        .execute(&mut *tx)
        .await?;
    }
    let class_ids = key_map(&mut tx, "ci_classes").await?;
    for cls in &classes {
        let class_id = must(&class_ids, cls.key)?;
        for (i, a) in cls.attributes.iter().enumerate() {
            let validation: Option<Value> = a.validation.map(serde_json::from_str).transpose()?;
            let reference_class_id = a.reference_class.map(|k| must(&class_ids, k)).transpose()?;
            sqlx::query(
                "INSERT INTO ci_attribute_definitions
                   (class_id, key, label, data_type, is_required, enum_values, reference_class_id, validation, group_name, sort_order)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) ON CONFLICT DO NOTHING",
            )
            .bind(class_id)
            .bind(a.key)
            .bind(a.label)
            .bind(a.data_type)
            .bind(a.is_required)
            .bind(a.enum_values.map(|v| json!(v)))
            .bind(reference_class_id)
            .bind(validation)
            .bind(a.group_name)
            .bind(i as i32 * 10)
            .execute(&mut *tx)
            .await?;
        }
    }

    for (i, (key, name, fwd, rev, directional)) in RELATIONSHIP_TYPES.iter().enumerate() {
        sqlx::query(
            "INSERT INTO relationship_types (key, name, forward_label, reverse_label, is_directional, sort_order)
             VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING",
        )
        .bind(key)
        .bind(name)
        .bind(fwd)
        .bind(rev)
        .bind(directional)
        .bind(i as i32 * 10)
        .execute(&mut *tx)
        .await?;
    }
    let type_ids = key_map(&mut tx, "relationship_types").await?;
    for (ty, src, tgt) in RELATIONSHIP_RULES {
        sqlx::query(
            "INSERT INTO relationship_type_rules (relationship_type_id, source_class_id, target_class_id)
             VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        )
        .bind(must(&type_ids, ty)?)
        .bind(must(&class_ids, src)?)
        .bind(must(&class_ids, tgt)?)
        .execute(&mut *tx)
        .await?;
    }

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

    tx.commit().await?;
    Ok(())
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

/// Small sample inventory; only loaded into a database with no CIs.
pub async fn seed_demo_data(pool: &PgPool) -> anyhow::Result<bool> {
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM configuration_items").fetch_one(pool).await?;
    if n > 0 {
        return Ok(false);
    }

    let mut tx = pool.begin().await?;
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

pub async fn run(cfg: &DatabaseConfig, demo: bool) -> anyhow::Result<()> {
    let pool = crate::db::connect(cfg).await?;
    let result = async {
        seed_reference_data(&pool).await?;
        let counts: Vec<(String, i64)> = sqlx::query_as(
            "SELECT 'ci_classes', count(*) FROM ci_classes
             UNION ALL SELECT 'ci_attribute_definitions', count(*) FROM ci_attribute_definitions
             UNION ALL SELECT 'statuses', count(*) FROM statuses
             UNION ALL SELECT 'environments', count(*) FROM environments
             UNION ALL SELECT 'locations', count(*) FROM locations
             UNION ALL SELECT 'owners', count(*) FROM owners
             UNION ALL SELECT 'relationship_types', count(*) FROM relationship_types
             UNION ALL SELECT 'relationship_type_rules', count(*) FROM relationship_type_rules",
        )
        .fetch_all(&pool)
        .await?;
        println!("Reference data:");
        for (table, n) in counts {
            println!("  {table}: {n}");
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
