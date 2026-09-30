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
use crate::http::error::AppError;

/// Relationship rows one analysis may read per `maxNodes`, over all hops
/// (the same budget as the relationship graph, GH#185).
pub const EDGES_PER_NODE: usize = 5;

/// Time the queries that assemble a result (in-edge counts) get when the
/// deadline has passed: they are bounded by the result's size, not the graph's.
pub const ASSEMBLY_TIMEOUT: Duration = Duration::from_secs(2);

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
    /// Wall-clock budget of the walks, from the start of [`traverse`].
    pub timeout: Duration,
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
fn hop_types(types: &[TypeRow], wanted: Option<&[Uuid]>, way: Way) -> HopTypes {
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

fn is_query_canceled(err: &sqlx::Error) -> bool {
    err.as_database_error().and_then(|e| e.code()).is_some_and(|c| c == "57014")
}

/// The time left, in whole milliseconds (at least 1), or `None` once it has run out.
fn remaining_ms(deadline: Instant) -> Option<u64> {
    let left = deadline.checked_duration_since(Instant::now())?;
    let ms = left.as_millis() as u64;
    (ms > 0).then_some(ms)
}

/// One hop's query under the deadline, in a savepoint so a cancelled
/// statement leaves the snapshot usable. `Ok(None)`: the deadline hit.
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
    let mut sp = conn.begin().await?;
    data::set_statement_timeout(&mut sp, ms).await?;
    match data::hop(&mut sp, frontier, visited, types, reach, limit).await {
        Ok(rows) => {
            sp.commit().await?;
            Ok(Some(rows))
        }
        Err(e) if is_query_canceled(&e) => {
            sp.rollback().await?;
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
    let start = Instant::now();
    let types = data::types(conn).await?;
    let mut budget = Budget {
        deadline: start + opts.timeout,
        edges: opts.max_nodes.saturating_mul(EDGES_PER_NODE),
        max_nodes: opts.max_nodes,
        returned: HashSet::new(),
    };

    let mut walks = Vec::new();
    for &way in opts.ways {
        let hop_types = hop_types(&types, opts.types, way);
        let (mut w, visited) = walk(conn, roots, way, &hop_types, opts, &mut budget).await?;

        // How many propagating edges lead into each CI from the walk's CIs.
        let ids: Vec<Uuid> = w.nodes.iter().map(|n| n.id).collect();
        let ms = remaining_ms(budget.deadline).unwrap_or(0).max(ASSEMBLY_TIMEOUT.as_millis() as u64);
        let mut sp = conn.begin().await?;
        data::set_statement_timeout(&mut sp, ms).await?;
        // Cancelled (a pathological graph): each CI counts its own via edge only.
        let counts: HashMap<Uuid, i64> = match data::in_edge_counts(&mut sp, &ids, &visited, &hop_types).await {
            Ok(rows) => {
                sp.commit().await?;
                rows.into_iter().collect()
            }
            Err(e) if is_query_canceled(&e) => {
                sp.rollback().await?;
                HashMap::new()
            }
            Err(e) => return Err(e.into()),
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
