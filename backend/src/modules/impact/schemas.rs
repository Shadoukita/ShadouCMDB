//! Request and response types of the impact analysis endpoints.

use serde::{Deserialize, Serialize};
use utoipa::openapi::schema::{ObjectBuilder, Schema, Type};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::api::schemas::{self, QueryBool, UuidList};
use crate::config::{IMPACT_MAX_DEPTH_CEILING, IMPACT_MAX_NODES_CEILING};

// ---------------------------------------------------------------------------
// Query
// ---------------------------------------------------------------------------

/// Which way to analyse, relative to the flow of impact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum AnalysisDirection {
    /// What is affected if the CI fails or changes (the CIs that depend on it)
    #[default]
    Downstream,
    /// What could affect the CI (the CIs it depends on)
    Upstream,
    /// Both walks, run separately; each item says which it was reached in
    Both,
}

fn direction_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(["downstream", "upstream", "both"]))
        .default(Some("downstream".into()))
        .description(Some(
            "downstream: the CIs affected if this CI fails (impact flows along each relationship type's \
             impactDirection); upstream: the CIs this CI depends on (against it); both: the two walks, run separately",
        ))
        .into()
}

fn depth_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::Integer)
        .minimum(Some(1))
        .maximum(Some(IMPACT_MAX_DEPTH_CEILING))
        .description(Some(
            "Hops from the CI; default 3 (or maxDepth when lower). At most `maxDepth` of GET /api/v1/settings/impact \
             (IMPACT_MAX_DEPTH): a larger value is refused, never lowered",
        ))
        .into()
}

fn max_nodes_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::Integer)
        .minimum(Some(1))
        .maximum(Some(IMPACT_MAX_NODES_CEILING))
        .description(Some(
            "Largest number of affected CIs returned; default 500 (or maxNodesLimit when lower). At most \
             `maxNodesLimit` of GET /api/v1/settings/impact (IMPACT_MAX_NODES): a larger value is refused",
        ))
        .into()
}

fn types_schema() -> Schema {
    schemas::uuid_list_described(
        "Relationship types to follow, comma-separated (at most 50); default: every type that propagates impact. A \
         type whose impactDirection is none is accepted and contributes nothing; an unknown id is refused",
    )
}

fn include_inactive_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(["true", "false"]))
        .default(Some("true".into()))
        .description(Some(
            "true: CIs outside their validity period are followed and returned with active=false; false: they are \
             neither returned nor followed",
        ))
        .into()
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ImpactQuery {
    #[param(required = false, schema_with = direction_schema)]
    pub direction: AnalysisDirection,
    #[param(schema_with = depth_schema)]
    pub depth: Option<i32>,
    #[param(schema_with = types_schema)]
    pub relationship_type_id: Option<UuidList>,
    #[param(required = false, schema_with = include_inactive_schema)]
    pub include_inactive: QueryBool,
    #[param(schema_with = max_nodes_schema)]
    pub max_nodes: Option<i32>,
}

// ---------------------------------------------------------------------------
// Response
// ---------------------------------------------------------------------------

/// A criticality value as impact analysis reports it.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ImpactCriticality {
    pub key: String,
    pub label: String,
    /// Position in the criticality list: 1 is the most critical
    pub rank: i64,
}

/// The value of the CI's `status` attribute (a lookup attribute keyed status).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ImpactStatus {
    pub key: String,
    pub label: String,
}

/// The analysed CI.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImpactRoot {
    pub id: Uuid,
    pub ident: String,
    /// The CI's label (display name)
    pub name: String,
    pub class_id: Uuid,
    pub class_name: String,
    #[schema(required = true)]
    pub criticality: Option<ImpactCriticality>,
    /// Inside its validity period
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImpactRelationshipType {
    pub id: Uuid,
    pub key: String,
    pub name: String,
    pub forward_label: String,
    pub reverse_label: String,
}

/// The relationship that first reached a CI: the last hop of its shortest path.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImpactVia {
    /// The CI one hop closer to the root (the root itself at hop 1); always the root or another item
    pub parent_id: Uuid,
    pub relationship_id: Uuid,
    pub relationship_type: ImpactRelationshipType,
    /// The relationship's source (read with forwardLabel: source runs on target)
    pub edge_source_id: Uuid,
    pub edge_target_id: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ReachedDirection {
    Downstream,
    Upstream,
}

/// An affected CI.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImpactItem {
    pub id: Uuid,
    pub ident: String,
    /// The CI's label (display name)
    pub name: String,
    pub class_id: Uuid,
    pub class_name: String,
    #[schema(required = true)]
    pub criticality: Option<ImpactCriticality>,
    pub active: bool,
    /// The value of the class's lookup attribute keyed `status`; null when the class has none or the CI holds none
    #[schema(required = true)]
    pub status: Option<ImpactStatus>,
    /// The walks that reached the CI: one, or both in direction=both
    #[schema(inline)]
    pub directions: Vec<ReachedDirection>,
    /// Hops from the root on the shortest path (in both mode: the shorter direction, downstream on a tie)
    pub hops: i32,
    /// The last hop of that path
    pub via: ImpactVia,
    /// In both mode, for a CI reached both ways: the last hop of the upstream path when `via` is the downstream one
    #[schema(required = true)]
    pub upstream_via: Option<ImpactVia>,
    /// In both mode, for a CI reached both ways: the last hop of the downstream path when `via` is the upstream one
    #[schema(required = true)]
    pub downstream_via: Option<ImpactVia>,
    /// Propagating relationships into this CI from CIs of the result (root included), at least 1: more than 1 means
    /// it is also reached another way
    pub reached_by_count: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImpactParameters {
    #[schema(inline)]
    pub direction: AnalysisDirection,
    pub depth: i32,
    /// The types followed: those asked for, or every type that propagates impact
    pub relationship_type_ids: Vec<Uuid>,
    pub include_inactive: bool,
    pub max_nodes: i32,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClassCount {
    pub class_id: Uuid,
    pub class_name: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CriticalityCount {
    /// null: not set
    #[schema(required = true)]
    pub key: Option<String>,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HopCount {
    pub hops: i32,
    pub count: i64,
}

/// Counts over the returned items only: exact for the response, a lower bound of the whole when truncated.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImpactSummary {
    pub total: i64,
    /// By class name
    #[schema(inline)]
    pub by_class: Vec<ClassCount>,
    /// By rank, most critical first, not set last
    #[schema(inline)]
    pub by_criticality: Vec<CriticalityCount>,
    #[schema(inline)]
    pub by_hops: Vec<HopCount>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TruncatedReason {
    /// maxNodes CIs were collected
    MaxNodes,
    /// The relationships read reached the edge budget (5 × maxNodes)
    MaxEdges,
    /// The analysis ran out of time (IMPACT_TIMEOUT_MS)
    Timeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// The caller may view every class
    AllClasses,
    /// The caller's profile limits the classes they may view: the result may leave CIs out. Says nothing about
    /// whether any were
    Restricted,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImpactLimits {
    pub max_depth: i32,
    pub max_nodes: i32,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImpactAnalysis {
    pub root: ImpactRoot,
    pub parameters: ImpactParameters,
    /// Ordered by hops, then name
    pub items: Vec<ImpactItem>,
    pub summary: ImpactSummary,
    /// The analysis stopped early: `items` holds every CI up to the last complete hop, plus those found in the
    /// partial one
    pub truncated: bool,
    #[schema(required = true, inline)]
    pub truncated_reason: Option<TruncatedReason>,
    /// The requested depth was reached and further CIs lie beyond it (not truncation)
    pub has_more_beyond_depth: bool,
    #[schema(inline)]
    pub visibility: Visibility,
    pub limits: ImpactLimits,
    pub elapsed_ms: u64,
}

/// Impact analysis settings, for clients building the controls.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImpactSettings {
    /// Largest depth (IMPACT_MAX_DEPTH)
    pub max_depth: i32,
    /// Largest maxNodes (IMPACT_MAX_NODES)
    pub max_nodes_limit: i32,
    pub default_depth: i32,
    pub default_max_nodes: i32,
    /// Deadline of one analysis (IMPACT_TIMEOUT_MS)
    pub timeout_ms: u64,
    /// Whether any relationship type propagates impact; false: every analysis is empty until an administrator
    /// configures one
    pub any_type_propagates: bool,
}
