//! SQL for impact analysis: one query per hop of the breadth-first traversal,
//! the probe beyond the last hop and the in-edge counts. The engine is
//! [`crate::modules::impact::engine`].
//!
//! Every query reads live edges between live CIs only, and filters the CI it
//! reaches ("next") by the caller's visible classes and by validity, so a CI
//! the caller may not view never enters the frontier: nothing behind it is
//! reached through it.

use sqlx::{PgConnection, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::schema::ACTIVE_SQL;

/// A relationship type and how impact flows across its edges.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TypeRow {
    pub id: Uuid,
    pub key: String,
    pub name: String,
    pub forward_label: String,
    pub reverse_label: String,
    pub impact_direction: String,
}

/// Every relationship type (a few dozen rows), in the traversal's snapshot.
pub async fn types(conn: &mut PgConnection) -> sqlx::Result<Vec<TypeRow>> {
    sqlx::query_as(
        "SELECT id, key, name, forward_label, reverse_label, impact_direction
         FROM cmdb.relationship_types ORDER BY sort_order, key",
    )
    .fetch_all(conn)
    .await
}

/// The edges one hop may follow, split by which end is the next CI:
/// `to_source` types lead from an edge's target to its source (the frontier
/// CI is the target), `to_target` types from its source to its target.
/// Either may be empty; a type with impact flowing both ways is in both.
#[derive(Debug, Clone, Default)]
pub struct HopTypes {
    pub to_source: Vec<Uuid>,
    pub to_target: Vec<Uuid>,
}

impl HopTypes {
    pub fn is_empty(&self) -> bool {
        self.to_source.is_empty() && self.to_target.is_empty()
    }
}

/// Filters on the CI a hop reaches.
#[derive(Debug, Clone, Copy)]
pub struct Reach<'a> {
    /// Classes the caller may view; `None` is every class.
    pub visible: Option<&'a [Uuid]>,
    /// Also CIs outside their validity period.
    pub include_inactive: bool,
}

/// An edge from a frontier CI (`from_id`) to a CI not visited yet (`next_id`).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct HopEdge {
    pub id: Uuid,
    pub relationship_type_id: Uuid,
    pub from_id: Uuid,
    pub next_id: Uuid,
    pub next_class_id: Uuid,
    pub source_ci_id: Uuid,
    pub target_ci_id: Uuid,
}

/// One branch of a hop: `next` is the end of the edge the frontier is not on.
fn push_branch(
    qb: &mut QueryBuilder<Postgres>,
    next: &str,
    from: &str,
    types: &[Uuid],
    frontier: &[Uuid],
    visited: &[Uuid],
    reach: Reach<'_>,
) {
    qb.push(format!(
        "SELECT r.id, r.relationship_type_id, r.{from} AS from_id, r.{next} AS next_id, ci.class_id AS next_class_id,
                r.source_ci_id, r.target_ci_id, r.created_at
         FROM cmdb.ci_relationships r JOIN cmdb.configuration_items ci ON ci.id = r.{next}
         WHERE r.deleted_at IS NULL AND ci.deleted_at IS NULL AND r.{from} = ANY("
    ))
    .push_bind(frontier.to_vec())
    .push(") AND r.relationship_type_id = ANY(")
    .push_bind(types.to_vec())
    .push(format!(") AND NOT (r.{next} = ANY("))
    .push_bind(visited.to_vec())
    .push("))");
    if let Some(classes) = reach.visible {
        qb.push(" AND ci.class_id = ANY(").push_bind(classes.to_vec()).push(")");
    }
    if !reach.include_inactive {
        qb.push(format!(" AND {ACTIVE_SQL}"));
    }
}

/// The query of one hop (see [`hop`]); `prefix` goes in front (`EXPLAIN …` in tests).
fn hop_query(
    prefix: &str,
    frontier: &[Uuid],
    visited: &[Uuid],
    types: &HopTypes,
    reach: Reach<'_>,
    limit: i64,
) -> QueryBuilder<Postgres> {
    let mut qb = QueryBuilder::<Postgres>::new(format!(
        "{prefix}SELECT id, relationship_type_id, from_id, next_id, next_class_id, source_ci_id, target_ci_id FROM ("
    ));
    let mut first = true;
    for (next, from, ids) in
        [("source_ci_id", "target_ci_id", &types.to_source), ("target_ci_id", "source_ci_id", &types.to_target)]
    {
        if ids.is_empty() {
            continue;
        }
        if !first {
            qb.push(" UNION ALL ");
        }
        first = false;
        push_branch(&mut qb, next, from, ids, frontier, visited, reach);
    }
    qb.push(") e ORDER BY created_at, id LIMIT ").push_bind(limit);
    qb
}

/// The edges leading from `frontier` to CIs outside `visited` that the hop
/// may reach, oldest first (then by id), at most `limit`.
pub async fn hop(
    conn: &mut PgConnection,
    frontier: &[Uuid],
    visited: &[Uuid],
    types: &HopTypes,
    reach: Reach<'_>,
    limit: i64,
) -> sqlx::Result<Vec<HopEdge>> {
    if frontier.is_empty() || types.is_empty() {
        return Ok(Vec::new());
    }
    let mut qb = hop_query("", frontier, visited, types, reach, limit);
    qb.build_query_as::<HopEdge>().persistent(false).fetch_all(conn).await
}

/// The plan of one hop's query, as `EXPLAIN (FORMAT JSON)` returns it.
#[cfg(test)]
pub async fn explain_hop(
    conn: &mut PgConnection,
    frontier: &[Uuid],
    visited: &[Uuid],
    types: &HopTypes,
    reach: Reach<'_>,
    limit: i64,
) -> sqlx::Result<serde_json::Value> {
    let mut qb = hop_query("EXPLAIN (FORMAT JSON) ", frontier, visited, types, reach, limit);
    qb.build_query_scalar::<serde_json::Value>().persistent(false).fetch_one(conn).await
}

/// Per CI in `targets`: how many propagating edges lead into it from a CI in
/// `visited` (the edges a hop would have followed, visited or not).
pub async fn in_edge_counts(
    conn: &mut PgConnection,
    targets: &[Uuid],
    visited: &[Uuid],
    types: &HopTypes,
) -> sqlx::Result<Vec<(Uuid, i64)>> {
    if targets.is_empty() || types.is_empty() {
        return Ok(Vec::new());
    }
    let mut qb = QueryBuilder::<Postgres>::new("SELECT next_id, count(*) FROM (");
    let mut first = true;
    for (next, from, ids) in
        [("source_ci_id", "target_ci_id", &types.to_source), ("target_ci_id", "source_ci_id", &types.to_target)]
    {
        if ids.is_empty() {
            continue;
        }
        if !first {
            qb.push(" UNION ALL ");
        }
        first = false;
        qb.push(format!(
            "SELECT r.{next} AS next_id FROM cmdb.ci_relationships r WHERE r.deleted_at IS NULL AND r.{next} = ANY("
        ))
        .push_bind(targets.to_vec())
        .push(format!(") AND r.{from} = ANY("))
        .push_bind(visited.to_vec())
        .push(") AND r.relationship_type_id = ANY(")
        .push_bind(ids.to_vec())
        .push(")");
    }
    qb.push(") e GROUP BY next_id");
    qb.build_query_as::<(Uuid, i64)>().persistent(false).fetch_all(conn).await
}

/// `statement_timeout` for the rest of the transaction (or savepoint).
pub async fn set_statement_timeout(conn: &mut PgConnection, ms: u64) -> sqlx::Result<()> {
    sqlx::query("SELECT set_config('statement_timeout', $1, true)").bind(format!("{ms}ms")).execute(conn).await?;
    Ok(())
}

/// Whether any relationship type propagates impact.
pub async fn any_type_propagates(conn: &mut PgConnection) -> sqlx::Result<bool> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM cmdb.relationship_types WHERE impact_direction <> 'none')")
        .fetch_one(conn)
        .await
}

/// A lookup value (key and name) by id.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct LookupValue {
    pub id: Uuid,
    pub key: String,
    pub name: String,
}

pub async fn lookup_values(conn: &mut PgConnection, ids: &[Uuid]) -> sqlx::Result<Vec<LookupValue>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as("SELECT id, key, name FROM cmdb.lookup_list_values WHERE id = ANY($1)")
        .bind(ids)
        .fetch_all(conn)
        .await
}

/// `(ci id, value id)` of a lookup column in a type table, for these CIs.
pub async fn lookup_column_values(
    conn: &mut PgConnection,
    table: &crate::schema::model::TableName,
    column: &crate::schema::naming::Ident,
    ids: &[Uuid],
) -> sqlx::Result<Vec<(Uuid, Uuid)>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT id, {column} FROM {} WHERE id = ANY($1) AND {column} IS NOT NULL",
        table.sql()
    )))
    .persistent(false)
    .bind(ids)
    .fetch_all(conn)
    .await
}
