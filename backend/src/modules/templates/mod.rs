//! Starter templates: ready-made data models an administrator installs on a
//! bare database (Administration > Templates, or `shadoucmdb seed --template`).
//!
//! A template is data compiled into the binary. Installing it adds every row
//! whose key is not there yet and leaves existing rows alone, so a re-install is
//! a no-op and an administrator's renames and archives survive. Each created
//! row gets an audit entry with the installing user as actor.

mod it_infrastructure;

use std::collections::{HashMap, HashSet};

use axum::http::Method;
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use utoipa::ToSchema;
use uuid::Uuid;

use super::classes::{AttributeDefinitions, CiClasses, RelationshipRules, RelationshipTypes};
use super::lookups::{Environments, Locations, Statuses};
use super::simple_resource::Resource;
use crate::api::context::RequestContext;
use crate::api::route::{In, Json, KeyPath, NoBody, NoPath, NoQuery, Route, route};
use crate::auth::permissions::GlobalPermission;
use crate::data::classes as class_data;
use crate::data::crud::{self, AuditAction, AuditEntry, ColumnSet};
use crate::http::error::{AppError, ErrorCode};

// ---------------------------------------------------------------------------
// Template content
// ---------------------------------------------------------------------------

pub struct Status {
    pub key: &'static str,
    pub name: &'static str,
    pub is_operational: bool,
    pub description: &'static str,
}

pub struct Location {
    pub key: &'static str,
    pub name: &'static str,
    pub location_type: &'static str,
    /// Key of a location listed earlier.
    pub parent: Option<&'static str>,
    pub address: Option<&'static str>,
}

#[derive(Default)]
pub struct Attr {
    pub key: &'static str,
    pub label: &'static str,
    pub data_type: &'static str,
    pub enum_values: Option<&'static [&'static str]>,
    /// Key of a class in the same template.
    pub reference_class: Option<&'static str>,
    pub is_required: bool,
    pub group_name: Option<&'static str>,
    pub help_text: Option<&'static str>,
    /// JSON object, e.g. `{"min":1}`.
    pub validation: Option<&'static str>,
}

pub fn attr(key: &'static str, label: &'static str, data_type: &'static str) -> Attr {
    Attr { key, label, data_type, ..Attr::default() }
}

impl Attr {
    pub fn group(mut self, g: &'static str) -> Self {
        self.group_name = Some(g);
        self
    }
    pub fn valid(mut self, v: &'static str) -> Self {
        self.validation = Some(v);
        self
    }
    pub fn values(mut self, v: &'static [&'static str]) -> Self {
        self.enum_values = Some(v);
        self
    }
    pub fn required(mut self) -> Self {
        self.is_required = true;
        self
    }
    pub fn refers(mut self, class: &'static str) -> Self {
        self.reference_class = Some(class);
        self
    }
    pub fn help(mut self, text: &'static str) -> Self {
        self.help_text = Some(text);
        self
    }
}

pub struct Class {
    pub key: &'static str,
    pub name: &'static str,
    /// Key of a class listed earlier.
    pub parent: Option<&'static str>,
    pub is_abstract: bool,
    pub description: &'static str,
    pub color: Option<&'static str>,
    pub attributes: Vec<Attr>,
}

pub struct RelationshipType {
    pub key: &'static str,
    pub name: &'static str,
    pub forward: &'static str,
    pub reverse: &'static str,
    pub directional: bool,
}

/// A relationship rule by keys: type, source class, target class.
pub struct Rule {
    pub kind: &'static str,
    pub source: &'static str,
    pub target: &'static str,
}

pub struct Content {
    pub statuses: &'static [Status],
    /// key, name
    pub environments: &'static [(&'static str, &'static str)],
    /// Parents before children.
    pub locations: &'static [Location],
    /// Parents before children.
    pub classes: Vec<Class>,
    pub relationship_types: &'static [RelationshipType],
    pub relationship_rules: &'static [Rule],
}

pub struct Template {
    pub key: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub content: fn() -> Content,
}

pub const TEMPLATES: &[Template] = &[it_infrastructure::TEMPLATE];

pub fn find(key: &str) -> Option<&'static Template> {
    TEMPLATES.iter().find(|t| t.key == key)
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

/// Rows per kind
#[derive(Debug, Default, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemplateCounts {
    pub classes: i64,
    pub attribute_definitions: i64,
    pub relationship_types: i64,
    pub relationship_rules: i64,
    pub statuses: i64,
    pub environments: i64,
    pub locations: i64,
}

impl TemplateCounts {
    fn total(&self) -> i64 {
        self.classes
            + self.attribute_definitions
            + self.relationship_types
            + self.relationship_rules
            + self.statuses
            + self.environments
            + self.locations
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TemplateStatus {
    /// None of its rows exist
    NotInstalled,
    /// Some rows exist (installed earlier and then partly deleted, or overlapping with rows an administrator made)
    Partial,
    /// Every row exists (possibly renamed or archived since)
    Installed,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemplateClass {
    pub key: String,
    pub name: String,
    pub is_abstract: bool,
    /// Attributes defined directly on the class
    pub attribute_count: i64,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StarterTemplate {
    pub key: String,
    pub name: String,
    pub description: String,
    /// What the template brings
    #[schema(inline)]
    pub contents: TemplateCounts,
    /// How many of those rows already exist (matched by key)
    #[schema(inline)]
    pub present: TemplateCounts,
    #[schema(inline)]
    pub status: TemplateStatus,
    #[schema(inline)]
    pub classes: Vec<TemplateClass>,
}

/// Every starter template (not paginated: they ship with the server)
#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StarterTemplateList {
    pub data: Vec<StarterTemplate>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemplateInstallResult {
    pub template: String,
    /// Rows added by this install (each has an audit entry)
    #[schema(inline)]
    pub created: TemplateCounts,
    /// Rows that already existed and were left as they are
    #[schema(inline)]
    pub existing: TemplateCounts,
    /// Rows not installed because they would clash with the current data model
    pub skipped: Vec<String>,
}

// ---------------------------------------------------------------------------
// Current state
// ---------------------------------------------------------------------------

type KeyMap = HashMap<String, Uuid>;

/// `key -> id` for one of the keyed tables (the name is always a constant).
async fn key_map(c: &mut PgConnection, table: &'static str) -> sqlx::Result<KeyMap> {
    let rows: Vec<(String, Uuid)> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT key, id FROM {table}"))).fetch_all(c).await?;
    Ok(rows.into_iter().collect())
}

struct State {
    statuses: KeyMap,
    environments: KeyMap,
    locations: KeyMap,
    classes: KeyMap,
    types: KeyMap,
    /// (class id, attribute key)
    attributes: HashSet<(Uuid, String)>,
    /// (type id, source class id, target class id)
    rules: HashSet<(Uuid, Uuid, Uuid)>,
}

impl State {
    async fn load(c: &mut PgConnection) -> sqlx::Result<Self> {
        let attributes: Vec<(Uuid, String)> =
            sqlx::query_as("SELECT class_id, key FROM ci_attribute_definitions").fetch_all(&mut *c).await?;
        let rules: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
            "SELECT relationship_type_id, source_class_id, target_class_id FROM relationship_type_rules",
        )
        .fetch_all(&mut *c)
        .await?;
        Ok(State {
            statuses: key_map(c, "statuses").await?,
            environments: key_map(c, "environments").await?,
            locations: key_map(c, "locations").await?,
            classes: key_map(c, "ci_classes").await?,
            types: key_map(c, "relationship_types").await?,
            attributes: attributes.into_iter().collect(),
            rules: rules.into_iter().collect(),
        })
    }

    fn has_attribute(&self, class: &str, key: &str) -> bool {
        self.classes.get(class).is_some_and(|id| self.attributes.contains(&(*id, key.to_owned())))
    }

    fn has_rule(&self, r: &Rule) -> bool {
        match (self.types.get(r.kind), self.classes.get(r.source), self.classes.get(r.target)) {
            (Some(t), Some(s), Some(g)) => self.rules.contains(&(*t, *s, *g)),
            _ => false,
        }
    }
}

fn contents(c: &Content) -> TemplateCounts {
    TemplateCounts {
        classes: c.classes.len() as i64,
        attribute_definitions: c.classes.iter().map(|k| k.attributes.len() as i64).sum(),
        relationship_types: c.relationship_types.len() as i64,
        relationship_rules: c.relationship_rules.len() as i64,
        statuses: c.statuses.len() as i64,
        environments: c.environments.len() as i64,
        locations: c.locations.len() as i64,
    }
}

fn present(c: &Content, s: &State) -> TemplateCounts {
    let n = |it: &mut dyn Iterator<Item = bool>| it.filter(|b| *b).count() as i64;
    TemplateCounts {
        classes: n(&mut c.classes.iter().map(|k| s.classes.contains_key(k.key))),
        attribute_definitions: n(&mut c
            .classes
            .iter()
            .flat_map(|k| k.attributes.iter().map(move |a| (k.key, a.key)))
            .map(|(k, a)| s.has_attribute(k, a))),
        relationship_types: n(&mut c.relationship_types.iter().map(|t| s.types.contains_key(t.key))),
        relationship_rules: n(&mut c.relationship_rules.iter().map(|r| s.has_rule(r))),
        statuses: n(&mut c.statuses.iter().map(|x| s.statuses.contains_key(x.key))),
        environments: n(&mut c.environments.iter().map(|(k, _)| s.environments.contains_key(*k))),
        locations: n(&mut c.locations.iter().map(|l| s.locations.contains_key(l.key))),
    }
}

fn describe(t: &Template, state: &State) -> StarterTemplate {
    let content = (t.content)();
    let contents = contents(&content);
    let present = present(&content, state);
    let status = match present.total() {
        0 => TemplateStatus::NotInstalled,
        n if n == contents.total() => TemplateStatus::Installed,
        _ => TemplateStatus::Partial,
    };
    StarterTemplate {
        key: t.key.into(),
        name: t.name.into(),
        description: t.description.into(),
        classes: content
            .classes
            .iter()
            .map(|k| TemplateClass {
                key: k.key.into(),
                name: k.name.into(),
                is_abstract: k.is_abstract,
                attribute_count: k.attributes.len() as i64,
            })
            .collect(),
        contents,
        present,
        status,
    }
}

pub async fn list(pool: &PgPool) -> Result<StarterTemplateList, AppError> {
    let mut conn = pool.acquire().await?;
    let state = State::load(&mut conn).await?;
    Ok(StarterTemplateList { data: TEMPLATES.iter().map(|t| describe(t, &state)).collect() })
}

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

struct Installer<'c> {
    conn: &'c mut PgConnection,
    audit: Vec<AuditEntry>,
}

impl Installer<'_> {
    /// Insert one row through the resource's table and columns, queueing its audit entry.
    async fn insert<R: Resource>(&mut self, columns: ColumnSet) -> Result<Uuid, AppError> {
        let row: R::Dto = crud::insert_row(self.conn, R::TABLE, R::COLUMNS, columns).await?;
        let id = R::id(&row);
        self.audit.push(AuditEntry {
            action: AuditAction::Create,
            entity_type: R::TABLE,
            entity_id: id,
            old_value: None,
            new_value: Some(crud::json(&row)),
        });
        Ok(id)
    }
}

fn text(s: &str) -> String {
    s.to_owned()
}

/// Installs a template in the caller's transaction. Idempotent: rows are
/// matched by key and existing ones are never changed.
pub async fn install(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    template: &Template,
) -> Result<TemplateInstallResult, AppError> {
    // Two concurrent installs would race on the unique keys; serialise them.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('shadoucmdb:template:' || $1))")
        .bind(template.key)
        .execute(&mut *conn)
        .await?;
    let content = (template.content)();
    let mut state = State::load(conn).await?;
    let existing = present(&content, &state);
    let mut created = TemplateCounts::default();
    let mut skipped = Vec::new();
    let mut ins = Installer { conn, audit: Vec::new() };

    for (i, s) in content.statuses.iter().enumerate() {
        if state.statuses.contains_key(s.key) {
            continue;
        }
        let mut c = ColumnSet::default();
        c.opt("key", Some(text(s.key)))
            .opt("name", Some(text(s.name)))
            .opt("description", Some(text(s.description)))
            .opt("is_operational", Some(s.is_operational))
            .opt("sort_order", Some(i as i32 * 10));
        let id = ins.insert::<Statuses>(c).await?;
        state.statuses.insert(s.key.into(), id);
        created.statuses += 1;
    }

    for (i, (key, name)) in content.environments.iter().enumerate() {
        if state.environments.contains_key(*key) {
            continue;
        }
        let mut c = ColumnSet::default();
        c.opt("key", Some(text(key))).opt("name", Some(text(name))).opt("sort_order", Some(i as i32 * 10));
        let id = ins.insert::<Environments>(c).await?;
        state.environments.insert((*key).into(), id);
        created.environments += 1;
    }

    for loc in content.locations {
        if state.locations.contains_key(loc.key) {
            continue;
        }
        let parent_id = loc.parent.and_then(|p| state.locations.get(p).copied());
        let mut c = ColumnSet::default();
        c.opt("key", Some(text(loc.key)))
            .opt("name", Some(text(loc.name)))
            .opt("location_type", Some(text(loc.location_type)))
            .opt("address", loc.address.map(|a| Some(text(a))))
            .opt("parent_id", parent_id.map(Some));
        let id = ins.insert::<Locations>(c).await?;
        state.locations.insert(loc.key.into(), id);
        created.locations += 1;
    }

    for (i, cls) in content.classes.iter().enumerate() {
        if state.classes.contains_key(cls.key) {
            continue;
        }
        let parent_id = cls.parent.and_then(|p| state.classes.get(p).copied());
        let mut c = ColumnSet::default();
        c.opt("key", Some(text(cls.key)))
            .opt("name", Some(text(cls.name)))
            .opt("description", Some(text(cls.description)))
            .opt("is_abstract", Some(cls.is_abstract))
            .opt("parent_id", parent_id.map(Some))
            .opt("color", cls.color.map(|v| Some(text(v))))
            .opt("sort_order", Some(i as i32 * 10));
        let id = ins.insert::<CiClasses>(c).await?;
        state.classes.insert(cls.key.into(), id);
        created.classes += 1;
    }

    for cls in &content.classes {
        let class_id = state.classes[cls.key];
        for (i, a) in cls.attributes.iter().enumerate() {
            if state.attributes.contains(&(class_id, a.key.to_owned())) {
                continue;
            }
            if let Some(on) = class_data::attribute_key_clash(ins.conn, class_id, a.key, Uuid::nil()).await? {
                skipped.push(format!(
                    "attribute {}.{}: already defined on class \"{on}\" in the same lineage",
                    cls.key, a.key
                ));
                continue;
            }
            let reference_class_id = match a.reference_class {
                Some(k) => match state.classes.get(k) {
                    Some(id) => Some(*id),
                    None => {
                        skipped.push(format!("attribute {}.{}: class \"{k}\" does not exist", cls.key, a.key));
                        continue;
                    }
                },
                None => None,
            };
            let validation: Option<Value> =
                a.validation.map(serde_json::from_str).transpose().map_err(|_| AppError::internal())?;
            let mut c = ColumnSet::default();
            c.opt("class_id", Some(class_id))
                .opt("key", Some(text(a.key)))
                .opt("label", Some(text(a.label)))
                .opt("data_type", Some(text(a.data_type)))
                .opt("is_required", Some(a.is_required))
                .opt("enum_values", a.enum_values.map(|v| Some(json!(v))))
                .opt("reference_class_id", reference_class_id.map(Some))
                .opt("validation", validation.map(Some))
                .opt("group_name", a.group_name.map(|g| Some(text(g))))
                .opt("help_text", a.help_text.map(|h| Some(text(h))))
                .opt("sort_order", Some(i as i32 * 10));
            ins.insert::<AttributeDefinitions>(c).await?;
            state.attributes.insert((class_id, a.key.to_owned()));
            created.attribute_definitions += 1;
        }
    }

    for (i, t) in content.relationship_types.iter().enumerate() {
        if state.types.contains_key(t.key) {
            continue;
        }
        let mut c = ColumnSet::default();
        c.opt("key", Some(text(t.key)))
            .opt("name", Some(text(t.name)))
            .opt("forward_label", Some(text(t.forward)))
            .opt("reverse_label", Some(text(t.reverse)))
            .opt("is_directional", Some(t.directional))
            .opt("sort_order", Some(i as i32 * 10));
        let id = ins.insert::<RelationshipTypes>(c).await?;
        state.types.insert(t.key.into(), id);
        created.relationship_types += 1;
    }

    for r in content.relationship_rules {
        if state.has_rule(r) {
            continue;
        }
        let ids = (state.types.get(r.kind), state.classes.get(r.source), state.classes.get(r.target));
        let (Some(t), Some(s), Some(g)) = ids else {
            skipped.push(format!("rule {} {} -> {}: type or class missing", r.kind, r.source, r.target));
            continue;
        };
        let (t, s, g) = (*t, *s, *g);
        let mut c = ColumnSet::default();
        c.opt("relationship_type_id", Some(t)).opt("source_class_id", Some(s)).opt("target_class_id", Some(g));
        ins.insert::<RelationshipRules>(c).await?;
        state.rules.insert((t, s, g));
        created.relationship_rules += 1;
    }

    let Installer { conn, audit } = ins;
    crud::write_audit(conn, ctx, audit).await?;
    Ok(TemplateInstallResult { template: template.key.into(), created, existing, skipped })
}

pub async fn install_by_key(pool: &PgPool, ctx: &RequestContext, key: &str) -> Result<TemplateInstallResult, AppError> {
    let template = find(key).ok_or_else(|| AppError::not_found(format!("Template {key} not found")))?;
    let mut tx = pool.begin().await?;
    let result = install(&mut tx, ctx, template).await?;
    tx.commit().await?;
    Ok(result)
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

const TAG: &str = "Templates";

pub fn routes() -> Vec<Route> {
    vec![
        route(Method::GET, "/api/v1/admin/templates", "listTemplates")
            .tag(TAG)
            .summary("List starter templates and whether each is installed")
            .requires(GlobalPermission::DatamodelManage)
            .handle(|api, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| async move {
                Ok(Json(list(&api.pool).await?))
            }),
        route(Method::POST, "/api/v1/admin/templates/{key}/install", "installTemplate")
            .tag(TAG)
            .summary("Install a starter template (idempotent)")
            .description(
                "Adds every class, attribute, relationship type and rule, status, environment and location of the \
                 template whose key does not exist yet, in one transaction. Existing rows are left unchanged, so \
                 installing again is a no-op and renamed or archived rows stay as they are. Each created row is \
                 written to the audit log.",
            )
            .requires(GlobalPermission::DatamodelManage)
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(KeyPath(key), NoQuery, NoBody): In<KeyPath, NoQuery, NoBody>| async move {
                Ok(Json(install_by_key(&api.pool, &api.ctx, &key).await?))
            }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_infrastructure_is_the_former_seed() {
        let t = find("it_infrastructure").expect("template");
        let c = contents(&(t.content)());
        assert_eq!(c.classes, 8);
        assert_eq!(c.attribute_definitions, 35);
        assert_eq!(c.relationship_types, 4);
        assert_eq!(c.relationship_rules, 12);
        assert_eq!(c.statuses, 5);
        assert_eq!(c.environments, 5);
        assert_eq!(c.locations, 7);
    }

    #[test]
    fn templates_are_self_consistent() {
        let key = regex::Regex::new(crate::api::schemas::KEY_PATTERN).unwrap();
        for t in TEMPLATES {
            assert!(key.is_match(t.key), "template key {}", t.key);
            let c = (t.content)();
            let mut classes = HashSet::new();
            for cls in &c.classes {
                assert!(cls.parent.is_none_or(|p| classes.contains(p)), "{}: parent listed later", cls.key);
                assert!(classes.insert(cls.key), "duplicate class {}", cls.key);
                let mut attrs = HashSet::new();
                for a in &cls.attributes {
                    assert!(attrs.insert(a.key), "duplicate attribute {}.{}", cls.key, a.key);
                    assert_eq!(a.data_type == "enum", a.enum_values.is_some(), "{}.{}", cls.key, a.key);
                    assert_eq!(a.data_type == "reference", a.reference_class.is_some(), "{}.{}", cls.key, a.key);
                    if let Some(v) = a.validation {
                        assert!(serde_json::from_str::<serde_json::Map<String, Value>>(v).is_ok(), "{v}");
                    }
                }
            }
            for cls in &c.classes {
                for a in &cls.attributes {
                    assert!(a.reference_class.is_none_or(|k| classes.contains(k)), "{}.{}", cls.key, a.key);
                }
            }
            let types: HashSet<&str> = c.relationship_types.iter().map(|t| t.key).collect();
            for r in c.relationship_rules {
                assert!(types.contains(r.kind) && classes.contains(r.source) && classes.contains(r.target));
            }
            let mut locations = HashSet::new();
            for l in c.locations {
                assert!(l.parent.is_none_or(|p| locations.contains(p)), "{}: parent listed later", l.key);
                locations.insert(l.key);
            }
        }
    }
}
