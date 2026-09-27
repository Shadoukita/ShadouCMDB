//! SQL for configuration items: inventory list, detail, global search, field
//! values in the per-type tables and relationship-graph expansion.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use ipnetwork::IpNetwork;
use serde_json::Value;
use sqlx::types::Json;
use sqlx::{AssertSqlSafe, PgConnection, PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

use super::crud::{self, Where};
use crate::api::schemas::{Deleted, OwnerKind, escape_like, like_pattern};
use crate::api::validate;
use crate::modules::classes::AttributeDataType;
use crate::schema::model::{Field, Model, TableName, pg_type};
use crate::schema::naming::Ident;

// ---------------------------------------------------------------------------
// Summary rows (CI + embedded class / status / environment / owner / location)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SummaryRow {
    pub id: Uuid,
    pub name: String,
    pub class_id: Uuid,
    pub status_id: Uuid,
    pub environment_id: Option<Uuid>,
    pub owner_id: Option<Uuid>,
    pub location_id: Option<Uuid>,
    pub hostname: Option<String>,
    pub ip_address: Option<IpNetwork>,
    pub serial_number: Option<String>,
    pub notes: Option<String>,
    pub version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub class_key: String,
    pub class_name: String,
    pub status_key: String,
    pub status_name: String,
    pub environment_key: Option<String>,
    pub environment_name: Option<String>,
    pub owner_name: Option<String>,
    pub owner_kind: Option<OwnerKind>,
    pub location_key: Option<String>,
    pub location_name: Option<String>,
}

const SUMMARY_COLUMNS: &str = "ci.id, ci.name, ci.class_id, ci.status_id, ci.environment_id, ci.owner_id,
    ci.location_id, ci.hostname, ci.ip_address, ci.serial_number, ci.notes, ci.version,
    ci.created_at, ci.updated_at, ci.deleted_at,
    cls.key AS class_key, cls.name AS class_name, st.key AS status_key, st.name AS status_name,
    env.key AS environment_key, env.name AS environment_name,
    own.name AS owner_name, own.kind AS owner_kind, loc.key AS location_key, loc.name AS location_name";

const SUMMARY_FROM: &str = "configuration_items ci
    JOIN ci_classes cls ON cls.id = ci.class_id
    JOIN statuses st ON st.id = ci.status_id
    LEFT JOIN environments env ON env.id = ci.environment_id
    LEFT JOIN owners own ON own.id = ci.owner_id
    LEFT JOIN locations loc ON loc.id = ci.location_id";

/// Filters reference only `ci.*`, so counting needs no joins.
const COUNT_FROM: &str = "configuration_items ci";

pub const SORT_FIELDS: &[&str] =
    &["name", "hostname", "ipAddress", "serialNumber", "className", "statusName", "createdAt", "updatedAt"];

fn sort_column(field: &str) -> &'static str {
    match field {
        "hostname" => "lower(ci.hostname)",
        "ipAddress" => "ci.ip_address",
        "serialNumber" => "ci.serial_number",
        "className" => "lower(cls.name)",
        "statusName" => "st.sort_order",
        "createdAt" => "ci.created_at",
        "updatedAt" => "ci.updated_at",
        _ => "lower(ci.name)",
    }
}

/// PostgreSQL's text form of an inet: the mask only when it is not a single host.
pub fn inet_text(n: &IpNetwork) -> String {
    let host_bits = if n.is_ipv4() { 32 } else { 128 };
    if n.prefix() == host_bits { n.ip().to_string() } else { format!("{}/{}", n.ip(), n.prefix()) }
}

#[derive(Debug, Clone, Default)]
pub struct ItemFilters {
    pub q: Option<String>,
    pub class_ids: Option<Vec<Uuid>>,
    pub status_ids: Option<Vec<Uuid>>,
    pub environment_ids: Option<Vec<Uuid>>,
    pub owner_ids: Option<Vec<Uuid>>,
    pub location_ids: Option<Vec<Uuid>>,
    pub ip_within: Option<String>,
    pub deleted: Option<Deleted>,
    /// Classes the caller may view; `None` means every class.
    pub visible_class_ids: Option<Vec<Uuid>>,
    /// Type tables searched by `q` (see [`search_tables`]).
    pub search_tables: Vec<SearchTable>,
}

/// Words of a query turned into a prefix tsquery ("web prod" -> 'web:* & prod:*').
pub fn query_words(q: &str) -> Vec<String> {
    q.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_owned).collect()
}

/// The search predicate shared by the inventory list and global search: name,
/// hostname and serial (substring, trigram-indexed), notes (word prefix, via the
/// tsvector), IP address (prefix, or containment when q is an IP/CIDR) and
/// field values in the type tables (text/enum substring, IP/CIDR prefix).
fn push_search(w: &mut Where<'_>, q: &str, tables: &[SearchTable]) {
    let pattern = like_pattern(q);
    let prefix = format!("{}%", escape_like(q));
    let qb = w.and();
    qb.push("(ci.name ILIKE ").push_bind(pattern.clone());
    qb.push(" OR ci.hostname ILIKE ").push_bind(pattern.clone());
    qb.push(" OR ci.serial_number ILIKE ").push_bind(pattern.clone());
    qb.push(" OR host(ci.ip_address) LIKE ").push_bind(prefix.clone());
    // Field values: one branch per type table with searchable columns.
    if !tables.is_empty() {
        qb.push(" OR ci.id IN (");
        for (i, t) in tables.iter().enumerate() {
            if i > 0 {
                qb.push(" UNION ALL ");
            }
            qb.push(format!("SELECT id FROM {} WHERE false", t.table.sql()));
            for c in &t.text {
                qb.push(format!(" OR {c} ILIKE ")).push_bind(pattern.clone());
            }
            for c in &t.ip {
                qb.push(format!(" OR host({c}) LIKE ")).push_bind(prefix.clone());
            }
            for c in &t.cidr {
                qb.push(format!(" OR {c}::text LIKE ")).push_bind(prefix.clone());
            }
        }
        qb.push(")");
    }
    let words = query_words(q);
    if !words.is_empty() {
        let tsq: Vec<String> = words.iter().take(8).map(|w| format!("{w}:*")).collect();
        qb.push(" OR ci.search_vector @@ to_tsquery('simple', ").push_bind(tsq.join(" & ")).push(")");
    }
    if validate::is_ip_or_cidr(q) {
        qb.push(" OR ci.ip_address <<= ").push_bind(q.to_owned()).push("::inet");
    }
    qb.push(")");
}

fn push_filters(w: &mut Where<'_>, f: &ItemFilters) {
    if let Some(p) = f.deleted.and_then(|d| d.predicate("ci.deleted_at")) {
        w.and_sql(&p);
    }
    if let Some(q) = &f.q {
        push_search(w, q, &f.search_tables);
    }
    for (column, ids) in [
        ("ci.class_id", &f.class_ids),
        ("ci.class_id", &f.visible_class_ids),
        ("ci.status_id", &f.status_ids),
        ("ci.environment_id", &f.environment_ids),
        ("ci.owner_id", &f.owner_ids),
        ("ci.location_id", &f.location_ids),
    ] {
        if let Some(ids) = ids {
            w.and().push(column).push(" = ANY(").push_bind(ids.clone()).push(")");
        }
    }
    if let Some(cidr) = &f.ip_within {
        w.and().push("ci.ip_address <<= ").push_bind(cidr.clone()).push("::inet");
    }
}

pub async fn list(
    pool: &PgPool,
    f: &ItemFilters,
    sort_field: &str,
    desc: bool,
    limit: i64,
    offset: i64,
) -> sqlx::Result<(Vec<SummaryRow>, i64)> {
    let dir = if desc { "DESC" } else { "ASC" };
    let order = format!("{} {dir} NULLS LAST, ci.id ASC", sort_column(sort_field));
    let filter = |w: &mut Where<'_>| push_filters(w, f);
    crud::select_page_counted(pool, SUMMARY_FROM, COUNT_FROM, SUMMARY_COLUMNS, &filter, &order, limit, offset).await
}

/// Global search: same predicate as the list, ranked by exact / prefix / trigram similarity.
pub async fn search(
    pool: &PgPool,
    q: &str,
    f: &ItemFilters,
    limit: i64,
    offset: i64,
) -> sqlx::Result<(Vec<SummaryRow>, i64)> {
    let f = ItemFilters { q: Some(q.to_owned()), ..f.clone() };
    let filter = |w: &mut Where<'_>| push_filters(w, &f);

    let lower = q.to_lowercase();
    let mut rows = QueryBuilder::<Postgres>::new(format!("SELECT {SUMMARY_COLUMNS} FROM {SUMMARY_FROM}"));
    filter(&mut Where::new(&mut rows));
    rows.push(" ORDER BY (lower(ci.name) = ")
        .push_bind(lower.clone())
        .push(" OR lower(ci.hostname) = ")
        .push_bind(lower.clone())
        .push(" OR lower(ci.serial_number) = ")
        .push_bind(lower.clone())
        .push(" OR host(ci.ip_address) = ")
        .push_bind(q.to_owned())
        .push(") DESC NULLS LAST, (lower(ci.name) LIKE ")
        .push_bind(format!("{}%", escape_like(&lower)))
        .push(") DESC NULLS LAST, greatest(similarity(ci.name, ")
        .push_bind(q.to_owned())
        .push("), similarity(coalesce(ci.hostname, ''), ")
        .push_bind(q.to_owned())
        .push("), similarity(coalesce(ci.serial_number, ''), ")
        .push_bind(q.to_owned())
        .push(")) DESC, lower(ci.name) ASC, ci.id ASC LIMIT ")
        .push_bind(limit)
        .push(" OFFSET ")
        .push_bind(offset);

    let mut count = QueryBuilder::<Postgres>::new(format!("SELECT count(*) FROM {COUNT_FROM}"));
    filter(&mut Where::new(&mut count));

    tokio::try_join!(
        rows.build_query_as::<SummaryRow>().fetch_all(pool),
        count.build_query_scalar::<i64>().fetch_one(pool)
    )
}

pub async fn summaries(conn: &mut PgConnection, ids: &[Uuid]) -> sqlx::Result<Vec<SummaryRow>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as(AssertSqlSafe(format!("SELECT {SUMMARY_COLUMNS} FROM {SUMMARY_FROM} WHERE ci.id = ANY($1)")))
        .bind(ids)
        .fetch_all(conn)
        .await
}

pub async fn summary(conn: &mut PgConnection, id: Uuid) -> sqlx::Result<Option<SummaryRow>> {
    sqlx::query_as(AssertSqlSafe(format!("SELECT {SUMMARY_COLUMNS} FROM {SUMMARY_FROM} WHERE ci.id = $1")))
        .bind(id)
        .fetch_optional(conn)
        .await
}

// ---------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------

/// Core columns of a CI as written by create.
pub struct NewItem<'a> {
    pub class_id: Uuid,
    pub name: &'a str,
    pub status_id: Uuid,
    pub environment_id: Option<Uuid>,
    pub owner_id: Option<Uuid>,
    pub location_id: Option<Uuid>,
    pub hostname: Option<&'a str>,
    pub ip_address: Option<&'a str>,
    pub serial_number: Option<&'a str>,
    pub notes: Option<&'a str>,
}

pub async fn insert(conn: &mut PgConnection, ci: &NewItem<'_>) -> sqlx::Result<Uuid> {
    sqlx::query_scalar!(
        "INSERT INTO configuration_items
           (class_id, name, status_id, environment_id, owner_id, location_id, hostname, ip_address, serial_number, notes)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8::text::inet, $9, $10)
         RETURNING id",
        ci.class_id,
        ci.name,
        ci.status_id,
        ci.environment_id,
        ci.owner_id,
        ci.location_id,
        ci.hostname,
        ci.ip_address,
        ci.serial_number,
        ci.notes
    )
    .fetch_one(conn)
    .await
}

/// PATCH of the core columns: `None` keeps a column, `Some(None)` clears a nullable one.
#[derive(Default)]
pub struct ItemPatch<'a> {
    pub class_id: Option<Uuid>,
    pub name: Option<&'a str>,
    pub status_id: Option<Uuid>,
    pub environment_id: Option<Option<Uuid>>,
    pub owner_id: Option<Option<Uuid>>,
    pub location_id: Option<Option<Uuid>>,
    pub hostname: Option<Option<&'a str>>,
    pub ip_address: Option<Option<&'a str>>,
    pub serial_number: Option<Option<&'a str>>,
    pub notes: Option<Option<&'a str>>,
}

/// Applies the patch and bumps the optimistic-locking version.
pub async fn update(conn: &mut PgConnection, id: Uuid, p: &ItemPatch<'_>) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE configuration_items SET
           class_id = COALESCE($2, class_id),
           name = COALESCE($3, name),
           status_id = COALESCE($4, status_id),
           environment_id = CASE WHEN $5 THEN $6 ELSE environment_id END,
           owner_id = CASE WHEN $7 THEN $8 ELSE owner_id END,
           location_id = CASE WHEN $9 THEN $10 ELSE location_id END,
           hostname = CASE WHEN $11 THEN $12 ELSE hostname END,
           ip_address = CASE WHEN $13 THEN $14::text::inet ELSE ip_address END,
           serial_number = CASE WHEN $15 THEN $16 ELSE serial_number END,
           notes = CASE WHEN $17 THEN $18 ELSE notes END,
           version = version + 1
         WHERE id = $1",
        id,
        p.class_id,
        p.name,
        p.status_id,
        p.environment_id.is_some(),
        p.environment_id.flatten(),
        p.owner_id.is_some(),
        p.owner_id.flatten(),
        p.location_id.is_some(),
        p.location_id.flatten(),
        p.hostname.is_some(),
        p.hostname.flatten(),
        p.ip_address.is_some(),
        p.ip_address.flatten(),
        p.serial_number.is_some(),
        p.serial_number.flatten(),
        p.notes.is_some(),
        p.notes.flatten()
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Locks the CI row; Some((class_id, version, deleted_at)) when it exists.
pub struct Locked {
    pub class_id: Uuid,
    pub version: i32,
    pub deleted_at: Option<DateTime<Utc>>,
}

pub async fn lock(conn: &mut PgConnection, id: Uuid) -> sqlx::Result<Option<Locked>> {
    sqlx::query_as!(
        Locked,
        "SELECT class_id, version, deleted_at FROM configuration_items WHERE id = $1 FOR UPDATE",
        id
    )
    .fetch_optional(conn)
    .await
}

pub async fn soft_delete(conn: &mut PgConnection, id: Uuid) -> sqlx::Result<()> {
    sqlx::query!("UPDATE configuration_items SET deleted_at = now(), version = version + 1 WHERE id = $1", id)
        .execute(conn)
        .await?;
    Ok(())
}

/// A relationship row as stored, for the audit entries of a cascaded delete.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeRecord {
    pub id: Uuid,
    pub relationship_type_id: Uuid,
    pub source_ci_id: Uuid,
    pub target_ci_id: Uuid,
    pub notes: Option<String>,
    #[serde(serialize_with = "crate::api::schemas::ts::serialize")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "crate::api::schemas::ts::serialize")]
    pub updated_at: DateTime<Utc>,
    /// Null after a soft delete: the audit entry records the edge as it was
    /// before the delete. A purge records the stored value.
    #[serde(serialize_with = "crate::api::schemas::ts_opt::serialize")]
    pub deleted_at: Option<DateTime<Utc>>,
}

/// Delete up to `limit` edges of these CIs, soft-deleted ones included (type
/// purge); returns the removed edges for auditing. Call until it returns fewer
/// than `limit`, so a large type never returns all its edges in one statement.
pub async fn delete_edges_of(conn: &mut PgConnection, ci_ids: &[Uuid], limit: i64) -> sqlx::Result<Vec<EdgeRecord>> {
    sqlx::query_as!(
        EdgeRecord,
        r#"DELETE FROM ci_relationships
           WHERE id IN (SELECT id FROM ci_relationships
                        WHERE source_ci_id = ANY($1) OR target_ci_id = ANY($1)
                        LIMIT $2)
           RETURNING id, relationship_type_id, source_ci_id, target_ci_id, notes, created_at, updated_at, deleted_at"#,
        ci_ids,
        limit
    )
    .fetch_all(conn)
    .await
}

/// Soft-delete every live edge of a CI; returns the removed edges for auditing.
pub async fn soft_delete_edges_of(conn: &mut PgConnection, ci_id: Uuid) -> sqlx::Result<Vec<EdgeRecord>> {
    sqlx::query_as!(
        EdgeRecord,
        r#"UPDATE ci_relationships SET deleted_at = now()
           WHERE deleted_at IS NULL AND (source_ci_id = $1 OR target_ci_id = $1)
           RETURNING id, relationship_type_id, source_ci_id, target_ci_id, notes, created_at, updated_at,
                     NULL::timestamptz AS deleted_at"#,
        ci_id
    )
    .fetch_all(conn)
    .await
}

// ---------------------------------------------------------------------------
// Field values in the type tables (class table inheritance: a CI has one row
// in the table of its class and of every ancestor class)
// ---------------------------------------------------------------------------

/// A stored field value of one CI, in the API's JSON shape.
#[derive(Debug, Clone)]
pub struct ItemValue {
    pub ci_id: Uuid,
    pub key: String,
    pub label: String,
    pub data_type: AttributeDataType,
    pub sort_order: i32,
    pub value: Value,
}

impl ItemValue {
    /// Text, enum, IP and CIDR values as searchable text.
    pub fn search_text(&self) -> Option<&str> {
        match self.data_type {
            AttributeDataType::Text | AttributeDataType::Enum | AttributeDataType::Ip | AttributeDataType::Cidr => {
                self.value.as_str()
            }
            _ => None,
        }
    }

    pub fn reference(&self) -> Option<Uuid> {
        (self.data_type == AttributeDataType::Reference)
            .then(|| self.value.as_str().and_then(|v| Uuid::parse_str(v).ok()))
            .flatten()
    }
}

fn number_json(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < 9.0e15 {
        Value::from(n as i64)
    } else {
        serde_json::Number::from_f64(n).map(Value::Number).unwrap_or(Value::Null)
    }
}

/// A column value as to_jsonb() gives it, in the shape the API uses.
fn api_value(t: AttributeDataType, v: &Value) -> Value {
    match (t, v) {
        (AttributeDataType::Number | AttributeDataType::Integer, Value::Number(n)) => {
            n.as_f64().map(number_json).unwrap_or(Value::Null)
        }
        (AttributeDataType::Datetime, Value::String(s)) => DateTime::parse_from_rfc3339(s)
            .map(|d| Value::String(crate::api::schemas::iso(&d.with_timezone(&Utc))))
            .unwrap_or_else(|_| v.clone()),
        _ => v.clone(),
    }
}

/// Every CI id of these classes, deleted ones included.
pub async fn ids_of_classes(conn: &mut PgConnection, class_ids: &[Uuid]) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar("SELECT id FROM cmdb.configuration_items WHERE class_id = ANY($1)")
        .bind(class_ids)
        .fetch_all(conn)
        .await
}

/// Stored values of the given CIs (any mix of classes), ordered like the form.
pub async fn values(conn: &mut PgConnection, model: &Model, ci_ids: &[Uuid]) -> sqlx::Result<Vec<ItemValue>> {
    if ci_ids.is_empty() {
        return Ok(Vec::new());
    }
    let classes: Vec<(Uuid, Uuid)> =
        sqlx::query_as("SELECT id, class_id FROM cmdb.configuration_items WHERE id = ANY($1)")
            .bind(ci_ids)
            .fetch_all(&mut *conn)
            .await?;
    // table class -> CIs that have a row in its table
    let mut by_table: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for (ci, class) in &classes {
        for c in model.lineage(*class) {
            by_table.entry(c.id).or_default().push(*ci);
        }
    }
    let mut out = Vec::new();
    for (class_id, ids) in by_table {
        let fields: Vec<_> = model.own_fields(class_id).collect();
        let Some(table) = model.table(class_id) else { continue };
        if fields.is_empty() {
            continue;
        }
        let rows: Vec<(Uuid, Json<serde_json::Map<String, Value>>)> =
            sqlx::query_as(AssertSqlSafe(format!("SELECT id, to_jsonb(t) FROM {} t WHERE id = ANY($1)", table.sql())))
                .bind(&ids)
                .persistent(false)
                .fetch_all(&mut *conn)
                .await?;
        for (ci_id, Json(row)) in rows {
            for f in &fields {
                match row.get(&f.key) {
                    None | Some(Value::Null) => {}
                    Some(v) => out.push(ItemValue {
                        ci_id,
                        key: f.key.clone(),
                        label: f.label.clone(),
                        data_type: f.data_type,
                        sort_order: f.sort_order,
                        value: api_value(f.data_type, v),
                    }),
                }
            }
        }
    }
    out.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then_with(|| a.key.cmp(&b.key)));
    Ok(out)
}

/// A referenced CI: its name, whether it is deleted, and its class (callers
/// redact references into classes the reader may not view).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ReferencedItem {
    pub id: Uuid,
    pub name: String,
    pub deleted: bool,
    pub class_id: Uuid,
}

/// The referenced CIs, by id.
pub async fn reference_names(conn: &mut PgConnection, ids: &[Uuid]) -> sqlx::Result<HashMap<Uuid, ReferencedItem>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<ReferencedItem> = sqlx::query_as(
        "SELECT id, name, deleted_at IS NOT NULL AS deleted, class_id FROM cmdb.configuration_items WHERE id = ANY($1)",
    )
    .bind(ids)
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(|r| (r.id, r)).collect())
}

/// One typed value, bound as text and cast to the column type.
#[derive(Debug, Clone)]
pub enum StoredValue {
    Text(String),
    Number(f64),
    Boolean(bool),
    Date(String),
    Datetime(String),
    Ip(String),
    Cidr(String),
    Reference(Uuid),
    Lookup(Uuid),
}

impl StoredValue {
    pub fn as_text(&self) -> String {
        match self {
            StoredValue::Text(v)
            | StoredValue::Date(v)
            | StoredValue::Datetime(v)
            | StoredValue::Ip(v)
            | StoredValue::Cidr(v) => v.clone(),
            StoredValue::Number(n) => n.to_string(),
            StoredValue::Boolean(b) => b.to_string(),
            StoredValue::Reference(id) | StoredValue::Lookup(id) => id.to_string(),
        }
    }
}

/// `$n::text::<column type>` for each value (None clears the column).
fn cast(n: usize, f: &Field) -> String {
    format!("${n}::text::{}", pg_type(f.data_type))
}

/// The CI's row in one type table, with these column values.
pub async fn insert_type_row(
    conn: &mut PgConnection,
    table: &TableName,
    id: Uuid,
    values: &[(&Field, Option<String>)],
) -> sqlx::Result<()> {
    let mut cols = vec!["id".to_owned()];
    let mut params = vec!["$1".to_owned()];
    for (i, (f, _)) in values.iter().enumerate() {
        cols.push(f.column().to_string());
        params.push(cast(i + 2, f));
    }
    let sql = format!("INSERT INTO {} ({}) VALUES ({})", table.sql(), cols.join(", "), params.join(", "));
    let mut q = sqlx::query(AssertSqlSafe(sql)).persistent(false).bind(id);
    for (_, v) in values {
        q = q.bind(v.clone());
    }
    q.execute(conn).await?;
    Ok(())
}

pub async fn update_type_row(
    conn: &mut PgConnection,
    table: &TableName,
    id: Uuid,
    values: &[(&Field, Option<String>)],
) -> sqlx::Result<()> {
    if values.is_empty() {
        return Ok(());
    }
    let sets: Vec<String> =
        values.iter().enumerate().map(|(i, (f, _))| format!("{} = {}", f.column(), cast(i + 2, f))).collect();
    let sql = format!("UPDATE {} SET {} WHERE id = $1", table.sql(), sets.join(", "));
    let mut q = sqlx::query(AssertSqlSafe(sql)).persistent(false).bind(id);
    for (_, v) in values {
        q = q.bind(v.clone());
    }
    q.execute(conn).await?;
    Ok(())
}

/// Rows (id only) for CIs that move into a type's lineage.
pub async fn insert_type_rows(conn: &mut PgConnection, table: &TableName, ids: &[Uuid]) -> sqlx::Result<()> {
    sqlx::query(AssertSqlSafe(format!("INSERT INTO {} (id) SELECT unnest($1::uuid[])", table.sql())))
        .persistent(false)
        .bind(ids)
        .execute(conn)
        .await?;
    Ok(())
}

pub async fn delete_type_rows(conn: &mut PgConnection, table: &TableName, ids: &[Uuid]) -> sqlx::Result<()> {
    sqlx::query(AssertSqlSafe(format!("DELETE FROM {} WHERE id = ANY($1)", table.sql())))
        .persistent(false)
        .bind(ids)
        .execute(conn)
        .await?;
    Ok(())
}

/// Of these fields of one table, those holding a value for any of the CIs.
pub async fn fields_with_values(
    conn: &mut PgConnection,
    table: &TableName,
    fields: &[&str],
    ids: &[Uuid],
) -> sqlx::Result<Vec<String>> {
    let mut used = Vec::new();
    for f in fields {
        let col = crate::schema::naming::Ident::trusted(f);
        let any: bool = sqlx::query_scalar(AssertSqlSafe(format!(
            "SELECT EXISTS (SELECT 1 FROM {} WHERE id = ANY($1) AND {col} IS NOT NULL)",
            table.sql()
        )))
        .persistent(false)
        .bind(ids)
        .fetch_one(&mut *conn)
        .await?;
        if any {
            used.push((*f).to_owned());
        }
    }
    Ok(used)
}

/// A type table's searchable columns (text, enum: substring; ip, cidr: prefix).
#[derive(Debug, Clone)]
pub struct SearchTable {
    pub table: TableName,
    pub text: Vec<Ident>,
    pub ip: Vec<Ident>,
    pub cidr: Vec<Ident>,
}

/// Every table with searchable fields.
pub fn search_tables(model: &Model) -> Vec<SearchTable> {
    let mut out = Vec::new();
    for c in &model.classes {
        let Some(table) = model.table(c.id) else { continue };
        let mut t = SearchTable { table, text: Vec::new(), ip: Vec::new(), cidr: Vec::new() };
        for f in model.own_fields(c.id) {
            match f.data_type {
                AttributeDataType::Text | AttributeDataType::Enum => t.text.push(f.column()),
                AttributeDataType::Ip => t.ip.push(f.column()),
                AttributeDataType::Cidr => t.cidr.push(f.column()),
                _ => {}
            }
        }
        if !(t.text.is_empty() && t.ip.is_empty() && t.cidr.is_empty()) {
            out.push(t);
        }
    }
    out
}

/// Of the given CI ids, those that exist and are not deleted, with their class.
pub async fn live_items(conn: &mut PgConnection, ids: &[Uuid]) -> sqlx::Result<Vec<(Uuid, Uuid)>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as("SELECT id, class_id FROM cmdb.configuration_items WHERE id = ANY($1) AND deleted_at IS NULL")
        .bind(ids)
        .fetch_all(conn)
        .await
}

// ---------------------------------------------------------------------------
// Relationship graph
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct EdgeRow {
    pub id: Uuid,
    pub relationship_type_id: Uuid,
    pub source_ci_id: Uuid,
    pub target_ci_id: Uuid,
    pub notes: Option<String>,
    pub type_key: String,
    pub type_name: String,
    pub forward_label: String,
    pub reverse_label: String,
    pub is_directional: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Both,
    Outgoing,
    Incoming,
}

/// Live edges touching any of the given CIs, in the requested direction(s).
/// Symmetric types (connected_to) are followed both ways whatever the direction.
/// With `visible_class_ids`, only edges whose both endpoints are in those classes.
pub async fn edges_touching(
    pool: &PgPool,
    ci_ids: &[Uuid],
    direction: Direction,
    type_ids: Option<&[Uuid]>,
    visible_class_ids: Option<&[Uuid]>,
) -> sqlx::Result<Vec<EdgeRow>> {
    if ci_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut qb = QueryBuilder::<Postgres>::new(
        "SELECT r.id, r.relationship_type_id, r.source_ci_id, r.target_ci_id, r.notes,
                t.key AS type_key, t.name AS type_name, t.forward_label, t.reverse_label, t.is_directional
         FROM ci_relationships r JOIN relationship_types t ON t.id = r.relationship_type_id
         WHERE r.deleted_at IS NULL AND ",
    );
    let ids = ci_ids.to_vec();
    match direction {
        Direction::Both => qb
            .push("(r.source_ci_id = ANY(")
            .push_bind(ids.clone())
            .push(") OR r.target_ci_id = ANY(")
            .push_bind(ids)
            .push("))"),
        Direction::Outgoing => qb
            .push("(r.source_ci_id = ANY(")
            .push_bind(ids.clone())
            .push(") OR (NOT t.is_directional AND r.target_ci_id = ANY(")
            .push_bind(ids)
            .push(")))"),
        Direction::Incoming => qb
            .push("(r.target_ci_id = ANY(")
            .push_bind(ids.clone())
            .push(") OR (NOT t.is_directional AND r.source_ci_id = ANY(")
            .push_bind(ids)
            .push(")))"),
    };
    if let Some(types) = type_ids {
        qb.push(" AND r.relationship_type_id = ANY(").push_bind(types.to_vec()).push(")");
    }
    if let Some(classes) = visible_class_ids {
        qb.push(" AND (SELECT s.class_id FROM configuration_items s WHERE s.id = r.source_ci_id) = ANY(")
            .push_bind(classes.to_vec())
            .push(") AND (SELECT g.class_id FROM configuration_items g WHERE g.id = r.target_ci_id) = ANY(")
            .push_bind(classes.to_vec())
            .push(")");
    }
    qb.push(" ORDER BY r.created_at, r.id");
    qb.build_query_as::<EdgeRow>().fetch_all(pool).await
}
