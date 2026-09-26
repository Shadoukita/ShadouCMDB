//! The schema change history, previews, technical-name suggestions and the
//! purge confirmation shared by areas, types and fields.

use axum::http::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::areas::{self, Areas};
use super::classes::{self, AttributeDefinitions, CiClasses};
use super::simple_resource::{self as simple, Writable};
use crate::api::context::RequestContext;
use crate::api::route::{Body, BodyInput, Check, IdPath, In, Json, NoBody, NoPath, NoQuery, Query, Route, route};
use crate::api::schemas::{self, Page, Paged, Sort, like_pattern};
use crate::auth::permissions::GlobalPermission;
use crate::data::crud::{self, Where};
use crate::http::error::{AppError, ErrorCode};
use crate::paged;
use crate::schema::naming::{self, NameKind};
use crate::schema::{self as engine, Impact, SchemaChange};

const TAG: &str = "Schema changes";

// ---------------------------------------------------------------------------
// Purge (shared)
// ---------------------------------------------------------------------------

/// Confirms an irreversible purge
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PurgeRequest {
    /// The technical name of what is purged, typed by the administrator
    #[schema(min_length = 1, max_length = 200)]
    pub confirm: String,
}
impl Check for PurgeRequest {}

/// What a purge dropped
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PurgeResult {
    /// The DDL that ran (null when the object had no table or column yet)
    #[schema(required = true)]
    pub schema_change: Option<SchemaChange>,
}

/// A purge needs an archived object and its technical name typed back.
pub fn check_purge(kind: &str, key: &str, is_active: bool, confirm: &str) -> Result<(), AppError> {
    if is_active {
        return Err(AppError::conflict(format!(
            "Archive the {kind} \"{key}\" first (DELETE), then purge it; archiving keeps the data"
        )));
    }
    if confirm.trim() != key {
        return Err(AppError::field(
            "confirm",
            format!("Type the technical name \"{key}\" to confirm that its data is deleted"),
            "confirmation_mismatch",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

fn change_sort() -> utoipa::openapi::schema::Schema {
    schemas::sort_schema(&["occurredAt"], "-occurredAt")
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct SchemaChangeList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    offset: i64,
    /// Substring of the summary or of any statement (e.g. a table name)
    #[param(schema_with = schemas::search_schema)]
    q: Option<String>,
    #[param(required = false, schema_with = change_sort)]
    sort: Sort,
}
paged!(SchemaChangeList);

const CHANGE_COLUMNS: &str =
    "id, occurred_at, actor_type, actor_id, actor_name, request_id, summary, statements, impact";

pub async fn list(pool: &PgPool, q: &SchemaChangeList) -> Result<Page<SchemaChange>, AppError> {
    let filter = |w: &mut Where<'_>| {
        if let Some(q) = &q.q {
            let p = like_pattern(q);
            w.and()
                .push("(summary ILIKE ")
                .push_bind(p.clone())
                .push(" OR array_to_string(statements, ' ') ILIKE ")
                .push_bind(p)
                .push(")");
        }
    };
    let order = format!("occurred_at {}, id {}", q.sort.dir(), q.sort.dir());
    let (rows, total) = crud::select_page::<SchemaChange>(
        pool,
        "cmdb.schema_changes",
        CHANGE_COLUMNS,
        &filter,
        &order,
        q.limit,
        q.offset,
    )
    .await?;
    Ok(Page { data: rows, page: q.page_meta(total) })
}

pub async fn get(pool: &PgPool, id: Uuid) -> Result<SchemaChange, AppError> {
    let mut conn = pool.acquire().await?;
    crud::select_by_id(&mut conn, "cmdb.schema_changes", CHANGE_COLUMNS, id, false)
        .await?
        .ok_or_else(|| AppError::missing("Schema change", id))
}

// ---------------------------------------------------------------------------
// Preview
// ---------------------------------------------------------------------------

/// The data model operation to preview (the endpoint it stands for in brackets)
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum PreviewOperation {
    /// POST /api/v1/areas
    CreateArea,
    /// PATCH /api/v1/areas/{id}
    UpdateArea,
    /// DELETE /api/v1/areas/{id} (archive)
    DeleteArea,
    /// POST /api/v1/areas/{id}/purge
    PurgeArea,
    /// POST /api/v1/ci-classes
    CreateType,
    /// PATCH /api/v1/ci-classes/{id}
    UpdateType,
    /// DELETE /api/v1/ci-classes/{id} (archive)
    DeleteType,
    /// POST /api/v1/ci-classes/{id}/purge
    PurgeType,
    /// POST /api/v1/attribute-definitions
    CreateField,
    /// PATCH /api/v1/attribute-definitions/{id}
    UpdateField,
    /// DELETE /api/v1/attribute-definitions/{id} (archive)
    DeleteField,
    /// POST /api/v1/attribute-definitions/{id}/purge
    PurgeField,
}

/// An operation to dry-run
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewRequest {
    #[schema(inline)]
    operation: PreviewOperation,
    /// The area, type or field (update, delete and purge)
    #[schema(schema_with = schemas::nullable_uuid_schema)]
    #[serde(default)]
    id: Option<Uuid>,
    /// The body the operation's endpoint takes (create, update, purge)
    #[schema(value_type = Option<Object>)]
    #[serde(default)]
    body: Option<Value>,
}
impl Check for PreviewRequest {}

/// What an operation would do, computed by running it in a transaction that is rolled back
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SchemaChangePreview {
    /// One line per schema change it would make
    pub summaries: Vec<String>,
    /// The DDL, in the order it would run (empty: the change touches no table)
    pub statements: Vec<String>,
    /// Effect on existing data; statement indexes refer to `statements`
    pub impact: Vec<Impact>,
    /// What the endpoint would return (the created or updated record); null for delete and purge
    #[schema(value_type = Option<Object>, required = true)]
    pub result: Option<Value>,
}

fn needs_id(id: Option<Uuid>) -> Result<Uuid, AppError> {
    id.ok_or_else(|| AppError::field("id", "Required for this operation", "required"))
}

fn body<T: BodyInput>(v: &Option<Value>) -> Result<T, AppError> {
    T::parse(Some(v.clone().unwrap_or(Value::Object(Default::default())))).map_err(|mut e| {
        if let Some(details) = &mut e.details {
            for d in details.iter_mut() {
                d.field = format!("body.{}", d.field);
            }
        }
        e
    })
}

async fn run_preview(
    tx: &mut sqlx::PgConnection,
    ctx: &RequestContext,
    r: &PreviewRequest,
) -> Result<Option<Value>, AppError> {
    use PreviewOperation as Op;
    Ok(match r.operation {
        Op::CreateArea => {
            let Body(b) = body::<Body<areas::AreaCreate>>(&r.body)?;
            Some(crud::json(&simple::create_in::<Areas>(tx, ctx, b.columns()).await?))
        }
        Op::UpdateArea => {
            let Body(b) = body::<Body<areas::AreaUpdate>>(&r.body)?;
            Some(crud::json(&simple::update_in::<Areas>(tx, ctx, needs_id(r.id)?, b.columns()).await?))
        }
        Op::DeleteArea => {
            simple::remove_in::<Areas>(tx, ctx, needs_id(r.id)?).await?;
            None
        }
        Op::PurgeArea => {
            let Body(b) = body::<Body<PurgeRequest>>(&r.body)?;
            areas::purge_in(tx, ctx, needs_id(r.id)?, &b.confirm).await?;
            None
        }
        Op::CreateType => {
            let Body(b) = body::<Body<classes::CiClassCreate>>(&r.body)?;
            Some(crud::json(&simple::create_in::<CiClasses>(tx, ctx, b.columns()).await?))
        }
        Op::UpdateType => {
            let Body(b) = body::<Body<classes::CiClassUpdate>>(&r.body)?;
            Some(crud::json(&simple::update_in::<CiClasses>(tx, ctx, needs_id(r.id)?, b.columns()).await?))
        }
        Op::DeleteType => {
            simple::remove_in::<CiClasses>(tx, ctx, needs_id(r.id)?).await?;
            None
        }
        Op::PurgeType => {
            let Body(b) = body::<Body<PurgeRequest>>(&r.body)?;
            classes::purge_class_in(tx, ctx, needs_id(r.id)?, &b.confirm).await?;
            None
        }
        Op::CreateField => {
            let Body(b) = body::<Body<classes::AttributeDefinitionCreate>>(&r.body)?;
            Some(crud::json(&simple::create_in::<AttributeDefinitions>(tx, ctx, b.columns()).await?))
        }
        Op::UpdateField => {
            let Body(b) = body::<Body<classes::AttributeDefinitionUpdate>>(&r.body)?;
            Some(crud::json(&simple::update_in::<AttributeDefinitions>(tx, ctx, needs_id(r.id)?, b.columns()).await?))
        }
        Op::DeleteField => {
            simple::remove_in::<AttributeDefinitions>(tx, ctx, needs_id(r.id)?).await?;
            None
        }
        Op::PurgeField => {
            let Body(b) = body::<Body<PurgeRequest>>(&r.body)?;
            classes::purge_attribute_in(tx, ctx, needs_id(r.id)?, &b.confirm).await?;
            None
        }
    })
}

pub async fn preview(pool: &PgPool, ctx: &RequestContext, r: &PreviewRequest) -> Result<SchemaChangePreview, AppError> {
    let mut tx = pool.begin().await?;
    let (result, changes) = engine::collect_previews(run_preview(&mut tx, ctx, r)).await;
    // Whatever happened, nothing is kept.
    tx.rollback().await?;
    let result = result?;
    let mut out = SchemaChangePreview { summaries: Vec::new(), statements: Vec::new(), impact: Vec::new(), result };
    for c in changes {
        let offset = out.statements.len();
        out.summaries.push(c.summary);
        out.impact.extend(c.impact.0.into_iter().map(|i| Impact { statement: i.statement.map(|s| s + offset), ..i }));
        out.statements.extend(c.statements);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Technical names
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct TechnicalNameQuery {
    #[param(inline)]
    kind: NameKind,
    /// The display name ("Virtuelle Maschinen")
    #[param(min_length = 1, max_length = 200)]
    name: String,
    /// The technical name the administrator typed instead, to check it
    #[param(min_length = 1, max_length = 200)]
    key: Option<String>,
    /// kind=type: the area the type goes into (for the table name shown)
    area_id: Option<Uuid>,
    /// kind=field: the type the field goes into (checked for names taken in its lineage)
    class_id: Option<Uuid>,
}

/// A suggested (or typed) technical name and whether it can be used
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TechnicalName {
    /// The name that would be used: `key` when given, otherwise derived from `name`
    pub technical_name: String,
    /// Derived from the display name (not typed)
    pub derived: bool,
    pub valid: bool,
    /// Why it cannot be used: empty, invalid_format, too_long, reserved_word, reserved_name, reserved_prefix, name_taken
    #[schema(required = true)]
    pub code: Option<String>,
    #[schema(required = true)]
    pub message: Option<String>,
    /// Where it would live, e.g. "bestand.virtuelle_maschinen" (types with areaId, fields with classId)
    #[schema(required = true)]
    pub qualified_name: Option<String>,
}

pub async fn technical_name(pool: &PgPool, q: &TechnicalNameQuery) -> Result<TechnicalName, AppError> {
    let derived = q.key.is_none();
    let name = q.key.clone().unwrap_or_else(|| naming::derive(&q.name, q.kind));
    let mut out = TechnicalName {
        technical_name: name.clone(),
        derived,
        valid: true,
        code: None,
        message: None,
        qualified_name: None,
    };
    if let Err(p) = naming::validate(&name, q.kind) {
        out.valid = false;
        out.code = Some(p.code.into());
        out.message = Some(p.message);
        return Ok(out);
    }
    let mut conn = pool.acquire().await?;
    let taken: Option<String> = match q.kind {
        NameKind::Area => sqlx::query_scalar(
            "SELECT CASE WHEN EXISTS (SELECT 1 FROM cmdb.areas WHERE key = $1) THEN 'an existing area'
                         WHEN EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = $1) THEN 'a schema in the database' END",
        )
        .bind(&name)
        .fetch_one(&mut *conn)
        .await?,
        NameKind::Type => {
            if let Some(area) = q.area_id {
                out.qualified_name = sqlx::query_scalar("SELECT key || '.' || $2 FROM cmdb.areas WHERE id = $1")
                    .bind(area)
                    .bind(&name)
                    .fetch_optional(&mut *conn)
                    .await?;
            }
            sqlx::query_scalar(
                "SELECT 'type \"' || c.name || '\" in area ' || a.key FROM cmdb.ci_classes c
                 JOIN cmdb.areas a ON a.id = c.area_id WHERE c.key = $1",
            )
            .bind(&name)
            .fetch_optional(&mut *conn)
            .await?
        }
        NameKind::Field => match q.class_id {
            Some(class_id) => {
                out.qualified_name = sqlx::query_scalar("SELECT cmdb.type_table($1) || '.' || $2")
                    .bind(class_id)
                    .bind(&name)
                    .fetch_one(&mut *conn)
                    .await?;
                crate::data::classes::attribute_key_clash(&mut conn, class_id, &name, Uuid::nil())
                    .await?
                    .map(|on| format!("a field of type \"{on}\" in the same lineage"))
            }
            None => None,
        },
    };
    if let Some(by) = taken {
        out.valid = false;
        out.code = Some("name_taken".into());
        out.message = Some(format!("\"{name}\" is already used by {by}"));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

/// The result of a reconcile
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReconcileResult {
    /// What ran; null when the database already matched the data model
    #[schema(required = true)]
    pub schema_change: Option<SchemaChange>,
}

pub fn routes() -> Vec<Route> {
    vec![
        route(Method::GET, "/api/v1/schema-changes", "listSchemaChanges")
            .tag(TAG)
            .summary("History of the DDL the data model administration ran (newest first)")
            .requires(GlobalPermission::DatamodelManage)
            .handle(|api, In(NoPath, Query(q), NoBody): In<NoPath, Query<SchemaChangeList>, NoBody>| async move {
                Ok(Json(list(&api.pool, &q).await?))
            }),
        route(Method::GET, "/api/v1/schema-changes/{id}", "getSchemaChange")
            .tag(TAG)
            .summary("One schema change")
            .requires(GlobalPermission::DatamodelManage)
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(get(&api.pool, id).await?))
            }),
        route(Method::POST, "/api/v1/schema-changes/preview", "previewSchemaChange")
            .tag(TAG)
            .summary("Preview a data model change: its DDL and its impact on stored data")
            .description(
                "Runs the operation exactly as its endpoint would, including every check and guard, inside a \
                 transaction that is always rolled back, and returns the DDL it would run. A refused change returns \
                 the same error the endpoint would (e.g. 422 SCHEMA_CHANGE_REFUSED when a type change would not \
                 convert every stored value). Nothing is changed.",
            )
            .requires(GlobalPermission::DatamodelManage)
            .errors(&[
                ErrorCode::NotFound,
                ErrorCode::Conflict,
                ErrorCode::InUse,
                ErrorCode::InvalidName,
                ErrorCode::SchemaChangeRefused,
            ])
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<PreviewRequest>>| async move {
                Ok(Json(preview(&api.pool, &api.ctx, &b).await?))
            }),
        route(Method::POST, "/api/v1/schema-changes/reconcile", "reconcileSchema")
            .tag(TAG)
            .summary("Bring every area schema, type table and reporting view in line with the data model")
            .description(
                "Creates what is missing (e.g. grants for a cmdb_reporting role created after the areas) and rebuilds \
                 stale reporting views. Never drops anything. `shadoucmdb migrate` runs the same after migrating.",
            )
            .requires(GlobalPermission::DatamodelManage)
            .errors(&[ErrorCode::SchemaChangeRefused])
            .handle(|api, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| async move {
                let mut tx = api.pool.begin().await?;
                let change = engine::reconcile(&mut tx, &api.ctx, "Reconcile").await?;
                tx.commit().await?;
                Ok(Json(ReconcileResult { schema_change: change }))
            }),
        route(Method::GET, "/api/v1/technical-names", "suggestTechnicalName")
            .tag(TAG)
            .summary("Derive a technical name from a display name, or check a typed one")
            .description(
                "\"Virtuelle Maschinen\" -> virtuelle_maschinen, \"Größe\" -> groesse. Reports whether the name can be \
                 used (format, reserved words and prefixes, names already taken) so the UI can show it before \
                 anything is created.",
            )
            .requires(GlobalPermission::DatamodelManage)
            .handle(|api, In(NoPath, Query(q), NoBody): In<NoPath, Query<TechnicalNameQuery>, NoBody>| async move {
                Ok(Json(technical_name(&api.pool, &q).await?))
            }),
    ]
}
