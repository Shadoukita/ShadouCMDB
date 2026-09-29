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

use std::collections::{BTreeMap, HashSet};

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
    &["label", "ident", "class", "validFrom", "validUntil", "active", "createdAt", "updatedAt"];

pub const FIELD_PATTERN: &str =
    "^(label|ident|class|validFrom|validUntil|active|createdAt|updatedAt|attributes\\.[a-z][a-z0-9_]{0,62})$";

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
        "Columns in display order: built-in fields (label, ident, class, validFrom, validUntil, active, createdAt, \
         updatedAt) or attributes.<key>",
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
        pattern = "^(label|ident|className|validFrom|validUntil|createdAt|updatedAt|attributes\\.[a-z][a-z0-9_]{0,62})$"
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

/// A field on a section's grid. Fields fill the grid row by row in the order given.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiLayoutField {
    #[schema(schema_with = layout_field_schema)]
    pub field: String,
    /// Grid columns the field spans, at most the section's `columns`
    #[serde(default = "default_width")]
    #[schema(minimum = 1, maximum = 12, default = 1)]
    pub width: u8,
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
    /// The detail page's relationships panel
    Relations,
    /// The detail page's version history panel
    History,
    /// The detail page's audit trail panel
    Audit,
}

impl UiSectionKind {
    /// Built-in panels of the detail page: each at most once per layout; where a layout does not place
    /// one, the page shows it at its usual position.
    pub fn is_panel(self) -> bool {
        matches!(self, Self::Relations | Self::History | Self::Audit)
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Fields => "fields",
            Self::Note => "note",
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
        .enum_values(Some(["fields", "note", "relations", "history", "audit"]))
        .description(Some(
            "What the section shows (absent: fields): fields (a grid of `fields`), note (static `text`), or a built-in panel of the \
             detail page (relations, history, audit). Each panel can be placed once per layout; one that is not \
             placed keeps its usual position on the detail page.",
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
    /// Where the section sits in a tab with `placement` free. Sent in a grid tab, it converts the tab back to
    /// the grid on save (see the tab's `placement`); a stored grid tab never has frames.
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
/// `minHeight` unit, 3em), the gap between grid rows, a note and a built-in panel.
pub const FRAME_HEADER_PX: u32 = 48;
pub const FRAME_ROW_PX: u32 = 48;
pub const FRAME_GAP_PX: u32 = 16;
pub const FRAME_NOTE_PX: u32 = 144;
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

/// How a tab arranges its sections. Absent means `grid`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UiTabPlacement {
    /// Sections fill the tab's 12-column grid row by row, in order (`width`, `newRow`, `minHeight`)
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
            "How the tab arranges its sections (absent: grid). grid: sections fill the 12-column grid row by row. \
             free: each section is a window placed by its `frame`, and windows may overlap. On save, a free tab's \
             sections without a frame get one from their grid position (below the existing windows), z becomes \
             1..n and the sections are ordered by y, then x: the reading order, used on narrow screens, in print \
             and by screen readers. A grid tab sent with frames is converted back: sections ordered by y, then \
             x, width from w, and the frames dropped.",
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
                let w = f.width.clamp(1, s.columns.max(1));
                if col + w > s.columns {
                    rows += 1;
                    col = 0;
                }
                col += w;
            }
            FRAME_HEADER_PX + rows.max(u32::from(s.min_height.unwrap_or(1))).max(1) * FRAME_ROW_PX
        }
        UiSectionKind::Note => FRAME_NOTE_PX,
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
    /// The stored form of the tab. A free tab: every section framed (the missing frames from their grid
    /// position, below the existing windows), x and w rounded and within the tab, z 1..n and the sections
    /// in reading order (y, then x). A grid tab sent with frames: back on the grid, see [`Self::convert_to_grid`].
    pub fn normalize(&mut self) {
        match self.placement {
            UiTabPlacement::Free => self.normalize_free(),
            UiTabPlacement::Grid if self.sections.iter().any(|s| s.frame.is_some()) => self.convert_to_grid(),
            UiTabPlacement::Grid => {}
        }
    }

    fn normalize_free(&mut self) {
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

    /// Free -> grid: sections ordered by y, then x; each spans the grid columns nearest its width, and one
    /// that starts below the bottom of the first window of the current row starts a new row. Frames are
    /// dropped; sections without one follow at the end in their order.
    pub fn convert_to_grid(&mut self) {
        self.placement = UiTabPlacement::Grid;
        let mut order: Vec<usize> = (0..self.sections.len()).collect();
        order.sort_by(|&a, &b| match (self.sections[a].frame, self.sections[b].frame) {
            (Some(fa), Some(fb)) => fa.y.cmp(&fb.y).then(fa.x.total_cmp(&fb.x)).then(a.cmp(&b)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.cmp(&b),
        });
        let mut taken: Vec<Option<UiLayoutSection>> =
            std::mem::take(&mut self.sections).into_iter().map(Some).collect();
        let mut row_bottom: Option<u32> = None;
        for (n, i) in order.into_iter().enumerate() {
            let mut s = taken[i].take().expect("each section once");
            if let Some(f) = s.frame.take() {
                s.width = ((f.w * f64::from(GRID_COLUMNS)).round() as u8).clamp(1, GRID_COLUMNS);
                s.new_row = match row_bottom {
                    Some(b) if f.y < b => false,
                    _ => {
                        row_bottom = Some(f.y + f.h);
                        n > 0
                    }
                };
            }
            self.sections.push(s);
        }
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields, try_from = "ClassLayoutInput")]
pub struct UiClassLayout {
    #[schema(schema_with = key_schema)]
    pub class_key: String,
    #[serde(default)]
    #[schema(max_items = 20)]
    pub tabs: Vec<UiLayoutTab>,
    #[schema(schema_with = hidden_fields_schema)]
    #[serde(default)]
    pub hidden_fields: Vec<String>,
    #[schema(schema_with = read_only_fields_schema)]
    #[serde(default)]
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
            fields: p.fields.into_iter().map(|field| UiLayoutField { field, width: 1 }).collect(),
            text: None,
            collapsed: p.collapsed,
            frame: None,
        })
        .collect();
    vec![UiLayoutTab { key: "general".into(), label: "General".into(), placement: UiTabPlacement::Grid, sections }]
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
    /// The form in which a valid document is stored: every layout tab normalised (see
    /// [`UiLayoutTab::normalize`]).
    pub fn normalized(mut self) -> Self {
        for tab in self.layouts.iter_mut().flat_map(|l| &mut l.tabs) {
            tab.normalize();
        }
        self
    }

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
            let mut tabs = HashSet::new();
            let mut sections = HashSet::new();
            let mut placed = HashSet::new();
            let mut panels = HashSet::new();
            for (t, tab) in l.tabs.iter().enumerate() {
                let pt = format!("{p}.tabs.{t}");
                if !tabs.insert(tab.key.as_str()) {
                    e.push(custom(at(format!("{pt}.key")), "Tab keys must be unique in a layout"));
                }
                for (j, s) in tab.sections.iter().enumerate() {
                    let ps = format!("{pt}.sections.{j}");
                    if !sections.insert(s.key.as_str()) {
                        e.push(custom(at(format!("{ps}.key")), "Section keys must be unique in a layout"));
                    }
                    if s.kind.is_panel() && !panels.insert(s.kind) {
                        e.push(custom(
                            at(format!("{ps}.kind")),
                            format!("The {} panel can be placed once in a layout", s.kind.as_str()),
                        ));
                    }
                    if s.kind != UiSectionKind::Fields && !s.fields.is_empty() {
                        e.push(custom(at(format!("{ps}.fields")), "Only sections of kind fields hold fields"));
                    }
                    match (&s.text, s.kind) {
                        (None, UiSectionKind::Note) => {
                            e.push(custom(at(format!("{ps}.text")), "Required for note sections"))
                        }
                        (Some(text), UiSectionKind::Note) if text.trim().is_empty() => {
                            e.push(custom(at(format!("{ps}.text")), "Required for note sections"))
                        }
                        (Some(text), UiSectionKind::Note) if text.chars().count() > NOTE_MAX_CHARS => {
                            e.push(custom(at(format!("{ps}.text")), format!("At most {NOTE_MAX_CHARS} characters")))
                        }
                        (Some(_), k) if k != UiSectionKind::Note => {
                            e.push(custom(at(format!("{ps}.text")), "Only allowed for note sections"))
                        }
                        _ => {}
                    }
                    if let Some(f) = &s.frame {
                        let pf = format!("{ps}.frame");
                        if !f.x.is_finite() || !f.w.is_finite() {
                            e.push(custom(at(pf.clone()), "x and w must be finite numbers"));
                        } else if f.w < FRAME_MIN_W - FRAME_EPSILON {
                            e.push(custom(at(format!("{pf}.w")), format!("At least {FRAME_MIN_W} of the tab's width")));
                        } else if f.x + f.w > 1.0 + FRAME_EPSILON {
                            e.push(custom(
                                at(format!("{pf}.w")),
                                "The window must end inside the tab: x + w must be at most 1",
                            ));
                        }
                        if !(FRAME_MIN_H..=FRAME_MAX_H).contains(&f.h) {
                            e.push(custom(
                                at(format!("{pf}.h")),
                                format!("Between {FRAME_MIN_H} and {FRAME_MAX_H} px"),
                            ));
                        }
                        if f.min_h.is_some_and(|m| m > f.h) {
                            e.push(custom(at(format!("{pf}.minH")), "At most the window's height h"));
                        }
                    }
                    for (k, f) in s.fields.iter().enumerate() {
                        if !placed.insert(f.field.as_str()) {
                            e.push(custom(at(format!("{ps}.fields.{k}.field")), "A field can be placed once only"));
                        }
                        if f.width > s.columns {
                            e.push(custom(
                                at(format!("{ps}.fields.{k}.width")),
                                format!("At most the section's {} column(s)", s.columns),
                            ));
                        }
                    }
                }
            }
            for (k, f) in l.hidden_fields.iter().enumerate() {
                if CORE_FIELDS.contains(&f.as_str()) {
                    e.push(custom(
                        at(format!("{p}.hiddenFields.{k}")),
                        "Ident, valid from and valid until belong to every CI: move them, but they cannot be hidden",
                    ));
                }
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
    /// The lookup list does not exist; the filter or widget setting is ignored
    UnknownLookupList,
    /// The value is not in the lookup list; it is ignored
    UnknownLookupValue,
    /// A required attribute is hidden or read-only on the form: CIs of the class cannot be created in the UI (kept, only flagged)
    RequiredFieldNotEditable,
    /// A core field (ident, validFrom, validUntil) is hidden, e.g. in a restored older version; it is shown anyway
    CoreFieldHidden,
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
                let names: Vec<String> = s.fields.iter().map(|f| f.field.clone()).collect();
                let kept: HashSet<String> = r.fields(&ps, &l.class_key, &names).into_iter().collect();
                s.fields.retain(|f| kept.contains(&f.field));
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
        assert_eq!(l.tabs[0].sections[0].fields, [UiLayoutField { field: "attributes.cpu_cores".into(), width: 2 }]);
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
        ], "hiddenFields": [], "readOnlyFields": []}]});
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
    fn free_tabs_round_trip_and_grid_tabs_are_written_as_before() {
        let stored = json!({"layouts": [{"classKey": "server", "tabs": [
            {"key": "g", "label": "Grid", "sections": [
                {"key": "a", "label": "A", "columns": 3, "width": 12, "collapsed": false, "fields": []}]},
            {"key": "f", "label": "Free", "placement": "free", "sections": [
                {"key": "b", "label": "B", "columns": 3, "width": 12, "collapsed": false, "fields": [],
                 "frame": {"x": 0.0, "y": 0, "w": 0.5, "h": 200, "z": 2, "minH": 96}},
                {"key": "n", "label": "N", "kind": "note", "text": "On top", "columns": 3, "width": 12,
                 "collapsed": false, "fields": [], "frame": {"x": 0.25, "y": 40, "w": 0.75, "h": 120, "z": 1}},
            ]},
        ], "hiddenFields": [], "readOnlyFields": []}]});
        let d = doc(stored.clone());
        assert!(d.check().is_empty(), "{:?}", d.check());
        let tabs = &d.layouts[0].tabs;
        assert_eq!((tabs[0].placement, tabs[1].placement), (UiTabPlacement::Grid, UiTabPlacement::Free));
        assert_eq!(tabs[1].sections[0].frame.unwrap().min_h, Some(96));
        // Written back exactly: no placement or frame on grid tabs, minH only when set.
        assert_eq!(serde_json::to_value(&d).unwrap()["layouts"], stored["layouts"]);
        // Already in stored form, so normalising changes nothing.
        let n = d.clone().normalized();
        assert_eq!(frames(&n.layouts[0].tabs[1]), [("b", 0.0, 0, 0.5, 200, 2), ("n", 0.25, 40, 0.75, 120, 1)]);
        assert_eq!(n, d);
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
    fn grid_to_free_keeps_every_section_where_it_was() {
        let mut tab = grid_tab();
        tab["placement"] = json!("free");
        let d = one_tab(tab).normalized();
        assert!(d.check().is_empty(), "{:?}", d.check());
        let t = &d.layouts[0].tabs[0];
        // Rows at y 0 (a; b with 3 field rows), 208 (c: 2 field rows), 368 (d) and 480 (r, a panel).
        assert_eq!(
            frames(t),
            [
                ("a", 0.0, 0, 0.5, 96, 1),
                ("b", 0.5, 0, 0.5, 192, 2),
                ("c", 0.0, 208, 1.0, 144, 3),
                ("d", 0.0, 368, 0.3333, 96, 4),
                ("r", 0.0, 480, 1.0, FRAME_PANEL_PX, 5),
            ]
        );
        // Back on the grid, every section is where it was.
        let mut back = t.clone();
        back.convert_to_grid();
        let grid: Vec<(&str, u8, bool, bool)> =
            back.sections.iter().map(|s| (s.key.as_str(), s.width, s.new_row, s.frame.is_none())).collect();
        assert_eq!(
            grid,
            [
                ("a", 6, false, true),
                ("b", 6, false, true),
                ("c", 12, true, true),
                ("d", 4, true, true),
                ("r", 12, true, true)
            ]
        );
        assert_eq!(grid_frames(&back.sections, 0), grid_frames(&one_tab(grid_tab()).layouts[0].tabs[0].sections, 0));
    }

    #[test]
    fn free_to_grid_orders_by_y_then_x() {
        // Sent as a grid tab that still has frames: the tab goes back on the grid on save.
        let d = one_tab(json!({"key": "t", "label": "T", "sections": [
            {"key": "low", "label": "Low", "frame": {"x": 0.0, "y": 500, "w": 1.0, "h": 100, "z": 1}},
            {"key": "right", "label": "Right", "frame": {"x": 0.6, "y": 10, "w": 0.4, "h": 200, "z": 3}},
            {"key": "left", "label": "Left", "frame": {"x": 0.0, "y": 0, "w": 0.55, "h": 300, "z": 2}},
            {"key": "under", "label": "Under", "frame": {"x": 0.1, "y": 150, "w": 0.2, "h": 50, "z": 4}},
        ]}))
        .normalized();
        let t = &d.layouts[0].tabs[0];
        assert_eq!(t.placement, UiTabPlacement::Grid);
        let grid: Vec<(&str, u8, bool)> = t.sections.iter().map(|s| (s.key.as_str(), s.width, s.new_row)).collect();
        // "under" starts above the bottom of "left", so it stays in that row (and wraps if it does not fit).
        assert_eq!(grid, [("left", 7, false), ("right", 5, false), ("under", 2, false), ("low", 12, true)]);
        assert!(t.sections.iter().all(|s| s.frame.is_none()));
        let out = serde_json::to_value(&d).unwrap();
        assert!(out["layouts"][0]["tabs"][0].get("placement").is_none(), "{out}");
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
}
