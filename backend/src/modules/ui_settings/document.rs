//! The UI settings document: branding, navigation, dashboard, list views and
//! detail/form layouts. One document applies to every user.
//!
//! The schema below is the contract: `PUT /api/v1/ui-settings` validates a
//! document against it (plus the cross-field rules in [`Check`]), and every
//! section has a default, so `{}` is a valid document and the UI falls back to
//! its built-in behaviour for anything left out.
//!
//! Classes, attributes and lookups are referenced by key, never by id, so a
//! document can be exported from one install and imported into another. A
//! reference to something that does not exist (deleted, or not imported yet)
//! is not an error: [`resolve`] drops it from the effective document and
//! reports it as an [`Issue`], and the stored document keeps it in case the
//! class comes back.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa::openapi::schema::{ArrayBuilder, ObjectBuilder, Schema, Type};

use crate::api::route::Check;
use crate::api::schemas::{COLOR_PATTERN, KEY_PATTERN, NOT_BLANK_PATTERN};
pub use crate::data::ui_settings::Model;
use crate::http::error::{FieldError, FieldLocation};

/// CI fields that are not attributes; attributes are `attributes.<key>`.
#[cfg(test)]
pub const BUILTIN_FIELDS: &[&str] = &[
    "name",
    "class",
    "status",
    "environment",
    "owner",
    "location",
    "hostname",
    "ipAddress",
    "serialNumber",
    "notes",
    "createdAt",
    "updatedAt",
];

pub const FIELD_PATTERN: &str = "^(name|class|status|environment|owner|location|hostname|ipAddress|serialNumber|notes|createdAt|updatedAt|attributes\\.[a-z][a-z0-9_]{0,62})$";

const ATTRIBUTE_PREFIX: &str = "attributes.";

fn string() -> ObjectBuilder {
    ObjectBuilder::new().schema_type(Type::String)
}

fn key_list(description: &str, max: usize) -> Schema {
    ArrayBuilder::new()
        .items(string().pattern(Some(KEY_PATTERN)))
        .max_items(Some(max))
        .unique_items(true)
        .description(Some(description))
        .into()
}

fn class_keys_schema() -> Schema {
    key_list("CI class keys", 500)
}
fn status_keys_schema() -> Schema {
    key_list("Status keys", 100)
}
fn environment_keys_schema() -> Schema {
    key_list("Environment keys", 100)
}
fn location_keys_schema() -> Schema {
    key_list("Location keys", 500)
}

fn field_list(description: &str) -> Schema {
    ArrayBuilder::new()
        .items(string().pattern(Some(FIELD_PATTERN)))
        .max_items(Some(200))
        .unique_items(true)
        .description(Some(description))
        .into()
}

fn columns_schema() -> Schema {
    field_list(
        "Columns in display order: built-in fields (name, class, status, environment, owner, location, hostname, \
         ipAddress, serialNumber, notes, createdAt, updatedAt) or attributes.<key>",
    )
}
fn panel_fields_schema() -> Schema {
    field_list("Fields in display order (built-in fields or attributes.<key>)")
}
fn hidden_fields_schema() -> Schema {
    field_list("Fields not shown on the detail page or the form (name cannot be hidden)")
}
fn read_only_fields_schema() -> Schema {
    field_list("Fields shown but not editable on the form (name cannot be read-only)")
}

fn label_schema() -> Schema {
    let s: Schema = string().min_length(Some(1)).max_length(Some(100)).pattern(Some(NOT_BLANK_PATTERN)).into();
    utoipa::openapi::schema::AnyOfBuilder::new()
        .item(s)
        .item(ObjectBuilder::new().schema_type(Type::Null))
        .description(Some("Display label; null keeps the default"))
        .into()
}

fn app_name_schema() -> Schema {
    let s: Schema = string().min_length(Some(1)).max_length(Some(60)).pattern(Some(NOT_BLANK_PATTERN)).into();
    utoipa::openapi::schema::AnyOfBuilder::new()
        .item(s)
        .item(ObjectBuilder::new().schema_type(Type::Null))
        .description(Some("Shown in the header, the login page and the browser title; null means \"ShadouCMDB\""))
        .into()
}

fn color_schema() -> Schema {
    utoipa::openapi::schema::AnyOfBuilder::new()
        .item(string().pattern(Some(COLOR_PATTERN)))
        .item(ObjectBuilder::new().schema_type(Type::Null))
        .description(Some("Hex colour, e.g. \"#1f6feb\"; null keeps the built-in theme colour"))
        .into()
}

fn theme_schema() -> Schema {
    string()
        .enum_values(Some(["light", "dark", "system"]))
        .default(Some("system".into()))
        .description(Some("Theme for users who have not picked one; system follows the operating system"))
        .into()
}

fn key_schema() -> Schema {
    string().pattern(Some(KEY_PATTERN)).description(Some("Stable machine key, lower_snake_case")).into()
}

fn custom(field: String, message: impl Into<String>) -> FieldError {
    FieldError { location: FieldLocation::Body, field, message: message.into(), code: "custom".into() }
}

// ---------------------------------------------------------------------------
// Branding
// ---------------------------------------------------------------------------

/// Theme for users who have not picked one
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UiTheme {
    Light,
    Dark,
    /// Follow the operating system
    #[default]
    System,
}

/// App name, colours and the default theme. Logo and favicon are uploaded separately (`/api/v1/ui-settings/assets/{kind}`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct UiBranding {
    #[schema(schema_with = app_name_schema)]
    pub app_name: Option<String>,
    #[schema(schema_with = color_schema)]
    pub primary_color: Option<String>,
    #[schema(schema_with = color_schema)]
    pub accent_color: Option<String>,
    #[schema(schema_with = theme_schema)]
    pub default_theme: UiTheme,
}

// ---------------------------------------------------------------------------
// Navigation
// ---------------------------------------------------------------------------

/// Built-in pages of the web UI
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UiPage {
    Dashboard,
    Inventory,
    Search,
    AuditLog,
    Administration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UiNavEntryType {
    /// A built-in page (`page`)
    Page,
    /// The inventory of one CI class (`classKey`)
    Class,
    /// A group of classes (`key`, `label`, `items`)
    Section,
}

/// A class inside a navigation section
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiNavClassItem {
    #[schema(schema_with = key_schema)]
    pub class_key: String,
    #[schema(schema_with = label_schema)]
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub hidden: bool,
}

/// One menu entry. `type` decides which other fields apply: page -> `page`;
/// class -> `classKey`; section -> `key`, `label` and `items`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiNavEntry {
    #[serde(rename = "type")]
    #[schema(inline)]
    pub kind: UiNavEntryType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(inline, nullable = false)]
    pub page: Option<UiPage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, pattern = "^[a-z][a-z0-9_]{0,62}$")]
    pub class_key: Option<String>,
    /// Section key (unique among sections)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, pattern = "^[a-z][a-z0-9_]{0,62}$")]
    pub key: Option<String>,
    #[schema(schema_with = label_schema)]
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub hidden: bool,
    /// section: its classes in display order
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schema(max_items = 500)]
    pub items: Vec<UiNavClassItem>,
}

/// Menu order, labels, hidden entries and class sections. Pages and classes
/// that are not listed appear after the listed ones, in their default order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct UiNavigation {
    #[schema(max_items = 500)]
    pub entries: Vec<UiNavEntry>,
}

// ---------------------------------------------------------------------------
// Dashboard
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UiWidgetType {
    CountByClass,
    CountByStatus,
    CountByEnvironment,
    RecentChanges,
    /// A saved inventory search (`search`), showing its first `limit` CIs and the total
    SavedSearch,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UiWidgetSize {
    Small,
    #[default]
    Medium,
    Large,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UiSortDirection {
    #[default]
    Asc,
    Desc,
}

/// Sort for an inventory list; `field` is one of the inventory sort fields
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiListSort {
    #[schema(pattern = "^(name|hostname|ipAddress|serialNumber|className|statusName|createdAt|updatedAt)$")]
    pub field: String,
    #[serde(default)]
    #[schema(inline)]
    pub direction: UiSortDirection,
}

/// Inventory filters, by key
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct UiListFilters {
    /// Search text
    #[schema(max_length = 200)]
    pub q: Option<String>,
    #[schema(schema_with = status_keys_schema)]
    pub status_keys: Vec<String>,
    #[schema(schema_with = environment_keys_schema)]
    pub environment_keys: Vec<String>,
    #[schema(schema_with = location_keys_schema)]
    pub location_keys: Vec<String>,
}

/// A stored inventory search
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct UiSavedSearch {
    #[schema(schema_with = class_keys_schema)]
    pub class_keys: Vec<String>,
    /// Include CIs of subclasses of `classKeys`
    pub include_subclasses: bool,
    #[schema(inline)]
    pub filters: UiListFilters,
    #[schema(inline)]
    pub sort: Option<UiListSort>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiWidget {
    /// Unique among the widgets
    #[schema(schema_with = key_schema)]
    pub id: String,
    #[serde(rename = "type")]
    #[schema(inline)]
    pub kind: UiWidgetType,
    #[schema(schema_with = label_schema)]
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    #[schema(inline)]
    pub size: UiWidgetSize,
    /// recent_changes and saved_search: rows shown (default 10)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, minimum = 1, maximum = 50)]
    pub limit: Option<i64>,
    /// count_by_class: only these classes (empty: all)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schema(schema_with = class_keys_schema)]
    pub class_keys: Vec<String>,
    /// saved_search: the search (required for that type)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub search: Option<UiSavedSearch>,
}

/// Dashboard widgets in display order; null keeps the built-in dashboard
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct UiDashboard {
    #[schema(max_items = 50)]
    pub widgets: Option<Vec<UiWidget>>,
}

// ---------------------------------------------------------------------------
// Per-class list views and layouts
// ---------------------------------------------------------------------------

/// The inventory list of one class
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiListView {
    #[schema(schema_with = key_schema)]
    pub class_key: String,
    /// Empty keeps the default columns
    #[schema(schema_with = columns_schema)]
    #[serde(default)]
    pub columns: Vec<String>,
    #[serde(default)]
    pub default_sort: Option<UiListSort>,
    #[serde(default)]
    pub default_filters: UiListFilters,
    /// Rows per page (default 50)
    #[serde(default)]
    #[schema(minimum = 10, maximum = 200)]
    pub page_size: Option<i64>,
}

/// A panel (card) on the detail page and the form
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiLayoutPanel {
    /// Unique within the layout
    #[schema(schema_with = key_schema)]
    pub key: String,
    #[schema(min_length = 1, max_length = 100, pattern = "\\S")]
    pub label: String,
    #[schema(schema_with = panel_fields_schema)]
    #[serde(default)]
    pub fields: Vec<String>,
    /// Start collapsed on the detail page
    #[serde(default)]
    pub collapsed: bool,
}

/// Detail page and form layout of one class. Fields not placed in a panel
/// follow in a trailing panel, grouped by attribute group as before.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiClassLayout {
    #[schema(schema_with = key_schema)]
    pub class_key: String,
    #[serde(default)]
    #[schema(max_items = 50)]
    pub panels: Vec<UiLayoutPanel>,
    #[schema(schema_with = hidden_fields_schema)]
    #[serde(default)]
    pub hidden_fields: Vec<String>,
    #[schema(schema_with = read_only_fields_schema)]
    #[serde(default)]
    pub read_only_fields: Vec<String>,
}

/// Every UI setting. All sections are optional; `{}` is the default UI.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct UiSettingsDocument {
    pub branding: UiBranding,
    pub navigation: UiNavigation,
    pub dashboard: UiDashboard,
    /// At most one per class
    #[schema(max_items = 1000)]
    pub list_views: Vec<UiListView>,
    /// At most one per class
    #[schema(max_items = 1000)]
    pub layouts: Vec<UiClassLayout>,
}

// ---------------------------------------------------------------------------
// Structural rules (independent of the data model)
// ---------------------------------------------------------------------------

impl Check for UiSettingsDocument {
    fn check(&self) -> Vec<FieldError> {
        self.problems("")
    }
}

impl UiSettingsDocument {
    /// Cross-field problems, with field paths below `prefix` (e.g. "settings.").
    pub fn problems(&self, prefix: &str) -> Vec<FieldError> {
        let mut e = Vec::new();
        let at = |p: String| format!("{prefix}{p}");

        let mut pages = HashSet::new();
        let mut classes = HashSet::new();
        let mut sections = HashSet::new();
        for (i, n) in self.navigation.entries.iter().enumerate() {
            let p = format!("navigation.entries.{i}");
            let wrong =
                |field: &str| custom(at(format!("{p}.{field}")), format!("Not allowed for {:?} entries", n.kind));
            match n.kind {
                UiNavEntryType::Page => {
                    match n.page {
                        None => e.push(custom(at(format!("{p}.page")), "Required for page entries")),
                        Some(pg) if !pages.insert(pg) => {
                            e.push(custom(at(format!("{p}.page")), "Each page can appear once"))
                        }
                        _ => {}
                    }
                    if n.class_key.is_some() {
                        e.push(wrong("classKey"));
                    }
                    if n.key.is_some() {
                        e.push(wrong("key"));
                    }
                    if !n.items.is_empty() {
                        e.push(wrong("items"));
                    }
                }
                UiNavEntryType::Class => {
                    match &n.class_key {
                        None => e.push(custom(at(format!("{p}.classKey")), "Required for class entries")),
                        Some(k) if !classes.insert(k.clone()) => {
                            e.push(custom(at(format!("{p}.classKey")), "Each class can appear once in the menu"))
                        }
                        _ => {}
                    }
                    if n.page.is_some() {
                        e.push(wrong("page"));
                    }
                    if n.key.is_some() {
                        e.push(wrong("key"));
                    }
                    if !n.items.is_empty() {
                        e.push(wrong("items"));
                    }
                }
                UiNavEntryType::Section => {
                    match &n.key {
                        None => e.push(custom(at(format!("{p}.key")), "Required for section entries")),
                        Some(k) if !sections.insert(k.clone()) => {
                            e.push(custom(at(format!("{p}.key")), "Section keys must be unique"))
                        }
                        _ => {}
                    }
                    if n.label.is_none() {
                        e.push(custom(at(format!("{p}.label")), "Required for section entries"));
                    }
                    if n.page.is_some() {
                        e.push(wrong("page"));
                    }
                    if n.class_key.is_some() {
                        e.push(wrong("classKey"));
                    }
                    for (j, item) in n.items.iter().enumerate() {
                        if !classes.insert(item.class_key.clone()) {
                            e.push(custom(
                                at(format!("{p}.items.{j}.classKey")),
                                "Each class can appear once in the menu",
                            ));
                        }
                    }
                }
            }
        }

        if let Some(widgets) = &self.dashboard.widgets {
            let mut ids = HashSet::new();
            for (i, w) in widgets.iter().enumerate() {
                let p = format!("dashboard.widgets.{i}");
                if !ids.insert(w.id.as_str()) {
                    e.push(custom(at(format!("{p}.id")), "Widget ids must be unique"));
                }
                match w.kind {
                    UiWidgetType::SavedSearch if w.search.is_none() => {
                        e.push(custom(at(format!("{p}.search")), "Required for saved_search widgets"))
                    }
                    UiWidgetType::SavedSearch => {}
                    _ if w.search.is_some() => {
                        e.push(custom(at(format!("{p}.search")), "Only allowed for saved_search widgets"))
                    }
                    _ => {}
                }
                if w.limit.is_some() && !matches!(w.kind, UiWidgetType::RecentChanges | UiWidgetType::SavedSearch) {
                    e.push(custom(
                        at(format!("{p}.limit")),
                        "Only allowed for recent_changes and saved_search widgets",
                    ));
                }
                if !w.class_keys.is_empty() && w.kind != UiWidgetType::CountByClass {
                    e.push(custom(
                        at(format!("{p}.classKeys")),
                        "Only allowed for count_by_class widgets (saved searches use search.classKeys)",
                    ));
                }
            }
        }

        let mut seen = HashSet::new();
        for (i, v) in self.list_views.iter().enumerate() {
            if !seen.insert(v.class_key.as_str()) {
                e.push(custom(at(format!("listViews.{i}.classKey")), "One list view per class"));
            }
        }

        let mut seen = HashSet::new();
        for (i, l) in self.layouts.iter().enumerate() {
            let p = format!("layouts.{i}");
            if !seen.insert(l.class_key.as_str()) {
                e.push(custom(at(format!("{p}.classKey")), "One layout per class"));
            }
            let mut panels = HashSet::new();
            let mut placed = HashSet::new();
            for (j, panel) in l.panels.iter().enumerate() {
                if !panels.insert(panel.key.as_str()) {
                    e.push(custom(at(format!("{p}.panels.{j}.key")), "Panel keys must be unique in a layout"));
                }
                for (k, f) in panel.fields.iter().enumerate() {
                    if !placed.insert(f.as_str()) {
                        e.push(custom(at(format!("{p}.panels.{j}.fields.{k}")), "A field can be in one panel only"));
                    }
                }
            }
            if let Some(k) = l.hidden_fields.iter().position(|f| f == "name") {
                e.push(custom(at(format!("{p}.hiddenFields.{k}")), "The name field cannot be hidden"));
            }
            if let Some(k) = l.read_only_fields.iter().position(|f| f == "name") {
                e.push(custom(at(format!("{p}.readOnlyFields.{k}")), "The name field cannot be read-only"));
            }
        }
        e
    }
}

// ---------------------------------------------------------------------------
// References to the data model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum IssueCode {
    /// The CI class does not exist (deleted, or not imported yet); the entry is ignored
    UnknownClass,
    /// The attribute is not defined on the class or its ancestors; the field is ignored
    UnknownAttribute,
    UnknownStatus,
    UnknownEnvironment,
    UnknownLocation,
    /// A required attribute is hidden or read-only on the form: CIs of the class cannot be created in the UI (kept, only flagged)
    RequiredFieldNotEditable,
}

/// A reference the effective settings ignore, or a setting worth a second look
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Issue {
    /// Path in the stored document, e.g. "listViews.2.columns.3"
    pub path: String,
    #[schema(inline)]
    pub code: IssueCode,
    pub message: String,
}

struct Resolver<'m> {
    model: &'m Model,
    issues: Vec<Issue>,
}

impl Resolver<'_> {
    fn flag(&mut self, path: String, code: IssueCode, message: String) {
        self.issues.push(Issue { path, code, message });
    }

    fn class_ok(&mut self, path: String, key: &str) -> bool {
        if self.model.classes.contains_key(key) {
            return true;
        }
        self.flag(path, IssueCode::UnknownClass, format!("CI class \"{key}\" does not exist"));
        false
    }

    /// Keeps the classes that exist.
    fn classes(&mut self, path: &str, keys: &[String]) -> Vec<String> {
        keys.iter()
            .enumerate()
            .filter(|(i, k)| self.class_ok(format!("{path}.{i}"), k))
            .map(|(_, k)| k.clone())
            .collect()
    }

    fn lookups(
        &mut self,
        path: &str,
        keys: &[String],
        known: fn(&Model) -> &HashSet<String>,
        code: IssueCode,
        what: &str,
    ) -> Vec<String> {
        let mut out = Vec::new();
        for (i, k) in keys.iter().enumerate() {
            if known(self.model).contains(k) {
                out.push(k.clone());
            } else {
                self.flag(format!("{path}.{i}"), code, format!("{what} \"{k}\" does not exist"));
            }
        }
        out
    }

    fn filters(&mut self, path: &str, f: &UiListFilters) -> UiListFilters {
        UiListFilters {
            q: f.q.clone(),
            status_keys: self.lookups(
                &format!("{path}.statusKeys"),
                &f.status_keys,
                |m| &m.statuses,
                IssueCode::UnknownStatus,
                "Status",
            ),
            environment_keys: self.lookups(
                &format!("{path}.environmentKeys"),
                &f.environment_keys,
                |m| &m.environments,
                IssueCode::UnknownEnvironment,
                "Environment",
            ),
            location_keys: self.lookups(
                &format!("{path}.locationKeys"),
                &f.location_keys,
                |m| &m.locations,
                IssueCode::UnknownLocation,
                "Location",
            ),
        }
    }

    /// Keeps built-in fields and attributes of the class.
    fn fields(&mut self, path: &str, class: &str, fields: &[String]) -> Vec<String> {
        let attrs = &self.model.classes[class];
        let mut out = Vec::new();
        for (i, f) in fields.iter().enumerate() {
            match f.strip_prefix(ATTRIBUTE_PREFIX) {
                Some(a) if !attrs.contains_key(a) => self.flag(
                    format!("{path}.{i}"),
                    IssueCode::UnknownAttribute,
                    format!("Attribute \"{a}\" is not defined on class \"{class}\""),
                ),
                _ => out.push(f.clone()),
            }
        }
        out
    }
}

/// The effective document (dangling references removed) and what was removed or flagged.
pub fn resolve(doc: &UiSettingsDocument, model: &Model) -> (UiSettingsDocument, Vec<Issue>) {
    let mut r = Resolver { model, issues: Vec::new() };
    let mut out = doc.clone();

    out.navigation.entries = Vec::new();
    for (i, n) in doc.navigation.entries.iter().enumerate() {
        let p = format!("navigation.entries.{i}");
        match n.kind {
            UiNavEntryType::Class => {
                if n.class_key.as_deref().is_some_and(|k| r.class_ok(format!("{p}.classKey"), k)) {
                    out.navigation.entries.push(n.clone());
                }
            }
            UiNavEntryType::Section => {
                let mut s = n.clone();
                s.items = n
                    .items
                    .iter()
                    .enumerate()
                    .filter(|(j, it)| r.class_ok(format!("{p}.items.{j}.classKey"), &it.class_key))
                    .map(|(_, it)| it.clone())
                    .collect();
                out.navigation.entries.push(s);
            }
            UiNavEntryType::Page => out.navigation.entries.push(n.clone()),
        }
    }

    if let Some(widgets) = &doc.dashboard.widgets {
        let mut kept = Vec::with_capacity(widgets.len());
        for (i, w) in widgets.iter().enumerate() {
            let p = format!("dashboard.widgets.{i}");
            let mut w = w.clone();
            w.class_keys = r.classes(&format!("{p}.classKeys"), &w.class_keys);
            if let Some(s) = &w.search {
                let class_keys = r.classes(&format!("{p}.search.classKeys"), &s.class_keys);
                let filters = r.filters(&format!("{p}.search.filters"), &s.filters);
                w.search = Some(UiSavedSearch { class_keys, filters, ..s.clone() });
            }
            kept.push(w);
        }
        out.dashboard.widgets = Some(kept);
    }

    out.list_views = Vec::new();
    for (i, v) in doc.list_views.iter().enumerate() {
        let p = format!("listViews.{i}");
        if !r.class_ok(format!("{p}.classKey"), &v.class_key) {
            continue;
        }
        let mut v = v.clone();
        v.columns = r.fields(&format!("{p}.columns"), &v.class_key, &v.columns);
        v.default_filters = r.filters(&format!("{p}.defaultFilters"), &v.default_filters);
        out.list_views.push(v);
    }

    out.layouts = Vec::new();
    for (i, l) in doc.layouts.iter().enumerate() {
        let p = format!("layouts.{i}");
        if !r.class_ok(format!("{p}.classKey"), &l.class_key) {
            continue;
        }
        let mut l = l.clone();
        for (j, panel) in l.panels.iter_mut().enumerate() {
            panel.fields = r.fields(&format!("{p}.panels.{j}.fields"), &l.class_key, &panel.fields);
        }
        l.hidden_fields = r.fields(&format!("{p}.hiddenFields"), &l.class_key, &l.hidden_fields);
        l.read_only_fields = r.fields(&format!("{p}.readOnlyFields"), &l.class_key, &l.read_only_fields);
        // Positions refer to the stored document, like every other issue path.
        let stored = &doc.layouts[i];
        let attrs = &model.classes[&l.class_key];
        for (list, name, what) in [
            (&stored.hidden_fields, "hiddenFields", "hidden"),
            (&stored.read_only_fields, "readOnlyFields", "read-only"),
        ] {
            for (k, f) in list.iter().enumerate() {
                let Some(a) = f.strip_prefix(ATTRIBUTE_PREFIX) else { continue };
                if attrs.get(a) == Some(&true) {
                    r.flag(
                        format!("{p}.{name}.{k}"),
                        IssueCode::RequiredFieldNotEditable,
                        format!(
                            "Attribute \"{a}\" is required but {what} on the form, so new CIs of this class cannot be saved in the UI"
                        ),
                    );
                }
            }
        }
        out.layouts.push(l);
    }

    (out, r.issues)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;

    fn doc(v: serde_json::Value) -> UiSettingsDocument {
        serde_json::from_value(v).expect("valid document")
    }

    fn model() -> Model {
        let mut m = Model::default();
        m.classes.insert("server".into(), HashMap::from([("cpu_cores".into(), false), ("serial".into(), true)]));
        m.classes.insert("application".into(), HashMap::new());
        m.statuses.insert("in_service".into());
        m.environments.insert("production".into());
        m
    }

    #[test]
    fn the_empty_document_is_the_default() {
        let d = doc(json!({}));
        assert_eq!(d, UiSettingsDocument::default());
        assert!(d.check().is_empty());
        assert_eq!(resolve(&d, &model()), (d.clone(), vec![]));
    }

    #[test]
    fn navigation_entries_need_their_fields_once() {
        let d = doc(json!({"navigation": {"entries": [
            {"type": "page", "page": "dashboard"},
            {"type": "page", "page": "dashboard"},
            {"type": "class"},
            {"type": "section", "key": "infra", "label": "Infrastructure", "items": [{"classKey": "server"}]},
            {"type": "class", "classKey": "server"},
            {"type": "section", "key": "infra"},
        ]}}));
        let fields: Vec<String> = d.check().into_iter().map(|e| e.field).collect();
        assert_eq!(
            fields,
            [
                "navigation.entries.1.page",
                "navigation.entries.2.classKey",
                "navigation.entries.4.classKey",
                "navigation.entries.5.key",
                "navigation.entries.5.label",
            ]
        );
    }

    #[test]
    fn widgets_and_layouts_follow_their_rules() {
        let d = doc(json!({
            "dashboard": {"widgets": [
                {"id": "a", "type": "saved_search"},
                {"id": "a", "type": "count_by_status", "limit": 5},
            ]},
            "layouts": [{"classKey": "server", "panels": [
                {"key": "p", "label": "P", "fields": ["name", "hostname"]},
                {"key": "p", "label": "Q", "fields": ["hostname"]},
            ], "hiddenFields": ["name"]}],
        }));
        let fields: Vec<String> = d.check().into_iter().map(|e| e.field).collect();
        assert_eq!(
            fields,
            [
                "dashboard.widgets.0.search",
                "dashboard.widgets.1.id",
                "dashboard.widgets.1.limit",
                "layouts.0.panels.1.key",
                "layouts.0.panels.1.fields.0",
                "layouts.0.hiddenFields.0",
            ]
        );
    }

    #[test]
    fn dangling_references_are_dropped_and_reported() {
        let d = doc(json!({
            "navigation": {"entries": [
                {"type": "class", "classKey": "retired"},
                {"type": "section", "key": "s", "label": "S", "items": [{"classKey": "server"}, {"classKey": "gone"}]},
            ]},
            "dashboard": {"widgets": [{"id": "w", "type": "saved_search", "search": {
                "classKeys": ["server", "gone"], "filters": {"statusKeys": ["in_service", "old"]}}}]},
            "listViews": [
                {"classKey": "server", "columns": ["name", "attributes.cpu_cores", "attributes.ram"]},
                {"classKey": "gone", "columns": ["name"]},
            ],
            "layouts": [{"classKey": "server", "hiddenFields": ["attributes.serial", "attributes.nope"]}],
        }));
        assert!(d.check().is_empty());
        let (effective, issues) = resolve(&d, &model());
        let got: Vec<(&str, IssueCode)> = issues.iter().map(|i| (i.path.as_str(), i.code)).collect();
        assert_eq!(
            got,
            [
                ("navigation.entries.0.classKey", IssueCode::UnknownClass),
                ("navigation.entries.1.items.1.classKey", IssueCode::UnknownClass),
                ("dashboard.widgets.0.search.classKeys.1", IssueCode::UnknownClass),
                ("dashboard.widgets.0.search.filters.statusKeys.1", IssueCode::UnknownStatus),
                ("listViews.0.columns.2", IssueCode::UnknownAttribute),
                ("listViews.1.classKey", IssueCode::UnknownClass),
                ("layouts.0.hiddenFields.1", IssueCode::UnknownAttribute),
                ("layouts.0.hiddenFields.0", IssueCode::RequiredFieldNotEditable),
            ]
        );
        assert_eq!(effective.navigation.entries.len(), 1);
        assert_eq!(effective.navigation.entries[0].items.len(), 1);
        assert_eq!(effective.list_views.len(), 1);
        assert_eq!(effective.list_views[0].columns, ["name", "attributes.cpu_cores"]);
        assert_eq!(effective.layouts[0].hidden_fields, ["attributes.serial"]);
        let search = effective.dashboard.widgets.unwrap()[0].search.clone().unwrap();
        assert_eq!(search.class_keys, ["server"]);
        assert_eq!(search.filters.status_keys, ["in_service"]);
    }

    #[test]
    fn builtin_fields_match_the_field_pattern() {
        let re = regex::Regex::new(FIELD_PATTERN).unwrap();
        for f in BUILTIN_FIELDS {
            assert!(re.is_match(f), "{f}");
        }
        assert!(re.is_match("attributes.cpu_cores"));
        assert!(!re.is_match("attributes.Bad"));
        assert!(!re.is_match("serial"));
    }
}
