import { useState } from "react";
import { useGraph, type Ci, type RelationshipGraph } from "../../api/queries";
import { StatusBadge } from "../../components/Badge";
import { CiLink } from "../../components/CiLink";
import { EmptyState, ErrorAlert, Loading } from "../../components/States";
import type { TrailStep } from "../../lib/trail";

type Direction = "both" | "outgoing" | "incoming";
type Node = RelationshipGraph["nodes"][number];
type Edge = RelationshipGraph["edges"][number];

/**
 * Multi-hop view from GET /configuration-items/{id}/graph, drawn as an indented
 * tree (App → runs on → VM → runs on → Server → is located in → Rack). Every
 * node is a link, so the operator can keep walking.
 */
export function RelationshipGraphPanel({ ci, self, trail }: { ci: Ci; self: TrailStep; trail: TrailStep[] }) {
  const [depth, setDepth] = useState(3);
  const [direction, setDirection] = useState<Direction>("outgoing");
  const graph = useGraph(ci.id, depth, direction);

  return (
    <section className="panel">
      <div className="toolbar">
        <div className="field">
          <label htmlFor="g-dir">Direction</label>
          <select id="g-dir" style={{ width: 260 }} value={direction} onChange={(e) => setDirection(e.target.value as Direction)}>
            <option value="outgoing">Outgoing — what this CI needs</option>
            <option value="incoming">Incoming — what needs this CI</option>
            <option value="both">Both directions</option>
          </select>
        </div>
        <div className="field">
          <label htmlFor="g-depth">Depth</label>
          <select id="g-depth" value={depth} onChange={(e) => setDepth(Number(e.target.value))}>
            {[1, 2, 3, 4, 5, 6].map((d) => (
              <option key={d} value={d}>
                {d} {d === 1 ? "hop" : "hops"}
              </option>
            ))}
          </select>
        </div>
        {graph.isFetching && !graph.isLoading && <span className="spinner" aria-label="Refreshing" />}
      </div>
      {graph.isLoading && <Loading label="Walking the relationship graph…" />}
      {graph.isError && (
        <div className="panel-body">
          <ErrorAlert error={graph.error} onRetry={() => graph.refetch()} />
        </div>
      )}
      {graph.data && graph.data.edges.length === 0 && <EmptyState title="Nothing connected in this direction">Try “Both directions”, or add relationships on the Overview tab.</EmptyState>}
      {graph.data && graph.data.edges.length > 0 && (
        <div className="panel-body">
          {graph.data.truncated && (
            <div className="alert alert-warn">The graph was truncated by the API's node limit. Reduce the depth for a complete picture.</div>
          )}
          <GraphTree graph={graph.data} rootId={ci.id} direction={direction} self={self} trail={trail} />
          <p className="muted" style={{ marginTop: "var(--sp-4)" }}>
            {graph.data.nodes.length} CIs, {graph.data.edges.length} relationships within {depth} {depth === 1 ? "hop" : "hops"}.
          </p>
        </div>
      )}
    </section>
  );
}

function GraphTree({
  graph,
  rootId,
  direction,
  self,
  trail,
}: {
  graph: RelationshipGraph;
  rootId: string;
  direction: Direction;
  self: TrailStep;
  trail: TrailStep[];
}) {
  const nodes = new Map(graph.nodes.map((n) => [n.id, n]));
  const adjacency = new Map<string, { edge: Edge; otherId: string; label: string }[]>();
  const add = (from: string, item: { edge: Edge; otherId: string; label: string }) => adjacency.set(from, [...(adjacency.get(from) ?? []), item]);
  for (const e of graph.edges) {
    if (direction !== "incoming") add(e.sourceCiId, { edge: e, otherId: e.targetCiId, label: e.type.forwardLabel });
    if (direction !== "outgoing") add(e.targetCiId, { edge: e, otherId: e.sourceCiId, label: e.type.isDirectional ? e.type.reverseLabel : e.type.forwardLabel });
  }

  const seen = new Set<string>([rootId]);
  const render = (id: string, level: number): React.ReactNode => {
    const children = (adjacency.get(id) ?? []).filter((c) => nodes.has(c.otherId));
    if (children.length === 0) return null;
    return (
      <ul style={{ listStyle: "none", margin: 0, paddingLeft: level === 0 ? 0 : 22 }}>
        {children.map(({ edge, otherId, label }) => {
          const n = nodes.get(otherId)!;
          const repeat = seen.has(otherId);
          seen.add(otherId);
          return (
            <li key={`${edge.id}-${otherId}`} style={{ padding: "3px 0" }}>
              <span className="muted">{label} → </span>
              <NodeLabel node={n} self={self} trail={trail} />
              {repeat ? <span className="muted"> (shown above)</span> : render(otherId, level + 1)}
            </li>
          );
        })}
      </ul>
    );
  };
  const root = nodes.get(rootId);
  return (
    <div>
      {root && (
        <div style={{ fontWeight: 600, marginBottom: 4 }}>
          {root.name} <span className="muted">({root.class.name})</span>
        </div>
      )}
      {render(rootId, 0)}
    </div>
  );
}

function NodeLabel({ node, self, trail }: { node: Node; self: TrailStep; trail: TrailStep[] }) {
  return (
    <>
      <CiLink id={node.id} from={self} trail={trail}>
        {node.name}
      </CiLink>{" "}
      <span className="muted">{node.class.name}</span> <StatusBadge status={node.status} />
      {node.hostname && <span className="muted mono"> {node.hostname}</span>}
    </>
  );
}
