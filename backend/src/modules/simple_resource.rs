//! A plain resource: one table, list/get/create/update/delete, an audit row
//! per change. Any signed-in user may read (the UI needs the data model and
//! lookups to render CIs); writes need `datamodel.manage`. Lookups, classes, attribute definitions and relationship types
//! are all built from this; the configuration-item and relationship modules
//! have their own services because they carry more rules.

use std::future::Future;
use std::pin::Pin;

use axum::http::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sqlx::postgres::PgRow;
use sqlx::{FromRow, PgConnection, PgPool};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::api::context::RequestContext;
use crate::api::route::{Body, Check, IdPath, In, Json, NoBody, NoContent, NoPath, NoQuery, Query, Route, route};
use crate::api::schemas::{Page, Paged, Sort, like_pattern};
use crate::auth::permissions::GlobalPermission;
use crate::data::crud::{self, AuditAction, AuditEntry, ColumnSet, Where};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One kind of record that can refer to a row; `sql` counts them for `$1` (the row id).
pub struct Usage {
    /// Machine name, camelCase (e.g. "configurationItems").
    pub kind: &'static str,
    /// Plural noun for messages (e.g. "configuration items").
    pub label: &'static str,
    pub sql: &'static str,
    /// Blocks a hard delete. Non-blocking references are removed with the row (cascade).
    pub blocking: bool,
}

/// How many records of one kind refer to the row.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageCount {
    pub kind: String,
    pub label: String,
    pub count: i64,
    /// A non-zero count prevents deleting the row; retire it with isActive=false instead
    pub blocking: bool,
}

/// What still refers to a record, so the UI can warn before a destructive change
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageReport {
    /// True when a blocking count is non-zero: DELETE would return 409 IN_USE
    pub in_use: bool,
    pub data: Vec<UsageCount>,
}

/// A list query: pagination, optional search, sort and resource-specific filters.
pub trait ListQuery: Paged + Send + Sync + 'static {
    fn q(&self) -> Option<&str> {
        None
    }
    fn sort(&self) -> &Sort;
    /// Resource-specific filters (called once for the page and once for the count).
    fn filter(&self, w: &mut Where<'_>);
}

/// A request body that sets columns.
pub trait Writable {
    fn columns(&self) -> ColumnSet;
}

pub trait Resource: Send + Sync + 'static {
    type Dto: for<'r> FromRow<'r, PgRow> + Serialize + ToSchema + Send + Sync + Unpin + 'static;
    type Create: ToSchema + DeserializeOwned + Check + Writable + Send + Sync + 'static;
    type Update: ToSchema + DeserializeOwned + Check + Writable + Send + Sync + 'static;
    type List: IntoParams + DeserializeOwned + ListQuery;

    /// Table name, also audit_log.entity_type.
    const TABLE: &'static str;
    /// Human name for messages, e.g. "Status".
    const LABEL: &'static str;
    const BASE_PATH: &'static str;
    const TAG: &'static str;
    /// For operation ids: listStatuses, getStatus, ...
    const SINGULAR: &'static str;
    const PLURAL: &'static str;
    /// SELECT list producing a `Dto` row.
    const COLUMNS: &'static str;
    const SEARCH_COLUMNS: &'static [&'static str];
    const DELETE_DESCRIPTION: &'static str = "Hard delete, allowed only while nothing references the row. A referenced row returns 409 IN_USE whose details name what still refers to it (the same counts as the usage endpoint, where there is one); retire it with `PATCH {\"isActive\": false}` instead so history keeps resolving.";

    /// References reported by `GET {BASE_PATH}/{id}/usage` and checked before a delete.
    const USAGE: &'static [Usage] = &[];

    /// DELETE archives the row (`is_active = false`, data kept) instead of
    /// removing it; a separate purge removes it. Areas, types and fields: their
    /// rows are database objects (schemas, tables, columns).
    const ARCHIVE_ON_DELETE: bool = false;

    /// Set for resources kept for compatibility: every operation is marked
    /// deprecated in the OpenAPI document and its description starts with this.
    const DEPRECATED: Option<&'static str> = None;

    /// Description of the PATCH operation (empty: none).
    const UPDATE_DESCRIPTION: &'static str = "";

    /// Further errors create, update and delete can return (documented in the spec).
    const WRITE_ERRORS: &'static [ErrorCode] = &[];

    fn id(row: &Self::Dto) -> Uuid;

    /// Checks the columns of a create (`create = true`) or update before they
    /// are written, e.g. a technical name.
    fn validate(_columns: &ColumnSet, _create: bool) -> Result<(), AppError> {
        Ok(())
    }

    /// Runs first in every write transaction (e.g. to take a lock).
    fn before_write(_conn: &mut PgConnection) -> BoxFuture<'_, Result<(), AppError>> {
        Box::pin(async { Ok(()) })
    }

    /// Runs in a create's transaction after `before_write`, before the insert, to
    /// fill in columns that depend on other rows (e.g. a default parent).
    fn prepare_create<'a>(
        _conn: &'a mut PgConnection,
        _ctx: &'a RequestContext,
        _columns: &'a mut ColumnSet,
    ) -> BoxFuture<'a, Result<(), AppError>> {
        Box::pin(async { Ok(()) })
    }

    /// Runs inside the write transaction after insert/update; an error rolls back.
    fn after_write<'a>(
        _conn: &'a mut PgConnection,
        _ctx: &'a RequestContext,
        _row: &'a Self::Dto,
        _previous: Option<&'a Self::Dto>,
    ) -> BoxFuture<'a, Result<(), AppError>> {
        Box::pin(async { Ok(()) })
    }
}

/// Require at least one field in a PATCH body.
pub fn non_empty(columns: &ColumnSet) -> Vec<crate::http::error::FieldError> {
    if columns.is_empty() {
        vec![crate::http::error::FieldError {
            location: crate::http::error::FieldLocation::Body,
            field: "(root)".into(),
            message: "Provide at least one field to update".into(),
            code: "custom".into(),
        }]
    } else {
        Vec::new()
    }
}

fn camel_to_snake(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for c in s.chars() {
        if c.is_ascii_uppercase() {
            out.push('_');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

pub async fn list<R: Resource>(pool: &PgPool, query: &R::List) -> Result<Page<R::Dto>, AppError> {
    let filter = |w: &mut Where<'_>| {
        if let Some(q) = query.q() {
            let pattern = like_pattern(q);
            let qb = w.and();
            qb.push("(");
            for (i, col) in R::SEARCH_COLUMNS.iter().enumerate() {
                if i > 0 {
                    qb.push(" OR ");
                }
                qb.push(*col).push(" ILIKE ").push_bind(pattern.clone());
            }
            qb.push(")");
        }
        query.filter(w);
    };
    let sort = query.sort();
    let order = format!("{} {}, id ASC", camel_to_snake(&sort.field), sort.dir());
    let (rows, total) =
        crud::select_page::<R::Dto>(pool, R::TABLE, R::COLUMNS, &filter, &order, query.limit(), query.offset()).await?;
    Ok(Page { data: rows, page: query.page_meta(total) })
}

pub async fn get<R: Resource>(pool: &PgPool, id: Uuid) -> Result<R::Dto, AppError> {
    let mut conn = pool.acquire().await?;
    crud::select_by_id(&mut conn, R::TABLE, R::COLUMNS, id, false).await?.ok_or_else(|| AppError::missing(R::LABEL, id))
}

/// Inserts a row in the caller's transaction with the resource's checks and its audit row.
pub async fn create_in<R: Resource>(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    mut columns: ColumnSet,
) -> Result<R::Dto, AppError> {
    R::validate(&columns, true)?;
    R::before_write(conn).await?;
    R::prepare_create(conn, ctx, &mut columns).await?;
    let row: R::Dto = crud::insert_row(conn, R::TABLE, R::COLUMNS, columns).await?;
    R::after_write(conn, ctx, &row, None).await?;
    let entry = AuditEntry {
        action: AuditAction::Create,
        entity_type: R::TABLE,
        entity_id: R::id(&row),
        old_value: None,
        new_value: Some(crud::json(&row)),
    };
    crud::write_audit(conn, ctx, vec![entry]).await?;
    Ok(row)
}

/// Updates a row in the caller's transaction with the resource's checks and its audit row.
pub async fn update_in<R: Resource>(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    id: Uuid,
    columns: ColumnSet,
) -> Result<R::Dto, AppError> {
    R::validate(&columns, false)?;
    R::before_write(conn).await?;
    let before: R::Dto = crud::select_by_id(conn, R::TABLE, R::COLUMNS, id, true)
        .await?
        .ok_or_else(|| AppError::missing(R::LABEL, id))?;
    let row: R::Dto = crud::update_row(conn, R::TABLE, R::COLUMNS, id, columns).await?;
    R::after_write(conn, ctx, &row, Some(&before)).await?;
    let entry = AuditEntry {
        action: AuditAction::Update,
        entity_type: R::TABLE,
        entity_id: id,
        old_value: Some(crud::json(&before)),
        new_value: Some(crud::json(&row)),
    };
    crud::write_audit(conn, ctx, vec![entry]).await?;
    Ok(row)
}

pub async fn create<R: Resource>(pool: &PgPool, ctx: &RequestContext, body: &R::Create) -> Result<R::Dto, AppError> {
    let mut tx = pool.begin().await?;
    let row = create_in::<R>(&mut tx, ctx, body.columns()).await?;
    tx.commit().await?;
    Ok(row)
}

pub async fn update<R: Resource>(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    body: &R::Update,
) -> Result<R::Dto, AppError> {
    let mut tx = pool.begin().await?;
    let row = update_in::<R>(&mut tx, ctx, id, body.columns()).await?;
    tx.commit().await?;
    Ok(row)
}

async fn usage_counts<R: Resource>(conn: &mut PgConnection, id: Uuid) -> Result<UsageReport, AppError> {
    let mut data = Vec::with_capacity(R::USAGE.len());
    for u in R::USAGE {
        let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(u.sql)).bind(id).fetch_one(&mut *conn).await?;
        data.push(UsageCount { kind: u.kind.into(), label: u.label.into(), count, blocking: u.blocking });
    }
    Ok(UsageReport { in_use: data.iter().any(|u| u.blocking && u.count > 0), data })
}

pub async fn usage<R: Resource>(pool: &PgPool, id: Uuid) -> Result<UsageReport, AppError> {
    let mut conn = pool.acquire().await?;
    if crud::select_by_id::<R::Dto>(&mut conn, R::TABLE, R::COLUMNS, id, false).await?.is_none() {
        return Err(AppError::missing(R::LABEL, id));
    }
    usage_counts::<R>(&mut conn, id).await
}

/// 409 IN_USE naming every blocking reference, or Ok when the row can go.
fn refuse_if_used(label: &str, report: &UsageReport) -> Result<(), AppError> {
    if !report.in_use {
        return Ok(());
    }
    let blocking: Vec<&UsageCount> = report.data.iter().filter(|u| u.blocking && u.count > 0).collect();
    let summary = blocking.iter().map(|u| format!("{} {}", u.count, u.label)).collect::<Vec<_>>().join(", ");
    Err(AppError::new(
        ErrorCode::InUse,
        format!("{label} is still used by {summary}. Retire it with isActive=false instead of deleting it."),
    )
    .with_details(
        blocking
            .into_iter()
            .map(|u| FieldError {
                location: FieldLocation::Params,
                field: u.kind.clone(),
                message: format!("{} {}", u.count, u.label),
                code: "in_use".into(),
            })
            .collect(),
    ))
}

pub async fn remove<R: Resource>(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    remove_in::<R>(&mut tx, ctx, id).await?;
    tx.commit().await?;
    Ok(())
}

/// DELETE in the caller's transaction: archive or hard delete (see [`Resource::ARCHIVE_ON_DELETE`]).
pub async fn remove_in<R: Resource>(tx: &mut PgConnection, ctx: &RequestContext, id: Uuid) -> Result<(), AppError> {
    if R::ARCHIVE_ON_DELETE {
        let mut c = ColumnSet::default();
        c.opt("is_active", Some(false));
        update_in::<R>(tx, ctx, id, c).await?;
        return Ok(());
    }
    R::before_write(tx).await?;
    let before: R::Dto =
        crud::select_by_id(tx, R::TABLE, R::COLUMNS, id, true).await?.ok_or_else(|| AppError::missing(R::LABEL, id))?;
    refuse_if_used(R::LABEL, &usage_counts::<R>(tx, id).await?)?;
    crud::delete_row(tx, R::TABLE, id).await?;
    let entry = AuditEntry {
        action: AuditAction::Delete,
        entity_type: R::TABLE,
        entity_id: id,
        old_value: Some(crud::json(&before)),
        new_value: None,
    };
    crud::write_audit(tx, ctx, vec![entry]).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

fn cap(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

pub fn routes<R: Resource>() -> Vec<Route> {
    let label = R::LABEL.to_lowercase();
    let by_id = format!("{}/{{id}}", R::BASE_PATH);

    let mut list_route = route(Method::GET, R::BASE_PATH, format!("list{}", cap(R::PLURAL)))
        .tag(R::TAG)
        .summary(format!("List {label} records (paginated, searchable, sortable)"));
    if !R::SEARCH_COLUMNS.is_empty() {
        list_route = list_route
            .description(format!("`q` matches {} (case-insensitive substring).", R::SEARCH_COLUMNS.join(", ")));
    }

    let mut routes = vec![
        list_route.handle(|api, In(NoPath, Query(q), NoBody): In<NoPath, Query<R::List>, NoBody>| async move {
            Ok(Json(list::<R>(&api.pool, &q).await?))
        }),
        route(Method::GET, by_id.clone(), format!("get{}", cap(R::SINGULAR)))
            .tag(R::TAG)
            .summary(format!("Get one {label}"))
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(get::<R>(&api.pool, id).await?))
            }),
        route(Method::POST, R::BASE_PATH, format!("create{}", cap(R::SINGULAR)))
            .tag(R::TAG)
            .summary(format!("Create a {label}"))
            .requires(GlobalPermission::DatamodelManage)
            .status(StatusCode::CREATED)
            .errors(&[ErrorCode::Conflict])
            .errors(R::WRITE_ERRORS)
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<R::Create>>| async move {
                Ok(Json(create::<R>(&api.pool, &api.ctx, &b).await?))
            }),
        route(Method::PATCH, by_id.clone(), format!("update{}", cap(R::SINGULAR)))
            .tag(R::TAG)
            .summary(format!("Update a {label} (partial)"))
            .description(R::UPDATE_DESCRIPTION)
            .requires(GlobalPermission::DatamodelManage)
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict])
            .errors(R::WRITE_ERRORS)
            .handle(|api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<R::Update>>| async move {
                Ok(Json(update::<R>(&api.pool, &api.ctx, id, &b).await?))
            }),
        route(Method::DELETE, by_id, format!("delete{}", cap(R::SINGULAR)))
            .tag(R::TAG)
            .summary(format!("Delete a {label}"))
            .requires(GlobalPermission::DatamodelManage)
            .description(R::DELETE_DESCRIPTION)
            .errors(if R::ARCHIVE_ON_DELETE {
                &[ErrorCode::NotFound]
            } else {
                &[ErrorCode::NotFound, ErrorCode::InUse]
            })
            .errors(R::WRITE_ERRORS)
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                remove::<R>(&api.pool, &api.ctx, id).await?;
                Ok(NoContent)
            }),
    ];
    if !R::USAGE.is_empty() {
        let kinds: Vec<&str> = R::USAGE.iter().map(|u| u.kind).collect();
        routes.push(
            route(Method::GET, format!("{}/{{id}}/usage", R::BASE_PATH), format!("get{}Usage", cap(R::SINGULAR)))
                .tag(R::TAG)
                .summary(format!("What still refers to a {label}"))
                .description(format!(
                    "Counts of {}. Check it before deleting or restructuring: a blocking count makes DELETE return 409 IN_USE.",
                    kinds.join(", ")
                ))
                .errors(&[ErrorCode::NotFound])
                .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                    Ok(Json(usage::<R>(&api.pool, id).await?))
                }),
        );
    }
    if let Some(note) = R::DEPRECATED {
        for r in &mut routes {
            r.deprecated = true;
            r.description = Some(match r.description.take() {
                Some(d) => format!("{note} {d}"),
                None => note.to_owned(),
            });
        }
    }
    routes
}

/// `isActive=true|false` and similar boolean column filters.
pub fn bool_filter(w: &mut Where<'_>, column: &str, value: Option<crate::api::schemas::QueryBool>) {
    if let Some(v) = value {
        w.and().push(column).push(" = ").push_bind(bool::from(v));
    }
}
