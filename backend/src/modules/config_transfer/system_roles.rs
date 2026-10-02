//! Built-in classes and relationship types are matched by system role, not by
//! key (format version 5, SHAA-927 §6.3).
//!
//! The business service class is `service` on an install that adopted the
//! starter class and `business_service` (or `business_service_2`, …) on
//! others; the membership type likewise. A file's class or type that carries
//! a `systemRole` therefore stands for this install's class or type of that
//! role. Before anything else looks at the file, its key is replaced with this
//! install's key everywhere the file refers to it (parents, attributes,
//! reference fields, rules, grants, saved mappings, UI settings), and the
//! class keeps the area it has here. Its labels, description and attributes
//! are then imported as for any class.
//!
//! An import never sets, clears or moves a role: the column is never written.
//! The file's `systemRole` values are replaced with this install's (by key),
//! so a version 1 to 4 file, which has none, matches by key exactly as before
//! and shows no role change in the diff. A grant's `classSystemRole` resolves
//! the grant to this install's class of that role, whatever its `class` says.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::format::{ClassSpec, ConfigFile, RelationshipTypeSpec};
use super::{ImportWarning, problem};
use crate::http::error::FieldError;
use crate::modules::classes::{ClassSystemRole, RelationshipTypeSystemRole};
use crate::modules::imports::schemas::ColumnTarget;
use crate::modules::ui_settings::document::UiSettingsDocument;

fn class_role_name(role: ClassSystemRole) -> &'static str {
    match role {
        ClassSystemRole::BusinessService => "business service class",
    }
}

fn type_role_name(role: RelationshipTypeSystemRole) -> &'static str {
    match role {
        RelationshipTypeSystemRole::BusinessServiceMember => "business service membership type",
    }
}

/// File key -> this install's key, for the rows of one kind that carry a
/// role. `here` is (key, role) of this install's rows; `items` the file's
/// (key, role), `section` their path in the file.
fn renames<R: Copy + Eq + std::hash::Hash>(
    items: &[(String, Option<R>)],
    here: &[(String, R)],
    section: &str,
    what: &str,
    name: fn(R) -> &'static str,
    errors: &mut Vec<FieldError>,
    warnings: &mut Vec<ImportWarning>,
) -> HashMap<String, String> {
    let by_role: HashMap<R, &str> = here.iter().map(|(k, r)| (*r, k.as_str())).collect();
    let keys: HashSet<&str> = items.iter().map(|(k, _)| k.as_str()).collect();
    let mut seen: HashSet<R> = HashSet::new();
    let mut out = HashMap::new();
    for (i, (key, role)) in items.iter().enumerate() {
        let Some(role) = role else { continue };
        let path = format!("{section}.{i}");
        if !seen.insert(*role) {
            problem(
                errors,
                format!("{path}.systemRole"),
                "duplicate",
                format!("Only one {what} can be the {}", name(*role)),
            );
            continue;
        }
        let Some(target) = by_role.get(role) else {
            problem(
                errors,
                format!("{path}.systemRole"),
                "not_found",
                format!("This install has no {}; update it before importing this file", name(*role)),
            );
            continue;
        };
        if key == target {
            continue;
        }
        if keys.contains(target) {
            problem(
                errors,
                format!("{path}.key"),
                "conflict",
                format!(
                    "\"{key}\" is the {role} in the file and is matched by role to this install's {role} \"{target}\", \
                     but the file also has another {what} \"{target}\". Rename that {what} in the file, or remove it",
                    role = name(*role)
                ),
            );
            continue;
        }
        warnings.push(ImportWarning {
            path,
            message: format!(
                "Matched by role: {what} \"{key}\" is the {} in the file and is imported into this install's \
                 \"{target}\"; its key stays \"{target}\"",
                name(*role)
            ),
        });
        out.insert(key.clone(), (*target).to_owned());
    }
    out
}

fn rename(key: &mut String, map: &HashMap<String, String>) {
    if let Some(new) = map.get(key.as_str()) {
        *key = new.clone();
    }
}

fn rename_opt(key: &mut Option<String>, map: &HashMap<String, String>) {
    if let Some(k) = key.as_mut() {
        rename(k, map);
    }
}

/// `classKey` and `classKeys` anywhere in the UI settings document.
fn rename_ui_classes(doc: &mut UiSettingsDocument, map: &HashMap<String, String>) {
    fn walk(v: &mut Value, map: &HashMap<String, String>) {
        match v {
            Value::Object(o) => {
                for (field, value) in o.iter_mut() {
                    match (field.as_str(), value) {
                        ("classKey", Value::String(k)) => rename(k, map),
                        ("classKeys", Value::Array(keys)) => {
                            for k in keys.iter_mut() {
                                if let Value::String(k) = k {
                                    rename(k, map);
                                }
                            }
                        }
                        (_, value) => walk(value, map),
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|i| walk(i, map)),
            _ => {}
        }
    }
    let Ok(mut v) = serde_json::to_value(&*doc) else { return };
    walk(&mut v, map);
    if let Ok(renamed) = serde_json::from_value(v) {
        *doc = renamed;
    }
}

/// Rewrites `file` so its built-in classes and types carry this install's keys
/// (see the module docs). Returns the problems that stop the import.
pub(super) fn match_by_role(
    file: &mut ConfigFile,
    current: &ConfigFile,
    warnings: &mut Vec<ImportWarning>,
) -> Vec<FieldError> {
    let mut errors = Vec::new();
    let empty: (Vec<ClassSpec>, Vec<RelationshipTypeSpec>) = (Vec::new(), Vec::new());
    let (cur_classes, cur_types) = match &current.data_model {
        Some(d) => (&d.classes, &d.relationship_types),
        None => (&empty.0, &empty.1),
    };
    let class_roles: Vec<(String, ClassSystemRole)> =
        cur_classes.iter().filter_map(|c| Some((c.key.clone(), c.system_role?))).collect();
    let type_roles: Vec<(String, RelationshipTypeSystemRole)> =
        cur_types.iter().filter_map(|t| Some((t.key.clone(), t.system_role?))).collect();
    let class_of_role: HashMap<ClassSystemRole, &str> = class_roles.iter().map(|(k, r)| (*r, k.as_str())).collect();
    let role_of_class: HashMap<&str, ClassSystemRole> = class_roles.iter().map(|(k, r)| (k.as_str(), *r)).collect();
    let role_of_type: HashMap<&str, RelationshipTypeSystemRole> =
        type_roles.iter().map(|(k, r)| (k.as_str(), *r)).collect();
    let area_here: HashMap<&str, Option<&String>> =
        cur_classes.iter().map(|c| (c.key.as_str(), c.area.as_ref())).collect();

    let (classes, types) = match &file.data_model {
        Some(dm) => {
            let c: Vec<(String, Option<ClassSystemRole>)> =
                dm.classes.iter().map(|c| (c.key.clone(), c.system_role)).collect();
            let t: Vec<(String, Option<RelationshipTypeSystemRole>)> =
                dm.relationship_types.iter().map(|t| (t.key.clone(), t.system_role)).collect();
            let classes =
                renames(&c, &class_roles, "dataModel.classes", "class", class_role_name, &mut errors, warnings);
            let types = renames(
                &t,
                &type_roles,
                "dataModel.relationshipTypes",
                "relationship type",
                type_role_name,
                &mut errors,
                warnings,
            );
            (classes, types)
        }
        None => (HashMap::new(), HashMap::new()),
    };

    for (i, p) in file.permission_profiles.iter().flatten().enumerate() {
        for (j, g) in p.class_permissions.iter().enumerate() {
            if let Some(role) = g.class_system_role
                && !class_of_role.contains_key(&role)
            {
                problem(
                    &mut errors,
                    format!("permissionProfiles.{i}.classPermissions.{j}.classSystemRole"),
                    "not_found",
                    format!("This install has no {}", class_role_name(role)),
                );
            }
        }
    }
    if !errors.is_empty() {
        return errors;
    }

    if let Some(dm) = file.data_model.as_mut() {
        for c in &mut dm.classes {
            rename(&mut c.key, &classes);
            rename_opt(&mut c.parent, &classes);
            // A class with a role keeps the area it has here (classes never move).
            if c.system_role.is_some()
                && let Some(area) = area_here.get(c.key.as_str())
            {
                c.area = area.cloned();
            }
            c.system_role = role_of_class.get(c.key.as_str()).copied();
        }
        for a in &mut dm.attributes {
            rename(&mut a.class, &classes);
            rename_opt(&mut a.reference_class, &classes);
        }
        for t in &mut dm.relationship_types {
            rename(&mut t.key, &types);
            t.system_role = role_of_type.get(t.key.as_str()).copied();
        }
        for r in &mut dm.relationship_rules {
            rename(&mut r.relationship_type, &types);
            rename(&mut r.source_class, &classes);
            rename(&mut r.target_class, &classes);
        }
        // Rules never applied to the member type and are refused on it now
        // (GH#410); a file from before that carries one imports without it.
        dm.relationship_rules.retain(|r| !role_of_type.contains_key(r.relationship_type.as_str()));
    }
    for p in file.permission_profiles.iter_mut().flatten() {
        for g in &mut p.class_permissions {
            match g.class_system_role.and_then(|r| class_of_role.get(&r)) {
                Some(here) => g.class = Some((*here).to_owned()),
                None => rename_opt(&mut g.class, &classes),
            }
            g.class_system_role = g.class.as_deref().and_then(|k| role_of_class.get(k).copied());
        }
    }
    for (i, m) in file.import_mappings.iter_mut().flatten().enumerate() {
        rename(&mut m.class_key, &classes);
        for (j, c) in m.definition.columns.iter_mut().enumerate() {
            if let ColumnTarget::Relationship { type_key, .. } = &mut c.target {
                rename(type_key, &types);
                // Members are added on the service, never imported (GH#410); a
                // mapping saved before that keeps the column, ignored (GH#516).
                if role_of_type.contains_key(type_key.as_str()) {
                    warnings.push(ImportWarning {
                        path: format!("importMappings.{i}.definition.columns.{j}.target"),
                        message: format!(
                            "Business service members cannot be imported; column \"{}\" of mapping \"{}\" is saved as ignored",
                            c.header, m.name
                        ),
                    });
                    c.target = ColumnTarget::Ignore;
                    c.options = None;
                }
            }
        }
    }
    if !classes.is_empty()
        && let Some(ui) = file.ui_settings.as_mut()
    {
        rename_ui_classes(&mut ui.settings, &classes);
    }
    errors
}
