//! A plain resource: one table, list/get/create/update/delete, an audit row
//! per change. Lookups, classes, attribute definitions and relationship types
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
use crate::data::crud::{self, AuditAction, AuditEntry, ColumnSet, Where};
use crate::http::error::{AppError, ErrorCode};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

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
    const DELETE_DESCRIPTION: &'static str = "Hard delete, allowed only while nothing references the row. A referenced row returns 409 IN_USE; retire it with `PATCH {\"isActive\": false}` instead so history keeps resolving.";

    fn id(row: &Self::Dto) -> Uuid;

    /// Runs inside the write transaction after insert/update; an error rolls back.
    fn after_write<'a>(
        _conn: &'a mut PgConnection,
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

pub async fn create<R: Resource>(pool: &PgPool, ctx: &RequestContext, body: &R::Create) -> Result<R::Dto, AppError> {
    let mut tx = pool.begin().await?;
    let row: R::Dto = crud::insert_row(&mut tx, R::TABLE, R::COLUMNS, body.columns()).await?;
    R::after_write(&mut tx, &row, None).await?;
    let entry = AuditEntry {
        action: AuditAction::Create,
        entity_type: R::TABLE,
        entity_id: R::id(&row),
        old_value: None,
        new_value: Some(crud::json(&row)),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
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
    let before: R::Dto = crud::select_by_id(&mut tx, R::TABLE, R::COLUMNS, id, true)
        .await?
        .ok_or_else(|| AppError::missing(R::LABEL, id))?;
    let row: R::Dto = crud::update_row(&mut tx, R::TABLE, R::COLUMNS, id, body.columns()).await?;
    R::after_write(&mut tx, &row, Some(&before)).await?;
    let entry = AuditEntry {
        action: AuditAction::Update,
        entity_type: R::TABLE,
        entity_id: id,
        old_value: Some(crud::json(&before)),
        new_value: Some(crud::json(&row)),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    tx.commit().await?;
    Ok(row)
}

pub async fn remove<R: Resource>(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    let before: R::Dto = crud::select_by_id(&mut tx, R::TABLE, R::COLUMNS, id, true)
        .await?
        .ok_or_else(|| AppError::missing(R::LABEL, id))?;
    crud::delete_row(&mut tx, R::TABLE, id).await?;
    let entry = AuditEntry {
        action: AuditAction::Delete,
        entity_type: R::TABLE,
        entity_id: id,
        old_value: Some(crud::json(&before)),
        new_value: None,
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    tx.commit().await?;
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

    vec![
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
            .status(StatusCode::CREATED)
            .errors(&[ErrorCode::Conflict])
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<R::Create>>| async move {
                Ok(Json(create::<R>(&api.pool, &api.ctx, &b).await?))
            }),
        route(Method::PATCH, by_id.clone(), format!("update{}", cap(R::SINGULAR)))
            .tag(R::TAG)
            .summary(format!("Update a {label} (partial)"))
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict])
            .handle(|api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<R::Update>>| async move {
                Ok(Json(update::<R>(&api.pool, &api.ctx, id, &b).await?))
            }),
        route(Method::DELETE, by_id, format!("delete{}", cap(R::SINGULAR)))
            .tag(R::TAG)
            .summary(format!("Delete a {label}"))
            .description(R::DELETE_DESCRIPTION)
            .errors(&[ErrorCode::NotFound, ErrorCode::InUse])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                remove::<R>(&api.pool, &api.ctx, id).await?;
                Ok(NoContent)
            }),
    ]
}

/// `isActive=true|false` and similar boolean column filters.
pub fn bool_filter(w: &mut Where<'_>, column: &str, value: Option<crate::api::schemas::QueryBool>) {
    if let Some(v) = value {
        w.and().push(column).push(" = ").push_bind(bool::from(v));
    }
}
