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

use super::areas::Areas;
use super::classes::{AttributeDefinitions, CiClasses, RelationshipRules, RelationshipTypes};
use super::lookups::{LookupListValues, LookupLists};
use super::simple_resource::Resource;
use crate::api::context::RequestContext;
use crate::api::route::{In, Json, KeyPath, NoBody, NoPath, NoQuery, Route, route};
use crate::auth::permissions::GlobalPermission;
use crate::data::classes as class_data;
use crate::data::crud::{self, AuditAction, AuditEntry, ColumnSet};
use crate::http::error::{AppError, ErrorCode};
use crate::schema::model::Model;
use crate::schema::{self as engine, Purge, SchemaChange, Scope};

// ---------------------------------------------------------------------------
// Template content
// ---------------------------------------------------------------------------

/// An admin-editable lookup list and its values (key, name, description).
pub struct LookupList {
    pub key: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub values: &'static [(&'static str, &'static str, Option<&'static str>)],
}

#[derive(Default)]
pub struct Attr {
    pub key: &'static str,
    pub label: &'static str,
    pub data_type: &'static str,
    pub enum_values: Option<&'static [&'static str]>,
    /// Key of a class in the same template.
    pub reference_class: Option<&'static str>,
    /// Key of a lookup list in the same template.
    pub lookup_list: Option<&'static str>,
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
    pub fn lookup(mut self, list: &'static str) -> Self {
        self.lookup_list = Some(list);
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

/// The area (menu tab, PostgreSQL schema) the template's types are created in.
pub struct AreaSpec {
    pub key: &'static str,
    pub name: &'static str,
    pub description: &'static str,
}

pub struct Content {
    pub area: AreaSpec,
    pub lookup_lists: &'static [LookupList],
    /// Parents before children. A class whose lineage has a "name" field is labelled by it.
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
    pub areas: i64,
    pub classes: i64,
    pub attribute_definitions: i64,
    pub relationship_types: i64,
    pub relationship_rules: i64,
    pub lookup_lists: i64,
    pub lookup_list_values: i64,
}

impl TemplateCounts {
    fn total(&self) -> i64 {
        self.areas
            + self.classes
            + self.attribute_definitions
            + self.relationship_types
            + self.relationship_rules
            + self.lookup_lists
            + self.lookup_list_values
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
    /// The DDL the install ran (null when every table and column already existed)
    #[schema(required = true)]
    pub schema_change: Option<SchemaChange>,
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
    areas: KeyMap,
    lists: KeyMap,
    /// (list id, value key)
    list_values: HashSet<(Uuid, String)>,
    classes: KeyMap,
    types: KeyMap,
    /// (class id, attribute key)
    attributes: HashSet<(Uuid, String)>,
    /// class id -> parent id
    parents: HashMap<Uuid, Option<Uuid>>,
    /// (type id, source class id, target class id)
    rules: HashSet<(Uuid, Uuid, Uuid)>,
    /// Classes this install gave a title attribute
    titled: Vec<Uuid>,
}

impl State {
    async fn load(c: &mut PgConnection) -> sqlx::Result<Self> {
        let attributes: Vec<(Uuid, String)> =
            sqlx::query_as("SELECT class_id, key FROM ci_attribute_definitions").fetch_all(&mut *c).await?;
        let parents: Vec<(Uuid, Option<Uuid>)> =
            sqlx::query_as("SELECT id, parent_id FROM ci_classes").fetch_all(&mut *c).await?;
        let list_values: Vec<(Uuid, String)> =
            sqlx::query_as("SELECT list_id, key FROM lookup_list_values").fetch_all(&mut *c).await?;
        let rules: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
            "SELECT relationship_type_id, source_class_id, target_class_id FROM relationship_type_rules",
        )
        .fetch_all(&mut *c)
        .await?;
        Ok(State {
            areas: key_map(c, "areas").await?,
            lists: key_map(c, "lookup_lists").await?,
            list_values: list_values.into_iter().collect(),
            classes: key_map(c, "ci_classes").await?,
            types: key_map(c, "relationship_types").await?,
            attributes: attributes.into_iter().collect(),
            parents: parents.into_iter().collect(),
            rules: rules.into_iter().collect(),
            titled: Vec::new(),
        })
    }

    /// The class, or a class in its lineage (above or below), defines the key: a
    /// CI of the class carries it. After migration 0016 a former fixed field can
    /// sit on the subclasses whose CIs held values rather than on the template's class.
    fn has_attribute(&self, class: &str, key: &str) -> bool {
        let Some(id) = self.classes.get(class) else { return false };
        self.attributes.iter().any(|(c, k)| k == key && (self.is_a(*id, *c) || self.is_a(*c, *id)))
    }

    fn is_a(&self, class: Uuid, ancestor: Uuid) -> bool {
        let mut next = Some(class);
        for _ in 0..64 {
            match next {
                Some(c) if c == ancestor => return true,
                Some(c) => next = self.parents.get(&c).copied().flatten(),
                None => return false,
            }
        }
        false
    }

    fn has_value(&self, list: &str, key: &str) -> bool {
        self.lists.get(list).is_some_and(|id| self.list_values.contains(&(*id, key.to_owned())))
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
        areas: 1,
        classes: c.classes.len() as i64,
        attribute_definitions: c.classes.iter().map(|k| k.attributes.len() as i64).sum(),
        relationship_types: c.relationship_types.len() as i64,
        relationship_rules: c.relationship_rules.len() as i64,
        lookup_lists: c.lookup_lists.len() as i64,
        lookup_list_values: c.lookup_lists.iter().map(|l| l.values.len() as i64).sum(),
    }
}

fn present(c: &Content, s: &State) -> TemplateCounts {
    let n = |it: &mut dyn Iterator<Item = bool>| it.filter(|b| *b).count() as i64;
    TemplateCounts {
        areas: n(&mut std::iter::once(s.areas.contains_key(c.area.key))),
        classes: n(&mut c.classes.iter().map(|k| s.classes.contains_key(k.key))),
        attribute_definitions: n(&mut c
            .classes
            .iter()
            .flat_map(|k| k.attributes.iter().map(move |a| (k.key, a.key)))
            .map(|(k, a)| s.has_attribute(k, a))),
        relationship_types: n(&mut c.relationship_types.iter().map(|t| s.types.contains_key(t.key))),
        relationship_rules: n(&mut c.relationship_rules.iter().map(|r| s.has_rule(r))),
        lookup_lists: n(&mut c.lookup_lists.iter().map(|l| s.lists.contains_key(l.key))),
        lookup_list_values: n(&mut c
            .lookup_lists
            .iter()
            .flat_map(|l| l.values.iter().map(move |v| (l.key, v.0)))
            .map(|(l, v)| s.has_value(l, v))),
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
    // Two concurrent installs would race on the unique keys, and any data model
    // change must wait for the schema; serialise them.
    engine::lock(conn).await?;
    let content = (template.content)();
    let mut state = State::load(conn).await?;
    let existing = present(&content, &state);
    let mut created = TemplateCounts::default();
    let mut skipped = Vec::new();
    let mut ins = Installer { conn, audit: Vec::new() };

    for (i, list) in content.lookup_lists.iter().enumerate() {
        let list_id = match state.lists.get(list.key) {
            Some(id) => *id,
            None => {
                let mut c = ColumnSet::default();
                c.opt("key", Some(text(list.key)))
                    .opt("name", Some(text(list.name)))
                    .opt("description", Some(Some(text(list.description))))
                    .opt("sort_order", Some(i as i32 * 10));
                let id = ins.insert::<LookupLists>(c).await?;
                state.lists.insert(list.key.into(), id);
                created.lookup_lists += 1;
                id
            }
        };
        for (j, (key, name, description)) in list.values.iter().enumerate() {
            if state.list_values.contains(&(list_id, (*key).to_owned())) {
                continue;
            }
            let mut c = ColumnSet::default();
            c.opt("list_id", Some(list_id))
                .opt("key", Some(text(key)))
                .opt("name", Some(text(name)))
                .opt("description", description.map(|d| Some(text(d))))
                .opt("sort_order", Some(j as i32 * 10));
            ins.insert::<LookupListValues>(c).await?;
            state.list_values.insert((list_id, (*key).to_owned()));
            created.lookup_list_values += 1;
        }
    }

    let area_id = match state.areas.get(content.area.key) {
        Some(id) => *id,
        None => {
            let mut c = ColumnSet::default();
            c.opt("key", Some(text(content.area.key)))
                .opt("name", Some(text(content.area.name)))
                .opt("description", Some(text(content.area.description)));
            let id = ins.insert::<Areas>(c).await?;
            state.areas.insert(content.area.key.into(), id);
            created.areas += 1;
            id
        }
    };

    for (i, cls) in content.classes.iter().enumerate() {
        if state.classes.contains_key(cls.key) {
            continue;
        }
        let parent_id = cls.parent.and_then(|p| state.classes.get(p).copied());
        let mut c = ColumnSet::default();
        c.opt("key", Some(text(cls.key)))
            .opt("area_id", Some(area_id))
            .opt("name", Some(text(cls.name)))
            .opt("description", Some(text(cls.description)))
            .opt("is_abstract", Some(cls.is_abstract))
            .opt("parent_id", parent_id.map(Some))
            .opt("color", cls.color.map(|v| Some(text(v))))
            .opt("sort_order", Some(i as i32 * 10));
        let id = ins.insert::<CiClasses>(c).await?;
        state.classes.insert(cls.key.into(), id);
        state.parents.insert(id, parent_id);
        created.classes += 1;
    }

    for cls in &content.classes {
        let class_id = state.classes[cls.key];
        for (i, a) in cls.attributes.iter().enumerate() {
            if state.has_attribute(cls.key, a.key) {
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
            let lookup_list_id = match a.lookup_list {
                Some(k) => match state.lists.get(k) {
                    Some(id) => Some(*id),
                    None => {
                        skipped.push(format!("attribute {}.{}: lookup list \"{k}\" does not exist", cls.key, a.key));
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
                .opt("lookup_list_id", lookup_list_id.map(Some))
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

    // Classes without a title attribute are labelled by the "name" field of their lineage.
    for cls in &content.classes {
        let class_id = state.classes[cls.key];
        let titled = sqlx::query(
            "UPDATE cmdb.ci_classes c SET title_attribute_id = d.id
             FROM cmdb.ci_class_lineage($1) l JOIN cmdb.ci_attribute_definitions d ON d.class_id = l.class_id
             WHERE c.id = $1 AND c.title_attribute_id IS NULL AND d.key = 'name' AND d.data_type = 'text'",
        )
        .bind(class_id)
        .execute(&mut *ins.conn)
        .await?;
        if titled.rows_affected() > 0 {
            state.titled.push(class_id);
        }
    }

    let Installer { conn, audit } = ins;
    crud::write_audit(conn, ctx, audit).await?;
    // The area's schema, a table per type and a column per field.
    let classes: Vec<Uuid> = content.classes.iter().filter_map(|c| state.classes.get(c.key).copied()).collect();
    let summary = format!("Install template {} into area {}", template.key, content.area.key);
    let change = engine::apply_with(conn, ctx, &summary, Scope::Classes(classes), Purge::default(), true).await?;
    if !state.titled.is_empty() {
        let model = Model::load(conn).await?;
        crate::data::items::refresh_labels(conn, &model, &state.titled, None).await?;
    }
    Ok(TemplateInstallResult { template: template.key.into(), created, existing, skipped, schema_change: change })
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
                "Adds the template's area (a PostgreSQL schema), every class (a table in it), attribute (a column), \
                 relationship type and rule, status, environment and location of the template whose key does not \
                 exist yet, in one transaction. Existing rows are left unchanged, so \
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

    /// GH#276: installing a template adds its required fields to an existing
    /// type of the same key. A caller who may not view that type gets the same
    /// schema change whether or not it holds CIs; a viewer learns the difference.
    #[tokio::test]
    async fn install_tells_a_hidden_type_nothing_about_its_cis() {
        use crate::auth::permissions::Permissions;
        use crate::db::scratch;
        use crate::modules::classes::{CiClass, CiClasses};
        use crate::modules::items::{schemas::CreateItemBody, service as items_service};
        use crate::modules::simple_resource as simple;

        let manager = || {
            let permissions = Permissions { global: [GlobalPermission::DatamodelManage].into(), ..Default::default() };
            let principal = crate::auth::Principal {
                user_id: Uuid::new_v4(),
                username: "modeller".into(),
                credential: crate::auth::Credential::Token,
                permissions,
            };
            RequestContext::user(std::sync::Arc::new(principal), "gh276".into())
        };
        // The notes on hardware's fields, as the caller reads them.
        let notes = |r: &TemplateInstallResult| {
            r.schema_change
                .as_ref()
                .expect("schema change")
                .impact
                .iter()
                .filter(|i| {
                    i.kind != "add_column"
                        && (i.message.contains(".hardware.name ") || i.message.contains(".hardware.status "))
                })
                .map(|i| (i.kind.clone(), i.rows, i.message.clone()))
                .collect::<Vec<_>>()
        };

        fn json<T: serde::de::DeserializeOwned>(v: Value) -> T {
            serde_json::from_value(v).unwrap()
        }
        let mut seen = Vec::new();
        for (with_cis, viewer) in [(false, false), (true, false), (false, true), (true, true)] {
            let Some(db) = scratch::database(&format!("gh276_{with_cis}_{viewer}")).await else { return };
            let pool = &db.pool;
            let admin = RequestContext::system("gh276-test", "gh276-test");
            let hardware: CiClass =
                simple::create::<CiClasses>(pool, &admin, &json(json!({"name": "Hardware", "key": "hardware"})))
                    .await
                    .unwrap();
            if with_cis {
                let item: CreateItemBody = json(json!({"classId": hardware.id, "attributes": {}}));
                items_service::create(pool, &admin, &item).await.unwrap();
            }
            let ctx = if viewer { admin } else { manager() };
            let result = install_by_key(pool, &ctx, "it_infrastructure").await.unwrap();
            let notes = notes(&result);
            assert_eq!(notes.len(), 2, "{with_cis} {viewer}: {:?}", result.schema_change);
            let hidden = sqlx::query_scalar::<_, bool>(
                "SELECT bool_and(NOT d.is_required OR c.is_nullable = 'YES')
                 FROM cmdb.ci_attribute_definitions d JOIN information_schema.columns c ON c.column_name = d.key
                 WHERE d.class_id = $1 AND d.key IN ('name', 'status') AND c.table_name = 'hardware'",
            )
            .bind(hardware.id)
            .fetch_one(pool)
            .await
            .unwrap();
            assert_eq!(hidden, !viewer || with_cis, "{with_cis} {viewer}: nullable");
            seen.push(notes);
            db.drop().await;
        }
        // Without view: identical, and nothing counted.
        assert_eq!(seen[0], seen[1]);
        assert!(seen[0].iter().all(|(kind, rows, _)| kind == "warning" && rows.is_none()), "{:?}", seen[0]);
        // With view: required when empty, a counted warning otherwise.
        assert!(seen[2].iter().all(|(kind, _, _)| kind == "not_null"), "{:?}", seen[2]);
        assert!(seen[3].iter().all(|(kind, rows, _)| kind == "warning" && *rows == Some(1)), "{:?}", seen[3]);
    }

    #[test]
    fn it_infrastructure_is_the_former_seed() {
        let t = find("it_infrastructure").expect("template");
        let c = contents(&(t.content)());
        assert_eq!(c.areas, 1);
        assert_eq!(c.classes, 8);
        assert_eq!(c.attribute_definitions, 69);
        assert_eq!(c.relationship_types, 4);
        assert_eq!(c.relationship_rules, 12);
        assert_eq!(c.lookup_lists, 4);
        assert_eq!(c.lookup_list_values, 17);
    }

    #[test]
    fn templates_are_self_consistent() {
        let key = regex::Regex::new(crate::api::schemas::KEY_PATTERN).unwrap();
        for t in TEMPLATES {
            assert!(key.is_match(t.key), "template key {}", t.key);
            let c = (t.content)();
            use crate::schema::naming::{NameKind, validate};
            assert_eq!(validate(c.area.key, NameKind::Area), Ok(()), "area {}", c.area.key);
            let mut classes = HashSet::new();
            for cls in &c.classes {
                assert_eq!(validate(cls.key, NameKind::Type), Ok(()), "type {}", cls.key);
                for a in &cls.attributes {
                    assert_eq!(validate(a.key, NameKind::Field), Ok(()), "field {}.{}", cls.key, a.key);
                }
                assert!(cls.parent.is_none_or(|p| classes.contains(p)), "{}: parent listed later", cls.key);
                assert!(classes.insert(cls.key), "duplicate class {}", cls.key);
                let mut attrs = HashSet::new();
                for a in &cls.attributes {
                    assert!(attrs.insert(a.key), "duplicate attribute {}.{}", cls.key, a.key);
                    assert_eq!(a.data_type == "enum", a.enum_values.is_some(), "{}.{}", cls.key, a.key);
                    assert_eq!(a.data_type == "reference", a.reference_class.is_some(), "{}.{}", cls.key, a.key);
                    assert_eq!(a.data_type == "lookup", a.lookup_list.is_some(), "{}.{}", cls.key, a.key);
                    assert!(
                        a.lookup_list.is_none_or(|l| c.lookup_lists.iter().any(|x| x.key == l)),
                        "{}.{}",
                        cls.key,
                        a.key
                    );
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
            for l in c.lookup_lists {
                assert!(key.is_match(l.key), "list {}", l.key);
                let mut values = HashSet::new();
                for v in l.values {
                    assert!(key.is_match(v.0) && values.insert(v.0), "value {}.{}", l.key, v.0);
                }
            }
        }
    }
}
