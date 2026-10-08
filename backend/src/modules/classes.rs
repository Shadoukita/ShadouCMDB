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
use super::impact::ImpactDirection;
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
use crate::modules::workflows::refs as workflow_refs;
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
    /// Data quality: the field (of this class or an ancestor) holding a CI's owner. A CI without a value there counts
    /// as "no owner". Null: the parent's setting applies; no setting in the lineage leaves the class out of the check
    #[schema(required = true)]
    pub owner_attribute_id: Option<Uuid>,
    /// Data quality: the date or datetime field (of this class or an ancestor) holding a CI's end of life. Null: the
    /// parent's setting applies; no setting in the lineage leaves the class out of the check
    #[schema(required = true)]
    pub end_of_life_attribute_id: Option<Uuid>,
    /// The attribute (of this class or an ancestor) whose value the UI shows under a CI's name, e.g. the model of a
    /// server; null shows the class name instead
    #[schema(required = true)]
    pub subtitle_attribute_id: Option<Uuid>,
    /// Set on the built-in type the application itself uses: `business_service` (the business services). It can be
    /// renamed and given fields, but not deleted, archived, purged, made abstract, given a parent or subtypes
    #[schema(required = true, inline)]
    pub system_role: Option<ClassSystemRole>,
    #[schema(inline)]
    pub kind: ClassKind,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
}

/// 409 IN_USE for a removal or change a built-in type (business service
/// §4.10, Person SHAA-1505) does not allow.
pub fn system_class_refused(key: &str, role: ClassSystemRole, what: &str) -> AppError {
    let message = format!("The {key} type is the {} and cannot be {what}", role.describe());
    AppError::new(ErrorCode::InUse, message.clone()).with_details(vec![FieldError {
        location: FieldLocation::Params,
        field: "id".into(),
        message,
        code: "system_class".into(),
    }])
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

fn owner_attribute_schema() -> Schema {
    let mut s = nullable_uuid_schema();
    if let Schema::AnyOf(a) = &mut s {
        a.description = Some(
            "Data quality: attribute of this class or an ancestor that holds a CI's owner (text, enum, lookup or \
             reference). CIs without a value count in the `no_owner` check. Null: the parent's setting applies."
                .into(),
        );
    }
    s
}

fn end_of_life_attribute_schema() -> Schema {
    let mut s = nullable_uuid_schema();
    if let Schema::AnyOf(a) = &mut s {
        a.description = Some(
            "Data quality: attribute of this class or an ancestor that holds a CI's end of life (date or datetime), \
             for the `end_of_life` check. Null: the parent's setting applies."
                .into(),
        );
    }
    s
}

fn subtitle_attribute_schema() -> Schema {
    let mut s = nullable_uuid_schema();
    if let Schema::AnyOf(a) = &mut s {
        a.description = Some(
            "Attribute of this class or an ancestor (any data type) whose value the UI shows under a CI's name; null \
             shows the class name. A new class takes its parent's."
                .into(),
        );
    }
    s
}

fn kind_schema(description: &str) -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(["asset", "process"]))
        .description(Some(description))
        .into()
}

fn create_kind_schema() -> Schema {
    kind_schema(
        "asset (inventory CIs) or process (records such as change requests, kept out of the inventory). Leave out to \
         take the parent's kind (asset for a root type); a type has its parent's kind.",
    )
}

fn update_kind_schema() -> Schema {
    kind_schema("Only for a type that has never held a CI (deleted ones included) and has no subtypes")
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
    #[schema(schema_with = owner_attribute_schema)]
    #[serde(default)]
    owner_attribute_id: Option<Uuid>,
    #[schema(schema_with = end_of_life_attribute_schema)]
    #[serde(default)]
    end_of_life_attribute_id: Option<Uuid>,
    #[schema(schema_with = subtitle_attribute_schema)]
    #[serde(default)]
    subtitle_attribute_id: Option<Uuid>,
    #[schema(schema_with = create_kind_schema)]
    #[serde(default)]
    kind: Option<ClassKind>,
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
    #[schema(schema_with = owner_attribute_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    owner_attribute_id: Option<Option<Uuid>>,
    #[schema(schema_with = end_of_life_attribute_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    end_of_life_attribute_id: Option<Option<Uuid>>,
    #[schema(schema_with = subtitle_attribute_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    subtitle_attribute_id: Option<Option<Uuid>>,
    #[schema(schema_with = update_kind_schema)]
    kind: Option<ClassKind>,
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
            .opt("title_attribute_id", self.title_attribute_id.map(Some))
            .opt("owner_attribute_id", self.owner_attribute_id.map(Some))
            .opt("end_of_life_attribute_id", self.end_of_life_attribute_id.map(Some))
            .opt("subtitle_attribute_id", self.subtitle_attribute_id.map(Some))
            .opt("kind", self.kind.map(|k| k.as_str().to_owned()));
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
            .opt("title_attribute_id", self.title_attribute_id)
            .opt("owner_attribute_id", self.owner_attribute_id)
            .opt("end_of_life_attribute_id", self.end_of_life_attribute_id)
            .opt("subtitle_attribute_id", self.subtitle_attribute_id)
            .opt("kind", self.kind.map(|k| k.as_str().to_owned()));
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
    /// Only asset types or only process types
    #[param(inline)]
    kind: Option<ClassKind>,
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
        if let Some(kind) = self.kind {
            w.and().push("kind = ").push_bind(kind.as_str());
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
        description, parent_id, is_abstract, icon, color, sort_order, is_active, title_attribute_id, owner_attribute_id,
        end_of_life_attribute_id, subtitle_attribute_id, system_role, kind, created_at, updated_at";
    const SEARCH_COLUMNS: &'static [&'static str] = &["key", "name", "description"];
    const ARCHIVE_ON_DELETE: bool = true;
    const WRITE_ERRORS: &'static [ErrorCode] = &[ErrorCode::InvalidName, ErrorCode::SchemaChangeRefused];
    const UPDATE_DESCRIPTION: &'static str = "Changing `titleAttributeId` relabels the class's CIs. Moving the type to another parent (`parentId`) keeps its title attribute only if the new lineage provides it; otherwise it takes the new parent's (so do its subtypes), and the CIs are relabelled. The same move clears an `ownerAttributeId` or `endOfLifeAttributeId` of the type or its subtypes that the new lineage does not provide (the parent's setting then applies). A `subtitleAttributeId` the new lineage does not provide is replaced by the new parent's, which changes no stored CI data.";
    const DELETE_DESCRIPTION: &'static str = "Archives the type (`isActive=false`): its table, CIs and values stay and stay readable, no new CIs can be created, and the UI hides it. `PATCH {\"isActive\": true}` restores it. To drop the table and delete its CIs, purge the type (`POST /api/v1/ci-classes/{id}/purge`).";
    // DELETE archives, which nothing blocks; `blocking` marks what refuses the
    // purge (the checks in `purge_class_in`). Everything else goes with the purge.
    const USAGE: &'static [Usage] = &[
        Usage {
            kind: "configurationItems",
            label: "configuration items",
            sql: "SELECT count(*) FROM configuration_items WHERE class_id = $1 AND deleted_at IS NULL",
            spans: Some(CLASS_SPANS),
            blocking: false,
        },
        Usage {
            kind: "deletedConfigurationItems",
            label: "deleted configuration items (kept for history)",
            sql: "SELECT count(*) FROM configuration_items WHERE class_id = $1 AND deleted_at IS NOT NULL",
            spans: Some(CLASS_SPANS),
            blocking: false,
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
            blocking: false,
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
            blocking: false,
        },
        Usage {
            kind: "workflowDefinitions",
            label: "workflows defined on it (delete them first)",
            sql: crate::modules::workflows::refs::CLASS_DEFINITIONS,
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

    /// The built-in business service type keeps its role, stays active and
    /// concrete, and has no parent (SHAA-927 §1.1); the database refuses the
    /// same, this answers first with the documented code.
    fn before_change(row: &CiClass, columns: Option<&ColumnSet>) -> Result<(), AppError> {
        let Some(role) = row.system_role else { return Ok(()) };
        let Some(columns) = columns else { return Err(system_class_refused(&row.key, role, "deleted")) };
        for (column, value) in &columns.0 {
            let what = match (*column, value) {
                ("is_active", Val::Bool(Some(false))) => "archived",
                ("is_abstract", Val::Bool(Some(true))) => "made abstract",
                ("parent_id", Val::Uuid(Some(_))) => "given a parent type",
                ("kind", Val::Text(Some(k))) if k != ClassKind::Asset.as_str() => "made a process type",
                _ => continue,
            };
            return Err(system_class_refused(&row.key, role, what));
        }
        Ok(())
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
            if let Some(parent) = parent
                && sqlx::query_scalar::<_, bool>(
                    "SELECT EXISTS (SELECT 1 FROM cmdb.ci_classes WHERE id = $1 AND system_role IS NOT NULL)",
                )
                .bind(parent)
                .fetch_one(&mut *conn)
                .await?
            {
                return Err(AppError::field(
                    "parentId",
                    "Built-in types (business service, Person) cannot have subtypes",
                    "system_class",
                ));
            }
            // A subtype has its parent's kind.
            if let Some(parent) = parent
                && !columns.0.iter().any(|(c, _)| *c == "kind")
            {
                let kind: Option<String> = sqlx::query_scalar("SELECT kind FROM cmdb.ci_classes WHERE id = $1")
                    .bind(parent)
                    .fetch_optional(&mut *conn)
                    .await?;
                columns.opt("kind", kind);
            }
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
            // And subtitled like it.
            if let Some(parent) = parent
                && !columns.0.iter().any(|(c, _)| *c == "subtitle_attribute_id")
            {
                let subtitle: Option<Uuid> =
                    sqlx::query_scalar("SELECT subtitle_attribute_id FROM cmdb.ci_classes WHERE id = $1")
                        .bind(parent)
                        .fetch_optional(&mut *conn)
                        .await?
                        .flatten();
                columns.opt("subtitle_attribute_id", subtitle.map(Some));
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
                if row.kind != previous.kind {
                    check_kind_change(conn, ctx, row).await?;
                }
                if row.parent_id != previous.parent_id {
                    move_to_new_parent(conn, ctx, row, previous).await?;
                    check_parent_fields_in_lineage(conn, row.id).await?;
                    repair_titles(conn, row, previous).await?;
                    clear_stale_quality_fields(conn, row).await?;
                    repair_subtitles(conn, row, previous).await?;
                }
                if row.parent_id != previous.parent_id || row.title_attribute_id != previous.title_attribute_id {
                    let model = Model::load(conn).await?;
                    items_data::refresh_labels(conn, &model, &model.subtree(row.id), None).await?;
                }
            }
            if previous.is_none_or(|p| p.kind != row.kind || p.parent_id != row.parent_id) {
                check_parent_kind(conn, row, previous.is_some_and(|p| p.parent_id != row.parent_id)).await?;
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

/// A type's kind changes only while it has never held a CI and has no
/// subtypes: process records and assets are never mixed in one table.
async fn check_kind_change(conn: &mut PgConnection, ctx: &RequestContext, row: &CiClass) -> Result<(), AppError> {
    let subtypes: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM cmdb.ci_classes WHERE parent_id = $1)")
        .bind(row.id)
        .fetch_one(&mut *conn)
        .await?;
    if subtypes {
        return Err(AppError::field(
            "kind",
            "The type has subtypes, which share its kind; change the kind before adding subtypes",
            "class_has_subtypes",
        ));
    }
    // Success or refusal tells whether the type holds CIs (GH#267).
    if !engine::may_view_all(conn, ctx, &[row.id]).await? {
        return Err(engine::view_required(
            "kind",
            "Changing the kind of a type is checked against the CIs it holds; that needs the view right on the type. \
             Nothing was changed."
                .into(),
        ));
    }
    let held: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM cmdb.configuration_items WHERE class_id = $1)")
        .bind(row.id)
        .fetch_one(&mut *conn)
        .await?;
    if held {
        return Err(AppError::field(
            "kind",
            "The type holds or has held CIs (deleted ones included); its kind cannot change",
            "class_has_items",
        ));
    }
    Ok(())
}

/// A type has its parent's kind.
async fn check_parent_kind(conn: &mut PgConnection, row: &CiClass, moved: bool) -> Result<(), AppError> {
    let Some(parent) = row.parent_id else { return Ok(()) };
    let parent_kind: Option<ClassKind> = sqlx::query_scalar("SELECT kind FROM cmdb.ci_classes WHERE id = $1")
        .bind(parent)
        .fetch_optional(&mut *conn)
        .await?;
    match parent_kind {
        Some(k) if k != row.kind => Err(AppError::field(
            if moved { "parentId" } else { "kind" },
            format!("A {} type cannot be a subtype of a {} type", row.kind.as_str(), k.as_str()),
            "kind_mismatch",
        )),
        _ => Ok(()),
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

/// After a move: the owner and end-of-life settings in the moved type's subtree
/// that name a field the new lineage does not provide are cleared, so the
/// parent's setting applies (the trigger clears the moved type's own).
async fn clear_stale_quality_fields(conn: &mut PgConnection, row: &CiClass) -> Result<(), AppError> {
    for column in ["owner_attribute_id", "end_of_life_attribute_id"] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE cmdb.ci_classes c SET {column} = NULL
             FROM cmdb.ci_attribute_definitions d
             WHERE d.id = c.{column} AND cmdb.ci_class_is_a(c.id, $1) AND NOT cmdb.ci_class_is_a(c.id, d.class_id)"
        )))
        .bind(row.id)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// A field that is a type's owner or end-of-life field keeps a data type that setting allows.
async fn check_quality_field_type(conn: &mut PgConnection, row: &AttributeDefinition) -> Result<(), AppError> {
    let (owner, end_of_life): (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE owner_attribute_id = $1), count(*) FILTER (WHERE end_of_life_attribute_id = $1)
         FROM cmdb.ci_classes WHERE owner_attribute_id = $1 OR end_of_life_attribute_id = $1",
    )
    .bind(row.id)
    .fetch_one(&mut *conn)
    .await?;
    for (count, allowed, what, code) in [
        (owner, OWNER_DATA_TYPES, "owner", "owner_attribute_type"),
        (end_of_life, END_OF_LIFE_DATA_TYPES, "end-of-life", "end_of_life_attribute_type"),
    ] {
        if count > 0 && !allowed.contains(&row.data_type) {
            return Err(engine::refused(
                "dataType",
                code,
                format!(
                    "This field is the {what} field of {count} types; a {} field cannot be. Choose another {what} \
                     field on those types first.",
                    row.data_type.as_str()
                ),
            ));
        }
    }
    Ok(())
}

/// After a move, the same for the subtitle attribute: a type whose subtitle
/// field is no longer in its lineage takes its parent's, top down.
async fn repair_subtitles(conn: &mut PgConnection, row: &CiClass, previous: &CiClass) -> Result<(), AppError> {
    let model = Model::load(conn).await?;
    let mut subtitles: std::collections::HashMap<Uuid, Option<Uuid>> =
        sqlx::query_as("SELECT id, subtitle_attribute_id FROM cmdb.ci_classes")
            .fetch_all(&mut *conn)
            .await?
            .into_iter()
            .collect();
    for class_id in model.subtree(row.id) {
        let lineage: Vec<Uuid> = model.lineage(class_id).iter().map(|c| c.id).collect();
        let current = subtitles.get(&class_id).copied().flatten();
        let fits = current.and_then(|t| model.field(t)).is_some_and(|f| lineage.contains(&f.class_id));
        let lost = if class_id == row.id {
            previous.subtitle_attribute_id.is_some() && current.is_none()
        } else {
            current.is_some()
        };
        if fits || !lost {
            continue;
        }
        let parent_subtitle =
            model.class(class_id).and_then(|c| c.parent_id).and_then(|p| subtitles.get(&p).copied().flatten());
        sqlx::query("UPDATE cmdb.ci_classes SET subtitle_attribute_id = $2 WHERE id = $1")
            .bind(class_id)
            .bind(parent_subtitle)
            .execute(&mut *conn)
            .await?;
        subtitles.insert(class_id, parent_subtitle);
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
    if let Some(role) = row.system_role {
        return Err(system_class_refused(&row.key, role, "purged"));
    }
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
        (
            sqlx::query_scalar(crate::modules::workflows::refs::CLASS_DEFINITIONS)
                .bind(id)
                .fetch_one(&mut *conn)
                .await?,
            "workflows defined on it (delete them first)",
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
    // The table is dropped empty, so the impact tells how many rows it held (GH#281).
    let rows = items_data::count_type_rows(conn, &table).await?;
    // Type rows first, so that references between these CIs are gone before
    // their registry rows are deleted (the foreign keys check at statement end).
    for c in model.lineage(id) {
        if let Some(t) = model.table(c.id) {
            items_data::delete_type_rows(conn, &t, &items).await?;
        }
    }
    // Their workflow instances and events move to the archive as the rows go
    // (migration 0050); the archive names this request, as the delete rows do.
    sqlx::query("SELECT set_config('shadoucmdb.request_id', $1, true)")
        .bind(&ctx.request_id)
        .execute(&mut *conn)
        .await?;
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
    let purge = Purge { rows: [(table.clone(), rows)].into(), tables: vec![table], classes, ..Purge::default() };
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

/// What a built-in CI class is for (`ci_classes.system_role`, migration 0033).
/// The class with a role cannot be deleted, archived or subclassed, and its
/// role never changes; its key may differ between installs (`service` when
/// the starter class was adopted, `business_service` otherwise).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum ClassSystemRole {
    BusinessService,
    /// The people sign-in accounts are linked to (migration 0044, SHAA-1505).
    Person,
}

impl ClassSystemRole {
    /// "built-in business service type", for messages.
    pub fn describe(self) -> &'static str {
        match self {
            ClassSystemRole::BusinessService => "built-in business service type",
            ClassSystemRole::Person => "built-in Person type",
        }
    }
}

/// What a built-in field is for (`ci_attribute_definitions.system_role`,
/// migration 0044): the Person's Name and Email. Such a field cannot be
/// archived, purged, made optional or change type; the Email is unique across
/// Person CIs, ignoring case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum AttributeSystemRole {
    PersonName,
    PersonEmail,
}

/// 409 IN_USE for a change the Person's Name or Email field does not allow.
fn system_attribute_refused(key: &str, what: &str) -> AppError {
    let message = format!("The {key} field is a key field of the built-in Person type and cannot be {what}");
    AppError::new(ErrorCode::InUse, message.clone()).with_details(vec![FieldError {
        location: FieldLocation::Params,
        field: "id".into(),
        message,
        code: "system_attribute".into(),
    }])
}

/// What a type's CIs are (`ci_classes.kind`, migration 0046). Asset CIs make
/// up the inventory. Process records (change requests, access reviews) live in
/// type tables like assets but stay out of the inventory list and global search
/// (unless asked for), the relationship graph, impact analysis, business
/// service membership and the dashboard counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum ClassKind {
    Asset,
    Process,
}

impl ClassKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ClassKind::Asset => "asset",
            ClassKind::Process => "process",
        }
    }
}

/// What a built-in relationship type is for (`relationship_types.system_role`,
/// migration 0033): its key, direction, impact direction and state are fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum RelationshipTypeSystemRole {
    BusinessServiceMember,
}

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

/// Data types an owner field can have (as `cmdb.owner_data_type()`).
pub const OWNER_DATA_TYPES: &[AttributeDataType] =
    &[AttributeDataType::Text, AttributeDataType::Enum, AttributeDataType::Lookup, AttributeDataType::Reference];

/// Data types an end-of-life field can have (as `cmdb.end_of_life_data_type()`).
pub const END_OF_LIFE_DATA_TYPES: &[AttributeDataType] = &[AttributeDataType::Date, AttributeDataType::Datetime];

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
    /// Counts towards completeness: a live CI without a value is incomplete. Not enforced on writes.
    pub is_expected: bool,
    /// Identifies one CI (serial number, asset tag, ...): not copied when a CI is cloned. Uniqueness is not enforced on writes.
    pub is_identifying: bool,
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
    /// Set on the key fields of the built-in Person type (`person_name`,
    /// `person_email`): they cannot be archived, purged, made optional or change
    /// type. Read-only.
    #[schema(required = true)]
    pub system_role: Option<AttributeSystemRole>,
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
    /// Counts towards completeness: a live CI without a value is incomplete. Not enforced on writes.
    pub is_expected: bool,
    /// Identifies one CI (serial number, asset tag, ...): not copied when a CI is cloned. Uniqueness is not enforced on writes.
    pub is_identifying: bool,
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
    /// `person_name` or `person_email` on the key fields of the built-in Person type. Read-only.
    #[schema(required = true)]
    pub system_role: Option<AttributeSystemRole>,
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
            is_expected: r.is_expected,
            is_identifying: r.is_identifying,
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
            system_role: r.system_role,
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
        if let Some(p) = &self.pattern {
            match validate::check_pattern(p) {
                Ok(()) => {}
                Err(validate::PatternError::Syntax) => {
                    errors.push(custom("validation.pattern", "Not a valid regular expression"))
                }
                Err(validate::PatternError::TooBig) => errors.push(FieldError {
                    code: "invalid_format".into(),
                    ..custom(
                        "validation.pattern",
                        format!(
                            "Regular expression is too complex: it compiles to more than {} KiB. \
                             Reduce bounded repetitions such as \\w{{200}}",
                            validate::PATTERN_SIZE_LIMIT / 1024
                        ),
                    )
                }),
            }
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
    schemas::multiline_text_schema(2000)
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
        // Line breaks are up to the attribute: its value rules check the default.
        .extensions(Some(schemas::multiline_extension()))
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
    /// Counts towards completeness: a live CI without a value is incomplete. Not enforced on writes.
    #[schema(nullable = false)]
    is_expected: Option<bool>,
    /// Identifies one CI (serial number, asset tag, ...): not copied when a CI is cloned. Uniqueness is not enforced on writes.
    #[schema(nullable = false)]
    is_identifying: Option<bool>,
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
    /// Counts towards completeness: a live CI without a value is incomplete. Not enforced on writes.
    #[schema(nullable = false)]
    is_expected: Option<bool>,
    /// Identifies one CI (serial number, asset tag, ...): not copied when a CI is cloned. Uniqueness is not enforced on writes.
    #[schema(nullable = false)]
    is_identifying: Option<bool>,
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
            .opt("is_expected", self.is_expected)
            .opt("is_identifying", self.is_identifying)
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
            .opt("is_expected", self.is_expected)
            .opt("is_identifying", self.is_identifying)
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
/// A retype to or from reference or lookup is refused against the stored type
/// in `before_change` (422 `type_change_unsupported`, GH#782), so restating
/// the current type is accepted.
impl Check for AttributeDefinitionUpdate {
    fn check(&self) -> Vec<FieldError> {
        non_empty(&self.columns())
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
    #[param(inline)]
    is_expected: Option<QueryBool>,
    #[param(inline)]
    is_identifying: Option<QueryBool>,
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
        bool_filter(w, "is_expected", self.is_expected);
        bool_filter(w, "is_identifying", self.is_identifying);
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
    const COLUMNS: &'static str = "id, class_id, key, label, description, data_type, is_required, is_expected, is_identifying, enum_values, reference_class_id, lookup_list_id, validation, group_name, help_text, default_value, sort_order, is_active, system_role, created_at, updated_at, parent_attribute_id";
    const SEARCH_COLUMNS: &'static [&'static str] = &["key", "label", "description", "group_name"];
    const UPDATE_DESCRIPTION: &'static str = "`dataType` changes the column type: every stored value is converted in a dry run first, and the change is refused (422 SCHEMA_CHANGE_REFUSED, naming values that fail) if any would not convert (`type_change_failed`) or would lose information (`type_change_lossy`: datetime to date keeps the UTC day, so it is refused while any value has a time of day other than midnight UTC). Only between text, number, integer, boolean, enum, date, datetime, ip and cidr: a reference or lookup field keeps its type (422 SCHEMA_CHANGE_REFUSED, `type_change_unsupported`), and no field can become one. `enumValues` is cleared when leaving enum. `isRequired: true` makes the column NOT NULL and is refused while an asset (deleted ones included) has no value. Removing enum values still stored is refused. `parentAttributeId` (lookup fields on a list with a parent list) names the field bound to the parent list, on this class or an ancestor; CI writes then only accept a value that belongs to the CI's value of that field. Preview any change with `POST /api/v1/schema-changes/preview`.";
    const ARCHIVE_ON_DELETE: bool = true;
    // IN_USE: archiving or retyping a field a workflow depends on (SHAA-1423).
    const WRITE_ERRORS: &'static [ErrorCode] =
        &[ErrorCode::InvalidName, ErrorCode::SchemaChangeRefused, ErrorCode::InUse];
    const DELETE_DESCRIPTION: &'static str = "Archives the field (`isActive=false`): its column and stored values stay readable, no new values are accepted, and forms hide it. `PATCH {\"isActive\": true}` restores it. To drop the column and its values, purge the field (`POST /api/v1/attribute-definitions/{id}/purge`). A field a workflow depends on (its state field, or a field a published version uses) is not archived: 409 IN_USE names the workflows.";
    // DELETE archives, which nothing blocks; `blocking` marks what refuses the
    // purge (the checks in `purge_attribute_in`). The values go with the purge.
    const USAGE: &'static [Usage] = &[
        Usage {
            kind: "attributeValues",
            label: "values stored on configuration items",
            sql: "SELECT cmdb.attribute_value_count($1)",
            spans: Some(ATTRIBUTE_SPANS),
            blocking: false,
        },
        Usage {
            kind: "workflows",
            label: "workflow versions or workflows using it (as a transition field, in a condition or as state field)",
            sql: "SELECT count(*) FROM (SELECT version_id FROM cmdb.workflow_version_attribute_refs WHERE attribute_id = $1
                  UNION SELECT t.version_id FROM cmdb.workflow_transition_fields f
                        JOIN cmdb.workflow_transitions t ON t.id = f.transition_id WHERE f.attribute_id = $1
                  UNION SELECT id FROM cmdb.workflow_definitions WHERE state_attribute_id = $1) u",
            spans: None,
            blocking: true,
        },
        Usage {
            kind: "dependentFields",
            label: "fields using it as their parent field (unlink them first)",
            sql: "SELECT count(*) FROM ci_attribute_definitions WHERE parent_attribute_id = $1",
            spans: None,
            blocking: true,
        },
    ];

    fn id(row: &AttributeDefinition) -> Uuid {
        row.id
    }

    fn validate(columns: &ColumnSet, create: bool) -> Result<(), AppError> {
        match engine::key_column(columns) {
            Some(key) if create => engine::validate_name(key, NameKind::Field, "key"),
            _ => Ok(()),
        }
    }

    /// The Person's Name and Email stay active, required and of their type
    /// (SHAA-1505 decision 1), and no field changes type from or to reference
    /// or lookup (GH#765). The database refuses both; this answers first with the
    /// documented code, ahead of the UPDATE and the workflow check.
    fn before_change(row: &AttributeDefinition, columns: Option<&ColumnSet>) -> Result<(), AppError> {
        let retyped_to = columns.and_then(|c| {
            c.0.iter().find_map(|(column, value)| match (*column, value) {
                ("data_type", Val::Text(Some(t))) if t != row.data_type.as_str() => Some(t.as_str()),
                _ => None,
            })
        });
        if row.system_role.is_none() {
            if let Some(to) = retyped_to {
                let message = if matches!(row.data_type, AttributeDataType::Reference | AttributeDataType::Lookup) {
                    format!("A {} field cannot change type; add a new field instead", row.data_type.as_str())
                } else if matches!(to, "reference" | "lookup") {
                    format!("A field cannot become a {to} field; add a new field instead")
                } else {
                    return Ok(());
                };
                return Err(engine::refused("dataType", "type_change_unsupported", message));
            }
            return Ok(());
        }
        let Some(columns) = columns else { return Err(system_attribute_refused(&row.key, "archived")) };
        for (column, value) in &columns.0 {
            let what = match (*column, value) {
                ("is_active", Val::Bool(Some(false))) => "archived",
                ("is_required", Val::Bool(Some(false))) => "made optional",
                ("data_type", Val::Text(Some(t))) if t != row.data_type.as_str() => "given another data type",
                _ => continue,
            };
            return Err(system_attribute_refused(&row.key, what));
        }
        Ok(())
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
            // A field a running workflow depends on keeps its type and stays active (SHAA-1423).
            if let Some(p) = previous {
                let what = if p.is_active && !row.is_active {
                    Some("archived")
                } else if p.data_type != row.data_type {
                    Some("given another data type")
                } else if p.lookup_list_id != row.lookup_list_id || p.reference_class_id != row.reference_class_id {
                    Some("pointed at another list or type")
                } else if p.enum_values.as_ref().is_some_and(|old| {
                    old.0.iter().any(|v| !row.enum_values.as_ref().is_some_and(|new| new.0.contains(v)))
                }) {
                    Some("stripped of enum values")
                } else {
                    None
                };
                if let Some(what) = what {
                    workflow_refs::check_attribute(conn, ctx, row.id, &row.key, workflow_refs::Reach::Live, what)
                        .await?;
                }
            }
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
            check_quality_field_type(conn, row).await?;
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
    workflow_refs::check_attribute(conn, ctx, id, &row.key, workflow_refs::Reach::All, "purged").await?;
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
    #[schema(inline)]
    pub impact_direction: ImpactDirection,
    /// Group heading the UI lists the type's relationships under, e.g. "Location"; types with the same category form
    /// one group, null: no group
    #[schema(required = true)]
    pub category: Option<String>,
    pub sort_order: i32,
    pub is_active: bool,
    /// Set on the built-in type the application itself uses: `business_service_member` (a business service includes
    /// a CI). Its name and labels can change; its key, direction, impact direction and active flag cannot, it cannot
    /// be deleted, and its relationships are managed on the business service (`/api/v1/business-services`)
    #[schema(required = true, inline)]
    pub system_role: Option<RelationshipTypeSystemRole>,
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
    #[schema(nullable = false, inline)]
    impact_direction: Option<ImpactDirection>,
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
    #[schema(schema_with = category_schema)]
    #[serde(default)]
    category: Option<String>,
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
    #[schema(schema_with = category_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    category: Option<Option<String>>,
    #[schema(schema_with = sort_order_schema)]
    sort_order: Option<i32>,
    #[schema(nullable = false)]
    is_active: Option<bool>,
    #[schema(nullable = false, inline)]
    impact_direction: Option<ImpactDirection>,
}

impl Writable for RelationshipTypeCreate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("key", Some(self.key.clone()))
            .opt("is_directional", self.is_directional)
            .opt("impact_direction", self.impact_direction.map(|d| d.as_str().to_owned()))
            .opt("name", Some(self.name.clone()))
            .opt("description", self.description.clone().map(Some))
            .opt("forward_label", Some(self.forward_label.clone()))
            .opt("reverse_label", Some(self.reverse_label.clone()))
            .opt("category", self.category.as_deref().map(category_value))
            .opt("sort_order", self.sort_order)
            .opt("is_active", self.is_active);
        c
    }
}
impl Check for RelationshipTypeCreate {
    fn check(&self) -> Vec<FieldError> {
        let mut errors = check_category(self.category.as_deref());
        errors.extend(match (self.is_directional, self.impact_direction) {
            (Some(false), Some(d)) if !d.allowed_without_direction() => vec![FieldError {
                location: FieldLocation::Body,
                field: "impactDirection".into(),
                message: "A non-directional type has no source or target side: impact can only flow both ways or \
                          not at all"
                    .into(),
                code: "invalid".into(),
            }],
            _ => Vec::new(),
        });
        errors
    }
}

impl Writable for RelationshipTypeUpdate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("name", self.name.clone())
            .opt("description", self.description.clone())
            .opt("forward_label", self.forward_label.clone())
            .opt("reverse_label", self.reverse_label.clone())
            .opt("category", self.category.as_ref().map(|c| c.as_deref().and_then(category_value)))
            .opt("sort_order", self.sort_order)
            .opt("is_active", self.is_active)
            .opt("impact_direction", self.impact_direction.map(|d| d.as_str().to_owned()));
        c
    }
}
impl Check for RelationshipTypeUpdate {
    fn check(&self) -> Vec<FieldError> {
        let mut errors = non_empty(&self.columns());
        errors.extend(check_category(self.category.clone().flatten().as_deref()));
        errors
    }
}

fn category_schema() -> Schema {
    let mut s = schemas::nullable_string_schema(CATEGORY_MAX);
    if let Schema::AnyOf(a) = &mut s {
        a.description = Some(
            "Group heading for the type's relationships on the CI page, e.g. \"Location\" or \"Network & power\"; \
             types with the same text form one group. Trimmed; null or an empty text: no group."
                .into(),
        );
    }
    s
}

pub(crate) const CATEGORY_MAX: usize = 100;

/// A category as stored: trimmed, and none when nothing is left.
pub(crate) fn category_value(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn check_category(raw: Option<&str>) -> Vec<FieldError> {
    match raw {
        Some(c) if c.trim().chars().count() > CATEGORY_MAX => {
            vec![custom("category", format!("At most {CATEGORY_MAX} characters"))]
        }
        _ => Vec::new(),
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
    const COLUMNS: &'static str = "id, key, name, description, forward_label, reverse_label, is_directional, impact_direction, category, sort_order, is_active, system_role, created_at, updated_at";
    const SEARCH_COLUMNS: &'static [&'static str] = &["key", "name", "forward_label", "reverse_label", "category"];
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

    /// The built-in member type keeps its key, direction, impact direction and
    /// active flag and cannot be deleted (SHAA-927 §4.10).
    fn before_change(row: &RelationshipType, columns: Option<&ColumnSet>) -> Result<(), AppError> {
        if row.system_role.is_none() {
            return Ok(());
        }
        let Some(columns) = columns else {
            let message = format!(
                "The {} relationship type is the built-in business service membership and cannot be deleted",
                row.key
            );
            return Err(AppError::new(ErrorCode::InUse, message.clone()).with_details(vec![FieldError {
                location: FieldLocation::Params,
                field: "id".into(),
                message,
                code: "system_relationship_type".into(),
            }]));
        };
        let fixed: Vec<FieldError> = columns
            .0
            .iter()
            .filter_map(|(column, value)| {
                let field = match (*column, value) {
                    ("impact_direction", Val::Text(Some(d))) if d != row.impact_direction.as_str() => "impactDirection",
                    ("is_active", Val::Bool(Some(a))) if *a != row.is_active => "isActive",
                    ("is_directional", Val::Bool(Some(d))) if *d != row.is_directional => "isDirectional",
                    ("key", Val::Text(Some(k))) if *k != row.key => "key",
                    _ => return None,
                };
                Some(FieldError {
                    location: FieldLocation::Body,
                    field: field.into(),
                    message: "Fixed on the built-in business service membership type; only its name, description \
                              and labels can change"
                        .into(),
                    code: "system_relationship_type".into(),
                })
            })
            .collect();
        if fixed.is_empty() { Ok(()) } else { Err(AppError::validation(fixed)) }
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

    /// Any class may be a business service member, and members change only
    /// through the service, so the member type takes no rules (GH#410; the
    /// trigger of migration 0041 is the backstop).
    fn after_write<'a>(
        conn: &'a mut PgConnection,
        _ctx: &'a RequestContext,
        row: &'a RelationshipRule,
        _previous: Option<&'a RelationshipRule>,
    ) -> BoxFuture<'a, Result<(), AppError>> {
        Box::pin(async move {
            let system: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM cmdb.relationship_types WHERE id = $1 AND system_role IS NOT NULL)",
            )
            .bind(row.relationship_type_id)
            .fetch_one(&mut *conn)
            .await?;
            if system {
                return Err(AppError::field(
                    "relationshipTypeId",
                    "The built-in business service membership type takes no rules: any CI can be a member, added on \
                     the business service",
                    "system_relationship_type",
                ));
            }
            Ok(())
        })
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

    /// SHAA-2357 (G8): the subtitle attribute is a field of the class or an
    /// ancestor; a new subtype takes its parent's, and a move to a parent that
    /// does not provide it gives the moved type the new parent's.
    #[tokio::test]
    async fn subtitle_attribute_follows_the_lineage() {
        let Some(db) = scratch::database("subtitle_attribute_follows_the_lineage").await else { return };
        let pool = &db.pool;
        fn ctx_for() -> RequestContext {
            RequestContext::system("subtitle-test", "subtitle-test")
        }
        let ctx = ctx_for();
        let class = |b: Value| async move { simple::create::<CiClasses>(pool, &ctx_for(), &body(b)).await.unwrap() };
        let field = |class_id: Uuid, key: &'static str| async move {
            simple::create::<AttributeDefinitions>(
                pool,
                &ctx_for(),
                &body(json!({"classId": class_id, "key": key, "label": key, "dataType": "text"})),
            )
            .await
            .unwrap()
        };

        let hardware: CiClass = class(json!({"name": "Subtitled Hardware"})).await;
        let model: AttributeDefinition = field(hardware.id, "model").await;
        let other: CiClass = class(json!({"name": "Subtitled Other"})).await;
        let kind: AttributeDefinition = field(other.id, "kind").await;

        let hardware: CiClass =
            simple::update::<CiClasses>(pool, &ctx, hardware.id, &body(json!({"subtitleAttributeId": model.id})))
                .await
                .unwrap();
        assert_eq!(hardware.subtitle_attribute_id, Some(model.id));
        // A new subtype takes its parent's.
        let server: CiClass = class(json!({"name": "Subtitled Server", "parentId": hardware.id})).await;
        assert_eq!(server.subtitle_attribute_id, Some(model.id));

        // A field outside the lineage is refused on subtitleAttributeId.
        let err = simple::update::<CiClasses>(pool, &ctx, server.id, &body(json!({"subtitleAttributeId": kind.id})))
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ValidationError, "{err:?}");
        assert!(format!("{err:?}").contains("subtitleAttributeId"), "{err:?}");
        let err =
            simple::update::<CiClasses>(pool, &ctx, server.id, &body(json!({"subtitleAttributeId": Uuid::new_v4()})))
                .await
                .unwrap_err();
        assert!(format!("{err:?}").contains("subtitleAttributeId"), "{err:?}");

        // Moved under a parent without the field: the new parent's, and it is audited.
        simple::update::<CiClasses>(pool, &ctx, other.id, &body(json!({"subtitleAttributeId": kind.id})))
            .await
            .unwrap();
        let moved: CiClass =
            simple::update::<CiClasses>(pool, &ctx, server.id, &body(json!({"parentId": other.id}))).await.unwrap();
        assert_eq!(moved.subtitle_attribute_id, Some(kind.id));
        let stored: Option<Uuid> = sqlx::query_scalar("SELECT subtitle_attribute_id FROM ci_classes WHERE id = $1")
            .bind(server.id)
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(stored, Some(kind.id));
        let audited: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_log WHERE entity_type = 'ci_classes' AND entity_id = $1
               AND new_value ? 'subtitleAttributeId'",
        )
        .bind(hardware.id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert!(audited > 0);

        // Cleared with null; deleting the field clears it too.
        let cleared: CiClass =
            simple::update::<CiClasses>(pool, &ctx, moved.id, &body(json!({"subtitleAttributeId": null})))
                .await
                .unwrap();
        assert_eq!(cleared.subtitle_attribute_id, None);
    }

    /// SHAA-2357 (G15): a relationship type's category is optional, trimmed,
    /// at most 100 characters, and travels with every relationship.
    #[tokio::test]
    async fn relationship_type_category_is_optional_and_trimmed() {
        use crate::api::route::Check;
        let Some(db) = scratch::database("relationship_type_category_is_optional_and_trimmed").await else { return };
        let pool = &db.pool;
        let ctx = RequestContext::system("category-test", "category-test");

        let create = |category: Value| {
            body::<RelationshipTypeCreate>(
                json!({"key": "powered_by", "name": "Powered by", "forwardLabel": "is powered by",
                "reverseLabel": "powers", "category": category}),
            )
        };
        let long = "x".repeat(CATEGORY_MAX + 1);
        assert_eq!(create(json!(long)).check().len(), 1);
        assert!(create(json!("  Network & power ")).check().is_empty());
        let powered: RelationshipType =
            simple::create::<RelationshipTypes>(pool, &ctx, &create(json!("  Network & power "))).await.unwrap();
        assert_eq!(powered.category.as_deref(), Some("Network & power"));

        let update = |v: Value| body::<RelationshipTypeUpdate>(json!({ "category": v }));
        assert_eq!(update(json!(long)).check().len(), 1);
        let blank: RelationshipType =
            simple::update::<RelationshipTypes>(pool, &ctx, powered.id, &update(json!("   "))).await.unwrap();
        assert_eq!(blank.category, None);
        let named: RelationshipType =
            simple::update::<RelationshipTypes>(pool, &ctx, powered.id, &update(json!("Power"))).await.unwrap();
        assert_eq!(named.category.as_deref(), Some("Power"));
        // Left out: unchanged.
        let renamed: RelationshipType =
            simple::update::<RelationshipTypes>(pool, &ctx, powered.id, &body(json!({"name": "Fed by"})))
                .await
                .unwrap();
        assert_eq!(renamed.category.as_deref(), Some("Power"));
        // The database refuses what the API would not store.
        let err = sqlx::query("UPDATE relationship_types SET category = ' Power' WHERE id = $1")
            .bind(powered.id)
            .execute(pool)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("relationship_types_category_valid"), "{err}");

        // Relationships and the graph carry it.
        let box_class: CiClass =
            simple::create::<CiClasses>(pool, &ctx, &body(json!({"name": "Category Box"}))).await.unwrap();
        sqlx::query("INSERT INTO relationship_type_rules (relationship_type_id, source_class_id, target_class_id) VALUES ($1, $2, $2)")
            .bind(powered.id)
            .bind(box_class.id)
            .execute(pool)
            .await
            .unwrap();
        let item = || body::<CreateItemBody>(json!({"classId": box_class.id}));
        let a = items_service::create(pool, &ctx, &item()).await.unwrap().summary.id;
        let b = items_service::create(pool, &ctx, &item()).await.unwrap().summary.id;
        let rel = relationships::create(
            pool,
            &ctx,
            &body::<RelationshipCreate>(json!({"relationshipTypeId": powered.id, "sourceCiId": a, "targetCiId": b})),
        )
        .await
        .unwrap();
        assert_eq!(serde_json::to_value(&rel).unwrap()["type"]["category"], json!("Power"));
        let graph =
            items_service::graph(pool, &ctx, a, &body(json!({"depth": 1, "direction": "both", "maxNodes": 10})))
                .await
                .unwrap();
        let graph = serde_json::to_value(&graph).unwrap();
        assert_eq!(graph["edges"][0]["type"]["category"], json!("Power"), "{graph}");
    }

    /// SHAA-2357 (G16): a CI's last change is its newest create, update,
    /// delete or restore audit entry; the actor is shown only to callers with
    /// `audit.view`, and a list does not carry it.
    #[tokio::test]
    async fn last_change_is_the_newest_audited_change() {
        let Some(db) = scratch::database("last_change_is_the_newest_audited_change").await else { return };
        let pool = &db.pool;
        let ctx = RequestContext::system("last-change-test", "last-change-test");
        let class: CiClass =
            simple::create::<CiClasses>(pool, &ctx, &body(json!({"name": "Changed Box"}))).await.unwrap();
        simple::create::<AttributeDefinitions>(
            pool,
            &ctx,
            &body(json!({"classId": class.id, "key": "rack", "label": "Rack", "dataType": "text"})),
        )
        .await
        .unwrap();
        let created = items_service::create(pool, &ctx, &body(json!({"classId": class.id}))).await.unwrap();
        let id = created.summary.id;
        assert!(serde_json::to_value(&created).unwrap().get("lastChange").is_none());
        let newest = || async move {
            sqlx::query_as::<_, (DateTime<Utc>, String, String, Option<String>, Option<String>)>(
                "SELECT occurred_at, action, actor_type, actor_id, actor_name FROM audit_log
                 WHERE entity_type = 'configuration_items' AND entity_id = $1 ORDER BY id DESC LIMIT 1",
            )
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
        };
        let last = |item: &crate::modules::items::schemas::ConfigurationItem| {
            serde_json::to_value(item).unwrap()["lastChange"].clone()
        };

        let item = items_service::get(pool, &ctx, id).await.unwrap();
        let (at, action, actor_type, _, actor_name) = newest().await;
        assert_eq!(action, "create");
        let lc = last(&item);
        assert_eq!(lc["action"], json!("create"));
        assert_eq!(lc["actor"]["type"], json!(actor_type));
        assert_eq!(lc["actor"]["name"], json!(actor_name));
        assert_eq!(lc["at"], json!(crate::api::schemas::iso(&at)));

        // Another user's update replaces it.
        let alice = Uuid::new_v4();
        let as_alice = ctx.acting_as_user(alice, "alice");
        items_service::update(pool, &as_alice, id, &body(json!({"attributes": {"rack": "R1"}}))).await.unwrap();
        let lc = last(&items_service::get(pool, &ctx, id).await.unwrap());
        assert_eq!(lc["action"], json!("update"));
        assert_eq!(lc["actor"], json!({"type": "user", "id": alice.to_string(), "name": "alice"}));
        let (at, action, ..) = newest().await;
        assert_eq!(action, "update");
        assert_eq!(lc["at"], json!(crate::api::schemas::iso(&at)));

        // An entry that is not a change (an export) does not count.
        sqlx::query(
            "INSERT INTO audit_log (actor_type, actor_name, action, entity_type, entity_id, new_value)
             VALUES ('system', 'exporter', 'export', 'configuration_items', $1, '{}')",
        )
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
        let lc = last(&items_service::get(pool, &ctx, id).await.unwrap());
        assert_eq!(lc["actor"]["name"], json!("alice"));

        // Who changed it is audit data: without audit.view the actor is null.
        let viewer = crate::api::context::datamodel_manager(&[class.id]);
        let lc = last(&items_service::get(pool, &viewer, id).await.unwrap());
        assert_eq!(lc["action"], json!("update"));
        assert_eq!(lc["actor"], Value::Null);
    }

    /// GH#412: a pattern that compiles past the size limit is refused as
    /// `invalid_format`; a syntax error stays a custom error.
    #[test]
    fn oversized_validation_patterns_are_refused() {
        let class_id = Uuid::nil();
        let errors = |pattern: &str| {
            body::<AttributeDefinitionCreate>(json!({"classId": class_id, "label": "Serial", "dataType": "text",
                "validation": {"pattern": pattern}}))
            .check()
            .into_iter()
            .map(|e| (e.field, e.code))
            .collect::<Vec<_>>()
        };
        assert_eq!(errors(r"\w{900}"), [("validation.pattern".to_owned(), "invalid_format".to_owned())]);
        assert_eq!(errors(r"\w{200}x1"), [("validation.pattern".to_owned(), "invalid_format".to_owned())]);
        assert_eq!(errors("(x"), [("validation.pattern".to_owned(), "custom".to_owned())]);
        assert_eq!(errors(r"^[A-Z]{2}-\d{4}-[A-Z0-9]{8}$"), []);
    }

    /// GH#765: a lookup or reference field keeps its type. The retype is
    /// refused with `type_change_unsupported` before the UPDATE, not with the
    /// database's check-constraint text.
    #[tokio::test]
    async fn lookup_and_reference_fields_refuse_a_type_change() {
        let Some(db) = scratch::database("lookup_and_reference_fields_refuse_a_type_change").await else { return };
        let pool = &db.pool;
        let ctx = RequestContext::system("retype-test", "retype-test");

        let class: CiClass = simple::create::<CiClasses>(pool, &ctx, &body(json!({"name": "Tiered"}))).await.unwrap();
        let tiers = simple::create::<crate::modules::lookups::LookupLists>(
            pool,
            &ctx,
            &body(json!({"key": "tier", "name": "Tier"})),
        )
        .await
        .unwrap();
        let lookup: AttributeDefinition = simple::create::<AttributeDefinitions>(
            pool,
            &ctx,
            &body(json!({"classId": class.id, "key": "tier_lk", "label": "Tier", "dataType": "lookup",
                         "lookupListId": tiers.id})),
        )
        .await
        .unwrap();
        let reference: AttributeDefinition = simple::create::<AttributeDefinitions>(
            pool,
            &ctx,
            &body(json!({"classId": class.id, "key": "peer", "label": "Peer", "dataType": "reference",
                         "referenceClassId": class.id})),
        )
        .await
        .unwrap();
        let text: AttributeDefinition = simple::create::<AttributeDefinitions>(
            pool,
            &ctx,
            &body(json!({"classId": class.id, "key": "notes", "label": "Notes", "dataType": "text"})),
        )
        .await
        .unwrap();

        for (field, change) in [
            (&lookup, json!({"dataType": "text"})),
            (&lookup, json!({"dataType": "enum", "enumValues": ["a"]})),
            (&reference, json!({"dataType": "text"})),
            (&text, json!({"dataType": "lookup"})),
            (&text, json!({"dataType": "reference"})),
        ] {
            let err =
                simple::update::<AttributeDefinitions>(pool, &ctx, field.id, &body(change.clone())).await.unwrap_err();
            assert_eq!(err.code, ErrorCode::SchemaChangeRefused, "{change}: {err:?}");
            let detail = &err.details.as_ref().expect("details")[0];
            assert_eq!((detail.field.as_str(), detail.code.as_str()), ("dataType", "type_change_unsupported"));
            assert!(!err.message.contains("constraint"), "{}", err.message);
        }
        // Restating the type, or changing anything else, still works.
        simple::update::<AttributeDefinitions>(
            pool,
            &ctx,
            lookup.id,
            &body(json!({"dataType": "lookup", "label": "Tier level"})),
        )
        .await
        .unwrap();
        db.drop().await;
    }

    /// SHAA-2553 (#772): through the API, retyping a lookup or reference field
    /// is the documented 422 SCHEMA_CHANGE_REFUSED with `type_change_unsupported`
    /// on `dataType`, and the schema-change preview answers the same. A field
    /// cannot become a lookup or reference field either, with the same answer
    /// (GH#782), and restating the current type is not a change.
    /// Either way the field keeps its type.
    #[tokio::test]
    async fn the_api_refuses_a_lookup_retype_with_its_documented_code() {
        use crate::modules::api_tokens::tests::{Creds, app, call, code, session_of};
        let Some(db) = scratch::database("the_api_refuses_a_lookup_retype").await else { return };
        let app = app(db.pool.clone());
        let password = format!("test passphrase {}", Uuid::new_v4());
        let setup = json!({ "username": "admin", "email": "admin@example.test", "displayName": "Admin",
            "password": password, "setupToken": crate::auth::setup_token::TEST_TOKEN });
        let (status, me, headers) = call(&app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
        assert_eq!(status, 201, "{me}");
        let admin = session_of(&me, &headers);
        let post = async |path: &str, body: Value| {
            let (status, v, _) = call(&app, "POST", path, &admin, Some(body)).await;
            assert_eq!(status, 201, "{path}: {v}");
            v["id"].as_str().unwrap().to_owned()
        };
        let class = post("/api/v1/ci-classes", json!({ "key": "tiered", "name": "Tiered" })).await;
        let list = post("/api/v1/lookup-lists", json!({ "key": "tier", "name": "Tier" })).await;
        let lookup = post(
            "/api/v1/attribute-definitions",
            json!({ "classId": class, "key": "tier_lk", "label": "Tier", "dataType": "lookup", "lookupListId": list }),
        )
        .await;
        let text = post(
            "/api/v1/attribute-definitions",
            json!({ "classId": class, "key": "notes", "label": "Notes", "dataType": "text" }),
        )
        .await;
        let refused = |v: &Value| {
            let d = &v["error"]["details"][0];
            (code(v).to_owned(), d["field"].as_str().map(str::to_owned), d["code"].as_str().map(str::to_owned))
        };
        let expected = (
            "SCHEMA_CHANGE_REFUSED".to_owned(),
            Some("dataType".to_owned()),
            Some("type_change_unsupported".to_owned()),
        );

        for (field, to) in
            [(&lookup, "text"), (&lookup, "integer"), (&lookup, "reference"), (&text, "lookup"), (&text, "reference")]
        {
            let url = format!("/api/v1/attribute-definitions/{field}");
            let (status, v, _) = call(&app, "PATCH", &url, &admin, Some(json!({ "dataType": to }))).await;
            assert_eq!((status, refused(&v)), (422, expected.clone()), "PATCH {field} to {to}: {v}");
            let preview = json!({ "operation": "updateField", "id": field, "body": { "dataType": to } });
            let (status, v, _) = call(&app, "POST", "/api/v1/schema-changes/preview", &admin, Some(preview)).await;
            assert_eq!((status, refused(&v)), (422, expected.clone()), "preview {field} to {to}: {v}");
        }
        // Restating the current type alongside another change is accepted.
        let url = format!("/api/v1/attribute-definitions/{lookup}");
        let restate = json!({ "dataType": "lookup", "label": "Tier level" });
        let (status, v, _) = call(&app, "PATCH", &url, &admin, Some(restate)).await;
        assert_eq!((status, v["label"].as_str()), (200, Some("Tier level")), "{v}");
        for (field, ty) in [(&lookup, "lookup"), (&text, "text")] {
            let (status, v, _) =
                call(&app, "GET", &format!("/api/v1/attribute-definitions/{field}"), &admin, None).await;
            assert_eq!((status, v["dataType"].as_str()), (200, Some(ty)), "{v}");
        }
        db.drop().await;
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

    /// GH#289: TAB, line breaks and bidi controls only in multiline text
    /// attributes; control characters nowhere. Checked on write only: a stored
    /// value does not block changing another attribute.
    #[tokio::test]
    async fn control_characters_follow_the_multiline_flag() {
        use crate::api::route::{Body, BodyInput};
        use crate::modules::items::schemas::UpdateItemBody;
        let Some(db) = scratch::database("control_characters_follow_the_multiline_flag").await else { return };
        let pool = &db.pool;
        let ctx = RequestContext::system("characters-test", "characters-test");
        let class: CiClass = simple::create::<CiClasses>(pool, &ctx, &body(json!({"name": "Char Box"}))).await.unwrap();
        let remarks: AttributeDefinition = simple::create::<AttributeDefinitions>(
            pool,
            &ctx,
            &body(json!({"classId": class.id, "key": "remarks", "label": "Remarks", "dataType": "text",
                         "validation": {"multiline": true}})),
        )
        .await
        .unwrap();
        simple::create::<AttributeDefinitions>(
            pool,
            &ctx,
            &body(json!({"classId": class.id, "key": "tag", "label": "Tag", "dataType": "text"})),
        )
        .await
        .unwrap();
        let codes = |err: AppError| -> Vec<(String, String)> {
            err.details.into_iter().flatten().map(|d| (d.field, d.code)).collect()
        };
        let refused = |field: &str| vec![(format!("attributes.{field}"), "invalid_character".to_owned())];
        let ctx_ref = &ctx;
        let create = |attributes: Value| {
            let body = Body::<CreateItemBody>::parse(Some(json!({"classId": class.id, "attributes": attributes})));
            async move { items_service::create(pool, ctx_ref, &body?.0).await }
        };

        // Multiline: line breaks, tabs and bidi controls (RTL notes) are kept.
        let notes = "Line 1\r\n\tLine 2\u{2028}\u{2067}עברית\u{2069}";
        let id = create(json!({"remarks": notes, "tag": "rack-7"})).await.unwrap().summary.id;
        let stored = items_service::get(pool, &ctx, id).await.unwrap();
        assert_eq!(serde_json::to_value(&stored).unwrap()["attributes"]["remarks"], json!(notes));
        // Single-line: refused, not stripped.
        for tag in ["a\nb", "a\tb", "\u{202E}gpj.exe", "a\u{2029}b"] {
            assert_eq!(codes(create(json!({"tag": tag})).await.unwrap_err()), refused("tag"), "{tag:?}");
        }
        // Control characters: refused in both, before the value rules.
        for (field, value) in [("remarks", "\u{1B}[2J"), ("tag", "a\u{7F}"), ("remarks", "\u{9B}31m")] {
            assert_eq!(codes(create(json!({field: value})).await.unwrap_err()), refused(field), "{value:?}");
        }
        // A default is checked against its own attribute.
        let parsed = Body::<AttributeDefinitionCreate>::parse(Some(json!({"classId": class.id, "label": "Motd",
            "dataType": "text", "defaultValue": "a\nb"})));
        assert!(parsed.is_ok(), "{:?}", parsed.err());
        let err = simple::create::<AttributeDefinitions>(
            pool,
            &ctx,
            &body(json!({"classId": class.id, "label": "Motd", "dataType": "text", "defaultValue": "a\nb"})),
        )
        .await
        .unwrap_err();
        let detail = &err.details.as_ref().unwrap()[0];
        assert_eq!((detail.field.as_str(), detail.code.as_str()), ("defaultValue", "invalid"));
        assert!(detail.message.ends_with("line break (U+000A) in a single-line field"), "{}", detail.message);

        // Write only: after remarks become single-line, the stored value stays
        // and another attribute can still change; resending it is refused.
        simple::update::<AttributeDefinitions>(pool, &ctx, remarks.id, &body(json!({"validation": null})))
            .await
            .unwrap();
        let update = Body::<UpdateItemBody>::parse(Some(json!({"attributes": {"tag": "rack-8"}}))).unwrap().0;
        items_service::update(pool, &ctx, id, &update).await.unwrap();
        let stored = items_service::get(pool, &ctx, id).await.unwrap();
        assert_eq!(serde_json::to_value(&stored).unwrap()["attributes"]["remarks"], json!(notes));
        let update = Body::<UpdateItemBody>::parse(Some(json!({"attributes": {"remarks": notes}}))).unwrap().0;
        assert_eq!(codes(items_service::update(pool, &ctx, id, &update).await.unwrap_err()), refused("remarks"));
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
                credential: crate::auth::Credential::Token {
                    profile_id: None,
                    creator_id: None,
                    token_id: None,
                    minted_by: None,
                },
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
                credential: crate::auth::Credential::Token {
                    profile_id: None,
                    creator_id: None,
                    token_id: None,
                    minted_by: None,
                },
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
        // A type purge deletes its CIs before the table is dropped: its summary carries the
        // count, and the table's rows are counted before they are deleted (GH#281).
        let purges = [
            ("purgeField", field.id, "code", "drop_column", 3, "Purge field", vec![secrets.id, vault.id]),
            (
                "purgeType",
                vault.id,
                vault.key.as_str(),
                "drop_table",
                1,
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
                credential: crate::auth::Credential::Token {
                    profile_id: None,
                    creator_id: None,
                    token_id: None,
                    minted_by: None,
                },
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
        // Viewing every class includes the built-in business service and Person types (0033, 0044).
        let mut viewer_classes: Vec<Uuid> =
            sqlx::query_scalar("SELECT id FROM ci_classes WHERE system_role IS NOT NULL")
                .fetch_all(pool)
                .await
                .unwrap();
        viewer_classes.extend([servers.id, secrets.id]);
        let viewer = datamodel_manager(&viewer_classes);

        // The hidden class: its CI counts are withheld, the data-model counts are not.
        let report = simple::usage::<CiClasses>(pool, &restricted, secrets.id).await.unwrap();
        assert!(!report.in_use, "CIs, fields and rules do not block a type's purge");
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
        assert_eq!(
            c,
            [
                ("attributeValues".to_string(), None, true),
                ("workflows".to_string(), Some(0), false),
                ("dependentFields".to_string(), Some(0), false)
            ]
        );
        let c = counts(&simple::usage::<AttributeDefinitions>(pool, &viewer, code.id).await.unwrap());
        assert_eq!(
            c,
            [
                ("attributeValues".to_string(), Some(2), false),
                ("workflows".to_string(), Some(0), false),
                ("dependentFields".to_string(), Some(0), false)
            ]
        );

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
                credential: crate::auth::Credential::Token {
                    profile_id: None,
                    creator_id: None,
                    token_id: None,
                    minted_by: None,
                },
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
        // Nor whether any asset lacks a value (GH#276).
        assert!(
            warning.message.contains("stays nullable") && !warning.message.contains("no value"),
            "{}",
            warning.message
        );
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
                credential: crate::auth::Credential::Token {
                    profile_id: None,
                    creator_id: None,
                    token_id: None,
                    minted_by: None,
                },
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
            let entries = audit::list(pool, &reader, &crate::secrets::Keyring::for_tests(), &query).await.unwrap().data;
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
    /// SHAA-812: DELETE on a type or field only archives it, so `/usage`
    /// reports `removal: purge`, and `blocking`/`inUse` say exactly what the
    /// purge refuses: CIs, fields and rules go with it; subtypes, reference
    /// fields of other types and dependent fields stop it.
    #[tokio::test]
    async fn usage_blocking_matches_what_the_purge_refuses() {
        use crate::modules::lookups::LookupLists;
        use simple::Removal;
        let Some(db) = scratch::database("usage_blocking_matches_purge").await else { return };
        let pool = &db.pool;
        let ctx = &RequestContext::system("shaa-812-test", "shaa-812-test");
        let class = |v: Value| async move { simple::create::<CiClasses>(pool, ctx, &body(v)).await.unwrap() };
        let field =
            |v: Value| async move { simple::create::<AttributeDefinitions>(pool, ctx, &body(v)).await.unwrap() };
        let archive = |id: Uuid| async move {
            simple::update::<CiClasses>(pool, ctx, id, &body(json!({"isActive": false}))).await.unwrap();
        };
        let purge = |id: Uuid, key: String| async move {
            let mut tx = pool.begin().await.unwrap();
            let r = purge_class_in(&mut tx, ctx, id, &key).await.map(|_| ());
            tx.commit().await.unwrap();
            r
        };
        let blocked = |report: &simple::UsageReport| -> Vec<String> {
            report.data.iter().filter(|u| u.blocking && u.count.exact() != Some(0)).map(|u| u.kind.clone()).collect()
        };

        // A type with CIs, a field and a relationship rule: nothing blocks.
        let servers = class(json!({"name": "Servers"})).await;
        field(json!({"classId": servers.id, "key": "code", "label": "Code", "dataType": "text"})).await;
        let feeds: RelationshipType = simple::create::<RelationshipTypes>(
            pool,
            ctx,
            &body(json!({"key": "feeds", "name": "Feeds", "forwardLabel": "feeds", "reverseLabel": "fed by"})),
        )
        .await
        .unwrap();
        simple::create::<RelationshipRules>(
            pool,
            ctx,
            &body(json!({"relationshipTypeId": feeds.id, "sourceClassId": servers.id, "targetClassId": servers.id})),
        )
        .await
        .unwrap();
        items_service::create(pool, ctx, &body::<CreateItemBody>(json!({"classId": servers.id}))).await.unwrap();
        let report = simple::usage::<CiClasses>(pool, ctx, servers.id).await.unwrap();
        assert_eq!(report.removal, Removal::Purge);
        assert!(!report.in_use);
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["removal"], json!("purge"), "{json}");
        for kind in ["configurationItems", "attributeDefinitions", "relationshipRules"] {
            let u = report.data.iter().find(|u| u.kind == kind).unwrap();
            assert_eq!((u.count.exact(), u.blocking), (Some(1), false), "{kind}");
        }

        // A subtype and a reference field of another type block the purge.
        let parent = class(json!({"name": "Hosts"})).await;
        let child = class(json!({"name": "Blades", "parentId": parent.id})).await;
        let other = class(json!({"name": "Racks"})).await;
        field(json!({"classId": other.id, "key": "host", "label": "Host", "dataType": "reference", "referenceClassId": parent.id}))
            .await;
        let report = simple::usage::<CiClasses>(pool, ctx, parent.id).await.unwrap();
        assert!(report.in_use);
        assert_eq!(blocked(&report), ["subclasses", "referencingAttributes"]);
        archive(parent.id).await;
        let err = purge(parent.id, parent.key.clone()).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::InUse, "{err:?}");

        // Once they are gone, the report and the purge agree again.
        archive(child.id).await;
        purge(child.id, child.key.clone()).await.unwrap();
        archive(other.id).await;
        purge(other.id, other.key.clone()).await.unwrap();
        assert!(!simple::usage::<CiClasses>(pool, ctx, parent.id).await.unwrap().in_use);
        purge(parent.id, parent.key.clone()).await.unwrap();
        archive(servers.id).await;
        purge(servers.id, servers.key.clone()).await.unwrap();

        // A field that is another field's parent field blocks the field's purge.
        let racks = class(json!({"name": "Cabinets"})).await;
        let maker =
            simple::create::<LookupLists>(pool, ctx, &body(json!({"key": "maker", "name": "Maker"}))).await.unwrap();
        let model = simple::create::<LookupLists>(
            pool,
            ctx,
            &body(json!({"key": "model", "name": "Model", "parentListId": maker.id})),
        )
        .await
        .unwrap();
        let vendor = field(json!({"classId": racks.id, "key": "vendor", "label": "Vendor", "dataType": "lookup", "lookupListId": maker.id}))
            .await;
        field(json!({"classId": racks.id, "key": "model", "label": "Model", "dataType": "lookup",
            "lookupListId": model.id, "parentAttributeId": vendor.id}))
        .await;
        let report = simple::usage::<AttributeDefinitions>(pool, ctx, vendor.id).await.unwrap();
        assert_eq!(report.removal, Removal::Purge);
        assert!(report.in_use);
        assert_eq!(blocked(&report), ["dependentFields"]);
        simple::update::<AttributeDefinitions>(pool, ctx, vendor.id, &body(json!({"isActive": false}))).await.unwrap();
        let mut tx = pool.begin().await.unwrap();
        let err = purge_attribute_in(&mut tx, ctx, vendor.id, "vendor").await.unwrap_err();
        assert_eq!(err.code, ErrorCode::InUse, "{err:?}");
        drop(tx);

        // A hard-deleted resource still reports `delete`.
        let report = simple::usage::<RelationshipTypes>(pool, ctx, feeds.id).await.unwrap();
        assert_eq!(report.removal, Removal::Delete);
        db.drop().await;
    }
}
