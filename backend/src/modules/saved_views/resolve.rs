//! Checking a definition on save and resolving a stored one against today's
//! data model (SHAA-578 §2.2, §2.6, §3.1, §3.2).
//!
//! A key the caller may not see is *hidden*: for a caller whose profile limits
//! the classes they may view, a class key is hidden unless the class exists and
//! they may view it, so a hidden class and one that never existed look alike
//! (no oracle on class names). A caller who may view every class has nothing
//! hidden; a class key that no longer exists is reported to them by name.
//! Hidden keys are never named in a response and are left out of the
//! definition returned to that caller; the stored definition keeps them.
//!
//! Resolution may drop what only narrows or presents results (a class among
//! several, one of several lookup values, a column, an attribute sort). It
//! never drops a constraint in a way that widens them: when a class list or a
//! lookup filter would be left empty, the view is `unavailable` and has no
//! query at all. The stored definition is never rewritten.

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use sqlx::PgConnection;
use utoipa::ToSchema;
use uuid::Uuid;

use super::definition::{SavedViewActive, SavedViewContext, SavedViewDefinition, SavedViewDeleted, body_field};
use crate::api::context::RequestContext;
use crate::auth::permissions::ClassOp;
use crate::data::items::is_sortable;
use crate::http::error::FieldError;
use crate::modules::classes::AttributeDataType;

const ATTRIBUTE_PREFIX: &str = "attributes.";

// ---------------------------------------------------------------------------
// Catalogue: the keys a definition may name
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ClassInfo {
    pub id: Uuid,
    pub is_active: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct AttributeInfo {
    pub id: Uuid,
    pub data_type: AttributeDataType,
    pub is_active: bool,
}

#[derive(Debug, Clone)]
pub struct ListInfo {
    pub is_active: bool,
    /// The criticality list (`system_role`): filtered by `criticalityValueId`, not `lookupValueId`.
    pub criticality: bool,
    /// value key -> (id, is_active)
    pub values: HashMap<String, (Uuid, bool)>,
}

/// Classes, their effective attributes and the lookup lists, by key. Three
/// small queries per request, like the UI settings model.
#[derive(Debug, Clone, Default)]
pub struct Catalogue {
    pub classes: HashMap<String, ClassInfo>,
    /// class key -> attribute key -> the attribute (own or inherited)
    pub attributes: HashMap<String, HashMap<String, AttributeInfo>>,
    pub lists: HashMap<String, ListInfo>,
}

/// A lookup list (key, is_active, criticality) with one of its values (key, id, is_active), if any.
type ListRow = (String, bool, bool, Option<String>, Option<Uuid>, Option<bool>);

impl Catalogue {
    pub async fn load(conn: &mut PgConnection) -> sqlx::Result<Catalogue> {
        let mut cat = Catalogue::default();
        let classes: Vec<(String, Uuid, bool)> =
            sqlx::query_as("SELECT key, id, is_active FROM cmdb.ci_classes").fetch_all(&mut *conn).await?;
        for (key, id, is_active) in classes {
            cat.classes.insert(key, ClassInfo { id, is_active });
        }
        let attrs: Vec<(String, String, Uuid, AttributeDataType, bool)> = sqlx::query_as(
            "WITH RECURSIVE lineage (class_id, ancestor_id, depth) AS (
               SELECT id, id, 0 FROM cmdb.ci_classes
               UNION ALL
               SELECT l.class_id, c.parent_id, l.depth + 1 FROM lineage l JOIN cmdb.ci_classes c ON c.id = l.ancestor_id
               WHERE c.parent_id IS NOT NULL AND l.depth < 64
             )
             SELECT c.key, a.key, a.id, a.data_type, a.is_active
             FROM lineage l
             JOIN cmdb.ci_classes c ON c.id = l.class_id
             JOIN cmdb.ci_attribute_definitions a ON a.class_id = l.ancestor_id",
        )
        .fetch_all(&mut *conn)
        .await?;
        for (class, key, id, data_type, is_active) in attrs {
            cat.attributes.entry(class).or_default().insert(key, AttributeInfo { id, data_type, is_active });
        }
        let lists: Vec<ListRow> = sqlx::query_as(
            "SELECT l.key, l.is_active, l.system_role IS NOT DISTINCT FROM 'criticality', v.key, v.id, v.is_active
             FROM cmdb.lookup_lists l LEFT JOIN cmdb.lookup_list_values v ON v.list_id = l.id",
        )
        .fetch_all(&mut *conn)
        .await?;
        for (list, is_active, criticality, value, id, value_active) in lists {
            let entry =
                cat.lists.entry(list).or_insert_with(|| ListInfo { is_active, criticality, values: HashMap::new() });
            if let (Some(v), Some(id), Some(a)) = (value, id, value_active) {
                entry.values.insert(v, (id, a));
            }
        }
        Ok(cat)
    }

    /// An active attribute every one of `classes` has, the same one on each
    /// (an attribute of a common ancestor).
    fn common_attribute(&self, classes: &[&str], key: &str) -> Result<AttributeInfo, AttributeProblem> {
        let mut found: Option<AttributeInfo> = None;
        for c in classes {
            let a = self.attributes.get(*c).and_then(|attrs| attrs.get(key)).filter(|a| a.is_active);
            match (a, found) {
                (None, _) => return Err(AttributeProblem::Missing((*c).to_owned())),
                (Some(a), Some(b)) if a.id != b.id => return Err(AttributeProblem::Ambiguous),
                (Some(a), _) => found = Some(*a),
            }
        }
        found.ok_or(AttributeProblem::NoClass)
    }

    /// An attribute column: an active attribute of that key on every class
    /// (not necessarily the same one: each row shows its own class's value).
    fn column_ok(&self, classes: &[&str], key: &str) -> bool {
        matches!(self.common_attribute(classes, key), Ok(_) | Err(AttributeProblem::Ambiguous))
    }
}

enum AttributeProblem {
    NoClass,
    Missing(String),
    Ambiguous,
}

// ---------------------------------------------------------------------------
// The caller's view of the classes
// ---------------------------------------------------------------------------

/// Which classes the caller may view: `None` is every class.
pub struct Viewer {
    scope: Option<HashSet<Uuid>>,
}

impl Viewer {
    pub fn of(ctx: &RequestContext) -> Viewer {
        Viewer { scope: ctx.class_scope(ClassOp::View).map(|v| v.into_iter().collect()) }
    }

    /// Whether the caller's view is limited to some classes.
    pub fn restricted(&self) -> bool {
        self.scope.is_some()
    }

    /// The class, if it exists and the caller may view it.
    pub fn class<'c>(&self, cat: &'c Catalogue, key: &str) -> Option<&'c ClassInfo> {
        cat.classes.get(key).filter(|c| self.scope.as_ref().is_none_or(|s| s.contains(&c.id)))
    }

    /// Whether the key is hidden from the caller (see the module docs).
    pub fn hides(&self, cat: &Catalogue, key: &str) -> bool {
        self.restricted() && self.class(cat, key).is_none()
    }

    /// The stored definition as this caller may see it: hidden class keys left
    /// out. Returns it and how many keys were left out.
    pub fn visible_part(&self, cat: &Catalogue, stored: &SavedViewDefinition) -> (SavedViewDefinition, usize) {
        let mut d = stored.clone();
        d.class_keys.retain(|k| !self.hides(cat, k));
        let hidden = stored.class_keys.len() - d.class_keys.len();
        (d, hidden)
    }

    /// A shared view whose classes are all hidden is not this caller's to see
    /// (§3.2): it is left out of lists and answers `404`.
    pub fn sees_shared(&self, cat: &Catalogue, stored: &SavedViewDefinition) -> bool {
        stored.class_keys.is_empty() || stored.class_keys.iter().any(|k| !self.hides(cat, k))
    }
}

/// `edited` with the stored keys hidden from the editor added back (§2.4): an
/// edit never quietly strips the parts of a view the editor cannot see.
pub fn merge_hidden(
    viewer: &Viewer,
    cat: &Catalogue,
    stored: &SavedViewDefinition,
    mut edited: SavedViewDefinition,
) -> SavedViewDefinition {
    for k in stored.class_keys.iter().filter(|k| viewer.hides(cat, k)) {
        if !edited.class_keys.contains(k) {
            edited.class_keys.push(k.clone());
        }
    }
    edited
}

// ---------------------------------------------------------------------------
// Save-time checks (§2.2, §3.1)
// ---------------------------------------------------------------------------

fn unknown_class(path: &str, key: &str) -> FieldError {
    // The same text whether the class does not exist or is hidden (§3.1).
    body_field(path, &format!("CI class \"{key}\" does not exist"), "unknown_class")
}

/// The rules the list endpoint applies (`class_required`, `unknown_attribute`,
/// `ambiguous_attribute`, `not_sortable`), plus: every class must exist and be
/// visible (`unknown_class`), and every lookup list and value must exist and be
/// active (`unknown_lookup`). Archived attributes are refused too: resolution
/// would drop them straight away. `prefix` is the definition's path.
pub fn check_references(
    def: &SavedViewDefinition,
    context: SavedViewContext,
    cat: &Catalogue,
    viewer: &Viewer,
    prefix: &str,
) -> Vec<FieldError> {
    let mut e = Vec::new();
    for (i, k) in def.class_keys.iter().enumerate() {
        if viewer.class(cat, k).is_none() {
            e.push(unknown_class(&format!("{prefix}.classKeys.{i}"), k));
        }
    }
    for (list, values) in &def.filters.lookups {
        let p = format!("{prefix}.filters.lookups.{list}");
        let Some(l) = cat.lists.get(list).filter(|l| l.is_active) else {
            e.push(body_field(&p, &format!("Lookup list \"{list}\" does not exist"), "unknown_lookup"));
            continue;
        };
        for (i, v) in values.iter().enumerate() {
            if !l.values.get(v).is_some_and(|(_, active)| *active) {
                e.push(body_field(
                    &format!("{p}.{i}"),
                    &format!("\"{v}\" is not an active value of lookup list \"{list}\""),
                    "unknown_lookup",
                ));
            }
        }
    }
    // Class errors are reported once, above; attribute rules need the classes.
    if !e.iter().any(|x| x.code == "unknown_class") && context == SavedViewContext::Inventory {
        let classes: Vec<&str> = def.class_keys.iter().map(String::as_str).collect();
        if let Some(sort) = &def.sort
            && let Some(key) = sort.field.strip_prefix(ATTRIBUTE_PREFIX)
        {
            let path = format!("{prefix}.sort.field");
            match cat.common_attribute(&classes, key) {
                Ok(a) if !is_sortable(a.data_type) => e.push(body_field(
                    &path,
                    &format!("Attribute \"{key}\" is a reference and cannot be sorted on"),
                    "not_sortable",
                )),
                Ok(_) => {}
                Err(AttributeProblem::NoClass) => {
                    e.push(body_field(&path, "Sorting by an attribute needs a class in classKeys", "class_required"))
                }
                Err(AttributeProblem::Missing(c)) => e.push(body_field(
                    &path,
                    &format!("Attribute \"{key}\" is not an active attribute of class \"{c}\""),
                    "unknown_attribute",
                )),
                Err(AttributeProblem::Ambiguous) => e.push(body_field(
                    &path,
                    &format!("Attribute \"{key}\" is a different attribute in the classes of classKeys"),
                    "ambiguous_attribute",
                )),
            }
        }
        for (i, c) in def.columns.iter().enumerate() {
            let Some(key) = c.strip_prefix(ATTRIBUTE_PREFIX) else { continue };
            let path = format!("{prefix}.columns.{i}");
            match cat.common_attribute(&classes, key) {
                Ok(_) | Err(AttributeProblem::Ambiguous) => {}
                Err(AttributeProblem::NoClass) => {
                    e.push(body_field(&path, "An attribute column needs a class in classKeys", "class_required"))
                }
                Err(AttributeProblem::Missing(class)) => e.push(body_field(
                    &path,
                    &format!("Attribute \"{key}\" is not an active attribute of class \"{class}\""),
                    "unknown_attribute",
                )),
            }
        }
    }
    e
}

// ---------------------------------------------------------------------------
// Resolution (§2.6)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SavedViewState {
    /// Everything resolves
    Ok,
    /// Something that only narrows or presents results was dropped (see issues); the query is still exact
    Degraded,
    /// A filter would disappear entirely, which would widen the result: the view is not applied and has no query
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SavedViewIssueCode {
    /// Info: the class is archived; the view still applies to it
    ClassArchived,
    /// Some of the view's classes are not available to the caller; they are left out, and only counted
    NotAvailable,
    /// The class no longer exists (only reported to callers who may view every class)
    UnknownClass,
    /// None of the view's classes is left: the view is unavailable
    NoClassLeft,
    /// The attribute is not (or no longer) an active attribute of every class: the column or sort is dropped
    UnknownAttribute,
    /// The lookup value is archived or deleted: it is dropped from the filter
    UnknownLookupValue,
    /// The lookup list is archived or deleted, or none of the filter's values is left: the view is unavailable
    LookupFilterGone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SavedViewIssueSeverity {
    Info,
    Warning,
}

/// Something resolution dropped or flagged
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SavedViewIssue {
    /// Path in the stored definition, e.g. "definition.columns.2"
    pub path: String,
    #[schema(inline)]
    pub code: SavedViewIssueCode,
    #[schema(inline)]
    pub severity: SavedViewIssueSeverity,
    pub message: String,
}

/// The list or search parameters the view stands for, ready for the URL. The
/// server does not trust them: the list and search endpoints apply the
/// caller's class rights to every request.
#[derive(Debug, Clone, Default, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SavedViewQuery {
    /// Class ids, comma-separated
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class_id: Option<String>,
    /// "true" or "false"; present with classId
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_subclasses: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
    /// Lookup value ids, comma-separated
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lookup_value_id: Option<String>,
    /// Criticality value ids, comma-separated (a filter on the criticality list)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub criticality_value_id: Option<String>,
    #[schema(schema_with = super::definition::active_schema)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<SavedViewActive>,
    #[schema(schema_with = super::definition::deleted_schema)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted: Option<SavedViewDeleted>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip_within: Option<String>,
    /// Inventory views: the list's sort parameter ("-" prefix for descending)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
}

/// The view as it applies today, for the caller
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SavedViewResolution {
    #[schema(inline)]
    pub state: SavedViewState,
    /// Null when the view is unavailable: it is never applied
    pub query: Option<SavedViewQuery>,
    /// Inventory views: the columns that resolve, in order; empty: the baseline columns
    pub columns: Vec<String>,
    pub issues: Vec<SavedViewIssue>,
}

struct Resolver {
    issues: Vec<SavedViewIssue>,
    degraded: bool,
    unavailable: bool,
}

impl Resolver {
    fn flag(&mut self, path: String, code: SavedViewIssueCode, message: String) {
        let severity = match code {
            SavedViewIssueCode::ClassArchived => SavedViewIssueSeverity::Info,
            SavedViewIssueCode::NoClassLeft | SavedViewIssueCode::LookupFilterGone => {
                self.unavailable = true;
                SavedViewIssueSeverity::Warning
            }
            _ => {
                self.degraded = true;
                SavedViewIssueSeverity::Warning
            }
        };
        self.issues.push(SavedViewIssue { path, code, severity, message });
    }
}

fn join(ids: &[Uuid]) -> Option<String> {
    (!ids.is_empty()).then(|| ids.iter().map(Uuid::to_string).collect::<Vec<_>>().join(","))
}

/// The stored definition as it applies today for this caller (§2.6).
pub fn resolve(
    stored: &SavedViewDefinition,
    context: SavedViewContext,
    cat: &Catalogue,
    viewer: &Viewer,
) -> SavedViewResolution {
    let mut r = Resolver { issues: Vec::new(), degraded: false, unavailable: false };

    // Classes: a class among several may go; all of them may not.
    let mut classes: Vec<(&str, &ClassInfo)> = Vec::new();
    let mut hidden = 0;
    for (i, k) in stored.class_keys.iter().enumerate() {
        match viewer.class(cat, k) {
            Some(c) => {
                if !c.is_active {
                    r.flag(
                        format!("definition.classKeys.{i}"),
                        SavedViewIssueCode::ClassArchived,
                        format!("CI class \"{k}\" is archived"),
                    );
                }
                classes.push((k, c));
            }
            None if viewer.restricted() => hidden += 1,
            None => r.flag(
                format!("definition.classKeys.{i}"),
                SavedViewIssueCode::UnknownClass,
                format!("CI class \"{k}\" no longer exists and is left out"),
            ),
        }
    }
    if hidden > 0 {
        let message = if hidden == 1 {
            "1 of the view's classes is not available to you and is left out".to_owned()
        } else {
            format!("{hidden} of the view's classes are not available to you and are left out")
        };
        r.flag("definition.classKeys".into(), SavedViewIssueCode::NotAvailable, message);
    }
    if !stored.class_keys.is_empty() && classes.is_empty() {
        r.flag(
            "definition.classKeys".into(),
            SavedViewIssueCode::NoClassLeft,
            "None of the view's classes is left; the view would show every class, so it is not applied".into(),
        );
    }
    let class_keys: Vec<&str> = classes.iter().map(|(k, _)| *k).collect();

    // Lookup filters: a value among several may go; a whole filter may not.
    let (mut values, mut criticality) = (Vec::new(), Vec::new());
    for (list, keys) in &stored.filters.lookups {
        let p = format!("definition.filters.lookups.{list}");
        let Some(l) = cat.lists.get(list).filter(|l| l.is_active) else {
            r.flag(
                p,
                SavedViewIssueCode::LookupFilterGone,
                format!("Lookup list \"{list}\" no longer exists or is archived; the view is not applied"),
            );
            continue;
        };
        let mut kept = Vec::new();
        for (i, v) in keys.iter().enumerate() {
            match l.values.get(v) {
                Some((id, true)) => kept.push(*id),
                _ => r.flag(
                    format!("{p}.{i}"),
                    SavedViewIssueCode::UnknownLookupValue,
                    format!("\"{v}\" is no longer an active value of lookup list \"{list}\" and is left out"),
                ),
            }
        }
        if kept.is_empty() {
            r.flag(
                p,
                SavedViewIssueCode::LookupFilterGone,
                format!("No value of the \"{list}\" filter is left; the view is not applied"),
            );
        }
        if l.criticality { criticality.extend(kept) } else { values.extend(kept) }
    }

    // Sort and columns only present results: they may go.
    let mut sort = None;
    let mut columns = Vec::new();
    if context == SavedViewContext::Inventory {
        if let Some(s) = &stored.sort {
            let ok = match s.field.strip_prefix(ATTRIBUTE_PREFIX) {
                Some(key) => cat.common_attribute(&class_keys, key).is_ok_and(|a| is_sortable(a.data_type)),
                None => true,
            };
            if ok {
                let desc = s.direction == crate::modules::ui_settings::document::UiSortDirection::Desc;
                sort = Some(format!("{}{}", if desc { "-" } else { "" }, s.field));
            } else {
                r.flag(
                    "definition.sort.field".into(),
                    SavedViewIssueCode::UnknownAttribute,
                    format!("Sort by \"{}\" is no longer possible; the list sorts by label", s.field),
                );
            }
        }
        for (i, c) in stored.columns.iter().enumerate() {
            match c.strip_prefix(ATTRIBUTE_PREFIX) {
                Some(key) if !cat.column_ok(&class_keys, key) => r.flag(
                    format!("definition.columns.{i}"),
                    SavedViewIssueCode::UnknownAttribute,
                    format!("Column \"{c}\" no longer exists and is left out"),
                ),
                _ => columns.push(c.clone()),
            }
        }
    }

    let state = if r.unavailable {
        SavedViewState::Unavailable
    } else if r.degraded {
        SavedViewState::Degraded
    } else {
        SavedViewState::Ok
    };
    let query = (state != SavedViewState::Unavailable).then(|| {
        let ids: Vec<Uuid> = classes.iter().map(|(_, c)| c.id).collect();
        SavedViewQuery {
            class_id: join(&ids),
            include_subclasses: (!ids.is_empty()).then(|| stored.include_subclasses.to_string()),
            q: stored.filters.q.clone(),
            lookup_value_id: join(&values),
            criticality_value_id: join(&criticality),
            active: stored.filters.active,
            deleted: stored.filters.deleted,
            ip_within: stored.filters.ip_within.clone(),
            sort,
            limit: stored.page_size,
        }
    });
    SavedViewResolution { state, query, columns, issues: r.issues }
}
