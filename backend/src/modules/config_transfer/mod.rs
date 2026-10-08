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
mod legacy;
mod system_roles;
mod workflows;

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
    AttributeDataType, AttributeDefinition, AttributeDefinitions, CiClass, CiClasses, ClassSystemRole,
    RelationshipRule, RelationshipRules, RelationshipType, RelationshipTypeSystemRole, RelationshipTypes,
    ValidationRules,
};
use super::imports::saved;
use super::imports::schemas::ColumnTarget;
use super::lookups::{LookupList, LookupListValue, LookupListValues, LookupLists};
use super::profiles::{self, ClassPermission};
use super::saved_views;
use super::simple_resource::{self as simple, Resource};
use super::ui_settings::assets::{AssetKind, ImageType};
use super::ui_settings::document::{self, Issue, UiSettingsDocument};
use super::ui_settings::{self as ui};
use crate::api::context::RequestContext;
use crate::api::route::{Body, In, Json, NoBody, NoPath, NoQuery, Query, Route, WithHeaders, route};
use crate::auth::permissions::{ClassOp, ClassRights, GlobalPermission};
use crate::data::crud::{self, ColumnSet};
use crate::data::ui_settings as ui_data;
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::schema::model::Model;
use crate::schema::naming::{self, NameKind};
use crate::schema::{self as engine, SchemaChange};

const TAG: &str = "Configuration export/import";

/// Largest accepted import file (the images alone can be ~850 KiB base64).
const IMPORT_BODY_LIMIT: usize = 16 * 1024 * 1024;
/// Entries per file: lookup values (the rows of the former tables included),
/// data-model entries, permission profiles, saved import mappings and shared
/// saved views. An applied import writes an audit row per changed entry just
/// before commit, and those rows hold the audit chain head, so every sign-in
/// waits for them; the per-list limits alone still let a file within the body
/// limit carry half a million lookup values (GH#546) or ~77,000 data-model
/// entries (GH#551). Checked before anything touches the database, dry runs
/// included.
const MAX_ENTRIES: usize = 25_000;

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
    /// The row's key in the file (class.key for attributes, list.value for list values)
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
    lists: HashMap<String, Uuid>,
    values: HashMap<(String, String), Uuid>,
    /// lower(name) -> id; the built-in profile included
    profiles: HashMap<String, Uuid>,
    /// (class key, lower(name)) -> id
    mappings: HashMap<(String, String), Uuid>,
    /// (context, lower(name)) -> id of a shared saved view
    views: HashMap<(String, String), Uuid>,
    /// lower(name) -> (id, name) of the user groups a file's workflow approvers name (groups are not in a file)
    groups: HashMap<String, (Uuid, String)>,
    /// lower(username) -> (id, username) of the users a file's workflow approvers name (users are not in a file)
    users: HashMap<String, (Uuid, String)>,
}

struct Snapshot {
    file: ConfigFile,
    ids: Ids,
    builtin_profile: String,
    /// Which stored shared views the importer may see (SHAA-578 §3.2, GH#475/#476)
    views_catalogue: saved_views::resolve::Catalogue,
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
    let field_keys: HashMap<Uuid, String> = sqlx::query_as::<_, (Uuid, String)>(
        "SELECT DISTINCT d.id, d.key FROM cmdb.ci_attribute_definitions d JOIN cmdb.ci_classes c
         ON d.id IN (c.title_attribute_id, c.owner_attribute_id, c.end_of_life_attribute_id)",
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .collect();
    // Built-in classes and types are matched by role on import (format version 5).
    let class_roles: HashMap<Uuid, ClassSystemRole> =
        sqlx::query_as("SELECT id, system_role FROM cmdb.ci_classes WHERE system_role IS NOT NULL")
            .fetch_all(&mut *conn)
            .await?
            .into_iter()
            .collect();
    let type_roles: HashMap<Uuid, RelationshipTypeSystemRole> =
        sqlx::query_as("SELECT id, system_role FROM cmdb.relationship_types WHERE system_role IS NOT NULL")
            .fetch_all(&mut *conn)
            .await?
            .into_iter()
            .collect();
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
            title_attribute: Some(c.title_attribute_id.and_then(|t| field_keys.get(&t).cloned())),
            owner_attribute: Some(c.owner_attribute_id.and_then(|t| field_keys.get(&t).cloned())),
            end_of_life_attribute: Some(c.end_of_life_attribute_id.and_then(|t| field_keys.get(&t).cloned())),
            system_role: class_roles.get(&c.id).copied(),
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
            parent: l.parent_list_id.and_then(|id| list_key.get(&id).cloned()),
            system_role: l.system_role,
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
                    parent: v.parent_value_id.and_then(|id| value_key.get(&id).cloned()),
                })
                .collect(),
        })
        .collect();
    let (mut list_specs, cyclic) = parents_first(list_specs, |l| &l.key, |l| l.parent.as_deref());
    list_specs.extend(cyclic);

    let attrs: Vec<AttributeDefinition> =
        crud::select_all(conn, AttributeDefinitions::TABLE, AttributeDefinitions::COLUMNS, "sort_order, key").await?;
    let attr_key: HashMap<Uuid, String> = attrs.iter().map(|a| (a.id, a.key.clone())).collect();
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
                is_expected: Some(a.is_expected),
                is_identifying: Some(a.is_identifying),
                enum_values: a.enum_values.as_ref().map(|v| v.0.clone()),
                reference_class: a.reference_class_id.and_then(|id| class_key.get(&id).cloned()),
                lookup_list: a.lookup_list_id.and_then(|id| list_key.get(&id).cloned()),
                parent_attribute: a.parent_attribute_id.and_then(|id| attr_key.get(&id).cloned()),
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
            impact_direction: Some(t.impact_direction),
            sort_order: t.sort_order,
            is_active: t.is_active,
            system_role: type_roles.get(&t.id).copied(),
        })
        .collect();
    let rules: Vec<RelationshipRule> =
        crud::select_all(conn, RelationshipRules::TABLE, RelationshipRules::COLUMNS, "created_at, id").await?;
    let mut rule_specs: Vec<RelationshipRuleSpec> = rules
        .iter()
        .filter_map(|r| {
            Some(RelationshipRuleSpec {
                relationship_type: type_key.get(&r.relationship_type_id)?.clone(),
                source_class: class_key.get(&r.source_class_id)?.clone(),
                target_class: class_key.get(&r.target_class_id)?.clone(),
            })
        })
        .collect();
    // By keys, not insertion order: a template install or an import writes all
    // rules in one transaction (same created_at), so the id tiebreak is random.
    rule_specs.sort();
    ids.rules = rule_specs
        .iter()
        .map(|r| (r.relationship_type.clone(), r.source_class.clone(), r.target_class.clone()))
        .collect();

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
                        let class_system_role = g.class_id.and_then(|id| class_roles.get(&id).copied());
                        Some(ClassGrantSpec {
                            class,
                            class_system_role,
                            view: g.view,
                            create: g.create,
                            edit: g.edit,
                            delete: g.delete,
                        })
                    })
                    .collect(),
            };
            normalise_profile(&mut spec);
            spec
        })
        .collect();

    let current = ui_data::current(conn, false).await?;
    let mut ui_section =
        UiSettingsSection { settings: ui::parse_stored(&current.settings).with_standard(), logo: None, favicon: None };
    for (kind, content_type, data) in ui_data::all_asset_data(conn).await? {
        let Some(content_type) = ImageType::parse(&content_type) else { continue };
        let asset = AssetData { content_type, data: base64::engine::general_purpose::STANDARD.encode(&data) };
        match AssetKind::parse(&kind) {
            Some(AssetKind::Logo) => ui_section.logo = Some(asset),
            Some(AssetKind::Favicon) => ui_section.favicon = Some(asset),
            None => {}
        }
    }

    let mapping_specs: Vec<ImportMappingSpec> = saved::all_for_config(conn)
        .await?
        .into_iter()
        .map(|m| {
            ids.mappings.insert((m.class_key.clone(), m.name.to_lowercase()), m.id);
            ImportMappingSpec {
                name: m.name,
                description: m.description,
                class_key: m.class_key,
                definition: m.definition,
            }
        })
        .collect();

    let view_specs: Vec<SavedViewSpec> = saved_views::service::shared_for_config(conn)
        .await?
        .into_iter()
        .map(|v| {
            ids.views.insert((v.context.as_str().to_owned(), v.name.to_lowercase()), v.id);
            SavedViewSpec { context: v.context, name: v.name, description: v.description, definition: v.definition }
        })
        .collect();

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
        lookups: Some(LookupSection { lists: list_specs, ..LookupSection::default() }),
        permission_profiles: Some(profile_specs),
        ui_settings: Some(ui_section),
        import_mappings: Some(mapping_specs),
        saved_views: Some(view_specs),
        workflows: Some(workflows::snapshot(conn).await?),
    };
    let views_catalogue = saved_views::resolve::Catalogue::load(conn).await?;
    Ok(Snapshot { file, ids, builtin_profile, views_catalogue })
}

/// GH#186: the permission profiles section is only exported to callers who may
/// read profiles on their own admin API (`profiles.manage` or `users.manage`);
/// for anyone else it is left out of the file. Likewise saved import mappings
/// need `cis.import`, and only those of classes the caller can view are
/// written (SHAA-714 §6.2, D9). Shared saved views need `views.share`; a view
/// is written without the class keys the caller may not view, and left out
/// when they may view none of its classes (SHAA-578 §3.2, §4).
pub async fn export(pool: &PgPool, ctx: &RequestContext) -> Result<ConfigFile, AppError> {
    // One snapshot: REPEATABLE READ so every section comes from the same moment.
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut *tx).await?;
    let Snapshot { mut file, ids, .. } = snapshot(&mut tx).await?;
    let catalogue = saved_views::resolve::Catalogue::load(&mut tx).await?;
    let model = Model::load(&mut tx).await?;
    tx.commit().await?;
    let reads_profiles =
        ctx.require(GlobalPermission::ProfilesManage).or_else(|_| ctx.require(GlobalPermission::UsersManage));
    if reads_profiles.is_err() {
        file.permission_profiles = None;
    }
    if ctx.require(GlobalPermission::CisImport).is_err() {
        file.import_mappings = None;
    } else if let Some(mappings) = file.import_mappings.as_mut() {
        mappings
            .retain(|m| ids.classes.get(&m.class_key).is_some_and(|c| ctx.require_class(*c, ClassOp::View).is_ok()));
    }
    if ctx.require(GlobalPermission::ViewsShare).is_err() {
        file.saved_views = None;
    } else if let Some(views) = file.saved_views.take() {
        file.saved_views = Some(
            views
                .into_iter()
                .filter_map(|v| {
                    let definition = saved_views::service::exportable(ctx, &catalogue, &v.definition)?;
                    Some(SavedViewSpec { definition, ..v })
                })
                .collect(),
        );
    }

    if ctx.require(GlobalPermission::WorkflowsManage).is_err() {
        file.workflows = None;
    } else if let Some(list) = file.workflows.as_mut() {
        list.retain(|w| workflows::exportable(ctx, &model, &ids, w));
    }

    // GH#407: one `export` row per download, in its own transaction (the
    // snapshot is read-only). Which sections left and how many mappings,
    // never the content.
    let sections: Vec<&str> = [
        ("dataModel", file.data_model.is_some()),
        ("lookups", file.lookups.is_some()),
        ("permissionProfiles", file.permission_profiles.is_some()),
        ("uiSettings", file.ui_settings.is_some()),
        ("importMappings", file.import_mappings.is_some()),
        ("savedViews", file.saved_views.is_some()),
        ("workflows", file.workflows.is_some()),
    ]
    .into_iter()
    .filter_map(|(name, present)| present.then_some(name))
    .collect();
    let entry = crud::AuditEntry {
        action: crud::AuditAction::Export,
        entity_type: "config",
        entity_id: Uuid::nil(),
        old_value: None,
        new_value: Some(serde_json::json!({
            "kind": "config",
            "format": "json",
            "formatVersion": file.format_version,
            "sections": sections,
            "profilesIncluded": file.permission_profiles.is_some(),
            "mappingCount": file.import_mappings.as_ref().map_or(0, Vec::len),
            "viewCount": file.saved_views.as_ref().map_or(0, Vec::len),
            "workflowCount": file.workflows.as_ref().map_or(0, Vec::len),
        })),
    };
    let mut tx = pool.begin().await?;
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
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

fn validate(
    file: &ConfigFile,
    snap: &Snapshot,
    ctx: &RequestContext,
    mut e: Vec<FieldError>,
    warnings: &mut Vec<ImportWarning>,
) -> Result<Decoded, AppError> {
    let empty_dm = DataModelSection::default();
    let empty_lk = LookupSection::default();
    let dm = file.data_model.as_ref().unwrap_or(&empty_dm);
    let lk = file.lookups.as_ref().unwrap_or(&empty_lk);
    let ids = &snap.ids;

    let classes = known(dm.classes.iter().map(|c| c.key.as_str()), ids.classes.keys());
    let lists = known(lk.lists.iter().map(|l| l.key.as_str()), ids.lists.keys());
    let types = known(dm.relationship_types.iter().map(|t| t.key.as_str()), ids.types.keys());
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
    // Lookup lists: parent lists exist, no cycles; parent values are values of the parent list
    for (i, l) in lk.lists.iter().enumerate() {
        match &l.parent {
            Some(p) if p == &l.key => {
                problem(&mut e, format!("lookups.lists.{i}.parent"), "cycle", "A list cannot be its own parent")
            }
            Some(p) if !lists.contains(p) => problem(
                &mut e,
                format!("lookups.lists.{i}.parent"),
                "not_found",
                format!("Lookup list \"{p}\" does not exist"),
            ),
            _ => {}
        }
        for (j, v) in l.values.iter().enumerate() {
            let path = format!("lookups.lists.{i}.values.{j}.parent");
            match (&l.parent, &v.parent) {
                (None, Some(_)) => {
                    problem(&mut e, path, "custom", "Only allowed for values of a list with a parent list")
                }
                (Some(pl), Some(k)) if !values.contains(&(pl.clone(), k.clone())) => {
                    problem(&mut e, path, "not_found", format!("List \"{pl}\" has no value \"{k}\""))
                }
                _ => {}
            }
        }
    }
    let (_, cyclic) = parents_first(lk.lists.iter().collect(), |l| &l.key, |l| l.parent.as_deref());
    for l in cyclic {
        let i = lk.lists.iter().position(|x| x.key == l.key).unwrap_or_default();
        problem(
            &mut e,
            format!("lookups.lists.{i}.parent"),
            "cycle",
            "The lookup lists in the file depend on each other in a cycle",
        );
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
        if !is(AttributeDataType::Lookup) && a.parent_attribute.is_some() {
            problem(&mut e, format!("{p}.parentAttribute"), "custom", "Only allowed for lookup attributes");
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
        if !t.is_directional && t.impact_direction.is_some_and(|d| !d.allowed_without_direction()) {
            problem(
                &mut e,
                format!("dataModel.relationshipTypes.{i}.impactDirection"),
                "invalid",
                "A non-directional type has no source or target side: impact can only flow both ways or not at all",
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

    // Saved import mappings
    if let Some(mappings) = &file.import_mappings {
        validate_mappings(mappings, file, snap, ctx, &mut e, warnings);
    }

    // Shared saved views
    if let Some(views) = &file.saved_views {
        validate_views(views, file, snap, ctx, &mut e, warnings);
    }

    // Workflows: their types, the profiles granted their transitions, and the profiles, groups and users that
    // approve their steps
    if let Some(list) = &file.workflows {
        let profiles: HashSet<String> = file
            .permission_profiles
            .iter()
            .flatten()
            .map(|p| p.name.to_lowercase())
            .chain(ids.profiles.keys().cloned())
            .collect();
        workflows::validate(list, &classes, &profiles, ids, &mut e);
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

/// §6.2: a mapping's class, attribute and relationship-type keys that do not
/// exist (here or in the file) are warnings, and the mapping is still saved:
/// it applies once they exist. A mapping of an existing class the caller
/// cannot view is skipped, so the import never writes one they could not
/// read (D9). Everything the saved-mapping API refuses is refused here too.
fn validate_mappings(
    mappings: &[ImportMappingSpec],
    file: &ConfigFile,
    snap: &Snapshot,
    ctx: &RequestContext,
    e: &mut Vec<FieldError>,
    warnings: &mut Vec<ImportWarning>,
) {
    let (file_dm, here_dm) =
        (file.data_model.clone().unwrap_or_default(), snap.file.data_model.clone().unwrap_or_default());
    let parents: HashMap<&str, Option<&str>> =
        here_dm.classes.iter().chain(file_dm.classes.iter()).map(|c| (c.key.as_str(), c.parent.as_deref())).collect();
    let attrs: HashSet<(&str, &str)> = here_dm
        .attributes
        .iter()
        .chain(file_dm.attributes.iter())
        .map(|a| (a.class.as_str(), a.key.as_str()))
        .collect();
    let types: HashSet<&str> =
        here_dm.relationship_types.iter().chain(file_dm.relationship_types.iter()).map(|t| t.key.as_str()).collect();
    let has_attribute = |class: &str, key: &str| {
        let mut at = Some(class);
        let mut hops = 0;
        while let Some(c) = at {
            if attrs.contains(&(c, key)) {
                return true;
            }
            at = parents.get(c).copied().flatten();
            hops += 1;
            if hops > parents.len() {
                break;
            }
        }
        false
    };

    let mut seen = HashSet::new();
    let mut creates = 0i64;
    for (i, m) in mappings.iter().enumerate() {
        let p = format!("importMappings.{i}");
        if m.name.is_empty() {
            problem(e, format!("{p}.name"), "required", "Required");
        } else if m.name.chars().count() > 100 {
            problem(e, format!("{p}.name"), "too_long", "At most 100 characters");
        }
        if m.description.as_deref().is_some_and(|d| d.chars().count() > 500) {
            problem(e, format!("{p}.description"), "too_long", "At most 500 characters");
        }
        let merge_key = (m.class_key.clone(), m.name.to_lowercase());
        if !seen.insert(merge_key.clone()) {
            problem(
                e,
                format!("{p}.name"),
                "duplicate",
                format!("\"{}\" appears more than once for this class", m.name),
            );
        }
        e.extend(m.definition.check(&format!("{p}.definition")).into_iter().map(|mut x| {
            x.location = FieldLocation::Body;
            x
        }));
        if let Some(class_id) = snap.ids.classes.get(&m.class_key)
            && ctx.require_class(*class_id, ClassOp::View).is_err()
        {
            warnings.push(ImportWarning {
                path: p,
                message: format!("You cannot view class \"{}\"; the mapping \"{}\" was skipped", m.class_key, m.name),
            });
            continue;
        }
        if !parents.contains_key(m.class_key.as_str()) {
            warnings.push(ImportWarning {
                path: format!("{p}.classKey"),
                message: format!("Class \"{}\" does not exist; the mapping applies once it does", m.class_key),
            });
        }
        for (j, c) in m.definition.columns.iter().enumerate() {
            let missing = match &c.target {
                ColumnTarget::Attribute { key, .. } if !has_attribute(&m.class_key, key) => {
                    Some(format!("Field \"{key}\" does not exist on class \"{}\"", m.class_key))
                }
                ColumnTarget::Relationship { type_key, .. } if !types.contains(type_key.as_str()) => {
                    Some(format!("Relationship type \"{type_key}\" does not exist"))
                }
                _ => None,
            };
            if let Some(message) = missing {
                warnings.push(ImportWarning { path: format!("{p}.definition.columns.{j}.target"), message });
            }
        }
        if !snap.ids.mappings.contains_key(&merge_key) {
            creates += 1;
        }
    }
    if snap.ids.mappings.len() as i64 + creates > saved::MAX_SAVED {
        problem(
            e,
            "importMappings".into(),
            "limit_reached",
            format!("An instance holds at most {} saved mappings; this file would exceed that", saved::MAX_SAVED),
        );
    }
}

/// Whether a shared view in the file refers to a class here that the caller
/// may not view. Such a view is skipped, so an import never writes a view the
/// importer could not save through the API (SHAA-578 §3.1).
/// GH#475/#476: a stored shared view the importer cannot see (every one of its
/// classes is hidden from them) is answered `404` by the API, so the import
/// neither shows nor rewrites it: the file's view of that name is skipped. So
/// is one with only some classes hidden, which the API refuses to change (GH#508).
fn existing_view_hidden(
    current: &[SavedViewSpec],
    key: &(String, String),
    cat: &saved_views::resolve::Catalogue,
    viewer: &saved_views::resolve::Viewer,
) -> bool {
    current
        .iter()
        .find(|v| v.context.as_str() == key.0 && v.name.to_lowercase() == key.1)
        .is_some_and(|v| !viewer.sees_all(cat, &v.definition))
}

fn hidden_view_warning(path: String, name: &str) -> ImportWarning {
    ImportWarning {
        path,
        message: format!(
            "A shared view named \"{name}\" exists and includes classes you cannot view; this view was skipped"
        ),
    }
}

fn view_names_hidden_class(v: &SavedViewSpec, snap: &Snapshot, ctx: &RequestContext) -> bool {
    v.definition
        .class_keys
        .iter()
        .any(|k| snap.ids.classes.get(k).is_some_and(|c| ctx.require_class(*c, ClassOp::View).is_err()))
}

/// SHAA-578 §4: a view's class, attribute and lookup keys that do not exist
/// (here or in the file) are warnings, and the view is still saved, as for UI
/// settings: resolution reports them until they exist. Everything the
/// saved-view API refuses about the definition's shape is refused here too.
fn validate_views(
    views: &[SavedViewSpec],
    file: &ConfigFile,
    snap: &Snapshot,
    ctx: &RequestContext,
    e: &mut Vec<FieldError>,
    warnings: &mut Vec<ImportWarning>,
) {
    let (file_dm, here_dm) =
        (file.data_model.clone().unwrap_or_default(), snap.file.data_model.clone().unwrap_or_default());
    let parents: HashMap<&str, Option<&str>> =
        here_dm.classes.iter().chain(file_dm.classes.iter()).map(|c| (c.key.as_str(), c.parent.as_deref())).collect();
    let attrs: HashSet<(&str, &str)> = here_dm
        .attributes
        .iter()
        .chain(file_dm.attributes.iter())
        .map(|a| (a.class.as_str(), a.key.as_str()))
        .collect();
    let has_attribute = |class: &str, key: &str| {
        let mut at = Some(class);
        let mut hops = 0;
        while let Some(c) = at {
            if attrs.contains(&(c, key)) {
                return true;
            }
            at = parents.get(c).copied().flatten();
            hops += 1;
            if hops > parents.len() {
                break;
            }
        }
        false
    };
    let (file_lk, here_lk) = (file.lookups.clone().unwrap_or_default(), snap.file.lookups.clone().unwrap_or_default());
    let values: HashMap<&str, HashSet<&str>> =
        here_lk.lists.iter().chain(file_lk.lists.iter()).fold(HashMap::new(), |mut m, l| {
            m.entry(l.key.as_str()).or_default().extend(l.values.iter().map(|v| v.key.as_str()));
            m
        });

    let viewer = saved_views::resolve::Viewer::of(ctx);
    let stored = snap.file.saved_views.as_deref().unwrap_or_default();
    let mut seen = HashSet::new();
    let mut creates = 0i64;
    for (i, v) in views.iter().enumerate() {
        let p = format!("savedViews.{i}");
        if v.name.is_empty() {
            problem(e, format!("{p}.name"), "required", "Required");
        }
        let merge_key = (v.context.as_str().to_owned(), v.name.to_lowercase());
        if !seen.insert(merge_key.clone()) {
            problem(
                e,
                format!("{p}.name"),
                "duplicate",
                format!("\"{}\" appears more than once for this context", v.name),
            );
        }
        if view_names_hidden_class(v, snap, ctx) {
            warnings.push(ImportWarning {
                path: p,
                message: format!("The shared view \"{}\" refers to a class you cannot view and was skipped", v.name),
            });
            continue;
        }
        if existing_view_hidden(stored, &merge_key, &snap.views_catalogue, &viewer) {
            warnings.push(hidden_view_warning(p, &v.name));
            continue;
        }
        let d = &v.definition;
        for (j, k) in d.class_keys.iter().enumerate() {
            if !parents.contains_key(k.as_str()) {
                warnings.push(ImportWarning {
                    path: format!("{p}.definition.classKeys.{j}"),
                    message: format!("Class \"{k}\" does not exist; the view leaves it out until it does"),
                });
            }
        }
        let mut fields: Vec<(String, &str)> =
            d.columns.iter().enumerate().map(|(j, c)| (format!("columns.{j}"), c.as_str())).collect();
        if let Some(s) = &d.sort {
            fields.push(("sort.field".into(), s.field.as_str()));
        }
        for (path, field) in fields {
            let Some(key) = field.strip_prefix("attributes.") else { continue };
            if let Some(c) = d.class_keys.iter().find(|c| parents.contains_key(c.as_str()) && !has_attribute(c, key)) {
                warnings.push(ImportWarning {
                    path: format!("{p}.definition.{path}"),
                    message: format!("Field \"{key}\" does not exist on class \"{c}\"; the view leaves it out"),
                });
            }
        }
        for (list, keys) in &d.filters.lookups {
            let path = format!("{p}.definition.filters.lookups.{list}");
            match values.get(list.as_str()) {
                None => warnings.push(ImportWarning {
                    path,
                    message: format!("Lookup list \"{list}\" does not exist; the view is unavailable until it does"),
                }),
                Some(known) => {
                    for (j, k) in keys.iter().enumerate() {
                        if !known.contains(k.as_str()) {
                            warnings.push(ImportWarning {
                                path: format!("{path}.{j}"),
                                message: format!("\"{k}\" is not a value of lookup list \"{list}\""),
                            });
                        }
                    }
                }
            }
        }
        if !snap.ids.views.contains_key(&merge_key) {
            creates += 1;
        }
    }
    let max = saved_views::service::MAX_SHARED;
    if snap.ids.views.len() as i64 + creates > max {
        problem(
            e,
            "savedViews".into(),
            "limit_reached",
            format!("An instance holds at most {max} shared views; this file would exceed that"),
        );
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

    /// Adds a changed field to the row's entry; a row counted as unchanged becomes updated.
    fn amend(&mut self, section: &str, key: String, change: FieldChange) {
        if let Some(c) = self.changes.iter_mut().find(|c| c.section == section && c.key == key) {
            if c.action == ChangeAction::Update {
                c.fields.push(change);
            }
            return;
        }
        if let Some(s) = self.summary.iter_mut().find(|s| s.section == section) {
            s.unchanged -= 1;
        }
        self.record(section, key, Some(ChangeAction::Update), vec![change]);
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
    let mut snap = snapshot(conn).await?;
    workflows::principals(conn, file, &mut snap.ids).await?;
    let mut warnings = Vec::new();
    let mut file = file.clone();
    let mut legacy_problems = legacy::fold(&mut file, snap.file.lookups.as_ref(), &mut warnings);
    legacy_problems.extend(system_roles::match_by_role(&mut file, &snap.file, &mut warnings));
    if file.format_version < 3 {
        file = keep_current_parents(&file, &snap.file);
    }
    let file = &archive_superseded_fields(keep_current_settings(file, &snap.file), &snap.file, &mut warnings);
    let decoded = validate(file, &snap, ctx, legacy_problems, &mut warnings)?;
    let Snapshot { file: current, ids, views_catalogue, .. } = snap;
    let cur_dm = current.data_model.unwrap_or_default();
    let cur_lk = current.lookups.unwrap_or_default();
    let cur_profiles = current.permission_profiles.unwrap_or_default();
    let mut im = Importer { conn, ctx, ids, summary: Vec::new(), changes: Vec::new() };

    // ---- lookups ----
    if let Some(lk) = &file.lookups {
        let keys: HashSet<String> = lk.lists.iter().map(|l| l.key.clone()).collect();
        im.section("lookupLists", not_in_file(im.ids.lists.keys(), &keys));
        let value_keys: HashSet<String> =
            lk.lists.iter().flat_map(|l| l.values.iter().map(move |v| format!("{}.{}", l.key, v.key))).collect();
        let here: Vec<String> =
            im.ids.values.keys().filter(|(l, _)| keys.contains(l)).map(|(l, v)| format!("{l}.{v}")).collect();
        im.section("lookupListValues", not_in_file(here.iter(), &value_keys));
        let old: HashMap<&str, &LookupListSpec> = cur_lk.lists.iter().map(|l| (l.key.as_str(), l)).collect();
        let indexed: Vec<(usize, &LookupListSpec)> = lk.lists.iter().enumerate().collect();
        let (ordered, _) = parents_first(indexed, |(_, l)| &l.key, |(_, l)| l.parent.as_deref());
        for (i, l) in ordered {
            let bare = |l: &LookupListSpec| LookupListSpec { values: Vec::new(), ..l.clone() };
            let new_bare = bare(l);
            let old_bare = old.get(l.key.as_str()).map(|o| bare(o));
            let existing = old_bare.as_ref().map(|o| (im.ids.lists[&l.key], o));
            // A new parent list unassigns the values' parent values (LookupLists::after_write).
            let parent_changed = old_bare.as_ref().is_some_and(|o| o.parent != l.parent);
            let mut c = ColumnSet::default();
            c.opt("name", Some(l.name.clone()))
                .opt("description", Some(l.description.clone()))
                .opt("sort_order", Some(l.sort_order))
                .opt("is_active", Some(l.is_active))
                .opt("parent_list_id", Some(l.parent.as_ref().map(|p| im.ids.lists[p])));
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
                let unassigned;
                let old_value = match old_values.get(v.key.as_str()) {
                    Some(o) if parent_changed => {
                        unassigned = LookupValueSpec { parent: None, ..(*o).clone() };
                        Some(&unassigned)
                    }
                    o => o.copied(),
                };
                let existing = old_value.map(|o| (im.ids.values[&vkey], o));
                let parent_value_id = match (&l.parent, &v.parent) {
                    (Some(pl), Some(k)) => Some(im.ids.values[&(pl.clone(), k.clone())]),
                    _ => None,
                };
                let mut c = ColumnSet::default();
                c.opt("name", Some(v.name.clone()))
                    .opt("description", Some(v.description.clone()))
                    .opt("color", Some(v.color.clone()))
                    .opt("sort_order", Some(v.sort_order))
                    .opt("is_active", Some(v.is_active))
                    .opt("parent_value_id", Some(parent_value_id));
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
            // A class without an area (version 1 files) stays where it is; one without a
            // title attribute (files from before SHAA-267), or without an owner or
            // end-of-life field (before version 11), keeps its own.
            let with_area;
            let cls = match existing {
                Some((_, o))
                    if cls.area.is_none()
                        || cls.title_attribute.is_none()
                        || cls.owner_attribute.is_none()
                        || cls.end_of_life_attribute.is_none() =>
                {
                    with_area = ClassSpec {
                        area: cls.area.clone().or_else(|| o.area.clone()),
                        title_attribute: cls.title_attribute.clone().or_else(|| o.title_attribute.clone()),
                        owner_attribute: cls.owner_attribute.clone().or_else(|| o.owner_attribute.clone()),
                        end_of_life_attribute: cls
                            .end_of_life_attribute
                            .clone()
                            .or_else(|| o.end_of_life_attribute.clone()),
                        ..cls.clone()
                    };
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
                .opt("is_expected", Some(a.is_expected.unwrap_or_default()))
                .opt("is_identifying", Some(a.is_identifying.unwrap_or_default()))
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
            // Parent fields are set in a second pass, once every field of the file exists.
            let compared =
                AttributeSpec { parent_attribute: existing.and_then(|(_, o)| o.parent_attribute.clone()), ..a.clone() };
            let id =
                im.upsert::<AttributeDefinitions, _>("attributes", &path, key, existing, &compared, create, c).await?;
            im.ids.attributes.insert(akey, id);
        }
        for (i, a) in dm.attributes.iter().enumerate() {
            let id = im.ids.attributes[&(a.class.clone(), a.key.clone())];
            let path = format!("dataModel.attributes.{i}");
            let wanted: Option<Uuid> = match &a.parent_attribute {
                None => None,
                Some(k) => Some(
                    sqlx::query_scalar(
                        "SELECT id FROM ci_attribute_definitions WHERE key = $2 AND ci_class_is_a($1, class_id)",
                    )
                    .bind(im.ids.classes[&a.class])
                    .bind(k)
                    .fetch_optional(&mut *im.conn)
                    .await?
                    .ok_or_else(|| {
                        at(
                            &format!("{path}.parentAttribute"),
                            AppError::new(
                                ErrorCode::ValidationError,
                                format!("Field \"{k}\" is not defined on class \"{}\" or an ancestor", a.class),
                            ),
                        )
                    })?,
                ),
            };
            let (now, now_key): (Option<Uuid>, Option<String>) = sqlx::query_as(
                "SELECT d.parent_attribute_id, p.key FROM ci_attribute_definitions d
                 LEFT JOIN ci_attribute_definitions p ON p.id = d.parent_attribute_id WHERE d.id = $1",
            )
            .bind(id)
            .fetch_one(&mut *im.conn)
            .await?;
            if now == wanted {
                continue;
            }
            let mut c = ColumnSet::default();
            c.opt("parent_attribute_id", Some(wanted));
            simple::update_in::<AttributeDefinitions>(im.conn, im.ctx, id, c).await.map_err(|e| at(&path, e))?;
            let change = FieldChange {
                field: "parentAttribute".into(),
                from: now_key.map_or(Value::Null, Value::String),
                to: a.parent_attribute.clone().map_or(Value::Null, Value::String),
            };
            im.amend("attributes", format!("{}.{}", a.class, a.key), change);
        }

        // Title, owner and end-of-life fields, now that the fields exist (a class may name an inherited one).
        for (i, cls) in dm.classes.iter().enumerate() {
            for (setting, file_field, column) in [
                (&cls.title_attribute, "titleAttribute", "title_attribute_id"),
                (&cls.owner_attribute, "ownerAttribute", "owner_attribute_id"),
                (&cls.end_of_life_attribute, "endOfLifeAttribute", "end_of_life_attribute_id"),
            ] {
                let Some(title) = setting else { continue };
                let class_id = im.ids.classes[&cls.key];
                let path = format!("dataModel.classes.{i}.{file_field}");
                let wanted: Option<Uuid> = match title {
                    None => None,
                    Some(key) => Some(
                        sqlx::query_scalar(
                            "SELECT d.id FROM cmdb.ci_class_lineage($1) l
                         JOIN cmdb.ci_attribute_definitions d ON d.class_id = l.class_id
                         WHERE d.key = $2 ORDER BY l.depth LIMIT 1",
                        )
                        .bind(class_id)
                        .bind(key)
                        .fetch_optional(&mut *im.conn)
                        .await?
                        .ok_or_else(|| {
                            at(
                                &path,
                                AppError::field(
                                    file_field,
                                    format!("Class \"{}\" has no field \"{key}\" (own or inherited)", cls.key),
                                    "not_found",
                                ),
                            )
                        })?,
                    ),
                };
                let current: Option<Uuid> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                    "SELECT {column} FROM cmdb.ci_classes WHERE id = $1"
                )))
                .bind(class_id)
                .fetch_one(&mut *im.conn)
                .await?;
                if current != wanted {
                    let mut c = ColumnSet::default();
                    c.opt(column, Some(wanted));
                    simple::update_in::<CiClasses>(im.conn, im.ctx, class_id, c).await.map_err(|e| at(&path, e))?;
                }
            }
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
                .opt("impact_direction", t.impact_direction.map(|d| d.as_str().to_owned()))
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
                        false,
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
                        None,
                    )
                    .await
                    .map_err(|e| at(&path, e))?;
                    im.record("permissionProfiles", spec.name.clone(), Some(ChangeAction::Update), fields);
                }
            }
        }
    }

    // ---- workflows (after the data model, lookups and profiles they refer to) ----
    if let Some(list) = &file.workflows {
        im.workflows(list, current.workflows.as_deref().unwrap_or_default(), &mut warnings).await?;
    }

    // ---- saved import mappings (merged by class key and name, never deleted) ----
    if let Some(list) = &file.import_mappings {
        let names: HashSet<String> =
            list.iter().map(|m| format!("{}.{}", m.class_key, m.name.to_lowercase())).collect();
        let here: Vec<String> = im.ids.mappings.keys().map(|(c, n)| format!("{c}.{n}")).collect();
        im.section("importMappings", not_in_file(here.iter(), &names));
        let old: HashMap<(String, String), &ImportMappingSpec> = current
            .import_mappings
            .iter()
            .flatten()
            .map(|m| ((m.class_key.clone(), m.name.to_lowercase()), m))
            .collect();
        for (i, m) in list.iter().enumerate() {
            let key = (m.class_key.clone(), m.name.to_lowercase());
            if let Some(c) = im.ids.classes.get(&m.class_key)
                && im.ctx.require_class(*c, ClassOp::View).is_err()
            {
                continue; // warned about in validate
            }
            let path = format!("importMappings.{i}");
            let label = format!("{}.{}", m.class_key, m.name);
            let existing = im.ids.mappings.get(&key).copied();
            let fields = match old.get(&key) {
                // The name keeps its current spelling; only description and definition are replaced.
                Some(before) => diff(*before, &ImportMappingSpec { name: before.name.clone(), ..m.clone() }),
                None => Vec::new(),
            };
            if existing.is_some() && fields.is_empty() {
                im.record("importMappings", label, None, Vec::new());
                continue;
            }
            let id = saved::config_write(
                im.conn,
                im.ctx,
                existing,
                &m.name,
                m.description.as_deref(),
                &m.class_key,
                &m.definition,
            )
            .await
            .map_err(|e| at(&path, e))?;
            im.ids.mappings.insert(key, id);
            let action = if existing.is_some() { ChangeAction::Update } else { ChangeAction::Create };
            im.record("importMappings", label, Some(action), fields);
        }
    }

    // ---- UI settings ----
    let mut ui_settings_issues = Vec::new();
    if let Some(section) = &file.ui_settings {
        im.section("uiSettings", 0);
        let old = current.ui_settings.as_ref().map(|u| u.settings.clone()).unwrap_or_default();
        let settings = section.settings.clone().normalized();
        let changed =
            ui::save_in(im.conn, im.ctx, None, &settings, Some("Imported from a configuration file"), ui::InUse::Keep)
                .await
                .map_err(|e| at("uiSettings.settings", e))?;
        if changed {
            im.record("uiSettings", "settings".into(), Some(ChangeAction::Update), diff(&old, &settings));
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
        ui_settings_issues = document::resolve(&section.settings.clone().returned(), &model).1;
    }

    // ---- shared saved views (merged by context and name, never deleted) ----
    if let Some(list) = &file.saved_views {
        let names: HashSet<String> =
            list.iter().map(|v| format!("{}.{}", v.context.as_str(), v.name.to_lowercase())).collect();
        let here: Vec<String> = im.ids.views.keys().map(|(c, n)| format!("{c}.{n}")).collect();
        im.section("savedViews", not_in_file(here.iter(), &names));
        let old: HashMap<(String, String), &SavedViewSpec> = current
            .saved_views
            .iter()
            .flatten()
            .map(|v| ((v.context.as_str().to_owned(), v.name.to_lowercase()), v))
            .collect();
        let catalogue = saved_views::resolve::Catalogue::load(im.conn).await?;
        let viewer = saved_views::resolve::Viewer::of(im.ctx);
        for (i, v) in list.iter().enumerate() {
            let key = (v.context.as_str().to_owned(), v.name.to_lowercase());
            if v.definition
                .class_keys
                .iter()
                .any(|k| im.ids.classes.get(k).is_some_and(|c| im.ctx.require_class(*c, ClassOp::View).is_err()))
            {
                continue; // warned about in validate
            }
            if existing_view_hidden(current.saved_views.as_deref().unwrap_or_default(), &key, &views_catalogue, &viewer)
            {
                continue; // warned about in validate
            }
            let path = format!("savedViews.{i}");
            let label = format!("{}.{}", v.context.as_str(), v.name);
            let existing = im.ids.views.get(&key).copied();
            let fields = match old.get(&key) {
                // The name keeps its current spelling; the class keys the importer may not view stay,
                // and the diff shows only what the importer may see of either side (GH#475).
                Some(before) => {
                    let definition = saved_views::resolve::merge_hidden(
                        &viewer,
                        &catalogue,
                        &before.definition,
                        v.definition.clone(),
                    );
                    let seen = |d: &saved_views::definition::SavedViewDefinition| viewer.visible_part(&catalogue, d).0;
                    let was = SavedViewSpec { definition: seen(&before.definition), ..(*before).clone() };
                    let now = SavedViewSpec { name: before.name.clone(), definition: seen(&definition), ..v.clone() };
                    diff(&was, &now)
                }
                None => Vec::new(),
            };
            if existing.is_some() && fields.is_empty() {
                im.record("savedViews", label, None, Vec::new());
                continue;
            }
            let id = saved_views::service::config_write(
                im.conn,
                im.ctx,
                existing,
                v.context,
                &v.name,
                v.description.as_deref(),
                &v.definition,
            )
            .await
            .map_err(|e| at(&path, e))?;
            im.ids.views.insert(key, id);
            let action = if existing.is_some() { ChangeAction::Update } else { ChangeAction::Create };
            im.record("savedViews", label, Some(action), fields);
        }
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

/// Files before version 3 have no parent lists, parent values or parent
/// fields: the rows that exist here keep theirs.
fn keep_current_parents(file: &ConfigFile, current: &ConfigFile) -> ConfigFile {
    let mut file = file.clone();
    if let (Some(lk), Some(cur)) = (file.lookups.as_mut(), current.lookups.as_ref()) {
        let lists: HashMap<&str, &LookupListSpec> = cur.lists.iter().map(|l| (l.key.as_str(), l)).collect();
        for l in &mut lk.lists {
            let Some(old) = lists.get(l.key.as_str()) else { continue };
            l.parent = old.parent.clone();
            let values: HashMap<&str, &LookupValueSpec> = old.values.iter().map(|v| (v.key.as_str(), v)).collect();
            for v in &mut l.values {
                v.parent = values.get(v.key.as_str()).and_then(|o| o.parent.clone());
            }
        }
    }
    if let (Some(dm), Some(cur)) = (file.data_model.as_mut(), current.data_model.as_ref()) {
        let attrs: HashMap<(&str, &str), &AttributeSpec> =
            cur.attributes.iter().map(|a| ((a.class.as_str(), a.key.as_str()), a)).collect();
        for a in &mut dm.attributes {
            a.parent_attribute =
                attrs.get(&(a.class.as_str(), a.key.as_str())).and_then(|o| o.parent_attribute.clone());
        }
    }
    file
}

/// What a file leaves out keeps its current value: whether an existing field
/// is expected (files before version 10) or identifying (before version 12),
/// which a new field is not, and the
/// impact direction of an existing relationship type (files before version 4,
/// or a hand-written one), which a new type gets as none. The system role of a list is never
/// imported, so the file's is replaced with the current one.
fn keep_current_settings(mut file: ConfigFile, current: &ConfigFile) -> ConfigFile {
    if let Some(dm) = file.data_model.as_mut() {
        let cur: HashMap<(&str, &str), &AttributeSpec> = current
            .data_model
            .iter()
            .flat_map(|d| d.attributes.iter())
            .map(|a| ((a.class.as_str(), a.key.as_str()), a))
            .collect();
        for a in &mut dm.attributes {
            if a.is_expected.is_none() {
                let old = cur.get(&(a.class.as_str(), a.key.as_str())).and_then(|o| o.is_expected);
                a.is_expected = Some(old.unwrap_or_default());
            }
            if a.is_identifying.is_none() {
                let old = cur.get(&(a.class.as_str(), a.key.as_str())).and_then(|o| o.is_identifying);
                a.is_identifying = Some(old.unwrap_or_default());
            }
        }
        let cur: HashMap<&str, &RelationshipTypeSpec> =
            current.data_model.iter().flat_map(|d| d.relationship_types.iter()).map(|t| (t.key.as_str(), t)).collect();
        for t in &mut dm.relationship_types {
            if t.impact_direction.is_none() {
                t.impact_direction = Some(cur.get(t.key.as_str()).and_then(|o| o.impact_direction).unwrap_or_default());
            }
        }
    }
    if let Some(lk) = file.lookups.as_mut() {
        let cur: HashMap<&str, &LookupListSpec> =
            current.lookups.iter().flat_map(|l| l.lists.iter()).map(|l| (l.key.as_str(), l)).collect();
        for l in &mut lk.lists {
            l.system_role = cur.get(l.key.as_str()).and_then(|o| o.system_role);
        }
    }
    file
}

/// Fields that a core field of every CI replaced, as (class, field): the
/// starter template's Application "criticality" (migration 0036, GH#354).
const SUPERSEDED_FIELDS: &[(&str, &str)] = &[("application", "criticality")];

/// A superseded field that is active in the file is imported archived, so an
/// export from before 0036 does not bring back a second Criticality on every
/// Application. Archived, its column and values stay readable and references
/// to it in the file still resolve. The one exception is a field an
/// administrator restored here: it stays active.
fn archive_superseded_fields(
    mut file: ConfigFile,
    current: &ConfigFile,
    warnings: &mut Vec<ImportWarning>,
) -> ConfigFile {
    let Some(dm) = file.data_model.as_mut() else { return file };
    let active_here: HashSet<(&str, &str)> = current
        .data_model
        .iter()
        .flat_map(|d| d.attributes.iter())
        .filter(|a| a.is_active)
        .map(|a| (a.class.as_str(), a.key.as_str()))
        .collect();
    for (i, a) in dm.attributes.iter_mut().enumerate() {
        let field = (a.class.as_str(), a.key.as_str());
        if a.is_active && SUPERSEDED_FIELDS.contains(&field) && !active_here.contains(&field) {
            a.is_active = false;
            warnings.push(ImportWarning {
                path: format!("dataModel.attributes.{i}.isActive"),
                message: format!(
                    "Field {}.{} is imported archived: every CI has a core Criticality field (criticalityValueId), \
                     which replaces it",
                    a.class, a.key
                ),
            });
        }
    }
    file
}

fn check_format(file: &ConfigFile) -> Result<(), AppError> {
    if file.format != FORMAT || !(1..=FORMAT_VERSION).contains(&file.format_version) {
        return Err(AppError::field(
            "formatVersion",
            format!("This server reads {FORMAT} versions 1 to {FORMAT_VERSION}"),
            "unsupported",
        ));
    }
    let lookups = file.lookups.as_ref().map_or(0, |l| {
        l.statuses.len()
            + l.environments.len()
            + l.locations.len()
            + l.owners.len()
            + l.lists.iter().map(|list| list.values.len()).sum::<usize>()
    });
    let data_model = file.data_model.as_ref().map_or(0, |d| {
        d.areas.len() + d.classes.len() + d.attributes.len() + d.relationship_types.len() + d.relationship_rules.len()
    });
    let sections = [
        ("dataModel", data_model),
        ("lookups", lookups),
        ("permissionProfiles", file.permission_profiles.as_ref().map_or(0, Vec::len)),
        ("importMappings", file.import_mappings.as_ref().map_or(0, Vec::len)),
        ("savedViews", file.saved_views.as_ref().map_or(0, Vec::len)),
        ("workflows", file.workflows.as_ref().map_or(0, Vec::len)),
    ];
    let total: usize = sections.iter().map(|s| s.1).sum();
    if total > MAX_ENTRIES {
        // Reported at the largest section, the one to split.
        let (field, _) = sections.iter().max_by_key(|s| s.1).expect("six sections");
        let held: Vec<String> =
            sections.iter().filter(|s| s.1 > 0).map(|(section, n)| format!("{section}: {n}")).collect();
        return Err(AppError::field(
            *field,
            format!(
                "The file holds {total} entries ({}); one file may hold at most {MAX_ENTRIES}. Split it into \
                 several files and import them one after the other",
                held.join(", ")
            ),
            "too_big",
        ));
    }
    Ok(())
}

/// `config.export_import` lets a file in, not past the permission each section
/// needs on its own admin API: the data model and lookups (which run DDL) need
/// `datamodel.manage`, UI settings need `customization.manage`, permission
/// profiles need `profiles.manage`, saved import mappings `cis.import`, shared saved views `views.share`. Checked
/// for dry runs too, before anything
/// touches the database. Each profile is still bounded by what the importing
/// user holds.
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
    if file.permission_profiles.as_ref().is_some_and(|p| !p.is_empty()) {
        ctx.require(GlobalPermission::ProfilesManage)?;
    }
    if file.import_mappings.as_ref().is_some_and(|m| !m.is_empty()) {
        ctx.require(GlobalPermission::CisImport)?;
    }
    if file.saved_views.as_ref().is_some_and(|v| !v.is_empty()) {
        ctx.require(GlobalPermission::ViewsShare)?;
    }
    if file.workflows.as_ref().is_some_and(|w| !w.is_empty()) {
        ctx.require(GlobalPermission::WorkflowsManage)?;
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
    // Audit rows lock the audit chain head until commit, which would stall
    // every sign-in for the whole import (GH#500): a dry run writes none, and
    // an apply writes them all at the end. Boxed: the import future is too
    // large to sit on a test thread's stack inside the audit scope.
    let run = Box::pin(engine::collect_previews(run(&mut tx, ctx, file, mode)));
    let ((result, schema_changes), audit) = match mode {
        ImportMode::DryRun => (crud::discard_audit(run).await, None),
        ImportMode::Apply => {
            let (out, audit) = crud::hold_audit(run).await;
            (out, Some(audit))
        }
    };
    let mut result = result?;
    result.schema_changes = schema_changes;
    match audit {
        None => tx.rollback().await?,
        Some(audit) => {
            audit.write(&mut tx).await?;
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
                "Data model (classes, attributes, relationship types and rules), lookup lists and their values, \
                 permission profiles (not the built-in one), UI settings including the logo and favicon, and \
                 saved import mappings, shared saved views and workflows (each workflow's current published version \
                 with its settings and grants; drafts, retired versions and workflow instances are never part of a \
                 file). Never contains users, user groups, passwords, sessions, \
                 CIs, relationships, business service members or owners, import jobs, the import switch, personal \
                 saved views or anyone's default view. \
                 Everything refers to everything else by key, so the file imports into another install. Answers \
                 with `Content-Disposition: attachment`. The `permissionProfiles` key is only present when the \
                 caller also holds `profiles.manage` or `users.manage` (the permissions that read profiles on \
                 `/api/v1/admin/profiles`); for other callers it is left out, and importing that file leaves \
                 the target's profiles untouched. Likewise `importMappings` is only present when the caller holds \
                 `cis.import`, and holds only the mappings of classes the caller can view; `savedViews` only when \
                 the caller holds `views.share`, without the class keys the caller may not view and without views \
                 whose classes they may view none of; `workflows` only when the caller holds `workflows.manage`, \
                 so a file exported without it carries no workflows and is not a full copy of the configuration. \
                 Every export is recorded in the audit log as one `export` \
                 entry (entity type `config`) naming the sections included and the number of import mappings and \
                 saved views and the number of workflows, never their content, so the request must send X-CSRF-Token as on a write.",
            )
            .requires(GlobalPermission::ConfigExportImport)
            .session_only()
            .csrf_on_read()
            .handle(|api, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| async move {
                let file = export(&api.pool, &api.ctx).await?;
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
                 returning the diff; `mode=apply` does the same and commits. Rows are matched by key (profiles by \
                 name) and created or updated; a class or relationship type with a `systemRole` (the built-in \
                 business service class and membership type, version 5) is matched to this install's class or type of \
                 that role whatever its key, keeps its key and area here, and is reported as a \"Matched by role\" \
                 warning; a grant's `classSystemRole` resolves the same way. An import never sets or clears a role; nothing is deleted, so data missing from the file \
                 is kept (counted as `notInFile`). The `uiSettings` section replaces the settings (as a new version) \
                 and the logo and favicon. All sections are optional. Problems in the file are reported together as \
                 400 VALIDATION_ERROR with paths into the file; a change the data model does not allow (e.g. making \
                 an attribute required while CIs lack a value) fails with the same error the admin API gives, with \
                 the file path prefixed. A file holds at most 25,000 entries in all: data-model entries, lookup values \
                 (former tables included), permission profiles, saved import mappings, saved views and workflows (400 \
                 VALIDATION_ERROR with code too_big at the largest section otherwise, dry run included): split a \
                 larger configuration across several files. A non-empty `dataModel` or `lookups` section also requires \
                 `datamodel.manage`, a `uiSettings` section `customization.manage`, and a non-empty \
                 `permissionProfiles` section `profiles.manage`, and a non-empty `importMappings` section \
                 `cis.import` (403 otherwise, dry run included). Saved import mappings are matched by class key and \
                 name (case-insensitive); an existing one gets the file's description and definition. Keys the \
                 target lacks are warnings, and mappings of classes the caller cannot view are skipped. A non-empty \
                 `savedViews` section (version 6) needs `views.share`; shared views are matched by context and name \
                 (case-insensitive), an existing one gets the file's description and definition (keeping the class \
                 keys the importer may not view), nothing is deleted, keys the target lacks are warnings, and a view \
                 naming a class the importer cannot view is skipped. A non-empty `workflows` section (version 8) \
                 needs `workflows.manage` (403 otherwise, dry run included). Workflows are matched by key: a new \
                 key creates the workflow and publishes v1; an existing one gets the file's settings and grants, and \
                 a new published version only when the file's graph differs from its current one (instances keep \
                 their version, and no published version is ever changed). A changed graph while the workflow has an \
                 unpublished draft here fails with 409 CONFLICT at `workflows.N.graph` (publish or delete the draft \
                 first). A graph the publish lint refuses is reported at `workflows.N.graph…`, and a grant naming a \
                 transition in no version of the workflow (the file's graph included) at \
                 `workflows.N.grants.M.transition` (code `unknown_transition`). Profiles cannot \
                 grant more than the importing user holds (403). Every applied change is audited. Files of earlier \
                 versions (0.1.0-rc.1) may carry `lookups.statuses`, `environments`, `locations` and `owners` (the former \
                 tables): they are imported as the lookup lists `status`, `environment`, `location` and `owner`, \
                 as migration 0016 converts those tables, or skipped when the file's lists already hold them; \
                 either way a warning names the section.",
            )
            .requires(GlobalPermission::ConfigExportImport)
            .recent_reauthentication()
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
    use crate::auth::permissions::Permissions;
    use crate::auth::{Credential, Principal};
    use crate::db::scratch;

    async fn user_ctx(pool: &PgPool, username: &str, global: &[GlobalPermission]) -> RequestContext {
        user_ctx_with(pool, username, global, Default::default()).await
    }

    async fn user_ctx_with(
        pool: &PgPool,
        username: &str,
        global: &[GlobalPermission],
        all_classes: crate::auth::permissions::ClassRights,
    ) -> RequestContext {
        let user_id: Uuid = sqlx::query_scalar(
            "INSERT INTO users (username, display_name, password_hash) VALUES ($1, $1, '$argon2id$v=19$test')
             RETURNING id",
        )
        .bind(username)
        .fetch_one(pool)
        .await
        .unwrap();
        let permissions = Permissions { global: global.iter().copied().collect(), all_classes, ..Default::default() };
        let principal = Principal {
            user_id,
            username: username.into(),
            credential: Credential::Session {
                id: Uuid::new_v4(),
                csrf_token: String::new(),
                mfa_enrolment_required: false,
                email_required: false,
                recently_confirmed: true,
            },
            permissions,
        };
        RequestContext::user(std::sync::Arc::new(principal), "test".into())
    }

    async fn schema_change_count(pool: &PgPool) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM cmdb.schema_changes").fetch_one(pool).await.unwrap()
    }

    /// GH#59, GH#80: `config.export_import` alone must not run data-model DDL (or
    /// touch lookups, UI settings or permission profiles) through the import.
    #[tokio::test]
    async fn import_sections_need_their_own_permission() {
        let Some(src) = scratch::database("import_sections_src").await else { return };
        let Some(dst) = scratch::database("import_sections_dst").await else { return };
        crate::seed::install_template(&src.pool, "it_infrastructure").await.unwrap();
        let file = export(&src.pool, &RequestContext::system("test", "test")).await.unwrap();
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

        // GH#80: profiles need `profiles.manage`; an empty section needs nothing extra.
        let no_profiles = ConfigFile { data_model: None, lookups: None, ui_settings: None, ..file.clone() };
        assert_eq!(no_profiles.permission_profiles.as_deref().map(<[_]>::len), Some(0));
        import(&dst.pool, &only_import, &no_profiles, ImportMode::DryRun).await.unwrap();
        let empty_profile = ProfileSpec {
            name: "Service Desk".into(),
            description: None,
            global_permissions: Vec::new(),
            class_permissions: Vec::new(),
        };
        let profiles_only = ConfigFile { permission_profiles: Some(vec![empty_profile]), ..no_profiles };
        for mode in [ImportMode::DryRun, ImportMode::Apply] {
            let err = import(&dst.pool, &only_import, &profiles_only, mode).await.unwrap_err();
            assert_eq!(err.code, ErrorCode::Forbidden, "{mode:?}: {err}");
            assert!(err.message.contains("profiles.manage"), "{err}");
        }
        let profile_count = || async {
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM permission_profiles WHERE name = 'Service Desk'")
                .fetch_one(&dst.pool)
                .await
                .unwrap()
        };
        assert_eq!(profile_count().await, 0);
        let profile_admin = user_ctx(
            &dst.pool,
            "profile_admin",
            &[GlobalPermission::ConfigExportImport, GlobalPermission::ProfilesManage],
        )
        .await;
        assert!(import(&dst.pool, &profile_admin, &profiles_only, ImportMode::Apply).await.unwrap().applied);
        assert_eq!(profile_count().await, 1);

        // Making a field of an existing type required is checked against its
        // stored values: the built-in types' tables exist after migrate.
        let full = user_ctx_with(
            &dst.pool,
            "configurator",
            &[
                GlobalPermission::ConfigExportImport,
                GlobalPermission::DatamodelManage,
                GlobalPermission::CustomizationManage,
            ],
            crate::auth::permissions::ClassRights { view: true, ..Default::default() },
        )
        .await;
        let res = import(&dst.pool, &full, &file, ImportMode::Apply).await.unwrap();
        assert!(res.applied);
        assert!(schema_change_count(&dst.pool).await > before);

        src.drop().await;
        dst.drop().await;
    }

    /// GH#186: `config.export_import` alone must not read permission profiles
    /// through the export; `profiles.manage` or `users.manage` (what
    /// `/api/v1/admin/profiles` asks for) brings them back.
    #[tokio::test]
    async fn export_profiles_need_profile_read_permission() {
        let Some(db) = scratch::database("export_profiles_read").await else { return };
        crate::seed::seed_system_rows(&db.pool).await.unwrap();
        let profile = ProfileSpec {
            name: "Service Desk".into(),
            description: None,
            global_permissions: Vec::new(),
            class_permissions: Vec::new(),
        };
        let file = ConfigFile {
            format: FORMAT.into(),
            format_version: FORMAT_VERSION,
            exported_at: None,
            app_version: None,
            data_model: None,
            lookups: None,
            permission_profiles: Some(vec![profile]),
            ui_settings: None,
            import_mappings: None,
            saved_views: None,
            workflows: None,
        };
        import(&db.pool, &RequestContext::system("test", "test"), &file, ImportMode::Apply).await.unwrap();

        let only_export = user_ctx(&db.pool, "exporter", &[GlobalPermission::ConfigExportImport]).await;
        let exported = export(&db.pool, &only_export).await.unwrap();
        assert_eq!(exported.permission_profiles, None);
        assert!(exported.data_model.is_some() && exported.lookups.is_some() && exported.ui_settings.is_some());
        let json = serde_json::to_value(&exported).unwrap();
        assert!(json.get("permissionProfiles").is_none(), "{json}");
        assert!(!json.to_string().contains("Service Desk"));

        for (name, extra) in
            [("profile_admin", GlobalPermission::ProfilesManage), ("user_admin", GlobalPermission::UsersManage)]
        {
            let ctx = user_ctx(&db.pool, name, &[GlobalPermission::ConfigExportImport, extra]).await;
            let profiles = export(&db.pool, &ctx).await.unwrap().permission_profiles.unwrap();
            assert!(profiles.iter().any(|p| p.name == "Service Desk"), "{name}: {profiles:?}");
        }

        db.drop().await;
    }

    /// GH#407: every export writes exactly one `export` row naming the sections
    /// and counts, attributed to the caller, never the profiles themselves.
    #[tokio::test]
    async fn export_is_audited() {
        let Some(db) = scratch::database("export_audited").await else { return };
        crate::seed::seed_system_rows(&db.pool).await.unwrap();
        let profile = ProfileSpec {
            name: "Service Desk".into(),
            description: Some("Tier one".into()),
            global_permissions: vec![GlobalPermission::CisImport],
            class_permissions: Vec::new(),
        };
        let file = ConfigFile {
            format: FORMAT.into(),
            format_version: FORMAT_VERSION,
            exported_at: None,
            app_version: None,
            data_model: None,
            lookups: None,
            permission_profiles: Some(vec![profile]),
            ui_settings: None,
            import_mappings: None,
            saved_views: None,
            workflows: None,
        };
        import(&db.pool, &RequestContext::system("test", "test"), &file, ImportMode::Apply).await.unwrap();

        let rows = |actor: Uuid| {
            let pool = db.pool.clone();
            async move {
                sqlx::query_as::<_, (String, Uuid, Option<Value>, Option<Value>)>(
                    "SELECT entity_type, entity_id, old_value, new_value FROM audit_log
                     WHERE action = 'export' AND actor_id = $1::text",
                )
                .bind(actor)
                .fetch_all(&pool)
                .await
                .unwrap()
            }
        };
        let only_export = user_ctx(&db.pool, "exporter", &[GlobalPermission::ConfigExportImport]).await;
        let admin = user_ctx(
            &db.pool,
            "profile_admin",
            &[GlobalPermission::ConfigExportImport, GlobalPermission::ProfilesManage],
        )
        .await;
        for (ctx, included) in [(&only_export, false), (&admin, true)] {
            let actor = ctx.principal().unwrap().user_id;
            assert!(rows(actor).await.is_empty());
            let exported = export(&db.pool, ctx).await.unwrap();
            assert_eq!(exported.permission_profiles.is_some(), included);
            let audited = rows(actor).await;
            assert_eq!(audited.len(), 1, "{audited:?}");
            let (entity_type, entity_id, old, new) = audited.into_iter().next().unwrap();
            assert_eq!((entity_type.as_str(), entity_id, old), ("config", Uuid::nil(), None));
            let new = new.unwrap();
            assert_eq!(new["kind"], "config");
            assert_eq!(new["profilesIncluded"], included);
            assert_eq!(new["mappingCount"], 0);
            let sections: Vec<&str> = new["sections"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
            assert_eq!(sections.contains(&"permissionProfiles"), included, "{sections:?}");
            assert!(sections.contains(&"dataModel") && !sections.contains(&"importMappings"), "{sections:?}");
            let text = new.to_string();
            assert!(!text.contains("Service Desk") && !text.contains("Tier one"), "{text}");
        }

        db.drop().await;
    }

    /// GH#516: a mapping saved before the member type was refused (GH#410)
    /// imports with that column ignored, and says so in a warning.
    #[tokio::test]
    async fn an_import_mapping_on_the_member_type_imports_with_the_column_ignored() {
        let Some(src) = scratch::database("config_mapping_member_src").await else { return };
        let Some(dst) = scratch::database("config_mapping_member_dst").await else { return };
        let system = RequestContext::system("test", "test");
        crate::seed::install_template(&src.pool, "it_infrastructure").await.unwrap();
        let (class, attr): (String, String) = sqlx::query_as(
            "SELECT c.key, d.key FROM cmdb.ci_attribute_definitions d JOIN cmdb.ci_classes c ON c.id = d.class_id
             WHERE NOT c.is_abstract ORDER BY c.key, d.key LIMIT 1",
        )
        .fetch_one(&src.pool)
        .await
        .unwrap();
        let member: String =
            sqlx::query_scalar("SELECT key FROM cmdb.relationship_types WHERE system_role = 'business_service_member'")
                .fetch_one(&src.pool)
                .await
                .unwrap();
        let definition: saved::MappingDefinition = serde_json::from_value(serde_json::json!({
            "mode": "create_only",
            "columns": [
                { "header": "Name", "target": { "kind": "attribute", "key": attr } },
                { "header": "Service", "target": {
                    "kind": "relationship", "typeKey": member, "direction": "incoming", "match": { "by": "label" } } },
            ],
        }))
        .unwrap();
        let mut conn = src.pool.acquire().await.unwrap();
        saved::config_write(&mut conn, &system, None, "Old layout", None, &class, &definition).await.unwrap();
        drop(conn);
        let file = export(&src.pool, &system).await.unwrap();

        let res = import(&dst.pool, &system, &file, ImportMode::Apply).await.unwrap();
        let warning = res.warnings.iter().find(|w| w.path == "importMappings.0.definition.columns.1.target");
        assert!(
            warning.is_some_and(|w| w.message.contains("\"Service\"") && w.message.contains("\"Old layout\"")),
            "{:?}",
            res.warnings
        );
        let back = export(&dst.pool, &system).await.unwrap().import_mappings.unwrap();
        assert_eq!(back.len(), 1, "{back:?}");
        assert_eq!(back[0].definition.columns[0].target, definition.columns[0].target);
        assert_eq!(back[0].definition.columns[1].header, "Service");
        assert_eq!(back[0].definition.columns[1].target, ColumnTarget::Ignore);
        src.drop().await;
        dst.drop().await;
    }

    /// SHAA-714 §6.2, AC10: saved import mappings round-trip through format
    /// version 4, merge by class key and name, need `cis.import`, stay within
    /// the classes the caller can view, and are audited with `actor_type = import`.
    #[tokio::test]
    async fn import_mappings_round_trip() {
        use crate::api::route::BodyInput;
        let Some(src) = scratch::database("config_mappings_src").await else { return };
        let Some(dst) = scratch::database("config_mappings_dst").await else { return };
        let system = RequestContext::system("test", "test");
        crate::seed::install_template(&src.pool, "it_infrastructure").await.unwrap();
        let (class, attr): (String, String) = sqlx::query_as(
            "SELECT c.key, d.key FROM cmdb.ci_attribute_definitions d JOIN cmdb.ci_classes c ON c.id = d.class_id
             WHERE NOT c.is_abstract ORDER BY c.key, d.key LIMIT 1",
        )
        .fetch_one(&src.pool)
        .await
        .unwrap();
        let definition = |attr: &str| -> saved::MappingDefinition {
            serde_json::from_value(serde_json::json!({
                "mode": "create_or_update",
                "key": { "field": format!("attributes.{attr}") },
                "columns": [
                    { "header": "Name", "target": { "kind": "attribute", "key": attr } },
                    { "header": "Notes", "target": { "kind": "ignore" } },
                ],
            }))
            .unwrap()
        };
        let mut conn = src.pool.acquire().await.unwrap();
        for (name, description) in [("Vendor layout", Some("From the vendor portal")), ("Plain", None)] {
            saved::config_write(&mut conn, &system, None, name, description, &class, &definition(&attr)).await.unwrap();
        }
        drop(conn);

        // Export writes the current version with the section; the JSON parses back.
        let file = export(&src.pool, &system).await.unwrap();
        assert_eq!(file.format_version, FORMAT_VERSION);
        let mappings = file.import_mappings.clone().unwrap();
        assert_eq!(mappings.len(), 2, "{mappings:?}");
        let raw = serde_json::to_value(&file).unwrap();
        let Ok(parsed) = Body::<ConfigFile>::parse(Some(raw)) else { panic!("export does not parse") };
        assert_eq!(parsed.0, file);

        // Into an empty install: created, then the same file changes nothing.
        let applied = import(&dst.pool, &system, &file, ImportMode::Apply).await.unwrap();
        let section = applied.summary.iter().find(|s| s.section == "importMappings").unwrap();
        assert_eq!((section.created, section.updated, section.unchanged), (2, 0, 0));
        let back = export(&dst.pool, &system).await.unwrap().import_mappings.unwrap();
        assert_eq!(back, mappings);
        let audits = || async {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM audit_log WHERE entity_type = 'import_mappings' AND actor_type = 'import'",
            )
            .fetch_one(&dst.pool)
            .await
            .unwrap()
        };
        assert_eq!(audits().await, 2);
        let again = import(&dst.pool, &system, &file, ImportMode::Apply).await.unwrap();
        let section = again.summary.iter().find(|s| s.section == "importMappings").unwrap();
        assert_eq!((section.created, section.updated, section.unchanged), (0, 0, 2));
        assert_eq!(audits().await, 2);

        // Matched by class and name ignoring case: the definition and description are
        // replaced, the name keeps its spelling. A field the target lacks is a warning.
        let mut changed = ConfigFile { data_model: None, lookups: None, ui_settings: None, ..file.clone() };
        changed.permission_profiles = None;
        let m = &mut changed.import_mappings.as_mut().unwrap()[0];
        m.name = m.name.to_uppercase();
        m.description = Some("Changed".into());
        m.definition = definition("no_such_field");
        let res = import(&dst.pool, &system, &changed, ImportMode::Apply).await.unwrap();
        let section = res.summary.iter().find(|s| s.section == "importMappings").unwrap();
        assert_eq!((section.created, section.updated, section.unchanged), (0, 1, 1), "{:?}", res.changes);
        assert!(res.warnings.iter().any(|w| w.message.contains("no_such_field")), "{:?}", res.warnings);
        let (name, description): (String, Option<String>) = sqlx::query_as(
            "SELECT name, description FROM cmdb.import_mappings WHERE class_key = $1 AND lower(name) = lower($2)",
        )
        .bind(&class)
        .bind(&mappings[0].name)
        .fetch_one(&dst.pool)
        .await
        .unwrap();
        assert_eq!((name.as_str(), description.as_deref()), (mappings[0].name.as_str(), Some("Changed")));
        assert_eq!(audits().await, 3);

        // A class that does not exist here: a warning, and the mapping is kept.
        let mut orphan = changed.clone();
        orphan.import_mappings = Some(vec![ImportMappingSpec { class_key: "not_here".into(), ..mappings[1].clone() }]);
        let res = import(&dst.pool, &system, &orphan, ImportMode::DryRun).await.unwrap();
        assert!(res.warnings.iter().any(|w| w.path == "importMappings.0.classKey"), "{:?}", res.warnings);
        assert_eq!(res.summary.iter().find(|s| s.section == "importMappings").unwrap().created, 1);

        // The same class and name twice in one file.
        let mut twice = changed.clone();
        twice.import_mappings = Some(vec![mappings[1].clone(), mappings[1].clone()]);
        let err = import(&dst.pool, &system, &twice, ImportMode::DryRun).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::ValidationError);
        assert!(err.details.unwrap().iter().any(|d| d.field == "importMappings.1.name" && d.code == "duplicate"));

        // Without cis.import: not exported, and a file with mappings is refused.
        let no_import = user_ctx(&dst.pool, "exporter", &[GlobalPermission::ConfigExportImport]).await;
        let exported = export(&dst.pool, &no_import).await.unwrap();
        assert_eq!(exported.import_mappings, None);
        assert!(serde_json::to_value(&exported).unwrap().get("importMappings").is_none());
        let err = import(&dst.pool, &no_import, &changed, ImportMode::DryRun).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::Forbidden);
        assert!(err.message.contains("cis.import"), "{err}");

        // With cis.import but no view right on the class: nothing exported, and the
        // import skips the mapping with a warning instead of writing it.
        let importer =
            user_ctx(&dst.pool, "importer", &[GlobalPermission::ConfigExportImport, GlobalPermission::CisImport]).await;
        assert_eq!(export(&dst.pool, &importer).await.unwrap().import_mappings, Some(Vec::new()));
        let res = import(&dst.pool, &importer, &changed, ImportMode::Apply).await.unwrap();
        assert!(res.warnings.iter().any(|w| w.message.contains("cannot view")), "{:?}", res.warnings);
        assert!(res.changes.iter().all(|c| c.section != "importMappings"), "{:?}", res.changes);
        assert_eq!(audits().await, 3);

        // An unknown version is refused; version 3 files (no section) still import.
        let err = import(
            &dst.pool,
            &system,
            &ConfigFile { format_version: FORMAT_VERSION + 1, ..changed.clone() },
            ImportMode::DryRun,
        )
        .await
        .unwrap_err();
        assert!(format!("{err:?}").contains(&format!("versions 1 to {FORMAT_VERSION}")), "{err:?}");
        let v3 = ConfigFile { format_version: 3, import_mappings: None, ..changed };
        import(&dst.pool, &system, &v3, ImportMode::DryRun).await.unwrap();

        src.drop().await;
        dst.drop().await;
    }

    /// GH#289: a configuration file with a NUL anywhere, including free-form
    /// parts like UI settings, is a 400 invalid_character before any import runs.
    #[tokio::test]
    async fn nul_in_import_file_is_refused() {
        use crate::api::route::BodyInput;
        let Some(db) = scratch::database("nul_in_import_file_is_refused").await else { return };
        crate::seed::install_template(&db.pool, "it_infrastructure").await.unwrap();
        let file = export(&db.pool, &RequestContext::system("test", "test")).await.unwrap();
        let mut raw = serde_json::to_value(&file).unwrap();
        assert!(Body::<ConfigFile>::parse(Some(raw.clone())).is_ok());
        raw["dataModel"]["classes"][0]["name"] = "Ser\u{0}ver".into();
        let Err(err) = Body::<ConfigFile>::parse(Some(raw)) else { panic!("file passed") };
        assert_eq!(err.code, ErrorCode::ValidationError);
        let details = err.details.unwrap();
        assert_eq!(
            (details[0].field.as_str(), details[0].code.as_str()),
            ("dataModel.classes.0.name", "invalid_character")
        );
        db.drop().await;
    }

    /// GH#289: an import file follows the same character policy as the API:
    /// line breaks in descriptions, not in names; control characters nowhere.
    #[tokio::test]
    async fn import_file_follows_the_character_policy() {
        use crate::api::route::BodyInput;
        let Some(db) = scratch::database("import_file_follows_the_character_policy").await else { return };
        crate::seed::install_template(&db.pool, "it_infrastructure").await.unwrap();
        let file = export(&db.pool, &RequestContext::system("test", "test")).await.unwrap();
        let raw = serde_json::to_value(&file).unwrap();
        let refused = |path: &str, value: &str| {
            let mut raw = raw.clone();
            *raw.pointer_mut(path).unwrap() = value.into();
            Body::<ConfigFile>::parse(Some(raw))
                .err()
                .map(|e| e.details.unwrap().into_iter().map(|d| (d.field, d.code)).collect::<Vec<_>>())
        };
        let character = |field: &str| Some(vec![(field.to_owned(), "invalid_character".to_owned())]);
        assert_eq!(refused("/dataModel/classes/0/name", "Ser\nver"), character("dataModel.classes.0.name"));
        assert_eq!(refused("/dataModel/classes/0/name", "\u{202E}revreS"), character("dataModel.classes.0.name"));
        assert_eq!(
            refused("/dataModel/classes/0/description", "a\u{1B}[0m"),
            character("dataModel.classes.0.description")
        );
        assert_eq!(refused("/dataModel/classes/0/description", "Line 1\nLine 2\t\u{2067}x\u{2069}"), None);
        db.drop().await;
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
        let a = LookupValueSpec {
            key: "live".into(),
            name: "Live".into(),
            description: None,
            color: None,
            sort_order: 0,
            is_active: true,
            parent: None,
        };
        let b = LookupValueSpec { name: "In service".into(), sort_order: 10, ..a.clone() };
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
                ClassGrantSpec {
                    class: Some("server".into()),
                    class_system_role: None,
                    view: false,
                    create: false,
                    edit: true,
                    delete: false,
                },
                ClassGrantSpec {
                    class: None,
                    class_system_role: None,
                    view: true,
                    create: false,
                    edit: false,
                    delete: false,
                },
                ClassGrantSpec {
                    class: Some("app".into()),
                    class_system_role: None,
                    view: false,
                    create: false,
                    edit: false,
                    delete: false,
                },
            ],
        };
        normalise_profile(&mut p);
        assert_eq!(p.global_permissions, [GlobalPermission::UsersManage, GlobalPermission::AuditView]);
        let classes: Vec<Option<&str>> = p.class_permissions.iter().map(|g| g.class.as_deref()).collect();
        assert_eq!(classes, [None, Some("server")]);
        assert!(p.class_permissions[1].view, "edit implies view");
    }

    /// GH#546, GH#551: the audit rows of an applied import hold sign-ins
    /// while they are written, so a file carries a bounded number of entries,
    /// counted across its sections.
    #[test]
    fn a_file_with_too_many_entries_is_refused() {
        let lookups = |owners: usize| -> Value {
            let values: Vec<Value> =
                (0..5000).map(|i| serde_json::json!({ "key": format!("v{i}"), "name": "V" })).collect();
            let lists: Vec<Value> =
                (0..4).map(|n| serde_json::json!({ "key": format!("l{n}"), "name": "L", "values": values })).collect();
            let owners: Vec<Value> =
                (0..owners).map(|i| serde_json::json!({ "kind": "team", "name": format!("o{i}") })).collect();
            serde_json::json!({ "lists": lists, "owners": owners })
        };
        let file = |body: Value| -> ConfigFile {
            let mut file = serde_json::json!({ "format": FORMAT, "formatVersion": 1 });
            file.as_object_mut().unwrap().extend(body.as_object().unwrap().clone());
            serde_json::from_value(file).unwrap()
        };
        let refused = |f: ConfigFile| {
            let e = check_format(&f).unwrap_err();
            assert_eq!(e.code, ErrorCode::ValidationError);
            let d = e.details.unwrap().remove(0);
            assert_eq!(d.code, "too_big");
            (d.field, d.message)
        };

        assert!(check_format(&file(serde_json::json!({ "lookups": lookups(MAX_ENTRIES - 20_000) }))).is_ok());
        let (field, message) = refused(file(serde_json::json!({ "lookups": lookups(MAX_ENTRIES - 20_000 + 1) })));
        assert_eq!(field, "lookups");
        assert!(message.starts_with("The file holds 25001 entries (lookups: 25001)"), "{message}");

        // The data model counts against the same budget: one class, 20,000
        // attributes and 4,999 rules fit; another rule, or a lookup value
        // next to them, does not.
        let data_model = |rules: usize| -> Value {
            let attributes: Vec<Value> = (0..20_000)
                .map(|i| serde_json::json!({ "class": "c", "key": format!("a{i}"), "label": "A", "dataType": "text" }))
                .collect();
            let rules: Vec<Value> = (0..rules)
                .map(|i| serde_json::json!({ "relationshipType": format!("t{i}"), "sourceClass": "c", "targetClass": "c" }))
                .collect();
            serde_json::json!({
                "classes": [{ "key": "c", "name": "C" }], "attributes": attributes, "relationshipRules": rules
            })
        };
        assert!(check_format(&file(serde_json::json!({ "dataModel": data_model(4_999) }))).is_ok());
        let (field, message) = refused(file(serde_json::json!({ "dataModel": data_model(5_000) })));
        assert_eq!(field, "dataModel");
        assert!(message.starts_with("The file holds 25001 entries (dataModel: 25001)"), "{message}");
        let (field, message) = refused(file(serde_json::json!({
            "dataModel": data_model(4_999), "lookups": { "owners": [{ "kind": "team", "name": "o" }] }
        })));
        assert_eq!(field, "dataModel", "reported at the largest section");
        assert!(message.contains("(dataModel: 25000, lookups: 1)"), "{message}");
    }

    #[test]
    fn errors_get_the_file_path() {
        let e = at("dataModel.attributes.3", AppError::field("isRequired", "CIs without a value: 2", "values_missing"));
        let d = &e.details.unwrap()[0];
        assert_eq!(d.field, "dataModel.attributes.3.isRequired");
        assert!(e.message.starts_with("dataModel.attributes.3: "));
        let e = at("lookups.lists.0", AppError::conflict("Duplicate"));
        assert_eq!(e.details.unwrap()[0].field, "lookups.lists.0");
        assert_eq!(e.code, ErrorCode::Conflict);
    }

    fn comparable(file: &ConfigFile) -> Value {
        let mut v = serde_json::to_value(file).unwrap();
        let o = v.as_object_mut().unwrap();
        o.remove("exportedAt");
        o.remove("appVersion");
        v
    }

    /// Same configuration, same file: export of a template install equals the
    /// export of a second database that imported it (GH#23).
    #[tokio::test]
    async fn equal_installs_export_identical_files() {
        const TEST: &str = "equal_installs_export_identical_files";
        let Some(a) = scratch::database(TEST).await else { return };
        let Some(b) = scratch::database(TEST).await else {
            a.drop().await;
            return;
        };
        let ctx = RequestContext::system("test", "test");
        crate::seed::seed_system_rows(&a.pool).await.unwrap();
        crate::seed::seed_system_rows(&b.pool).await.unwrap();
        crate::modules::templates::install_by_key(&a.pool, &ctx, "it_infrastructure").await.unwrap();

        let exported = export(&a.pool, &ctx).await.unwrap();
        let rules = &exported.data_model.as_ref().unwrap().relationship_rules;
        assert!(rules.len() > 1);
        assert!(rules.windows(2).all(|w| w[0] < w[1]), "rules not in key order: {rules:?}");
        assert_eq!(comparable(&exported), comparable(&export(&a.pool, &ctx).await.unwrap()));

        import(&b.pool, &ctx, &exported, ImportMode::Apply).await.unwrap();
        let reexported = export(&b.pool, &ctx).await.unwrap();
        assert_eq!(
            serde_json::to_string_pretty(&comparable(&exported)).unwrap(),
            serde_json::to_string_pretty(&comparable(&reexported)).unwrap()
        );
        // The template's owner fields travel (version 11), and a file without the
        // settings (before version 11) leaves them as they are.
        let owned = |f: &ConfigFile| {
            f.data_model
                .as_ref()
                .unwrap()
                .classes
                .iter()
                .filter(|c| c.owner_attribute == Some(Some("owner".into())))
                .count()
        };
        assert!(owned(&reexported) > 0, "no class exports an owner field");
        let mut old = exported.clone();
        for c in &mut old.data_model.as_mut().unwrap().classes {
            (c.owner_attribute, c.end_of_life_attribute) = (None, None);
        }
        import(&b.pool, &ctx, &ConfigFile { format_version: 10, ..old }, ImportMode::Apply).await.unwrap();
        assert_eq!(owned(&export(&b.pool, &ctx).await.unwrap()), owned(&reexported));
        a.drop().await;
        b.drop().await;
    }

    /// GH#199 (SHAA-490 §A7): identity providers are not part of a
    /// configuration file, so neither their secrets nor the ciphertexts of
    /// them can leave through an export.
    #[tokio::test]
    async fn an_export_carries_no_identity_provider_secret() {
        use crate::secrets::sealed::{ProviderSecret, seal_provider_secret};
        let Some(db) = scratch::database("an_export_carries_no_identity_provider_secret").await else { return };
        let pool = &db.pool;
        let ctx = RequestContext::system("test", "test");
        crate::seed::seed_system_rows(pool).await.unwrap();
        crate::modules::templates::install_by_key(pool, &ctx, "it_infrastructure").await.unwrap();
        const CLIENT_SECRET: &str = "export-client-secret";
        const BIND_PASSWORD: &str = "export-bind-password";
        let ring = crate::secrets::Keyring::for_tests();
        let (oidc, ldap) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let cs = seal_provider_secret(&ring, oidc, ProviderSecret::ClientSecret, CLIENT_SECRET);
        let bp = seal_provider_secret(&ring, ldap, ProviderSecret::BindPassword, BIND_PASSWORD);
        sqlx::query(
            "INSERT INTO identity_providers (id, kind, name, issuer_url, client_id, client_secret_enc, secrets_key_id,
               scopes, username_claim, groups_claim, mfa_assurance, required_acr)
             VALUES ($1, 'oidc', 'Entra ID', 'https://idp.example.test', 'cmdb', $2, $3, 'profile',
               'preferred_username', 'groups', 'verify', '{}')",
        )
        .bind(oidc)
        .bind(&cs.bytes)
        .bind(cs.key_id.0)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO identity_providers (id, kind, name, ldap_url, start_tls, bind_dn, bind_password_enc,
               secrets_key_id, user_base_dn, user_filter, username_attribute, display_name_attribute,
               email_attribute, group_attribute)
             VALUES ($1, 'ldap', 'Corporate AD', 'ldaps://dc1.example.test', false, 'cn=svc', $2, $3,
               'dc=example,dc=com', '(uid={username})', 'uid', 'cn', 'mail', 'memberOf')",
        )
        .bind(ldap)
        .bind(&bp.bytes)
        .bind(bp.key_id.0)
        .execute(pool)
        .await
        .unwrap();

        let file = serde_json::to_string(&export(pool, &ctx).await.unwrap()).unwrap();
        assert!(file.contains("\"dataModel\""), "the export has content");
        let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        use base64::Engine;
        let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
        for needle in [
            CLIENT_SECRET.to_owned(),
            BIND_PASSWORD.to_owned(),
            "clientSecret".into(),
            "bindPassword".into(),
            "client_secret".into(),
            "bind_password".into(),
            "Entra ID".into(),
            "Corporate AD".into(),
            hex(&cs.bytes),
            hex(&bp.bytes),
            b64(&cs.bytes),
            b64(&bp.bytes),
        ] {
            assert!(!file.contains(&needle), "the export contains {needle:?}");
        }
        db.drop().await;
    }

    /// SHAA-268: parent lists, parent values and parent fields go through a
    /// file; files before version 3 leave them as they are.
    #[tokio::test]
    async fn dependent_lookup_lists_round_trip() {
        const TEST: &str = "dependent_lookup_lists_round_trip";
        let Some(a) = scratch::database(TEST).await else { return };
        let Some(b) = scratch::database(TEST).await else {
            a.drop().await;
            return;
        };
        let ctx = RequestContext::system("test", "test");
        crate::seed::seed_system_rows(&a.pool).await.unwrap();
        crate::seed::seed_system_rows(&b.pool).await.unwrap();
        crate::modules::templates::install_by_key(&a.pool, &ctx, "it_infrastructure").await.unwrap();
        fn body<T: serde::de::DeserializeOwned>(v: Value) -> T {
            serde_json::from_value(v).unwrap()
        }
        let maker =
            simple::create::<LookupLists>(&a.pool, &ctx, &body(serde_json::json!({ "key": "maker", "name": "Maker" })))
                .await
                .unwrap();
        let model = simple::create::<LookupLists>(
            &a.pool,
            &ctx,
            &body(serde_json::json!({ "key": "model", "name": "Model", "parentListId": maker.id })),
        )
        .await
        .unwrap();
        let cisco = simple::create::<LookupListValues>(
            &a.pool,
            &ctx,
            &body(serde_json::json!({ "listId": maker.id, "key": "cisco", "name": "Cisco" })),
        )
        .await
        .unwrap();
        simple::create::<LookupListValues>(
            &a.pool,
            &ctx,
            &body(
                serde_json::json!({ "listId": model.id, "key": "c9300", "name": "C9300", "parentValueId": cisco.id }),
            ),
        )
        .await
        .unwrap();
        let server: Uuid =
            sqlx::query_scalar("SELECT id FROM ci_classes WHERE key = 'server'").fetch_one(&a.pool).await.unwrap();
        // The dependent field sorts first, so the import meets it before its parent field.
        let maker_field = simple::create::<AttributeDefinitions>(
            &a.pool,
            &ctx,
            &body(serde_json::json!({ "classId": server, "key": "vendor_name", "label": "Maker", "dataType": "lookup",
                "lookupListId": maker.id, "sortOrder": 900 })),
        )
        .await
        .unwrap();
        simple::create::<AttributeDefinitions>(
            &a.pool,
            &ctx,
            &body(
                serde_json::json!({ "classId": server, "key": "vendor_model", "label": "Model", "dataType": "lookup",
                "lookupListId": model.id, "parentAttributeId": maker_field.id, "sortOrder": -900 }),
            ),
        )
        .await
        .unwrap();

        let exported = export(&a.pool, &ctx).await.unwrap();
        let lists = &exported.lookups.as_ref().unwrap().lists;
        let model_spec = lists.iter().find(|l| l.key == "model").unwrap();
        assert_eq!(model_spec.parent.as_deref(), Some("maker"));
        assert_eq!(model_spec.values[0].parent.as_deref(), Some("cisco"));
        let attrs = &exported.data_model.as_ref().unwrap().attributes;
        let model_attr = attrs.iter().find(|a| a.key == "vendor_model").unwrap();
        assert_eq!(model_attr.parent_attribute.as_deref(), Some("vendor_name"));

        import(&b.pool, &ctx, &exported, ImportMode::Apply).await.unwrap();
        let reexported = export(&b.pool, &ctx).await.unwrap();
        assert_eq!(
            serde_json::to_string_pretty(&comparable(&exported)).unwrap(),
            serde_json::to_string_pretty(&comparable(&reexported)).unwrap()
        );

        // Without parents: a version 2 file keeps them, a version 3 file clears them.
        let mut stripped = exported.clone();
        for l in &mut stripped.lookups.as_mut().unwrap().lists {
            l.parent = None;
            for v in &mut l.values {
                v.parent = None;
            }
        }
        for a in &mut stripped.data_model.as_mut().unwrap().attributes {
            a.parent_attribute = None;
        }
        let v2 = ConfigFile { format_version: 2, ..stripped.clone() };
        let result = import(&b.pool, &ctx, &v2, ImportMode::DryRun).await.unwrap();
        assert!(result.changes.is_empty(), "{:?}", result.changes);
        let result = import(&b.pool, &ctx, &stripped, ImportMode::DryRun).await.unwrap();
        let changed: Vec<(&str, &str)> = result.changes.iter().map(|c| (c.section.as_str(), c.key.as_str())).collect();
        assert_eq!(changed, [("lookupLists", "model")]);
        a.drop().await;
        b.drop().await;
    }

    /// SHAA-784: a file of 0.1.0-rc.1 (version 1, the former tables as
    /// `lookups.statuses` and so on) still imports: the rows become lookup
    /// lists, the former tables stay as they are, and the export leaves the
    /// sections out.
    #[tokio::test]
    async fn an_rc1_file_imports_its_lookups_as_lists() {
        let Some(db) = scratch::database("an_rc1_file_imports_its_lookups_as_lists").await else { return };
        let pool = &db.pool;
        let ctx = RequestContext::system("test", "test");
        crate::seed::seed_system_rows(pool).await.unwrap();
        let legacy_rows = || async {
            sqlx::query_scalar::<_, i64>(
                "SELECT (SELECT count(*) FROM statuses) + (SELECT count(*) FROM environments)
                      + (SELECT count(*) FROM locations) + (SELECT count(*) FROM owners)",
            )
            .fetch_one(pool)
            .await
            .unwrap()
        };
        let before = legacy_rows().await;
        let file: ConfigFile = serde_json::from_value(serde_json::json!({
            "format": "shadoucmdb.config",
            "formatVersion": 1,
            "appVersion": "0.1.0-rc.1",
            "lookups": {
                "statuses": [{ "key": "in_service", "name": "In service", "isOperational": true, "sortOrder": 10 }],
                "environments": [{ "key": "production", "name": "Production" }],
                "locations": [{ "key": "fra1", "name": "Frankfurt 1", "locationType": "site", "address": "Main St 1" }],
                "owners": [{ "kind": "team", "name": "Ops Team", "email": "ops@example.com" }]
            }
        }))
        .unwrap();

        let dry = import(pool, &ctx, &file, ImportMode::DryRun).await.unwrap();
        let paths: Vec<&str> = dry.warnings.iter().map(|w| w.path.as_str()).collect();
        assert_eq!(paths, ["lookups.statuses", "lookups.environments", "lookups.locations", "lookups.owners"]);
        let created: Vec<(&str, &str)> =
            dry.changes.iter().map(|c| (c.section.as_str(), c.key.as_str())).filter(|c| c.0 == "lookupLists").collect();
        assert_eq!(
            created,
            [
                ("lookupLists", "status"),
                ("lookupLists", "environment"),
                ("lookupLists", "location"),
                ("lookupLists", "owner")
            ]
        );
        assert!(
            dry.summary
                .iter()
                .all(|s| !["statuses", "environments", "locations", "owners"].contains(&s.section.as_str()))
        );

        import(pool, &ctx, &file, ImportMode::Apply).await.unwrap();
        assert_eq!(legacy_rows().await, before, "the former tables are not written");
        let exported = export(pool, &ctx).await.unwrap();
        let text = serde_json::to_value(&exported).unwrap();
        let lookups = text["lookups"].as_object().unwrap();
        assert_eq!(lookups.keys().collect::<Vec<_>>(), ["lists"], "{text}");
        let values: Vec<(String, String, Option<String>)> = exported
            .lookups
            .unwrap()
            .lists
            .iter()
            // The criticality system list (migration 0030) is on every install.
            .filter(|l| l.system_role.is_none())
            .flat_map(|l| l.values.iter().map(|v| (l.key.clone(), v.key.clone(), v.description.clone())))
            .collect();
        assert_eq!(
            values,
            [
                ("environment".into(), "production".into(), None),
                ("location".into(), "fra1".into(), Some("Main St 1".into())),
                ("owner".into(), "ops_team".into(), Some("Team, ops@example.com".into())),
                ("status".into(), "in_service".into(), None),
            ]
        );

        // Importing the same old file again changes nothing: the lists already hold the values.
        let again = import(pool, &ctx, &file, ImportMode::DryRun).await.unwrap();
        assert!(again.changes.is_empty(), "{:?}", again.changes);

        // Problems in these sections are reported with the rest of the file.
        let broken: ConfigFile = serde_json::from_value(serde_json::json!({
            "format": "shadoucmdb.config",
            "formatVersion": 1,
            "dataModel": { "attributes": [{ "class": "nope", "key": "x", "label": "X", "dataType": "text" }] },
            "lookups": { "statuses": [{ "key": "dup", "name": "A" }, { "key": "dup", "name": "B" }] }
        }))
        .unwrap();
        let err = import(pool, &ctx, &broken, ImportMode::DryRun).await.unwrap_err();
        let fields: Vec<String> = err.details.unwrap().into_iter().map(|d| d.field).collect();
        assert!(
            fields.contains(&"lookups.statuses.1".into()) && fields.contains(&"dataModel.attributes.0.class".into()),
            "{fields:?}"
        );
        db.drop().await;
    }

    /// GH#344: 20,000 owners with the same key, and lists to look through for
    /// the statuses, import within the request timeout. Four lists keep the
    /// file within `MAX_ENTRIES`; the fold of 200 lists, more than a
    /// file may carry, is timed in `legacy`.
    #[tokio::test]
    async fn the_largest_legacy_sections_import_quickly() {
        let Some(db) = scratch::database("the_largest_legacy_sections_import_quickly").await else { return };
        let ctx = RequestContext::system("test", "test");
        crate::seed::seed_system_rows(&db.pool).await.unwrap();
        let file: ConfigFile = serde_json::from_value(serde_json::json!({
            "format": "shadoucmdb.config", "formatVersion": 1, "lookups": legacy::worst_case(4)
        }))
        .unwrap();
        let started = std::time::Instant::now();
        let dry = import(&db.pool, &ctx, &file, ImportMode::DryRun).await.unwrap();
        let took = started.elapsed();
        assert!(took < std::time::Duration::from_secs(120), "{took:?}");
        let created: Vec<&str> =
            dry.changes.iter().filter(|c| c.section == "lookupLists").map(|c| c.key.as_str()).collect();
        assert!(created.contains(&"owner") && created.contains(&"status_5"), "{created:?}");
        db.drop().await;
    }

    /// Format 4: impactDirection and the criticality list round-trip; a file
    /// before version 4 leaves the impact direction as it is; a dry run shows
    /// a change; a non-directional type is refused a one-way direction.
    #[tokio::test]
    async fn impact_directions_round_trip() {
        use crate::modules::impact::ImpactDirection;
        const TEST: &str = "impact_directions_round_trip";
        let Some(a) = scratch::database(TEST).await else { return };
        let Some(b) = scratch::database(TEST).await else {
            a.drop().await;
            return;
        };
        let ctx = RequestContext::system("test", "test");
        for db in [&a, &b] {
            crate::seed::seed_system_rows(&db.pool).await.unwrap();
            crate::modules::templates::install_by_key(&db.pool, &ctx, "it_infrastructure").await.unwrap();
        }
        sqlx::query("UPDATE relationship_types SET impact_direction = 'both' WHERE key = 'connected_to'")
            .execute(&a.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE lookup_list_values SET name = 'Mission critical' WHERE key = 'critical'")
            .execute(&a.pool)
            .await
            .unwrap();

        let exported = export(&a.pool, &ctx).await.unwrap();
        assert_eq!(exported.format_version, FORMAT_VERSION);
        let direction = |f: &ConfigFile, key: &str| {
            f.data_model.as_ref().unwrap().relationship_types.iter().find(|t| t.key == key).unwrap().impact_direction
        };
        assert_eq!(direction(&exported, "runs_on"), Some(ImpactDirection::TargetToSource));
        assert_eq!(direction(&exported, "connected_to"), Some(ImpactDirection::Both));
        let lists = &exported.lookups.as_ref().unwrap().lists;
        let criticality = lists.iter().find(|l| l.key == "criticality").unwrap();
        assert_eq!(criticality.system_role, Some(crate::modules::lookups::SystemRole::Criticality));
        // Version 5: the built-in business service class and membership type carry their role.
        let dm = exported.data_model.as_ref().unwrap();
        let roles: Vec<(&str, ClassSystemRole)> =
            dm.classes.iter().filter_map(|c| Some((c.key.as_str(), c.system_role?))).collect();
        assert_eq!(
            roles,
            vec![("business_service", ClassSystemRole::BusinessService), ("person", ClassSystemRole::Person)]
        );
        let roles: Vec<(&str, RelationshipTypeSystemRole)> =
            dm.relationship_types.iter().filter_map(|t| Some((t.key.as_str(), t.system_role?))).collect();
        assert_eq!(roles, vec![("business_service_member", RelationshipTypeSystemRole::BusinessServiceMember)]);

        // The dry run shows the change, the import applies it, and the target then exports the same file.
        let result = import(&b.pool, &ctx, &exported, ImportMode::DryRun).await.unwrap();
        let changed: Vec<(&str, &str, Vec<&str>)> = result
            .changes
            .iter()
            .map(|c| (c.section.as_str(), c.key.as_str(), c.fields.iter().map(|f| f.field.as_str()).collect()))
            .collect();
        assert!(changed.contains(&("relationshipTypes", "connected_to", vec!["impactDirection"])), "{changed:?}");
        import(&b.pool, &ctx, &exported, ImportMode::Apply).await.unwrap();
        let reexported = export(&b.pool, &ctx).await.unwrap();
        assert_eq!(
            serde_json::to_string_pretty(&comparable(&exported)).unwrap(),
            serde_json::to_string_pretty(&comparable(&reexported)).unwrap()
        );
        let role: Option<String> = sqlx::query_scalar("SELECT system_role FROM lookup_lists WHERE key = 'criticality'")
            .fetch_one(&b.pool)
            .await
            .unwrap();
        assert_eq!(role.as_deref(), Some("criticality"));

        // A version 3 file (no impactDirection, no systemRole) changes neither.
        let mut v3 = ConfigFile { format_version: 3, ..exported.clone() };
        for t in &mut v3.data_model.as_mut().unwrap().relationship_types {
            t.impact_direction = None;
        }
        for l in &mut v3.lookups.as_mut().unwrap().lists {
            l.system_role = None;
        }
        strip_system_roles(&mut v3);
        let result = import(&b.pool, &ctx, &v3, ImportMode::DryRun).await.unwrap();
        assert!(result.changes.is_empty(), "{:?}", result.changes);
        import(&b.pool, &ctx, &v3, ImportMode::Apply).await.unwrap();
        assert_eq!(direction(&export(&b.pool, &ctx).await.unwrap(), "connected_to"), Some(ImpactDirection::Both));

        // One-way impact on a non-directional type is refused.
        let mut bad = exported.clone();
        for t in &mut bad.data_model.as_mut().unwrap().relationship_types {
            if t.key == "connected_to" {
                t.impact_direction = Some(ImpactDirection::SourceToTarget);
            }
        }
        let err = import(&b.pool, &ctx, &bad, ImportMode::DryRun).await.unwrap_err();
        let fields: Vec<String> = err.details.unwrap().into_iter().map(|d| d.field).collect();
        assert!(fields.iter().any(|f| f.ends_with(".impactDirection")), "{fields:?}");
        a.drop().await;
        b.drop().await;
    }

    /// SHAA-2460: identifying fields (not copied on clone) travel with the
    /// file from version 12; an older file keeps the current setting.
    #[tokio::test]
    async fn identifying_fields_round_trip() {
        const TEST: &str = "identifying_fields_round_trip";
        let Some(a) = scratch::database(TEST).await else { return };
        let Some(b) = scratch::database(TEST).await else {
            a.drop().await;
            return;
        };
        let ctx = RequestContext::system("test", "test");
        for db in [&a, &b] {
            crate::seed::seed_system_rows(&db.pool).await.unwrap();
            crate::modules::templates::install_by_key(&db.pool, &ctx, "it_infrastructure").await.unwrap();
        }
        let identifying = |f: &ConfigFile| -> Vec<String> {
            let dm = f.data_model.as_ref().unwrap();
            dm.attributes.iter().filter(|a| a.is_identifying == Some(true)).map(|a| a.key.clone()).collect()
        };
        // The template marks serial number and asset tag.
        let exported = export(&a.pool, &ctx).await.unwrap();
        assert_eq!(identifying(&exported), vec!["serial_number", "asset_tag"]);
        assert!(exported.data_model.as_ref().unwrap().attributes.iter().all(|a| a.is_identifying.is_some()));

        sqlx::query(
            "UPDATE ci_attribute_definitions SET is_identifying = true
             WHERE key = 'hostname' AND class_id = (SELECT id FROM ci_classes WHERE key = 'hardware')",
        )
        .execute(&a.pool)
        .await
        .unwrap();
        sqlx::query("UPDATE ci_attribute_definitions SET is_identifying = false WHERE key = 'asset_tag'")
            .execute(&a.pool)
            .await
            .unwrap();
        let exported = export(&a.pool, &ctx).await.unwrap();
        let mut keys = identifying(&exported);
        keys.sort();
        assert_eq!(keys, vec!["hostname", "serial_number"]);

        let result = import(&b.pool, &ctx, &exported, ImportMode::DryRun).await.unwrap();
        let changed: Vec<(&str, Vec<&str>)> = result
            .changes
            .iter()
            .map(|c| (c.key.as_str(), c.fields.iter().map(|f| f.field.as_str()).collect()))
            .collect();
        assert!(changed.contains(&("hardware.hostname", vec!["isIdentifying"])), "{changed:?}");
        assert!(changed.contains(&("hardware.asset_tag", vec!["isIdentifying"])), "{changed:?}");
        import(&b.pool, &ctx, &exported, ImportMode::Apply).await.unwrap();
        let reexported = export(&b.pool, &ctx).await.unwrap();
        assert_eq!(
            serde_json::to_string_pretty(&comparable(&exported)).unwrap(),
            serde_json::to_string_pretty(&comparable(&reexported)).unwrap()
        );

        // A version 11 file (no isIdentifying) changes nothing.
        let mut v11 = ConfigFile { format_version: 11, ..reexported };
        for a in &mut v11.data_model.as_mut().unwrap().attributes {
            a.is_identifying = None;
        }
        let result = import(&b.pool, &ctx, &v11, ImportMode::DryRun).await.unwrap();
        assert!(result.changes.iter().all(|c| c.fields.is_empty()), "{:?}", result.changes);
        import(&b.pool, &ctx, &v11, ImportMode::Apply).await.unwrap();
        let mut keys = identifying(&export(&b.pool, &ctx).await.unwrap());
        keys.sort();
        assert_eq!(keys, vec!["hostname", "serial_number"]);
        a.drop().await;
        b.drop().await;
    }

    /// GH#354: an export from before 0036 still has the template's active
    /// Application field "criticality". It imports archived, with a warning,
    /// so Applications do not get a second Criticality; a field an
    /// administrator restored here stays active.
    #[tokio::test]
    async fn superseded_application_criticality_imports_archived() {
        let Some(db) = scratch::database("superseded_application_criticality_imports_archived").await else { return };
        let pool = &db.pool;
        let ctx = RequestContext::system("test", "test");
        crate::seed::seed_system_rows(pool).await.unwrap();
        crate::modules::templates::install_by_key(pool, &ctx, "it_infrastructure").await.unwrap();

        let mut old = export(pool, &ctx).await.unwrap();
        let attrs = &mut old.data_model.as_mut().unwrap().attributes;
        assert!(!attrs.iter().any(|a| a.key == "criticality"), "the template no longer has the field");
        let version = attrs.iter().find(|a| a.class == "application" && a.key == "version").unwrap().clone();
        attrs.push(AttributeSpec {
            key: "criticality".into(),
            label: "Criticality".into(),
            data_type: AttributeDataType::Enum,
            enum_values: Some(["low", "medium", "high", "critical"].map(String::from).to_vec()),
            ..version
        });
        let at = format!("dataModel.attributes.{}.isActive", attrs.len() - 1);
        let field_state = async || -> Option<bool> {
            sqlx::query_scalar(
                "SELECT d.is_active FROM ci_attribute_definitions d JOIN ci_classes c ON c.id = d.class_id
                 WHERE c.key = 'application' AND d.key = 'criticality'",
            )
            .fetch_optional(pool)
            .await
            .unwrap()
        };

        // Created archived, and imported again it stays archived.
        for _ in 0..2 {
            let res = import(pool, &ctx, &old, ImportMode::Apply).await.unwrap();
            assert!(
                res.warnings.iter().any(|w| w.path == at && w.message.contains("core Criticality")),
                "{:?}",
                res.warnings
            );
            assert_eq!(field_state().await, Some(false));
        }

        // Restored by an administrator: the file no longer overrides that.
        sqlx::query("UPDATE ci_attribute_definitions SET is_active = true WHERE key = 'criticality'")
            .execute(pool)
            .await
            .unwrap();
        let res = import(pool, &ctx, &old, ImportMode::Apply).await.unwrap();
        assert!(!res.warnings.iter().any(|w| w.path == at), "{:?}", res.warnings);
        assert_eq!(field_state().await, Some(true));
        db.drop().await;
    }

    /// A file without the version 5 role markers, as versions 1 to 4 write it.
    fn strip_system_roles(file: &mut ConfigFile) {
        if let Some(dm) = file.data_model.as_mut() {
            dm.classes.iter_mut().for_each(|c| c.system_role = None);
            dm.relationship_types.iter_mut().for_each(|t| t.system_role = None);
        }
        for p in file.permission_profiles.iter_mut().flatten() {
            p.class_permissions.iter_mut().for_each(|g| g.class_system_role = None);
        }
    }

    /// SHAA-927 §6.3, §7.1 "Config transfer": an export from an install that
    /// adopted the starter `service` class imports into one whose class is
    /// `business_service`, matched by role: the target class keeps its key,
    /// area and role, gets the file's labels and fields, and no `service` class
    /// appears. A grant follows `classSystemRole`; so does the member type. A
    /// version 4 file still matches by key only.
    #[tokio::test]
    async fn system_classes_and_types_are_matched_by_role() {
        const TEST: &str = "config_system_roles";
        let Some(src) = crate::db::upgrade_0033::v02x(TEST, true).await else { return };
        crate::db::MIGRATOR.run(&src.pool).await.expect("migrations after 0032");
        let Some(dst) = scratch::database(TEST).await else {
            src.drop().await;
            return;
        };
        let ctx = RequestContext::system("test", "test");
        crate::modules::templates::install_by_key(&dst.pool, &ctx, "it_infrastructure").await.unwrap();
        let system_class = |pool: PgPool| async move {
            sqlx::query_as::<_, (Uuid, String, String, String)>(
                "SELECT c.id, c.key, c.name, a.key FROM ci_classes c JOIN areas a ON a.id = c.area_id
                 WHERE c.system_role = 'business_service'",
            )
            .fetch_one(&pool)
            .await
            .unwrap()
        };
        let (_, src_key, _, src_area) = system_class(src.pool.clone()).await;
        assert_eq!((src_key.as_str(), src_area.as_str()), ("service", "infrastruktur"), "the source adopted service");
        let (dst_class, dst_key, _, dst_area) = system_class(dst.pool.clone()).await;
        assert_eq!(dst_key, "business_service");

        // The source renames its class and gives it a field the target lacks.
        sqlx::query("UPDATE ci_classes SET name = 'IT service' WHERE key = 'service'")
            .execute(&src.pool)
            .await
            .unwrap();
        let mut tx = src.pool.begin().await.unwrap();
        let mut c = ColumnSet::default();
        c.opt(
            "class_id",
            Some(
                sqlx::query_scalar::<_, Uuid>("SELECT id FROM ci_classes WHERE key = 'service'")
                    .fetch_one(&mut *tx)
                    .await
                    .unwrap(),
            ),
        )
        .opt("key", Some("cost_centre".to_owned()))
        .opt("label", Some("Cost centre".to_owned()))
        .opt("data_type", Some("text".to_owned()));
        simple::create_in::<AttributeDefinitions>(&mut tx, &ctx, c).await.unwrap_or_else(|e| panic!("{}", e.message));
        tx.commit().await.unwrap();

        let file = export(&src.pool, &ctx).await.unwrap();
        assert_eq!(file.format_version, FORMAT_VERSION);
        let dm = file.data_model.as_ref().unwrap();
        assert_eq!(
            dm.classes.iter().find(|c| c.key == "service").unwrap().system_role,
            Some(ClassSystemRole::BusinessService)
        );
        let desk = file.permission_profiles.as_ref().unwrap().iter().find(|p| p.name == "Service desk").unwrap();
        assert_eq!(desk.class_permissions[0].class.as_deref(), Some("service"));
        assert_eq!(desk.class_permissions[0].class_system_role, Some(ClassSystemRole::BusinessService));

        // The dry run says what it matched, and changes the target's class, not a new "service".
        let result =
            import(&dst.pool, &ctx, &file, ImportMode::DryRun).await.unwrap_or_else(|e| panic!("{e}: {:?}", e.details));
        let matched: Vec<&str> = result
            .warnings
            .iter()
            .filter(|w| w.message.starts_with("Matched by role"))
            .map(|w| w.message.as_str())
            .collect();
        assert_eq!(matched.len(), 1, "{:?}", result.warnings);
        assert!(matched[0].contains("\"service\"") && matched[0].contains("\"business_service\""), "{matched:?}");
        let changes: Vec<(&str, &str, ChangeAction)> =
            result.changes.iter().map(|c| (c.section.as_str(), c.key.as_str(), c.action)).collect();
        assert!(changes.contains(&("classes", "business_service", ChangeAction::Update)), "{changes:?}");
        assert!(changes.contains(&("attributes", "business_service.cost_centre", ChangeAction::Create)), "{changes:?}");
        assert!(!changes.iter().any(|(s, k, _)| *s == "classes" && *k == "service"), "{changes:?}");
        let class_change =
            result.changes.iter().find(|c| c.section == "classes" && c.key == "business_service").unwrap();
        let fields: Vec<&str> = class_change.fields.iter().map(|f| f.field.as_str()).collect();
        assert!(fields.contains(&"name") && !fields.contains(&"systemRole") && !fields.contains(&"area"), "{fields:?}");

        import(&dst.pool, &ctx, &file, ImportMode::Apply).await.unwrap();
        let (id, key, name, area) = system_class(dst.pool.clone()).await;
        assert_eq!(
            (id, key.as_str(), name.as_str(), area.as_str()),
            (dst_class, "business_service", "IT service", dst_area.as_str())
        );
        let service: i64 = sqlx::query_scalar("SELECT count(*) FROM ci_classes WHERE key = 'service'")
            .fetch_one(&dst.pool)
            .await
            .unwrap();
        assert_eq!(service, 0, "no second service class");
        let cost: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM ci_attribute_definitions WHERE class_id = $1 AND key = 'cost_centre'",
        )
        .bind(dst_class)
        .fetch_one(&dst.pool)
        .await
        .unwrap();
        assert_eq!(cost, 1);
        let grant: (bool, bool) = sqlx::query_as(
            "SELECT g.can_view, g.can_edit FROM permission_profile_class_permissions g
             JOIN permission_profiles p ON p.id = g.profile_id WHERE p.name = 'Service desk' AND g.class_id = $1",
        )
        .bind(dst_class)
        .fetch_one(&dst.pool)
        .await
        .unwrap();
        assert_eq!(grant, (true, true), "the grant on service lands on business_service");
        // Importing the same file again changes nothing.
        let again = import(&dst.pool, &ctx, &file, ImportMode::DryRun).await.unwrap();
        assert!(again.changes.is_empty(), "{:?}", again.changes);

        // A grant resolves by classSystemRole, whatever its class key says; the member type by role too.
        let mut by_role = file.clone();
        for g in by_role.permission_profiles.iter_mut().flatten().flat_map(|p| p.class_permissions.iter_mut()) {
            if g.class_system_role.is_some() {
                g.class = Some("no_such_class".into());
                g.create = true;
            }
        }
        for t in &mut by_role.data_model.as_mut().unwrap().relationship_types {
            if t.system_role.is_some() {
                t.key = "service_member".into();
                t.forward_label = "contains".into();
            }
        }
        let result = import(&dst.pool, &ctx, &by_role, ImportMode::Apply)
            .await
            .unwrap_or_else(|e| panic!("{e}: {:?}", e.details));
        let changes: Vec<(&str, &str)> = result.changes.iter().map(|c| (c.section.as_str(), c.key.as_str())).collect();
        assert!(changes.contains(&("relationshipTypes", "business_service_member")), "{changes:?}");
        assert!(changes.contains(&("permissionProfiles", "Service desk")), "{changes:?}");
        let created: i64 = sqlx::query_scalar("SELECT count(*) FROM relationship_types WHERE key = 'service_member'")
            .fetch_one(&dst.pool)
            .await
            .unwrap();
        assert_eq!(created, 0);
        let label: String = sqlx::query_scalar(
            "SELECT forward_label FROM relationship_types WHERE system_role = 'business_service_member'",
        )
        .fetch_one(&dst.pool)
        .await
        .unwrap();
        assert_eq!(label, "contains");
        let can_create: bool = sqlx::query_scalar(
            "SELECT g.can_create FROM permission_profile_class_permissions g
             JOIN permission_profiles p ON p.id = g.profile_id WHERE p.name = 'Service desk' AND g.class_id = $1",
        )
        .bind(dst_class)
        .fetch_one(&dst.pool)
        .await
        .unwrap();
        assert!(can_create);

        // A file holding both the role class and another class with the target's key is refused.
        let mut clash = file.clone();
        let mut other =
            clash.data_model.as_ref().unwrap().classes.iter().find(|c| c.key == "application").unwrap().clone();
        other.key = "business_service".into();
        other.system_role = None;
        clash.data_model.as_mut().unwrap().classes.push(other);
        let err = import(&dst.pool, &ctx, &clash, ImportMode::DryRun).await.unwrap_err();
        let codes: Vec<(String, String)> = err.details.unwrap().into_iter().map(|d| (d.field, d.code)).collect();
        assert!(codes.iter().any(|(f, c)| f.ends_with(".key") && c == "conflict"), "{codes:?}");

        // Version 4 (no roles): matched by key, so "service" is an ordinary new class here.
        let mut v4 = ConfigFile { format_version: 4, ..file.clone() };
        strip_system_roles(&mut v4);
        let result =
            import(&dst.pool, &ctx, &v4, ImportMode::DryRun).await.unwrap_or_else(|e| panic!("{e}: {:?}", e.details));
        let changes: Vec<(&str, &str, ChangeAction)> =
            result.changes.iter().map(|c| (c.section.as_str(), c.key.as_str(), c.action)).collect();
        assert!(changes.contains(&("classes", "service", ChangeAction::Create)), "{changes:?}");
        assert!(!result.warnings.iter().any(|w| w.message.starts_with("Matched by role")), "{:?}", result.warnings);
        let roles: i64 = sqlx::query_scalar("SELECT count(*) FROM ci_classes WHERE system_role IS NOT NULL")
            .fetch_one(&dst.pool)
            .await
            .unwrap();
        assert_eq!(roles, 2, "the business service and Person types");

        src.drop().await;
        dst.drop().await;
    }

    /// GH#500: an import used to write its audit rows as it went, and every
    /// audit row locks the chain head until commit, so a sign-in waited for
    /// the whole import, dry run included. The import is stopped part-way (a
    /// row lock on the second list) after it changed the first list; a
    /// sign-in's audit row must still be written meanwhile.
    #[tokio::test]
    async fn an_import_does_not_hold_up_sign_ins() {
        let Some(db) = scratch::database("an_import_does_not_hold_up_sign_ins").await else { return };
        let pool = &db.pool;
        let ctx = RequestContext::system("test", "test");
        crate::seed::seed_system_rows(pool).await.unwrap();
        let file = |second: &str| -> ConfigFile {
            serde_json::from_value(serde_json::json!({
                "format": FORMAT,
                "formatVersion": FORMAT_VERSION,
                "lookups": { "lists": [
                    { "key": "first", "name": format!("First {second}"), "values": [{ "key": "a", "name": "A" }] },
                    { "key": "second", "name": second }
                ] }
            }))
            .unwrap()
        };
        import(pool, &ctx, &file("v1"), ImportMode::Apply).await.unwrap();

        for (mode, name) in [(ImportMode::DryRun, "v2"), (ImportMode::Apply, "v3")] {
            let mut blocker = pool.begin().await.unwrap();
            let blocker_pid: i32 =
                sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&mut *blocker).await.unwrap();
            sqlx::query("SELECT 1 FROM lookup_lists WHERE key = 'second' FOR UPDATE")
                .execute(&mut *blocker)
                .await
                .unwrap();
            let running = tokio::spawn({
                let (pool, ctx, file) = (pool.clone(), ctx.clone(), file(name));
                async move { import(&pool, &ctx, &file, mode).await }
            });
            // Wait until the import is stuck on the second list.
            let mut waits = 0;
            while !sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY (pg_blocking_pids(pid)))",
            )
            .bind(blocker_pid)
            .fetch_one(pool)
            .await
            .unwrap()
            {
                waits += 1;
                assert!(waits < 200, "{mode:?}: the import never reached the second list");
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }

            let mut login = pool.begin().await.unwrap();
            sqlx::query("SET LOCAL lock_timeout = '2s'").execute(&mut *login).await.unwrap();
            crate::auth::events::login_failure(&mut login, &ctx, "someone", None)
                .await
                .unwrap_or_else(|e| panic!("{mode:?}: a sign-in waited for the import: {e}"));
            login.commit().await.unwrap();

            blocker.commit().await.unwrap();
            let result = running.await.unwrap().unwrap();
            assert_eq!(result.applied, mode == ImportMode::Apply);
        }

        // The apply's rows were written at the end, after the sign-in's, and
        // the dry run's not at all.
        let names: Vec<String> = sqlx::query_scalar(
            "SELECT coalesce(new_value->>'name', action) FROM audit_log
             WHERE entity_type = 'lookup_lists' OR action = 'login.failure' ORDER BY chain_seq",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(names, ["First v1", "v1", "login.failure", "login.failure", "First v3", "v3"]);
        let broken: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log_verify()").fetch_one(pool).await.unwrap();
        assert_eq!(broken, 0);
        db.drop().await;
    }

    /// GH#551: a data model past the entry budget is refused before the
    /// import touches the database: nothing is created and nothing audited.
    #[tokio::test]
    async fn a_data_model_past_the_entry_budget_writes_nothing() {
        let Some(db) = scratch::database("a_data_model_past_the_entry_budget_writes_nothing").await else { return };
        let pool = &db.pool;
        crate::seed::seed_system_rows(pool).await.unwrap();
        let ctx = RequestContext::system("test", "test");
        let attributes: Vec<Value> = (0..MAX_ENTRIES)
            .map(|i| serde_json::json!({ "class": "budget", "key": format!("a{i}"), "label": "A", "dataType": "text" }))
            .collect();
        let file: ConfigFile = serde_json::from_value(serde_json::json!({
            "format": FORMAT, "formatVersion": FORMAT_VERSION,
            "dataModel": { "classes": [{ "key": "budget", "name": "Budget" }], "attributes": attributes }
        }))
        .unwrap();
        let audit_rows =
            || async { sqlx::query_scalar::<_, i64>("SELECT count(*) FROM audit_log").fetch_one(pool).await.unwrap() };
        let before = audit_rows().await;
        for mode in [ImportMode::DryRun, ImportMode::Apply] {
            let err = import(pool, &ctx, &file, mode).await.unwrap_err();
            assert_eq!(err.code, ErrorCode::ValidationError);
            let d = &err.details.unwrap()[0];
            assert_eq!((d.field.as_str(), d.code.as_str()), ("dataModel", "too_big"), "{}", d.message);
        }
        let classes: i64 =
            sqlx::query_scalar("SELECT count(*) FROM ci_classes WHERE key = 'budget'").fetch_one(pool).await.unwrap();
        assert_eq!(classes, 0);
        assert_eq!(audit_rows().await, before);
        db.drop().await;
    }
}
