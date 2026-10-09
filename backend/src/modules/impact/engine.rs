//! The impact traversal: a breadth-first search over CIs, one query per hop,
//! bounded by depth, a node budget, an edge budget and a wall-clock deadline.
//!
//! [`traverse`] takes any number of roots (business services will ask for the
//! impact of a set of CIs) and runs each requested direction separately: the
//! downstream walk follows impact the way it flows, the upstream walk against
//! it. Each CI is expanded at most once, at its shortest hop distance, so
//! cycles end by themselves; the first edge that reaches a CI (BFS order, ties
//! by the edge's `created_at, id`) is its `via`. The caller runs it inside one
//! REPEATABLE READ transaction so every hop sees the same snapshot.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use sqlx::{Connection, PgConnection};
use tokio::time::Instant;
use uuid::Uuid;

use super::ImpactDirection;
use crate::data::impact::{self as data, HopEdge, HopTypes, Reach, TypeRow};
use crate::http::error::{AppError, ErrorCode};

/// Relationship rows one analysis may read per `maxNodes`, over all hops
/// (the same budget as the relationship graph, GH#185).
pub const EDGES_PER_NODE: usize = 5;

/// Time the queries that assemble a result get after the walks' deadline, all
/// of them together (GH#393): they are bounded by the result's size, not the
/// graph's. The in-edge counts, which have a fallback, may use the first half;
/// the summaries and statuses the caller reads have until the end.
pub const ASSEMBLY_ALLOWANCE: Duration = Duration::from_millis(crate::config::IMPACT_ASSEMBLY_ALLOWANCE_MS);

/// Roots whose in-edge counts sleep until cancelled, as on a very slow
/// database (tests of the assembly allowance).
#[cfg(test)]
pub static SLOW_COUNTS: std::sync::Mutex<Vec<Uuid>> = std::sync::Mutex::new(Vec::new());

/// The end of the assembly allowance after the walks' `deadline`.
pub fn assembly_deadline(deadline: Instant) -> Instant {
    deadline + ASSEMBLY_ALLOWANCE
}

/// Which way a walk goes relative to the flow of impact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Way {
    /// With the flow: what is affected when the roots fail.
    Downstream,
    /// Against the flow: what the roots depend on.
    Upstream,
}

/// Why a walk stopped before it was complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Truncation {
    MaxNodes,
    MaxEdges,
    Timeout,
}

#[derive(Debug, Clone)]
pub struct Options<'a> {
    /// The walks to run, in this order (they share the budgets).
    pub ways: &'a [Way],
    /// Hops from the roots, at least 1.
    pub depth: i32,
    /// Follow only these relationship types (`None`: every type). A type
    /// whose impact direction is `none` contributes nothing either way.
    pub types: Option<&'a [Uuid]>,
    pub include_inactive: bool,
    /// CIs a result may hold, over all walks (a CI reached both ways counts once).
    pub max_nodes: usize,
    /// When the walks stop; the in-edge counts may run until half of
    /// [`ASSEMBLY_ALLOWANCE`] after it.
    pub deadline: Instant,
    /// Classes the caller may view (`None`: every class). CIs of other classes
    /// are neither returned nor walked through.
    pub visible: Option<&'a [Uuid]>,
    /// Keep only CIs of these classes in the result, without changing the walk
    /// (not exposed by the API yet; for business services, "which services are
    /// affected"). A kept CI's `via` may then name a CI that was left out.
    pub result_classes: Option<&'a [Uuid]>,
}

/// The edge that first reached a CI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Via {
    /// The CI the walk came from: a root or a CI reached one hop earlier.
    pub parent_id: Uuid,
    pub relationship_id: Uuid,
    pub relationship_type_id: Uuid,
    pub edge_source_id: Uuid,
    pub edge_target_id: Uuid,
}

#[derive(Debug, Clone)]
pub struct Reached {
    pub id: Uuid,
    pub class_id: Uuid,
    pub hops: i32,
    pub via: Via,
    /// Propagating edges into this CI from CIs the walk visited (roots included).
    pub reached_by: i64,
}

#[derive(Debug, Clone)]
pub struct Walk {
    pub way: Way,
    /// In BFS order: by hop, then by the order of the edges that reached them.
    pub nodes: Vec<Reached>,
    pub truncated: Option<Truncation>,
    /// The walk reached `depth` and a propagating edge leads on from its last
    /// hop to a CI it has not visited (only when not truncated).
    pub more_beyond_depth: bool,
}

#[derive(Debug, Clone)]
pub struct Traversal {
    pub walks: Vec<Walk>,
    /// Every relationship type in the snapshot the walks used.
    pub types: Vec<TypeRow>,
}

impl Traversal {
    /// The first reason any walk stopped early (walks run in order).
    pub fn truncated(&self) -> Option<Truncation> {
        self.walks.iter().find_map(|w| w.truncated)
    }
}

/// Which edges a walk follows for the configured impact directions.
pub(crate) fn hop_types(types: &[TypeRow], wanted: Option<&[Uuid]>, way: Way) -> HopTypes {
    let mut out = HopTypes::default();
    for t in types.iter().filter(|t| wanted.is_none_or(|w| w.contains(&t.id))) {
        let Some(dir) = ImpactDirection::parse(&t.impact_direction) else { continue };
        // Downstream from a target means reaching the source when impact flows target -> source.
        let (to_source, to_target) = match dir {
            ImpactDirection::None => (false, false),
            ImpactDirection::Both => (true, true),
            ImpactDirection::TargetToSource => (way == Way::Downstream, way == Way::Upstream),
            ImpactDirection::SourceToTarget => (way == Way::Upstream, way == Way::Downstream),
        };
        if to_source {
            out.to_source.push(t.id);
        }
        if to_target {
            out.to_target.push(t.id);
        }
    }
    out
}

pub(crate) fn is_query_canceled(err: &sqlx::Error) -> bool {
    err.as_database_error().and_then(|e| e.code()).is_some_and(|c| c == "57014")
}

#[cfg(test)]
thread_local! {
    /// When each statement bounded on this thread would be cancelled, and whether
    /// it was the in-edge counts: tests of the allowance compare these with the
    /// deadline on the analysis's own clock instead of timing the request, which a
    /// loaded host stretches (GH#791). `#[tokio::test]` runs the analysis's task on
    /// the test's thread.
    pub static CANCEL_AT: std::cell::RefCell<Vec<(Instant, bool)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Takes the bounds recorded on this thread since the last call and checks
/// them against the allowance of the analysis they belong to (GH#791). Its
/// first statement is bounded by the end of the allowance, which fixes that end
/// to the millisecond: every statement must be cancelled by then, the in-edge
/// counts half the allowance earlier. Returns how many counts were bounded.
#[cfg(test)]
pub fn assert_bounded_by_the_allowance() -> usize {
    let bounds = CANCEL_AT.take();
    let Some(&(first, _)) = bounds.first() else { panic!("no statement was bounded") };
    let end = first + Duration::from_millis(1);
    for (i, &(at, counts)) in bounds.iter().enumerate() {
        let limit = if counts { end - ASSEMBLY_ALLOWANCE / 2 } else { end };
        assert!(at <= limit, "statement {i} (counts: {counts}) ends {:?} after its limit", at - limit);
    }
    bounds.iter().filter(|b| b.1).count()
}

/// The time left, in whole milliseconds (at least 1), or `None` once it has run out.
pub(crate) fn remaining_ms(deadline: Instant) -> Option<u64> {
    let now = Instant::now();
    let left = deadline.checked_duration_since(now)?;
    let ms = left.as_millis() as u64;
    #[cfg(test)]
    if ms > 0 {
        CANCEL_AT.with_borrow_mut(|c| c.push((now + Duration::from_millis(ms), false)));
    }
    (ms > 0).then_some(ms)
}

/// The analysis could not assemble its result within the allowance after its
/// deadline (a very slow database): answered rather than holding the
/// connection and the concurrency places until the request times out (GH#393).
pub(crate) fn out_of_time() -> AppError {
    let mut err = AppError::new(
        ErrorCode::ServerBusy,
        "The impact analysis could not be completed in time because the database is responding slowly; retry \
         shortly, or lower depth or maxNodes",
    );
    err.retry_after = Some(1);
    err
}

/// A statement's error, with one cancelled by the deadline as [`out_of_time`].
pub(crate) fn db_error(e: sqlx::Error) -> AppError {
    if is_query_canceled(&e) { out_of_time() } else { e.into() }
}

/// Bounds the next statement on `conn` by `until`. `statement_timeout` limits
/// each statement on its own, so this runs again before every statement.
pub(crate) async fn bound(conn: &mut PgConnection, until: Instant) -> Result<(), AppError> {
    let ms = remaining_ms(until).ok_or_else(out_of_time)?;
    data::set_statement_timeout(conn, ms).await.map_err(db_error)?;
    Ok(())
}

/// One hop's query under the deadline, in a savepoint so a cancelled
/// statement leaves the snapshot usable. The savepoint is rolled back either
/// way (the hop only reads): `set_config` survives a released savepoint, and
/// the hop's short timeout would otherwise bound the statements after it, a
/// cancelled SAVEPOINT aborting the analysis with 500 (GH#825). `Ok(None)`:
/// the deadline hit.
async fn timed_hop(
    conn: &mut PgConnection,
    deadline: Instant,
    frontier: &[Uuid],
    visited: &[Uuid],
    types: &HopTypes,
    reach: Reach<'_>,
    limit: i64,
) -> Result<Option<Vec<HopEdge>>, AppError> {
    let Some(ms) = remaining_ms(deadline) else { return Ok(None) };
    let mut sp = conn.begin().await.map_err(db_error)?;
    data::set_statement_timeout(&mut sp, ms).await.map_err(db_error)?;
    match data::hop(&mut sp, frontier, visited, types, reach, limit).await {
        Ok(rows) => {
            sp.rollback().await.map_err(db_error)?;
            Ok(Some(rows))
        }
        Err(e) if is_query_canceled(&e) => {
            sp.rollback().await.map_err(db_error)?;
            Ok(None)
        }
        Err(e) => Err(e.into()),
    }
}

/// Budgets shared by the walks of one traversal.
struct Budget {
    deadline: Instant,
    edges: usize,
    max_nodes: usize,
    /// Every CI any walk returns, so one reached both ways counts once.
    returned: HashSet<Uuid>,
}

async fn walk(
    conn: &mut PgConnection,
    roots: &[Uuid],
    way: Way,
    types: &HopTypes,
    opts: &Options<'_>,
    budget: &mut Budget,
) -> Result<(Walk, Vec<Uuid>), AppError> {
    let reach = Reach { visible: opts.visible, include_inactive: opts.include_inactive };
    let mut visited: Vec<Uuid> = roots.to_vec();
    let mut seen: HashSet<Uuid> = roots.iter().copied().collect();
    let mut nodes: Vec<Reached> = Vec::new();
    let mut frontier: Vec<Uuid> = roots.to_vec();
    let mut truncated = None;
    let mut completed_depth = false;

    for hop in 1..=opts.depth {
        if frontier.is_empty() {
            break;
        }
        // One row over the budget tells a cut-off result from an exact fit.
        let Some(mut edges) =
            timed_hop(conn, budget.deadline, &frontier, &visited, types, reach, budget.edges as i64 + 1).await?
        else {
            truncated = Some(Truncation::Timeout);
            break;
        };
        if edges.len() > budget.edges {
            edges.truncate(budget.edges);
            truncated = Some(Truncation::MaxEdges);
        }
        budget.edges -= edges.len();
        let mut next = Vec::new();
        for e in edges {
            if seen.contains(&e.next_id) {
                continue;
            }
            if !budget.returned.contains(&e.next_id) && budget.returned.len() >= budget.max_nodes {
                truncated.get_or_insert(Truncation::MaxNodes);
                continue;
            }
            budget.returned.insert(e.next_id);
            seen.insert(e.next_id);
            visited.push(e.next_id);
            next.push(e.next_id);
            nodes.push(Reached {
                id: e.next_id,
                class_id: e.next_class_id,
                hops: hop,
                via: Via {
                    parent_id: e.from_id,
                    relationship_id: e.id,
                    relationship_type_id: e.relationship_type_id,
                    edge_source_id: e.source_ci_id,
                    edge_target_id: e.target_ci_id,
                },
                reached_by: 0,
            });
        }
        if truncated.is_some() {
            break;
        }
        frontier = next;
        completed_depth = hop == opts.depth;
    }

    // Is there more beyond the requested depth? One LIMIT 1 probe, visible CIs only.
    let mut more_beyond_depth = false;
    if truncated.is_none() && completed_depth && !frontier.is_empty() {
        more_beyond_depth = matches!(
            timed_hop(conn, budget.deadline, &frontier, &visited, types, reach, 1).await?,
            Some(rows) if !rows.is_empty()
        );
    }
    Ok((Walk { way, nodes, truncated, more_beyond_depth }, visited))
}

/// Runs the walks from `roots` (live CIs the caller may view, checked by the
/// caller) on `conn`, which should be in a REPEATABLE READ transaction.
pub async fn traverse(conn: &mut PgConnection, roots: &[Uuid], opts: &Options<'_>) -> Result<Traversal, AppError> {
    bound(conn, assembly_deadline(opts.deadline)).await?;
    let types = data::types(conn).await.map_err(db_error)?;
    let counts_deadline = opts.deadline + ASSEMBLY_ALLOWANCE / 2;
    let mut budget = Budget {
        deadline: opts.deadline,
        edges: opts.max_nodes.saturating_mul(EDGES_PER_NODE),
        max_nodes: opts.max_nodes,
        returned: HashSet::new(),
    };

    let mut walks = Vec::new();
    for &way in opts.ways {
        let hop_types = hop_types(&types, opts.types, way);
        let (mut w, visited) = walk(conn, roots, way, &hop_types, opts, &mut budget).await?;

        // How many propagating edges lead into each CI from the walk's CIs.
        // Out of time or cancelled (a pathological graph or a slow database):
        // each CI counts its own via edge only.
        let ids: Vec<Uuid> = w.nodes.iter().map(|n| n.id).collect();
        let counts: HashMap<Uuid, i64> = match remaining_ms(counts_deadline) {
            None => HashMap::new(),
            Some(ms) => {
                #[cfg(test)]
                CANCEL_AT.with_borrow_mut(|c| c.last_mut().expect("recorded by remaining_ms").1 = true);
                let mut sp = conn.begin().await.map_err(db_error)?;
                data::set_statement_timeout(&mut sp, ms).await.map_err(db_error)?;
                let counted = async {
                    #[cfg(test)]
                    if SLOW_COUNTS.lock().unwrap().iter().any(|r| roots.contains(r)) {
                        sqlx::query("SELECT pg_sleep(60)").execute(&mut *sp).await?;
                    }
                    data::in_edge_counts(&mut sp, &ids, &visited, &hop_types).await
                };
                // Rolled back either way, as a hop: the counts' timeout must
                // not bound the summaries after them.
                match counted.await {
                    Ok(rows) => {
                        sp.rollback().await.map_err(db_error)?;
                        rows.into_iter().collect()
                    }
                    Err(e) if is_query_canceled(&e) => {
                        sp.rollback().await.map_err(db_error)?;
                        HashMap::new()
                    }
                    Err(e) => return Err(e.into()),
                }
            }
        };
        for n in &mut w.nodes {
            n.reached_by = counts.get(&n.id).copied().unwrap_or(1).max(1);
        }
        if let Some(classes) = opts.result_classes {
            w.nodes.retain(|n| classes.contains(&n.class_id));
        }
        walks.push(w);
    }
    Ok(Traversal { walks, types })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A statement cancelled by the deadline rolls back to the hop's savepoint:
    /// the snapshot stays usable for the rest of the analysis.
    #[tokio::test]
    async fn a_cancelled_hop_leaves_the_snapshot_usable() {
        let Some(db) = crate::db::scratch::database("a_cancelled_hop_leaves_the_snapshot_usable").await else { return };
        let mut tx = db.pool.begin().await.unwrap();
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY").execute(&mut *tx).await.unwrap();
        let before: i64 =
            sqlx::query_scalar("SELECT count(*) FROM relationship_types").fetch_one(&mut *tx).await.unwrap();
        let mut sp = tx.begin().await.unwrap();
        data::set_statement_timeout(&mut sp, 1).await.unwrap();
        let err = sqlx::query("SELECT pg_sleep(1)").execute(&mut *sp).await.unwrap_err();
        assert!(is_query_canceled(&err), "{err}");
        sp.rollback().await.unwrap();
        // Another session's change is not seen: still the same snapshot.
        sqlx::query(
            "INSERT INTO relationship_types (key, name, forward_label, reverse_label) VALUES ('x', 'x', 'x', 'x')",
        )
        .execute(&db.pool)
        .await
        .unwrap();
        let after: i64 =
            sqlx::query_scalar("SELECT count(*) FROM relationship_types").fetch_one(&mut *tx).await.unwrap();
        let isolation: String = sqlx::query_scalar("SHOW transaction_isolation").fetch_one(&mut *tx).await.unwrap();
        assert_eq!((after, isolation.as_str()), (before, "repeatable read"));
        tx.commit().await.unwrap();
        db.drop().await;
    }

    /// GH#825: `set_config` survives a released savepoint. A hop's timeout,
    /// the time left of the walks, must not bound the statements after it
    /// (a SAVEPOINT cancelled by it aborted the analysis with 500).
    #[tokio::test]
    async fn a_hop_leaves_the_statement_timeout_as_it_found_it() {
        let Some(db) = crate::db::scratch::database("a_hop_leaves_the_statement_timeout_as_it_found_it").await else {
            return;
        };
        let mut tx = db.pool.begin().await.unwrap();
        data::set_statement_timeout(&mut tx, 60_000).await.unwrap();
        let root = [Uuid::new_v4()];
        let reach = Reach { visible: None, include_inactive: true };
        let deadline = Instant::now() + Duration::from_secs(5);
        let rows = timed_hop(&mut tx, deadline, &root, &root, &HopTypes::default(), reach, 10).await.unwrap();
        assert_eq!(rows.map(|r| r.len()), Some(0), "the hop ran");
        let timeout: String = sqlx::query_scalar("SHOW statement_timeout").fetch_one(&mut *tx).await.unwrap();
        assert_eq!(timeout, "1min");
        tx.rollback().await.unwrap();
        db.drop().await;
    }

    fn t(id: u128, dir: &str) -> TypeRow {
        TypeRow {
            id: Uuid::from_u128(id),
            key: format!("t{id}"),
            name: String::new(),
            forward_label: String::new(),
            reverse_label: String::new(),
            impact_direction: dir.into(),
        }
    }

    #[test]
    fn hop_types_follow_the_flow_downstream_and_against_it_upstream() {
        let types = [t(1, "target_to_source"), t(2, "source_to_target"), t(3, "both"), t(4, "none")];
        let id = Uuid::from_u128;
        let down = hop_types(&types, None, Way::Downstream);
        assert_eq!((down.to_source, down.to_target), (vec![id(1), id(3)], vec![id(2), id(3)]));
        let up = hop_types(&types, None, Way::Upstream);
        assert_eq!((up.to_source, up.to_target), (vec![id(2), id(3)], vec![id(1), id(3)]));
        // A requested type set narrows it; a `none` type contributes nothing.
        let only = hop_types(&types, Some(&[id(1), id(4)]), Way::Downstream);
        assert_eq!((only.to_source, only.to_target), (vec![id(1)], vec![]));
        assert!(hop_types(&types, Some(&[id(4)]), Way::Upstream).is_empty());
    }
}
