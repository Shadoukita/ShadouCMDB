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

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa::openapi::schema::{ArrayBuilder, ObjectBuilder, Schema, Type};

use crate::api::route::Check;
use crate::api::schemas::{COLOR_PATTERN, KEY_PATTERN, NOT_BLANK_PATTERN};
pub use crate::data::ui_settings::Model;
use crate::http::error::{FieldError, FieldLocation};

/// CI fields that are not attributes; attributes are `attributes.<key>`.
#[cfg(test)]
pub const BUILTIN_FIELDS: &[&str] =
    &["label", "ident", "class", "criticality", "validFrom", "validUntil", "active", "createdAt", "updatedAt"];

pub const FIELD_PATTERN: &str = "^(label|ident|class|criticality|validFrom|validUntil|active|createdAt|updatedAt|attributes\\.[a-z][a-z0-9_]{0,62})$";

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
fn lookup_filters_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .property_names(Some(string().pattern(Some(KEY_PATTERN))))
        .additional_properties(Some(key_list("Value keys of the list", 500)))
        .max_properties(Some(50))
        .description(Some(
            "Lookup list key -> value keys: CIs holding one of the values in a lookup attribute of that list, for \
             every list given (e.g. {\"status\": [\"in_service\"], \"environment\": [\"production\"]})",
        ))
        .into()
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
        "Columns in display order: built-in fields (label, ident, class, criticality, validFrom, validUntil, active, \
         createdAt, updatedAt) or attributes.<key>",
    )
}
fn panel_fields_schema() -> Schema {
    field_list("Fields in display order (built-in fields or attributes.<key>)")
}
fn hidden_fields_schema() -> Schema {
    field_list(
        "Fields not shown on the detail page or the form. Presentation only: the API still returns them; restrict \
         access with permission profiles",
    )
}
fn read_only_fields_schema() -> Schema {
    field_list(
        "Fields shown but not editable on the form. Presentation only: the API still accepts writes to them; \
         restrict access with permission profiles",
    )
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
    /// CIs per value of a lookup list (`lookupListKey`), e.g. per status
    CountByLookup,
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

/// Sort for an inventory list; `field` is one of the inventory sort fields or
/// `attributes.<key>` (the list's `sort` parameter without the "-")
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiListSort {
    #[schema(
        pattern = "^(label|ident|className|criticality|validFrom|validUntil|createdAt|updatedAt|attributes\\.[a-z][a-z0-9_]{0,62})$"
    )]
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
    #[schema(schema_with = lookup_filters_schema)]
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub lookups: BTreeMap<String, Vec<String>>,
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
    /// count_by_lookup: the lookup list whose values are counted (required for that type)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, pattern = "^[a-z][a-z0-9_]{0,62}$")]
    pub lookup_list_key: Option<String>,
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

/// Grid columns of a section that does not say (and of converted v1 panels).
pub const DEFAULT_COLUMNS: u8 = 3;
/// Columns of a tab's grid, and the most a section's field grid can have.
pub const GRID_COLUMNS: u8 = 12;
/// Core fields of every CI: a layout can move them but never hide them.
pub const CORE_FIELDS: &[&str] = &["ident", "validFrom", "validUntil"];

fn default_columns() -> u8 {
    DEFAULT_COLUMNS
}
fn default_width() -> u8 {
    1
}
fn default_section_width() -> u8 {
    GRID_COLUMNS
}
fn is_false(b: &bool) -> bool {
    !b
}

fn layout_field_schema() -> Schema {
    string().pattern(Some(FIELD_PATTERN)).description(Some("A built-in field or attributes.<key>")).into()
}

/// An entry of a section's grid: a field, or a separator (`separator: true`) that divides the fields
/// into groups. Entries fill the grid row by row in the order given.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiLayoutField {
    /// The field (required, except for a separator, which holds none)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(schema_with = layout_field_schema)]
    pub field: Option<String>,
    /// Grid columns the field spans, at most the section's `columns`. A separator always takes a row of
    /// its own across the whole section: stored as the section's `columns`
    #[serde(default = "default_width")]
    #[schema(minimum = 1, maximum = 12, default = 1)]
    pub width: u8,
    /// A separator: a line across the section, with an optional `label`, instead of a field
    #[serde(default, skip_serializing_if = "is_false")]
    pub separator: bool,
    /// separator: its heading (none: a plain line)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, min_length = 1, max_length = 100, pattern = "\\S")]
    pub label: Option<String>,
}

impl UiLayoutField {
    pub fn field(field: impl Into<String>, width: u8) -> Self {
        Self { field: Some(field.into()), width, separator: false, label: None }
    }
}

/// Longest note text, in characters.
pub const NOTE_MAX_CHARS: usize = 4000;

/// What a section shows. Absent means `fields`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UiSectionKind {
    /// A grid of fields (`fields`)
    #[default]
    Fields,
    /// Static text written by an administrator (`text`): plain text or limited Markdown, never raw HTML
    Note,
    /// The detail page's record details: the CI's class, when it was created and last changed, and the
    /// other bookkeeping fields no field section places
    Record,
    /// The detail page's relationships panel
    Relations,
    /// The detail page's version history panel
    History,
    /// The detail page's audit trail panel
    Audit,
}

impl UiSectionKind {
    /// Built-in panels of the detail page: each at most once per layout. A layout with tabs shows the
    /// record details, the relationships and the audit trail only where it places them; the history has
    /// a tab of its own when the layout does not place it.
    pub fn is_panel(self) -> bool {
        matches!(self, Self::Record | Self::Relations | Self::History | Self::Audit)
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Fields => "fields",
            Self::Note => "note",
            Self::Record => "record",
            Self::Relations => "relations",
            Self::History => "history",
            Self::Audit => "audit",
        }
    }
}

fn is_fields(k: &UiSectionKind) -> bool {
    *k == UiSectionKind::Fields
}

fn section_kind_schema() -> Schema {
    string()
        .enum_values(Some(["fields", "note", "record", "relations", "history", "audit"]))
        .description(Some(
            "What the section shows (absent: fields): fields (a grid of `fields`), note (static `text`), or a built-in panel of the \
             detail page: record (the CI's class, created and last changed, and the other bookkeeping fields no \
             field section places), relations, history or audit. Each panel can be placed once per layout. A \
             layout with tabs shows the record details, the relationships and the audit trail only where it \
             places them, so removing one of these sections hides it; the history has a tab of its own when it \
             is not placed. A layout without tabs shows the built-in arrangement: the fields by attribute \
             group, then the record details and the relationships.",
        ))
        .into()
}

fn note_text_schema() -> Schema {
    string()
        .min_length(Some(1))
        .max_length(Some(NOTE_MAX_CHARS))
        .pattern(Some(NOT_BLANK_PATTERN))
        .description(Some(
            "note: the text (required for that kind). Plain text or limited Markdown (emphasis, lists, links); \
             raw HTML is shown as text, never rendered",
        ))
        .extensions(Some(crate::api::schemas::multiline_extension()))
        .into()
}

/// A section (card) of a tab: a heading and, depending on `kind`, a grid of fields, a note or a built-in
/// panel of the detail page. Sections sit on the tab's grid of 12 columns and fill it row by row in the
/// order given, so two sections of width 6 sit side by side. Below the tablet breakpoint every section
/// takes the full width; sizes are never in pixels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiLayoutSection {
    /// Unique within the layout (across its tabs)
    #[schema(schema_with = key_schema)]
    pub key: String,
    #[schema(min_length = 1, max_length = 100, pattern = "\\S")]
    pub label: String,
    #[serde(default, skip_serializing_if = "is_fields")]
    #[schema(schema_with = section_kind_schema)]
    pub kind: UiSectionKind,
    /// Columns of the section's field grid on a wide screen; narrow screens use fewer
    #[serde(default = "default_columns")]
    #[schema(minimum = 1, maximum = 12, default = 3)]
    pub columns: u8,
    /// Columns of the tab's 12-column grid the section spans (12: the full width)
    #[serde(default = "default_section_width")]
    #[schema(minimum = 1, maximum = 12, default = 12)]
    pub width: u8,
    /// Start a new row of the tab's grid, even if the section would fit next to the previous one
    #[serde(default, skip_serializing_if = "is_false")]
    pub new_row: bool,
    /// Minimum height in field rows (the height of one row of fields); absent: as tall as its content
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, minimum = 1, maximum = 50)]
    pub min_height: Option<u8>,
    /// fields: the grid (other kinds have none)
    #[serde(default)]
    #[schema(max_items = 200)]
    pub fields: Vec<UiLayoutField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(schema_with = note_text_schema)]
    pub text: Option<String>,
    /// Start collapsed on the detail page
    #[serde(default)]
    pub collapsed: bool,
    /// Where the section sits as a window of its tab. Optional on input: a section without one gets one
    /// from its grid position (`width`, `newRow`, `minHeight`) below the other windows; always stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub frame: Option<UiSectionFrame>,
}

/// Narrowest frame, as a fraction of the tab's width (a 12-column grid cell is 1/12).
pub const FRAME_MIN_W: f64 = 0.05;
/// Lowest frame, in px: the section's title bar.
pub const FRAME_MIN_H: u32 = 48;
pub const FRAME_MAX_H: u32 = 4000;
pub const FRAME_MAX_Y: u32 = 100_000;
/// Heights used to turn grid positions into frames: a section's title bar, one row of its field grid (the
/// `minHeight` unit, 3em), the gap between grid rows, a note, the record details and the other built-in panels.
pub const FRAME_HEADER_PX: u32 = 48;
pub const FRAME_ROW_PX: u32 = 48;
pub const FRAME_GAP_PX: u32 = 16;
pub const FRAME_NOTE_PX: u32 = 144;
pub const FRAME_RECORD_PX: u32 = 144;
pub const FRAME_PANEL_PX: u32 = 320;
/// Tolerance for `x + w <= 1`, so that e.g. 11/12 + 1/12 passes.
const FRAME_EPSILON: f64 = 1e-6;

/// A window on a free tab: position and size, and its place in the stacking order. `x` and `w` are
/// fractions of the tab's width, so windows scale with the browser window; `y` and `h` are px from the top
/// of the tab. The window scrolls its own content, so an overlapped window loses nothing.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiSectionFrame {
    /// Left edge, as a fraction of the tab's width (0: the left edge). `x + w` is at most 1.
    #[schema(minimum = 0.0, maximum = 0.95)]
    pub x: f64,
    /// Top edge in px from the top of the tab
    #[schema(minimum = 0, maximum = 100_000)]
    pub y: u32,
    /// Width, as a fraction of the tab's width (1: the full width)
    #[schema(minimum = 0.05, maximum = 1.0)]
    pub w: f64,
    /// Height in px
    #[schema(minimum = 48, maximum = 4000)]
    pub h: u32,
    /// Stacking order: a higher z is drawn on top. Saved as 1..n per tab, in the order given (ties: the
    /// section order).
    #[schema(minimum = 0, maximum = 10_000)]
    pub z: u32,
    /// Smallest height in px the window may be resized to, and its least height when the tab stacks the
    /// windows on a narrow screen; at most `h`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, minimum = 48, maximum = 4000)]
    pub min_h: Option<u32>,
}

/// How a tab arranges its sections. Every stored tab is free; `grid` (and an absent placement) is still
/// accepted and converted to free on save and when stored settings are read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UiTabPlacement {
    /// The earlier 12-column grid: accepted on input, converted to free (each section a window where it
    /// was on the grid)
    #[default]
    Grid,
    /// Every section is a window placed by its `frame`; windows may overlap
    Free,
}

fn is_grid(p: &UiTabPlacement) -> bool {
    *p == UiTabPlacement::Grid
}

fn placement_schema() -> Schema {
    string()
        .enum_values(Some(["grid", "free"]))
        .description(Some(
            "How the tab arranges its sections. Always free when returned: each section is a window placed by \
             its `frame`, and windows may overlap. grid (or absent) is still accepted for older documents and \
             exports and is converted to free: each section becomes a window where it was on the 12-column grid \
             (`width`, `newRow`, `minHeight`). On save, sections without a frame get one from their grid \
             position (below the existing windows), z becomes 1..n and the sections are ordered by y, then x: \
             the reading order, used on narrow screens, in print and by screen readers.",
        ))
        .into()
}

/// A tab of the detail page and the form
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiLayoutTab {
    /// Unique among the layout's tabs
    #[schema(schema_with = key_schema)]
    pub key: String,
    #[schema(min_length = 1, max_length = 100, pattern = "\\S")]
    pub label: String,
    #[serde(default, skip_serializing_if = "is_grid")]
    #[schema(schema_with = placement_schema)]
    pub placement: UiTabPlacement,
    #[serde(default)]
    #[schema(max_items = 50)]
    pub sections: Vec<UiLayoutSection>,
}

/// Rounds a fraction of the tab's width to 4 decimals (0.2 px of a 2000 px tab).
fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

/// Height of a section's frame when it leaves the grid: title bar plus its field rows (at least one, and at
/// least `minHeight`), a note or a built-in panel.
pub fn estimated_height(s: &UiLayoutSection) -> u32 {
    let h = match s.kind {
        UiSectionKind::Fields => {
            let (mut rows, mut col) = (0u32, s.columns);
            for f in &s.fields {
                let w = if f.separator { s.columns.max(1) } else { f.width.clamp(1, s.columns.max(1)) };
                if col + w > s.columns {
                    rows += 1;
                    col = 0;
                }
                col += w;
            }
            FRAME_HEADER_PX + rows.max(u32::from(s.min_height.unwrap_or(1))).max(1) * FRAME_ROW_PX
        }
        UiSectionKind::Note => FRAME_NOTE_PX,
        UiSectionKind::Record => FRAME_RECORD_PX,
        _ => FRAME_PANEL_PX,
    };
    h.clamp(FRAME_MIN_H, FRAME_MAX_H)
}

/// Frames for sections in grid order, starting `top` px down: the 12-column grid's rows (a section
/// starts a new row when it has `newRow` or does not fit), each as tall as its tallest section, with
/// [`FRAME_GAP_PX`] between rows. `z` is 1..n in order.
pub fn grid_frames<'a>(sections: impl IntoIterator<Item = &'a UiLayoutSection>, top: u32) -> Vec<UiSectionFrame> {
    let cols = u32::from(GRID_COLUMNS);
    let (mut col, mut row_y, mut row_h) = (0u32, top, 0u32);
    let mut out = Vec::new();
    for (i, s) in sections.into_iter().enumerate() {
        let w = u32::from(s.width.clamp(1, GRID_COLUMNS));
        if col > 0 && (s.new_row || col + w > cols) {
            row_y = row_y.saturating_add(row_h + FRAME_GAP_PX);
            (col, row_h) = (0, 0);
        }
        let h = estimated_height(s);
        out.push(UiSectionFrame {
            x: round4(f64::from(col) / f64::from(cols)),
            y: row_y.min(FRAME_MAX_Y),
            w: round4(f64::from(w) / f64::from(cols)),
            h,
            z: i as u32 + 1,
            min_h: None,
        });
        col += w;
        row_h = row_h.max(h);
    }
    out
}

impl UiLayoutTab {
    /// The stored form of the tab: free, every section framed (the missing frames from their grid
    /// position, below the existing windows), x and w rounded and within the tab, z 1..n and the sections
    /// in reading order (y, then x). A tab sent as `grid` (older exports, API clients and stored versions
    /// from before free placement was the only one) becomes free the same way: each section a window
    /// where it was on the grid.
    pub fn normalize(&mut self) {
        self.placement = UiTabPlacement::Free;
        for s in &mut self.sections {
            for f in s.fields.iter_mut().filter(|f| f.separator) {
                f.width = s.columns;
            }
        }
        let bottom = self.sections.iter().filter_map(|s| s.frame).map(|f| f.y + f.h).max();
        let top = bottom.map_or(0, |b| b + FRAME_GAP_PX);
        let top_z = self.sections.iter().filter_map(|s| s.frame).map(|f| f.z).max().unwrap_or(0);
        let derived = grid_frames(self.sections.iter().filter(|s| s.frame.is_none()), top);
        let mut derived = derived.into_iter();
        for s in &mut self.sections {
            if s.frame.is_none() {
                let mut f = derived.next().expect("one frame per section without one");
                f.z += top_z;
                s.frame = Some(f);
            }
        }
        // Stacking order: rank by (z, position), so equal z keeps the section order.
        let mut by_z: Vec<usize> = (0..self.sections.len()).collect();
        by_z.sort_by_key(|&i| (self.sections[i].frame.map_or(0, |f| f.z), i));
        for (rank, i) in by_z.into_iter().enumerate() {
            let f = self.sections[i].frame.as_mut().expect("framed above");
            f.z = rank as u32 + 1;
            f.w = round4(f.w).clamp(FRAME_MIN_W, 1.0);
            f.x = round4(f.x.clamp(0.0, 1.0 - f.w));
            if f.x + f.w > 1.0 + FRAME_EPSILON {
                f.x = round4(f.x - 0.0001);
            }
        }
        self.sections.sort_by(|a, b| {
            let (a, b) = (a.frame.expect("framed"), b.frame.expect("framed"));
            a.y.cmp(&b.y).then(a.x.total_cmp(&b.x))
        });
    }
}

/// A panel of the layout format before tabs (v1). Accepted on input and converted to a section of one
/// "General" tab; never returned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiLayoutPanel {
    #[schema(schema_with = key_schema)]
    pub key: String,
    #[schema(min_length = 1, max_length = 100, pattern = "\\S")]
    pub label: String,
    #[schema(schema_with = panel_fields_schema)]
    #[serde(default)]
    pub fields: Vec<String>,
    #[serde(default)]
    pub collapsed: bool,
}

/// Detail page and form layout of one class (layout format v2): tabs of
/// sections, each a grid of fields with a width. Fields the tabs do not place
/// (and that are not hidden) follow at the end of the first tab, grouped by
/// attribute group; so do attributes added to the class later.
///
/// The layout itself lives in a template (`layoutTemplates`): `templateKey` is the class's default
/// template, which its CIs use unless they have their own layout. The settings the API returns carry the
/// template's tabs, hidden and read-only fields here as well. Tabs or fields sent here with a
/// `templateKey` replace that template's layout (every class using it changes); sent without one, they
/// become a new template "<class name> layout" that the class then uses, as migration 0042 did with the
/// layouts stored before templates. A class without an entry uses the Standard template.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields, try_from = "ClassLayoutInput")]
pub struct UiClassLayout {
    #[schema(schema_with = key_schema)]
    pub class_key: String,
    /// The class's default layout template (`layoutTemplates[].key`). Always set in stored settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, pattern = "^[a-z][a-z0-9_]{0,62}$")]
    pub template_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schema(max_items = 20)]
    pub tabs: Vec<UiLayoutTab>,
    #[schema(schema_with = hidden_fields_schema)]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden_fields: Vec<String>,
    #[schema(schema_with = read_only_fields_schema)]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_only_fields: Vec<String>,
    /// Layout format v1, still accepted (older exports, API clients and saved versions): converted to
    /// one "General" tab with a section per panel and never returned. Send `tabs` instead.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schema(max_items = 50, deprecated, write_only)]
    pub panels: Vec<UiLayoutPanel>,
}

/// What `UiClassLayout` is read from: either format, converted to v2.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClassLayoutInput {
    class_key: String,
    #[serde(default)]
    template_key: Option<String>,
    #[serde(default)]
    tabs: Vec<UiLayoutTab>,
    #[serde(default)]
    hidden_fields: Vec<String>,
    #[serde(default)]
    read_only_fields: Vec<String>,
    #[serde(default)]
    panels: Vec<UiLayoutPanel>,
}

impl TryFrom<ClassLayoutInput> for UiClassLayout {
    type Error = String;

    fn try_from(l: ClassLayoutInput) -> Result<Self, String> {
        if !l.panels.is_empty() && !l.tabs.is_empty() {
            return Err("custom|Send tabs, or panels in the older layout format, not both".into());
        }
        let tabs = if l.panels.is_empty() { l.tabs } else { convert_panels(l.panels) };
        Ok(UiClassLayout {
            class_key: l.class_key,
            template_key: l.template_key,
            tabs,
            hidden_fields: l.hidden_fields,
            read_only_fields: l.read_only_fields,
            panels: Vec::new(),
        })
    }
}

/// Layout format v1 -> v2: the panels become the sections of one "General" tab, each field one column
/// wide on a grid of [`DEFAULT_COLUMNS`]. Migration 0017 does the same to the stored settings.
pub fn convert_panels(panels: Vec<UiLayoutPanel>) -> Vec<UiLayoutTab> {
    let sections = panels
        .into_iter()
        .map(|p| UiLayoutSection {
            key: p.key,
            label: p.label,
            kind: UiSectionKind::Fields,
            columns: DEFAULT_COLUMNS,
            width: GRID_COLUMNS,
            new_row: false,
            min_height: None,
            fields: p.fields.into_iter().map(|field| UiLayoutField::field(field, 1)).collect(),
            text: None,
            collapsed: p.collapsed,
            frame: None,
        })
        .collect();
    vec![UiLayoutTab { key: "general".into(), label: "General".into(), placement: UiTabPlacement::Grid, sections }]
}

// ---------------------------------------------------------------------------
// Layout templates
// ---------------------------------------------------------------------------

/// Key of the built-in template every class without a layout entry uses. Saving settings without it
/// adds it again (with no tabs: the detail page's built-in arrangement); it can be renamed.
pub const STANDARD_TEMPLATE: &str = "standard";
pub const STANDARD_TEMPLATE_NAME: &str = "Standard";
pub const TEMPLATE_NAME_MAX: usize = 100;

/// Largest layout of a template or a CI's own, as stored (normalised) JSON. The database check on
/// `ci_layout_overrides.layout` (512 KiB, compressed) stays as the backstop.
pub const LAYOUT_MAX_BYTES: usize = 256 * 1024;

/// A detail page and form layout on its own, without a class: the body of a template and of a CI's own
/// layout. The same tabs, hidden and read-only fields as a class layout; attribute fields are resolved
/// against the class of the CI that shows it, and ones the class does not have are left out. At most 256 KiB
/// as JSON, counted after the server fills in section frames (400 VALIDATION_ERROR, code `too_large`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct UiLayout {
    #[schema(max_items = 20)]
    pub tabs: Vec<UiLayoutTab>,
    #[schema(schema_with = hidden_fields_schema)]
    pub hidden_fields: Vec<String>,
    #[schema(schema_with = read_only_fields_schema)]
    pub read_only_fields: Vec<String>,
}

impl UiLayout {
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty() && self.hidden_fields.is_empty() && self.read_only_fields.is_empty()
    }

    /// The stored form: every tab normalised (see [`UiLayoutTab::normalize`]).
    pub fn normalized(mut self) -> Self {
        for tab in &mut self.tabs {
            tab.normalize();
        }
        self
    }

    /// Structural problems, with field paths below `prefix` (e.g. "layout"), and the size limit.
    pub fn problems(&self, prefix: &str) -> Vec<FieldError> {
        let mut e = layout_problems(prefix, &self.tabs, &self.hidden_fields);
        if serde_json::to_vec(&self.clone().normalized()).map_or(usize::MAX, |v| v.len()) > LAYOUT_MAX_BYTES {
            e.push(FieldError {
                location: FieldLocation::Body,
                field: prefix.into(),
                message: format!("The layout is larger than {} KiB", LAYOUT_MAX_BYTES / 1024),
                code: "too_large".into(),
            });
        }
        e
    }
}

impl UiClassLayout {
    /// The tabs, hidden and read-only fields sent or expanded on the class.
    pub fn content(&self) -> UiLayout {
        UiLayout {
            tabs: self.tabs.clone(),
            hidden_fields: self.hidden_fields.clone(),
            read_only_fields: self.read_only_fields.clone(),
        }
    }

    fn set_content(&mut self, l: UiLayout) {
        (self.tabs, self.hidden_fields, self.read_only_fields) = (l.tabs, l.hidden_fields, l.read_only_fields);
    }

    /// The class's default template; Standard when not set.
    pub fn template(&self) -> &str {
        self.template_key.as_deref().unwrap_or(STANDARD_TEMPLATE)
    }
}

/// A named detail page and form layout. Each class has one as its default (`layouts[].templateKey`,
/// Standard when the class has no entry); a CI can use another one or a layout of its own
/// (`/api/v1/configuration-items/{id}/layout`). A template that a class or a live CI uses cannot be
/// removed (409 CONFLICT naming them); renaming is always allowed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiLayoutTemplate {
    /// Stable machine key, lower_snake_case and unique among the templates: what classes and CIs refer
    /// to. Rename a template by changing its `name`; a new key is a new template.
    #[schema(schema_with = key_schema)]
    pub key: String,
    /// Unique among the templates, ignoring case
    #[schema(min_length = 1, max_length = 100, pattern = "\\S")]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, min_length = 1, max_length = 500, pattern = "\\S")]
    pub description: Option<String>,
    #[serde(default)]
    pub layout: UiLayout,
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
    /// At most one per class: the class's default template (see `UiClassLayout`)
    #[schema(max_items = 1000)]
    pub layouts: Vec<UiClassLayout>,
    /// Named layouts that classes and CIs use. Stored settings always hold the Standard template
    /// (key "standard"); settings without templates (older exports and versions) are converted on save.
    #[schema(max_items = 1000)]
    pub layout_templates: Vec<UiLayoutTemplate>,
    /// Layout format of the document; always 3 when returned. Send back what was returned. A document
    /// without it (an older configuration export, an older settings version being restored, an API client
    /// written before format 3) is from before layouts placed the record details and the relationships
    /// explicitly: when it is saved, every layout that has tabs and does not place them gets a "Record" and
    /// a "Relationships" section at the end of its first tab, as migration 0048 did with the stored
    /// settings, so the detail page shows what it showed before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, minimum = 2, maximum = 3)]
    pub layout_format: Option<u8>,
}

/// Layout format of the documents the API stores and returns (`layoutFormat`): layouts with tabs show the
/// record and relations panels only where they place them.
pub const LAYOUT_FORMAT: u8 = 3;

// ---------------------------------------------------------------------------
// Structural rules (independent of the data model)
// ---------------------------------------------------------------------------

impl Check for UiSettingsDocument {
    fn check(&self) -> Vec<FieldError> {
        self.problems("")
    }
}

impl UiSettingsDocument {
    /// In the current layout format: a document from before format 3 gets the record and relations panels
    /// placed where the detail page showed them (see [`place_implicit_panels`]).
    pub fn upgraded(mut self) -> Self {
        if self.layout_format.is_none_or(|f| f < LAYOUT_FORMAT) {
            let templates = self.layout_templates.iter_mut().map(|t| &mut t.layout.tabs);
            for tabs in self.layouts.iter_mut().map(|l| &mut l.tabs).chain(templates) {
                place_implicit_panels(tabs);
            }
        }
        self.layout_format = Some(LAYOUT_FORMAT);
        self
    }

    /// The form in which a valid document is stored: every layout tab normalised (see
    /// [`UiLayoutTab::normalize`]).
    pub fn normalized(mut self) -> Self {
        let templates = self.layout_templates.iter_mut().flat_map(|t| &mut t.layout.tabs);
        for tab in self.layouts.iter_mut().flat_map(|l| &mut l.tabs).chain(templates) {
            tab.normalize();
        }
        self
    }

    /// Cross-field problems, with field paths below `prefix` (e.g. "settings.").
    pub fn problems(&self, prefix: &str) -> Vec<FieldError> {
        let mut e = Vec::new();
        let at = |p: String| format!("{prefix}{p}");
        if self.layout_format.is_some_and(|f| !(2..=LAYOUT_FORMAT).contains(&f)) {
            e.push(custom(at("layoutFormat".into()), format!("2 or {LAYOUT_FORMAT}")));
        }

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
                match (w.kind, &w.lookup_list_key) {
                    (UiWidgetType::CountByLookup, None) => {
                        e.push(custom(at(format!("{p}.lookupListKey")), "Required for count_by_lookup widgets"))
                    }
                    (UiWidgetType::CountByLookup, _) | (_, None) => {}
                    (_, Some(_)) => {
                        e.push(custom(at(format!("{p}.lookupListKey")), "Only allowed for count_by_lookup widgets"))
                    }
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
            e.extend(layout_problems(&at(p), &l.tabs, &l.hidden_fields));
        }

        let (mut keys, mut names) = (HashSet::new(), HashSet::new());
        for (i, t) in self.layout_templates.iter().enumerate() {
            let p = format!("layoutTemplates.{i}");
            if !keys.insert(t.key.as_str()) {
                e.push(custom(at(format!("{p}.key")), "Template keys must be unique"));
            }
            if !names.insert(t.name.trim().to_lowercase()) {
                e.push(custom(at(format!("{p}.name")), "Another template has this name"));
            }
            e.extend(t.layout.problems(&at(format!("{p}.layout"))));
        }
        e
    }

    pub fn template(&self, key: &str) -> Option<&UiLayoutTemplate> {
        self.layout_templates.iter().find(|t| t.key == key)
    }

    /// The default template of a class: its entry's, else Standard.
    pub fn class_template(&self, class_key: &str) -> &str {
        self.layouts.iter().find(|l| l.class_key == class_key).map_or(STANDARD_TEMPLATE, |l| l.template())
    }

    /// The form the API returns: [`Self::expanded`], with the Standard template when the stored settings
    /// do not have it yet (settings never saved since templates came).
    pub fn returned(self) -> Self {
        self.with_standard().expanded()
    }

    /// With the Standard template, which settings saved before templates do not have yet.
    pub fn with_standard(mut self) -> Self {
        if self.template(STANDARD_TEMPLATE).is_none() {
            let names = self.layout_templates.iter().map(|t| t.name.trim().to_lowercase()).collect();
            self.layout_templates.insert(0, standard_template(&names, UiLayout::default()));
        }
        self
    }

    /// Each class layout carries its template's tabs, hidden and read-only fields
    /// (see [`UiClassLayout`]). Layouts without a template (settings saved before templates) stay as they are.
    pub fn expanded(mut self) -> Self {
        for l in &mut self.layouts {
            if let Some(t) = l.template_key.as_deref().and_then(|k| self.layout_templates.iter().find(|t| t.key == k)) {
                let content = t.layout.clone();
                l.set_content(content);
            }
        }
        self
    }
}

/// Adds what a layout with tabs showed before layout format 3 without placing it: a "Record" section
/// (kind record) and then a "Relationships" section (kind relations) at the end of the first tab, each
/// unless the layout places that panel already. They have no frame, so they go below the tab's windows.
/// A layout without tabs is left alone: it shows the built-in arrangement, which has both. Migration 0048
/// does the same to the stored layouts.
pub fn place_implicit_panels(tabs: &mut [UiLayoutTab]) {
    let sections = || tabs.iter().flat_map(|t| &t.sections);
    let kinds: HashSet<UiSectionKind> = sections().map(|s| s.kind).collect();
    let mut keys: HashSet<String> = sections().map(|s| s.key.clone()).collect();
    let Some(first) = tabs.first_mut() else { return };
    for (kind, label) in [(UiSectionKind::Record, "Record"), (UiSectionKind::Relations, "Relationships")] {
        if kinds.contains(&kind) {
            continue;
        }
        let key = free_key(kind.as_str(), &keys);
        keys.insert(key.clone());
        first.sections.push(UiLayoutSection {
            key,
            label: label.into(),
            kind,
            columns: DEFAULT_COLUMNS,
            width: GRID_COLUMNS,
            new_row: false,
            min_height: None,
            fields: Vec::new(),
            text: None,
            collapsed: false,
            frame: None,
        });
    }
}

/// Structural problems of a layout's tabs and hidden fields, with field paths below `base` (e.g.
/// "settings.layouts.0"; the tabs are at `{base}.tabs`).
fn layout_problems(base: &str, tabs: &[UiLayoutTab], hidden_fields: &[String]) -> Vec<FieldError> {
    let mut e = Vec::new();
    let p = base;
    let mut tab_keys = HashSet::new();
    let mut sections = HashSet::new();
    let mut placed = HashSet::new();
    let mut panels = HashSet::new();
    for (t, tab) in tabs.iter().enumerate() {
        let pt = format!("{p}.tabs.{t}");
        if !tab_keys.insert(tab.key.as_str()) {
            e.push(custom(format!("{pt}.key"), "Tab keys must be unique in a layout"));
        }
        for (j, s) in tab.sections.iter().enumerate() {
            let ps = format!("{pt}.sections.{j}");
            if !sections.insert(s.key.as_str()) {
                e.push(custom(format!("{ps}.key"), "Section keys must be unique in a layout"));
            }
            if s.kind.is_panel() && !panels.insert(s.kind) {
                e.push(custom(
                    format!("{ps}.kind"),
                    format!("The {} panel can be placed once in a layout", s.kind.as_str()),
                ));
            }
            if s.kind != UiSectionKind::Fields && !s.fields.is_empty() {
                e.push(custom(format!("{ps}.fields"), "Only sections of kind fields hold fields"));
            }
            match (&s.text, s.kind) {
                (None, UiSectionKind::Note) => e.push(custom(format!("{ps}.text"), "Required for note sections")),
                (Some(text), UiSectionKind::Note) if text.trim().is_empty() => {
                    e.push(custom(format!("{ps}.text"), "Required for note sections"))
                }
                (Some(text), UiSectionKind::Note) if text.chars().count() > NOTE_MAX_CHARS => {
                    e.push(custom(format!("{ps}.text"), format!("At most {NOTE_MAX_CHARS} characters")))
                }
                (Some(_), k) if k != UiSectionKind::Note => {
                    e.push(custom(format!("{ps}.text"), "Only allowed for note sections"))
                }
                _ => {}
            }
            if let Some(f) = &s.frame {
                let pf = format!("{ps}.frame");
                if !f.x.is_finite() || !f.w.is_finite() {
                    e.push(custom(pf.clone(), "x and w must be finite numbers"));
                } else if f.w < FRAME_MIN_W - FRAME_EPSILON {
                    e.push(custom(format!("{pf}.w"), format!("At least {FRAME_MIN_W} of the tab's width")));
                } else if f.x + f.w > 1.0 + FRAME_EPSILON {
                    e.push(custom(format!("{pf}.w"), "The window must end inside the tab: x + w must be at most 1"));
                }
                if !(FRAME_MIN_H..=FRAME_MAX_H).contains(&f.h) {
                    e.push(custom(format!("{pf}.h"), format!("Between {FRAME_MIN_H} and {FRAME_MAX_H} px")));
                }
                if f.min_h.is_some_and(|m| m > f.h) {
                    e.push(custom(format!("{pf}.minH"), "At most the window's height h"));
                }
            }
            for (k, f) in s.fields.iter().enumerate() {
                let pf = format!("{ps}.fields.{k}");
                match (&f.field, f.separator) {
                    (Some(_), true) => e.push(custom(format!("{pf}.field"), "A separator holds no field")),
                    (None, false) => e.push(custom(format!("{pf}.field"), "Required, except for separators")),
                    (Some(field), false) if !placed.insert(field.as_str()) => {
                        e.push(custom(format!("{pf}.field"), "A field can be placed once only"))
                    }
                    _ => {}
                }
                if !f.separator && f.label.is_some() {
                    e.push(custom(format!("{pf}.label"), "Only allowed for separators"));
                }
                if f.label.as_deref().is_some_and(|l| l.trim().is_empty() || l.chars().count() > 100) {
                    e.push(custom(format!("{pf}.label"), "1 to 100 characters, not blank"));
                }
                if !f.separator && f.width > s.columns {
                    e.push(custom(
                        format!("{ps}.fields.{k}.width"),
                        format!("At most the section's {} column(s)", s.columns),
                    ));
                }
            }
        }
    }
    for (k, f) in hidden_fields.iter().enumerate() {
        if CORE_FIELDS.contains(&f.as_str()) {
            e.push(custom(
                format!("{p}.hiddenFields.{k}"),
                "Ident, valid from and valid until belong to every CI: move them, but they cannot be hidden",
            ));
        }
    }
    e
}

// ---------------------------------------------------------------------------
// Class layouts -> templates (the stored form)
// ---------------------------------------------------------------------------

/// A template key for `class_key` that no template in `taken` has: the class key, else with `_2`, `_3`...
/// Migration 0042 picks keys the same way.
fn free_key(class_key: &str, taken: &HashSet<String>) -> String {
    if !taken.contains(class_key) {
        return class_key.to_owned();
    }
    (2..)
        .map(|n| {
            let suffix = format!("_{n}");
            let stem: String = class_key.chars().take(63 - suffix.len()).collect();
            format!("{stem}{suffix}")
        })
        .find(|k| !taken.contains(k))
        .expect("a free key")
}

/// "<class name> layout", unique among `taken` (lower case) by " (2)", " (3)"... and at most
/// [`TEMPLATE_NAME_MAX`] characters. Migration 0042 picks names the same way.
fn free_name(class_name: &str, taken: &HashSet<String>) -> String {
    (1..)
        .map(|n| {
            let suffix = if n == 1 { " layout".to_owned() } else { format!(" layout ({n})") };
            let stem: String = class_name.trim().chars().take(TEMPLATE_NAME_MAX - suffix.chars().count()).collect();
            format!("{}{suffix}", stem.trim_end())
        })
        .find(|name| !taken.contains(&name.to_lowercase()))
        .expect("a free name")
}

/// The Standard template, named "Standard" unless another template has that name ("Standard (2)"...).
fn standard_template(taken: &HashSet<String>, layout: UiLayout) -> UiLayoutTemplate {
    let name = (1..)
        .map(|n| if n == 1 { STANDARD_TEMPLATE_NAME.to_owned() } else { format!("{STANDARD_TEMPLATE_NAME} ({n})") })
        .find(|n| !taken.contains(&n.to_lowercase()))
        .expect("a free name");
    UiLayoutTemplate { key: STANDARD_TEMPLATE.into(), name, description: None, layout }
}

/// The stored form of a (normalised) document, given the templates of the version it replaces (`previous`)
/// and the class names (key -> name, for new templates):
///
/// * a class layout without `templateKey` that has tabs or fields becomes a new template
///   "<class name> layout" (key: the class key) that the class uses; one without anything uses Standard;
/// * tabs or fields sent with a `templateKey` replace that template's layout, unless they are the layout it
///   has (now or in `previous`), as when the settings the API returned are sent back;
/// * class layouts keep only `classKey` and `templateKey`;
/// * the Standard template is added when missing, as it is in `previous` (an administrator who sends only
///   the templates they edit keeps Standard's layout; GH#521), else empty.
///
/// A class layout naming a template that does not exist, and two different layouts sent for one
/// template, are refused.
pub fn contract(
    mut doc: UiSettingsDocument,
    previous: &[UiLayoutTemplate],
    class_names: &HashMap<String, String>,
) -> Result<UiSettingsDocument, Vec<FieldError>> {
    let mut errors = Vec::new();
    let mut keys: HashSet<String> = doc.layout_templates.iter().map(|t| t.key.clone()).collect();
    let mut names: HashSet<String> = doc.layout_templates.iter().map(|t| t.name.trim().to_lowercase()).collect();
    if !keys.contains(STANDARD_TEMPLATE) {
        // Before the new templates, so that none takes its key.
        keys.insert(STANDARD_TEMPLATE.into());
    }
    let sent: HashMap<String, UiLayout> =
        doc.layout_templates.iter().map(|t| (t.key.clone(), t.layout.clone())).collect();
    // Standard when `doc` leaves it out: as in the version it replaces.
    let kept_standard = previous.iter().find(|t| t.key == STANDARD_TEMPLATE);
    let empty = UiLayout::default();
    let standard_layout = kept_standard.map_or(&empty, |t| &t.layout);
    let mut edited: HashMap<String, UiLayout> = HashMap::new();
    let mut created = Vec::new();
    for (i, l) in doc.layouts.iter_mut().enumerate() {
        let content = l.content();
        l.set_content(UiLayout::default());
        let Some(key) = l.template_key.clone() else {
            if content.is_empty() {
                l.template_key = Some(STANDARD_TEMPLATE.into());
                continue;
            }
            let key = free_key(&l.class_key, &keys);
            let class_name = class_names.get(&l.class_key).map_or(l.class_key.as_str(), String::as_str);
            let name = free_name(class_name, &names);
            keys.insert(key.clone());
            names.insert(name.to_lowercase());
            created.push(UiLayoutTemplate { key: key.clone(), name, description: None, layout: content });
            l.template_key = Some(key);
            continue;
        };
        let Some(current) = sent.get(&key).or((key == STANDARD_TEMPLATE).then_some(standard_layout)) else {
            errors.push(custom(
                format!("settings.layouts.{i}.templateKey"),
                format!("No layout template has the key \"{key}\""),
            ));
            continue;
        };
        let before = previous.iter().find(|t| t.key == key).map(|t| &t.layout);
        if content.is_empty() || &content == current || before == Some(&content) {
            continue;
        }
        if before.is_some_and(|b| b != current) {
            errors.push(custom(
                format!("settings.layouts.{i}.tabs"),
                format!("Template \"{key}\" was changed in layoutTemplates as well; send its layout in one place"),
            ));
            continue;
        }
        match edited.get(&key) {
            Some(other) if *other != content => errors.push(custom(
                format!("settings.layouts.{i}.tabs"),
                format!("Another class using template \"{key}\" sent a different layout for it"),
            )),
            Some(_) => {}
            None => {
                edited.insert(key, content);
            }
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    for t in &mut doc.layout_templates {
        if let Some(l) = edited.remove(&t.key) {
            t.layout = l;
        }
    }
    if !doc.layout_templates.iter().any(|t| t.key == STANDARD_TEMPLATE) {
        let layout = edited.remove(STANDARD_TEMPLATE).unwrap_or_else(|| standard_layout.clone());
        let mut standard = standard_template(&names, layout);
        if let Some(kept) = kept_standard {
            standard.description = kept.description.clone();
            if !names.contains(&kept.name.trim().to_lowercase()) {
                standard.name = kept.name.clone();
            }
        }
        doc.layout_templates.insert(0, standard);
    }
    doc.layout_templates.extend(created);
    Ok(doc)
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
    /// The lookup list does not exist; the filter or widget setting is ignored
    UnknownLookupList,
    /// The value is not in the lookup list; it is ignored
    UnknownLookupValue,
    /// A required attribute is hidden or read-only on the form: CIs of the class cannot be created in the UI (kept, only flagged)
    RequiredFieldNotEditable,
    /// A core field (ident, validFrom, validUntil) is hidden, e.g. in a restored older version; it is shown anyway
    CoreFieldHidden,
    /// The layout template chosen for a CI does not exist; the class's default is shown
    UnknownTemplate,
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

    fn list_ok(&mut self, path: String, key: &str) -> bool {
        if self.model.lookups.contains_key(key) {
            return true;
        }
        self.flag(path, IssueCode::UnknownLookupList, format!("Lookup list \"{key}\" does not exist"));
        false
    }

    /// Keeps the lists and values that exist.
    fn filters(&mut self, path: &str, f: &UiListFilters) -> UiListFilters {
        let mut lookups = BTreeMap::new();
        for (list, values) in &f.lookups {
            let p = format!("{path}.lookups.{list}");
            if !self.list_ok(p.clone(), list) {
                continue;
            }
            let mut kept = Vec::new();
            for (i, v) in values.iter().enumerate() {
                if self.model.lookups[list].contains(v) {
                    kept.push(v.clone());
                } else {
                    self.flag(
                        format!("{p}.{i}"),
                        IssueCode::UnknownLookupValue,
                        format!("\"{v}\" is not a value of lookup list \"{list}\""),
                    );
                }
            }
            if !kept.is_empty() {
                lookups.insert(list.clone(), kept);
            }
        }
        UiListFilters { q: f.q.clone(), lookups }
    }

    /// Keeps a sort on a built-in field, or on an attribute every one of the classes has.
    fn sort(&mut self, path: &str, classes: &[String], sort: &Option<UiListSort>) -> Option<UiListSort> {
        let s = sort.as_ref()?;
        let Some(a) = s.field.strip_prefix(ATTRIBUTE_PREFIX) else { return Some(s.clone()) };
        match classes.iter().find(|c| !self.model.classes.get(*c).is_some_and(|attrs| attrs.contains_key(a))) {
            None if !classes.is_empty() => Some(s.clone()),
            missing => {
                let message = match missing {
                    Some(c) => format!("Attribute \"{a}\" is not defined on class \"{c}\"; the list sorts by label"),
                    None => format!("Sorting by attribute \"{a}\" needs a class; the list sorts by label"),
                };
                self.flag(format!("{path}.field"), IssueCode::UnknownAttribute, message);
                None
            }
        }
    }

    /// Whether `f` is a built-in field or an attribute of the class.
    fn field_ok(&mut self, path: String, class: &str, f: &str) -> bool {
        match f.strip_prefix(ATTRIBUTE_PREFIX) {
            Some(a) if !self.model.classes[class].contains_key(a) => {
                self.flag(
                    path,
                    IssueCode::UnknownAttribute,
                    format!("Attribute \"{a}\" is not defined on class \"{class}\""),
                );
                false
            }
            _ => true,
        }
    }

    /// Keeps built-in fields and attributes of the class.
    fn fields(&mut self, path: &str, class: &str, fields: &[String]) -> Vec<String> {
        (fields.iter().enumerate())
            .filter(|(i, f)| self.field_ok(format!("{path}.{i}"), class, f))
            .map(|(_, f)| f.clone())
            .collect()
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
            if let Some(list) = &w.lookup_list_key {
                r.list_ok(format!("{p}.lookupListKey"), list);
            }
            if let Some(s) = &w.search {
                let class_keys = r.classes(&format!("{p}.search.classKeys"), &s.class_keys);
                let filters = r.filters(&format!("{p}.search.filters"), &s.filters);
                let sort = r.sort(&format!("{p}.search.sort"), &class_keys, &s.sort);
                w.search = Some(UiSavedSearch { class_keys, filters, sort, ..s.clone() });
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
        v.default_sort = r.sort(&format!("{p}.defaultSort"), std::slice::from_ref(&v.class_key), &v.default_sort);
        out.list_views.push(v);
    }

    out.layouts = Vec::new();
    for (i, l) in doc.layouts.iter().enumerate() {
        let p = format!("layouts.{i}");
        if !r.class_ok(format!("{p}.classKey"), &l.class_key) {
            continue;
        }
        let mut l = l.clone();
        for (t, tab) in l.tabs.iter_mut().enumerate() {
            for (j, s) in tab.sections.iter_mut().enumerate() {
                let ps = format!("{p}.tabs.{t}.sections.{j}.fields");
                // Separators are kept; paths count every entry, as stored.
                let kept: Vec<bool> = (s.fields.iter().enumerate())
                    .map(|(k, f)| {
                        f.field.as_ref().is_none_or(|name| r.field_ok(format!("{ps}.{k}"), &l.class_key, name))
                    })
                    .collect();
                let mut keep = kept.into_iter();
                s.fields.retain(|_| keep.next().unwrap_or(true));
            }
        }
        for (k, f) in l.hidden_fields.iter().enumerate() {
            if CORE_FIELDS.contains(&f.as_str()) {
                r.flag(
                    format!("{p}.hiddenFields.{k}"),
                    IssueCode::CoreFieldHidden,
                    format!("\"{f}\" belongs to every CI and cannot be hidden; it is shown"),
                );
            }
        }
        l.hidden_fields.retain(|f| !CORE_FIELDS.contains(&f.as_str()));
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

/// One layout as a CI of `class_key` shows it: attribute fields the class does not have left out, and
/// what was left out or is worth a look, with paths below "layout".
pub fn resolve_layout(layout: &UiLayout, class_key: &str, model: &Model) -> (UiLayout, Vec<Issue>) {
    if !model.classes.contains_key(class_key) {
        return (layout.clone(), Vec::new());
    }
    let mut one = UiClassLayout {
        class_key: class_key.into(),
        template_key: None,
        tabs: Vec::new(),
        hidden_fields: Vec::new(),
        read_only_fields: Vec::new(),
        panels: Vec::new(),
    };
    one.set_content(layout.clone());
    let doc = UiSettingsDocument { layouts: vec![one], ..Default::default() };
    let (out, issues) = resolve(&doc, model);
    let issues = issues.into_iter().map(|i| Issue { path: i.path.replacen("layouts.0", "layout", 1), ..i }).collect();
    (out.layouts.into_iter().next().map(|l| l.content()).unwrap_or_default(), issues)
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
        m.lookups.insert("status".into(), ["in_service".to_owned()].into());
        m.lookups.insert("environment".into(), ["production".to_owned()].into());
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
                {"id": "a", "type": "count_by_lookup", "limit": 5},
                {"id": "b", "type": "count_by_class", "lookupListKey": "status"},
            ]},
            "layouts": [{"classKey": "server", "tabs": [
                {"key": "main", "label": "Main", "sections": [
                    {"key": "p", "label": "P", "columns": 2, "fields": [
                        {"field": "label"}, {"field": "attributes.hostname", "width": 3}]},
                ]},
                {"key": "main", "label": "Other", "sections": [
                    {"key": "p", "label": "Q", "fields": [{"field": "attributes.hostname"}]},
                ]},
            ], "hiddenFields": ["ident", "attributes.notes"]}],
        }));
        let fields: Vec<String> = d.check().into_iter().map(|e| e.field).collect();
        assert_eq!(
            fields,
            [
                "dashboard.widgets.0.search",
                "dashboard.widgets.1.id",
                "dashboard.widgets.1.limit",
                "dashboard.widgets.1.lookupListKey",
                "dashboard.widgets.2.lookupListKey",
                "layouts.0.tabs.0.sections.0.fields.1.width",
                "layouts.0.tabs.1.key",
                "layouts.0.tabs.1.sections.0.key",
                "layouts.0.tabs.1.sections.0.fields.0.field",
                "layouts.0.hiddenFields.0",
            ]
        );
    }

    #[test]
    fn layout_defaults_fill_in_columns_and_widths() {
        let d = doc(json!({"layouts": [{"classKey": "server", "tabs": [
            {"key": "t", "label": "T", "sections": [{"key": "s", "label": "S", "fields": [{"field": "ident"}]}]},
        ]}]}));
        let s = &d.layouts[0].tabs[0].sections[0];
        assert_eq!((s.columns, s.fields[0].width, s.collapsed), (DEFAULT_COLUMNS, 1, false));
        assert!(d.check().is_empty());
    }

    #[test]
    fn sections_sit_on_a_twelve_column_grid() {
        let d = doc(json!({"layouts": [{"classKey": "server", "tabs": [{"key": "t", "label": "T", "sections": [
            {"key": "a", "label": "A", "width": 6, "columns": 12, "minHeight": 4,
             "fields": [{"field": "ident", "width": 12}]},
            {"key": "b", "label": "B", "width": 6, "columns": 5, "fields": [{"field": "label", "width": 5}]},
            {"key": "c", "label": "C", "width": 4, "newRow": true},
            {"key": "d", "label": "D", "columns": 6, "fields": [{"field": "validFrom", "width": 7}]},
        ]}]}]}));
        let s = &d.layouts[0].tabs[0].sections;
        assert_eq!((s[0].width, s[0].new_row, s[0].min_height), (6, false, Some(4)));
        assert_eq!((s[2].width, s[2].new_row, s[2].min_height), (4, true, None));
        assert_eq!(s[3].width, GRID_COLUMNS);
        let fields: Vec<String> = d.check().into_iter().map(|e| e.field).collect();
        assert_eq!(fields, ["layouts.0.tabs.0.sections.3.fields.0.width"]);
        // Only what differs from the default is written for the optional keys.
        let out = serde_json::to_value(&d).unwrap();
        let sections = &out["layouts"][0]["tabs"][0]["sections"];
        assert_eq!((&sections[0]["minHeight"], &sections[2]["newRow"]), (&json!(4), &json!(true)));
        assert!(sections[1].get("newRow").is_none() && sections[1].get("minHeight").is_none(), "{out}");
        assert_eq!(doc(out), d);
    }

    /// A layout saved before the grid (columns and widths 1-4, no section width) reads as full-width
    /// sections stacked in order, exactly as it rendered before, and stays the same when saved again.
    #[test]
    fn layouts_without_section_widths_keep_their_meaning() {
        let old = json!({"layouts": [{"classKey": "server", "tabs": [{"key": "general", "label": "General",
        "sections": [
            {"key": "main", "label": "Main", "columns": 4, "collapsed": false,
             "fields": [{"field": "ident", "width": 4}, {"field": "attributes.cpu_cores", "width": 1}]},
            {"key": "hw", "label": "Hardware", "fields": [{"field": "validFrom"}]},
        ]}]}]});
        let d = doc(old.clone());
        assert!(d.check().is_empty());
        let s = &d.layouts[0].tabs[0].sections;
        for section in s {
            assert_eq!((section.width, section.new_row, section.min_height), (GRID_COLUMNS, false, None));
        }
        assert_eq!(
            (s[0].columns, s[0].fields[0].width, s[1].columns, s[1].fields[0].width),
            (4, 4, DEFAULT_COLUMNS, 1)
        );
        // Saved again: the defaults are spelled out, nothing else changes, and it reads back the same.
        let out = serde_json::to_value(&d).unwrap();
        let mut expected = old;
        let sections = expected["layouts"][0]["tabs"][0]["sections"].as_array_mut().unwrap();
        sections[0]["width"] = json!(12);
        sections[1].as_object_mut().unwrap().extend([
            ("columns".to_owned(), json!(3)),
            ("width".to_owned(), json!(12)),
            ("collapsed".to_owned(), json!(false)),
        ]);
        sections[1]["fields"][0]["width"] = json!(1);
        let exp = &expected["layouts"][0];
        assert_eq!(out["layouts"][0]["tabs"], exp["tabs"]);
        assert_eq!(doc(out), d);
        // v1 panels become full-width sections too.
        let v1 = doc(json!({"layouts": [{"classKey": "server", "panels": [{"key": "p", "label": "P"}]}]}));
        assert_eq!(v1.layouts[0].tabs[0].sections[0].width, GRID_COLUMNS);
    }

    #[test]
    fn v1_panels_become_sections_of_one_general_tab() {
        let d = doc(json!({"layouts": [{"classKey": "server",
            "panels": [
                {"key": "hw", "label": "Hardware", "fields": ["attributes.cpu_cores", "validFrom"], "collapsed": true},
                {"key": "empty", "label": "Empty"},
            ],
            "hiddenFields": ["attributes.serial"], "readOnlyFields": ["ident"]}]}));
        let v2 = doc(json!({"layouts": [{"classKey": "server",
            "tabs": [{"key": "general", "label": "General", "sections": [
                {"key": "hw", "label": "Hardware", "columns": 3, "width": 12, "collapsed": true,
                 "fields": [{"field": "attributes.cpu_cores", "width": 1}, {"field": "validFrom", "width": 1}]},
                {"key": "empty", "label": "Empty", "columns": 3, "fields": []},
            ]}],
            "hiddenFields": ["attributes.serial"], "readOnlyFields": ["ident"]}]}));
        assert_eq!(d, v2);
        assert!(d.check().is_empty());
        // Never written back in the old format.
        let out = serde_json::to_value(&d).unwrap();
        assert!(out["layouts"][0].get("panels").is_none(), "{out}");
        assert_eq!(
            out["layouts"][0]["tabs"][0]["sections"][0]["fields"][0],
            json!({"field": "attributes.cpu_cores", "width": 1})
        );
    }

    #[test]
    fn a_layout_is_in_one_format_only() {
        let err = serde_json::from_value::<UiSettingsDocument>(json!({"layouts": [{"classKey": "server",
            "panels": [{"key": "p", "label": "P"}],
            "tabs": [{"key": "t", "label": "T"}]}]}))
        .unwrap_err();
        assert!(err.to_string().contains("not both"), "{err}");
    }

    #[test]
    fn hidden_core_fields_and_unknown_placed_attributes_are_ignored() {
        let d = doc(json!({"layouts": [{"classKey": "server",
            "tabs": [{"key": "t", "label": "T", "sections": [{"key": "s", "label": "S", "fields": [
                {"field": "attributes.gone"}, {"field": "attributes.cpu_cores", "width": 2}]}]}],
            "hiddenFields": ["validUntil"]}]}));
        let (effective, issues) = resolve(&d, &model());
        let got: Vec<(&str, IssueCode)> = issues.iter().map(|i| (i.path.as_str(), i.code)).collect();
        assert_eq!(
            got,
            [
                ("layouts.0.tabs.0.sections.0.fields.0", IssueCode::UnknownAttribute),
                ("layouts.0.hiddenFields.0", IssueCode::CoreFieldHidden),
            ]
        );
        let l = &effective.layouts[0];
        assert_eq!(l.tabs[0].sections[0].fields, [UiLayoutField::field("attributes.cpu_cores", 2)]);
        assert!(l.hidden_fields.is_empty());
    }

    #[test]
    fn dangling_references_are_dropped_and_reported() {
        let d = doc(json!({
            "navigation": {"entries": [
                {"type": "class", "classKey": "retired"},
                {"type": "section", "key": "s", "label": "S", "items": [{"classKey": "server"}, {"classKey": "gone"}]},
            ]},
            "dashboard": {"widgets": [
                {"id": "w", "type": "saved_search", "search": {
                    "classKeys": ["server", "gone"], "filters": {"lookups": {"status": ["in_service", "old"], "owner": ["x"]}}}},
                {"id": "c", "type": "count_by_lookup", "lookupListKey": "gone"},
            ]},
            "listViews": [
                {"classKey": "server", "columns": ["label", "attributes.cpu_cores", "attributes.ram"]},
                {"classKey": "gone", "columns": ["label"]},
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
                ("dashboard.widgets.0.search.filters.lookups.owner", IssueCode::UnknownLookupList),
                ("dashboard.widgets.0.search.filters.lookups.status.1", IssueCode::UnknownLookupValue),
                ("dashboard.widgets.1.lookupListKey", IssueCode::UnknownLookupList),
                ("listViews.0.columns.2", IssueCode::UnknownAttribute),
                ("listViews.1.classKey", IssueCode::UnknownClass),
                ("layouts.0.hiddenFields.1", IssueCode::UnknownAttribute),
                ("layouts.0.hiddenFields.0", IssueCode::RequiredFieldNotEditable),
            ]
        );
        assert_eq!(effective.navigation.entries.len(), 1);
        assert_eq!(effective.navigation.entries[0].items.len(), 1);
        assert_eq!(effective.list_views.len(), 1);
        assert_eq!(effective.list_views[0].columns, ["label", "attributes.cpu_cores"]);
        assert_eq!(effective.layouts[0].hidden_fields, ["attributes.serial"]);
        let search = effective.dashboard.widgets.unwrap()[0].search.clone().unwrap();
        assert_eq!(search.class_keys, ["server"]);
        assert_eq!(search.filters.lookups, BTreeMap::from([("status".to_owned(), vec!["in_service".to_owned()])]));
    }

    /// GH#112: a sort on an attribute is kept where the class has it, otherwise dropped (label order).
    #[test]
    fn attribute_sorts_need_the_attribute() {
        let sort = |field: &str| json!({"field": field, "direction": "desc"});
        let search = |classes: serde_json::Value, field: &str| json!({"id": field.replace('.', "_"), "type": "saved_search", "search": {"classKeys": classes, "sort": sort(field)}});
        let d = doc(json!({
            "dashboard": {"widgets": [
                search(json!(["server"]), "attributes.cpu_cores"),
                search(json!(["server", "application"]), "attributes.serial"),
                search(json!([]), "attributes.ram"),
                search(json!([]), "createdAt"),
            ]},
            "listViews": [
                {"classKey": "server", "defaultSort": sort("attributes.cpu_cores")},
                {"classKey": "application", "defaultSort": sort("attributes.cpu_cores")},
            ],
        }));
        assert!(d.check().is_empty());
        let (effective, issues) = resolve(&d, &model());
        let got: Vec<(&str, IssueCode)> = issues.iter().map(|i| (i.path.as_str(), i.code)).collect();
        assert_eq!(
            got,
            [
                ("dashboard.widgets.1.search.sort.field", IssueCode::UnknownAttribute),
                ("dashboard.widgets.2.search.sort.field", IssueCode::UnknownAttribute),
                ("listViews.1.defaultSort.field", IssueCode::UnknownAttribute),
            ]
        );
        let kept: Vec<Option<String>> =
            effective.dashboard.widgets.unwrap().into_iter().map(|w| w.search.unwrap().sort.map(|s| s.field)).collect();
        assert_eq!(kept, [Some("attributes.cpu_cores".into()), None, None, Some("createdAt".into())]);
        assert_eq!(
            effective.list_views[0].default_sort.as_ref().map(|s| s.field.as_str()),
            Some("attributes.cpu_cores")
        );
        assert_eq!(effective.list_views[1].default_sort, None);
    }

    #[test]
    fn sections_can_hold_a_note_or_a_built_in_panel() {
        let d = doc(json!({"layouts": [{"classKey": "server", "tabs": [
            {"key": "main", "label": "Main", "sections": [
                {"key": "hint", "label": "Before you edit", "kind": "note", "text": "Owned by **Ops**. See the runbook."},
                {"key": "hw", "label": "Hardware", "fields": [{"field": "attributes.cpu_cores"}]},
            ]},
            {"key": "links", "label": "Links", "sections": [
                {"key": "rel", "label": "Relationships", "kind": "relations"},
                {"key": "hist", "label": "History", "kind": "history", "collapsed": true},
                {"key": "log", "label": "Audit trail", "kind": "audit"},
            ]},
        ]}]}));
        assert!(d.check().is_empty(), "{:?}", d.check());
        let kinds: Vec<UiSectionKind> = d.layouts[0].tabs.iter().flat_map(|t| &t.sections).map(|s| s.kind).collect();
        use UiSectionKind::*;
        assert_eq!(kinds, [Note, Fields, Relations, History, Audit]);
        // Round trip: `kind` is written except for field sections, `text` only for notes.
        let out = serde_json::to_value(&d).unwrap();
        let tabs = &out["layouts"][0]["tabs"];
        assert_eq!(tabs[0]["sections"][0]["kind"], "note");
        assert_eq!(tabs[0]["sections"][0]["text"], "Owned by **Ops**. See the runbook.");
        assert!(tabs[0]["sections"][1].get("kind").is_none(), "{out}");
        assert!(tabs[1]["sections"][0].get("text").is_none(), "{out}");
        assert_eq!(serde_json::from_value::<UiSettingsDocument>(out).unwrap(), d);
        // Panels and notes carry no field references to resolve.
        assert_eq!(resolve(&d, &model()), (d.clone(), vec![]));
    }

    #[test]
    fn documents_without_section_kinds_read_and_write_back_unchanged() {
        let stored = json!({"layouts": [{"classKey": "server", "tabs": [
            {"key": "general", "label": "General", "sections": [
                {"key": "main", "label": "Main", "columns": 2, "collapsed": false,
                 "fields": [{"field": "ident", "width": 2}, {"field": "attributes.cpu_cores", "width": 1}]},
            ]},
        ]}]});
        let d = doc(stored.clone());
        assert_eq!(d.layouts[0].tabs[0].sections[0].kind, UiSectionKind::Fields);
        assert!(d.check().is_empty());
        // Written back unchanged except for the grid width every section returns (default 12).
        let mut expected = stored["layouts"].clone();
        expected[0]["tabs"][0]["sections"][0]["width"] = json!(12);
        assert_eq!(serde_json::to_value(&d).unwrap()["layouts"], expected);
        // v1 panels still convert to field sections.
        let v1 = doc(
            json!({"layouts": [{"classKey": "server", "panels": [{"key": "p", "label": "P", "fields": ["ident"]}]}]}),
        );
        assert_eq!(v1.layouts[0].tabs[0].sections[0].kind, UiSectionKind::Fields);
        assert!(serde_json::to_value(&v1).unwrap()["layouts"][0]["tabs"][0]["sections"][0].get("kind").is_none());
    }

    #[test]
    fn notes_and_panels_follow_their_rules() {
        let long = "x".repeat(NOTE_MAX_CHARS + 1);
        let d = doc(json!({"layouts": [{"classKey": "server", "tabs": [
            {"key": "a", "label": "A", "sections": [
                {"key": "n1", "label": "N", "kind": "note"},
                {"key": "n2", "label": "N", "kind": "note", "text": "  "},
                {"key": "n3", "label": "N", "kind": "note", "text": long},
                {"key": "n4", "label": "N", "kind": "note", "text": "ok", "fields": [{"field": "ident"}]},
                {"key": "f", "label": "F", "text": "stray"},
                {"key": "r1", "label": "R", "kind": "relations"},
            ]},
            {"key": "b", "label": "B", "sections": [
                {"key": "r2", "label": "R", "kind": "relations", "text": "stray"},
                {"key": "h", "label": "H", "kind": "history", "fields": [{"field": "label"}]},
                {"key": "r1", "label": "Dup", "kind": "audit"},
            ]},
        ]}]}));
        let fields: Vec<String> = d.check().into_iter().map(|e| e.field).collect();
        assert_eq!(
            fields,
            [
                "layouts.0.tabs.0.sections.0.text",
                "layouts.0.tabs.0.sections.1.text",
                "layouts.0.tabs.0.sections.2.text",
                "layouts.0.tabs.0.sections.3.fields",
                "layouts.0.tabs.0.sections.4.text",
                "layouts.0.tabs.1.sections.0.kind",
                "layouts.0.tabs.1.sections.0.text",
                "layouts.0.tabs.1.sections.1.fields",
                "layouts.0.tabs.1.sections.2.key",
            ]
        );
        // The same panel in two layouts is fine: the rule is per layout.
        let two = doc(json!({"layouts": [
            {"classKey": "server", "tabs": [{"key": "t", "label": "T", "sections": [{"key": "r", "label": "R", "kind": "relations"}]}]},
            {"classKey": "application", "tabs": [{"key": "t", "label": "T", "sections": [{"key": "r", "label": "R", "kind": "relations"}]}]},
        ]}));
        assert!(two.check().is_empty());
        let unknown = serde_json::from_value::<UiSettingsDocument>(json!({"layouts": [{"classKey": "server",
            "tabs": [{"key": "t", "label": "T", "sections": [{"key": "s", "label": "S", "kind": "html"}]}]}]}));
        assert!(unknown.is_err());
    }

    fn one_tab(tab: serde_json::Value) -> UiSettingsDocument {
        doc(json!({"layouts": [{"classKey": "server", "tabs": [tab]}]}))
    }

    fn frames(tab: &UiLayoutTab) -> Vec<(&str, f64, u32, f64, u32, u32)> {
        tab.sections
            .iter()
            .map(|s| {
                let f = s.frame.unwrap();
                (s.key.as_str(), f.x, f.y, f.w, f.h, f.z)
            })
            .collect()
    }

    #[test]
    fn free_tabs_round_trip_and_grid_tabs_are_read_as_before() {
        let stored = json!({"layouts": [{"classKey": "server", "tabs": [
            {"key": "g", "label": "Grid", "sections": [
                {"key": "a", "label": "A", "columns": 3, "width": 12, "collapsed": false, "fields": []}]},
            {"key": "f", "label": "Free", "placement": "free", "sections": [
                {"key": "b", "label": "B", "columns": 3, "width": 12, "collapsed": false, "fields": [],
                 "frame": {"x": 0.0, "y": 0, "w": 0.5, "h": 200, "z": 2, "minH": 96}},
                {"key": "n", "label": "N", "kind": "note", "text": "On top", "columns": 3, "width": 12,
                 "collapsed": false, "fields": [], "frame": {"x": 0.25, "y": 40, "w": 0.75, "h": 120, "z": 1}},
            ]},
        ]}]});
        let d = doc(stored.clone());
        assert!(d.check().is_empty(), "{:?}", d.check());
        let tabs = &d.layouts[0].tabs;
        assert_eq!((tabs[0].placement, tabs[1].placement), (UiTabPlacement::Grid, UiTabPlacement::Free));
        assert_eq!(tabs[1].sections[0].frame.unwrap().min_h, Some(96));
        // Read as sent: no placement or frame on the grid tab, minH only when set.
        assert_eq!(serde_json::to_value(&d).unwrap()["layouts"], stored["layouts"]);
        // Normalising leaves the free tab as it is and makes the grid tab free.
        let n = d.clone().normalized();
        assert_eq!(frames(&n.layouts[0].tabs[1]), [("b", 0.0, 0, 0.5, 200, 2), ("n", 0.25, 40, 0.75, 120, 1)]);
        assert_eq!(n.layouts[0].tabs[1], d.layouts[0].tabs[1]);
        assert_eq!(n.layouts[0].tabs[0].placement, UiTabPlacement::Free);
        assert_eq!(frames(&n.layouts[0].tabs[0]), [("a", 0.0, 0, 1.0, 96, 1)]);
        assert_eq!(serde_json::to_value(&n).unwrap()["layouts"][0]["tabs"][0]["placement"], "free");
    }

    #[test]
    fn frames_must_fit_the_tab() {
        let d = one_tab(json!({"key": "t", "label": "T", "placement": "free", "sections": [
            {"key": "a", "label": "A", "frame": {"x": 0.6, "y": 0, "w": 0.5, "h": 100, "z": 1}},
            {"key": "b", "label": "B", "frame": {"x": 0.0, "y": 0, "w": 0.01, "h": 100, "z": 2}},
            {"key": "c", "label": "C", "frame": {"x": 0.0, "y": 0, "w": 1.0, "h": 100, "z": 3, "minH": 101}},
            {"key": "d", "label": "D", "frame": {"x": 0.0, "y": 0, "w": 1.0, "h": 10, "z": 4}},
            // 11/12 + 1/12 is 1 within rounding.
            {"key": "e", "label": "E", "frame": {"x": 11.0 / 12.0, "y": 0, "w": 1.0 / 12.0, "h": 48, "z": 5}},
        ]}));
        let fields: Vec<String> = d.check().into_iter().map(|e| e.field).collect();
        assert_eq!(
            fields,
            [
                "layouts.0.tabs.0.sections.0.frame.w",
                "layouts.0.tabs.0.sections.1.frame.w",
                "layouts.0.tabs.0.sections.2.frame.minH",
                "layouts.0.tabs.0.sections.3.frame.h",
            ]
        );
        let junk = |frame: serde_json::Value| {
            serde_json::from_value::<UiSettingsDocument>(json!({"layouts": [{"classKey": "server", "tabs": [
                {"key": "t", "label": "T", "placement": "free", "sections": [{"key": "a", "label": "A", "frame": frame}]}]}]}))
        };
        assert!(junk(json!({"x": 0, "y": 0, "w": 1, "h": 100})).is_err(), "z is required");
        assert!(junk(json!({"x": 0, "y": -1, "w": 1, "h": 100, "z": 1})).is_err());
        assert!(junk(json!({"x": 0, "y": 0, "w": 1, "h": 100, "z": 1, "depth": 3})).is_err());
        assert!(junk(json!({"x": "0", "y": 0, "w": 1, "h": 100, "z": 1})).is_err());
        let placement = serde_json::from_value::<UiSettingsDocument>(
            json!({"layouts": [{"classKey": "server", "tabs": [{"key": "t", "label": "T", "placement": "floating"}]}]}),
        );
        assert!(placement.is_err());
    }

    /// Two halves, a full-width section with two rows of fields, one forced onto a new row, and a panel
    /// that does not fit next to it.
    fn grid_tab() -> serde_json::Value {
        json!({"key": "t", "label": "T", "sections": [
            {"key": "a", "label": "A", "width": 6},
            {"key": "b", "label": "B", "width": 6, "minHeight": 3},
            {"key": "c", "label": "C", "fields": [
                {"field": "ident"}, {"field": "label"}, {"field": "validFrom"}, {"field": "validUntil"}]},
            {"key": "d", "label": "D", "width": 4, "newRow": true},
            {"key": "r", "label": "R", "kind": "relations"},
        ]})
    }

    #[test]
    fn grid_tabs_become_free_with_every_section_where_it_was() {
        // Rows at y 0 (a; b with 3 field rows), 208 (c: 2 field rows), 368 (d) and 480 (r, a panel).
        let expected = [
            ("a", 0.0, 0, 0.5, 96, 1),
            ("b", 0.5, 0, 0.5, 192, 2),
            ("c", 0.0, 208, 1.0, 144, 3),
            ("d", 0.0, 368, 0.3333, 96, 4),
            ("r", 0.0, 480, 1.0, FRAME_PANEL_PX, 5),
        ];
        // Free without frames, grid (still accepted, as older exports send it) and no placement at all.
        for placement in [json!("free"), json!("grid"), serde_json::Value::Null] {
            let mut tab = grid_tab();
            if !placement.is_null() {
                tab["placement"] = placement.clone();
            }
            let d = one_tab(tab).normalized();
            assert!(d.check().is_empty(), "{placement}: {:?}", d.check());
            let t = &d.layouts[0].tabs[0];
            assert_eq!(t.placement, UiTabPlacement::Free, "{placement}");
            assert_eq!(frames(t), expected, "{placement}");
            assert_eq!(d.clone().normalized(), d, "{placement}: stable");
        }
    }

    #[test]
    fn a_grid_tab_sent_with_frames_keeps_them() {
        // Before free placement was the only one, frames in a grid tab meant "back on the grid"; now the
        // tab is free and every window stays where it was sent.
        let d = one_tab(json!({"key": "t", "label": "T", "placement": "grid", "sections": [
            {"key": "low", "label": "Low", "frame": {"x": 0.0, "y": 500, "w": 1.0, "h": 100, "z": 1}},
            {"key": "right", "label": "Right", "frame": {"x": 0.6, "y": 10, "w": 0.4, "h": 200, "z": 3}},
            {"key": "left", "label": "Left", "frame": {"x": 0.0, "y": 0, "w": 0.55, "h": 300, "z": 2}},
            {"key": "new", "label": "New"},
        ]}))
        .normalized();
        let t = &d.layouts[0].tabs[0];
        assert_eq!(t.placement, UiTabPlacement::Free);
        assert_eq!(
            frames(t),
            [
                ("left", 0.0, 0, 0.55, 300, 2),
                ("right", 0.6, 10, 0.4, 200, 3),
                ("low", 0.0, 500, 1.0, 100, 1),
                ("new", 0.0, 616, 1.0, 96, 4),
            ]
        );
    }

    #[test]
    fn saving_a_free_tab_normalises_it() {
        let d = one_tab(json!({"key": "t", "label": "T", "placement": "free", "sections": [
            {"key": "b", "label": "B", "frame": {"x": 0.123456, "y": 300, "w": 0.3, "h": 100, "z": 7}},
            {"key": "a", "label": "A", "frame": {"x": 0.5, "y": 0, "w": 0.5, "h": 200, "z": 7}},
            {"key": "c", "label": "C", "frame": {"x": 0.1, "y": 0, "w": 0.2, "h": 60, "z": 0}},
            {"key": "new", "label": "New"},
        ]}))
        .normalized();
        assert!(d.check().is_empty(), "{:?}", d.check());
        // Reading order y, then x; z 1..n by (z, order sent); the new section below every window, on top.
        assert_eq!(
            frames(&d.layouts[0].tabs[0]),
            [
                ("c", 0.1, 0, 0.2, 60, 1),
                ("a", 0.5, 0, 0.5, 200, 3),
                ("b", 0.1235, 300, 0.3, 100, 2),
                ("new", 0.0, 416, 1.0, 96, 4),
            ]
        );
        // Saving again changes nothing.
        assert_eq!(d.clone().normalized(), d);
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
        assert!(!re.is_match("hostname"), "a class field since SHAA-267");
    }

    #[test]
    fn list_sort_pattern_accepts_every_inventory_sort_field() {
        let schema = serde_json::to_value(<UiListSort as utoipa::PartialSchema>::schema()).unwrap();
        let re = regex::Regex::new(schema["properties"]["field"]["pattern"].as_str().unwrap()).unwrap();
        for f in crate::data::items::SORT_FIELDS {
            assert!(re.is_match(f), "{f}");
        }
        assert!(re.is_match("attributes.cpu_cores"));
    }

    fn sections_doc(sections: serde_json::Value) -> UiSettingsDocument {
        doc(json!({"layoutFormat": 3, "layouts": [{"classKey": "server", "tabs": [
            {"key": "main", "label": "Main", "sections": sections}]}]}))
    }

    #[test]
    fn separators_hold_no_field_and_span_the_section() {
        let d = sections_doc(json!([{"key": "s", "label": "S", "columns": 2, "fields": [
            {"field": "label"},
            {"separator": true, "label": "Hardware", "width": 4},
            {"separator": true},
            {"field": "attributes.cpu_cores", "width": 2},
            {"separator": true, "field": "ident"},
            {"width": 1},
            {"field": "attributes.serial", "label": "Serial"},
            {"separator": true, "label": "  "},
        ]}]));
        let fields: Vec<String> = d.check().into_iter().map(|e| e.field).collect();
        assert_eq!(
            fields,
            [
                "layouts.0.tabs.0.sections.0.fields.4.field",
                "layouts.0.tabs.0.sections.0.fields.5.field",
                "layouts.0.tabs.0.sections.0.fields.6.label",
                "layouts.0.tabs.0.sections.0.fields.7.label",
            ],
            "a separator's width is not checked, any number of them is fine"
        );

        let d = sections_doc(json!([{"key": "s", "label": "S", "columns": 2, "fields": [
            {"field": "label"},
            {"separator": true, "label": "Hardware", "width": 1},
            {"field": "attributes.nope"},
            {"separator": true},
        ]}]));
        assert!(d.check().is_empty(), "{:?}", d.check());
        let stored = d.normalized();
        let section = &stored.layouts[0].tabs[0].sections[0];
        assert_eq!(section.fields[1].width, 2, "stored as the section's columns");
        // Label, separator (a row of its own), separator: three rows.
        let frame = section.frame.unwrap();
        assert_eq!(frame.h, FRAME_HEADER_PX + 4 * FRAME_ROW_PX);
        let json = serde_json::to_value(&section.fields).unwrap();
        assert_eq!(json[1], json!({"separator": true, "label": "Hardware", "width": 2}));
        assert_eq!(json[0], json!({"field": "label", "width": 1}), "fields are stored as before");

        // Separators stay when unknown attributes are left out; the issue points at the stored entry.
        let (out, issues) = resolve(&stored, &model());
        let kept: Vec<Option<&str>> =
            out.layouts[0].tabs[0].sections[0].fields.iter().map(|f| f.field.as_deref()).collect();
        assert_eq!(kept, [Some("label"), None, None]);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].path, "layouts.0.tabs.0.sections.0.fields.2");
    }

    #[test]
    fn the_record_panel_is_placed_once_and_holds_nothing() {
        let d = sections_doc(json!([
            {"key": "a", "label": "Record", "kind": "record"},
            {"key": "b", "label": "Again", "kind": "record", "fields": [{"field": "label"}]},
        ]));
        let fields: Vec<String> = d.check().into_iter().map(|e| e.field).collect();
        assert_eq!(fields, ["layouts.0.tabs.0.sections.1.kind", "layouts.0.tabs.0.sections.1.fields"]);
        let d = sections_doc(json!([{"key": "a", "label": "Record", "kind": "record", "text": "x"}]));
        assert_eq!(d.check()[0].field, "layouts.0.tabs.0.sections.0.text");
        let d = sections_doc(json!([{"key": "a", "label": "Record", "kind": "record"}]));
        assert_eq!(d.normalized().layouts[0].tabs[0].sections[0].frame.unwrap().h, FRAME_RECORD_PX);
    }

    #[test]
    fn older_documents_get_the_panels_they_showed() {
        let tabs = json!([
            {"key": "main", "label": "Main", "sections": [{"key": "relations", "label": "Notes", "fields": []}]},
            {"key": "more", "label": "More", "sections": [{"key": "log", "label": "Log", "kind": "audit"}]},
        ]);
        let old = doc(json!({
            "layouts": [{"classKey": "server", "tabs": tabs}, {"classKey": "vm", "hiddenFields": ["label"]}],
            "layoutTemplates": [
                {"key": "standard", "name": "Standard"},
                {"key": "placed", "name": "Placed", "layout": {"tabs": [{"key": "t", "label": "T", "sections": [
                    {"key": "x", "label": "Links", "kind": "relations"},
                    {"key": "y", "label": "Details", "kind": "record"}]}]}},
            ],
        }));
        let new = old.clone().upgraded();
        assert_eq!(new.layout_format, Some(LAYOUT_FORMAT));
        let first: Vec<(&str, UiSectionKind)> =
            new.layouts[0].tabs[0].sections.iter().map(|s| (s.key.as_str(), s.kind)).collect();
        assert_eq!(
            first,
            [
                ("relations", UiSectionKind::Fields),
                ("record", UiSectionKind::Record),
                ("relations_2", UiSectionKind::Relations)
            ],
            "last on the first tab, keys kept unique"
        );
        assert_eq!(new.layouts[0].tabs[1], old.layouts[0].tabs[1]);
        assert_eq!(new.layouts[1], old.layouts[1], "no tabs: the built-in arrangement");
        assert_eq!(new.layout_templates, old.layout_templates, "nothing to add");
        assert!(new.problems("").is_empty(), "{:?}", new.problems(""));

        // In the current format the layout is taken as sent: a removed panel stays removed.
        let current = sections_doc(json!([{"key": "a", "label": "A", "fields": [{"field": "label"}]}]));
        assert_eq!(current.clone().upgraded(), current);
        let mut future = current.clone();
        future.layout_format = Some(4);
        assert_eq!(future.check()[0].field, "layoutFormat");
    }
}
