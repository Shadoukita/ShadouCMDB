//! The "IT infrastructure" starter template: the content `shadoucmdb seed`
//! loaded before SHAA-31. Hardware, servers, network devices, VMs,
//! applications, databases, services and locations, with their attributes,
//! relationship types and rules, plus statuses, environments and a sample
//! location tree.

use super::{AreaSpec, Class, Content, Location, RelationshipType, Rule, Status, Template, attr};

pub const TEMPLATE: Template = Template {
    key: "it_infrastructure",
    name: "IT infrastructure",
    description: "Servers, virtual machines, network devices, applications, databases, services and locations, \
                  with runs_on / depends_on / located_in / connected_to relationships, lifecycle statuses, \
                  environments and a sample location tree.",
    content,
};

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

const OS_FAMILIES: &[&str] = &["linux", "windows", "bsd", "unix", "other"];

fn classes() -> Vec<Class> {
    vec![
        Class {
            key: "hardware",
            name: "Hardware",
            parent: None,
            is_abstract: true,
            description: "Any physical device",
            color: Some("#6e7781"),
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
            color: Some("#0969da"),
            attributes: vec![
                attr("cpu_cores", "CPU cores", "integer").group("Compute").valid(r#"{"min":1}"#),
                attr("memory_gb", "Memory (GB)", "number").group("Compute").valid(r#"{"min":0}"#),
                attr("os_family", "OS family", "enum").values(OS_FAMILIES).group("Software"),
                attr("os_version", "OS version", "text").group("Software"),
                attr("management_ip", "Management IP (BMC)", "ip")
                    .group("Network")
                    .help("Out-of-band controller address (iLO, iDRAC, IPMI)"),
            ],
        },
        Class {
            key: "network_device",
            name: "Network device",
            parent: Some("hardware"),
            is_abstract: false,
            description: "Switch, router, firewall, load balancer or access point",
            color: Some("#1a7f37"),
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
            color: Some("#8250df"),
            attributes: vec![
                attr("vcpu", "vCPU", "integer").group("Compute").valid(r#"{"min":1}"#),
                attr("memory_gb", "Memory (GB)", "number").group("Compute").valid(r#"{"min":0}"#),
                attr("os_family", "OS family", "enum").values(OS_FAMILIES).group("Software"),
                attr("platform", "Platform", "enum")
                    .values(&["vmware", "hyper_v", "kvm", "aws", "azure", "gcp", "other"])
                    .group("Compute"),
                attr("instance_id", "Instance ID", "text").group("Compute").help("Hypervisor or cloud provider id"),
            ],
        },
        Class {
            key: "application",
            name: "Application",
            parent: None,
            is_abstract: false,
            description: "Deployed software application",
            color: Some("#bf3989"),
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
            color: Some("#9a6700"),
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
            color: Some("#cf222e"),
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
            color: Some("#57606a"),
            attributes: vec![
                attr("rack_units", "Rack units", "integer").valid(r#"{"min":1}"#),
                attr("power_kw", "Power budget (kW)", "number").valid(r#"{"min":0}"#),
            ],
        },
    ]
}

const RELATIONSHIP_TYPES: &[RelationshipType] = &[
    RelationshipType { key: "runs_on", name: "Runs on", forward: "runs on", reverse: "hosts", directional: true },
    RelationshipType {
        key: "depends_on",
        name: "Depends on",
        forward: "depends on",
        reverse: "is required by",
        directional: true,
    },
    RelationshipType {
        key: "located_in",
        name: "Located in",
        forward: "is located in",
        reverse: "contains",
        directional: true,
    },
    RelationshipType {
        key: "connected_to",
        name: "Connected to",
        forward: "is connected to",
        reverse: "is connected to",
        directional: false,
    },
];

/// Rules also match descendant classes.
const RELATIONSHIP_RULES: &[Rule] = &[
    Rule { kind: "runs_on", source: "application", target: "server" },
    Rule { kind: "runs_on", source: "application", target: "virtual_machine" },
    Rule { kind: "runs_on", source: "database", target: "server" },
    Rule { kind: "runs_on", source: "database", target: "virtual_machine" },
    Rule { kind: "runs_on", source: "virtual_machine", target: "server" },
    Rule { kind: "depends_on", source: "application", target: "database" },
    Rule { kind: "depends_on", source: "application", target: "application" },
    Rule { kind: "depends_on", source: "service", target: "application" },
    Rule { kind: "depends_on", source: "service", target: "service" },
    Rule { kind: "located_in", source: "hardware", target: "location" },
    Rule { kind: "located_in", source: "location", target: "location" },
    Rule { kind: "connected_to", source: "hardware", target: "hardware" },
];

fn content() -> Content {
    Content {
        area: AreaSpec {
            key: "infrastruktur",
            name: "Infrastruktur",
            description: "IT infrastructure: hardware, virtual machines, applications, databases and services",
        },
        statuses: STATUSES,
        environments: ENVIRONMENTS,
        locations: LOCATIONS,
        classes: classes(),
        relationship_types: RELATIONSHIP_TYPES,
        relationship_rules: RELATIONSHIP_RULES,
    }
}
