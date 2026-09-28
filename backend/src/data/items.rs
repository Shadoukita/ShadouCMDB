//! SQL for configuration items: inventory list, detail, global search, field
//! values in the per-type tables and relationship-graph expansion.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::types::Json;
use sqlx::{AssertSqlSafe, PgConnection, PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

use super::crud::{self, Where};
use crate::api::schemas::{Deleted, escape_like, like_pattern};
use crate::api::validate;
use crate::modules::classes::AttributeDataType;
use crate::schema::ACTIVE_SQL;
use crate::schema::model::{Field, Model, TableName, pg_type};
use crate::schema::naming::Ident;

// ---------------------------------------------------------------------------
// Summary rows (registry columns + embedded class)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SummaryRow {
    pub id: Uuid,
    pub ident: String,
    pub label: String,
    pub class_id: Uuid,
    pub valid_from: DateTime<Utc>,
    pub valid_until: Option<DateTime<Utc>>,
    pub active: bool,
    pub version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub class_key: String,
    pub class_name: String,
}

fn summary_columns() -> String {
    format!(
        "ci.id, ci.ident, ci.label, ci.class_id, ci.valid_from, ci.valid_until, {ACTIVE_SQL} AS active, ci.version,
         ci.created_at, ci.updated_at, ci.deleted_at, cls.key AS class_key, cls.name AS class_name"
    )
}

const SUMMARY_FROM: &str = "configuration_items ci JOIN ci_classes cls ON cls.id = ci.class_id";

/// Filters reference only `ci.*` and the type tables, so counting needs no joins.
const COUNT_FROM: &str = "configuration_items ci";

pub const SORT_FIELDS: &[&str] = &["label", "ident", "className", "validFrom", "validUntil", "createdAt", "updatedAt"];

/// `sort=attributes.<key>` sorts on an attribute (see [`ListSort::Attribute`]).
pub const ATTRIBUTE_SORT_PREFIX: &str = "attributes.";

/// A sort parameter: a core field or an attribute, "-" for descending.
pub const SORT_PATTERN: &str =
    "^-?(label|ident|className|validFrom|validUntil|createdAt|updatedAt|attributes\\.[a-z][a-z0-9_]{0,62})$";

fn sort_column(field: &str) -> &'static str {
    match field {
        "ident" => "lower(ci.ident)",
        "className" => "lower(cls.name)",
        "validFrom" => "ci.valid_from",
        "validUntil" => "ci.valid_until",
        "createdAt" => "ci.created_at",
        "updatedAt" => "ci.updated_at",
        _ => "lower(ci.label)",
    }
}

/// The order of the inventory list.
#[derive(Debug, Clone)]
pub enum ListSort<'a> {
    /// One of [`SORT_FIELDS`]
    Core(&'a str),
    /// An attribute, in the table of the class that defines it: every listed
    /// CI has a row there (see [`values`]).
    Attribute(TableName, &'a Field),
}

/// Attribute data types the list can sort on. A reference would order by the
/// label of a CI the reader may not be allowed to see.
pub fn is_sortable(t: AttributeDataType) -> bool {
    t != AttributeDataType::Reference
}

/// Joins added to [`SUMMARY_FROM`] and the ORDER BY terms. Text sorts
/// case-insensitively, IP and CIDR by address (inet order), a lookup by the
/// list's value order, then name. Ties and CIs without a value (last) fall
/// back to the label.
fn list_order(sort: &ListSort<'_>, dir: &str) -> (String, String) {
    let (field, table) = match sort {
        ListSort::Core(field) => return (String::new(), format!("{} {dir} NULLS LAST, ci.id ASC", sort_column(field))),
        ListSort::Attribute(table, field) => (*field, table),
    };
    let col = field.column();
    let mut join = format!(" LEFT JOIN {} srt ON srt.id = ci.id", table.sql());
    let terms = match field.data_type {
        AttributeDataType::Text | AttributeDataType::Enum => vec![format!("lower(srt.{col})")],
        AttributeDataType::Lookup => {
            join.push_str(&format!(" LEFT JOIN cmdb.lookup_list_values srt_v ON srt_v.id = srt.{col}"));
            vec!["srt_v.sort_order".to_owned(), "lower(srt_v.name)".to_owned()]
        }
        _ => vec![format!("srt.{col}")],
    };
    let order: Vec<String> = terms.into_iter().map(|t| format!("{t} {dir} NULLS LAST")).collect();
    (join, format!("{}, lower(ci.label) ASC, ci.id ASC", order.join(", ")))
}

/// Which CIs by validity (see [`ACTIVE_SQL`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ActiveFilter {
    /// Only active CIs (the default for lists and search)
    #[default]
    Active,
    /// Only CIs outside their validity period
    Inactive,
    Any,
}

/// Lookup columns (table, column) holding values of one list.
pub type LookupColumns = Vec<(TableName, Ident)>;

#[derive(Debug, Clone, Default)]
pub struct ItemFilters {
    pub q: Option<String>,
    pub class_ids: Option<Vec<Uuid>>,
    pub active: ActiveFilter,
    /// Per lookup list: the requested values and the fields that store them. A
    /// CI matches when it holds one of the values of every list.
    pub lookups: Vec<(Vec<Uuid>, LookupColumns)>,
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

/// The search predicate shared by the inventory list and global search: label
/// and ident (substring, trigram-indexed; word prefix via the tsvector) and
/// field values in the type tables (text/enum substring, IP/CIDR prefix, and
/// IP containment when q is an IP or CIDR).
fn push_search(w: &mut Where<'_>, q: &str, tables: &[SearchTable]) {
    let pattern = like_pattern(q);
    let prefix = format!("{}%", escape_like(q));
    let is_net = validate::is_ip_or_cidr(q);
    let qb = w.and();
    qb.push("(ci.label ILIKE ").push_bind(pattern.clone());
    qb.push(" OR ci.ident ILIKE ").push_bind(pattern.clone());
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
                if is_net {
                    qb.push(format!(" OR {c} <<= ")).push_bind(q.to_owned()).push("::inet");
                }
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
    qb.push(")");
}

/// `ci.id IN (SELECT id FROM t1 WHERE c1 <op> $v UNION ALL ...)`, or false
/// when no field could hold a match.
fn push_in_columns(
    w: &mut Where<'_>,
    columns: &[(TableName, Ident)],
    mut test: impl FnMut(&mut QueryBuilder<Postgres>, &Ident),
) {
    if columns.is_empty() {
        w.and_sql("false");
        return;
    }
    let qb = w.and();
    qb.push("ci.id IN (");
    for (i, (table, column)) in columns.iter().enumerate() {
        if i > 0 {
            qb.push(" UNION ALL ");
        }
        qb.push(format!("SELECT id FROM {} WHERE ", table.sql()));
        test(qb, column);
    }
    qb.push(")");
}

fn push_filters(w: &mut Where<'_>, f: &ItemFilters) {
    if let Some(p) = f.deleted.and_then(|d| d.predicate("ci.deleted_at")) {
        w.and_sql(&p);
    }
    match f.active {
        ActiveFilter::Active => w.and_sql(ACTIVE_SQL),
        ActiveFilter::Inactive => w.and_sql(&format!("NOT {ACTIVE_SQL}")),
        ActiveFilter::Any => {}
    }
    if let Some(q) = &f.q {
        push_search(w, q, &f.search_tables);
    }
    for (column, ids) in [("ci.class_id", &f.class_ids), ("ci.class_id", &f.visible_class_ids)] {
        if let Some(ids) = ids {
            w.and().push(column).push(" = ANY(").push_bind(ids.clone()).push(")");
        }
    }
    for (values, columns) in &f.lookups {
        push_in_columns(w, columns, |qb, c| {
            qb.push(format!("{c} = ANY(")).push_bind(values.clone()).push(")");
        });
    }
    if let Some(cidr) = &f.ip_within {
        let columns: Vec<(TableName, Ident)> =
            f.search_tables.iter().flat_map(|t| t.ip.iter().map(|c| (t.table.clone(), c.clone()))).collect();
        push_in_columns(w, &columns, |qb, c| {
            qb.push(format!("{c} <<= ")).push_bind(cidr.clone()).push("::inet");
        });
    }
}

pub async fn list(
    pool: &PgPool,
    f: &ItemFilters,
    sort: ListSort<'_>,
    desc: bool,
    limit: i64,
    offset: i64,
) -> sqlx::Result<(Vec<SummaryRow>, i64)> {
    let (join, order) = list_order(&sort, if desc { "DESC" } else { "ASC" });
    let from = format!("{SUMMARY_FROM}{join}");
    let filter = |w: &mut Where<'_>| push_filters(w, f);
    crud::select_page_counted(pool, &from, COUNT_FROM, &summary_columns(), &filter, &order, limit, offset).await
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
    let mut rows = QueryBuilder::<Postgres>::new(format!("SELECT {} FROM {SUMMARY_FROM}", summary_columns()));
    filter(&mut Where::new(&mut rows));
    rows.push(" ORDER BY (lower(ci.label) = ")
        .push_bind(lower.clone())
        .push(" OR lower(ci.ident) = ")
        .push_bind(lower.clone())
        .push(") DESC NULLS LAST, (lower(ci.label) LIKE ")
        .push_bind(format!("{}%", escape_like(&lower)))
        .push(") DESC NULLS LAST, similarity(ci.label, ")
        .push_bind(q.to_owned())
        .push(") DESC, lower(ci.label) ASC, ci.id ASC LIMIT ")
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
    sqlx::query_as(AssertSqlSafe(format!("SELECT {} FROM {SUMMARY_FROM} WHERE ci.id = ANY($1)", summary_columns())))
        .bind(ids)
        .fetch_all(conn)
        .await
}

pub async fn summary(conn: &mut PgConnection, id: Uuid) -> sqlx::Result<Option<SummaryRow>> {
    sqlx::query_as(AssertSqlSafe(format!("SELECT {} FROM {SUMMARY_FROM} WHERE ci.id = $1", summary_columns())))
        .bind(id)
        .fetch_optional(conn)
        .await
}

// ---------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------

/// Registry columns of a new CI; the label follows from its field values (see [`refresh_labels`]).
pub struct NewItem<'a> {
    pub class_id: Uuid,
    /// None: generated (`cmdb.new_ci_ident()`)
    pub ident: Option<&'a str>,
    /// None: now
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_until: Option<DateTime<Utc>>,
}

pub async fn insert(conn: &mut PgConnection, ci: &NewItem<'_>) -> sqlx::Result<Uuid> {
    // The label is set from the ident here and replaced once the field values are written.
    sqlx::query_scalar(
        "WITH new AS (SELECT COALESCE($2, cmdb.new_ci_ident()) AS ident)
         INSERT INTO cmdb.configuration_items (class_id, ident, label, valid_from, valid_until)
         SELECT $1, new.ident, new.ident, COALESCE($3, now()), $4 FROM new
         RETURNING id",
    )
    .bind(ci.class_id)
    .bind(ci.ident)
    .bind(ci.valid_from)
    .bind(ci.valid_until)
    .fetch_one(conn)
    .await
}

/// PATCH of the registry columns: `None` keeps a column, `Some(None)` clears a nullable one.
#[derive(Default)]
pub struct ItemPatch<'a> {
    pub class_id: Option<Uuid>,
    pub ident: Option<&'a str>,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_until: Option<Option<DateTime<Utc>>>,
}

/// Applies the patch and bumps the optimistic-locking version.
pub async fn update(conn: &mut PgConnection, id: Uuid, p: &ItemPatch<'_>) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE cmdb.configuration_items SET
           class_id = COALESCE($2, class_id),
           ident = COALESCE($3, ident),
           valid_from = COALESCE($4, valid_from),
           valid_until = CASE WHEN $5 THEN $6 ELSE valid_until END,
           version = version + 1
         WHERE id = $1",
    )
    .bind(id)
    .bind(p.class_id)
    .bind(p.ident)
    .bind(p.valid_from)
    .bind(p.valid_until.is_some())
    .bind(p.valid_until.flatten())
    .execute(conn)
    .await?;
    Ok(())
}

/// Recomputes the label of the CIs of these classes (only `ci_ids` when given):
/// the value of the class's title attribute as text, or the ident. Classes are
/// labelled one statement each; rows already right are not touched.
pub async fn refresh_labels(
    conn: &mut PgConnection,
    model: &Model,
    class_ids: &[Uuid],
    ci_ids: Option<&[Uuid]>,
) -> sqlx::Result<u64> {
    let mut changed = 0;
    for class_id in class_ids {
        let title = model.title_field(*class_id).and_then(|f| Some((f, model.table(f.class_id)?)));
        let (from, value) = match &title {
            Some((f, table)) => (
                format!(" FROM {} t WHERE t.id = ci.id AND", table.sql()),
                format!(
                    "nullif(btrim(left({}, {LABEL_MAX})), '')",
                    label_text(&format!("t.{}", f.column()), f.data_type)
                ),
            ),
            None => (" WHERE".to_owned(), "NULL".to_owned()),
        };
        let sql = format!(
            "UPDATE cmdb.configuration_items ci SET label = COALESCE({value}, ci.ident){from} ci.class_id = $1
               AND ($2::uuid[] IS NULL OR ci.id = ANY($2))
               AND ci.label IS DISTINCT FROM COALESCE({value}, ci.ident)"
        );
        changed += sqlx::query(AssertSqlSafe(sql))
            .persistent(false)
            .bind(class_id)
            .bind(ci_ids)
            .execute(&mut *conn)
            .await?
            .rows_affected();
    }
    Ok(changed)
}

/// Longest label kept (a title field may hold long text).
const LABEL_MAX: usize = 500;

/// A title field's value as label text (an inet without its /32, a datetime in UTC).
fn label_text(column: &str, t: AttributeDataType) -> String {
    use AttributeDataType as T;
    match t {
        T::Ip => format!("host({column})"),
        T::Datetime => format!("to_char({column} AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI \"UTC\"')"),
        _ => format!("{column}::text"),
    }
}

/// The locked CI row, when it exists.
#[derive(sqlx::FromRow)]
pub struct Locked {
    pub class_id: Uuid,
    pub ident: String,
    pub version: i32,
    pub deleted_at: Option<DateTime<Utc>>,
}

pub async fn lock(conn: &mut PgConnection, id: Uuid) -> sqlx::Result<Option<Locked>> {
    sqlx::query_as("SELECT class_id, ident, version, deleted_at FROM cmdb.configuration_items WHERE id = $1 FOR UPDATE")
        .bind(id)
        .fetch_optional(conn)
        .await
}

/// Lookup list of each of these values (unknown ids are left out).
pub async fn lookup_value_lists(conn: &mut PgConnection, ids: &[Uuid]) -> sqlx::Result<Vec<(Uuid, Uuid)>> {
    sqlx::query_as("SELECT id, list_id FROM cmdb.lookup_list_values WHERE id = ANY($1)").bind(ids).fetch_all(conn).await
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

/// A referenced CI: its label, whether it is deleted, and its class (callers
/// redact references into classes the reader may not view).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ReferencedItem {
    pub id: Uuid,
    pub label: String,
    pub deleted: bool,
    pub class_id: Uuid,
}

/// The referenced CIs, by id.
pub async fn reference_names(conn: &mut PgConnection, ids: &[Uuid]) -> sqlx::Result<HashMap<Uuid, ReferencedItem>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<ReferencedItem> = sqlx::query_as(
        "SELECT id, label, deleted_at IS NOT NULL AS deleted, class_id FROM cmdb.configuration_items WHERE id = ANY($1)",
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
