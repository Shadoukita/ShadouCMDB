//! CI classes (with the effective-attributes view), attribute definitions,
//! relationship types and relationship rules.

use axum::http::Method;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Number, Value};
use sqlx::PgConnection;
use sqlx::types::Json as SqlJson;
use utoipa::openapi::schema::{ArrayBuilder, ObjectBuilder, Schema, Type};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::areas;
use super::items::service as items_service;
use super::schema_changes::{PurgeRequest, PurgeResult, check_purge};
use super::simple_resource::{
    self as simple, ATTRIBUTE_SPANS, BoxFuture, CLASS_SPANS, EVERY_CLASS_SPANS, ListQuery, RULE_SPANS, Resource, Usage,
    Writable, bool_filter, non_empty,
};
use crate::api::context::RequestContext;
use crate::api::route::{Body, Check, IdPath, In, Json, NoBody, NoQuery, Query, Route, route};
use crate::api::schemas::{
    self, IdOrNone, QueryBool, Sort, UuidList, description_schema, key_schema, name_schema, nullable_uuid_schema,
    sort_order_schema, technical_name_schema, trimmed, ts,
};
use crate::api::validate;
use crate::auth::permissions::{ClassOp, GlobalPermission};
use crate::data::classes as data;
use crate::data::crud::{self, AuditAction, AuditEntry, ColumnSet, Val, Where};
use crate::data::items as items_data;
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::paged;
use crate::schema::model::Model;
use crate::schema::naming::{self, Ident, NameKind};
use crate::schema::{self as engine, Purge, SchemaChange, Scope, Summary};

fn custom(field: &str, message: impl Into<String>) -> FieldError {
    FieldError { location: FieldLocation::Body, field: field.into(), message: message.into(), code: "custom".into() }
}

// ===========================================================================
// CI classes
// ===========================================================================

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CiClass {
    pub id: Uuid,
    /// Technical name: the type's table in its area's schema. Immutable.
    pub key: String,
    pub name: String,
    /// The area (menu tab and PostgreSQL schema) the type belongs to. Immutable.
    pub area_id: Uuid,
    /// The type's table, e.g. "bestand.netzwerk"
    pub table_name: String,
    /// Read-only reporting view: registry columns plus every field, e.g. "bestand.v_netzwerk"
    pub view_name: String,
    #[schema(required = true)]
    pub description: Option<String>,
    /// Parent class; attributes and relationship rules are inherited from it
    #[schema(required = true)]
    pub parent_id: Option<Uuid>,
    /// Abstract classes group attributes and rules but cannot hold CIs
    pub is_abstract: bool,
    #[schema(required = true)]
    pub icon: Option<String>,
    /// Hex colour for badges and charts, e.g. "#1f6feb"
    #[schema(required = true)]
    pub color: Option<String>,
    /// Position in menus and pickers (ascending)
    pub sort_order: i32,
    /// Archived classes keep their CIs but accept no new ones
    pub is_active: bool,
    /// The attribute (of this class or an ancestor) whose value labels its CIs in lists, references, the graph
    /// and search; null labels them by their ident
    #[schema(required = true)]
    pub title_attribute_id: Option<Uuid>,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
}

fn title_attribute_schema() -> Schema {
    let mut s = nullable_uuid_schema();
    if let Schema::AnyOf(a) = &mut s {
        a.description = Some(
            "Attribute of this class or an ancestor whose value labels the CIs (text, enum, number, integer, date, \
             datetime, ip or cidr); null labels them by their ident. A new class takes its parent's."
                .into(),
        );
    }
    s
}

pub(crate) fn icon_schema() -> Schema {
    schemas::nullable_string_schema(100)
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CiClassCreate {
    /// Leave out to derive it from the name ("Virtuelle Maschinen" -> "virtuelle_maschinen")
    #[schema(schema_with = technical_name_schema)]
    #[serde(default)]
    key: Option<String>,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    name: String,
    /// The area the type's table is created in. Leave out to use the parent's area, or, for a root type, the
    /// default area "infrastruktur" (created if missing).
    #[schema(nullable = false)]
    #[serde(default)]
    area_id: Option<Uuid>,
    #[schema(schema_with = description_schema)]
    #[serde(default)]
    description: Option<String>,
    #[schema(schema_with = nullable_uuid_schema)]
    #[serde(default)]
    parent_id: Option<Uuid>,
    #[schema(nullable = false)]
    is_abstract: Option<bool>,
    #[schema(schema_with = icon_schema)]
    #[serde(default)]
    icon: Option<String>,
    #[schema(schema_with = schemas::nullable_color_schema)]
    #[serde(default)]
    color: Option<String>,
    #[schema(schema_with = sort_order_schema)]
    sort_order: Option<i32>,
    #[schema(nullable = false)]
    is_active: Option<bool>,
    #[schema(schema_with = title_attribute_schema)]
    #[serde(default)]
    title_attribute_id: Option<Uuid>,
}

impl CiClassCreate {
    pub fn technical_name(&self) -> String {
        self.key.clone().unwrap_or_else(|| naming::derive(&self.name, NameKind::Type))
    }
}

// `key` and `areaId` are immutable: the table name is what imports, integrations and reports refer to.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CiClassUpdate {
    #[schema(schema_with = name_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    name: Option<String>,
    #[schema(schema_with = description_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    description: Option<Option<String>>,
    #[schema(schema_with = nullable_uuid_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    parent_id: Option<Option<Uuid>>,
    #[schema(nullable = false)]
    is_abstract: Option<bool>,
    #[schema(schema_with = icon_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    icon: Option<Option<String>>,
    #[schema(schema_with = schemas::nullable_color_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    color: Option<Option<String>>,
    #[schema(schema_with = sort_order_schema)]
    sort_order: Option<i32>,
    #[schema(nullable = false)]
    is_active: Option<bool>,
    #[schema(schema_with = title_attribute_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    title_attribute_id: Option<Option<Uuid>>,
}

impl Writable for CiClassCreate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("key", Some(self.technical_name()))
            .opt("name", Some(self.name.clone()))
            .opt("area_id", self.area_id)
            .opt("description", self.description.clone().map(Some))
            .opt("parent_id", self.parent_id.map(Some))
            .opt("is_abstract", self.is_abstract)
            .opt("icon", self.icon.clone().map(Some))
            .opt("color", self.color.clone().map(Some))
            .opt("sort_order", self.sort_order)
            .opt("is_active", self.is_active)
            .opt("title_attribute_id", self.title_attribute_id.map(Some));
        c
    }
}
impl Check for CiClassCreate {}

impl Writable for CiClassUpdate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("name", self.name.clone())
            .opt("description", self.description.clone())
            .opt("parent_id", self.parent_id)
            .opt("is_abstract", self.is_abstract)
            .opt("icon", self.icon.clone())
            .opt("color", self.color.clone())
            .opt("sort_order", self.sort_order)
            .opt("is_active", self.is_active)
            .opt("title_attribute_id", self.title_attribute_id);
        c
    }
}
impl Check for CiClassUpdate {
    fn check(&self) -> Vec<FieldError> {
        non_empty(&self.columns())
    }
}

fn class_parent_schema() -> Schema {
    schemas::id_or_none_schema("Direct children of this class; \"none\" for root classes")
}

fn class_sort() -> Schema {
    schemas::sort_schema(CiClassList::SORT_FIELDS, "name")
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct CiClassList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    offset: i64,
    #[param(schema_with = schemas::search_schema)]
    q: Option<String>,
    #[param(required = false, schema_with = class_sort)]
    sort: Sort,
    #[param(inline)]
    is_active: Option<QueryBool>,
    #[param(inline)]
    is_abstract: Option<QueryBool>,
    #[param(schema_with = class_parent_schema)]
    parent_id: Option<IdOrNone>,
    /// This class and every class below it
    descendant_of: Option<Uuid>,
    #[param(schema_with = schemas::uuid_list_schema)]
    area_id: Option<UuidList>,
}
paged!(CiClassList);

impl ListQuery for CiClassList {
    const SORT_FIELDS: &'static [&'static str] = &["name", "key", "sortOrder", "createdAt", "updatedAt"];
    fn q(&self) -> Option<&str> {
        self.q.as_deref()
    }
    fn sort(&self) -> &Sort {
        &self.sort
    }
    fn filter(&self, w: &mut Where<'_>) {
        bool_filter(w, "is_active", self.is_active);
        bool_filter(w, "is_abstract", self.is_abstract);
        match self.parent_id {
            Some(IdOrNone::None) => w.and_sql("parent_id IS NULL"),
            Some(IdOrNone::Id(id)) => {
                w.and().push("parent_id = ").push_bind(id);
            }
            None => {}
        }
        if let Some(id) = self.descendant_of {
            w.and().push("ci_class_is_a(id, ").push_bind(id).push(")");
        }
        if let Some(ids) = &self.area_id {
            w.and().push("area_id = ANY(").push_bind(ids.0.clone()).push(")");
        }
    }
}

pub struct CiClasses;

impl Resource for CiClasses {
    type Dto = CiClass;
    type Create = CiClassCreate;
    type Update = CiClassUpdate;
    type List = CiClassList;
    const TABLE: &'static str = "ci_classes";
    const LABEL: &'static str = "CI class";
    const BASE_PATH: &'static str = "/api/v1/ci-classes";
    const TAG: &'static str = "CI classes";
    const SINGULAR: &'static str = "ciClass";
    const PLURAL: &'static str = "ciClasses";
    const COLUMNS: &'static str = "id, key, name, area_id,
        (SELECT a.key FROM cmdb.areas a WHERE a.id = ci_classes.area_id) || '.' || key AS table_name,
        (SELECT a.key FROM cmdb.areas a WHERE a.id = ci_classes.area_id) || '.v_' || key AS view_name,
        description, parent_id, is_abstract, icon, color, sort_order, is_active, title_attribute_id, created_at, updated_at";
    const SEARCH_COLUMNS: &'static [&'static str] = &["key", "name", "description"];
    const ARCHIVE_ON_DELETE: bool = true;
    const WRITE_ERRORS: &'static [ErrorCode] = &[ErrorCode::InvalidName, ErrorCode::SchemaChangeRefused];
    const UPDATE_DESCRIPTION: &'static str = "Changing `titleAttributeId` relabels the class's CIs. Moving the type to another parent (`parentId`) keeps its title attribute only if the new lineage provides it; otherwise it takes the new parent's (so do its subtypes), and the CIs are relabelled.";
    const DELETE_DESCRIPTION: &'static str = "Archives the type (`isActive=false`): its table, CIs and values stay and stay readable, no new CIs can be created, and the UI hides it. `PATCH {\"isActive\": true}` restores it. To drop the table and delete its CIs, purge the type (`POST /api/v1/ci-classes/{id}/purge`).";
    const USAGE: &'static [Usage] = &[
        Usage {
            kind: "configurationItems",
            label: "configuration items",
            sql: "SELECT count(*) FROM configuration_items WHERE class_id = $1 AND deleted_at IS NULL",
            spans: Some(CLASS_SPANS),
            blocking: true,
        },
        Usage {
            kind: "deletedConfigurationItems",
            label: "deleted configuration items (kept for history)",
            sql: "SELECT count(*) FROM configuration_items WHERE class_id = $1 AND deleted_at IS NOT NULL",
            spans: Some(CLASS_SPANS),
            blocking: true,
        },
        Usage {
            kind: "subclasses",
            label: "subclasses",
            sql: "SELECT count(*) FROM ci_classes WHERE parent_id = $1",
            spans: None,
            blocking: true,
        },
        Usage {
            kind: "attributeDefinitions",
            label: "attribute definitions",
            sql: "SELECT count(*) FROM ci_attribute_definitions WHERE class_id = $1",
            spans: None,
            blocking: true,
        },
        Usage {
            kind: "referencingAttributes",
            label: "reference attributes on other classes pointing at it",
            sql: "SELECT count(*) FROM ci_attribute_definitions WHERE reference_class_id = $1 AND class_id <> $1",
            spans: None,
            blocking: true,
        },
        Usage {
            kind: "relationshipRules",
            label: "relationship rules",
            sql: "SELECT count(*) FROM relationship_type_rules WHERE source_class_id = $1 OR target_class_id = $1",
            spans: None,
            blocking: true,
        },
        Usage {
            kind: "permissionGrants",
            label: "permission profile grants (removed with the class)",
            sql: "SELECT count(*) FROM permission_profile_class_permissions WHERE class_id = $1",
            spans: None,
            blocking: false,
        },
    ];

    fn id(row: &CiClass) -> Uuid {
        row.id
    }

    fn validate(columns: &ColumnSet, create: bool) -> Result<(), AppError> {
        match engine::key_column(columns) {
            Some(key) if create => engine::validate_name(key, NameKind::Type, "key"),
            _ => Ok(()),
        }
    }

    fn before_write(conn: &mut PgConnection) -> BoxFuture<'_, Result<(), AppError>> {
        Box::pin(async move { Ok(engine::lock(conn).await?) })
    }

    fn prepare_create<'a>(
        conn: &'a mut PgConnection,
        ctx: &'a RequestContext,
        columns: &'a mut ColumnSet,
    ) -> BoxFuture<'a, Result<(), AppError>> {
        Box::pin(async move {
            let parent = columns.0.iter().find_map(|(c, v)| match (c, v) {
                (&"parent_id", Val::Uuid(Some(id))) => Some(*id),
                _ => None,
            });
            // A subtype is labelled like its parent unless told otherwise.
            if let Some(parent) = parent
                && !columns.0.iter().any(|(c, _)| *c == "title_attribute_id")
            {
                let title: Option<Uuid> =
                    sqlx::query_scalar("SELECT title_attribute_id FROM cmdb.ci_classes WHERE id = $1")
                        .bind(parent)
                        .fetch_optional(&mut *conn)
                        .await?
                        .flatten();
                columns.opt("title_attribute_id", title.map(Some));
            }
            if columns.0.iter().any(|(c, _)| *c == "area_id") {
                return Ok(());
            }
            let area_id = match parent {
                // An unknown parent is reported by the insert's foreign key.
                Some(parent) => {
                    sqlx::query_scalar("SELECT area_id FROM cmdb.ci_classes WHERE id = $1")
                        .bind(parent)
                        .fetch_optional(&mut *conn)
                        .await?
                }
                None => None,
            };
            let area_id = match area_id {
                Some(id) => id,
                None => areas::default_area(conn, ctx).await?,
            };
            columns.opt("area_id", Some(area_id));
            Ok(())
        })
    }

    fn after_write<'a>(
        conn: &'a mut PgConnection,
        ctx: &'a RequestContext,
        row: &'a CiClass,
        previous: Option<&'a CiClass>,
    ) -> BoxFuture<'a, Result<(), AppError>> {
        Box::pin(async move {
            if let Some(previous) = previous {
                if row.is_abstract && !previous.is_abstract {
                    // Success or refusal tells whether the type holds CIs (GH#267).
                    if !engine::may_view_all(conn, ctx, &[row.id]).await? {
                        return Err(engine::view_required(
                            "isAbstract",
                            "Making a type abstract is checked against the CIs it holds; that needs the view right on \
                             the type. Nothing was changed."
                                .into(),
                        ));
                    }
                }
                if row.is_abstract && !previous.is_abstract && data::class_has_items(conn, row.id).await? {
                    return Err(AppError::field(
                        "isAbstract",
                        "Class still holds CIs; an abstract class cannot",
                        "class_has_items",
                    ));
                }
                if row.parent_id != previous.parent_id {
                    move_to_new_parent(conn, ctx, row, previous).await?;
                    check_parent_fields_in_lineage(conn, row.id).await?;
                    repair_titles(conn, row, previous).await?;
                }
                if row.parent_id != previous.parent_id || row.title_attribute_id != previous.title_attribute_id {
                    let model = Model::load(conn).await?;
                    items_data::refresh_labels(conn, &model, &model.subtree(row.id), None).await?;
                }
            }
            let verb = match previous {
                None => "Create",
                Some(p) if p.is_active && !row.is_active => "Archive",
                Some(p) if !p.is_active && row.is_active => "Restore",
                Some(_) => "Update",
            };
            let summary = format!("{verb} type {}", row.table_name);
            engine::apply(conn, ctx, &summary, Scope::Classes(vec![row.id]), Purge::default()).await?;
            Ok(())
        })
    }
}

/// A type got a new parent: its CIs (and those of its subtypes) need rows in
/// the tables of the new ancestors and lose them in the tables of ancestors
/// they no longer have, which is refused while those rows hold values.
async fn move_to_new_parent(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    row: &CiClass,
    previous: &CiClass,
) -> Result<(), AppError> {
    let model = Model::load(conn).await?;
    let new_lineage: Vec<Uuid> = model.lineage(row.id).iter().map(|c| c.id).collect();
    let mut old_lineage: Vec<Uuid> =
        previous.parent_id.map(|p| model.lineage(p).iter().map(|c| c.id).collect()).unwrap_or_default();
    old_lineage.push(row.id);
    let subtree = model.subtree(row.id);
    // Success or refusal tells whether the moved CIs hold values or lack
    // required ones, so the caller must see them all before any is read (GH#267).
    if !engine::may_view_all(conn, ctx, &subtree).await? {
        return Err(engine::view_required(
            "parentId",
            "Moving a type is checked against the CIs of the type and every type below it; that needs the view right \
             on all of them. Nothing was changed."
                .into(),
        ));
    }
    let items = items_data::ids_of_classes(conn, &subtree).await?;
    if items.is_empty() {
        return Ok(());
    }
    for gone in old_lineage.iter().filter(|c| !new_lineage.contains(c)) {
        let Some(table) = model.table(*gone) else { continue };
        let fields: Vec<&str> = model.own_fields(*gone).map(|f| f.key.as_str()).collect();
        let used = items_data::fields_with_values(conn, &table, &fields, &items).await?;
        if !used.is_empty() {
            return Err(engine::refused(
                "parentId",
                "attributes_outside_lineage",
                format!(
                    "CIs of this type hold values for fields that the new parent does not provide: {}. Clear them \
                     first.",
                    used.join(", ")
                ),
            ));
        }
        items_data::delete_type_rows(conn, &table, &items).await?;
    }
    for added in new_lineage.iter().filter(|c| !old_lineage.contains(c)) {
        let Some(table) = model.table(*added) else { continue };
        if let Err(err) = items_data::insert_type_rows(conn, &table, &items).await {
            let required: Vec<&str> =
                model.own_fields(*added).filter(|f| f.not_null()).map(|f| f.key.as_str()).collect();
            return Err(match err {
                sqlx::Error::Database(e) if e.code().as_deref() == Some("23502") => engine::refused(
                    "parentId",
                    "values_missing",
                    format!("The new parent has required fields these CIs have no value for: {}", required.join(", ")),
                ),
                other => other.into(),
            });
        }
    }
    Ok(())
}

/// Fields of the moved type and its subtypes whose parent field (dependent
/// dropdowns) is no longer on the type or an ancestor.
async fn check_parent_fields_in_lineage(conn: &mut PgConnection, class_id: Uuid) -> Result<(), AppError> {
    let broken: Vec<String> = sqlx::query_scalar(
        "SELECT c.key || '.' || d.key
         FROM ci_attribute_definitions d
         JOIN ci_attribute_definitions p ON p.id = d.parent_attribute_id
         JOIN ci_classes c ON c.id = d.class_id
         WHERE ci_class_is_a(d.class_id, $1) AND NOT ci_class_is_a(d.class_id, p.class_id)
         ORDER BY 1",
    )
    .bind(class_id)
    .fetch_all(&mut *conn)
    .await?;
    if broken.is_empty() {
        return Ok(());
    }
    Err(engine::refused(
        "parentId",
        "parent_field_outside_lineage",
        format!(
            "Fields {} depend on a parent field the new parent type does not provide; unlink them (parentAttributeId: \
             null) first.",
            broken.join(", ")
        ),
    ))
}

/// After a move: a type whose title attribute is no longer in its lineage (the
/// trigger cleared the moved type's; its subtypes still point at the old one)
/// takes its parent's, top down.
async fn repair_titles(conn: &mut PgConnection, row: &CiClass, previous: &CiClass) -> Result<(), AppError> {
    let model = Model::load(conn).await?;
    let mut titles: std::collections::HashMap<Uuid, Option<Uuid>> =
        model.classes.iter().map(|c| (c.id, c.title_attribute_id)).collect();
    for class_id in model.subtree(row.id) {
        let lineage: Vec<Uuid> = model.lineage(class_id).iter().map(|c| c.id).collect();
        let current = titles.get(&class_id).copied().flatten();
        let fits = current.and_then(|t| model.field(t)).is_some_and(|f| lineage.contains(&f.class_id));
        let lost = if class_id == row.id {
            previous.title_attribute_id.is_some() && current.is_none()
        } else {
            current.is_some()
        };
        if fits || !lost {
            continue;
        }
        let parent_title =
            model.class(class_id).and_then(|c| c.parent_id).and_then(|p| titles.get(&p).copied().flatten());
        sqlx::query("UPDATE cmdb.ci_classes SET title_attribute_id = $2 WHERE id = $1")
            .bind(class_id)
            .bind(parent_title)
            .execute(&mut *conn)
            .await?;
        titles.insert(class_id, parent_title);
    }
    Ok(())
}

/// Deletes an archived type: its CIs (with their relationships), fields,
/// relationship rules and table. Irreversible; the audit log keeps the history.
pub async fn purge_class_in(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    id: Uuid,
    confirm: &str,
) -> Result<Option<SchemaChange>, AppError> {
    engine::lock(conn).await?;
    let row: CiClass = crud::select_by_id(conn, CiClasses::TABLE, CiClasses::COLUMNS, id, true)
        .await?
        .ok_or_else(|| AppError::missing(CiClasses::LABEL, id))?;
    check_purge("type", &row.key, row.is_active, confirm)?;
    let model = Model::load(conn).await?;
    let table = model.table(id).ok_or_else(AppError::internal)?;
    let blockers: Vec<(i64, &str)> = vec![
        (
            sqlx::query_scalar("SELECT count(*) FROM cmdb.ci_classes WHERE parent_id = $1")
                .bind(id)
                .fetch_one(&mut *conn)
                .await?,
            "subtypes (purge them first)",
        ),
        (
            sqlx::query_scalar(
                "SELECT count(*) FROM cmdb.ci_attribute_definitions WHERE reference_class_id = $1 AND class_id <> $1",
            )
            .bind(id)
            .fetch_one(&mut *conn)
            .await?,
            "reference fields of other types pointing at it (purge or change them first)",
        ),
    ];
    let blocking: Vec<String> =
        blockers.iter().filter(|(n, _)| *n > 0).map(|(n, what)| format!("{n} {what}")).collect();
    if !blocking.is_empty() {
        return Err(AppError::new(
            ErrorCode::InUse,
            format!("Type \"{}\" cannot be purged: {}", row.key, blocking.join(", ")),
        ));
    }
    let items = items_data::ids_of_classes(conn, &[id]).await?;
    // Every destroyed CI and relationship gets its own delete entry with its
    // last state, so the audit log shows what the purge took with it.
    for batch in items.chunks(crud::AUDIT_BATCH) {
        let before = items_service::details(conn, &model, batch).await?;
        let entries = before
            .iter()
            .map(|ci| AuditEntry {
                action: AuditAction::Delete,
                entity_type: "configuration_items",
                entity_id: ci.summary.id,
                old_value: Some(crud::json(ci)),
                new_value: None,
            })
            .collect();
        crud::write_audit(conn, ctx, entries).await?;
    }
    // The relationships go with the CIs. How many is told only to a caller who
    // may view the CIs at both ends (GH#268), so the other ends' types join
    // the purge's classes.
    let mut classes = model.subtree(id);
    let ends: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT c.class_id FROM cmdb.ci_relationships e
         JOIN cmdb.configuration_items c ON c.id IN (e.source_ci_id, e.target_ci_id)
         WHERE e.source_ci_id = ANY($1) OR e.target_ci_id = ANY($1)",
    )
    .bind(&items)
    .fetch_all(&mut *conn)
    .await?;
    classes.extend(ends.into_iter().filter(|c| !classes.contains(c)).collect::<Vec<_>>());
    let mut edge_count = 0;
    loop {
        let edges = items_data::delete_edges_of(conn, &items, crud::AUDIT_BATCH as i64).await?;
        edge_count += edges.len();
        let done = edges.len() < crud::AUDIT_BATCH;
        let entries = edges
            .iter()
            .map(|e| AuditEntry {
                action: AuditAction::Delete,
                entity_type: "ci_relationships",
                entity_id: e.id,
                old_value: Some(crud::json(e)),
                new_value: None,
            })
            .collect();
        crud::write_audit(conn, ctx, entries).await?;
        if done {
            break;
        }
    }
    // Type rows first, so that references between these CIs are gone before
    // their registry rows are deleted (the foreign keys check at statement end).
    for c in model.lineage(id) {
        if let Some(t) = model.table(c.id) {
            items_data::delete_type_rows(conn, &t, &items).await?;
        }
    }
    if let Err(err) =
        sqlx::query("DELETE FROM cmdb.configuration_items WHERE id = ANY($1)").bind(&items).execute(&mut *conn).await
    {
        return Err(match err {
            // The PostgreSQL message names the referencing type's table and
            // constraint, which the caller may not view (GH#268): logged only.
            sqlx::Error::Database(e) if e.code().as_deref() == Some("23503") => {
                tracing::info!(class = %row.key, error = e.message(), "type purge refused: CIs still referenced");
                AppError::new(
                    ErrorCode::InUse,
                    format!(
                        "CIs of other types still reference CIs of \"{}\" in reference fields; clear those values first",
                        row.key
                    ),
                )
            }
            other => other.into(),
        });
    }
    sqlx::query("DELETE FROM cmdb.relationship_type_rules WHERE source_class_id = $1 OR target_class_id = $1")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    sqlx::query("DELETE FROM cmdb.ci_attribute_definitions WHERE class_id = $1").bind(id).execute(&mut *conn).await?;
    crud::delete_row(conn, CiClasses::TABLE, id).await?;
    let purge = Purge { tables: vec![table], classes, ..Purge::default() };
    let without = format!("Purge type {} (its CIs and their relationships deleted)", row.table_name);
    let summary = if purge.reveals(ctx.class_scope(ClassOp::View).as_deref()) {
        let counted =
            format!("Purge type {} ({} CIs, {} relationships deleted)", row.table_name, items.len(), edge_count);
        Summary::counted(counted, without)
    } else {
        Summary::from(&without)
    };
    let parent_scope = row.parent_id.map(|p| vec![p]).unwrap_or_default();
    let change = engine::apply(conn, ctx, summary, Scope::Classes(parent_scope), purge).await?;
    let entry = AuditEntry {
        action: AuditAction::Delete,
        entity_type: CiClasses::TABLE,
        entity_id: id,
        old_value: Some(crud::json(&row)),
        new_value: None,
    };
    crud::write_audit(conn, ctx, vec![entry]).await?;
    Ok(change)
}

// ===========================================================================
// Attribute definitions
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum AttributeDataType {
    Text,
    Number,
    Integer,
    Boolean,
    Enum,
    Date,
    Datetime,
    Ip,
    Cidr,
    Reference,
    /// A value from an admin-defined lookup list (stored by value id)
    Lookup,
}

/// Data types a title attribute can have (as `cmdb.title_data_type()`).
pub const TITLE_DATA_TYPES: &[AttributeDataType] = &[
    AttributeDataType::Text,
    AttributeDataType::Enum,
    AttributeDataType::Number,
    AttributeDataType::Integer,
    AttributeDataType::Date,
    AttributeDataType::Datetime,
    AttributeDataType::Ip,
    AttributeDataType::Cidr,
];

impl AttributeDataType {
    pub fn as_str(self) -> &'static str {
        match self {
            AttributeDataType::Text => "text",
            AttributeDataType::Number => "number",
            AttributeDataType::Integer => "integer",
            AttributeDataType::Boolean => "boolean",
            AttributeDataType::Enum => "enum",
            AttributeDataType::Date => "date",
            AttributeDataType::Datetime => "datetime",
            AttributeDataType::Ip => "ip",
            AttributeDataType::Cidr => "cidr",
            AttributeDataType::Reference => "reference",
            AttributeDataType::Lookup => "lookup",
        }
    }
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttributeDefinition {
    pub id: Uuid,
    pub class_id: Uuid,
    pub key: String,
    pub label: String,
    #[schema(required = true)]
    pub description: Option<String>,
    #[schema(inline)]
    pub data_type: AttributeDataType,
    pub is_required: bool,
    /// Allowed values when dataType is "enum"
    #[schema(value_type = Option<Vec<String>>, required = true)]
    pub enum_values: Option<SqlJson<Vec<String>>>,
    /// When dataType is "reference": the class (or ancestor) the referenced CI must belong to
    #[schema(required = true)]
    pub reference_class_id: Option<Uuid>,
    /// When dataType is "lookup": the admin-defined list its values come from
    #[schema(required = true)]
    pub lookup_list_id: Option<Uuid>,
    /// When the lookup list has a parent list: the field (on this class or an ancestor) bound to the parent
    /// list. A CI's value must then belong to the CI's value of that field.
    #[schema(required = true)]
    pub parent_attribute_id: Option<Uuid>,
    #[schema(value_type = Option<std::collections::HashMap<String, serde_json::Value>>, required = true)]
    pub validation: Option<SqlJson<Map<String, Value>>>,
    /// Form section the field is shown in, e.g. "Hardware"
    #[schema(required = true)]
    pub group_name: Option<String>,
    /// Shown under the field on CI forms
    #[schema(required = true)]
    pub help_text: Option<String>,
    /// Pre-filled on new CIs and stored when a CI is created without a value (same shape as the value)
    #[schema(value_type = Option<serde_json::Value>, required = true)]
    pub default_value: Option<SqlJson<Value>>,
    /// Order within the form section
    pub sort_order: i32,
    /// Retired attributes keep their stored values but accept no new ones
    pub is_active: bool,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
}

// Where an inherited attribute is defined.
#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DefinedOn {
    pub id: Uuid,
    pub key: String,
    pub name: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EffectiveAttribute {
    pub id: Uuid,
    pub class_id: Uuid,
    pub key: String,
    pub label: String,
    #[schema(required = true)]
    pub description: Option<String>,
    #[schema(inline)]
    pub data_type: AttributeDataType,
    pub is_required: bool,
    /// Allowed values when dataType is "enum"
    #[schema(value_type = Option<Vec<String>>, required = true)]
    pub enum_values: Option<SqlJson<Vec<String>>>,
    /// When dataType is "reference": the class (or ancestor) the referenced CI must belong to
    #[schema(required = true)]
    pub reference_class_id: Option<Uuid>,
    /// When dataType is "lookup": the admin-defined list its values come from
    #[schema(required = true)]
    pub lookup_list_id: Option<Uuid>,
    /// When the lookup list has a parent list: the field (on this class or an ancestor) bound to the parent
    /// list. A CI's value must then belong to the CI's value of that field.
    #[schema(required = true)]
    pub parent_attribute_id: Option<Uuid>,
    #[schema(value_type = Option<std::collections::HashMap<String, serde_json::Value>>, required = true)]
    pub validation: Option<SqlJson<Map<String, Value>>>,
    /// Form section the field is shown in, e.g. "Hardware"
    #[schema(required = true)]
    pub group_name: Option<String>,
    /// Shown under the field on CI forms
    #[schema(required = true)]
    pub help_text: Option<String>,
    /// Pre-filled on new CIs and stored when a CI is created without a value (same shape as the value)
    #[schema(value_type = Option<serde_json::Value>, required = true)]
    pub default_value: Option<SqlJson<Value>>,
    /// Order within the form section
    pub sort_order: i32,
    /// Retired attributes keep their stored values but accept no new ones
    pub is_active: bool,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
    /// Defined on an ancestor class rather than this one
    pub inherited: bool,
    #[schema(inline)]
    pub defined_on: DefinedOn,
}

/// All attributes a CI of this class can carry (not paginated; bounded by the class lineage)
#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EffectiveAttributeList {
    pub data: Vec<EffectiveAttribute>,
}

impl From<data::EffectiveAttributeRow> for EffectiveAttribute {
    fn from(r: data::EffectiveAttributeRow) -> Self {
        EffectiveAttribute {
            id: r.id,
            class_id: r.class_id,
            key: r.key,
            label: r.label,
            description: r.description,
            data_type: r.data_type,
            is_required: r.is_required,
            enum_values: r.enum_values,
            reference_class_id: r.reference_class_id,
            lookup_list_id: r.lookup_list_id,
            parent_attribute_id: r.parent_attribute_id,
            validation: r.validation,
            group_name: r.group_name,
            help_text: r.help_text,
            default_value: r.default_value,
            sort_order: r.sort_order,
            is_active: r.is_active,
            created_at: r.created_at,
            updated_at: r.updated_at,
            inherited: r.depth > 0,
            defined_on: DefinedOn { id: r.class_id, key: r.defined_on_key, name: r.defined_on_name },
        }
    }
}

/// Extra validation the API enforces on attribute values
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ValidationRules {
    /// number/integer: minimum
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(schema_with = min_schema)]
    pub min: Option<Number>,
    /// number/integer: maximum
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(schema_with = max_schema)]
    pub max: Option<Number>,
    /// text: maximum length
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, minimum = 1)]
    pub max_length: Option<i64>,
    /// text: regular expression the value must match
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, max_length = 500)]
    pub pattern: Option<String>,
    /// text: the value may hold line breaks; forms edit it in a multi-line text area.
    /// Omitted when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub multiline: bool,
    /// Display unit, e.g. "GB"
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, max_length = 20)]
    pub unit: Option<String>,
}

impl ValidationRules {
    pub(crate) fn check(&self, data_type: AttributeDataType, errors: &mut Vec<FieldError>) {
        let numeric = matches!(data_type, AttributeDataType::Number | AttributeDataType::Integer);
        if (self.min.is_some() || self.max.is_some()) && !numeric {
            errors.push(custom("validation", "min/max apply to number and integer attributes only"));
        }
        if (self.pattern.is_some() || self.max_length.is_some()) && data_type != AttributeDataType::Text {
            errors.push(custom("validation", "pattern/maxLength apply to text attributes only"));
        }
        if self.multiline && data_type != AttributeDataType::Text {
            errors.push(custom("validation", "multiline applies to text attributes only"));
        }
        if let (Some(min), Some(max)) = (&self.min, &self.max)
            && min.as_f64() > max.as_f64()
        {
            errors.push(custom("validation.min", "min must not exceed max"));
        }
        if let Some(p) = &self.pattern
            && validate::cached_regex(p).is_none()
        {
            errors.push(custom("validation.pattern", "Not a valid regular expression"));
        }
    }
}

fn number_schema(description: &str) -> Schema {
    ObjectBuilder::new().schema_type(Type::Number).description(Some(description)).into()
}
fn min_schema() -> Schema {
    number_schema("number/integer: minimum")
}
fn max_schema() -> Schema {
    number_schema("number/integer: maximum")
}

pub(crate) fn enum_values_schema() -> Schema {
    let item = ObjectBuilder::new()
        .schema_type(Type::String)
        .min_length(Some(1))
        .max_length(Some(200))
        .pattern(Some(schemas::NOT_BLANK_PATTERN));
    let array = ArrayBuilder::new().items(item).min_items(Some(1)).max_items(Some(500)).unique_items(true);
    utoipa::openapi::schema::AnyOfBuilder::new().item(array).item(ObjectBuilder::new().schema_type(Type::Null)).into()
}

pub(crate) fn validation_schema() -> Schema {
    utoipa::openapi::schema::AnyOfBuilder::new()
        .item(ValidationRules::schema_inline())
        .item(ObjectBuilder::new().schema_type(Type::Null))
        .into()
}

impl ValidationRules {
    fn schema_inline() -> utoipa::openapi::RefOr<Schema> {
        <ValidationRules as utoipa::PartialSchema>::schema()
    }
}

pub(crate) fn group_name_schema() -> Schema {
    utoipa::openapi::schema::AnyOfBuilder::new()
        .item(ObjectBuilder::new().schema_type(Type::String).max_length(Some(100)))
        .item(ObjectBuilder::new().schema_type(Type::Null))
        .into()
}

pub(crate) fn help_text_schema() -> Schema {
    schemas::nullable_string_schema(2000)
}

pub(crate) fn default_value_schema() -> Schema {
    utoipa::openapi::schema::AnyOfBuilder::new()
        .item(ObjectBuilder::new().schema_type(Type::String).max_length(Some(10_000)))
        .item(ObjectBuilder::new().schema_type(Type::Number))
        .item(ObjectBuilder::new().schema_type(Type::Boolean))
        .item(ObjectBuilder::new().schema_type(Type::Null))
        .description(Some(
            "Value in the same shape the CI API takes for this attribute (a lookup value id for \"lookup\"). \
             Not allowed for \"reference\" attributes.",
        ))
        .into()
}

fn trimmed_list<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<Vec<String>>>, D::Error> {
    Ok(Some(Option::<Vec<String>>::deserialize(d)?.map(|v| v.into_iter().map(|s| s.trim().to_owned()).collect())))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttributeDefinitionCreate {
    class_id: Uuid,
    /// Leave out to derive it from the label ("Größe" -> "groesse"); the column name in the type's table
    #[schema(schema_with = technical_name_schema)]
    #[serde(default)]
    key: Option<String>,
    #[schema(inline)]
    data_type: AttributeDataType,
    #[schema(schema_with = nullable_uuid_schema)]
    #[serde(default)]
    reference_class_id: Option<Uuid>,
    #[schema(schema_with = nullable_uuid_schema)]
    #[serde(default)]
    lookup_list_id: Option<Uuid>,
    /// Lookup fields on a list with a parent list: the field bound to the parent list (this class or an ancestor)
    #[schema(schema_with = nullable_uuid_schema)]
    #[serde(default)]
    parent_attribute_id: Option<Uuid>,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    label: String,
    #[schema(schema_with = description_schema)]
    #[serde(default)]
    description: Option<String>,
    #[schema(nullable = false)]
    is_required: Option<bool>,
    #[schema(schema_with = enum_values_schema)]
    #[serde(default, deserialize_with = "trimmed_list")]
    enum_values: Option<Option<Vec<String>>>,
    #[schema(schema_with = validation_schema)]
    #[serde(default)]
    validation: Option<ValidationRules>,
    #[schema(schema_with = group_name_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    group_name: Option<String>,
    #[schema(schema_with = help_text_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    help_text: Option<String>,
    #[schema(schema_with = default_value_schema)]
    #[serde(default)]
    default_value: Option<Value>,
    #[schema(schema_with = sort_order_schema)]
    sort_order: Option<i32>,
    #[schema(nullable = false)]
    is_active: Option<bool>,
}

impl AttributeDefinitionCreate {
    pub fn technical_name(&self) -> String {
        self.key.clone().unwrap_or_else(|| naming::derive(&self.label, NameKind::Field))
    }
}

// `classId` and `key` are immutable (the column's table and name), and so are
// `referenceClassId` and `lookupListId`: stored values depend on them.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttributeDefinitionUpdate {
    // Changes the column type. The conversion is dry-run over every stored
    // value first and refused (422 SCHEMA_CHANGE_REFUSED) if any would not
    // convert. Only between text, number, integer, boolean, enum, date,
    // datetime, ip and cidr; enumValues is cleared when leaving enum. (A doc
    // comment here would turn the inline enum into an allOf with an object.)
    #[schema(inline, nullable = false)]
    data_type: Option<AttributeDataType>,
    #[schema(schema_with = name_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    label: Option<String>,
    #[schema(schema_with = description_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    description: Option<Option<String>>,
    #[schema(nullable = false)]
    is_required: Option<bool>,
    #[schema(schema_with = enum_values_schema)]
    #[serde(default, deserialize_with = "trimmed_list")]
    enum_values: Option<Option<Vec<String>>>,
    #[schema(schema_with = validation_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    validation: Option<Option<ValidationRules>>,
    #[schema(schema_with = group_name_schema)]
    #[serde(default, deserialize_with = "schemas::patch_trimmed")]
    group_name: Option<Option<String>>,
    #[schema(schema_with = help_text_schema)]
    #[serde(default, deserialize_with = "schemas::patch_trimmed")]
    help_text: Option<Option<String>>,
    #[schema(schema_with = default_value_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    default_value: Option<Option<Value>>,
    #[schema(schema_with = sort_order_schema)]
    sort_order: Option<i32>,
    #[schema(nullable = false)]
    is_active: Option<bool>,
    /// Lookup fields on a list with a parent list: the field bound to the parent list (this class or an ancestor)
    #[schema(schema_with = nullable_uuid_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    parent_attribute_id: Option<Option<Uuid>>,
}

fn json_list(v: &[String]) -> Value {
    Value::Array(v.iter().cloned().map(Value::String).collect())
}

fn json_rules(v: &ValidationRules) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

impl Writable for AttributeDefinitionCreate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("class_id", Some(self.class_id))
            .opt("key", Some(self.technical_name()))
            .opt("data_type", Some(self.data_type.as_str().to_owned()))
            .opt("reference_class_id", self.reference_class_id.map(Some))
            .opt("lookup_list_id", self.lookup_list_id.map(Some))
            .opt("parent_attribute_id", self.parent_attribute_id.map(Some))
            .opt("label", Some(self.label.clone()))
            .opt("description", self.description.clone().map(Some))
            .opt("is_required", self.is_required)
            .opt("enum_values", self.enum_values.clone().map(|v| v.map(|l| json_list(&l))))
            .opt("validation", self.validation.as_ref().map(|v| Some(json_rules(v))))
            .opt("group_name", self.group_name.clone().map(Some))
            .opt("help_text", self.help_text.clone().map(Some))
            .opt("default_value", self.default_value.clone().filter(|v| !v.is_null()).map(Some))
            .opt("sort_order", self.sort_order)
            .opt("is_active", self.is_active);
        c
    }
}

impl Check for AttributeDefinitionCreate {
    fn check(&self) -> Vec<FieldError> {
        let mut errors = Vec::new();
        let has_enum = matches!(self.enum_values, Some(Some(_)));
        let is_enum = self.data_type == AttributeDataType::Enum;
        let is_ref = self.data_type == AttributeDataType::Reference;
        if is_enum && !has_enum {
            errors.push(custom("enumValues", "Required for enum attributes"));
        }
        if !is_enum && has_enum {
            errors.push(custom("enumValues", "Only allowed for enum attributes"));
        }
        if is_ref && self.reference_class_id.is_none() {
            errors.push(custom("referenceClassId", "Required for reference attributes"));
        }
        if !is_ref && self.reference_class_id.is_some() {
            errors.push(custom("referenceClassId", "Only allowed for reference attributes"));
        }
        let is_lookup = self.data_type == AttributeDataType::Lookup;
        if is_lookup && self.lookup_list_id.is_none() {
            errors.push(custom("lookupListId", "Required for lookup attributes"));
        }
        if !is_lookup && self.lookup_list_id.is_some() {
            errors.push(custom("lookupListId", "Only allowed for lookup attributes"));
        }
        if !is_lookup && self.parent_attribute_id.is_some() {
            errors.push(custom("parentAttributeId", "Only allowed for lookup attributes"));
        }
        if is_ref && self.default_value.as_ref().is_some_and(|v| !v.is_null()) {
            errors.push(custom("defaultValue", "Reference attributes cannot have a default"));
        }
        if let Some(v) = &self.validation {
            v.check(self.data_type, &mut errors);
        }
        errors
    }
}

impl Writable for AttributeDefinitionUpdate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        // Leaving enum drops the list unless the body sets it (the definition's check requires that).
        let enum_values = match (self.data_type, &self.enum_values) {
            (Some(t), None) if t != AttributeDataType::Enum => Some(None),
            _ => self.enum_values.clone().map(|v| v.map(|l| json_list(&l))),
        };
        c.opt("data_type", self.data_type.map(|t| t.as_str().to_owned()))
            .opt("enum_values", enum_values)
            .opt("label", self.label.clone())
            .opt("description", self.description.clone())
            .opt("is_required", self.is_required)
            .opt("validation", self.validation.as_ref().map(|v| v.as_ref().map(json_rules)))
            .opt("group_name", self.group_name.clone())
            .opt("help_text", self.help_text.clone())
            .opt("default_value", self.default_value.clone())
            .opt("sort_order", self.sort_order)
            .opt("is_active", self.is_active)
            .opt("parent_attribute_id", self.parent_attribute_id);
        c
    }
}
impl Check for AttributeDefinitionUpdate {
    fn check(&self) -> Vec<FieldError> {
        let mut errors = non_empty(&self.columns());
        if matches!(self.data_type, Some(AttributeDataType::Reference | AttributeDataType::Lookup)) {
            errors
                .push(custom("dataType", "A field cannot become a reference or lookup field; add a new field instead"));
        }
        errors
    }
}

fn defined_on_schema() -> Schema {
    schemas::uuid_list_described("Defined directly on these classes")
}

fn attribute_sort() -> Schema {
    schemas::sort_schema(AttributeDefinitionList::SORT_FIELDS, "sortOrder")
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct AttributeDefinitionList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    offset: i64,
    #[param(schema_with = schemas::search_schema)]
    q: Option<String>,
    #[param(required = false, schema_with = attribute_sort)]
    sort: Sort,
    #[param(schema_with = defined_on_schema)]
    class_id: Option<UuidList>,
    /// Everything a CI of this class can carry, including inherited definitions
    effective_for_class_id: Option<Uuid>,
    #[param(inline)]
    data_type: Option<AttributeDataType>,
    #[param(inline)]
    is_active: Option<QueryBool>,
    #[param(inline)]
    is_required: Option<QueryBool>,
}
paged!(AttributeDefinitionList);

impl ListQuery for AttributeDefinitionList {
    const SORT_FIELDS: &'static [&'static str] = &["sortOrder", "key", "label", "createdAt", "updatedAt"];
    fn q(&self) -> Option<&str> {
        self.q.as_deref()
    }
    fn sort(&self) -> &Sort {
        &self.sort
    }
    fn filter(&self, w: &mut Where<'_>) {
        if let Some(ids) = &self.class_id {
            w.and().push("class_id = ANY(").push_bind(ids.0.clone()).push(")");
        }
        if let Some(id) = self.effective_for_class_id {
            w.and().push("ci_class_is_a(").push_bind(id).push(", class_id)");
        }
        if let Some(t) = self.data_type {
            w.and().push("data_type = ").push_bind(t.as_str());
        }
        bool_filter(w, "is_active", self.is_active);
        bool_filter(w, "is_required", self.is_required);
    }
}

pub struct AttributeDefinitions;

impl Resource for AttributeDefinitions {
    type Dto = AttributeDefinition;
    type Create = AttributeDefinitionCreate;
    type Update = AttributeDefinitionUpdate;
    type List = AttributeDefinitionList;
    const TABLE: &'static str = "ci_attribute_definitions";
    const LABEL: &'static str = "Attribute definition";
    const BASE_PATH: &'static str = "/api/v1/attribute-definitions";
    const TAG: &'static str = "Attribute definitions";
    const SINGULAR: &'static str = "attributeDefinition";
    const PLURAL: &'static str = "attributeDefinitions";
    const COLUMNS: &'static str = "id, class_id, key, label, description, data_type, is_required, enum_values, reference_class_id, lookup_list_id, validation, group_name, help_text, default_value, sort_order, is_active, created_at, updated_at, parent_attribute_id";
    const SEARCH_COLUMNS: &'static [&'static str] = &["key", "label", "description", "group_name"];
    const UPDATE_DESCRIPTION: &'static str = "`dataType` changes the column type: every stored value is converted in a dry run first, and the change is refused (422 SCHEMA_CHANGE_REFUSED, naming values that fail) if any would not convert (`type_change_failed`) or would lose information (`type_change_lossy`: datetime to date keeps the UTC day, so it is refused while any value has a time of day other than midnight UTC). Only between text, number, integer, boolean, enum, date, datetime, ip and cidr; `enumValues` is cleared when leaving enum. `isRequired: true` makes the column NOT NULL and is refused while an asset (deleted ones included) has no value. Removing enum values still stored is refused. `parentAttributeId` (lookup fields on a list with a parent list) names the field bound to the parent list, on this class or an ancestor; CI writes then only accept a value that belongs to the CI's value of that field. Preview any change with `POST /api/v1/schema-changes/preview`.";
    const ARCHIVE_ON_DELETE: bool = true;
    const WRITE_ERRORS: &'static [ErrorCode] = &[ErrorCode::InvalidName, ErrorCode::SchemaChangeRefused];
    const DELETE_DESCRIPTION: &'static str = "Archives the field (`isActive=false`): its column and stored values stay readable, no new values are accepted, and forms hide it. `PATCH {\"isActive\": true}` restores it. To drop the column and its values, purge the field (`POST /api/v1/attribute-definitions/{id}/purge`).";
    const USAGE: &'static [Usage] = &[Usage {
        kind: "attributeValues",
        label: "values stored on configuration items",
        sql: "SELECT cmdb.attribute_value_count($1)",
        spans: Some(ATTRIBUTE_SPANS),
        blocking: false,
    }];

    fn id(row: &AttributeDefinition) -> Uuid {
        row.id
    }

    fn validate(columns: &ColumnSet, create: bool) -> Result<(), AppError> {
        match engine::key_column(columns) {
            Some(key) if create => engine::validate_name(key, NameKind::Field, "key"),
            _ => Ok(()),
        }
    }

    fn before_write(conn: &mut PgConnection) -> BoxFuture<'_, Result<(), AppError>> {
        Box::pin(async move { Ok(engine::lock(conn).await?) })
    }

    fn after_write<'a>(
        conn: &'a mut PgConnection,
        ctx: &'a RequestContext,
        row: &'a AttributeDefinition,
        previous: Option<&'a AttributeDefinition>,
    ) -> BoxFuture<'a, Result<(), AppError>> {
        Box::pin(async move {
            if let Some(clash) = data::attribute_key_clash(conn, row.class_id, &row.key, row.id).await? {
                return Err(engine::invalid_name(
                    "key",
                    "name_taken",
                    format!(
                        "Field \"{}\" is already defined on type \"{clash}\" in the same lineage (a CI of this type \
                         would carry both)",
                        row.key
                    ),
                ));
            }
            if let Some(p) = previous
                && p.data_type != row.data_type
                && matches!(p.data_type, AttributeDataType::Reference | AttributeDataType::Lookup)
            {
                return Err(engine::refused(
                    "dataType",
                    "type_change_unsupported",
                    "Reference and lookup fields cannot change type; add a new field instead".into(),
                ));
            }
            if previous.is_some() {
                check_changed_definition(row)?;
            }
            let titled: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM cmdb.ci_classes WHERE title_attribute_id = $1")
                .bind(row.id)
                .fetch_all(&mut *conn)
                .await?;
            if !titled.is_empty() && !TITLE_DATA_TYPES.contains(&row.data_type) {
                return Err(engine::refused(
                    "dataType",
                    "title_attribute_type",
                    format!(
                        "This field labels the CIs of {} types; a {} field cannot. Choose another title attribute \
                         on those types first.",
                        titled.len(),
                        row.data_type.as_str()
                    ),
                ));
            }
            check_default_value(conn, row).await?;
            let table: String =
                sqlx::query_scalar("SELECT cmdb.type_table($1)").bind(row.class_id).fetch_one(&mut *conn).await?;
            let verb = match previous {
                None => "Add field",
                Some(p) if p.is_active && !row.is_active => "Archive field",
                Some(p) if !p.is_active && row.is_active => "Restore field",
                Some(_) => "Update field",
            };
            let summary = format!("{verb} {table}.{}", row.key);
            engine::apply(conn, ctx, &summary, Scope::Classes(vec![row.class_id]), Purge::default()).await?;
            // A converted title field reads differently.
            if previous.is_some_and(|p| p.data_type != row.data_type) && !titled.is_empty() {
                let model = Model::load(conn).await?;
                items_data::refresh_labels(conn, &model, &titled, None).await?;
            }
            Ok(())
        })
    }
}

/// Drops the column of an archived field and deletes its definition.
pub async fn purge_attribute_in(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    id: Uuid,
    confirm: &str,
) -> Result<Option<SchemaChange>, AppError> {
    engine::lock(conn).await?;
    let row: AttributeDefinition =
        crud::select_by_id(conn, AttributeDefinitions::TABLE, AttributeDefinitions::COLUMNS, id, true)
            .await?
            .ok_or_else(|| AppError::missing(AttributeDefinitions::LABEL, id))?;
    check_purge("field", &row.key, row.is_active, confirm)?;
    let dependents: Vec<String> = sqlx::query_scalar(
        "SELECT c.key || '.' || d.key FROM ci_attribute_definitions d JOIN ci_classes c ON c.id = d.class_id
         WHERE d.parent_attribute_id = $1 ORDER BY 1",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    if !dependents.is_empty() {
        return Err(AppError::new(
            ErrorCode::InUse,
            format!(
                "Fields {} use this field as their parent field; unlink them (parentAttributeId: null) first",
                dependents.join(", ")
            ),
        ));
    }
    let model = Model::load(conn).await?;
    let table = model.table(row.class_id).ok_or_else(AppError::internal)?;
    let titled: Vec<Uuid> = model.classes.iter().filter(|c| c.title_attribute_id == Some(id)).map(|c| c.id).collect();
    // The foreign key clears the title of the types it labelled; their CIs fall back to the ident.
    crud::delete_row(conn, AttributeDefinitions::TABLE, id).await?;
    let summary = format!("Purge field {}.{}", table.display(), row.key);
    let purge = Purge {
        columns: vec![(table, Ident::trusted(&row.key))],
        classes: model.subtree(row.class_id),
        ..Purge::default()
    };
    let change = engine::apply(conn, ctx, &summary, Scope::Classes(vec![row.class_id]), purge).await?;
    if !titled.is_empty() {
        let model = Model::load(conn).await?;
        items_data::refresh_labels(conn, &model, &titled, None).await?;
    }
    let entry = AuditEntry {
        action: AuditAction::Delete,
        entity_type: AttributeDefinitions::TABLE,
        entity_id: id,
        old_value: Some(crud::json(&row)),
        new_value: None,
    };
    crud::write_audit(conn, ctx, vec![entry]).await?;
    Ok(change)
}

/// Validation rules of an edited definition fit its (possibly new) data type.
/// Enum values still stored are checked by the DDL engine when it replaces the
/// column's CHECK constraint.
fn check_changed_definition(row: &AttributeDefinition) -> Result<(), AppError> {
    let rules: ValidationRules = row
        .validation
        .as_ref()
        .and_then(|v| serde_json::from_value(Value::Object(v.0.clone())).ok())
        .unwrap_or_default();
    let mut issues = Vec::new();
    rules.check(row.data_type, &mut issues);
    if !issues.is_empty() {
        for i in &mut issues {
            i.code = "invalid".into();
        }
        return Err(AppError::validation(issues));
    }
    Ok(())
}

/// The default must be a value a CI of this attribute could hold.
async fn check_default_value(conn: &mut PgConnection, row: &AttributeDefinition) -> Result<(), AppError> {
    let Some(default) = row.default_value.as_ref().map(|d| &d.0) else { return Ok(()) };
    let schema = crate::modules::items::value_schema(
        row.data_type,
        row.enum_values.as_ref().map(|v| v.0.as_slice()),
        row.validation.as_ref().map(|v| &v.0),
    );
    let problems = validate::check(&schema, default, FieldLocation::Body, None);
    if let Some(p) = problems.into_iter().next() {
        return Err(AppError::field(
            "defaultValue",
            format!("Not a valid value for this attribute: {}", p.message),
            "invalid",
        ));
    }
    if let (Some(list_id), Some(value_id)) =
        (row.lookup_list_id, default.as_str().and_then(|v| Uuid::parse_str(v).ok()))
    {
        match data::lookup_value_state(conn, list_id, value_id).await? {
            Some(true) => {}
            Some(false) => return Err(AppError::field("defaultValue", "This list value is retired", "invalid")),
            None => {
                return Err(AppError::field("defaultValue", "Not a value of the attribute's lookup list", "not_found"));
            }
        }
    }
    Ok(())
}

// ===========================================================================
// Relationship types and rules
// ===========================================================================

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationshipType {
    pub id: Uuid,
    pub key: String,
    pub name: String,
    #[schema(required = true)]
    pub description: Option<String>,
    /// Reads source -> target, e.g. "runs on"
    pub forward_label: String,
    /// Reads target -> source, e.g. "hosts"
    pub reverse_label: String,
    /// false for symmetric types such as connected_to
    pub is_directional: bool,
    pub sort_order: i32,
    pub is_active: bool,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationshipTypeCreate {
    #[schema(schema_with = key_schema)]
    key: String,
    #[schema(nullable = false)]
    is_directional: Option<bool>,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    name: String,
    #[schema(schema_with = description_schema)]
    #[serde(default)]
    description: Option<String>,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    forward_label: String,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    reverse_label: String,
    #[schema(schema_with = sort_order_schema)]
    sort_order: Option<i32>,
    #[schema(nullable = false)]
    is_active: Option<bool>,
}

// `key` and `isDirectional` are immutable: existing edges were validated against them.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationshipTypeUpdate {
    #[schema(schema_with = name_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    name: Option<String>,
    #[schema(schema_with = description_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    description: Option<Option<String>>,
    #[schema(schema_with = name_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    forward_label: Option<String>,
    #[schema(schema_with = name_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    reverse_label: Option<String>,
    #[schema(schema_with = sort_order_schema)]
    sort_order: Option<i32>,
    #[schema(nullable = false)]
    is_active: Option<bool>,
}

impl Writable for RelationshipTypeCreate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("key", Some(self.key.clone()))
            .opt("is_directional", self.is_directional)
            .opt("name", Some(self.name.clone()))
            .opt("description", self.description.clone().map(Some))
            .opt("forward_label", Some(self.forward_label.clone()))
            .opt("reverse_label", Some(self.reverse_label.clone()))
            .opt("sort_order", self.sort_order)
            .opt("is_active", self.is_active);
        c
    }
}
impl Check for RelationshipTypeCreate {}

impl Writable for RelationshipTypeUpdate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("name", self.name.clone())
            .opt("description", self.description.clone())
            .opt("forward_label", self.forward_label.clone())
            .opt("reverse_label", self.reverse_label.clone())
            .opt("sort_order", self.sort_order)
            .opt("is_active", self.is_active);
        c
    }
}
impl Check for RelationshipTypeUpdate {
    fn check(&self) -> Vec<FieldError> {
        non_empty(&self.columns())
    }
}

fn relationship_type_sort() -> Schema {
    schemas::sort_schema(RelationshipTypeList::SORT_FIELDS, "sortOrder")
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct RelationshipTypeList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    offset: i64,
    #[param(schema_with = schemas::search_schema)]
    q: Option<String>,
    #[param(required = false, schema_with = relationship_type_sort)]
    sort: Sort,
    #[param(inline)]
    is_active: Option<QueryBool>,
    /// Only types a CI of this class may use as source (rules are inherited)
    source_class_id: Option<Uuid>,
    /// Only types a CI of this class may use as target; combine with sourceClassId for a pair
    target_class_id: Option<Uuid>,
}
paged!(RelationshipTypeList);

/// `(ci_class_is_a(a, r.source_class_id) AND ci_class_is_a(b, r.target_class_id))`, either side optional.
fn push_pair(w: &mut sqlx::QueryBuilder<sqlx::Postgres>, a: Option<Uuid>, b: Option<Uuid>) {
    w.push("(");
    match a {
        Some(a) => w.push("ci_class_is_a(").push_bind(a).push(", r.source_class_id)"),
        None => w.push("true"),
    };
    w.push(" AND ");
    match b {
        Some(b) => w.push("ci_class_is_a(").push_bind(b).push(", r.target_class_id)"),
        None => w.push("true"),
    };
    w.push(")");
}

impl ListQuery for RelationshipTypeList {
    const SORT_FIELDS: &'static [&'static str] = &["sortOrder", "name", "key", "createdAt", "updatedAt"];
    fn q(&self) -> Option<&str> {
        self.q.as_deref()
    }
    fn sort(&self) -> &Sort {
        &self.sort
    }
    fn filter(&self, w: &mut Where<'_>) {
        bool_filter(w, "is_active", self.is_active);
        let (src, tgt) = (self.source_class_id, self.target_class_id);
        if src.is_some() || tgt.is_some() {
            let qb = w.and();
            qb.push(
                "EXISTS (SELECT 1 FROM relationship_type_rules r WHERE r.relationship_type_id = relationship_types.id AND (",
            );
            push_pair(qb, src, tgt);
            qb.push(" OR (NOT relationship_types.is_directional AND ");
            push_pair(qb, tgt, src);
            qb.push(")))");
        }
    }
}

pub struct RelationshipTypes;

impl Resource for RelationshipTypes {
    type Dto = RelationshipType;
    type Create = RelationshipTypeCreate;
    type Update = RelationshipTypeUpdate;
    type List = RelationshipTypeList;
    const TABLE: &'static str = "relationship_types";
    const LABEL: &'static str = "Relationship type";
    const BASE_PATH: &'static str = "/api/v1/relationship-types";
    const TAG: &'static str = "Relationship types";
    const SINGULAR: &'static str = "relationshipType";
    const PLURAL: &'static str = "relationshipTypes";
    const COLUMNS: &'static str = "id, key, name, description, forward_label, reverse_label, is_directional, sort_order, is_active, created_at, updated_at";
    const SEARCH_COLUMNS: &'static [&'static str] = &["key", "name", "forward_label", "reverse_label"];
    const USAGE: &'static [Usage] = &[
        Usage {
            kind: "relationships",
            label: "relationships",
            sql: "SELECT count(*) FROM ci_relationships WHERE relationship_type_id = $1 AND deleted_at IS NULL",
            spans: Some(EVERY_CLASS_SPANS),
            blocking: true,
        },
        Usage {
            kind: "deletedRelationships",
            label: "deleted relationships (kept for history)",
            sql: "SELECT count(*) FROM ci_relationships WHERE relationship_type_id = $1 AND deleted_at IS NOT NULL",
            spans: Some(EVERY_CLASS_SPANS),
            blocking: true,
        },
        Usage {
            kind: "relationshipRules",
            label: "relationship rules (removed with the type)",
            sql: "SELECT count(*) FROM relationship_type_rules WHERE relationship_type_id = $1",
            spans: None,
            blocking: false,
        },
    ];

    fn id(row: &RelationshipType) -> Uuid {
        row.id
    }
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationshipRule {
    pub id: Uuid,
    pub relationship_type_id: Uuid,
    /// Matches this class and all its descendants
    pub source_class_id: Uuid,
    /// Matches this class and all its descendants
    pub target_class_id: Uuid,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationshipRuleCreate {
    relationship_type_id: Uuid,
    source_class_id: Uuid,
    target_class_id: Uuid,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationshipRuleUpdate {
    #[schema(nullable = false)]
    relationship_type_id: Option<Uuid>,
    #[schema(nullable = false)]
    source_class_id: Option<Uuid>,
    #[schema(nullable = false)]
    target_class_id: Option<Uuid>,
}

impl Writable for RelationshipRuleCreate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("relationship_type_id", Some(self.relationship_type_id))
            .opt("source_class_id", Some(self.source_class_id))
            .opt("target_class_id", Some(self.target_class_id));
        c
    }
}
impl Check for RelationshipRuleCreate {}

impl Writable for RelationshipRuleUpdate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("relationship_type_id", self.relationship_type_id)
            .opt("source_class_id", self.source_class_id)
            .opt("target_class_id", self.target_class_id);
        c
    }
}
impl Check for RelationshipRuleUpdate {
    fn check(&self) -> Vec<FieldError> {
        non_empty(&self.columns())
    }
}

fn rule_sort() -> Schema {
    schemas::sort_schema(RelationshipRuleList::SORT_FIELDS, "createdAt")
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct RelationshipRuleList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    offset: i64,
    #[param(required = false, schema_with = rule_sort)]
    sort: Sort,
    #[param(schema_with = schemas::uuid_list_schema)]
    relationship_type_id: Option<UuidList>,
    /// Rules on this class or an ancestor (i.e. rules that apply to it)
    source_class_id: Option<Uuid>,
    /// Rules on this class or an ancestor (i.e. rules that apply to it)
    target_class_id: Option<Uuid>,
}
paged!(RelationshipRuleList);

impl ListQuery for RelationshipRuleList {
    const SORT_FIELDS: &'static [&'static str] = &["createdAt", "updatedAt"];
    fn sort(&self) -> &Sort {
        &self.sort
    }
    fn filter(&self, w: &mut Where<'_>) {
        if let Some(ids) = &self.relationship_type_id {
            w.and().push("relationship_type_id = ANY(").push_bind(ids.0.clone()).push(")");
        }
        if let Some(id) = self.source_class_id {
            w.and().push("ci_class_is_a(").push_bind(id).push(", source_class_id)");
        }
        if let Some(id) = self.target_class_id {
            w.and().push("ci_class_is_a(").push_bind(id).push(", target_class_id)");
        }
    }
}

pub struct RelationshipRules;

impl Resource for RelationshipRules {
    type Dto = RelationshipRule;
    type Create = RelationshipRuleCreate;
    type Update = RelationshipRuleUpdate;
    type List = RelationshipRuleList;
    const TABLE: &'static str = "relationship_type_rules";
    const LABEL: &'static str = "Relationship rule";
    const BASE_PATH: &'static str = "/api/v1/relationship-rules";
    const TAG: &'static str = "Relationship types";
    const SINGULAR: &'static str = "relationshipRule";
    const PLURAL: &'static str = "relationshipRules";
    const COLUMNS: &'static str = "id, relationship_type_id, source_class_id, target_class_id, created_at, updated_at";
    const SEARCH_COLUMNS: &'static [&'static str] = &[];
    const DELETE_DESCRIPTION: &'static str =
        "Hard delete. Existing relationships stay; new ones need another matching rule.";
    const USAGE: &'static [Usage] = &[Usage {
        kind: "relationships",
        label: "live relationships between classes it covers (they stay after a delete)",
        sql: "SELECT count(*)
              FROM relationship_type_rules r
              JOIN relationship_types t ON t.id = r.relationship_type_id
              JOIN ci_relationships e ON e.relationship_type_id = r.relationship_type_id AND e.deleted_at IS NULL
              JOIN configuration_items s ON s.id = e.source_ci_id
              JOIN configuration_items g ON g.id = e.target_ci_id
              WHERE r.id = $1
                AND ((ci_class_is_a(s.class_id, r.source_class_id) AND ci_class_is_a(g.class_id, r.target_class_id))
                  OR (NOT t.is_directional
                      AND ci_class_is_a(g.class_id, r.source_class_id) AND ci_class_is_a(s.class_id, r.target_class_id)))",
        spans: Some(RULE_SPANS),
        blocking: false,
    }];

    fn id(row: &RelationshipRule) -> Uuid {
        row.id
    }
}

// ===========================================================================

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct EffectiveQuery {
    /// Include retired definitions (default false)
    #[param(inline)]
    include_inactive: Option<QueryBool>,
}

pub async fn effective_attributes(
    pool: &sqlx::PgPool,
    class_id: Uuid,
    include_inactive: bool,
) -> Result<EffectiveAttributeList, AppError> {
    let mut conn = pool.acquire().await?;
    if !data::class_exists(&mut conn, class_id).await? {
        return Err(AppError::missing("CI class", class_id));
    }
    let rows = data::effective_attributes(&mut conn, class_id).await?;
    Ok(EffectiveAttributeList {
        data: rows.into_iter().filter(|r| include_inactive || r.is_active).map(EffectiveAttribute::from).collect(),
    })
}

pub fn routes() -> Vec<Route> {
    let effective = route(Method::GET, "/api/v1/ci-classes/{id}/attributes", "listCiClassEffectiveAttributes")
        .tag("CI classes")
        .summary("Every attribute a CI of this class can carry, including inherited ones")
        .description("Ordered root class first, then by sortOrder. Use it to render the CI form for a class.")
        .errors(&[ErrorCode::NotFound])
        .handle(|api, In(IdPath(id), Query(q), NoBody): In<IdPath, Query<EffectiveQuery>, NoBody>| async move {
            let include = q.include_inactive.map(bool::from).unwrap_or(false);
            Ok(Json(effective_attributes(&api.pool, id, include).await?))
        });

    let purge_class = route(Method::POST, "/api/v1/ci-classes/{id}/purge", "purgeCiClass")
        .tag("CI classes")
        .summary("Purge an archived type: drop its table and delete its CIs")
        .description(
            "Irreversible. The type must be archived (DELETE) and `confirm` must repeat its technical name. Deletes \
             its CIs (deleted ones included) with their relationships, its fields and relationship rules, and drops \
             its table and reporting view. Refused (409 IN_USE) while it has subtypes or other types have reference \
             fields pointing at it. Returns the schema change that ran.",
        )
        .requires(GlobalPermission::DatamodelManage)
        .errors(&[ErrorCode::NotFound, ErrorCode::InUse, ErrorCode::Conflict])
        .handle(|api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<PurgeRequest>>| async move {
            let mut tx = api.pool.begin().await?;
            let change = purge_class_in(&mut tx, &api.ctx, id, &b.confirm).await?;
            tx.commit().await?;
            Ok(Json(PurgeResult { schema_change: change }))
        });
    let purge_attribute = route(Method::POST, "/api/v1/attribute-definitions/{id}/purge", "purgeAttributeDefinition")
        .tag("Attribute definitions")
        .summary("Purge an archived field: drop its column and values")
        .description(
            "Irreversible. The field must be archived (DELETE) and `confirm` must repeat its technical name. Drops the \
             column (and every stored value) from the type's table and rebuilds the reporting views. Returns the \
             schema change that ran. Refused (409 IN_USE) while other fields name it as their parent field.",
        )
        .requires(GlobalPermission::DatamodelManage)
        .errors(&[ErrorCode::NotFound, ErrorCode::Conflict, ErrorCode::InUse])
        .handle(|api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<PurgeRequest>>| async move {
            let mut tx = api.pool.begin().await?;
            let change = purge_attribute_in(&mut tx, &api.ctx, id, &b.confirm).await?;
            tx.commit().await?;
            Ok(Json(PurgeResult { schema_change: change }))
        });

    let mut r = simple::routes::<CiClasses>();
    r.push(effective);
    r.push(purge_class);
    r.extend(simple::routes::<AttributeDefinitions>());
    r.push(purge_attribute);
    r.extend(simple::routes::<RelationshipTypes>());
    r.extend(simple::routes::<RelationshipRules>());
    r
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::db::scratch;
    use crate::modules::items::schemas::CreateItemBody;
    use crate::modules::relationships::{self, RelationshipCreate};

    fn body<T: serde::de::DeserializeOwned>(value: Value) -> T {
        serde_json::from_value(value).unwrap()
    }

    /// GH#109: text attributes can be flagged multi-line, and their values keep
    /// their line breaks exactly as sent.
    #[tokio::test]
    async fn multiline_text_attributes_keep_line_breaks() {
        use crate::api::route::Check;
        let Some(db) = scratch::database("multiline_text_attributes_keep_line_breaks").await else { return };
        let pool = &db.pool;
        let ctx = RequestContext::system("multiline-test", "multiline-test");

        let class: CiClass = simple::create::<CiClasses>(pool, &ctx, &body(json!({"name": "Note Box"}))).await.unwrap();
        let create = json!({"classId": class.id, "key": "remarks", "label": "Remarks", "dataType": "text",
                            "validation": {"maxLength": 100, "multiline": true}});
        assert!(body::<AttributeDefinitionCreate>(create.clone()).check().is_empty());
        let field: AttributeDefinition =
            simple::create::<AttributeDefinitions>(pool, &ctx, &body(create)).await.unwrap();
        assert_eq!(serde_json::to_value(&field).unwrap()["validation"], json!({"maxLength": 100, "multiline": true}));

        // Only for text attributes.
        let number = body::<AttributeDefinitionCreate>(json!({"classId": class.id, "label": "Count",
            "dataType": "number", "validation": {"multiline": true}}));
        let errors = number.check();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].message, "multiline applies to text attributes only");
        // An update is checked against the stored definition.
        let count: AttributeDefinition = simple::create::<AttributeDefinitions>(
            pool,
            &ctx,
            &body(json!({"classId": class.id, "label": "Count", "dataType": "number"})),
        )
        .await
        .unwrap();
        let err = simple::update::<AttributeDefinitions>(
            pool,
            &ctx,
            count.id,
            &body(json!({"validation": {"multiline": true}})),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::ValidationError, "{err:?}");

        // Cleared by leaving it out (or sending false, which is not stored).
        let cleared: AttributeDefinition = simple::update::<AttributeDefinitions>(
            pool,
            &ctx,
            field.id,
            &body(json!({"validation": {"maxLength": 100, "multiline": false}})),
        )
        .await
        .unwrap();
        assert_eq!(serde_json::to_value(&cleared).unwrap()["validation"], json!({"maxLength": 100}));
        simple::update::<AttributeDefinitions>(pool, &ctx, field.id, &body(json!({"validation": {"multiline": true}})))
            .await
            .unwrap();

        let text = "  Line 1\nLine 2\r\n\r\n\tLine 4\n";
        let item = body::<CreateItemBody>(json!({"classId": class.id, "attributes": {"remarks": text}}));
        let id = items_service::create(pool, &ctx, &item).await.unwrap().summary.id;
        let stored = items_service::get(pool, &ctx, id).await.unwrap();
        assert_eq!(serde_json::to_value(&stored).unwrap()["attributes"]["remarks"], json!(text));
        let update =
            body::<crate::modules::items::schemas::UpdateItemBody>(json!({"attributes": {"remarks": "a\r\nb\n"}}));
        items_service::update(pool, &ctx, id, &update).await.unwrap();
        let stored = items_service::get(pool, &ctx, id).await.unwrap();
        assert_eq!(serde_json::to_value(&stored).unwrap()["attributes"]["remarks"], json!("a\r\nb\n"));
        db.drop().await;
    }

    /// GH#180: a refused field change quotes stored values only to a caller who
    /// may view every type whose assets store the field (the type and its subtypes).
    #[tokio::test]
    async fn refused_field_changes_quote_values_only_to_callers_who_may_view_them() {
        use crate::auth::permissions::{ClassRights, Permissions};
        use crate::modules::schema_changes::{self, PreviewRequest};
        let Some(db) = scratch::database("refused_field_changes_quote_values_only_to_callers_who_may_view_them").await
        else {
            return;
        };
        let pool = &db.pool;
        let ctx = RequestContext::system("gh180-test", "gh180-test");

        let secrets: CiClass =
            simple::create::<CiClasses>(pool, &ctx, &body(json!({"name": "Secrets"}))).await.unwrap();
        let vault: CiClass =
            simple::create::<CiClasses>(pool, &ctx, &body(json!({"name": "Vault Secrets", "parentId": secrets.id})))
                .await
                .unwrap();
        let field: AttributeDefinition = simple::create::<AttributeDefinitions>(
            pool,
            &ctx,
            &body(json!({"classId": secrets.id, "key": "code", "label": "Code", "dataType": "text"})),
        )
        .await
        .unwrap();
        for (class, code) in [(secrets.id, "s3cr3t-alpha"), (vault.id, "s3cr3t-vault")] {
            let item = body::<CreateItemBody>(json!({"classId": class, "attributes": {"code": code}}));
            items_service::create(pool, &ctx, &item).await.unwrap();
        }

        let manager = |view: &[Uuid]| {
            let permissions = Permissions {
                global: [GlobalPermission::DatamodelManage].into(),
                classes: view.iter().map(|id| (*id, ClassRights { view: true, ..Default::default() })).collect(),
                ..Default::default()
            };
            let principal = crate::auth::Principal {
                user_id: Uuid::new_v4(),
                username: "modeller".into(),
                credential: crate::auth::Credential::Token,
                permissions,
            };
            RequestContext::user(std::sync::Arc::new(principal), "gh180".into())
        };
        let refusal = |err: AppError| {
            assert_eq!(err.code, ErrorCode::SchemaChangeRefused, "{err:?}");
            let detail = &err.details.as_ref().expect("details")[0];
            (detail.code.clone(), err.message.clone())
        };
        let changes = [
            (json!({"dataType": "enum", "enumValues": ["~"]}), "enum_value_in_use"),
            (json!({"dataType": "integer"}), "type_change_failed"),
            (json!({"dataType": "date"}), "type_change_failed"),
        ];

        // GH#221: datamodel managers who may not view every type storing the field
        // (here: the subtype, or neither type) are refused before any value is
        // read, so neither the outcome nor a count tells them what is stored.
        for scope in [vec![secrets.id], vec![]] {
            let caller = manager(&scope);
            for (change, _) in &changes {
                let preview: PreviewRequest =
                    body(json!({"operation": "updateField", "id": field.id, "body": change.clone()}));
                let via_preview = schema_changes::preview(pool, &caller, &preview).await.unwrap_err();
                let via_patch = simple::update::<AttributeDefinitions>(pool, &caller, field.id, &body(change.clone()))
                    .await
                    .unwrap_err();
                for err in [via_preview, via_patch] {
                    assert_eq!(err.code, ErrorCode::Forbidden, "{scope:?} {change}: {err:?}");
                    assert_eq!(err.details.as_ref().expect("details")[0].code, "view_required");
                    assert!(!err.message.contains("s3cr3t") && !err.message.contains('2'), "{}", err.message);
                }
            }
        }

        // A correct guess and a wrong one get the same answer.
        let caller = manager(&[secrets.id]);
        let guess = |values: &[&str]| {
            body::<PreviewRequest>(json!({"operation": "updateField", "id": field.id,
                                          "body": {"dataType": "enum", "enumValues": values}}))
        };
        let right =
            schema_changes::preview(pool, &caller, &guess(&["s3cr3t-alpha", "s3cr3t-vault"])).await.unwrap_err();
        let wrong = schema_changes::preview(pool, &caller, &guess(&["s3cr3t-alpha", "nope"])).await.unwrap_err();
        assert_eq!((right.code, &right.message), (wrong.code, &wrong.message));
        // Only a viewer of both types sees the difference: all stored values listed, so no refusal.
        let viewer = manager(&[secrets.id, vault.id]);
        schema_changes::preview(pool, &viewer, &guess(&["s3cr3t-alpha", "s3cr3t-vault"])).await.unwrap();

        // Every refused preview is audited with the caller, the request and the reason.
        let rows: Vec<(String, Uuid, Value)> = sqlx::query_as(
            "SELECT actor_name, entity_id, new_value FROM audit_log
             WHERE action = 'schema_change.refused' AND entity_type = 'ci_attribute_definitions' ORDER BY chain_seq",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 2 * changes.len() + 2, "{rows:?}");
        let (actor, entity, last) = rows.last().unwrap();
        assert_eq!((actor.as_str(), *entity), ("modeller", field.id));
        assert_eq!(
            *last,
            json!({"preview": true, "operation": "updateField", "code": "view_required", "field": "dataType",
                   "body": {"dataType": "enum", "enumValues": ["s3cr3t-alpha", "nope"]}, "message": wrong.message})
        );

        // A caller who may view both types still gets the values to correct.
        let caller = manager(&[secrets.id, vault.id]);
        for (change, code) in &changes {
            let preview: PreviewRequest = body(json!({"operation": "updateField", "id": field.id, "body": change}));
            let (got, message) = refusal(schema_changes::preview(pool, &caller, &preview).await.unwrap_err());
            assert_eq!(got, *code);
            assert!(message.contains("\"s3cr3t-alpha\", \"s3cr3t-vault\""), "{message}");
        }
        db.drop().await;
    }

    /// GH#243: purges and re-parenting refusals tell how many values are stored,
    /// and in which fields, only to a caller who may view every type concerned.
    #[tokio::test]
    async fn purges_and_moves_count_values_only_for_callers_who_may_view_them() {
        use crate::auth::permissions::{ClassRights, Permissions};
        use crate::modules::schema_changes::{self, PreviewRequest};
        let Some(db) = scratch::database("purges_and_moves_count_values_only_for_callers_who_may_view_them").await
        else {
            return;
        };
        let pool = &db.pool;
        let ctx = RequestContext::system("gh243-test", "gh243-test");
        let manager = |view: &[Uuid]| {
            let permissions = Permissions {
                global: [GlobalPermission::DatamodelManage].into(),
                classes: view.iter().map(|id| (*id, ClassRights { view: true, ..Default::default() })).collect(),
                ..Default::default()
            };
            let principal = crate::auth::Principal {
                user_id: Uuid::new_v4(),
                username: "modeller".into(),
                credential: crate::auth::Credential::Token,
                permissions,
            };
            RequestContext::user(std::sync::Arc::new(principal), "gh243".into())
        };

        let secrets: CiClass =
            simple::create::<CiClasses>(pool, &ctx, &body(json!({"name": "Secrets"}))).await.unwrap();
        let vault: CiClass =
            simple::create::<CiClasses>(pool, &ctx, &body(json!({"name": "Vault Secrets", "parentId": secrets.id})))
                .await
                .unwrap();
        let field: AttributeDefinition = simple::create::<AttributeDefinitions>(
            pool,
            &ctx,
            &body(json!({"classId": secrets.id, "key": "code", "label": "Code", "dataType": "text"})),
        )
        .await
        .unwrap();
        for class in [secrets.id, secrets.id, vault.id] {
            let item = body::<CreateItemBody>(json!({"classId": class, "attributes": {"code": "x"}}));
            items_service::create(pool, &ctx, &item).await.unwrap();
        }
        simple::update::<AttributeDefinitions>(pool, &ctx, field.id, &body(json!({"isActive": false}))).await.unwrap();
        simple::update::<CiClasses>(pool, &ctx, vault.id, &body(json!({"isActive": false}))).await.unwrap();

        // (operation, id, confirm, impact kind, rows for a viewer, summary for a viewer, who may view it all).
        // A type purge deletes its CIs before the table is dropped: its summary carries the count.
        let purges = [
            ("purgeField", field.id, "code", "drop_column", 3, "Purge field", vec![secrets.id, vault.id]),
            (
                "purgeType",
                vault.id,
                vault.key.as_str(),
                "drop_table",
                0,
                "(1 CIs, 0 relationships deleted)",
                vec![vault.id],
            ),
        ];
        for (operation, id, confirm, kind, n, summary, viewable) in &purges {
            let request: PreviewRequest = body(json!({"operation": operation, "id": id, "body": {"confirm": confirm}}));
            let impact = |p: &schema_changes::SchemaChangePreview| {
                p.impact.iter().find(|i| i.kind == *kind).cloned().expect(kind)
            };
            // The field is stored for both types: viewing only the parent is not enough.
            for scope in [vec![secrets.id], vec![]] {
                let preview = schema_changes::preview(pool, &manager(&scope), &request).await.unwrap();
                let i = impact(&preview);
                assert_eq!(i.rows, None, "{operation} {scope:?}: {i:?}");
                assert!(!i.message.contains(char::is_numeric), "{operation} {scope:?}: {}", i.message);
                assert!(i.message.contains("are deleted"), "{}", i.message);
                let purge_summary = &preview.summaries[0];
                assert!(!purge_summary.contains(char::is_numeric), "{operation} {scope:?}: {purge_summary}");
            }
            let preview = schema_changes::preview(pool, &manager(viewable), &request).await.unwrap();
            let i = impact(&preview);
            assert_eq!(i.rows, Some(*n), "{operation}: {i:?}");
            assert!(i.message.contains(&format!("{n} ")), "{}", i.message);
            assert!(preview.summaries[0].contains(summary), "{:?}", preview.summaries);
        }

        // Moving a type whose CIs hold values in a field the new parent lacks is
        // refused either way; a caller who may not view every moved CI is refused
        // before any is read (GH#267), a viewer of them all learns which field.
        let holder: CiClass = simple::create::<CiClasses>(pool, &ctx, &body(json!({"name": "Holder"}))).await.unwrap();
        simple::create::<AttributeDefinitions>(
            pool,
            &ctx,
            &body(json!({"classId": holder.id, "key": "pin_hint", "label": "PIN hint", "dataType": "text"})),
        )
        .await
        .unwrap();
        let leaf: CiClass =
            simple::create::<CiClasses>(pool, &ctx, &body(json!({"name": "Leaf", "parentId": holder.id})))
                .await
                .unwrap();
        let sub: CiClass =
            simple::create::<CiClasses>(pool, &ctx, &body(json!({"name": "Sub Leaf", "parentId": leaf.id})))
                .await
                .unwrap();
        let item = body::<CreateItemBody>(json!({"classId": sub.id, "attributes": {"pin_hint": "x"}}));
        items_service::create(pool, &ctx, &item).await.unwrap();
        let mv = json!({"parentId": null});
        for scope in [vec![leaf.id], vec![]] {
            let err =
                simple::update::<CiClasses>(pool, &manager(&scope), leaf.id, &body(mv.clone())).await.unwrap_err();
            assert_eq!(err.code, ErrorCode::Forbidden, "{scope:?}: {err:?}");
            assert_eq!(err.details.as_ref().expect("details")[0].code, "view_required");
            assert!(!err.message.contains("pin_hint"), "{scope:?}: {}", err.message);
        }
        let err =
            simple::update::<CiClasses>(pool, &manager(&[leaf.id, sub.id]), leaf.id, &body(mv)).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::SchemaChangeRefused, "{err:?}");
        assert_eq!(err.details.as_ref().expect("details")[0].code, "attributes_outside_lineage");
        assert!(err.message.contains("pin_hint"), "{}", err.message);
        db.drop().await;
    }

    /// GH#267: making a field required, a type abstract, or moving a type
    /// succeeds or fails on the CIs stored; a datamodel manager who may not view
    /// them all is refused (and audited) before any is read, CIs or not.
    #[tokio::test]
    async fn required_abstract_and_moves_need_view_on_the_cis_they_check() {
        use crate::auth::permissions::{ClassRights, Permissions};
        use crate::modules::schema_changes::{self, PreviewRequest};
        let Some(db) = scratch::database("required_abstract_and_moves_need_view_on_the_cis_they_check").await else {
            return;
        };
        let pool = &db.pool;
        let ctx = RequestContext::system("gh267-test", "gh267-test");
        let manager = |view: &[Uuid]| {
            let permissions = Permissions {
                global: [GlobalPermission::DatamodelManage].into(),
                classes: view.iter().map(|id| (*id, ClassRights { view: true, ..Default::default() })).collect(),
                ..Default::default()
            };
            let principal = crate::auth::Principal {
                user_id: Uuid::new_v4(),
                username: "modeller".into(),
                credential: crate::auth::Credential::Token,
                permissions,
            };
            RequestContext::user(std::sync::Arc::new(principal), "gh267".into())
        };

        let secrets: CiClass =
            simple::create::<CiClasses>(pool, &ctx, &body(json!({"name": "Secrets"}))).await.unwrap();
        let vault: CiClass =
            simple::create::<CiClasses>(pool, &ctx, &body(json!({"name": "Vault Secrets", "parentId": secrets.id})))
                .await
                .unwrap();
        let other: CiClass = simple::create::<CiClasses>(pool, &ctx, &body(json!({"name": "Other"}))).await.unwrap();
        let owner: AttributeDefinition = simple::create::<AttributeDefinitions>(
            pool,
            &ctx,
            &body(json!({"classId": secrets.id, "key": "owner", "label": "Owner", "dataType": "text"})),
        )
        .await
        .unwrap();
        let probes = [
            json!({"operation": "updateField", "id": owner.id, "body": {"isRequired": true}}),
            json!({"operation": "createField",
                   "body": {"classId": secrets.id, "key": "pin", "label": "PIN", "dataType": "text", "isRequired": true}}),
            json!({"operation": "updateType", "id": vault.id, "body": {"isAbstract": true}}),
            json!({"operation": "updateType", "id": secrets.id, "body": {"parentId": other.id}}),
        ];
        let refused = || async {
            let n: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'schema_change.refused'")
                .fetch_one(pool)
                .await
                .unwrap();
            n
        };

        // Viewing Secrets but not Vault Secrets, or neither: the same 403 whether
        // or not a hidden CI (one without an owner) exists, each one audited.
        for with_cis in [false, true] {
            if with_cis {
                let item = body::<CreateItemBody>(json!({"classId": vault.id, "attributes": {}}));
                items_service::create(pool, &ctx, &item).await.unwrap();
            }
            for scope in [vec![secrets.id], vec![]] {
                let caller = manager(&scope);
                for probe in &probes {
                    let before = refused().await;
                    let err = schema_changes::preview(pool, &caller, &body(probe.clone())).await.unwrap_err();
                    assert_eq!(err.code, ErrorCode::Forbidden, "{with_cis} {scope:?} {probe}: {err:?}");
                    assert_eq!(err.details.as_ref().expect("details")[0].code, "view_required", "{probe}");
                    assert_eq!(refused().await, before + 1, "{probe}: not audited");
                }
            }
        }

        // A caller who may view every type involved gets the real answer.
        let viewer = manager(&[secrets.id, vault.id]);
        for probe in &probes {
            let err = schema_changes::preview(pool, &viewer, &body::<PreviewRequest>(probe.clone())).await;
            let code = err.err().map(|e| e.details.expect("details")[0].code.clone());
            let expected = match probe["body"].as_object().unwrap() {
                b if b.contains_key("isRequired") => Some("values_missing"),
                b if b.contains_key("isAbstract") => Some("class_has_items"),
                _ => None,
            };
            assert_eq!(code.as_deref(), expected, "{probe}");
        }
        db.drop().await;
    }

    fn counts(report: &simple::UsageReport) -> Vec<(String, Option<i64>, bool)> {
        report.data.iter().map(|u| (u.kind.clone(), u.count.exact(), u.withheld)).collect()
    }

    /// GH#265, GH#268: usage counts and a type purge's relationship count cover
    /// CIs, so they are told only to a manager who may view every class they
    /// can include; whether something is in use is still decided on them all.
    #[tokio::test]
    async fn usage_and_purge_counts_are_withheld_across_classes_the_caller_may_not_view() {
        use crate::api::context::datamodel_manager;
        use crate::modules::schema_changes::{self, PreviewRequest};
        let Some(db) = scratch::database("usage_and_purge_counts_are_withheld").await else { return };
        let pool = &db.pool;
        let ctx = RequestContext::system("gh265-test", "gh265-test");

        let servers: CiClass =
            simple::create::<CiClasses>(pool, &ctx, &body(json!({"name": "Servers"}))).await.unwrap();
        let secrets: CiClass =
            simple::create::<CiClasses>(pool, &ctx, &body(json!({"name": "Secrets"}))).await.unwrap();
        let code: AttributeDefinition = simple::create::<AttributeDefinitions>(
            pool,
            &ctx,
            &body(json!({"classId": secrets.id, "key": "code", "label": "Code", "dataType": "text"})),
        )
        .await
        .unwrap();
        let feeds: RelationshipType = simple::create::<RelationshipTypes>(
            pool,
            &ctx,
            &body(json!({"key": "feeds", "name": "Feeds", "forwardLabel": "feeds", "reverseLabel": "fed by"})),
        )
        .await
        .unwrap();
        let rule: RelationshipRule = simple::create::<RelationshipRules>(
            pool,
            &ctx,
            &body(json!({"relationshipTypeId": feeds.id, "sourceClassId": servers.id, "targetClassId": secrets.id})),
        )
        .await
        .unwrap();
        let server = items_service::create(pool, &ctx, &body::<CreateItemBody>(json!({"classId": servers.id})))
            .await
            .unwrap()
            .summary
            .id;
        let mut hidden = Vec::new();
        for value in ["a", "b"] {
            let item = body::<CreateItemBody>(json!({"classId": secrets.id, "attributes": {"code": value}}));
            hidden.push(items_service::create(pool, &ctx, &item).await.unwrap().summary.id);
        }
        let edge = json!({"relationshipTypeId": feeds.id, "sourceCiId": server, "targetCiId": hidden[0]});
        relationships::create(pool, &ctx, &body::<RelationshipCreate>(edge)).await.unwrap();

        let restricted = datamodel_manager(&[servers.id]);
        let viewer = datamodel_manager(&[servers.id, secrets.id]);

        // The hidden class: its CI counts are withheld, the data-model counts are not.
        let report = simple::usage::<CiClasses>(pool, &restricted, secrets.id).await.unwrap();
        assert!(report.in_use);
        let c = counts(&report);
        assert!(c.contains(&("configurationItems".into(), None, true)), "{c:?}");
        assert!(c.contains(&("deletedConfigurationItems".into(), None, true)), "{c:?}");
        assert!(c.contains(&("attributeDefinitions".into(), Some(1), false)), "{c:?}");
        assert!(c.contains(&("relationshipRules".into(), Some(1), false)), "{c:?}");
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["data"][0]["count"], Value::Null, "{json}");
        assert_eq!(json["data"][0]["withheld"], json!(true), "{json}");
        let c = counts(&simple::usage::<CiClasses>(pool, &viewer, secrets.id).await.unwrap());
        assert!(c.contains(&("configurationItems".into(), Some(2), false)), "{c:?}");
        let c = counts(&simple::usage::<CiClasses>(pool, &restricted, servers.id).await.unwrap());
        assert!(c.contains(&("configurationItems".into(), Some(1), false)), "{c:?}");

        let c = counts(&simple::usage::<AttributeDefinitions>(pool, &restricted, code.id).await.unwrap());
        assert_eq!(c, [("attributeValues".to_string(), None, true)]);
        let c = counts(&simple::usage::<AttributeDefinitions>(pool, &viewer, code.id).await.unwrap());
        assert_eq!(c, [("attributeValues".to_string(), Some(2), false)]);

        // A relationship type's edges may join any classes; a rule's join its classes.
        let report = simple::usage::<RelationshipTypes>(pool, &restricted, feeds.id).await.unwrap();
        assert!(report.in_use);
        let c = counts(&report);
        assert!(c.contains(&("relationships".into(), None, true)), "{c:?}");
        assert!(c.contains(&("relationshipRules".into(), Some(1), false)), "{c:?}");
        let c = counts(&simple::usage::<RelationshipTypes>(pool, &viewer, feeds.id).await.unwrap());
        assert!(c.contains(&("relationships".into(), Some(1), false)), "{c:?}");
        let c = counts(&simple::usage::<RelationshipRules>(pool, &restricted, rule.id).await.unwrap());
        assert_eq!(c, [("relationships".to_string(), None, true)]);
        let c = counts(&simple::usage::<RelationshipRules>(pool, &viewer, rule.id).await.unwrap());
        assert_eq!(c, [("relationships".to_string(), Some(1), false)]);

        // DELETE is still refused on the withheld count, without telling it.
        let err = simple::remove::<RelationshipTypes>(pool, &restricted, feeds.id).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::InUse);
        assert!(err.message.contains("still in use (details withheld)"), "{}", err.message);
        assert!(!err.message.contains(char::is_numeric), "{}", err.message);
        assert!(err.details.as_ref().is_none_or(|d| d.is_empty()), "{:?}", err.details);
        let err = simple::remove::<RelationshipTypes>(pool, &viewer, feeds.id).await.unwrap_err();
        assert!(err.message.contains("1 relationships"), "{}", err.message);

        // Purging the visible type deletes its relationship to a hidden CI: the
        // relationship count is told only to a viewer of both ends.
        simple::update::<CiClasses>(pool, &ctx, servers.id, &body(json!({"isActive": false}))).await.unwrap();
        let request: PreviewRequest =
            body(json!({"operation": "purgeType", "id": servers.id, "body": {"confirm": servers.key}}));
        let preview = schema_changes::preview(pool, &restricted, &request).await.unwrap();
        let summary = &preview.summaries[0];
        assert!(!summary.contains(char::is_numeric), "{summary}");
        assert!(summary.contains("their relationships deleted"), "{summary}");
        let preview = schema_changes::preview(pool, &viewer, &request).await.unwrap();
        assert!(preview.summaries[0].contains("(1 CIs, 1 relationships deleted)"), "{:?}", preview.summaries);
        db.drop().await;
    }

    #[tokio::test]
    async fn purging_a_type_audits_each_deleted_ci_and_relationship() {
        let Some(db) = scratch::database("purging_a_type_audits_each_deleted_ci_and_relationship").await else {
            return;
        };
        let pool = &db.pool;
        let ctx = RequestContext::system("purge-test", "purge-test");

        let class: CiClass =
            simple::create::<CiClasses>(pool, &ctx, &body(json!({"name": "Purge Box"}))).await.unwrap();
        let field: AttributeDefinition = simple::create::<AttributeDefinitions>(
            pool,
            &ctx,
            &body(json!({"classId": class.id, "label": "Rack", "dataType": "text"})),
        )
        .await
        .unwrap();
        let name: AttributeDefinition = simple::create::<AttributeDefinitions>(
            pool,
            &ctx,
            &body(json!({"classId": class.id, "key": "name", "label": "Name", "dataType": "text"})),
        )
        .await
        .unwrap();
        simple::update::<CiClasses>(pool, &ctx, class.id, &body(json!({"titleAttributeId": name.id}))).await.unwrap();
        let rel_type: Uuid = sqlx::query_scalar(
            "INSERT INTO relationship_types (key, name, forward_label, reverse_label)
             VALUES ('feeds', 'Feeds', 'feeds', 'fed by') RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO relationship_type_rules (relationship_type_id, source_class_id, target_class_id) VALUES ($1, $2, $2)",
        )
        .bind(rel_type)
        .bind(class.id)
        .execute(pool)
        .await
        .unwrap();

        let mut cis = Vec::new();
        for (name, rack) in [("box-a", "R1"), ("box-b", "R2"), ("box-c", "R3")] {
            let item = body::<CreateItemBody>(json!({
                "classId": class.id, "attributes": {field.key.clone(): rack, "name": name}
            }));
            cis.push(items_service::create(pool, &ctx, &item).await.unwrap().summary.id);
        }
        let mut edges = Vec::new();
        for (source, target) in [(0, 1), (1, 0), (0, 2)] {
            let input = json!({"relationshipTypeId": rel_type, "sourceCiId": cis[source], "targetCiId": cis[target]});
            edges.push(relationships::create(pool, &ctx, &body::<RelationshipCreate>(input)).await.unwrap().id);
        }
        // A soft-deleted edge between live CIs, and a soft-deleted CI (which
        // soft-deletes its edge to box-a): the purge must audit them too.
        relationships::remove(pool, &ctx, edges[1]).await.unwrap();
        items_service::remove(pool, &ctx, cis[2]).await.unwrap();

        simple::update::<CiClasses>(pool, &ctx, class.id, &body(json!({"isActive": false}))).await.unwrap();
        let purge_ctx = RequestContext::system("purge-test", "purge");
        let mut tx = pool.begin().await.unwrap();
        purge_class_in(&mut tx, &purge_ctx, class.id, &class.key).await.unwrap();
        tx.commit().await.unwrap();

        let rows: Vec<(String, Uuid, Value)> = sqlx::query_as(
            "SELECT entity_type, entity_id, old_value FROM audit_log
             WHERE action = 'delete' AND request_id = 'purge'
               AND entity_type IN ('configuration_items', 'ci_relationships')",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 6, "{rows:?}");
        let entry = |kind: &str, id: Uuid| {
            let found = rows.iter().filter(|(k, i, _)| k == kind && *i == id).collect::<Vec<_>>();
            assert_eq!(found.len(), 1, "one {kind} delete entry for {id}: {rows:?}");
            found[0].2.clone()
        };
        for (edge, (source, target), soft_deleted) in
            [(edges[0], (0, 1), false), (edges[1], (1, 0), true), (edges[2], (0, 2), true)]
        {
            let old = entry("ci_relationships", edge);
            assert_eq!(old["sourceCiId"], json!(cis[source]));
            assert_eq!(old["targetCiId"], json!(cis[target]));
            assert_eq!(!old["deletedAt"].is_null(), soft_deleted, "{old}");
        }
        for (ci, rack, soft_deleted) in [(cis[0], "R1", false), (cis[1], "R2", false), (cis[2], "R3", true)] {
            let old = entry("configuration_items", ci);
            assert_eq!(old["attributes"][&field.key], json!(rack), "{old}");
            assert_eq!(old["classId"], json!(class.id));
            assert_eq!(old["label"], old["attributes"]["name"], "labelled by the title attribute: {old}");
            assert_eq!(!old["deletedAt"].is_null(), soft_deleted, "{old}");
        }
        let class_entries: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_log WHERE action = 'delete' AND entity_type = 'ci_classes' AND entity_id = $1",
        )
        .bind(class.id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(class_entries, 1);
        db.drop().await;
    }

    /// GH#252: the schema change history shows the counts a change recorded
    /// (values purged or converted, assets without a value) only to a reader
    /// who may view every type counted, and `q` never matches on them.
    #[tokio::test]
    async fn the_history_counts_values_only_for_readers_who_may_view_them() {
        use crate::auth::permissions::{ClassRights, Permissions};
        use crate::modules::schema_changes::{self, SchemaChangeList};
        let Some(db) = scratch::database("the_history_counts_values_only_for_readers_who_may_view_them").await else {
            return;
        };
        let pool = &db.pool;
        let ctx = RequestContext::system("gh252-test", "gh252-test");
        let manager = |view: &[Uuid]| {
            let permissions = Permissions {
                global: [GlobalPermission::DatamodelManage].into(),
                classes: view.iter().map(|id| (*id, ClassRights { view: true, ..Default::default() })).collect(),
                ..Default::default()
            };
            let principal = crate::auth::Principal {
                user_id: Uuid::new_v4(),
                username: "modeller".into(),
                credential: crate::auth::Credential::Token,
                permissions,
            };
            RequestContext::user(std::sync::Arc::new(principal), "gh252".into())
        };

        // An unrestricted writer: Secrets with 12 codes (one CI below it), a
        // field converted to a number, then the codes and the subtype purged.
        let secrets: CiClass =
            simple::create::<CiClasses>(pool, &ctx, &body(json!({"name": "Secrets"}))).await.unwrap();
        let vault: CiClass =
            simple::create::<CiClasses>(pool, &ctx, &body(json!({"name": "Vault Secrets", "parentId": secrets.id})))
                .await
                .unwrap();
        for (key, data_type) in [("code", "text"), ("rank", "text"), ("owner_hint", "text")] {
            let def = json!({"classId": secrets.id, "key": key, "label": key, "dataType": data_type});
            simple::create::<AttributeDefinitions>(pool, &ctx, &body(def)).await.unwrap();
        }
        for i in 0..12 {
            let class = if i == 0 { vault.id } else { secrets.id };
            let item = body::<CreateItemBody>(
                json!({"classId": class, "attributes": {"code": "x", "rank": "7", "owner_hint": "y"}}),
            );
            items_service::create(pool, &ctx, &item).await.unwrap();
        }
        let defs: Vec<(String, Uuid)> =
            sqlx::query_as("SELECT key, id FROM cmdb.ci_attribute_definitions WHERE class_id = $1")
                .bind(secrets.id)
                .fetch_all(pool)
                .await
                .unwrap();
        let def = |key: &str| defs.iter().find(|(k, _)| k == key).unwrap().1;
        simple::update::<AttributeDefinitions>(pool, &ctx, def("rank"), &body(json!({"dataType": "number"})))
            .await
            .unwrap();
        simple::update::<AttributeDefinitions>(pool, &ctx, def("code"), &body(json!({"isActive": false})))
            .await
            .unwrap();
        let mut tx = pool.begin().await.unwrap();
        purge_attribute_in(&mut tx, &ctx, def("code"), "code").await.unwrap();
        tx.commit().await.unwrap();
        simple::update::<CiClasses>(pool, &ctx, vault.id, &body(json!({"isActive": false}))).await.unwrap();
        let mut tx = pool.begin().await.unwrap();
        purge_class_in(&mut tx, &ctx, vault.id, &vault.key).await.unwrap();
        tx.commit().await.unwrap();
        // A required field that 11 assets lack, kept nullable by a lenient reconcile.
        let hint = def("owner_hint");
        simple::update::<AttributeDefinitions>(pool, &ctx, hint, &body(json!({"isRequired": true}))).await.unwrap();
        let table: String =
            sqlx::query_scalar("SELECT cmdb.type_table($1)").bind(secrets.id).fetch_one(pool).await.unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "ALTER TABLE {table} ALTER COLUMN owner_hint DROP NOT NULL; UPDATE {table} SET owner_hint = NULL"
        )))
        .execute(pool)
        .await
        .unwrap();
        // Each reconcile has a view to rebuild, so that it records a change.
        let view: String = sqlx::query_scalar(
            "SELECT format('%I.%I', a.key, 'v_' || c.key) FROM cmdb.ci_classes c JOIN cmdb.areas a ON a.id = c.area_id
             WHERE c.id = $1",
        )
        .bind(secrets.id)
        .fetch_one(pool)
        .await
        .unwrap();
        let drop_view = format!("DROP VIEW {view}");
        // A restricted writer's own reconcile never counts what it may not view.
        let mut tx = pool.begin().await.unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(drop_view.clone())).execute(&mut *tx).await.unwrap();
        let own = engine::reconcile(&mut tx, &manager(&[]), "reconcile").await.unwrap().unwrap();
        tx.rollback().await.unwrap();
        let warning = own.impact.0.iter().find(|i| i.message.contains("owner_hint")).unwrap();
        assert_eq!(warning.rows, None, "{warning:?}");
        assert!(warning.message.contains("some assets have no value"), "{}", warning.message);
        let mut tx = pool.begin().await.unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(drop_view)).execute(&mut *tx).await.unwrap();
        engine::reconcile(&mut tx, &ctx, "reconcile").await.unwrap().unwrap();
        tx.commit().await.unwrap();

        let list = async |reader: &RequestContext, q: Option<&str>| {
            let query: SchemaChangeList = body(json!({"limit": 200, "offset": 0, "sort": "-occurredAt", "q": q}));
            schema_changes::list(pool, reader, &query).await.unwrap().data
        };
        let find = |changes: &[SchemaChange], kind: &str, text: &str| {
            changes
                .iter()
                .flat_map(|c| c.impact.0.iter().map(move |i| (c, i)))
                .find(|(_, i)| i.kind == kind && i.message.contains(text))
                .map(|(c, i)| (c.clone(), i.clone()))
                .unwrap_or_else(|| panic!("no {kind} impact on {text}"))
        };

        let subtree = [secrets.id, vault.id];
        for view in [Some(vec![]), Some(vec![secrets.id]), Some(vec![vault.id]), Some(subtree.to_vec()), None] {
            let reader = match &view {
                Some(v) => manager(v),
                None => ctx.clone(),
            };
            let sees = |classes: &[Uuid]| view.as_ref().is_none_or(|v| classes.iter().all(|id| v.contains(id)));
            let changes = list(&reader, None).await;
            let (purge, column) = find(&changes, "drop_column", "code");
            let (_, rewrite) = find(&changes, "rewrite", "rank");
            let (_, warning) = find(&changes, "warning", "owner_hint");
            // The subtype was purged before the reconcile: then only Secrets held the field.
            for (impact, n, counted) in
                [(&column, 12, &subtree[..]), (&rewrite, 12, &subtree), (&warning, 11, &[secrets.id])]
            {
                if sees(counted) {
                    assert_eq!(impact.rows, Some(n), "{view:?} {impact:?}");
                    assert!(impact.message.contains(&n.to_string()), "{}", impact.message);
                } else {
                    assert_eq!(impact.rows, None, "{view:?} {impact:?}");
                    assert!(!impact.message.contains(char::is_numeric), "{}", impact.message);
                }
            }
            let got = schema_changes::get(pool, &reader, purge.id).await.unwrap();
            assert_eq!(got.impact.0.iter().find(|i| i.kind == "drop_column").unwrap().rows, column.rows);

            let type_purge = changes.iter().find(|c| c.summary.starts_with("Purge type")).unwrap();
            let shown = sees(&[vault.id]);
            assert_eq!(
                type_purge.summary.contains("(1 CIs, 0 relationships deleted)"),
                shown,
                "{}",
                type_purge.summary
            );
            let found = list(&reader, Some("1 CIs")).await;
            assert_eq!(found.iter().any(|c| c.id == type_purge.id), shown, "{view:?}: q matched a hidden count");
            assert!(list(&reader, Some("Purge type")).await.iter().any(|c| c.id == type_purge.id));
        }
        db.drop().await;
    }

    /// GH#261: the audit log's schema change entries show the counts they hold
    /// only to a reader who may view every type counted, the same rule as the
    /// history (GH#252); a record from before migration 0028 never shows them
    /// to a restricted reader.
    #[tokio::test]
    async fn audit_entries_of_schema_changes_count_values_only_for_readers_who_may_view_them() {
        use crate::auth::permissions::{ClassRights, Permissions};
        use crate::modules::audit::{self, AuditQuery};
        let Some(db) = scratch::database("audit_entries_of_schema_changes_count_values_only_for_readers").await else {
            return;
        };
        let pool = &db.pool;
        let ctx = RequestContext::system("gh261-test", "gh261-test");
        let auditor = |view: &[Uuid]| {
            let permissions = Permissions {
                global: [GlobalPermission::AuditView].into(),
                classes: view.iter().map(|id| (*id, ClassRights { view: true, ..Default::default() })).collect(),
                ..Default::default()
            };
            let principal = crate::auth::Principal {
                user_id: Uuid::new_v4(),
                username: "auditor".into(),
                credential: crate::auth::Credential::Token,
                permissions,
            };
            RequestContext::user(std::sync::Arc::new(principal), "gh261".into())
        };

        // An unrestricted writer purges the field `code` of Secrets (12 values).
        let secrets: CiClass =
            simple::create::<CiClasses>(pool, &ctx, &body(json!({"name": "Secrets"}))).await.unwrap();
        let def: AttributeDefinition = simple::create::<AttributeDefinitions>(
            pool,
            &ctx,
            &body(json!({"classId": secrets.id, "key": "code", "label": "Code", "dataType": "text"})),
        )
        .await
        .unwrap();
        for _ in 0..12 {
            let item = body::<CreateItemBody>(json!({"classId": secrets.id, "attributes": {"code": "x"}}));
            items_service::create(pool, &ctx, &item).await.unwrap();
        }
        simple::update::<AttributeDefinitions>(pool, &ctx, def.id, &body(json!({"isActive": false}))).await.unwrap();
        let mut tx = pool.begin().await.unwrap();
        let purge = purge_attribute_in(&mut tx, &ctx, def.id, "code").await.unwrap().unwrap();
        tx.commit().await.unwrap();

        // A record from before migration 0028: a count-free variant, no count_classes.
        let mut conn = pool.acquire().await.unwrap();
        let legacy: SchemaChange = sqlx::query_as(
            r#"INSERT INTO cmdb.schema_changes (actor_type, actor_name, summary, statements, impact,
                                                redacted_summary, redacted_impact)
               VALUES ('system', 'migration 0009', 'Migration 0009: 7 attribute values moved', '{SELECT 1}',
                       '[{"statement": null, "kind": "data_moved", "rows": 7, "message": "7 values moved"}]',
                       'Migration 0009: attribute values moved',
                       '[{"statement": null, "kind": "data_moved", "rows": null, "message": "The values moved"}]')
               RETURNING id, occurred_at, actor_type, actor_id, actor_name, request_id, summary, statements, impact"#,
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        let entry = AuditEntry {
            action: AuditAction::Create,
            entity_type: "schema_changes",
            entity_id: legacy.id,
            old_value: None,
            new_value: Some(crud::json(&legacy)),
        };
        crud::write_audit(&mut conn, &ctx, vec![entry]).await.unwrap();
        drop(conn);

        for (reader, sees_purge, sees_legacy) in
            [(auditor(&[]), false, false), (auditor(&[secrets.id]), true, false), (ctx.clone(), true, true)]
        {
            let query: AuditQuery =
                body(json!({"limit": 200, "offset": 0, "sort": "-occurredAt", "entityType": "schema_changes"}));
            let entries = audit::list(pool, &reader, &query).await.unwrap().data;
            let value = |id: Uuid| {
                let e = entries.iter().find(|e| e.entity_id == id).expect("audit entry");
                assert!(!e.redacted);
                e.new_value.clone().unwrap()
            };

            let v = value(purge.id);
            // The summary and messages only: ids and statements may contain digits.
            let text = std::iter::once(&v["summary"])
                .chain(v["impact"].as_array().unwrap().iter().map(|i| &i["message"]))
                .map(|t| t.as_str().unwrap())
                .collect::<Vec<_>>()
                .join(" | ");
            assert_eq!(v["statements"], json!(purge.statements), "statements are kept");
            let column = v["impact"].as_array().unwrap().iter().find(|i| i["kind"] == "drop_column").unwrap().clone();
            if sees_purge {
                assert_eq!(column["rows"], 12, "{column}");
                assert!(text.contains("12"), "{text}");
            } else {
                assert_eq!(column["rows"], Value::Null, "{column}");
                assert!(!column["message"].as_str().unwrap().contains(char::is_numeric), "{column}");
                assert!(!text.contains("12"), "{text}");
            }

            let v = value(legacy.id);
            if sees_legacy {
                assert_eq!(v["summary"], "Migration 0009: 7 attribute values moved");
                assert_eq!(v["impact"][0]["rows"], 7);
            } else {
                assert_eq!(v["summary"], "Migration 0009: attribute values moved");
                assert_eq!(
                    v["impact"][0],
                    json!({"statement": null, "kind": "data_moved", "rows": null, "message": "The values moved"})
                );
            }
        }
        db.drop().await;
    }
}
