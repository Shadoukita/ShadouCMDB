//! Configuration export and import (`config.export_import`).
//!
//! Export writes the data model, lookups, permission profiles (never users or
//! passwords) and UI settings into one JSON file (see [`format`]). Import
//! reads such a file and merges it into this install in one transaction:
//! rows are matched by key and created or updated; nothing is deleted, so CIs
//! that use a class or lookup missing from the file keep working. The UI
//! settings section is the exception: it replaces the settings document (as a
//! new version) and the logo and favicon.
//!
//! A dry run executes exactly the same writes, with every check the admin API
//! applies (constraints, triggers, `after_write` rules, permission coverage),
//! and then rolls back, so the diff it reports is what applying would do.
//! Every applied change is audited with the importing user as the actor.

pub mod format;

use std::collections::{HashMap, HashSet};

use axum::http::{HeaderValue, Method, header};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use self::format::*;
use super::areas::{Area, Areas, DEFAULT_AREA};
use super::classes::{
    AttributeDataType, AttributeDefinition, AttributeDefinitions, CiClass, CiClasses, RelationshipRule,
    RelationshipRules, RelationshipType, RelationshipTypes, ValidationRules,
};
use super::lookups::{
    Environment, Environments, Location, Locations, LookupList, LookupListValue, LookupListValues, LookupLists, Owner,
    Owners, Status, Statuses,
};
use super::profiles::{self, ClassPermission};
use super::simple_resource::{self as simple, Resource};
use super::ui_settings::assets::{AssetKind, ImageType};
use super::ui_settings::document::{self, Issue, UiSettingsDocument};
use super::ui_settings::{self as ui};
use crate::api::context::RequestContext;
use crate::api::route::{Body, In, Json, NoBody, NoPath, NoQuery, Query, Route, WithHeaders, route};
use crate::auth::permissions::{ClassRights, GlobalPermission};
use crate::data::crud::{self, ColumnSet};
use crate::data::ui_settings as ui_data;
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::schema::naming::{self, NameKind};
use crate::schema::{self as engine, SchemaChange};

const TAG: &str = "Configuration export/import";

/// Largest accepted import file (the images alone can be ~850 KiB base64).
const IMPORT_BODY_LIMIT: usize = 16 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Import result
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImportMode {
    /// Validate and compute the diff, then roll back
    DryRun,
    /// Validate and apply in one transaction
    Apply,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ImportQuery {
    /// dry_run reports what would change without changing anything; apply makes the changes
    #[param(inline)]
    mode: ImportMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeAction {
    Create,
    Update,
    /// Only the logo and favicon are ever deleted (when the file has none)
    Delete,
}

/// One changed field: API names, values as in the file format (keys, not ids)
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FieldChange {
    pub field: String,
    #[schema(value_type = serde_json::Value, required = true)]
    pub from: Value,
    #[schema(value_type = serde_json::Value, required = true)]
    pub to: Value,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportChange {
    /// e.g. classes, attributes, lookupListValues, permissionProfiles, uiSettings
    pub section: String,
    /// The row's key in the file (class.key for attributes, list.value for list values, kind:name for owners)
    pub key: String,
    #[schema(inline)]
    pub action: ChangeAction,
    /// Changed fields (updates only)
    pub fields: Vec<FieldChange>,
}

/// Counts for one section of the file
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SectionSummary {
    pub section: String,
    pub created: i64,
    pub updated: i64,
    pub deleted: i64,
    pub unchanged: i64,
    /// Rows of this kind that exist here but are not in the file (kept as they are)
    pub not_in_file: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportWarning {
    /// Path in the file
    pub path: String,
    pub message: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportResult {
    #[schema(inline)]
    pub mode: ImportMode,
    /// True only for mode=apply (a dry run never changes anything)
    pub applied: bool,
    /// The DDL the import runs (would run, for a dry run): new schemas, tables, columns and changed columns
    pub schema_changes: Vec<SchemaChange>,
    /// Per section, in import order
    pub summary: Vec<SectionSummary>,
    /// Every create, update and delete (unchanged rows are only counted)
    pub changes: Vec<ImportChange>,
    pub warnings: Vec<ImportWarning>,
    /// References in the imported UI settings that do not resolve after the import (see GET /api/v1/ui-settings)
    pub ui_settings_issues: Vec<Issue>,
}

// ---------------------------------------------------------------------------
// Snapshot: the current configuration in file terms, plus key -> id maps
// ---------------------------------------------------------------------------

/// Parents before children, otherwise in the given order. Items in a cycle go last.
fn parents_first<T>(items: Vec<T>, key: impl Fn(&T) -> &str, parent: impl Fn(&T) -> Option<&str>) -> (Vec<T>, Vec<T>) {
    let keys: HashSet<String> = items.iter().map(|i| key(i).to_owned()).collect();
    let mut done: HashSet<String> = HashSet::new();
    let mut pending = items;
    let mut out = Vec::with_capacity(pending.len());
    loop {
        let before = pending.len();
        let mut rest = Vec::new();
        for item in pending {
            let ready = parent(&item).is_none_or(|p| !keys.contains(p) || done.contains(p));
            if ready {
                done.insert(key(&item).to_owned());
                out.push(item);
            } else {
                rest.push(item);
            }
        }
        pending = rest;
        if pending.is_empty() || pending.len() == before {
            return (out, pending);
        }
    }
}

fn owner_key(kind: crate::api::schemas::OwnerKind, name: &str) -> String {
    let kind = serde_json::to_value(kind).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default();
    format!("{kind}:{}", name.to_lowercase())
}

fn normalise_profile(p: &mut ProfileSpec) {
    p.global_permissions.sort();
    p.global_permissions.dedup();
    for g in &mut p.class_permissions {
        g.view = g.view || g.create || g.edit || g.delete;
    }
    p.class_permissions.retain(|g| g.view);
    p.class_permissions.sort_by(|a, b| a.class.cmp(&b.class));
}

#[derive(Default)]
struct Ids {
    areas: HashMap<String, Uuid>,
    classes: HashMap<String, Uuid>,
    attributes: HashMap<(String, String), Uuid>,
    types: HashMap<String, Uuid>,
    rules: HashSet<(String, String, String)>,
    statuses: HashMap<String, Uuid>,
    environments: HashMap<String, Uuid>,
    locations: HashMap<String, Uuid>,
    owners: HashMap<String, Uuid>,
    lists: HashMap<String, Uuid>,
    values: HashMap<(String, String), Uuid>,
    /// lower(name) -> id; the built-in profile included
    profiles: HashMap<String, Uuid>,
}

struct Snapshot {
    file: ConfigFile,
    ids: Ids,
    builtin_profile: String,
}

async fn snapshot(conn: &mut PgConnection) -> Result<Snapshot, AppError> {
    let mut ids = Ids::default();

    let areas: Vec<Area> = crud::select_all(conn, Areas::TABLE, Areas::COLUMNS, "sort_order, key").await?;
    let area_key: HashMap<Uuid, String> = areas.iter().map(|a| (a.id, a.key.clone())).collect();
    ids.areas = areas.iter().map(|a| (a.key.clone(), a.id)).collect();
    let area_specs: Vec<AreaSpec> = areas
        .iter()
        .map(|a| AreaSpec {
            key: a.key.clone(),
            name: a.name.clone(),
            description: a.description.clone(),
            icon: a.icon.clone(),
            color: a.color.clone(),
            sort_order: a.sort_order,
            is_active: a.is_active,
        })
        .collect();

    let classes: Vec<CiClass> = crud::select_all(conn, CiClasses::TABLE, CiClasses::COLUMNS, "sort_order, key").await?;
    let class_key: HashMap<Uuid, String> = classes.iter().map(|c| (c.id, c.key.clone())).collect();
    let class_specs: Vec<ClassSpec> = classes
        .iter()
        .map(|c| ClassSpec {
            key: c.key.clone(),
            area: area_key.get(&c.area_id).cloned(),
            name: c.name.clone(),
            description: c.description.clone(),
            parent: c.parent_id.and_then(|p| class_key.get(&p).cloned()),
            is_abstract: c.is_abstract,
            icon: c.icon.clone(),
            color: c.color.clone(),
            sort_order: c.sort_order,
            is_active: c.is_active,
        })
        .collect();
    let (mut class_specs, cyclic) = parents_first(class_specs, |c| &c.key, |c| c.parent.as_deref());
    class_specs.extend(cyclic);
    ids.classes = classes.iter().map(|c| (c.key.clone(), c.id)).collect();
    let class_rank: HashMap<&str, usize> = class_specs.iter().enumerate().map(|(i, c)| (c.key.as_str(), i)).collect();

    let lists: Vec<LookupList> =
        crud::select_all(conn, LookupLists::TABLE, LookupLists::COLUMNS, "sort_order, key").await?;
    let values: Vec<LookupListValue> =
        crud::select_all(conn, LookupListValues::TABLE, LookupListValues::COLUMNS, "sort_order, key").await?;
    let list_key: HashMap<Uuid, String> = lists.iter().map(|l| (l.id, l.key.clone())).collect();
    let value_key: HashMap<Uuid, String> = values.iter().map(|v| (v.id, v.key.clone())).collect();
    ids.lists = lists.iter().map(|l| (l.key.clone(), l.id)).collect();
    for v in &values {
        if let Some(l) = list_key.get(&v.list_id) {
            ids.values.insert((l.clone(), v.key.clone()), v.id);
        }
    }
    let list_specs: Vec<LookupListSpec> = lists
        .iter()
        .map(|l| LookupListSpec {
            key: l.key.clone(),
            name: l.name.clone(),
            description: l.description.clone(),
            sort_order: l.sort_order,
            is_active: l.is_active,
            values: values
                .iter()
                .filter(|v| v.list_id == l.id)
                .map(|v| LookupValueSpec {
                    key: v.key.clone(),
                    name: v.name.clone(),
                    description: v.description.clone(),
                    color: v.color.clone(),
                    sort_order: v.sort_order,
                    is_active: v.is_active,
                })
                .collect(),
        })
        .collect();

    let attrs: Vec<AttributeDefinition> =
        crud::select_all(conn, AttributeDefinitions::TABLE, AttributeDefinitions::COLUMNS, "sort_order, key").await?;
    let mut attr_specs: Vec<AttributeSpec> = attrs
        .iter()
        .filter_map(|a| {
            let class = class_key.get(&a.class_id)?.clone();
            ids.attributes.insert((class.clone(), a.key.clone()), a.id);
            let default_value = a.default_value.as_ref().map(|d| d.0.clone()).map(|d| {
                if a.data_type == AttributeDataType::Lookup {
                    d.as_str()
                        .and_then(|s| Uuid::parse_str(s).ok())
                        .and_then(|id| value_key.get(&id))
                        .map(|k| Value::String(k.clone()))
                        .unwrap_or(d)
                } else {
                    d
                }
            });
            Some(AttributeSpec {
                class,
                key: a.key.clone(),
                label: a.label.clone(),
                description: a.description.clone(),
                data_type: a.data_type,
                is_required: a.is_required,
                enum_values: a.enum_values.as_ref().map(|v| v.0.clone()),
                reference_class: a.reference_class_id.and_then(|id| class_key.get(&id).cloned()),
                lookup_list: a.lookup_list_id.and_then(|id| list_key.get(&id).cloned()),
                validation: a
                    .validation
                    .as_ref()
                    .and_then(|v| serde_json::from_value::<ValidationRules>(Value::Object(v.0.clone())).ok()),
                group_name: a.group_name.clone(),
                help_text: a.help_text.clone(),
                default_value,
                sort_order: a.sort_order,
                is_active: a.is_active,
            })
        })
        .collect();
    attr_specs.sort_by_key(|a| class_rank.get(a.class.as_str()).copied().unwrap_or(usize::MAX));

    let types: Vec<RelationshipType> =
        crud::select_all(conn, RelationshipTypes::TABLE, RelationshipTypes::COLUMNS, "sort_order, key").await?;
    let type_key: HashMap<Uuid, String> = types.iter().map(|t| (t.id, t.key.clone())).collect();
    ids.types = types.iter().map(|t| (t.key.clone(), t.id)).collect();
    let type_specs: Vec<RelationshipTypeSpec> = types
        .iter()
        .map(|t| RelationshipTypeSpec {
            key: t.key.clone(),
            name: t.name.clone(),
            description: t.description.clone(),
            forward_label: t.forward_label.clone(),
            reverse_label: t.reverse_label.clone(),
            is_directional: t.is_directional,
            sort_order: t.sort_order,
            is_active: t.is_active,
        })
        .collect();
    let rules: Vec<RelationshipRule> =
        crud::select_all(conn, RelationshipRules::TABLE, RelationshipRules::COLUMNS, "created_at, id").await?;
    let rule_specs: Vec<RelationshipRuleSpec> = rules
        .iter()
        .filter_map(|r| {
            Some(RelationshipRuleSpec {
                relationship_type: type_key.get(&r.relationship_type_id)?.clone(),
                source_class: class_key.get(&r.source_class_id)?.clone(),
                target_class: class_key.get(&r.target_class_id)?.clone(),
            })
        })
        .collect();
    ids.rules = rule_specs
        .iter()
        .map(|r| (r.relationship_type.clone(), r.source_class.clone(), r.target_class.clone()))
        .collect();

    let statuses: Vec<Status> = crud::select_all(conn, Statuses::TABLE, Statuses::COLUMNS, "sort_order, key").await?;
    ids.statuses = statuses.iter().map(|s| (s.key.clone(), s.id)).collect();
    let environments: Vec<Environment> =
        crud::select_all(conn, Environments::TABLE, Environments::COLUMNS, "sort_order, key").await?;
    ids.environments = environments.iter().map(|s| (s.key.clone(), s.id)).collect();
    let locations: Vec<Location> =
        crud::select_all(conn, Locations::TABLE, Locations::COLUMNS, "sort_order, key").await?;
    let location_key: HashMap<Uuid, String> = locations.iter().map(|l| (l.id, l.key.clone())).collect();
    ids.locations = locations.iter().map(|l| (l.key.clone(), l.id)).collect();
    let location_specs: Vec<LocationSpec> = locations
        .iter()
        .map(|l| LocationSpec {
            key: l.key.clone(),
            name: l.name.clone(),
            description: l.description.clone(),
            parent: l.parent_id.and_then(|p| location_key.get(&p).cloned()),
            location_type: l.location_type,
            address: l.address.clone(),
            sort_order: l.sort_order,
            is_active: l.is_active,
        })
        .collect();
    let (mut location_specs, cyclic) = parents_first(location_specs, |l| &l.key, |l| l.parent.as_deref());
    location_specs.extend(cyclic);
    let owners: Vec<Owner> = crud::select_all(conn, Owners::TABLE, Owners::COLUMNS, "kind, lower(name), id").await?;
    for o in &owners {
        ids.owners.entry(owner_key(o.kind, &o.name)).or_insert(o.id);
    }

    let builtin_id = crate::data::auth::builtin_profile_id(conn).await?;
    let builtin_profile: String = sqlx::query_scalar("SELECT name FROM permission_profiles WHERE id = $1")
        .bind(builtin_id)
        .fetch_one(&mut *conn)
        .await?;
    ids.profiles.insert(builtin_profile.to_lowercase(), builtin_id);
    let profile_specs: Vec<ProfileSpec> = profiles::all_editable(conn)
        .await?
        .into_iter()
        .map(|p| {
            ids.profiles.insert(p.name.to_lowercase(), p.id);
            let mut spec = ProfileSpec {
                name: p.name,
                description: p.description,
                global_permissions: p.global_permissions,
                class_permissions: p
                    .class_permissions
                    .iter()
                    .filter_map(|g| {
                        let class = match g.class_id {
                            None => None,
                            Some(id) => Some(class_key.get(&id)?.clone()),
                        };
                        Some(ClassGrantSpec { class, view: g.view, create: g.create, edit: g.edit, delete: g.delete })
                    })
                    .collect(),
            };
            normalise_profile(&mut spec);
            spec
        })
        .collect();

    let current = ui_data::current(conn, false).await?;
    let mut ui_section = UiSettingsSection { settings: ui::parse_stored(&current.settings), logo: None, favicon: None };
    for (kind, content_type, data) in ui_data::all_asset_data(conn).await? {
        let Some(content_type) = ImageType::parse(&content_type) else { continue };
        let asset = AssetData { content_type, data: base64::engine::general_purpose::STANDARD.encode(&data) };
        match AssetKind::parse(&kind) {
            Some(AssetKind::Logo) => ui_section.logo = Some(asset),
            Some(AssetKind::Favicon) => ui_section.favicon = Some(asset),
            None => {}
        }
    }

    let file = ConfigFile {
        format: FORMAT.into(),
        format_version: FORMAT_VERSION,
        exported_at: Some(crate::api::schemas::iso(&chrono::Utc::now())),
        app_version: Some(env!("CARGO_PKG_VERSION").into()),
        data_model: Some(DataModelSection {
            areas: area_specs,
            classes: class_specs,
            attributes: attr_specs,
            relationship_types: type_specs,
            relationship_rules: rule_specs,
        }),
        lookups: Some(LookupSection {
            statuses: statuses
                .iter()
                .map(|s| StatusSpec {
                    key: s.key.clone(),
                    name: s.name.clone(),
                    description: s.description.clone(),
                    is_operational: s.is_operational,
                    sort_order: s.sort_order,
                    is_active: s.is_active,
                })
                .collect(),
            environments: environments
                .iter()
                .map(|e| EnvironmentSpec {
                    key: e.key.clone(),
                    name: e.name.clone(),
                    description: e.description.clone(),
                    sort_order: e.sort_order,
                    is_active: e.is_active,
                })
                .collect(),
            locations: location_specs,
            owners: owners
                .iter()
                .map(|o| OwnerSpec {
                    kind: o.kind,
                    name: o.name.clone(),
                    email: o.email.clone(),
                    external_ref: o.external_ref.clone(),
                    is_active: o.is_active,
                })
                .collect(),
            lists: list_specs,
        }),
        permission_profiles: Some(profile_specs),
        ui_settings: Some(ui_section),
    };
    Ok(Snapshot { file, ids, builtin_profile })
}

pub async fn export(pool: &PgPool) -> Result<ConfigFile, AppError> {
    // One snapshot: REPEATABLE READ so every section comes from the same moment.
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut *tx).await?;
    let file = snapshot(&mut tx).await?.file;
    tx.commit().await?;
    Ok(file)
}

// ---------------------------------------------------------------------------
// Validation before any write
// ---------------------------------------------------------------------------

fn problem(errors: &mut Vec<FieldError>, path: String, code: &str, message: impl Into<String>) {
    errors.push(FieldError { location: FieldLocation::Body, field: path, message: message.into(), code: code.into() });
}

/// Keys found more than once: (path of the repeat, key).
fn duplicates<'a, T>(items: &'a [T], path: &str, key: impl Fn(&'a T) -> String, errors: &mut Vec<FieldError>) {
    let mut seen = HashSet::new();
    for (i, item) in items.iter().enumerate() {
        let k = key(item);
        if !seen.insert(k.clone()) {
            problem(errors, format!("{path}.{i}"), "duplicate", format!("\"{k}\" appears more than once"));
        }
    }
}

/// Keys known after the import: in the file or already here.
fn known<'a>(file: impl Iterator<Item = &'a str>, here: impl Iterator<Item = &'a String>) -> HashSet<String> {
    file.map(str::to_owned).chain(here.cloned()).collect()
}

struct Decoded {
    logo: Option<(ImageType, Vec<u8>)>,
    favicon: Option<(ImageType, Vec<u8>)>,
}

fn validate(file: &ConfigFile, snap: &Snapshot, warnings: &mut Vec<ImportWarning>) -> Result<Decoded, AppError> {
    let mut e = Vec::new();
    let empty_dm = DataModelSection::default();
    let empty_lk = LookupSection::default();
    let dm = file.data_model.as_ref().unwrap_or(&empty_dm);
    let lk = file.lookups.as_ref().unwrap_or(&empty_lk);
    let ids = &snap.ids;

    let classes = known(dm.classes.iter().map(|c| c.key.as_str()), ids.classes.keys());
    let lists = known(lk.lists.iter().map(|l| l.key.as_str()), ids.lists.keys());
    let types = known(dm.relationship_types.iter().map(|t| t.key.as_str()), ids.types.keys());
    let locations = known(lk.locations.iter().map(|l| l.key.as_str()), ids.locations.keys());
    let mut values: HashSet<(String, String)> = ids.values.keys().cloned().collect();
    for l in &lk.lists {
        values.extend(l.values.iter().map(|v| (l.key.clone(), v.key.clone())));
    }

    // Duplicates
    duplicates(&dm.areas, "dataModel.areas", |a| a.key.clone(), &mut e);
    duplicates(&dm.classes, "dataModel.classes", |c| c.key.clone(), &mut e);
    duplicates(&dm.attributes, "dataModel.attributes", |a| format!("{}.{}", a.class, a.key), &mut e);
    duplicates(&dm.relationship_types, "dataModel.relationshipTypes", |t| t.key.clone(), &mut e);
    duplicates(
        &dm.relationship_rules,
        "dataModel.relationshipRules",
        |r| format!("{} {} -> {}", r.relationship_type, r.source_class, r.target_class),
        &mut e,
    );
    duplicates(&lk.statuses, "lookups.statuses", |s| s.key.clone(), &mut e);
    duplicates(&lk.environments, "lookups.environments", |s| s.key.clone(), &mut e);
    duplicates(&lk.locations, "lookups.locations", |s| s.key.clone(), &mut e);
    duplicates(&lk.owners, "lookups.owners", |o| owner_key(o.kind, &o.name), &mut e);
    duplicates(&lk.lists, "lookups.lists", |l| l.key.clone(), &mut e);
    for (i, l) in lk.lists.iter().enumerate() {
        duplicates(&l.values, &format!("lookups.lists.{i}.values"), |v| v.key.clone(), &mut e);
    }
    if let Some(profiles) = &file.permission_profiles {
        duplicates(profiles, "permissionProfiles", |p| p.name.to_lowercase(), &mut e);
    }

    // Areas: usable technical names; classes stay in their area
    let areas = known(dm.areas.iter().map(|a| a.key.as_str()), ids.areas.keys());
    for (i, a) in dm.areas.iter().enumerate() {
        if !ids.areas.contains_key(&a.key)
            && let Err(p) = naming::validate(&a.key, NameKind::Area)
        {
            problem(&mut e, format!("dataModel.areas.{i}.key"), p.code, p.message);
        }
    }
    let current_area: HashMap<&str, Option<&str>> = snap
        .file
        .data_model
        .iter()
        .flat_map(|d| d.classes.iter())
        .map(|c| (c.key.as_str(), c.area.as_deref()))
        .collect();
    for (i, c) in dm.classes.iter().enumerate() {
        match (&c.area, current_area.get(c.key.as_str())) {
            (Some(a), Some(Some(now))) if a != now => problem(
                &mut e,
                format!("dataModel.classes.{i}.area"),
                "immutable",
                format!("Class \"{}\" is in area \"{now}\" here and cannot move to another area", c.key),
            ),
            (Some(a), _) if !areas.contains(a) => problem(
                &mut e,
                format!("dataModel.classes.{i}.area"),
                "not_found",
                format!("Area \"{a}\" does not exist"),
            ),
            _ => {}
        }
        if !ids.classes.contains_key(&c.key)
            && let Err(p) = naming::validate(&c.key, NameKind::Type)
        {
            problem(&mut e, format!("dataModel.classes.{i}.key"), p.code, p.message);
        }
    }

    // Classes: parents exist, no cycles in the file
    for (i, c) in dm.classes.iter().enumerate() {
        if let Some(p) = &c.parent
            && !classes.contains(p)
        {
            problem(
                &mut e,
                format!("dataModel.classes.{i}.parent"),
                "not_found",
                format!("Class \"{p}\" does not exist"),
            );
        }
        if c.parent.as_deref() == Some(c.key.as_str()) {
            problem(&mut e, format!("dataModel.classes.{i}.parent"), "cycle", "A class cannot be its own parent");
        }
    }
    let (_, cyclic) = parents_first(dm.classes.iter().collect(), |c| &c.key, |c| c.parent.as_deref());
    for c in cyclic {
        let i = dm.classes.iter().position(|x| x.key == c.key).unwrap_or_default();
        problem(
            &mut e,
            format!("dataModel.classes.{i}.parent"),
            "cycle",
            "The class hierarchy in the file has a cycle",
        );
    }
    let (_, cyclic) = parents_first(lk.locations.iter().collect(), |l| &l.key, |l| l.parent.as_deref());
    for l in cyclic {
        let i = lk.locations.iter().position(|x| x.key == l.key).unwrap_or_default();
        problem(&mut e, format!("lookups.locations.{i}.parent"), "cycle", "The location tree in the file has a cycle");
    }
    for (i, l) in lk.locations.iter().enumerate() {
        if let Some(p) = &l.parent
            && !locations.contains(p)
        {
            problem(
                &mut e,
                format!("lookups.locations.{i}.parent"),
                "not_found",
                format!("Location \"{p}\" does not exist"),
            );
        }
    }

    // Attributes: the rules of the attribute API, references, immutable fields
    let current_attrs: HashMap<(String, String), &AttributeSpec> = snap
        .file
        .data_model
        .iter()
        .flat_map(|d| d.attributes.iter())
        .map(|a| ((a.class.clone(), a.key.clone()), a))
        .collect();
    for (i, a) in dm.attributes.iter().enumerate() {
        let p = format!("dataModel.attributes.{i}");
        if !classes.contains(&a.class) {
            problem(&mut e, format!("{p}.class"), "not_found", format!("Class \"{}\" does not exist", a.class));
        }
        let is = |t: AttributeDataType| a.data_type == t;
        match (is(AttributeDataType::Enum), a.enum_values.is_some()) {
            (true, false) => problem(&mut e, format!("{p}.enumValues"), "custom", "Required for enum attributes"),
            (false, true) => problem(&mut e, format!("{p}.enumValues"), "custom", "Only allowed for enum attributes"),
            _ => {}
        }
        match (is(AttributeDataType::Reference), &a.reference_class) {
            (true, None) => {
                problem(&mut e, format!("{p}.referenceClass"), "custom", "Required for reference attributes")
            }
            (false, Some(_)) => {
                problem(&mut e, format!("{p}.referenceClass"), "custom", "Only allowed for reference attributes")
            }
            (true, Some(k)) if !classes.contains(k) => {
                problem(&mut e, format!("{p}.referenceClass"), "not_found", format!("Class \"{k}\" does not exist"))
            }
            _ => {}
        }
        match (is(AttributeDataType::Lookup), &a.lookup_list) {
            (true, None) => problem(&mut e, format!("{p}.lookupList"), "custom", "Required for lookup attributes"),
            (false, Some(_)) => {
                problem(&mut e, format!("{p}.lookupList"), "custom", "Only allowed for lookup attributes")
            }
            (true, Some(k)) if !lists.contains(k) => {
                problem(&mut e, format!("{p}.lookupList"), "not_found", format!("Lookup list \"{k}\" does not exist"))
            }
            _ => {}
        }
        if is(AttributeDataType::Reference) && a.default_value.as_ref().is_some_and(|v| !v.is_null()) {
            problem(&mut e, format!("{p}.defaultValue"), "custom", "Reference attributes cannot have a default");
        }
        if let (Some(list), Some(d)) = (&a.lookup_list, &a.default_value)
            && is(AttributeDataType::Lookup)
        {
            match d.as_str() {
                Some(k) if values.contains(&(list.clone(), k.to_owned())) => {}
                _ => problem(
                    &mut e,
                    format!("{p}.defaultValue"),
                    "not_found",
                    format!("A lookup default is the key of a value in list \"{list}\""),
                ),
            }
        }
        if let Some(v) = &a.validation {
            let mut errs = Vec::new();
            v.check(a.data_type, &mut errs);
            for mut x in errs {
                x.field = format!("{p}.{}", x.field);
                e.push(x);
            }
        }
        if let Some(old) = current_attrs.get(&(a.class.clone(), a.key.clone())) {
            for (field, changed) in [
                ("dataType", old.data_type != a.data_type),
                ("referenceClass", old.reference_class != a.reference_class),
                ("lookupList", old.lookup_list != a.lookup_list),
            ] {
                if changed {
                    problem(
                        &mut e,
                        format!("{p}.{field}"),
                        "immutable",
                        format!("Cannot change {field} of an existing attribute (stored values depend on it)"),
                    );
                }
            }
        }
    }

    // Relationship types and rules
    let current_types: HashMap<&str, &RelationshipTypeSpec> =
        snap.file.data_model.iter().flat_map(|d| d.relationship_types.iter()).map(|t| (t.key.as_str(), t)).collect();
    for (i, t) in dm.relationship_types.iter().enumerate() {
        if current_types.get(t.key.as_str()).is_some_and(|old| old.is_directional != t.is_directional) {
            problem(
                &mut e,
                format!("dataModel.relationshipTypes.{i}.isDirectional"),
                "immutable",
                "Cannot change isDirectional of an existing relationship type",
            );
        }
    }
    for (i, r) in dm.relationship_rules.iter().enumerate() {
        let p = format!("dataModel.relationshipRules.{i}");
        if !types.contains(&r.relationship_type) {
            problem(
                &mut e,
                format!("{p}.relationshipType"),
                "not_found",
                format!("Relationship type \"{}\" does not exist", r.relationship_type),
            );
        }
        for (field, k) in [("sourceClass", &r.source_class), ("targetClass", &r.target_class)] {
            if !classes.contains(k) {
                problem(&mut e, format!("{p}.{field}"), "not_found", format!("Class \"{k}\" does not exist"));
            }
        }
    }

    // Profiles
    for (i, prof) in file.permission_profiles.iter().flatten().enumerate() {
        let p = format!("permissionProfiles.{i}");
        if prof.name.to_lowercase() == snap.builtin_profile.to_lowercase() {
            warnings.push(ImportWarning {
                path: p.clone(),
                message: format!("\"{}\" is the built-in profile; it cannot be imported and was skipped", prof.name),
            });
            continue;
        }
        let mut seen = HashSet::new();
        for (j, g) in prof.class_permissions.iter().enumerate() {
            if let Some(k) = &g.class
                && !classes.contains(k)
            {
                problem(
                    &mut e,
                    format!("{p}.classPermissions.{j}.class"),
                    "not_found",
                    format!("Class \"{k}\" does not exist"),
                );
            }
            if !seen.insert(g.class.clone()) {
                problem(&mut e, format!("{p}.classPermissions.{j}.class"), "duplicate", "One entry per class");
            }
        }
    }

    // Images
    let mut decoded = Decoded { logo: None, favicon: None };
    if let Some(ui_section) = &file.ui_settings {
        for (kind, asset) in [(AssetKind::Logo, &ui_section.logo), (AssetKind::Favicon, &ui_section.favicon)] {
            let Some(asset) = asset else { continue };
            match ui::decode_upload(kind, asset.content_type, &asset.data) {
                Ok(bytes) => match kind {
                    AssetKind::Logo => decoded.logo = Some((asset.content_type, bytes)),
                    AssetKind::Favicon => decoded.favicon = Some((asset.content_type, bytes)),
                },
                Err(errs) => {
                    for mut x in errs {
                        x.field = format!("uiSettings.{}.{}", kind.as_str(), x.field);
                        e.push(x);
                    }
                }
            }
        }
    }

    if e.is_empty() {
        Ok(decoded)
    } else {
        let mut err = AppError::validation(e);
        err.message =
            format!("The file has {} problem(s); nothing was imported", err.details.as_ref().map_or(0, Vec::len));
        Err(err)
    }
}

// ---------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------

/// Attaches a file path to an error raised while writing one row.
fn at(path: &str, mut e: AppError) -> AppError {
    if matches!(e.code, ErrorCode::InternalError | ErrorCode::DatabaseUnavailable) {
        return e;
    }
    let code = serde_json::to_value(e.code).ok().and_then(|v| v.as_str().map(str::to_lowercase)).unwrap_or_default();
    let details = match e.details.take() {
        Some(d) if !d.is_empty() => d
            .into_iter()
            .map(|mut d| {
                d.location = FieldLocation::Body;
                d.field = if d.field == "(root)" { path.to_owned() } else { format!("{path}.{}", d.field) };
                d
            })
            .collect(),
        _ => {
            vec![FieldError { location: FieldLocation::Body, field: path.to_owned(), message: e.message.clone(), code }]
        }
    };
    e.message = format!("{path}: {}", e.message);
    e.details = Some(details);
    e
}

fn diff<T: Serialize>(old: &T, new: &T) -> Vec<FieldChange> {
    let (Ok(Value::Object(o)), Ok(Value::Object(n))) = (serde_json::to_value(old), serde_json::to_value(new)) else {
        return Vec::new();
    };
    n.into_iter()
        .filter_map(|(field, to)| {
            let from = o.get(&field).cloned().unwrap_or(Value::Null);
            (from != to).then_some(FieldChange { field, from, to })
        })
        .collect()
}

fn text(s: &str) -> String {
    s.to_owned()
}

struct Importer<'c> {
    conn: &'c mut PgConnection,
    ctx: &'c RequestContext,
    ids: Ids,
    summary: Vec<SectionSummary>,
    changes: Vec<ImportChange>,
}

impl Importer<'_> {
    fn section(&mut self, name: &str, not_in_file: i64) {
        self.summary.push(SectionSummary { section: name.into(), not_in_file, ..Default::default() });
    }

    fn record(&mut self, section: &str, key: String, action: Option<ChangeAction>, fields: Vec<FieldChange>) {
        let s = match self.summary.iter_mut().find(|s| s.section == section) {
            Some(s) => s,
            None => {
                self.section(section, 0);
                self.summary.last_mut().expect("just pushed")
            }
        };
        match action {
            None => s.unchanged += 1,
            Some(ChangeAction::Create) => s.created += 1,
            Some(ChangeAction::Update) => s.updated += 1,
            Some(ChangeAction::Delete) => s.deleted += 1,
        }
        if let Some(action) = action {
            self.changes.push(ImportChange { section: section.into(), key, action, fields });
        }
    }

    /// Creates the row, updates the fields that differ, or leaves it alone.
    #[allow(clippy::too_many_arguments)]
    async fn upsert<R: Resource, S: Serialize>(
        &mut self,
        section: &str,
        path: &str,
        key: String,
        existing: Option<(Uuid, &S)>,
        new: &S,
        create: ColumnSet,
        update: ColumnSet,
    ) -> Result<Uuid, AppError> {
        match existing {
            None => {
                let row = simple::create_in::<R>(self.conn, self.ctx, create).await.map_err(|e| at(path, e))?;
                self.record(section, key, Some(ChangeAction::Create), Vec::new());
                Ok(R::id(&row))
            }
            Some((id, old)) => {
                let fields = diff(old, new);
                if fields.is_empty() {
                    self.record(section, key, None, Vec::new());
                } else {
                    simple::update_in::<R>(self.conn, self.ctx, id, update).await.map_err(|e| at(path, e))?;
                    self.record(section, key, Some(ChangeAction::Update), fields);
                }
                Ok(id)
            }
        }
    }
}

fn not_in_file<'a>(here: impl Iterator<Item = &'a String>, file: &HashSet<String>) -> i64 {
    here.filter(|k| !file.contains(*k)).count() as i64
}

async fn run(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    file: &ConfigFile,
    mode: ImportMode,
) -> Result<ImportResult, AppError> {
    // One import at a time.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('shadoucmdb:config-import'))").execute(&mut *conn).await?;
    let snap = snapshot(conn).await?;
    let mut warnings = Vec::new();
    let decoded = validate(file, &snap, &mut warnings)?;
    let Snapshot { file: current, ids, .. } = snap;
    let cur_dm = current.data_model.unwrap_or_default();
    let cur_lk = current.lookups.unwrap_or_default();
    let cur_profiles = current.permission_profiles.unwrap_or_default();
    let mut im = Importer { conn, ctx, ids, summary: Vec::new(), changes: Vec::new() };

    // ---- lookups ----
    if let Some(lk) = &file.lookups {
        let keys: HashSet<String> = lk.statuses.iter().map(|s| s.key.clone()).collect();
        im.section("statuses", not_in_file(im.ids.statuses.keys(), &keys));
        let old: HashMap<&str, &StatusSpec> = cur_lk.statuses.iter().map(|s| (s.key.as_str(), s)).collect();
        for (i, s) in lk.statuses.iter().enumerate() {
            let existing = old.get(s.key.as_str()).map(|o| (im.ids.statuses[&s.key], *o));
            let mut c = ColumnSet::default();
            c.opt("name", Some(s.name.clone()))
                .opt("description", Some(s.description.clone()))
                .opt("is_operational", Some(s.is_operational))
                .opt("sort_order", Some(s.sort_order))
                .opt("is_active", Some(s.is_active));
            let mut create = c.clone();
            create.opt("key", Some(s.key.clone()));
            let id = im
                .upsert::<Statuses, _>(
                    "statuses",
                    &format!("lookups.statuses.{i}"),
                    s.key.clone(),
                    existing,
                    s,
                    create,
                    c,
                )
                .await?;
            im.ids.statuses.insert(s.key.clone(), id);
        }

        let keys: HashSet<String> = lk.environments.iter().map(|s| s.key.clone()).collect();
        im.section("environments", not_in_file(im.ids.environments.keys(), &keys));
        let old: HashMap<&str, &EnvironmentSpec> = cur_lk.environments.iter().map(|s| (s.key.as_str(), s)).collect();
        for (i, s) in lk.environments.iter().enumerate() {
            let existing = old.get(s.key.as_str()).map(|o| (im.ids.environments[&s.key], *o));
            let mut c = ColumnSet::default();
            c.opt("name", Some(s.name.clone()))
                .opt("description", Some(s.description.clone()))
                .opt("sort_order", Some(s.sort_order))
                .opt("is_active", Some(s.is_active));
            let mut create = c.clone();
            create.opt("key", Some(s.key.clone()));
            let path = format!("lookups.environments.{i}");
            let id = im.upsert::<Environments, _>("environments", &path, s.key.clone(), existing, s, create, c).await?;
            im.ids.environments.insert(s.key.clone(), id);
        }

        let keys: HashSet<String> = lk.locations.iter().map(|s| s.key.clone()).collect();
        im.section("locations", not_in_file(im.ids.locations.keys(), &keys));
        let old: HashMap<&str, &LocationSpec> = cur_lk.locations.iter().map(|s| (s.key.as_str(), s)).collect();
        let indexed: Vec<(usize, &LocationSpec)> = lk.locations.iter().enumerate().collect();
        let (ordered, _) = parents_first(indexed, |(_, l)| &l.key, |(_, l)| l.parent.as_deref());
        for (i, l) in ordered {
            let existing = old.get(l.key.as_str()).map(|o| (im.ids.locations[&l.key], *o));
            let parent_id = l.parent.as_ref().map(|p| im.ids.locations[p]);
            let mut c = ColumnSet::default();
            c.opt("name", Some(l.name.clone()))
                .opt("description", Some(l.description.clone()))
                .opt("parent_id", Some(parent_id))
                .opt("location_type", Some(text(l.location_type.as_str())))
                .opt("address", Some(l.address.clone()))
                .opt("sort_order", Some(l.sort_order))
                .opt("is_active", Some(l.is_active));
            let mut create = c.clone();
            create.opt("key", Some(l.key.clone()));
            let path = format!("lookups.locations.{i}");
            let id = im.upsert::<Locations, _>("locations", &path, l.key.clone(), existing, l, create, c).await?;
            im.ids.locations.insert(l.key.clone(), id);
        }

        let keys: HashSet<String> = lk.owners.iter().map(|o| owner_key(o.kind, &o.name)).collect();
        im.section("owners", not_in_file(im.ids.owners.keys(), &keys));
        let old: HashMap<String, &OwnerSpec> = cur_lk.owners.iter().map(|o| (owner_key(o.kind, &o.name), o)).collect();
        for (i, o) in lk.owners.iter().enumerate() {
            let key = owner_key(o.kind, &o.name);
            let existing = old.get(&key).and_then(|x| im.ids.owners.get(&key).map(|id| (*id, *x)));
            let kind =
                serde_json::to_value(o.kind).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default();
            let mut c = ColumnSet::default();
            c.opt("kind", Some(kind))
                .opt("name", Some(o.name.clone()))
                .opt("email", Some(o.email.clone()))
                .opt("external_ref", Some(o.external_ref.clone()))
                .opt("is_active", Some(o.is_active));
            let id = im
                .upsert::<Owners, _>("owners", &format!("lookups.owners.{i}"), key.clone(), existing, o, c.clone(), c)
                .await?;
            im.ids.owners.insert(key, id);
        }

        let keys: HashSet<String> = lk.lists.iter().map(|l| l.key.clone()).collect();
        im.section("lookupLists", not_in_file(im.ids.lists.keys(), &keys));
        let value_keys: HashSet<String> =
            lk.lists.iter().flat_map(|l| l.values.iter().map(move |v| format!("{}.{}", l.key, v.key))).collect();
        let here: Vec<String> =
            im.ids.values.keys().filter(|(l, _)| keys.contains(l)).map(|(l, v)| format!("{l}.{v}")).collect();
        im.section("lookupListValues", not_in_file(here.iter(), &value_keys));
        let old: HashMap<&str, &LookupListSpec> = cur_lk.lists.iter().map(|l| (l.key.as_str(), l)).collect();
        for (i, l) in lk.lists.iter().enumerate() {
            let bare = |l: &LookupListSpec| LookupListSpec { values: Vec::new(), ..l.clone() };
            let new_bare = bare(l);
            let old_bare = old.get(l.key.as_str()).map(|o| bare(o));
            let existing = old_bare.as_ref().map(|o| (im.ids.lists[&l.key], o));
            let mut c = ColumnSet::default();
            c.opt("name", Some(l.name.clone()))
                .opt("description", Some(l.description.clone()))
                .opt("sort_order", Some(l.sort_order))
                .opt("is_active", Some(l.is_active));
            let mut create = c.clone();
            create.opt("key", Some(l.key.clone()));
            let path = format!("lookups.lists.{i}");
            let list_id = im
                .upsert::<LookupLists, _>("lookupLists", &path, l.key.clone(), existing, &new_bare, create, c)
                .await?;
            im.ids.lists.insert(l.key.clone(), list_id);

            let old_values: HashMap<&str, &LookupValueSpec> = old
                .get(l.key.as_str())
                .map(|o| o.values.iter().map(|v| (v.key.as_str(), v)).collect())
                .unwrap_or_default();
            for (j, v) in l.values.iter().enumerate() {
                let vkey = (l.key.clone(), v.key.clone());
                let existing = old_values.get(v.key.as_str()).map(|o| (im.ids.values[&vkey], *o));
                let mut c = ColumnSet::default();
                c.opt("name", Some(v.name.clone()))
                    .opt("description", Some(v.description.clone()))
                    .opt("color", Some(v.color.clone()))
                    .opt("sort_order", Some(v.sort_order))
                    .opt("is_active", Some(v.is_active));
                let mut create = c.clone();
                create.opt("list_id", Some(list_id)).opt("key", Some(v.key.clone()));
                let path = format!("lookups.lists.{i}.values.{j}");
                let key = format!("{}.{}", l.key, v.key);
                let id =
                    im.upsert::<LookupListValues, _>("lookupListValues", &path, key, existing, v, create, c).await?;
                im.ids.values.insert(vkey, id);
            }
        }
    }

    // ---- data model ----
    if let Some(dm) = &file.data_model {
        let keys: HashSet<String> = dm.areas.iter().map(|a| a.key.clone()).collect();
        im.section("areas", not_in_file(im.ids.areas.keys(), &keys));
        let old: HashMap<&str, &AreaSpec> = cur_dm.areas.iter().map(|a| (a.key.as_str(), a)).collect();
        for (i, a) in dm.areas.iter().enumerate() {
            let existing = old.get(a.key.as_str()).map(|o| (im.ids.areas[&a.key], *o));
            let mut c = ColumnSet::default();
            c.opt("name", Some(a.name.clone()))
                .opt("description", Some(a.description.clone()))
                .opt("icon", Some(a.icon.clone()))
                .opt("color", Some(a.color.clone()))
                .opt("sort_order", Some(a.sort_order))
                .opt("is_active", Some(a.is_active));
            let mut create = c.clone();
            create.opt("key", Some(a.key.clone()));
            let path = format!("dataModel.areas.{i}");
            let id = im.upsert::<Areas, _>("areas", &path, a.key.clone(), existing, a, create, c).await?;
            im.ids.areas.insert(a.key.clone(), id);
        }

        let keys: HashSet<String> = dm.classes.iter().map(|c| c.key.clone()).collect();
        im.section("classes", not_in_file(im.ids.classes.keys(), &keys));
        let old: HashMap<&str, &ClassSpec> = cur_dm.classes.iter().map(|c| (c.key.as_str(), c)).collect();
        let indexed: Vec<(usize, &ClassSpec)> = dm.classes.iter().enumerate().collect();
        let (ordered, _) = parents_first(indexed, |(_, c)| &c.key, |(_, c)| c.parent.as_deref());
        for (i, cls) in ordered {
            let existing = old.get(cls.key.as_str()).map(|o| (im.ids.classes[&cls.key], *o));
            // A class without an area (version 1 files) stays where it is.
            let with_area;
            let cls = match (&cls.area, existing) {
                (None, Some((_, o))) => {
                    with_area = ClassSpec { area: o.area.clone(), ..cls.clone() };
                    &with_area
                }
                _ => cls,
            };
            let parent_id = cls.parent.as_ref().map(|p| im.ids.classes[p]);
            let mut c = ColumnSet::default();
            c.opt("name", Some(cls.name.clone()))
                .opt("description", Some(cls.description.clone()))
                .opt("parent_id", Some(parent_id))
                .opt("is_abstract", Some(cls.is_abstract))
                .opt("icon", Some(cls.icon.clone()))
                .opt("color", Some(cls.color.clone()))
                .opt("sort_order", Some(cls.sort_order))
                .opt("is_active", Some(cls.is_active));
            let mut create = c.clone();
            let path = format!("dataModel.classes.{i}");
            if existing.is_none() {
                // Files without areas (version 1) put new classes where the upgrade put existing ones.
                let area = cls.area.clone().unwrap_or_else(|| DEFAULT_AREA.0.to_owned());
                let area_id = match im.ids.areas.get(&area) {
                    Some(id) => *id,
                    None => {
                        let mut a = ColumnSet::default();
                        a.opt("key", Some(area.clone())).opt("name", Some(DEFAULT_AREA.1.to_owned()));
                        let row = simple::create_in::<Areas>(im.conn, im.ctx, a).await.map_err(|e| at(&path, e))?;
                        im.record("areas", area.clone(), Some(ChangeAction::Create), Vec::new());
                        im.ids.areas.insert(area.clone(), row.id);
                        row.id
                    }
                };
                create.opt("area_id", Some(area_id));
            }
            create.opt("key", Some(cls.key.clone()));
            let id = im.upsert::<CiClasses, _>("classes", &path, cls.key.clone(), existing, cls, create, c).await?;
            im.ids.classes.insert(cls.key.clone(), id);
        }

        let keys: HashSet<String> = dm.attributes.iter().map(|a| format!("{}.{}", a.class, a.key)).collect();
        let here: Vec<String> = im.ids.attributes.keys().map(|(c, a)| format!("{c}.{a}")).collect();
        im.section("attributes", not_in_file(here.iter(), &keys));
        let old: HashMap<(&str, &str), &AttributeSpec> =
            cur_dm.attributes.iter().map(|a| ((a.class.as_str(), a.key.as_str()), a)).collect();
        for (i, a) in dm.attributes.iter().enumerate() {
            let akey = (a.class.clone(), a.key.clone());
            let existing = old.get(&(a.class.as_str(), a.key.as_str())).map(|o| (im.ids.attributes[&akey], *o));
            let default_value = match (&a.lookup_list, &a.default_value) {
                (Some(list), Some(Value::String(k))) if a.data_type == AttributeDataType::Lookup => {
                    Some(Value::String(im.ids.values[&(list.clone(), k.clone())].to_string()))
                }
                (_, d) => d.clone().filter(|v| !v.is_null()),
            };
            let mut c = ColumnSet::default();
            c.opt("label", Some(a.label.clone()))
                .opt("description", Some(a.description.clone()))
                .opt("is_required", Some(a.is_required))
                .opt(
                    "enum_values",
                    Some(a.enum_values.as_ref().map(|v| Value::Array(v.iter().cloned().map(Value::String).collect()))),
                )
                .opt("validation", Some(a.validation.as_ref().map(crud::json)))
                .opt("group_name", Some(a.group_name.clone()))
                .opt("help_text", Some(a.help_text.clone()))
                .opt("default_value", Some(default_value))
                .opt("sort_order", Some(a.sort_order))
                .opt("is_active", Some(a.is_active));
            let mut create = c.clone();
            create
                .opt("class_id", Some(im.ids.classes[&a.class]))
                .opt("key", Some(a.key.clone()))
                .opt("data_type", Some(text(a.data_type.as_str())))
                .opt("reference_class_id", Some(a.reference_class.as_ref().map(|k| im.ids.classes[k])))
                .opt("lookup_list_id", Some(a.lookup_list.as_ref().map(|k| im.ids.lists[k])));
            let path = format!("dataModel.attributes.{i}");
            let key = format!("{}.{}", a.class, a.key);
            let id = im.upsert::<AttributeDefinitions, _>("attributes", &path, key, existing, a, create, c).await?;
            im.ids.attributes.insert(akey, id);
        }

        let keys: HashSet<String> = dm.relationship_types.iter().map(|t| t.key.clone()).collect();
        im.section("relationshipTypes", not_in_file(im.ids.types.keys(), &keys));
        let old: HashMap<&str, &RelationshipTypeSpec> =
            cur_dm.relationship_types.iter().map(|t| (t.key.as_str(), t)).collect();
        for (i, t) in dm.relationship_types.iter().enumerate() {
            let existing = old.get(t.key.as_str()).map(|o| (im.ids.types[&t.key], *o));
            let mut c = ColumnSet::default();
            c.opt("name", Some(t.name.clone()))
                .opt("description", Some(t.description.clone()))
                .opt("forward_label", Some(t.forward_label.clone()))
                .opt("reverse_label", Some(t.reverse_label.clone()))
                .opt("sort_order", Some(t.sort_order))
                .opt("is_active", Some(t.is_active));
            let mut create = c.clone();
            create.opt("key", Some(t.key.clone())).opt("is_directional", Some(t.is_directional));
            let path = format!("dataModel.relationshipTypes.{i}");
            let id = im
                .upsert::<RelationshipTypes, _>("relationshipTypes", &path, t.key.clone(), existing, t, create, c)
                .await?;
            im.ids.types.insert(t.key.clone(), id);
        }

        let keys: HashSet<String> = dm
            .relationship_rules
            .iter()
            .map(|r| format!("{} {} -> {}", r.relationship_type, r.source_class, r.target_class))
            .collect();
        let here: Vec<String> = im.ids.rules.iter().map(|(t, s, g)| format!("{t} {s} -> {g}")).collect();
        im.section("relationshipRules", not_in_file(here.iter(), &keys));
        for (i, r) in dm.relationship_rules.iter().enumerate() {
            let triple = (r.relationship_type.clone(), r.source_class.clone(), r.target_class.clone());
            let key = format!("{} {} -> {}", r.relationship_type, r.source_class, r.target_class);
            if im.ids.rules.contains(&triple) {
                im.record("relationshipRules", key, None, Vec::new());
                continue;
            }
            let mut c = ColumnSet::default();
            c.opt("relationship_type_id", Some(im.ids.types[&r.relationship_type]))
                .opt("source_class_id", Some(im.ids.classes[&r.source_class]))
                .opt("target_class_id", Some(im.ids.classes[&r.target_class]));
            let path = format!("dataModel.relationshipRules.{i}");
            simple::create_in::<RelationshipRules>(im.conn, im.ctx, c).await.map_err(|e| at(&path, e))?;
            im.ids.rules.insert(triple);
            im.record("relationshipRules", key, Some(ChangeAction::Create), Vec::new());
        }
    }

    // ---- permission profiles ----
    if let Some(list) = &file.permission_profiles {
        let names: HashSet<String> = list.iter().map(|p| p.name.to_lowercase()).collect();
        let editable: Vec<String> = cur_profiles.iter().map(|p| p.name.to_lowercase()).collect();
        im.section("permissionProfiles", not_in_file(editable.iter(), &names));
        let old: HashMap<String, &ProfileSpec> = cur_profiles.iter().map(|p| (p.name.to_lowercase(), p)).collect();
        for (i, p) in list.iter().enumerate() {
            let lower = p.name.to_lowercase();
            let path = format!("permissionProfiles.{i}");
            if im.ids.profiles.contains_key(&lower) && !old.contains_key(&lower) {
                continue; // the built-in profile (warned about in validate)
            }
            let mut spec = p.clone();
            normalise_profile(&mut spec);
            let grants: Vec<ClassPermission> = spec
                .class_permissions
                .iter()
                .map(|g| {
                    let rights = ClassRights { view: g.view, create: g.create, edit: g.edit, delete: g.delete };
                    ClassPermission::new(g.class.as_ref().map(|k| im.ids.classes[k]), rights)
                })
                .collect();
            match old.get(&lower) {
                None => {
                    let dto = profiles::insert(
                        im.conn,
                        im.ctx,
                        &spec.name,
                        spec.description.as_deref(),
                        &spec.global_permissions,
                        &grants,
                    )
                    .await
                    .map_err(|e| at(&path, e))?;
                    im.ids.profiles.insert(lower, dto.id);
                    im.record("permissionProfiles", spec.name.clone(), Some(ChangeAction::Create), Vec::new());
                }
                Some(before) => {
                    let fields = diff(*before, &spec);
                    if fields.is_empty() {
                        im.record("permissionProfiles", spec.name.clone(), None, Vec::new());
                        continue;
                    }
                    profiles::update_in(
                        im.conn,
                        im.ctx,
                        im.ids.profiles[&lower],
                        Some(&spec.name),
                        Some(spec.description.as_deref()),
                        Some(&spec.global_permissions),
                        Some(&grants),
                    )
                    .await
                    .map_err(|e| at(&path, e))?;
                    im.record("permissionProfiles", spec.name.clone(), Some(ChangeAction::Update), fields);
                }
            }
        }
    }

    // ---- UI settings ----
    let mut ui_settings_issues = Vec::new();
    if let Some(section) = &file.ui_settings {
        im.section("uiSettings", 0);
        let old = current.ui_settings.as_ref().map(|u| u.settings.clone()).unwrap_or_default();
        let changed = ui::save_in(im.conn, im.ctx, None, &section.settings, Some("Imported from a configuration file"))
            .await
            .map_err(|e| at("uiSettings.settings", e))?;
        if changed {
            im.record("uiSettings", "settings".into(), Some(ChangeAction::Update), diff(&old, &section.settings));
        } else {
            im.record("uiSettings", "settings".into(), None, Vec::new());
        }

        let old_assets = current.ui_settings.unwrap_or(UiSettingsSection {
            settings: UiSettingsDocument::default(),
            logo: None,
            favicon: None,
        });
        for (kind, new, old) in [
            (AssetKind::Logo, &decoded.logo, &old_assets.logo),
            (AssetKind::Favicon, &decoded.favicon, &old_assets.favicon),
        ] {
            let path = format!("uiSettings.{}", kind.as_str());
            let old_bytes = old.as_ref().and_then(|a| base64::engine::general_purpose::STANDARD.decode(&a.data).ok());
            let info = |t: Option<ImageType>, b: Option<&Vec<u8>>| -> Value {
                match (t, b) {
                    (Some(t), Some(b)) => {
                        serde_json::json!({ "contentType": t.as_str(), "size": b.len(), "sha256": ui::sha256_hex(b) })
                    }
                    _ => Value::Null,
                }
            };
            let from = info(old.as_ref().map(|a| a.content_type), old_bytes.as_ref());
            match new {
                Some((t, bytes)) => {
                    let to = info(Some(*t), Some(bytes));
                    if from == to {
                        im.record("uiSettings", kind.as_str().into(), None, Vec::new());
                        continue;
                    }
                    ui::put_asset_in(im.conn, im.ctx, kind, *t, bytes).await.map_err(|e| at(&path, e))?;
                    let action = if old.is_some() { ChangeAction::Update } else { ChangeAction::Create };
                    let fields = vec![FieldChange { field: kind.as_str().into(), from, to }];
                    im.record("uiSettings", kind.as_str().into(), Some(action), fields);
                }
                None if old.is_some() => {
                    ui::delete_asset_in(im.conn, im.ctx, kind).await.map_err(|e| at(&path, e))?;
                    let fields = vec![FieldChange { field: kind.as_str().into(), from, to: Value::Null }];
                    im.record("uiSettings", kind.as_str().into(), Some(ChangeAction::Delete), fields);
                }
                None => {}
            }
        }
        let model = ui_data::model(im.conn).await?;
        ui_settings_issues = document::resolve(&section.settings, &model).1;
    }

    let Importer { summary, changes, .. } = im;
    Ok(ImportResult {
        mode,
        applied: false,
        schema_changes: Vec::new(),
        summary,
        changes,
        warnings,
        ui_settings_issues,
    })
}

fn check_format(file: &ConfigFile) -> Result<(), AppError> {
    if file.format != FORMAT || !(1..=FORMAT_VERSION).contains(&file.format_version) {
        return Err(AppError::field(
            "formatVersion",
            format!("This server reads {FORMAT} versions 1 to {FORMAT_VERSION}"),
            "unsupported",
        ));
    }
    Ok(())
}

/// `config.export_import` lets a file in, not past the permission each section
/// needs on its own admin API: the data model and lookups (which run DDL) need
/// `datamodel.manage`, UI settings need `customization.manage`. Checked for dry
/// runs too, before anything touches the database. Profiles are bounded per
/// profile by what the importing user holds.
fn check_sections(ctx: &RequestContext, file: &ConfigFile) -> Result<(), AppError> {
    let data_model = file.data_model.as_ref().is_some_and(|d| {
        !(d.areas.is_empty()
            && d.classes.is_empty()
            && d.attributes.is_empty()
            && d.relationship_types.is_empty()
            && d.relationship_rules.is_empty())
    });
    let lookups = file.lookups.as_ref().is_some_and(|l| {
        !(l.statuses.is_empty()
            && l.environments.is_empty()
            && l.locations.is_empty()
            && l.owners.is_empty()
            && l.lists.is_empty())
    });
    if data_model || lookups {
        ctx.require(GlobalPermission::DatamodelManage)?;
    }
    if file.ui_settings.is_some() {
        ctx.require(GlobalPermission::CustomizationManage)?;
    }
    Ok(())
}

pub async fn import(
    pool: &PgPool,
    ctx: &RequestContext,
    file: &ConfigFile,
    mode: ImportMode,
) -> Result<ImportResult, AppError> {
    check_format(file)?;
    check_sections(ctx, file)?;
    let mut tx = pool.begin().await?;
    let (result, schema_changes) = engine::collect_previews(run(&mut tx, ctx, file, mode)).await;
    let mut result = result?;
    result.schema_changes = schema_changes;
    match mode {
        ImportMode::DryRun => tx.rollback().await?,
        ImportMode::Apply => {
            tx.commit().await?;
            result.applied = true;
        }
    }
    Ok(result)
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

pub fn routes() -> Vec<Route> {
    vec![
        route(Method::GET, "/api/v1/admin/config/export", "exportConfig")
            .tag(TAG)
            .summary("Download the whole configuration as one JSON file")
            .description(
                "Data model (classes, attributes, relationship types and rules), lookups (statuses, environments, \
                 locations, owners, lookup lists), permission profiles (not the built-in one) and UI settings \
                 including the logo and favicon. Never contains users, passwords, sessions, CIs or relationships. \
                 Everything refers to everything else by key, so the file imports into another install. Answers \
                 with `Content-Disposition: attachment`.",
            )
            .requires(GlobalPermission::ConfigExportImport)
            .handle(|api, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| async move {
                let file = export(&api.pool).await?;
                let name = format!(
                    "attachment; filename=\"shadoucmdb-config-{}.json\"",
                    chrono::Utc::now().format("%Y%m%d-%H%M%S")
                );
                let disposition = HeaderValue::from_str(&name).map_err(|_| AppError::internal())?;
                Ok(WithHeaders(Json(file), vec![(header::CONTENT_DISPOSITION, disposition)]))
            }),
        route(Method::POST, "/api/v1/admin/config/import", "importConfig")
            .tag(TAG)
            .summary("Import a configuration file (dry run or apply)")
            .description(
                "`mode=dry_run` validates the file and runs the whole import in a transaction that is rolled back, \
                 returning the diff; `mode=apply` does the same and commits. Rows are matched by key (owners by kind \
                 and name, profiles by name) and created or updated; nothing is deleted, so data missing from the file \
                 is kept (counted as `notInFile`). The `uiSettings` section replaces the settings (as a new version) \
                 and the logo and favicon. All sections are optional. Problems in the file are reported together as \
                 400 VALIDATION_ERROR with paths into the file; a change the data model does not allow (e.g. making \
                 an attribute required while CIs lack a value) fails with the same error the admin API gives, with \
                 the file path prefixed. A non-empty `dataModel` or `lookups` section also requires \
                 `datamodel.manage`, and a `uiSettings` section `customization.manage` (403 otherwise, dry run \
                 included). Profiles cannot grant more than the importing user holds (403). Every \
                 applied change is audited.",
            )
            .requires(GlobalPermission::ConfigExportImport)
            .body_limit(IMPORT_BODY_LIMIT)
            .errors(&[ErrorCode::Conflict, ErrorCode::InUse, ErrorCode::PayloadTooLarge])
            .handle(
                |api, In(NoPath, Query(q), Body(file)): In<NoPath, Query<ImportQuery>, Body<ConfigFile>>| async move {
                    Ok(Json(import(&api.pool, &api.ctx, &file, q.mode).await?))
                },
            ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Principal;
    use crate::auth::permissions::Permissions;
    use crate::db::scratch;

    async fn user_ctx(pool: &PgPool, username: &str, global: &[GlobalPermission]) -> RequestContext {
        let user_id: Uuid = sqlx::query_scalar(
            "INSERT INTO users (username, display_name, password_hash) VALUES ($1, $1, '$argon2id$v=19$test')
             RETURNING id",
        )
        .bind(username)
        .fetch_one(pool)
        .await
        .unwrap();
        let permissions = Permissions { global: global.iter().copied().collect(), ..Default::default() };
        let principal = Principal {
            user_id,
            username: username.into(),
            session_id: Uuid::new_v4(),
            csrf_token: String::new(),
            permissions,
        };
        RequestContext::user(std::sync::Arc::new(principal), "test".into())
    }

    async fn schema_change_count(pool: &PgPool) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM cmdb.schema_changes").fetch_one(pool).await.unwrap()
    }

    /// GH#59: `config.export_import` alone must not run data-model DDL (or touch
    /// lookups or UI settings) through the import.
    #[tokio::test]
    async fn import_sections_need_their_own_permission() {
        let Some(src) = scratch::database("import_sections_src").await else { return };
        let Some(dst) = scratch::database("import_sections_dst").await else { return };
        crate::seed::install_template(&src.pool, "it_infrastructure").await.unwrap();
        let file = export(&src.pool).await.unwrap();
        assert!(!file.data_model.as_ref().unwrap().classes.is_empty());

        let before = schema_change_count(&dst.pool).await;
        let only_import = user_ctx(&dst.pool, "importer", &[GlobalPermission::ConfigExportImport]).await;
        for mode in [ImportMode::DryRun, ImportMode::Apply] {
            let err = import(&dst.pool, &only_import, &file, mode).await.unwrap_err();
            assert_eq!(err.code, ErrorCode::Forbidden, "{mode:?}: {err}");
            assert!(err.message.contains("datamodel.manage"), "{err}");
        }
        let lookups_only = ConfigFile { data_model: None, ui_settings: None, ..file.clone() };
        let err = import(&dst.pool, &only_import, &lookups_only, ImportMode::Apply).await.unwrap_err();
        assert!(err.message.contains("datamodel.manage"), "{err}");
        let ui_only = ConfigFile { data_model: None, lookups: None, ..file.clone() };
        let err = import(&dst.pool, &only_import, &ui_only, ImportMode::Apply).await.unwrap_err();
        assert!(err.message.contains("customization.manage"), "{err}");
        assert_eq!(schema_change_count(&dst.pool).await, before);

        // Profiles alone stay open to the permission (bounded by what the user holds).
        let profiles_only = ConfigFile { data_model: None, lookups: None, ui_settings: None, ..file.clone() };
        import(&dst.pool, &only_import, &profiles_only, ImportMode::DryRun).await.unwrap();

        let full = user_ctx(
            &dst.pool,
            "configurator",
            &[
                GlobalPermission::ConfigExportImport,
                GlobalPermission::DatamodelManage,
                GlobalPermission::CustomizationManage,
            ],
        )
        .await;
        let res = import(&dst.pool, &full, &file, ImportMode::Apply).await.unwrap();
        assert!(res.applied);
        assert!(schema_change_count(&dst.pool).await > before);

        src.drop().await;
        dst.drop().await;
    }

    #[test]
    fn parents_come_first_and_cycles_are_left_over() {
        let items = vec![
            ("c", Some("b")),
            ("b", Some("a")),
            ("a", None),
            ("x", Some("y")),
            ("y", Some("x")),
            ("z", Some("elsewhere")),
        ];
        let (ordered, cyclic) = parents_first(items, |i| i.0, |i| i.1);
        let keys: Vec<&str> = ordered.iter().map(|i| i.0).collect();
        assert_eq!(keys, ["a", "z", "b", "c"]);
        let rest: Vec<&str> = cyclic.iter().map(|i| i.0).collect();
        assert_eq!(rest, ["x", "y"]);
    }

    #[test]
    fn diff_lists_changed_fields_only() {
        let a = StatusSpec {
            key: "live".into(),
            name: "Live".into(),
            description: None,
            is_operational: true,
            sort_order: 0,
            is_active: true,
        };
        let b = StatusSpec { name: "In service".into(), sort_order: 10, ..a.clone() };
        let d = diff(&a, &b);
        let fields: Vec<&str> = d.iter().map(|f| f.field.as_str()).collect();
        assert_eq!(fields, ["name", "sortOrder"]);
        assert_eq!(d[0].from, Value::from("Live"));
        assert!(diff(&a, &a).is_empty());
    }

    #[test]
    fn profiles_compare_after_normalising() {
        let mut p = ProfileSpec {
            name: "Ops".into(),
            description: None,
            global_permissions: vec![GlobalPermission::AuditView, GlobalPermission::UsersManage],
            class_permissions: vec![
                ClassGrantSpec { class: Some("server".into()), view: false, create: false, edit: true, delete: false },
                ClassGrantSpec { class: None, view: true, create: false, edit: false, delete: false },
                ClassGrantSpec { class: Some("app".into()), view: false, create: false, edit: false, delete: false },
            ],
        };
        normalise_profile(&mut p);
        assert_eq!(p.global_permissions, [GlobalPermission::UsersManage, GlobalPermission::AuditView]);
        let classes: Vec<Option<&str>> = p.class_permissions.iter().map(|g| g.class.as_deref()).collect();
        assert_eq!(classes, [None, Some("server")]);
        assert!(p.class_permissions[1].view, "edit implies view");
    }

    #[test]
    fn errors_get_the_file_path() {
        let e = at("dataModel.attributes.3", AppError::field("isRequired", "CIs without a value: 2", "values_missing"));
        let d = &e.details.unwrap()[0];
        assert_eq!(d.field, "dataModel.attributes.3.isRequired");
        assert!(e.message.starts_with("dataModel.attributes.3: "));
        let e = at("lookups.statuses.0", AppError::conflict("Duplicate"));
        assert_eq!(e.details.unwrap()[0].field, "lookups.statuses.0");
        assert_eq!(e.code, ErrorCode::Conflict);
    }
}
