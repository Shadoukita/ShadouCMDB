//! Impact analysis of one CI: validation, the traversal in one snapshot, and
//! the result as JSON or CSV.

use std::collections::{BTreeMap, HashMap};

use chrono::Utc;
use serde_json::json;
use sqlx::PgPool;
use tokio::time::Instant;
use uuid::Uuid;

use super::engine::{self, Options, Reached, Traversal, Truncation, Way, bound, db_error};
use super::schemas::{
    AnalysisDirection, ClassCount, CriticalityCount, HopCount, ImpactAnalysis, ImpactCriticality, ImpactItem,
    ImpactLimits, ImpactParameters, ImpactQuery, ImpactRelationshipType, ImpactRoot, ImpactSettings, ImpactStatus,
    ImpactSummary, ImpactVia, ReachedDirection, TruncatedReason, Visibility,
};
use super::{ImpactDirection, ImpactState};
use crate::api::context::RequestContext;
use crate::auth::permissions::ClassOp;
use crate::data::crud::{self, AuditAction, AuditEntry};
use crate::data::impact as data;
use crate::data::items::{self as items_data, SummaryRow};
use crate::http::error::{AppError, FieldError, FieldLocation};
use crate::modules::classes::AttributeDataType;
use crate::modules::csv_safe;
use crate::schema::model::Model;

/// Defaults of `depth` and `maxNodes` (lowered to the configured limits).
pub const DEFAULT_DEPTH: i32 = 3;
pub const DEFAULT_MAX_NODES: i32 = 500;
/// Most relationship types one request may name.
pub const MAX_TYPE_IDS: usize = 50;

fn query_error(field: &str, message: String, code: &str) -> FieldError {
    FieldError { location: FieldLocation::Query, field: field.into(), message, code: code.into() }
}

/// The query with its defaults applied and every bound checked. Out-of-range
/// values are refused, never lowered, so a probe learns nothing new.
struct Checked {
    direction: AnalysisDirection,
    depth: i32,
    types: Option<Vec<Uuid>>,
    include_inactive: bool,
    max_nodes: i32,
}

fn check(q: &ImpactQuery, state: &ImpactState) -> Result<Checked, AppError> {
    let limits = state.config;
    let mut errors = Vec::new();
    let depth = q.depth.unwrap_or(DEFAULT_DEPTH.min(limits.max_depth));
    // The schema refuses these already; checked again so the engine never sees them.
    if depth < 1 {
        errors.push(query_error("depth", "Too small: expected number to be >=1".into(), "too_small"));
    }
    if depth > limits.max_depth {
        errors.push(query_error(
            "depth",
            format!("Must be at most {} (IMPACT_MAX_DEPTH)", limits.max_depth),
            "too_big",
        ));
    }
    let max_nodes = q.max_nodes.unwrap_or(DEFAULT_MAX_NODES.min(limits.max_nodes));
    if max_nodes < 1 {
        errors.push(query_error("maxNodes", "Too small: expected number to be >=1".into(), "too_small"));
    }
    if max_nodes > limits.max_nodes {
        errors.push(query_error(
            "maxNodes",
            format!("Must be at most {} (IMPACT_MAX_NODES)", limits.max_nodes),
            "too_big",
        ));
    }
    let types = q.relationship_type_id.as_ref().map(|l| {
        let mut ids: Vec<Uuid> = Vec::with_capacity(l.0.len());
        for id in &l.0 {
            if !ids.contains(id) {
                ids.push(*id);
            }
        }
        ids
    });
    if types.as_ref().is_some_and(|t| t.len() > MAX_TYPE_IDS) {
        errors.push(query_error(
            "relationshipTypeId",
            format!("Too big: expected at most {MAX_TYPE_IDS} relationship types"),
            "too_big",
        ));
    }
    if !errors.is_empty() {
        return Err(AppError::validation(errors));
    }
    Ok(Checked { direction: q.direction, depth, types, include_inactive: q.include_inactive.into(), max_nodes })
}

/// A finished analysis with what the JSON and the CSV are built from.
pub struct Analysis {
    pub result: ImpactAnalysis,
    /// Ident of every CI in the result (root included), for paths.
    idents: HashMap<Uuid, String>,
    /// Per walk, the CI each CI was reached from; and the walk of each item's `via`.
    parents: HashMap<(Way, Uuid), Uuid>,
    chosen: HashMap<Uuid, Way>,
}

fn criticality(r: &SummaryRow) -> Option<ImpactCriticality> {
    match (&r.criticality_key, &r.criticality_name, r.criticality_rank) {
        (Some(key), Some(name), Some(rank)) => Some(ImpactCriticality { key: key.clone(), label: name.clone(), rank }),
        _ => None,
    }
}

/// The value of each CI's lookup attribute keyed `status`, when its class has
/// one. Every statement ends by `until`: `statement_timeout` limits each
/// statement, so it is set again before each one.
async fn statuses(
    conn: &mut sqlx::PgConnection,
    items: &[(Uuid, Uuid)],
    until: Instant,
) -> Result<HashMap<Uuid, ImpactStatus>, AppError> {
    bound(conn, until).await?;
    let areas = Model::load_areas(conn).await.map_err(db_error)?;
    bound(conn, until).await?;
    let classes = Model::load_classes(conn).await.map_err(db_error)?;
    bound(conn, until).await?;
    let fields = Model::load_fields(conn).await.map_err(db_error)?;
    let model = Model { areas, classes, fields };
    // CIs grouped by the status field their class has (own or inherited).
    let mut by_field: BTreeMap<Uuid, Vec<Uuid>> = BTreeMap::new();
    let mut field_of_class: HashMap<Uuid, Option<Uuid>> = HashMap::new();
    for &(id, class_id) in items {
        let field = *field_of_class.entry(class_id).or_insert_with(|| {
            model.lineage(class_id).iter().find_map(|c| {
                model
                    .own_fields(c.id)
                    .find(|f| f.key == "status" && f.data_type == AttributeDataType::Lookup)
                    .map(|f| f.id)
            })
        });
        if let Some(f) = field {
            by_field.entry(f).or_default().push(id);
        }
    }
    let mut value_of: HashMap<Uuid, Uuid> = HashMap::new();
    for (field_id, ids) in by_field {
        let Some(field) = model.fields.iter().find(|f| f.id == field_id) else { continue };
        let Some(table) = model.table(field.class_id) else { continue };
        bound(conn, until).await?;
        value_of.extend(data::lookup_column_values(conn, &table, &field.column(), &ids).await.map_err(db_error)?);
    }
    let mut values: Vec<Uuid> = value_of.values().copied().collect();
    values.sort_unstable();
    values.dedup();
    bound(conn, until).await?;
    let names: HashMap<Uuid, data::LookupValue> =
        data::lookup_values(conn, &values).await.map_err(db_error)?.into_iter().map(|v| (v.id, v)).collect();
    Ok(value_of
        .into_iter()
        .filter_map(|(ci, v)| {
            let v = names.get(&v)?;
            Some((ci, ImpactStatus { key: v.key.clone(), label: v.name.clone() }))
        })
        .collect())
}

/// A CI as the walks found it: the chosen (shorter) path and the other one.
struct Merged {
    class_id: Uuid,
    directions: Vec<ReachedDirection>,
    chosen: (Way, Reached),
    other: Option<(Way, Reached)>,
}

fn reached_direction(w: Way) -> ReachedDirection {
    match w {
        Way::Downstream => ReachedDirection::Downstream,
        Way::Upstream => ReachedDirection::Upstream,
    }
}

fn merge(t: &Traversal) -> Vec<(Uuid, Merged)> {
    let mut order: Vec<Uuid> = Vec::new();
    let mut merged: HashMap<Uuid, Merged> = HashMap::new();
    for walk in &t.walks {
        for n in &walk.nodes {
            match merged.get_mut(&n.id) {
                None => {
                    order.push(n.id);
                    merged.insert(
                        n.id,
                        Merged {
                            class_id: n.class_id,
                            directions: vec![reached_direction(walk.way)],
                            chosen: (walk.way, n.clone()),
                            other: None,
                        },
                    );
                }
                Some(m) => {
                    m.directions.push(reached_direction(walk.way));
                    m.directions.sort();
                    // Downstream runs first, so a tie keeps it.
                    if n.hops < m.chosen.1.hops {
                        let old = std::mem::replace(&mut m.chosen, (walk.way, n.clone()));
                        m.other = Some(old);
                    } else {
                        m.other = Some((walk.way, n.clone()));
                    }
                }
            }
        }
    }
    order.into_iter().filter_map(|id| merged.remove(&id).map(|m| (id, m))).collect()
}

/// Runs the analysis of `root_id` for the caller.
pub async fn analyse(
    pool: &PgPool,
    ctx: &RequestContext,
    state: &std::sync::Arc<ImpactState>,
    root_id: Uuid,
    q: &ImpactQuery,
) -> Result<Analysis, AppError> {
    let started = Instant::now();
    let p = check(q, state)?;
    let visible = ctx.class_scope(ClassOp::View);
    let permit = state.acquire(ctx)?;
    // The work runs in its own task, which holds the place. If the request is
    // dropped (the client disconnects, the request times out), the task still
    // runs to its deadline, so the place is not freed while its queries hold a
    // pool connection (GitHub #337).
    let (pool, ctx, state) = (pool.clone(), ctx.clone(), state.clone());
    tokio::spawn(async move {
        let _permit = permit;
        run(&pool, &ctx, &state, root_id, p, visible, started).await
    })
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "impact analysis task failed");
        AppError::internal()
    })?
}

async fn run(
    pool: &PgPool,
    ctx: &RequestContext,
    state: &ImpactState,
    root_id: Uuid,
    p: Checked,
    visible: Option<Vec<Uuid>>,
    started: Instant,
) -> Result<Analysis, AppError> {
    // The walks stop at IMPACT_TIMEOUT_MS from the start of the request; every
    // statement, before and after them, ends within the assembly allowance
    // after that (GH#393).
    let deadline = started + state.config.timeout;
    let assembly = engine::assembly_deadline(deadline);
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY").execute(&mut *tx).await?;
    bound(&mut tx, assembly).await?;
    // A missing, deleted or hidden root answers alike.
    let root = match items_data::summary(&mut tx, root_id).await.map_err(db_error)? {
        Some(r) if r.deleted_at.is_none() => r,
        _ => return Err(AppError::missing("Configuration item", root_id)),
    };
    ctx.require_class_visible(root.class_id, "Configuration item", root_id)?;

    // Every named type must exist (the list is read again in the walk's snapshot).
    bound(&mut tx, assembly).await?;
    let all_types = data::types(&mut tx).await.map_err(db_error)?;
    if let Some(ids) = &p.types
        && let Some(unknown) = ids.iter().find(|id| !all_types.iter().any(|t| t.id == **id))
    {
        return Err(AppError::validation(vec![query_error(
            "relationshipTypeId",
            format!("Relationship type {unknown} not found"),
            "not_found",
        )]));
    }

    let ways: &[Way] = match p.direction {
        AnalysisDirection::Downstream => &[Way::Downstream],
        AnalysisDirection::Upstream => &[Way::Upstream],
        AnalysisDirection::Both => &[Way::Downstream, Way::Upstream],
    };
    let opts = Options {
        ways,
        depth: p.depth,
        types: p.types.as_deref(),
        include_inactive: p.include_inactive,
        max_nodes: p.max_nodes as usize,
        deadline,
        visible: visible.as_deref(),
        result_classes: None,
    };
    let traversal = engine::traverse(&mut tx, &[root_id], &opts).await?;
    let merged = merge(&traversal);
    let parents: HashMap<(Way, Uuid), Uuid> =
        traversal.walks.iter().flat_map(|w| w.nodes.iter().map(move |n| ((w.way, n.id), n.via.parent_id))).collect();
    let chosen: HashMap<Uuid, Way> = merged.iter().map(|(id, m)| (*id, m.chosen.0)).collect();

    // Summaries of the result (the snapshot guarantees every row is there).
    bound(&mut tx, assembly).await?;
    let ids: Vec<Uuid> = merged.iter().map(|(id, _)| *id).collect();
    let rows: HashMap<Uuid, SummaryRow> =
        items_data::summaries(&mut tx, &ids).await.map_err(db_error)?.into_iter().map(|r| (r.id, r)).collect();
    let pairs: Vec<(Uuid, Uuid)> = merged.iter().map(|(id, m)| (*id, m.class_id)).collect();
    let status_of = statuses(&mut tx, &pairs, assembly).await?;
    tx.commit().await?;

    let type_of: HashMap<Uuid, &data::TypeRow> = traversal.types.iter().map(|t| (t.id, t)).collect();
    let via_dto = |v: &engine::Via| -> Result<ImpactVia, AppError> {
        let t = type_of.get(&v.relationship_type_id).ok_or_else(AppError::internal)?;
        Ok(ImpactVia {
            parent_id: v.parent_id,
            relationship_id: v.relationship_id,
            relationship_type: ImpactRelationshipType {
                id: t.id,
                key: t.key.clone(),
                name: t.name.clone(),
                forward_label: t.forward_label.clone(),
                reverse_label: t.reverse_label.clone(),
            },
            edge_source_id: v.edge_source_id,
            edge_target_id: v.edge_target_id,
        })
    };

    let mut idents: HashMap<Uuid, String> = HashMap::from([(root.id, root.ident.clone())]);
    let mut items = Vec::with_capacity(merged.len());
    for (id, m) in merged {
        let row = rows.get(&id).ok_or_else(AppError::internal)?;
        idents.insert(id, row.ident.clone());
        let (chosen_way, chosen) = &m.chosen;
        let other_via = match &m.other {
            Some((_, o)) if o.via != chosen.via => Some(via_dto(&o.via)?),
            _ => None,
        };
        let (upstream_via, downstream_via) = match chosen_way {
            Way::Downstream => (other_via, None),
            Way::Upstream => (None, other_via),
        };
        items.push(ImpactItem {
            id,
            ident: row.ident.clone(),
            name: row.label.clone(),
            class_id: row.class_id,
            class_name: row.class_name.clone(),
            criticality: criticality(row),
            active: row.active,
            status: status_of.get(&id).cloned(),
            directions: m.directions,
            hops: chosen.hops,
            via: via_dto(&chosen.via)?,
            upstream_via,
            downstream_via,
            reached_by_count: chosen.reached_by,
        });
    }
    items.sort_by(|a, b| {
        a.hops
            .cmp(&b.hops)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.id.cmp(&b.id))
    });

    let relationship_type_ids = match &p.types {
        Some(ids) => ids.clone(),
        None => traversal
            .types
            .iter()
            .filter(|t| t.impact_direction != ImpactDirection::None.as_str())
            .map(|t| t.id)
            .collect(),
    };
    let truncated = traversal.truncated();
    let result = ImpactAnalysis {
        root: ImpactRoot {
            id: root.id,
            ident: root.ident.clone(),
            name: root.label.clone(),
            class_id: root.class_id,
            class_name: root.class_name.clone(),
            criticality: criticality(&root),
            active: root.active,
        },
        parameters: ImpactParameters {
            direction: p.direction,
            depth: p.depth,
            relationship_type_ids,
            include_inactive: p.include_inactive,
            max_nodes: p.max_nodes,
        },
        summary: summary(&items),
        items,
        truncated: truncated.is_some(),
        truncated_reason: truncated.map(|t| match t {
            Truncation::MaxNodes => TruncatedReason::MaxNodes,
            Truncation::MaxEdges => TruncatedReason::MaxEdges,
            Truncation::Timeout => TruncatedReason::Timeout,
        }),
        has_more_beyond_depth: traversal.walks.iter().any(|w| w.more_beyond_depth),
        // From the caller's permissions only, never from the data.
        visibility: if visible.is_some() { Visibility::Restricted } else { Visibility::AllClasses },
        limits: ImpactLimits { max_depth: state.config.max_depth, max_nodes: state.config.max_nodes },
        elapsed_ms: started.elapsed().as_millis() as u64,
    };
    Ok(Analysis { result, idents, parents, chosen })
}

fn summary(items: &[ImpactItem]) -> ImpactSummary {
    let mut by_class: BTreeMap<(String, Uuid), i64> = BTreeMap::new();
    let mut by_criticality: BTreeMap<(i64, Option<String>), i64> = BTreeMap::new();
    let mut by_hops: BTreeMap<i32, i64> = BTreeMap::new();
    for i in items {
        *by_class.entry((i.class_name.clone(), i.class_id)).or_default() += 1;
        let key = match &i.criticality {
            Some(c) => (c.rank, Some(c.key.clone())),
            None => (i64::MAX, None),
        };
        *by_criticality.entry(key).or_default() += 1;
        *by_hops.entry(i.hops).or_default() += 1;
    }
    let mut by_class: Vec<ClassCount> = by_class
        .into_iter()
        .map(|((class_name, class_id), count)| ClassCount { class_id, class_name, count })
        .collect();
    by_class.sort_by(|a, b| {
        a.class_name.to_lowercase().cmp(&b.class_name.to_lowercase()).then_with(|| a.class_id.cmp(&b.class_id))
    });
    ImpactSummary {
        total: items.len() as i64,
        by_class,
        by_criticality: by_criticality.into_iter().map(|((_, key), count)| CriticalityCount { key, count }).collect(),
        by_hops: by_hops.into_iter().map(|(hops, count)| HopCount { hops, count }).collect(),
    }
}

// ---------------------------------------------------------------------------
// CSV export
// ---------------------------------------------------------------------------

/// A CSV record through [`csv_safe`]: every cell quoted, formulas neutralised
/// (feature requirement I8).
fn row(cells: &[&str]) -> String {
    let mut line = String::new();
    csv_safe::write_record(&mut line, ',', cells.iter().copied());
    line
}

pub const CSV_COLUMNS: &[&str] = &[
    "ci_id",
    "ident",
    "name",
    "class",
    "criticality",
    "direction",
    "hops",
    "via_relationship",
    "via_ci_ident",
    "path_idents",
    "active",
    "status",
];

fn direction_word(d: AnalysisDirection) -> &'static str {
    match d {
        AnalysisDirection::Downstream => "downstream",
        AnalysisDirection::Upstream => "upstream",
        AnalysisDirection::Both => "both",
    }
}

/// The CSV of an analysis: a comment row with the root, the parameters,
/// truncation and the visibility note, the header, then one row per item.
pub fn csv(a: &Analysis) -> String {
    let r = &a.result;
    let p = &r.parameters;
    let one_line = |s: &str| s.replace(['\r', '\n'], " ");
    let mut comment = format!(
        "# Impact analysis of {} ({}): direction={}, depth={}, relationshipTypeIds={}, includeInactive={}, \
         maxNodes={}; truncated={}",
        r.root.ident,
        one_line(&r.root.name),
        direction_word(p.direction),
        p.depth,
        p.relationship_type_ids.iter().map(Uuid::to_string).collect::<Vec<_>>().join(" "),
        p.include_inactive,
        p.max_nodes,
        r.truncated,
    );
    if let Some(reason) = r.truncated_reason {
        comment.push_str(match reason {
            TruncatedReason::MaxNodes => " (max_nodes)",
            TruncatedReason::MaxEdges => " (max_edges)",
            TruncatedReason::Timeout => " (timeout)",
        });
    }
    if r.visibility == Visibility::Restricted {
        comment.push_str("; results include only CIs of classes you are allowed to view");
    }
    let mut out = row(&[&comment]);
    out.push_str(&row(CSV_COLUMNS));

    for i in &r.items {
        // root -> ... -> this CI, all in the walk of its `via`.
        let way = a.chosen.get(&i.id).copied().unwrap_or(Way::Downstream);
        let mut path = vec![i.id];
        let mut at = i.id;
        while let Some(parent) = a.parents.get(&(way, at)).copied() {
            if path.contains(&parent) || path.len() > 64 {
                break;
            }
            path.push(parent);
            at = parent;
        }
        let path_idents: Vec<&str> =
            path.iter().rev().map(|id| a.idents.get(id).map(String::as_str).unwrap_or("")).collect();
        let via = &i.via;
        // Read from this CI to its parent: "runs on" when it is the edge's source.
        let via_label = if via.edge_source_id == i.id {
            &via.relationship_type.forward_label
        } else {
            &via.relationship_type.reverse_label
        };
        let directions: Vec<&str> = i
            .directions
            .iter()
            .map(|d| match d {
                ReachedDirection::Downstream => "downstream",
                ReachedDirection::Upstream => "upstream",
            })
            .collect();
        out.push_str(&row(&[
            &i.id.to_string(),
            &i.ident,
            &i.name,
            &i.class_name,
            i.criticality.as_ref().map(|c| c.label.as_str()).unwrap_or(""),
            &directions.join(";"),
            &i.hops.to_string(),
            via_label,
            a.idents.get(&via.parent_id).map(String::as_str).unwrap_or(""),
            &path_idents.join(" > "),
            if i.active { "true" } else { "false" },
            i.status.as_ref().map(|s| s.label.as_str()).unwrap_or(""),
        ]));
    }
    out
}

/// `impact-<root ident>-<direction>-<yyyymmdd-hhmm>.csv` (UTC).
pub fn file_name(a: &Analysis) -> String {
    let ident: String = a
        .result
        .root
        .ident
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' { c } else { '_' })
        .collect();
    format!("impact-{ident}-{}-{}.csv", direction_word(a.result.parameters.direction), Utc::now().format("%Y%m%d-%H%M"))
}

/// Runs the analysis and records the export (action `export`, I10): the
/// parameters and the row count, never the rows.
pub async fn export(
    pool: &PgPool,
    ctx: &RequestContext,
    state: &std::sync::Arc<ImpactState>,
    root_id: Uuid,
    q: &ImpactQuery,
) -> Result<(String, String), AppError> {
    let analysis = analyse(pool, ctx, state, root_id, q).await?;
    let body = csv(&analysis);
    let r = &analysis.result;
    let mut tx = pool.begin().await?;
    let entry = AuditEntry {
        action: AuditAction::Export,
        entity_type: "configuration_items",
        entity_id: root_id,
        old_value: None,
        new_value: Some(json!({
            "kind": "impact",
            "format": "csv",
            "parameters": crud::json(&r.parameters),
            "rowCount": r.items.len(),
            "truncated": r.truncated,
            "truncatedReason": r.truncated_reason,
            "visibility": r.visibility,
        })),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    tx.commit().await?;
    Ok((file_name(&analysis), body))
}

pub async fn settings(pool: &PgPool, state: &ImpactState) -> Result<ImpactSettings, AppError> {
    let mut conn = pool.acquire().await?;
    let c = state.config;
    Ok(ImpactSettings {
        max_depth: c.max_depth,
        max_nodes_limit: c.max_nodes,
        default_depth: DEFAULT_DEPTH.min(c.max_depth),
        default_max_nodes: DEFAULT_MAX_NODES.min(c.max_nodes),
        timeout_ms: c.timeout.as_millis() as u64,
        any_type_propagates: data::any_type_propagates(&mut conn).await?,
    })
}

/// The CSV of an analysis, without recording an export (tests).
#[cfg(test)]
pub async fn csv_of(pool: &PgPool, ctx: &RequestContext, root_id: Uuid, q: &ImpactQuery) -> String {
    let state = std::sync::Arc::new(ImpactState::default());
    match analyse(pool, ctx, &state, root_id, q).await {
        Ok(a) => csv(&a),
        Err(e) => panic!("analysis failed: {e:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GH#388: the shared csv_safe rule, not a weaker local one.
    const FORMULAS: [&str; 14] = [
        "+1",
        "-1",
        "-2+3",
        "@SUM(A1)",
        " =1",
        "\u{a0}=1",
        "\u{3000}+1",
        "＝1",
        "＋1",
        "－1",
        "＠SUM(A1)",
        "\tx",
        "\rx",
        "\nx",
    ];

    #[test]
    fn cells_are_quoted_and_formulas_neutralised() {
        assert_eq!(row(&["web-01", "=HYPERLINK(\"x\")", ""]), "\"web-01\",\"'=HYPERLINK(\"\"x\"\")\",\"\"\r\n");
        for bad in FORMULAS {
            assert_eq!(row(&[bad]), format!("\"'{bad}\"\r\n"), "{bad:?}");
        }
        // A line break inside a value stays inside its quoted cell.
        assert_eq!(row(&["Rack A\nSlot 4", "x"]), "\"Rack A\nSlot 4\",\"x\"\r\n");
    }
}
