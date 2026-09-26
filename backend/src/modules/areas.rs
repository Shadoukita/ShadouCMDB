//! Areas: the top-level groups of types, shown as menu tabs. Each area is a
//! PostgreSQL schema ("Bestand" -> `bestand`) holding the tables of its types.
//!
//! The technical name (`key`) is derived from the display name unless given,
//! and cannot change afterwards. DELETE archives an area (its schema and data
//! stay); POST .../purge drops the schema once the area is archived and holds
//! no types.

use axum::http::Method;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::schema_changes::{PurgeRequest, check_purge};
use super::simple_resource::{self as simple, BoxFuture, ListQuery, Resource, Writable, bool_filter, non_empty};
use crate::api::context::RequestContext;
use crate::api::route::{Body, Check, IdPath, In, Json, NoQuery, Route, route};
use crate::api::schemas::{
    self, QueryBool, Sort, description_schema, name_schema, sort_order_schema, technical_name_schema, trimmed, ts,
};
use crate::auth::permissions::GlobalPermission;
use crate::data::crud::{self, AuditAction, AuditEntry, ColumnSet, Where};
use crate::http::error::{AppError, ErrorCode, FieldError};
use crate::paged;
use crate::schema::naming::{self, Ident, NameKind};
use crate::schema::{self as engine, Purge, SchemaChange, Scope};

/// Where types go that were created before areas existed (migration 0007), by
/// a version 1 configuration file, or by the IT infrastructure template.
pub const DEFAULT_AREA: (&str, &str) = ("infrastruktur", "Infrastruktur");

/// The id of the default area, created (and audited) if it does not exist yet.
pub async fn default_area(conn: &mut PgConnection, ctx: &RequestContext) -> Result<Uuid, AppError> {
    let existing: Option<Uuid> = sqlx::query_scalar("SELECT id FROM cmdb.areas WHERE key = $1")
        .bind(DEFAULT_AREA.0)
        .fetch_optional(&mut *conn)
        .await?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let mut columns = ColumnSet::default();
    columns.opt("key", Some(DEFAULT_AREA.0.to_owned())).opt("name", Some(DEFAULT_AREA.1.to_owned()));
    Ok(simple::create_in::<Areas>(conn, ctx, columns).await?.id)
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Area {
    pub id: Uuid,
    /// Technical name: the PostgreSQL schema holding the area's tables. Immutable.
    pub key: String,
    pub name: String,
    #[schema(required = true)]
    pub description: Option<String>,
    #[schema(required = true)]
    pub icon: Option<String>,
    /// Hex colour for the menu tab, e.g. "#1f6feb"
    #[schema(required = true)]
    pub color: Option<String>,
    /// Position of the tab (ascending)
    pub sort_order: i32,
    /// Archived areas keep their schema and data; the UI hides them
    pub is_active: bool,
    /// Types in the area (archived ones included)
    pub type_count: i64,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AreaCreate {
    /// Leave out to derive it from the name ("Bestand" -> "bestand")
    #[schema(schema_with = technical_name_schema)]
    #[serde(default)]
    key: Option<String>,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    name: String,
    #[schema(schema_with = description_schema)]
    #[serde(default)]
    description: Option<String>,
    #[schema(schema_with = super::classes::icon_schema)]
    #[serde(default)]
    icon: Option<String>,
    #[schema(schema_with = schemas::nullable_color_schema)]
    #[serde(default)]
    color: Option<String>,
    #[schema(schema_with = sort_order_schema)]
    sort_order: Option<i32>,
    #[schema(nullable = false)]
    is_active: Option<bool>,
}

impl AreaCreate {
    pub fn technical_name(&self) -> String {
        self.key.clone().unwrap_or_else(|| naming::derive(&self.name, NameKind::Area))
    }
}

// `key` is immutable: it is the schema name reports and integrations query.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AreaUpdate {
    #[schema(schema_with = name_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    name: Option<String>,
    #[schema(schema_with = description_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    description: Option<Option<String>>,
    #[schema(schema_with = super::classes::icon_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    icon: Option<Option<String>>,
    #[schema(schema_with = schemas::nullable_color_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    color: Option<Option<String>>,
    #[schema(schema_with = sort_order_schema)]
    sort_order: Option<i32>,
    /// false archives the area, true restores it
    #[schema(nullable = false)]
    is_active: Option<bool>,
}

impl Writable for AreaCreate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("key", Some(self.technical_name()))
            .opt("name", Some(self.name.clone()))
            .opt("description", self.description.clone().map(Some))
            .opt("icon", self.icon.clone().map(Some))
            .opt("color", self.color.clone().map(Some))
            .opt("sort_order", self.sort_order)
            .opt("is_active", self.is_active);
        c
    }
}
impl Check for AreaCreate {}

impl Writable for AreaUpdate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("name", self.name.clone())
            .opt("description", self.description.clone())
            .opt("icon", self.icon.clone())
            .opt("color", self.color.clone())
            .opt("sort_order", self.sort_order)
            .opt("is_active", self.is_active);
        c
    }
}
impl Check for AreaUpdate {
    fn check(&self) -> Vec<FieldError> {
        non_empty(&self.columns())
    }
}

fn area_sort() -> utoipa::openapi::schema::Schema {
    schemas::sort_schema(&["sortOrder", "name", "key", "createdAt", "updatedAt"], "sortOrder")
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct AreaList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    offset: i64,
    #[param(schema_with = schemas::search_schema)]
    q: Option<String>,
    #[param(required = false, schema_with = area_sort)]
    sort: Sort,
    #[param(inline)]
    is_active: Option<QueryBool>,
}
paged!(AreaList);

impl ListQuery for AreaList {
    fn q(&self) -> Option<&str> {
        self.q.as_deref()
    }
    fn sort(&self) -> &Sort {
        &self.sort
    }
    fn filter(&self, w: &mut Where<'_>) {
        bool_filter(w, "is_active", self.is_active);
    }
}

pub struct Areas;

impl Resource for Areas {
    type Dto = Area;
    type Create = AreaCreate;
    type Update = AreaUpdate;
    type List = AreaList;
    const TABLE: &'static str = "areas";
    const LABEL: &'static str = "Area";
    const BASE_PATH: &'static str = "/api/v1/areas";
    const TAG: &'static str = "Areas";
    const SINGULAR: &'static str = "area";
    const PLURAL: &'static str = "areas";
    const COLUMNS: &'static str = "id, key, name, description, icon, color, sort_order, is_active,
        (SELECT count(*) FROM cmdb.ci_classes c WHERE c.area_id = areas.id) AS type_count, created_at, updated_at";
    const SEARCH_COLUMNS: &'static [&'static str] = &["key", "name", "description"];
    const ARCHIVE_ON_DELETE: bool = true;
    const WRITE_ERRORS: &'static [ErrorCode] = &[ErrorCode::InvalidName, ErrorCode::SchemaChangeRefused];
    const DELETE_DESCRIPTION: &'static str = "Archives the area (`isActive=false`): its schema, tables and data stay, \
        the UI hides it. `PATCH {\"isActive\": true}` restores it. To drop the schema, purge the area.";

    fn id(row: &Area) -> Uuid {
        row.id
    }

    fn validate(columns: &ColumnSet, create: bool) -> Result<(), AppError> {
        match engine::key_column(columns) {
            Some(key) if create => engine::validate_name(key, NameKind::Area, "key"),
            _ => Ok(()),
        }
    }

    fn before_write(conn: &mut PgConnection) -> BoxFuture<'_, Result<(), AppError>> {
        Box::pin(async move { Ok(engine::lock(conn).await?) })
    }

    fn after_write<'a>(
        conn: &'a mut PgConnection,
        ctx: &'a RequestContext,
        row: &'a Area,
        previous: Option<&'a Area>,
    ) -> BoxFuture<'a, Result<(), AppError>> {
        Box::pin(async move {
            if previous.is_none() {
                let taken: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = $1)")
                    .bind(&row.key)
                    .fetch_one(&mut *conn)
                    .await?;
                if taken {
                    return Err(engine::invalid_name(
                        "key",
                        "name_taken",
                        format!("A schema named \"{}\" already exists in the database", row.key),
                    ));
                }
                engine::apply(conn, ctx, &format!("Create area {}", row.key), Scope::Areas, Purge::default()).await?;
            }
            Ok(())
        })
    }
}

/// Drops the schema of an archived area that holds no types.
pub async fn purge_in(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    id: Uuid,
    confirm: &str,
) -> Result<Option<SchemaChange>, AppError> {
    engine::lock(conn).await?;
    let row: Area = crud::select_by_id(conn, Areas::TABLE, Areas::COLUMNS, id, true)
        .await?
        .ok_or_else(|| AppError::missing(Areas::LABEL, id))?;
    check_purge("area", &row.key, row.is_active, confirm)?;
    if row.type_count > 0 {
        return Err(AppError::new(
            ErrorCode::InUse,
            format!("Area \"{}\" still holds {} types; purge them first", row.key, row.type_count),
        ));
    }
    crud::delete_row(conn, Areas::TABLE, id).await?;
    let purge = Purge { schemas: vec![Ident::trusted(&row.key)], ..Purge::default() };
    let change = engine::apply(conn, ctx, &format!("Purge area {}", row.key), Scope::Areas, purge).await?;
    let entry = AuditEntry {
        action: AuditAction::Delete,
        entity_type: Areas::TABLE,
        entity_id: id,
        old_value: Some(crud::json(&row)),
        new_value: None,
    };
    crud::write_audit(conn, ctx, vec![entry]).await?;
    Ok(change)
}

pub fn routes() -> Vec<Route> {
    let mut r = simple::routes::<Areas>();
    r.push(
        route(Method::POST, "/api/v1/areas/{id}/purge", "purgeArea")
            .tag(Areas::TAG)
            .summary("Purge an archived area: drop its schema")
            .description(
                "Irreversible. The area must be archived and hold no types (purge those first), and `confirm` must \
                 repeat its technical name. Returns the schema change that ran.",
            )
            .requires(GlobalPermission::DatamodelManage)
            .errors(&[ErrorCode::NotFound, ErrorCode::InUse, ErrorCode::Conflict])
            .handle(|api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<PurgeRequest>>| async move {
                let mut tx = api.pool.begin().await?;
                let change = purge_in(&mut tx, &api.ctx, id, &b.confirm).await?;
                tx.commit().await?;
                Ok(Json(super::schema_changes::PurgeResult { schema_change: change }))
            }),
    );
    r
}
