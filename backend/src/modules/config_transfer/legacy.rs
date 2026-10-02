//! The `statuses`, `environments`, `locations` and `owners` sections of files
//! exported before 0.1.0. Migration 0016 turned those tables into the lookup
//! lists `status`, `environment`, `location` and `owner` (same ids, same keys),
//! so an import does the same with the sections: they become lookup lists and
//! go through the normal list import (diff, audit, permissions). The former
//! tables are never written.
//!
//! Exports made after 0016 but before these sections were dropped carry both a
//! frozen copy of the former table and the list 0016 made from it. That section
//! is skipped: the list in the file holds the same values.
//!
//! A list that already exists here keeps what the section cannot know: its
//! name, description, order, state and parent list, and the colour and parent
//! value of each value. The section brings the values' names, descriptions,
//! order and state, as a list in the file would.

use std::collections::{HashMap, HashSet};

use super::format::{ConfigFile, LookupListSpec, LookupSection, LookupValueSpec};
use super::{ImportWarning, duplicates, owner_key};
use crate::api::schemas::OwnerKind;
use crate::http::error::FieldError;

/// Converts the legacy sections of `file` into lookup lists and empties them.
/// `current` is this install's lookups. Returns the problems of the sections
/// (repeated keys), for the validation to report with the rest of the file; a
/// section with problems is not converted.
pub(super) fn fold(
    file: &mut ConfigFile,
    current: Option<&LookupSection>,
    warnings: &mut Vec<ImportWarning>,
) -> Vec<FieldError> {
    let mut e = Vec::new();
    let Some(lk) = file.lookups.as_mut() else { return e };
    let statuses_ok = no_repeats(&lk.statuses, "lookups.statuses", |s| s.key.clone(), &mut e);
    let environments_ok = no_repeats(&lk.environments, "lookups.environments", |s| s.key.clone(), &mut e);
    let locations_ok = no_repeats(&lk.locations, "lookups.locations", |s| s.key.clone(), &mut e);
    let owners_ok = no_repeats(&lk.owners, "lookups.owners", |o| owner_key(o.kind, &o.name), &mut e);

    let statuses = std::mem::take(&mut lk.statuses)
        .into_iter()
        .map(|s| value(s.key, s.name, s.description, s.sort_order, s.is_active))
        .collect();
    let environments = std::mem::take(&mut lk.environments)
        .into_iter()
        .map(|s| value(s.key, s.name, s.description, s.sort_order, s.is_active))
        .collect();
    // The tree, the location type and is_operational have no place in a list (as in 0016).
    let locations = std::mem::take(&mut lk.locations)
        .into_iter()
        .map(|l| value(l.key, l.name, l.description.or(l.address), l.sort_order, l.is_active))
        .collect();
    let owners = owner_values(std::mem::take(&mut lk.owners));

    for (section, table, key, name, values, ok) in [
        ("statuses", "statuses", "status", "Status", statuses, statuses_ok),
        ("environments", "environments", "environment", "Environment", environments, environments_ok),
        ("locations", "locations", "location", "Location", locations, locations_ok),
        ("owners", "owners", "owner", "Owner", owners, owners_ok),
    ] {
        if ok {
            let here = |key: &str| current.and_then(|c| c.lists.iter().find(|l| l.key == key));
            add(lk, here, section, table, key, name, values, warnings);
        }
    }
    e
}

/// [`duplicates`], telling whether it found none.
fn no_repeats<'a, T>(items: &'a [T], path: &str, key: impl Fn(&'a T) -> String, e: &mut Vec<FieldError>) -> bool {
    let before = e.len();
    duplicates(items, path, key, e);
    e.len() == before
}

fn value(key: String, name: String, description: Option<String>, sort_order: i32, is_active: bool) -> LookupValueSpec {
    LookupValueSpec { key, name, description, color: None, sort_order, is_active, parent: None }
}

#[allow(clippy::too_many_arguments)]
fn add<'a>(
    lk: &mut LookupSection,
    here: impl Fn(&str) -> Option<&'a LookupListSpec>,
    section: &str,
    table: &str,
    base: &str,
    name: &str,
    mut values: Vec<LookupValueSpec>,
    warnings: &mut Vec<ImportWarning>,
) {
    if values.is_empty() {
        return;
    }
    let path = format!("lookups.{section}");
    let keys: HashSet<&str> = values.iter().map(|v| v.key.as_str()).collect();
    // The first free key, as 0016 picks it ("status", "status_2", ...), unless
    // a list on the way already holds every value: then the file carries them.
    // Indexed, so that a file of many lists with many values costs linear time.
    let by_key: HashMap<&str, &LookupListSpec> = lk.lists.iter().map(|l| (l.key.as_str(), l)).collect();
    let mut key = base.to_owned();
    let mut n = 1;
    while let Some(list) = by_key.get(key.as_str()) {
        let held: HashSet<&str> = list.values.iter().map(|v| v.key.as_str()).collect();
        if keys.is_subset(&held) {
            warnings.push(ImportWarning {
                path,
                message: format!(
                    "Ignored: {} rows of the former {table} table. The lookup list \"{key}\" in this file already holds \
                     them; the former table is no longer imported.",
                    values.len()
                ),
            });
            return;
        }
        n += 1;
        key = format!("{base}_{n}");
    }
    let n = values.len();
    let list = match here(&key) {
        Some(existing) => {
            let old: HashMap<&str, &LookupValueSpec> = existing.values.iter().map(|o| (o.key.as_str(), o)).collect();
            for v in &mut values {
                if let Some(old) = old.get(v.key.as_str()) {
                    v.color.clone_from(&old.color);
                    v.parent.clone_from(&old.parent);
                }
            }
            warnings.push(ImportWarning {
                path,
                message: format!(
                    "{n} rows of the former {table} table are imported as values of the existing lookup list \
                     \"{key}\", as migration 0016 converts that table."
                ),
            });
            LookupListSpec { values, ..existing.clone() }
        }
        None => {
            warnings.push(ImportWarning {
                path,
                message: format!(
                    "{n} rows of the former {table} table are imported as the new lookup list \"{key}\", as \
                     migration 0016 converts that table. Assign the list to a lookup field to use it on CIs."
                ),
            });
            LookupListSpec {
                key,
                name: name.to_owned(),
                description: Some(format!("Values of the former {table} table (migration 0016)")),
                sort_order: 0,
                is_active: true,
                parent: None,
                system_role: None,
                values,
            }
        }
    };
    lk.lists.push(list);
}

/// Owners had no key. 0016 derives one from the name (lower_snake_case, a
/// suffix for repeats) and numbers them by name; the description keeps the
/// kind and e-mail.
fn owner_values(mut owners: Vec<super::format::OwnerSpec>) -> Vec<LookupValueSpec> {
    owners.sort_by_key(|o| o.name.to_lowercase());
    let mut taken: HashSet<String> = HashSet::new();
    // The last suffix given per base: the search for a free key resumes there
    // instead of trying "base", "base_2", ... again for every repeat. A key
    // can still be taken by another base ("a 2" takes "a_2"), hence the loop.
    let mut last: HashMap<String, usize> = HashMap::new();
    owners
        .into_iter()
        .enumerate()
        .map(|(i, o)| {
            let base = owner_key_base(&o.name);
            let k = last.entry(base.clone()).or_insert(0);
            let mut key = base.clone();
            loop {
                *k += 1;
                if *k > 1 {
                    key = format!("{base}_{k}");
                }
                if !taken.contains(&key) {
                    break;
                }
            }
            taken.insert(key.clone());
            let kind = match o.kind {
                OwnerKind::Team => "Team",
                OwnerKind::Person => "Person",
            };
            let description =
                Some([Some(kind), o.email.as_deref()].into_iter().flatten().collect::<Vec<_>>().join(", "));
            let n = i as i32 + 1;
            value(key, o.name, description, n * 10, o.is_active)
        })
        .collect()
}

/// `btrim(lower(regexp_replace(name, '[^A-Za-z0-9]+', '_', 'g')), '_')`, then an
/// `owner_` prefix unless it starts with a letter, cut to 56 characters.
fn owner_key_base(name: &str) -> String {
    let mut s = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c.to_ascii_lowercase());
        } else if !s.ends_with('_') {
            s.push('_');
        }
    }
    let mut s = s.trim_matches('_').to_owned();
    if !s.starts_with(|c: char| c.is_ascii_lowercase()) {
        s = format!("owner_{s}").trim_matches('_').to_owned();
    }
    s.truncate(56);
    s.trim_end_matches('_').to_owned()
}

/// Sections about as large as a file within the size limit can carry: 20,000
/// owners whose names all give the key "a", and `lists` lists "status",
/// "status_2", ... that each hold all but one of 1,000 statuses (GH#344).
/// Searching for a free key one by one took minutes, while the import holds
/// the import lock.
#[cfg(test)]
pub(super) fn worst_case(lists: usize) -> serde_json::Value {
    use serde_json::json;
    let owners: Vec<_> = (0..20_000u32)
        .map(|i| {
            let marks: String = format!("{i:b}").chars().map(|c| if c == '0' { '.' } else { '-' }).collect();
            json!({ "kind": "person", "name": format!("a{marks}") })
        })
        .collect();
    let statuses: Vec<_> = (0..1000).map(|i| json!({ "key": format!("s{i}"), "name": "S" })).collect();
    let lists: Vec<_> = (1..=lists)
        .map(|n| {
            let key = if n == 1 { "status".to_owned() } else { format!("status_{n}") };
            let values: Vec<_> = (0..999).rev().map(|i| json!({ "key": format!("s{i}"), "name": "S" })).collect();
            json!({ "key": key, "name": "S", "values": values })
        })
        .collect();
    json!({ "owners": owners, "statuses": statuses, "lists": lists })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn file(lookups: serde_json::Value) -> ConfigFile {
        serde_json::from_value(json!({ "format": "shadoucmdb.config", "formatVersion": 1, "lookups": lookups }))
            .unwrap()
    }

    fn folded(lookups: serde_json::Value) -> (LookupSection, Vec<ImportWarning>) {
        let mut f = file(lookups);
        let mut w = Vec::new();
        assert!(fold(&mut f, None, &mut w).is_empty());
        (f.lookups.unwrap(), w)
    }

    #[test]
    fn an_old_file_gets_the_lists_migration_0016_makes() {
        let (lk, w) = folded(json!({
            "statuses": [{ "key": "in_service", "name": "In service", "isOperational": true, "sortOrder": 20 },
                         { "key": "retired", "name": "Retired", "isActive": false }],
            "environments": [{ "key": "production", "name": "Production" }],
            "locations": [{ "key": "fra1", "name": "Frankfurt 1", "locationType": "site", "address": "Main St 1" },
                          { "key": "fra1_r1", "name": "Room 1", "parent": "fra1", "locationType": "room",
                            "description": "Ground floor" }],
            "owners": [{ "kind": "team", "name": "Ops Team", "email": "ops@example.com" },
                       { "kind": "person", "name": "ops team" },
                       { "kind": "person", "name": "4 Eyes" }],
        }));
        assert!(
            lk.statuses.is_empty() && lk.environments.is_empty() && lk.locations.is_empty() && lk.owners.is_empty()
        );
        let keys: Vec<&str> = lk.lists.iter().map(|l| l.key.as_str()).collect();
        assert_eq!(keys, ["status", "environment", "location", "owner"]);
        assert_eq!(w.len(), 4);
        assert_eq!(w[0].path, "lookups.statuses");

        let status = &lk.lists[0];
        assert_eq!(status.description.as_deref(), Some("Values of the former statuses table (migration 0016)"));
        assert_eq!(
            (status.values[0].key.as_str(), status.values[0].sort_order, status.values[0].is_active),
            ("in_service", 20, true)
        );
        assert!(!status.values[1].is_active);

        let loc = &lk.lists[2].values;
        assert_eq!(loc[0].description.as_deref(), Some("Main St 1"), "the address when there is no description");
        assert_eq!((loc[1].description.as_deref(), &loc[1].parent), (Some("Ground floor"), &None));

        let owners: Vec<(&str, &str, Option<&str>, i32)> = lk.lists[3]
            .values
            .iter()
            .map(|v| (v.key.as_str(), v.name.as_str(), v.description.as_deref(), v.sort_order))
            .collect();
        assert_eq!(
            owners,
            [
                ("owner_4_eyes", "4 Eyes", Some("Person"), 10),
                ("ops_team", "Ops Team", Some("Team, ops@example.com"), 20),
                ("ops_team_2", "ops team", Some("Person"), 30),
            ]
        );
    }

    #[test]
    fn a_section_the_files_list_already_holds_is_skipped() {
        let (lk, w) = folded(json!({
            "statuses": [{ "key": "in_service", "name": "In service" }],
            "lists": [{ "key": "status", "name": "Status", "values": [
                { "key": "in_service", "name": "In service" }, { "key": "planned", "name": "Planned" }] }],
        }));
        assert_eq!(lk.lists.len(), 1);
        assert_eq!(lk.lists[0].values.len(), 2);
        assert!(w[0].message.starts_with("Ignored: 1 rows of the former statuses table"), "{}", w[0].message);

        // 0016 put the table in "status_2" because an administrator's list had the key.
        let (lk, w) = folded(json!({
            "statuses": [{ "key": "in_service", "name": "In service" }],
            "lists": [{ "key": "status", "name": "Ticket status", "values": [{ "key": "open", "name": "Open" }] },
                      { "key": "status_2", "name": "Status", "values": [{ "key": "in_service", "name": "In service" }] }],
        }));
        assert_eq!(lk.lists.len(), 2);
        assert!(w[0].message.contains("\"status_2\""), "{}", w[0].message);
    }

    #[test]
    fn a_taken_key_gets_a_suffix_like_0016() {
        let (lk, _) = folded(json!({
            "statuses": [{ "key": "in_service", "name": "In service" }],
            "lists": [{ "key": "status", "name": "Ticket status", "values": [{ "key": "open", "name": "Open" }] }],
        }));
        assert_eq!(lk.lists[1].key, "status_2");
        assert_eq!(lk.lists[1].values[0].key, "in_service");
    }

    #[test]
    fn a_list_that_exists_here_keeps_what_the_section_cannot_know() {
        let here: LookupSection = serde_json::from_value(json!({ "lists": [{
            "key": "status", "name": "Lifecycle", "description": "Ours", "sortOrder": 5, "parent": "phase",
            "values": [{ "key": "in_service", "name": "Live", "color": "#00aa00", "parent": "run" },
                       { "key": "planned", "name": "Planned" }]
        }] }))
        .unwrap();
        let mut f = file(json!({ "statuses": [{ "key": "in_service", "name": "In service", "sortOrder": 20 },
                                              { "key": "broken", "name": "Broken" }] }));
        let mut w = Vec::new();
        assert!(fold(&mut f, Some(&here), &mut w).is_empty());
        let list = &f.lookups.unwrap().lists[0];
        assert_eq!(
            (list.name.as_str(), list.description.as_deref(), list.sort_order, list.parent.as_deref()),
            ("Lifecycle", Some("Ours"), 5, Some("phase"))
        );
        let v = &list.values;
        assert_eq!(v.len(), 2, "values missing from the section stay (notInFile), they are not listed");
        assert_eq!(
            (v[0].name.as_str(), v[0].sort_order, v[0].color.as_deref(), v[0].parent.as_deref()),
            ("In service", 20, Some("#00aa00"), Some("run"))
        );
        assert_eq!((v[1].key.as_str(), v[1].color.as_deref(), v[1].parent.as_deref()), ("broken", None, None));
        assert!(w[0].message.contains("existing lookup list \"status\""), "{}", w[0].message);
    }

    #[test]
    fn duplicates_are_reported_on_the_legacy_path() {
        let mut f = file(json!({ "owners": [{ "kind": "team", "name": "Ops" }, { "kind": "team", "name": "OPS" }] }));
        let errors = fold(&mut f, None, &mut Vec::new());
        assert_eq!(errors[0].field, "lookups.owners.1");
        assert!(f.lookups.unwrap().lists.is_empty(), "a section with problems is not converted");
    }

    /// The suffixes as 0016 gives them, by trying "base", "base_2", ... for
    /// every owner (the code before GH#344).
    fn keys_by_probing(names: &[&str]) -> Vec<String> {
        let mut names = names.to_vec();
        names.sort_by_key(|n| n.to_lowercase());
        let mut taken = HashSet::new();
        names
            .iter()
            .map(|n| {
                let base = owner_key_base(n);
                let (mut key, mut k) = (base.clone(), 1);
                while taken.contains(&key) {
                    k += 1;
                    key = format!("{base}_{k}");
                }
                taken.insert(key.clone());
                key
            })
            .collect()
    }

    #[test]
    fn owner_suffixes_are_those_of_0016() {
        // Bases that take each other's suffixed keys ("a 2" is "a_2").
        let names = ["a", "a 2", "a-", "A.", "a 3", "a_2 ", "a 2 2", "a--", "b", "a 4", "a---", "a 2-", "B 2"];
        let owners = names.iter().map(|n| serde_json::from_value(json!({ "kind": "team", "name": n })).unwrap());
        let keys: Vec<String> = owner_values(owners.collect()).into_iter().map(|v| v.key).collect();
        assert_eq!(keys, keys_by_probing(&names));
        let mut unique = keys.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), keys.len(), "{keys:?}");
    }

    #[test]
    fn the_largest_sections_fold_in_linear_time() {
        let here: LookupSection = serde_json::from_value(json!({ "lists": [{ "key": "owner", "name": "Owner",
            "values": (0..5000).map(|i| json!({ "key": format!("a_{}", i + 1), "name": "A" })).collect::<Vec<_>>() }] }))
        .unwrap();
        let mut f = file(worst_case(200));
        let mut w = Vec::new();
        let started = std::time::Instant::now();
        assert!(fold(&mut f, Some(&here), &mut w).is_empty());
        let took = started.elapsed();
        assert!(took < std::time::Duration::from_secs(5), "{took:?}");
        let lk = f.lookups.unwrap();
        let owner = lk.lists.iter().find(|l| l.key == "owner").unwrap();
        assert_eq!((owner.values[0].key.as_str(), owner.values[19_999].key.as_str()), ("a", "a_20000"));
        assert_eq!(lk.lists.last().unwrap().key, "owner");
        assert!(lk.lists.iter().any(|l| l.key == "status_201" && l.values.len() == 1000));
    }

    #[test]
    fn owner_keys_follow_0016() {
        assert_eq!(owner_key_base("  Jane  Doe (IT) "), "jane_doe_it");
        assert_eq!(owner_key_base("Müller"), "m_ller");
        assert_eq!(owner_key_base("42"), "owner_42");
        assert_eq!(owner_key_base("---"), "owner");
        assert_eq!(owner_key_base(&"a".repeat(80)).len(), 56);
        assert_eq!(owner_key_base(&format!("{}_b", "a".repeat(55))), "a".repeat(55));
    }
}
