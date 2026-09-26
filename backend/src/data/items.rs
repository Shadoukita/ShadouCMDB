//! SQL for configuration items: inventory list, detail, global search,
//! attribute values and relationship-graph expansion.

use chrono::{DateTime, Utc};
use ipnetwork::IpNetwork;
use sqlx::{AssertSqlSafe, PgConnection, PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

use super::crud::{self, Where};
use crate::api::schemas::{Deleted, OwnerKind, escape_like, like_pattern};
use crate::api::validate;

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
}

/// Words of a query turned into a prefix tsquery ("web prod" -> 'web:* & prod:*').
pub fn query_words(q: &str) -> Vec<String> {
    q.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_owned).collect()
}

/// The search predicate shared by the inventory list and global search: name,
/// hostname and serial (substring, trigram-indexed), notes (word prefix, via the
/// tsvector), IP address (prefix, or containment when q is an IP/CIDR) and
/// attribute values (text/enum substring, IP/CIDR prefix).
fn push_search(w: &mut Where<'_>, q: &str) {
    let pattern = like_pattern(q);
    let prefix = format!("{}%", escape_like(q));
    let qb = w.and();
    qb.push("(ci.name ILIKE ").push_bind(pattern.clone());
    qb.push(" OR ci.hostname ILIKE ").push_bind(pattern.clone());
    qb.push(" OR ci.serial_number ILIKE ").push_bind(pattern.clone());
    qb.push(" OR host(ci.ip_address) LIKE ").push_bind(prefix.clone());
    qb.push(" OR EXISTS (SELECT 1 FROM ci_attribute_values v WHERE v.ci_id = ci.id AND (v.value_text ILIKE ")
        .push_bind(pattern)
        .push(" OR host(v.value_ip) LIKE ")
        .push_bind(prefix.clone())
        .push(" OR v.value_cidr::text LIKE ")
        .push_bind(prefix)
        .push("))");
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
        push_search(w, q);
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
    /// Null: the audit entry records the edge as it was before the delete.
    #[serde(serialize_with = "crate::api::schemas::ts_opt::serialize")]
    pub deleted_at: Option<DateTime<Utc>>,
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
// Attribute values
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct StoredValueRow {
    pub ci_id: Uuid,
    pub attribute_id: Uuid,
    pub key: String,
    pub label: String,
    pub value_text: Option<String>,
    pub value_number: Option<f64>,
    pub value_boolean: Option<bool>,
    pub value_date: Option<String>,
    pub value_datetime: Option<DateTime<Utc>>,
    pub value_ip: Option<String>,
    pub value_cidr: Option<String>,
    pub value_ref_ci_id: Option<Uuid>,
    pub ref_name: Option<String>,
    pub ref_deleted: Option<bool>,
}

pub async fn attribute_values(conn: &mut PgConnection, ci_ids: &[Uuid]) -> sqlx::Result<Vec<StoredValueRow>> {
    if ci_ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as!(
        StoredValueRow,
        r#"SELECT v.ci_id, v.attribute_id, d.key, d.label,
                  v.value_text, v.value_number::float8 AS value_number, v.value_boolean,
                  v.value_date::text AS value_date, v.value_datetime,
                  host(v.value_ip) AS value_ip, v.value_cidr::text AS value_cidr, v.value_ref_ci_id,
                  r.name AS "ref_name?", (r.deleted_at IS NOT NULL) AS ref_deleted
           FROM ci_attribute_values v
           JOIN ci_attribute_definitions d ON d.id = v.attribute_id
           LEFT JOIN configuration_items r ON r.id = v.value_ref_ci_id
           WHERE v.ci_id = ANY($1)
           ORDER BY d.sort_order, d.key"#,
        ci_ids
    )
    .fetch_all(conn)
    .await
}

/// One typed value, bound to the column its data type is stored in.
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
}

pub async fn upsert_attribute_value(
    conn: &mut PgConnection,
    ci_id: Uuid,
    attribute_id: Uuid,
    value: &StoredValue,
) -> sqlx::Result<()> {
    let (mut text, mut number, mut boolean, mut date, mut datetime, mut ip, mut cidr, mut reference) =
        (None, None, None, None, None, None, None, None);
    match value {
        StoredValue::Text(v) => text = Some(v.as_str()),
        StoredValue::Number(v) => number = Some(v.to_string()),
        StoredValue::Boolean(v) => boolean = Some(*v),
        StoredValue::Date(v) => date = Some(v.as_str()),
        StoredValue::Datetime(v) => datetime = Some(v.as_str()),
        StoredValue::Ip(v) => ip = Some(v.as_str()),
        StoredValue::Cidr(v) => cidr = Some(v.as_str()),
        StoredValue::Reference(v) => reference = Some(*v),
    }
    sqlx::query!(
        "INSERT INTO ci_attribute_values
           (ci_id, attribute_id, value_text, value_number, value_boolean, value_date, value_datetime,
            value_ip, value_cidr, value_ref_ci_id)
         VALUES ($1, $2, $3, $4::text::numeric, $5, $6::text::date, $7::text::timestamptz,
                 $8::text::inet, $9::text::cidr, $10)
         ON CONFLICT (ci_id, attribute_id) DO UPDATE SET
           value_text = EXCLUDED.value_text, value_number = EXCLUDED.value_number,
           value_boolean = EXCLUDED.value_boolean, value_date = EXCLUDED.value_date,
           value_datetime = EXCLUDED.value_datetime, value_ip = EXCLUDED.value_ip,
           value_cidr = EXCLUDED.value_cidr, value_ref_ci_id = EXCLUDED.value_ref_ci_id",
        ci_id,
        attribute_id,
        text,
        number,
        boolean,
        date,
        datetime,
        ip,
        cidr,
        reference
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn delete_attribute_values(conn: &mut PgConnection, ci_id: Uuid, attribute_ids: &[Uuid]) -> sqlx::Result<()> {
    if attribute_ids.is_empty() {
        return Ok(());
    }
    sqlx::query!("DELETE FROM ci_attribute_values WHERE ci_id = $1 AND attribute_id = ANY($2)", ci_id, attribute_ids)
        .execute(conn)
        .await?;
    Ok(())
}

/// Of the given CI ids, those that exist and are not deleted.
pub async fn live_items(conn: &mut PgConnection, ids: &[Uuid]) -> sqlx::Result<Vec<Uuid>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_scalar!("SELECT id FROM configuration_items WHERE id = ANY($1) AND deleted_at IS NULL", ids)
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
