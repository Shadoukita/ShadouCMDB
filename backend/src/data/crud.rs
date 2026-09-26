//! Data-access helpers shared by the resource repositories. Everything that
//! talks SQL lives under data/; services call these and never build HTTP
//! responses, routes call services and never touch the database.

use serde_json::Value;
use sqlx::postgres::PgRow;
use sqlx::{AssertSqlSafe, FromRow, PgConnection, PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::api::context::RequestContext;

// ---------------------------------------------------------------------------
// WHERE clauses shared by a page query and its count
// ---------------------------------------------------------------------------

/// Appends `WHERE a AND b ...` to a query as conditions are added.
pub struct Where<'q> {
    qb: &'q mut QueryBuilder<Postgres>,
    any: bool,
}

impl<'q> Where<'q> {
    pub fn new(qb: &'q mut QueryBuilder<Postgres>) -> Self {
        Where { qb, any: false }
    }

    /// Start a new condition; push its SQL and binds on the returned builder.
    pub fn and(&mut self) -> &mut QueryBuilder<Postgres> {
        self.qb.push(if self.any { " AND " } else { " WHERE " });
        self.any = true;
        self.qb
    }

    /// A condition without parameters.
    pub fn and_sql(&mut self, sql: &str) {
        self.and().push(sql);
    }
}

/// A filter callback: called once for the page query and once for its count.
pub type Filter<'f> = &'f (dyn for<'q> Fn(&mut Where<'q>) + Sync);

/// One page of rows plus the total matching the same filter (queried concurrently).
pub async fn select_page<T>(
    pool: &PgPool,
    from: &str,
    columns: &str,
    filter: Filter<'_>,
    order_by: &str,
    limit: i64,
    offset: i64,
) -> sqlx::Result<(Vec<T>, i64)>
where
    T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
{
    select_page_counted(pool, from, from, columns, filter, order_by, limit, offset).await
}

/// Like [`select_page`], counting over `count_from` (e.g. without joins the filter does not need).
#[allow(clippy::too_many_arguments)]
pub async fn select_page_counted<T>(
    pool: &PgPool,
    from: &str,
    count_from: &str,
    columns: &str,
    filter: Filter<'_>,
    order_by: &str,
    limit: i64,
    offset: i64,
) -> sqlx::Result<(Vec<T>, i64)>
where
    T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
{
    let mut rows = QueryBuilder::<Postgres>::new(format!("SELECT {columns} FROM {from}"));
    filter(&mut Where::new(&mut rows));
    rows.push(format!(" ORDER BY {order_by} LIMIT ")).push_bind(limit).push(" OFFSET ").push_bind(offset);

    let mut count = QueryBuilder::<Postgres>::new(format!("SELECT count(*) FROM {count_from}"));
    filter(&mut Where::new(&mut count));

    tokio::try_join!(rows.build_query_as::<T>().fetch_all(pool), count.build_query_scalar::<i64>().fetch_one(pool))
}

// ---------------------------------------------------------------------------
// Column values for INSERT / UPDATE
// ---------------------------------------------------------------------------

/// A value bound to one column.
#[derive(Debug, Clone)]
pub enum Val {
    Text(Option<String>),
    Int(Option<i32>),
    Bool(Option<bool>),
    Uuid(Option<Uuid>),
    Json(Option<Value>),
}

/// The columns a request body sets. For PATCH bodies, absent fields are left out.
#[derive(Debug, Default, Clone)]
pub struct ColumnSet(pub Vec<(&'static str, Val)>);

impl ColumnSet {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Converts an optional field into a column value, leaving absent fields out.
pub trait IntoVal {
    fn val(self) -> Val;
}
impl IntoVal for String {
    fn val(self) -> Val {
        Val::Text(Some(self))
    }
}
impl IntoVal for Option<String> {
    fn val(self) -> Val {
        Val::Text(self)
    }
}
impl IntoVal for i32 {
    fn val(self) -> Val {
        Val::Int(Some(self))
    }
}
impl IntoVal for bool {
    fn val(self) -> Val {
        Val::Bool(Some(self))
    }
}
impl IntoVal for Uuid {
    fn val(self) -> Val {
        Val::Uuid(Some(self))
    }
}
impl IntoVal for Option<Uuid> {
    fn val(self) -> Val {
        Val::Uuid(self)
    }
}
impl IntoVal for Value {
    fn val(self) -> Val {
        Val::Json(Some(self))
    }
}
impl IntoVal for Option<Value> {
    fn val(self) -> Val {
        Val::Json(self)
    }
}

impl ColumnSet {
    /// Set `column` when the field was present in the request.
    pub fn opt<V: IntoVal>(&mut self, column: &'static str, field: Option<V>) -> &mut Self {
        if let Some(v) = field {
            self.0.push((column, v.val()));
        }
        self
    }
}

fn push_val(qb: &mut QueryBuilder<Postgres>, v: Val) {
    match v {
        Val::Text(x) => qb.push_bind(x),
        Val::Int(x) => qb.push_bind(x),
        Val::Bool(x) => qb.push_bind(x),
        Val::Uuid(x) => qb.push_bind(x),
        Val::Json(x) => qb.push_bind(x),
    };
}

pub async fn select_by_id<T>(
    conn: &mut PgConnection,
    table: &str,
    columns: &str,
    id: Uuid,
    for_update: bool,
) -> sqlx::Result<Option<T>>
where
    T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
{
    let lock = if for_update { " FOR UPDATE" } else { "" };
    // Table and column lists are compile-time constants of the resource modules.
    sqlx::query_as(AssertSqlSafe(format!("SELECT {columns} FROM {table} WHERE id = $1{lock}")))
        .bind(id)
        .fetch_optional(conn)
        .await
}

/// Every row of a table, in `order_by` order (configuration export).
pub async fn select_all<T>(conn: &mut PgConnection, table: &str, columns: &str, order_by: &str) -> sqlx::Result<Vec<T>>
where
    T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
{
    sqlx::query_as(AssertSqlSafe(format!("SELECT {columns} FROM {table} ORDER BY {order_by}"))).fetch_all(conn).await
}

pub async fn insert_row<T>(conn: &mut PgConnection, table: &str, columns: &str, values: ColumnSet) -> sqlx::Result<T>
where
    T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
{
    let mut qb = QueryBuilder::<Postgres>::new(format!("INSERT INTO {table} ("));
    let names: Vec<&str> = values.0.iter().map(|(c, _)| *c).collect();
    qb.push(names.join(", ")).push(") VALUES (");
    for (i, (_, v)) in values.0.into_iter().enumerate() {
        if i > 0 {
            qb.push(", ");
        }
        push_val(&mut qb, v);
    }
    qb.push(format!(") RETURNING {columns}"));
    qb.build_query_as::<T>().fetch_one(conn).await
}

pub async fn update_row<T>(
    conn: &mut PgConnection,
    table: &str,
    columns: &str,
    id: Uuid,
    values: ColumnSet,
) -> sqlx::Result<T>
where
    T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
{
    let mut qb = QueryBuilder::<Postgres>::new(format!("UPDATE {table} SET "));
    for (i, (column, v)) in values.0.into_iter().enumerate() {
        if i > 0 {
            qb.push(", ");
        }
        qb.push(column).push(" = ");
        push_val(&mut qb, v);
    }
    qb.push(" WHERE id = ").push_bind(id).push(format!(" RETURNING {columns}"));
    qb.build_query_as::<T>().fetch_one(conn).await
}

pub async fn delete_row(conn: &mut PgConnection, table: &str, id: Uuid) -> sqlx::Result<()> {
    sqlx::query(AssertSqlSafe(format!("DELETE FROM {table} WHERE id = $1"))).bind(id).execute(conn).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Audit
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum AuditAction {
    Create,
    Update,
    Delete,
    // Reserved: undelete is not an API operation yet.
    Restore,
}

impl AuditAction {
    pub fn as_str(self) -> &'static str {
        match self {
            AuditAction::Create => "create",
            AuditAction::Update => "update",
            AuditAction::Delete => "delete",
            AuditAction::Restore => "restore",
        }
    }
}

pub struct AuditEntry {
    pub action: AuditAction,
    pub entity_type: &'static str,
    pub entity_id: Uuid,
    /// API representation before the change (None for create).
    pub old_value: Option<Value>,
    /// API representation after the change (None for delete).
    pub new_value: Option<Value>,
}

/// Append audit rows in the caller's transaction so a change and its audit commit together.
pub async fn write_audit(conn: &mut PgConnection, ctx: &RequestContext, entries: Vec<AuditEntry>) -> sqlx::Result<()> {
    for e in entries {
        sqlx::query!(
            "INSERT INTO audit_log (actor_type, actor_id, actor_name, action, entity_type, entity_id, old_value, new_value, request_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            ctx.actor.actor_type.as_str(),
            ctx.actor.id,
            ctx.actor.name,
            e.action.as_str(),
            e.entity_type,
            e.entity_id,
            e.old_value,
            e.new_value,
            ctx.request_id,
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Serialise an API representation for audit_log.
pub fn json<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}
