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

use crate::api::context::{Count, RequestContext};
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
    /// For a count over CI data: SQL returning (`uuid[]`, for `$1`) every class
    /// whose CIs the count can include, from the data model alone. The count is
    /// told only to a caller who may view them all (GH#265). `None`: the count
    /// is over data-model rows, which every signed-in user may read.
    pub spans: Option<&'static str>,
    /// A non-zero count refuses removing the row (409 IN_USE): DELETE, or the
    /// purge for a resource DELETE only archives ([`Resource::ARCHIVE_ON_DELETE`]).
    /// Non-blocking references are removed with the row (cascade).
    pub blocking: bool,
}

/// [`Usage::spans`] of a count over the CIs storing lookup value `$1`: every
/// class carrying a lookup field of its list, with the types below it.
pub const LOOKUP_VALUE_SPANS: &str = "SELECT coalesce(array_agg(DISTINCT c.id), '{}')
     FROM ci_attribute_definitions d
     JOIN lookup_list_values v ON v.list_id = d.lookup_list_id
     JOIN ci_classes c ON ci_class_is_a(c.id, d.class_id)
     WHERE v.id = $1 AND d.data_type = 'lookup'";
/// [`Usage::spans`] of a count over the CIs whose Criticality is value `$1`:
/// every class when the value is on the system list Criticality, none for a
/// value of any other list (no CI can hold it as its criticality).
pub const CRITICALITY_VALUE_SPANS: &str = "SELECT coalesce(array_agg(c.id), '{}') FROM ci_classes c
     WHERE EXISTS (SELECT 1 FROM lookup_list_values v JOIN lookup_lists l ON l.id = v.list_id
                   WHERE v.id = $1 AND l.system_role = 'criticality')";
/// [`Usage::spans`] of a count over the values of field `$1`: its type's table
/// holds the CIs of the type and of every type below it.
pub const ATTRIBUTE_SPANS: &str = "SELECT coalesce(array_agg(c.id), '{}')
     FROM ci_attribute_definitions d JOIN ci_classes c ON ci_class_is_a(c.id, d.class_id)
     WHERE d.id = $1";
/// [`Usage::spans`] of a count over the CIs of class `$1` itself.
pub const CLASS_SPANS: &str = "SELECT ARRAY[$1::uuid]";
/// [`Usage::spans`] of a count over relationships of any class (a relationship
/// may outlive the rule that allowed it).
pub const EVERY_CLASS_SPANS: &str = "SELECT coalesce(array_agg(id), '{}') FROM ci_classes WHERE $1::uuid IS NOT NULL";
/// [`Usage::spans`] of a count over the relationships rule `$1` covers: its
/// source and target classes with the types below them.
pub const RULE_SPANS: &str = "SELECT coalesce(array_agg(c.id), '{}')
     FROM relationship_type_rules r
     JOIN ci_classes c ON ci_class_is_a(c.id, r.source_class_id) OR ci_class_is_a(c.id, r.target_class_id)
     WHERE r.id = $1";

/// How many records of one kind refer to the row.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageCount {
    pub kind: String,
    pub label: String,
    /// Null when withheld (see `withheld`)
    #[schema(value_type = Option<i64>, required = true)]
    pub count: Count,
    /// True when the count spans CIs of a class the caller may not view: `count` is then null
    pub withheld: bool,
    /// A non-zero count prevents removing the row (the operation named by `removal`). A non-blocking count is removed together with the row.
    pub blocking: bool,
}

/// The operation that removes a record, which `inUse` and `blocking` refer to
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum Removal {
    /// DELETE removes the record
    Delete,
    /// DELETE only archives the record (isActive=false; nothing blocks that, and nothing is removed); POST …/purge removes it
    Purge,
}

/// What still refers to a record, so the UI can warn before a destructive change
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageReport {
    /// True when a blocking count is non-zero: the removal (see `removal`) would return 409 IN_USE (decided on every count, withheld ones included)
    pub in_use: bool,
    /// Which operation removes the record: `delete`, or `purge` for a type or field, which DELETE only archives
    pub removal: Removal,
    pub data: Vec<UsageCount>,
}

/// A list query: pagination, optional search, sort and resource-specific filters.
pub trait ListQuery: Paged + Send + Sync + 'static {
    /// The sortable fields (camelCase, each naming a column of the table).
    /// `list` rejects any other `sort` value before it reaches ORDER BY, so a
    /// query can never put its own SQL there, whatever the spec validation does.
    const SORT_FIELDS: &'static [&'static str];
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

    /// Set for resources whose data moved elsewhere: create, update and delete
    /// still need the write permission but then answer 410 GONE with this
    /// message (which names the replacement) and change nothing. Reads stay.
    const WRITES_GONE: Option<&'static str> = None;

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

/// The ORDER BY clause for a list: only a field in `allowed` gets there.
fn order_by(sort: &Sort, allowed: &[&str]) -> Result<String, AppError> {
    let Some(field) = allowed.iter().find(|f| **f == sort.field) else {
        return Err(AppError::validation(vec![FieldError {
            location: FieldLocation::Query,
            field: "sort".into(),
            message: format!("Invalid option: sort by one of {}", allowed.join("|")),
            code: "invalid_value".into(),
        }]));
    };
    Ok(format!("{} {}, id ASC", camel_to_snake(field), sort.dir()))
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
    let order = order_by(query.sort(), R::List::SORT_FIELDS)?;
    let (rows, total) = crud::select_page::<R::Dto>(
        &mut *pool.acquire().await?,
        R::TABLE,
        R::COLUMNS,
        &filter,
        &order,
        query.limit(),
        query.offset(),
    )
    .await?;
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

/// The usage counts and whether a blocking one is non-zero, i.e. whether the
/// removal (DELETE, or the purge of an archivable resource) is refused. `in_use` is decided
/// on every count; only what the caller may view is told ([`Usage::spans`]).
async fn usage_counts<R: Resource>(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    id: Uuid,
) -> Result<UsageReport, AppError> {
    let mut data = Vec::with_capacity(R::USAGE.len());
    let mut in_use = false;
    for u in R::USAGE {
        let n: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(u.sql)).bind(id).fetch_one(&mut *conn).await?;
        in_use |= u.blocking && n > 0;
        let count = match u.spans {
            None => Count::Exact(n),
            Some(spans) => {
                let classes: Vec<Uuid> =
                    sqlx::query_scalar(sqlx::AssertSqlSafe(spans)).bind(id).fetch_one(&mut *conn).await?;
                Count::scoped(ctx, &classes, n)
            }
        };
        data.push(UsageCount {
            kind: u.kind.into(),
            label: u.label.into(),
            count,
            withheld: count == Count::Withheld,
            blocking: u.blocking,
        });
    }
    let removal = if R::ARCHIVE_ON_DELETE { Removal::Purge } else { Removal::Delete };
    Ok(UsageReport { in_use, removal, data })
}

pub async fn usage<R: Resource>(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<UsageReport, AppError> {
    let mut conn = pool.acquire().await?;
    if crud::select_by_id::<R::Dto>(&mut conn, R::TABLE, R::COLUMNS, id, false).await?.is_none() {
        return Err(AppError::missing(R::LABEL, id));
    }
    usage_counts::<R>(&mut conn, ctx, id).await
}

/// 409 IN_USE naming every blocking reference, or Ok when the row can go.
/// A withheld count is neither told nor named: whether the message says
/// "details withheld" depends only on what the caller may view.
fn refuse_if_used(label: &str, report: &UsageReport) -> Result<(), AppError> {
    if !report.in_use {
        return Ok(());
    }
    let blocking: Vec<(&UsageCount, i64)> = report
        .data
        .iter()
        .filter(|u| u.blocking)
        .filter_map(|u| u.count.exact().filter(|n| *n > 0).map(|n| (u, n)))
        .collect();
    let withheld = report.data.iter().any(|u| u.blocking && u.withheld);
    let mut parts: Vec<String> = blocking.iter().map(|(u, n)| format!("{n} {}", u.label)).collect();
    if withheld {
        parts.push("records whose details are withheld (they span CI types you may not view)".into());
    }
    let message = if blocking.is_empty() {
        format!("{label} is still in use (details withheld). Retire it with isActive=false instead of deleting it.")
    } else {
        format!("{label} is still used by {}. Retire it with isActive=false instead of deleting it.", parts.join(", "))
    };
    Err(AppError::new(ErrorCode::InUse, message).with_details(
        blocking
            .into_iter()
            .map(|(u, n)| FieldError {
                location: FieldLocation::Params,
                field: u.kind.clone(),
                message: format!("{n} {}", u.label),
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
    refuse_if_used(R::LABEL, &usage_counts::<R>(tx, ctx, id).await?)?;
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
    ];
    match R::WRITES_GONE {
        None => routes.extend(write_routes::<R>(&label, &by_id)),
        Some(message) => routes.extend(gone_routes::<R>(&label, &by_id, message)),
    }
    if !R::USAGE.is_empty() {
        let kinds: Vec<&str> = R::USAGE.iter().map(|u| u.kind).collect();
        routes.push(
            route(Method::GET, format!("{}/{{id}}/usage", R::BASE_PATH), format!("get{}Usage", cap(R::SINGULAR)))
                .tag(R::TAG)
                .summary(format!("What still refers to a {label}"))
                .description(format!(
                    "Counts of {}. Check it before deleting or restructuring: {} A count over CIs is told only when the caller may view every CI class it can include; otherwise `count` is null and `withheld` true. `inUse` is decided on every count, withheld ones included.",
                    kinds.join(", "),
                    if R::ARCHIVE_ON_DELETE {
                        "DELETE only archives it, which no count blocks (`removal` is `purge`). A blocking count makes the purge return 409 IN_USE; the non-blocking ones are removed by the purge."
                    } else {
                        "a blocking count makes DELETE return 409 IN_USE (`removal` is `delete`); the non-blocking ones are removed with it."
                    }
                ))
                .requires(GlobalPermission::DatamodelManage)
                .errors(&[ErrorCode::NotFound])
                .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                    Ok(Json(usage::<R>(&api.pool, &api.ctx, id).await?))
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

fn write_routes<R: Resource>(label: &str, by_id: &str) -> Vec<Route> {
    vec![
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
        route(Method::PATCH, by_id, format!("update{}", cap(R::SINGULAR)))
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
    ]
}

/// Create, update and delete of a resource whose writes moved elsewhere ([`Resource::WRITES_GONE`]):
/// same paths and operation ids as before, so a client learns why instead of getting 404/405. They
/// need no permission: nothing can succeed, so every signed-in caller gets the 410 and its message
/// rather than a 403 that suggests a permission would help.
fn gone_routes<R: Resource>(label: &str, by_id: &str, message: &'static str) -> Vec<Route> {
    let gone = move || async move { Err::<NoContent, _>(AppError::new(ErrorCode::Gone, message)) };
    let removed = |method: Method, path: &str, op: String, summary: String| {
        route(method, path, op)
            .tag(R::TAG)
            .summary(summary)
            .description(message)
            .status(StatusCode::GONE)
            .errors(&[ErrorCode::Gone])
    };
    vec![
        removed(
            Method::POST,
            R::BASE_PATH,
            format!("create{}", cap(R::SINGULAR)),
            format!("Create a {label} (removed)"),
        )
        .handle(move |_, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| gone()),
        removed(Method::PATCH, by_id, format!("update{}", cap(R::SINGULAR)), format!("Update a {label} (removed)"))
            .handle(move |_, In(IdPath(_), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| gone()),
        removed(Method::DELETE, by_id, format!("delete{}", cap(R::SINGULAR)), format!("Delete a {label} (removed)"))
            .handle(move |_, In(IdPath(_), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| gone()),
    ]
}

/// `isActive=true|false` and similar boolean column filters.
pub fn bool_filter(w: &mut Where<'_>, column: &str, value: Option<crate::api::schemas::QueryBool>) {
    if let Some(v) = value {
        w.and().push(column).push(" = ").push_bind(bool::from(v));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sort(field: &str, desc: bool) -> Sort {
        Sort { field: field.into(), desc }
    }

    #[test]
    fn order_by_takes_only_listed_fields() {
        let allowed = &["sortOrder", "name"];
        assert_eq!(order_by(&sort("sortOrder", false), allowed).unwrap(), "sort_order ASC, id ASC");
        assert_eq!(order_by(&sort("name", true), allowed).unwrap(), "name DESC, id ASC");
        for bad in ["createdAt", "name; DROP TABLE statuses", "(SELECT 1)", ""] {
            let err = order_by(&sort(bad, false), allowed).unwrap_err();
            assert_eq!(err.code, ErrorCode::ValidationError, "{bad}");
            assert_eq!(err.details.unwrap()[0].field, "sort");
        }
    }
}
