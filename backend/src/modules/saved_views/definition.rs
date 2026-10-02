//! What a saved view stores (SHAA-578 §2.2): the inventory or search query by
//! key, never by id (D3), never data or rights (D2).
//!
//! One schema serves both contexts, so a bad field is reported on its own path
//! instead of as a failed union. The rules of a search view (a search term,
//! no sort, no columns) are checked by [`SavedViewDefinition::problems`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa::openapi::schema::{ArrayBuilder, ObjectBuilder, Schema, Type};

use crate::api::schemas::{self, KEY_PATTERN, NOT_BLANK_PATTERN};
use crate::http::error::{FieldError, FieldLocation};
use crate::modules::ui_settings::document::{FIELD_PATTERN, UiListSort};

/// Most class keys in one view (the list endpoint's `classId` limit).
pub const MAX_CLASS_KEYS: usize = 100;
/// Most lookup values across all lookup filters of one view.
pub const MAX_LOOKUP_VALUES: usize = 100;
/// Most columns in one view.
pub const MAX_COLUMNS: usize = 50;
/// Largest definition, as compact JSON.
pub const MAX_DEFINITION_BYTES: usize = 16 * 1024;

/// Where a view applies: the inventory list or global search
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SavedViewContext {
    Inventory,
    Search,
}

impl SavedViewContext {
    pub fn as_str(self) -> &'static str {
        match self {
            SavedViewContext::Inventory => "inventory",
            SavedViewContext::Search => "search",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [SavedViewContext::Inventory, SavedViewContext::Search].into_iter().find(|c| c.as_str() == s)
    }
}

/// personal: only its owner sees it; shared: every user who may view one of its classes
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SavedViewVisibility {
    Personal,
    Shared,
}

/// The list's `active` parameter
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum SavedViewActive {
    #[serde(rename = "true")]
    True,
    #[serde(rename = "false")]
    False,
    #[serde(rename = "all")]
    All,
}

/// The list's `deleted` parameter
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SavedViewDeleted {
    Exclude,
    Include,
    Only,
}

fn string() -> ObjectBuilder {
    ObjectBuilder::new().schema_type(Type::String)
}

fn class_keys_schema() -> Schema {
    ArrayBuilder::new()
        .items(string().pattern(Some(KEY_PATTERN)))
        .max_items(Some(MAX_CLASS_KEYS))
        .unique_items(true)
        .description(Some(
            "CI class keys; empty: every class the user may view. The view's default slot (`home`) is its class \
             when there is exactly one, else the unscoped inventory",
        ))
        .into()
}

fn lookups_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .property_names(Some(string().pattern(Some(KEY_PATTERN))))
        .additional_properties(Some(
            ArrayBuilder::new()
                .items(string().pattern(Some(KEY_PATTERN)))
                .min_items(Some(1))
                .max_items(Some(MAX_LOOKUP_VALUES))
                .unique_items(true),
        ))
        .max_properties(Some(MAX_LOOKUP_VALUES))
        .description(Some(
            "Lookup list key -> value keys (at most 100 values in all): CIs holding one of the values of each list \
             given, e.g. {\"environment\": [\"production\", \"staging\"]}",
        ))
        .into()
}

fn columns_schema() -> Schema {
    ArrayBuilder::new()
        .items(string().pattern(Some(FIELD_PATTERN)))
        .max_items(Some(MAX_COLUMNS))
        .unique_items(true)
        .description(Some(
            "Inventory views: columns in display order, built-in fields (label, ident, class, criticality, \
             validFrom, validUntil, active, createdAt, updatedAt) or attributes.<key> of every class in classKeys; \
             empty: the class's admin list view columns, else the built-in ones. Not allowed in search views",
        ))
        .into()
}

fn q_schema() -> Schema {
    string()
        .min_length(Some(1))
        .max_length(Some(200))
        .pattern(Some(NOT_BLANK_PATTERN))
        .description(Some("Search text; required in search views"))
        .into()
}

pub(crate) fn active_schema() -> Schema {
    string()
        .enum_values(Some(["true", "false", "all"]))
        .description(Some("The list's `active` parameter; left out: its default (true)"))
        .into()
}

pub(crate) fn deleted_schema() -> Schema {
    string()
        .enum_values(Some(["exclude", "include", "only"]))
        .description(Some("The list's `deleted` parameter; left out: its default (exclude)"))
        .into()
}

fn ip_within_schema() -> Schema {
    let mut s = schemas::cidr_schema();
    if let Schema::AnyOf(a) = &mut s {
        a.description = Some("Only CIs with a value of an IP attribute inside this CIDR, e.g. 10.20.0.0/16".into());
    }
    s
}

/// The filters of a view, by key
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SavedViewFilters {
    #[schema(schema_with = q_schema)]
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "schemas::trimmed_opt")]
    pub q: Option<String>,
    #[schema(schema_with = lookups_schema)]
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lookups: BTreeMap<String, Vec<String>>,
    #[schema(schema_with = active_schema)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<SavedViewActive>,
    #[schema(schema_with = deleted_schema)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted: Option<SavedViewDeleted>,
    #[schema(schema_with = ip_within_schema)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip_within: Option<String>,
}

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

/// The query a view saves. Inventory and search views share it; a search
/// view needs `filters.q` and has neither `sort` nor `columns`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SavedViewDefinition {
    #[schema(schema_with = class_keys_schema)]
    #[serde(default)]
    pub class_keys: Vec<String>,
    /// Include CIs of subclasses of classKeys (default true)
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub include_subclasses: bool,
    #[serde(default)]
    pub filters: SavedViewFilters,
    /// Inventory views: the sort; left out, the list sorts by label. Not allowed in search views (they are ranked)
    #[schema(nullable = false)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<UiListSort>,
    #[schema(schema_with = columns_schema)]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<String>,
    /// Rows per page; left out, the list's default (50)
    #[schema(nullable = false, minimum = 10, maximum = 200)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_size: Option<i64>,
}

impl Default for SavedViewDefinition {
    fn default() -> Self {
        SavedViewDefinition {
            class_keys: Vec::new(),
            include_subclasses: true,
            filters: SavedViewFilters::default(),
            sort: None,
            columns: Vec::new(),
            page_size: None,
        }
    }
}

pub(crate) fn body_field(field: &str, message: &str, code: &str) -> FieldError {
    FieldError { location: FieldLocation::Body, field: field.into(), message: message.into(), code: code.into() }
}

impl SavedViewDefinition {
    /// Its size as stored: compact JSON.
    pub fn json_bytes(&self) -> usize {
        serde_json::to_vec(self).map(|v| v.len()).unwrap_or(usize::MAX)
    }

    /// What the schema cannot say, reported under `prefix` (e.g. `definition`):
    /// the lookup value total, the size, and the rules of a search view.
    pub fn problems(&self, context: SavedViewContext, prefix: &str) -> Vec<FieldError> {
        let mut e = Vec::new();
        let values: usize = self.filters.lookups.values().map(Vec::len).sum();
        if values > MAX_LOOKUP_VALUES {
            e.push(body_field(
                &format!("{prefix}.filters.lookups"),
                &format!("At most {MAX_LOOKUP_VALUES} lookup values in all"),
                "too_big",
            ));
        }
        if context == SavedViewContext::Search {
            if self.filters.q.as_deref().is_none_or(str::is_empty) {
                e.push(body_field(&format!("{prefix}.filters.q"), "A search view needs a search term", "required"));
            }
            if self.sort.is_some() {
                e.push(body_field(
                    &format!("{prefix}.sort"),
                    "Search results are ranked; a search view has no sort",
                    "not_allowed",
                ));
            }
            if !self.columns.is_empty() {
                e.push(body_field(&format!("{prefix}.columns"), "A search view has no columns", "not_allowed"));
            }
        }
        if self.json_bytes() > MAX_DEFINITION_BYTES {
            e.push(body_field(prefix, "The definition is larger than 16 KiB", "too_large"));
        }
        e
    }

    /// The view's default slot: its class when it has exactly one, else the
    /// unscoped inventory (`""`, as `saved_view_defaults.home` stores it).
    pub fn home(&self) -> &str {
        match self.class_keys.as_slice() {
            [one] => one,
            _ => "",
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn defaults_are_left_out_when_stored() {
        let d: SavedViewDefinition = serde_json::from_value(json!({})).unwrap();
        assert_eq!(d, SavedViewDefinition::default());
        assert_eq!(serde_json::to_value(&d).unwrap(), json!({ "classKeys": [], "filters": {} }));
        let d: SavedViewDefinition =
            serde_json::from_value(json!({ "classKeys": ["server"], "includeSubclasses": false })).unwrap();
        assert_eq!(serde_json::to_value(&d).unwrap()["includeSubclasses"], false);
        assert_eq!(d.home(), "server");
        assert_eq!(SavedViewDefinition { class_keys: vec!["a".into(), "b".into()], ..d }.home(), "");
    }

    #[test]
    fn search_views_need_a_term_and_take_no_sort_or_columns() {
        let d: SavedViewDefinition = serde_json::from_value(json!({
            "sort": { "field": "label" }, "columns": ["label"]
        }))
        .unwrap();
        let fields: Vec<(String, String)> =
            d.problems(SavedViewContext::Search, "definition").into_iter().map(|e| (e.field, e.code)).collect();
        assert_eq!(
            fields,
            [
                ("definition.filters.q".to_owned(), "required".to_owned()),
                ("definition.sort".to_owned(), "not_allowed".to_owned()),
                ("definition.columns".to_owned(), "not_allowed".to_owned()),
            ]
        );
        assert!(d.problems(SavedViewContext::Inventory, "definition").is_empty());
    }

    #[test]
    fn lookup_values_are_counted_across_lists() {
        let values = |n: usize| (0..n).map(|i| format!("v{i}")).collect::<Vec<_>>();
        let mut d = SavedViewDefinition::default();
        d.filters.lookups.insert("a".into(), values(60));
        d.filters.lookups.insert("b".into(), values(40));
        assert!(d.problems(SavedViewContext::Inventory, "definition").is_empty(), "100 values");
        d.filters.lookups.get_mut("b").unwrap().push("v99".into());
        let e = d.problems(SavedViewContext::Inventory, "definition");
        assert_eq!((e[0].field.as_str(), e[0].code.as_str()), ("definition.filters.lookups", "too_big"));
    }
}
