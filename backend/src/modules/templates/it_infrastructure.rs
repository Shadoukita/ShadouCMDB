//! The "IT infrastructure" starter template: the content `shadoucmdb seed`
//! loaded before SHAA-31. Hardware, servers, network devices, VMs,
//! applications, databases, services and locations, with their attributes,
//! relationship types and rules, plus lookup lists for lifecycle status,
//! environment and a sample set of locations. Every root class carries the
//! fields that were fixed CI columns before SHAA-267 (name, status, ...), so a
//! fresh install looks like an upgraded one.

use super::{AreaSpec, Attr, Class, Content, LookupList, RelationshipType, Rule, Template, attr};
use crate::modules::impact::ImpactDirection;

pub const TEMPLATE: Template = Template {
    key: "it_infrastructure",
    name: "IT infrastructure",
    description: "Servers, virtual machines, network devices, applications, databases, services and locations, \
                  with runs_on / depends_on / located_in / connected_to relationships and lookup lists for \
                  lifecycle status, environment and location.",
    content,
};

const LOOKUP_LISTS: &[LookupList] = &[
    LookupList {
        key: "status",
        name: "Status",
        description: "Lifecycle status of a configuration item",
        values: &[
            ("planned", "Planned", Some("Approved but not yet deployed")),
            ("in_service", "In service", Some("Deployed and serving its purpose")),
            ("maintenance", "Maintenance", Some("Temporarily degraded or under maintenance")),
            ("retired", "Retired", Some("Decommissioned, kept for records")),
            ("disposed", "Disposed", Some("Physically disposed or destroyed")),
        ],
    },
    LookupList {
        key: "environment",
        name: "Environment",
        description: "Deployment environment",
        values: &[
            ("production", "Production", None),
            ("staging", "Staging", None),
            ("test", "Test", None),
            ("development", "Development", None),
            ("disaster_recovery", "Disaster recovery", None),
        ],
    },
    LookupList {
        key: "owner",
        name: "Owner",
        description: "Teams and people responsible for configuration items",
        values: &[],
    },
    LookupList {
        key: "location",
        name: "Location",
        description: "Sites, rooms and racks",
        values: &[
            ("emea", "EMEA", Some("Region")),
            ("fra1", "Frankfurt DC 1", Some("Site, Frankfurt am Main, DE")),
            ("fra1_room_101", "FRA1 Room 101", Some("Room in Frankfurt DC 1")),
            ("fra1_rack_a01", "FRA1 Rack A01", Some("Rack in FRA1 Room 101")),
            ("amer", "Americas", Some("Region")),
            ("nyc1", "New York DC 1", Some("Site, New York, NY, US")),
            ("aws_eu_central_1", "AWS eu-central-1", Some("Cloud region")),
        ],
    },
];

fn name() -> Attr {
    attr("name", "Name", "text").required().valid(r#"{"maxLength":200,"pattern":"\\S"}"#)
}
fn status() -> Attr {
    attr("status", "Status", "lookup").lookup("status").required()
}
fn environment() -> Attr {
    attr("environment", "Environment", "lookup").lookup("environment")
}
fn owner() -> Attr {
    attr("owner", "Owner", "lookup").lookup("owner")
}
fn hostname() -> Attr {
    attr("hostname", "Hostname", "text").valid(r#"{"maxLength":253,"pattern":"^[A-Za-z0-9]([A-Za-z0-9._-]{0,252})$"}"#)
}
fn ip_address() -> Attr {
    attr("ip_address", "IP address", "ip")
}
fn notes() -> Attr {
    attr("notes", "Notes", "text").valid(r#"{"maxLength":4000,"multiline":true}"#)
}

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
                name(),
                status(),
                environment(),
                owner(),
                attr("location", "Location", "lookup").lookup("location"),
                hostname(),
                ip_address(),
                attr("serial_number", "Serial number", "text").valid(r#"{"maxLength":200}"#).identifying(),
                attr("manufacturer", "Manufacturer", "text").group("Hardware"),
                attr("model", "Model", "text").group("Hardware"),
                attr("asset_tag", "Asset tag", "text").group("Asset").identifying(),
                attr("purchase_date", "Purchase date", "date").group("Asset"),
                attr("warranty_end", "Warranty end", "date").group("Asset"),
                notes(),
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
                name(),
                status(),
                environment(),
                owner(),
                hostname(),
                ip_address(),
                attr("vcpu", "vCPU", "integer").group("Compute").valid(r#"{"min":1}"#),
                attr("memory_gb", "Memory (GB)", "number").group("Compute").valid(r#"{"min":0}"#),
                attr("os_family", "OS family", "enum").values(OS_FAMILIES).group("Software"),
                attr("platform", "Platform", "enum")
                    .values(&["vmware", "hyper_v", "kvm", "aws", "azure", "gcp", "other"])
                    .group("Compute"),
                attr("instance_id", "Instance ID", "text").group("Compute").help("Hypervisor or cloud provider id"),
                notes(),
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
                name(),
                status(),
                environment(),
                owner(),
                attr("version", "Version", "text"),
                attr("vendor", "Vendor", "text"),
                attr("url", "URL", "text").valid(r#"{"pattern":"^https?://"}"#),
                attr("primary_database", "Primary database", "reference").refers("database"),
                notes(),
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
                name(),
                status(),
                environment(),
                owner(),
                attr("engine", "Engine", "enum")
                    .values(&["postgresql", "mysql", "mariadb", "sql_server", "oracle", "mongodb", "redis", "other"])
                    .required(),
                attr("engine_version", "Engine version", "text"),
                attr("port", "Port", "integer").valid(r#"{"min":1,"max":65535}"#),
                attr("size_gb", "Size (GB)", "number").valid(r#"{"min":0}"#),
                attr("backup_enabled", "Backups enabled", "boolean"),
                notes(),
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
                name(),
                status(),
                environment(),
                owner(),
                attr("service_tier", "Service tier", "enum").values(&["tier_1", "tier_2", "tier_3"]),
                attr("sla_uptime_percent", "SLA uptime (%)", "number").valid(r#"{"min":0,"max":100}"#),
                attr("support_url", "Support URL", "text"),
                attr("go_live_at", "Go-live", "datetime"),
                notes(),
            ],
        },
        Class {
            key: "location",
            name: "Location",
            parent: None,
            is_abstract: false,
            description: "A place that participates in the relationship graph (located_in)",
            color: Some("#57606a"),
            attributes: vec![
                name(),
                status(),
                attr("rack_units", "Rack units", "integer").valid(r#"{"min":1}"#),
                attr("power_kw", "Power budget (kW)", "number").valid(r#"{"min":0}"#),
                notes(),
            ],
        },
    ]
}

const RELATIONSHIP_TYPES: &[RelationshipType] = &[
    RelationshipType {
        key: "runs_on",
        name: "Runs on",
        forward: "runs on",
        reverse: "hosts",
        directional: true,
        impact: ImpactDirection::TargetToSource,
    },
    RelationshipType {
        key: "depends_on",
        name: "Depends on",
        forward: "depends on",
        reverse: "is required by",
        directional: true,
        impact: ImpactDirection::TargetToSource,
    },
    RelationshipType {
        key: "located_in",
        name: "Located in",
        forward: "is located in",
        reverse: "contains",
        directional: true,
        impact: ImpactDirection::TargetToSource,
    },
    RelationshipType {
        key: "connected_to",
        name: "Connected to",
        forward: "is connected to",
        reverse: "is connected to",
        directional: false,
        impact: ImpactDirection::None,
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
        lookup_lists: LOOKUP_LISTS,
        classes: classes(),
        relationship_types: RELATIONSHIP_TYPES,
        relationship_rules: RELATIONSHIP_RULES,
    }
}
