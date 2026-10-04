//! SQL for the inventory facet counts (SHAA-1686): how many CIs of the
//! filtered inventory hold each class, criticality, lookup value and business
//! service.
//!
//! One statement: every distinct filter set (all filters, and all filters but
//! one facet's own) is a materialized CTE of the matching CI ids, and each
//! facet is a GROUP BY over its set. The filter sets are built by
//! [`push_filters`], the same predicate as the list, view scope included, so a
//! count never covers a CI the caller could not list.

use sqlx::{PgConnection, Postgres, QueryBuilder};
use uuid::Uuid;

use super::crud::Where;
use super::items::{ItemFilters, LookupColumns, push_filters, push_member_edges};

/// What a facet counts.
#[derive(Debug, Clone)]
pub enum FacetSource {
    Class,
    Criticality,
    /// Values of one lookup list, in the fields that store it
    Lookup(LookupColumns),
    /// Direct members of each live business service of a visible class
    Service {
        member_type: Uuid,
        visible: Option<Vec<Uuid>>,
    },
}

/// A facet over one of the filter sets passed to [`counts`].
#[derive(Debug, Clone)]
pub struct FacetSql {
    pub base: usize,
    pub source: FacetSource,
}

/// One counted value: (facet index, value id, CIs).
pub type FacetCount = (i32, Uuid, i64);

/// The CIs matching `sets[0]` and the counts of every facet. A CI holding the
/// same lookup value in two fields counts once.
pub async fn counts(
    conn: &mut PgConnection,
    sets: &[ItemFilters],
    facets: &[FacetSql],
) -> sqlx::Result<(i64, Vec<FacetCount>)> {
    let mut qb = QueryBuilder::<Postgres>::new("WITH ");
    for (i, f) in sets.iter().enumerate() {
        if i > 0 {
            qb.push(", ");
        }
        qb.push(format!(
            "b{i} AS MATERIALIZED (SELECT ci.id, ci.class_id, ci.criticality_value_id FROM configuration_items ci"
        ));
        push_filters(&mut Where::new(&mut qb), f);
        qb.push(")");
    }
    // The total first, as facet -1.
    qb.push(" SELECT -1 AS facet, NULL::uuid AS value, count(*) AS n FROM b0");
    for (i, facet) in facets.iter().enumerate() {
        let b = format!("b{}", facet.base);
        qb.push(" UNION ALL ");
        match &facet.source {
            FacetSource::Class => {
                qb.push(format!("SELECT {i}, class_id, count(*) FROM {b} GROUP BY class_id"));
            }
            FacetSource::Criticality => {
                qb.push(format!(
                    "SELECT {i}, criticality_value_id, count(*) FROM {b} WHERE criticality_value_id IS NOT NULL
                     GROUP BY criticality_value_id"
                ));
            }
            FacetSource::Lookup(columns) => {
                qb.push(format!("SELECT {i}, v, count(DISTINCT id) FROM ("));
                for (j, (table, column)) in columns.iter().enumerate() {
                    if j > 0 {
                        qb.push(" UNION ALL ");
                    }
                    qb.push(format!(
                        "SELECT t.id, t.{column} AS v FROM {} t JOIN {b} ON {b}.id = t.id WHERE t.{column} IS NOT NULL",
                        table.sql()
                    ));
                }
                qb.push(") x GROUP BY v");
            }
            FacetSource::Service { member_type, visible } => {
                qb.push(format!("SELECT {i}, e.source_ci_id, count(DISTINCT e.target_ci_id)"));
                push_member_edges(&mut qb, *member_type, visible.as_deref());
                qb.push(format!(" AND e.target_ci_id IN (SELECT id FROM {b}) GROUP BY e.source_ci_id"));
            }
        }
    }
    let rows: Vec<(i32, Option<Uuid>, i64)> = qb.build_query_as().fetch_all(conn).await?;
    let mut total = 0;
    let mut out = Vec::with_capacity(rows.len());
    for (facet, value, n) in rows {
        match value {
            Some(value) if facet >= 0 => out.push((facet, value, n)),
            _ => total = n,
        }
    }
    Ok((total, out))
}

/// A lookup list that can be a facet.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ListRow {
    pub id: Uuid,
    pub key: String,
    pub name: String,
}

/// Every lookup list, in display order; the criticality list is its own facet.
pub async fn lookup_lists(conn: &mut PgConnection) -> sqlx::Result<Vec<ListRow>> {
    sqlx::query_as(
        "SELECT id, key, name FROM lookup_lists WHERE system_role IS DISTINCT FROM 'criticality'
         ORDER BY sort_order, lower(name), key",
    )
    .fetch_all(conn)
    .await
}

/// The criticality list (a system list), if there is one.
pub async fn criticality_list(conn: &mut PgConnection) -> sqlx::Result<Option<Uuid>> {
    sqlx::query_scalar("SELECT id FROM lookup_lists WHERE system_role = 'criticality'").fetch_optional(conn).await
}

/// The label of a counted value.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ValueRow {
    pub id: Uuid,
    pub key: String,
    pub name: String,
    /// The list's sort order for lookup values, 0 otherwise
    pub sort_order: i32,
    /// The list of a lookup value, the class of a service CI, the class itself for a class
    pub scope: Option<Uuid>,
}

pub async fn class_labels(conn: &mut PgConnection, ids: &[Uuid]) -> sqlx::Result<Vec<ValueRow>> {
    sqlx::query_as("SELECT id, key, name, 0 AS sort_order, id AS scope FROM ci_classes WHERE id = ANY($1)")
        .bind(ids)
        .fetch_all(conn)
        .await
}

pub async fn value_labels(conn: &mut PgConnection, ids: &[Uuid]) -> sqlx::Result<Vec<ValueRow>> {
    sqlx::query_as("SELECT id, key, name, sort_order, list_id AS scope FROM lookup_list_values WHERE id = ANY($1)")
        .bind(ids)
        .fetch_all(conn)
        .await
}

/// Live services among `ids` (the caller checks their class).
pub async fn service_labels(conn: &mut PgConnection, ids: &[Uuid]) -> sqlx::Result<Vec<ValueRow>> {
    sqlx::query_as(
        "SELECT id, ident AS key, label AS name, 0 AS sort_order, class_id AS scope FROM configuration_items
         WHERE id = ANY($1) AND deleted_at IS NULL",
    )
    .bind(ids)
    .fetch_all(conn)
    .await
}
