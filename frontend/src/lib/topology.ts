// The CI record's topology panel (design §2.7, audit R6): the CIs around one CI as columns of
// nodes. The root sits in the middle; a CI its relationship points to (it is the source) goes to
// the right, a CI pointing at it to the left, and each further hop one column further out on the
// side of the CI it was reached from. Only the edge a CI was first reached by is drawn, so the
// picture stays a tree; the list under the canvas names every relationship.
import type { RelationshipGraph } from "../api/queries";
import type { ImpactAnalysis } from "./impact";
import { viaFor } from "./impact";

export interface TopoNode {
  id: string;
  label: string;
  classId: string;
  className: string;
  active: boolean;
  deletedAt?: string | null;
  /** Hops from the root; 0 for the root. */
  hops: number;
  /** The CI one hop closer to the root; null for the root. */
  parentId: string | null;
  /** -1 left, 1 right, 0 the root. */
  side: -1 | 0 | 1;
}

export interface TopoEdge {
  /** The parent (closer to the root). */
  from: string;
  to: string;
  /** How the relationship reads from its source ("runs on"); several between one pair, joined. */
  label: string;
  /** Whether the relationship's source is the parent: the arrow points away from the root. */
  outward: boolean;
  /** A directional type: drawn with an arrow at its target. */
  directional: boolean;
  /** A relationship that does not propagate impact (location, backup): drawn dashed. */
  dashed: boolean;
}

export interface Topology {
  rootId: string;
  nodes: TopoNode[];
  edges: TopoEdge[];
}

interface Link {
  otherId: string;
  label: string;
  /** The current node is the relationship's source. */
  isSource: boolean;
  /** Symmetric type: either end reads the same, so no side follows from it. */
  symmetric: boolean;
  dashed: boolean;
}

type GraphNode = RelationshipGraph["nodes"][number];

/**
 * The topology of GET /configuration-items/{id}/graph. `propagates` tells whether a relationship
 * type carries impact (from the relationship types' impact direction); unknown types are drawn solid.
 */
export function topologyFromGraph(graph: RelationshipGraph, propagates: (typeId: string) => boolean | undefined = () => undefined): Topology {
  const byId = new Map(graph.nodes.map((n) => [n.id, n]));
  const links = new Map<string, Link[]>();
  const add = (id: string, l: Link) => links.set(id, [...(links.get(id) ?? []), l]);
  for (const e of graph.edges) {
    const symmetric = !e.type.isDirectional;
    const dashed = propagates(e.relationshipTypeId) === false;
    add(e.sourceCiId, { otherId: e.targetCiId, label: e.type.forwardLabel, isSource: true, symmetric, dashed });
    add(e.targetCiId, { otherId: e.sourceCiId, label: e.type.forwardLabel, isSource: false, symmetric, dashed });
  }
  const root = byId.get(graph.rootId);
  const node = (n: GraphNode, hops: number, parentId: string | null, side: -1 | 0 | 1): TopoNode => ({
    id: n.id,
    label: n.label,
    classId: n.classId,
    className: n.class.name,
    active: n.active,
    deletedAt: n.deletedAt,
    hops,
    parentId,
    side,
  });
  if (!root) return { rootId: graph.rootId, nodes: [], edges: [] };
  const nodes: TopoNode[] = [node(root, 0, null, 0)];
  const placed = new Map<string, TopoNode>([[root.id, nodes[0]]]);
  const edges = new Map<string, TopoEdge>();
  // Breadth-first by the API's hop count, so every CI hangs off a CI one hop closer.
  let frontier = [nodes[0]];
  let balance = 0;
  while (frontier.length > 0) {
    const next: TopoNode[] = [];
    for (const parent of frontier) {
      for (const l of links.get(parent.id) ?? []) {
        const other = byId.get(l.otherId);
        if (other && placed.has(other.id)) {
          // A second relationship to a CI already drawn from this parent: its label joins the edge.
          const e = edges.get(`${parent.id}>${other.id}`);
          if (e && !e.label.split(", ").includes(l.label)) e.label = `${e.label}, ${l.label}`;
          continue;
        }
        if (!other || other.depth !== parent.hops + 1) continue;
        let side: -1 | 1;
        if (parent.side !== 0) side = parent.side;
        else if (!l.symmetric) side = l.isSource ? 1 : -1;
        else side = balance > 0 ? -1 : 1; // a symmetric neighbour of the root goes to the emptier side
        if (parent.side === 0) balance += side;
        const n = node(other, parent.hops + 1, parent.id, side);
        placed.set(n.id, n);
        nodes.push(n);
        next.push(n);
        edges.set(`${parent.id}>${n.id}`, { from: parent.id, to: n.id, label: l.label, outward: l.isSource, directional: !l.symmetric, dashed: l.dashed });
      }
    }
    frontier = next;
  }
  return { rootId: root.id, nodes, edges: [...edges.values()] };
}

/**
 * The topology of an impact analysis (downstream, or upstream when asked): each CI under the CI its
 * shortest path comes from. The response does not carry the root's class id: `rootClassId` gives it.
 */
export function topologyFromImpact(a: Pick<ImpactAnalysis, "root" | "items" | "parameters">, rootClassId: string): Topology {
  const root: TopoNode = { id: a.root.id, label: a.root.name, classId: rootClassId, className: a.root.className, active: true, hops: 0, parentId: null, side: 0 };
  const placed = new Map<string, TopoNode>([[root.id, root]]);
  const edges: TopoEdge[] = [];
  const way = a.parameters.direction === "upstream" ? "upstream" : "downstream";
  // Shortest paths first, so a parent is always placed before its children.
  const items = [...a.items].filter((i) => i.directions.includes(way)).sort((x, y) => x.hops - y.hops);
  for (const item of items) {
    const via = viaFor(item, way);
    const parent = via ? placed.get(via.parentId) : undefined;
    if (!via || !parent || placed.has(item.id)) continue;
    const outward = via.edgeSourceId === parent.id;
    const side: -1 | 1 = parent.side !== 0 ? parent.side : outward ? 1 : -1;
    const n: TopoNode = { id: item.id, label: item.name, classId: item.classId, className: item.className, active: item.active, hops: parent.hops + 1, parentId: parent.id, side };
    placed.set(n.id, n);
    const t = via.relationshipType;
    // Every type an analysis follows propagates impact: solid. The label reads from the edge's source.
    edges.push({ from: parent.id, to: n.id, label: t.forwardLabel, outward, directional: t.forwardLabel !== t.reverseLabel, dashed: false });
  }
  return { rootId: root.id, nodes: [...placed.values()], edges };
}

export interface PlacedNode extends TopoNode {
  x: number;
  y: number;
}
export interface PlacedEdge extends TopoEdge {
  x1: number;
  y1: number;
  x2: number;
  y2: number;
}
export interface TopologyLayout {
  width: number;
  height: number;
  boxWidth: number;
  boxHeight: number;
  nodes: PlacedNode[];
  edges: PlacedEdge[];
  /** Per column, the CIs left out for room ("+3 more"), with the box's position. */
  more: { column: number; count: number; x: number; y: number }[];
  /** CIs left out in total. */
  hidden: number;
}

export const BOX_HEIGHT = 40;
const ROW_GAP = 12;
const PAD = 16;

/**
 * Positions for a canvas `width` pixels wide: one column per hop on each side, at most `maxRows`
 * boxes in a column (the rest become a "+n more" box), children in their parents' order so few
 * edges cross. x and y are the boxes' top-left corners.
 */
export function layoutTopology(topo: Topology, width: number, maxRows = 8): TopologyLayout {
  const depth = Math.max(0, ...topo.nodes.map((n) => n.hops));
  const leftDepth = Math.max(0, ...topo.nodes.filter((n) => n.side < 0).map((n) => n.hops));
  const rightDepth = Math.max(0, ...topo.nodes.filter((n) => n.side > 0).map((n) => n.hops));
  // Both sides get the same number of columns, so the root stays in the middle.
  const half = Math.max(leftDepth, rightDepth, depth > 0 ? 1 : 0);
  const columns = 2 * half + 1;
  const colWidth = (width - 2 * PAD) / columns;
  // Narrower boxes with more columns, so the gaps keep room for the edge labels.
  const boxWidth = Math.round(Math.min(200, colWidth * (columns > 3 ? 0.6 : 0.68)));
  const colX = (c: number) => PAD + (c + half) * colWidth + (colWidth - boxWidth) / 2;

  const order = new Map<string, number>();
  const shown: TopoNode[] = [];
  const more: TopologyLayout["more"] = [];
  let hidden = 0;
  const columnsOf = new Map<number, TopoNode[]>();
  for (const n of topo.nodes) {
    const c = n.side * n.hops;
    columnsOf.set(c, [...(columnsOf.get(c) ?? []), n]);
  }
  // Columns from the root outwards, so a parent's row is known before its children are sorted.
  const keys = [...columnsOf.keys()].sort((a, b) => Math.abs(a) - Math.abs(b));
  const rows = new Map<number, number>();
  for (const c of keys) {
    const members = columnsOf
      .get(c)!
      .filter((n) => n.parentId === null || order.has(n.parentId))
      .sort((a, b) => (order.get(a.parentId ?? "") ?? 0) - (order.get(b.parentId ?? "") ?? 0) || a.label.localeCompare(b.label));
    const lost = columnsOf.get(c)!.length - members.length;
    const fits = members.length > maxRows ? maxRows - 1 : members.length;
    members.slice(0, fits).forEach((n, i) => {
      order.set(n.id, i);
      shown.push(n);
    });
    const left = members.length - fits + lost;
    hidden += left;
    rows.set(c, fits + (left > 0 ? 1 : 0));
    if (left > 0) more.push({ column: c, count: left, x: 0, y: 0 });
  }
  const tallest = Math.max(1, ...rows.values());
  const height = 2 * PAD + tallest * BOX_HEIGHT + (tallest - 1) * ROW_GAP;
  const colY = (c: number, i: number) => {
    const n = rows.get(c) ?? 1;
    const block = n * BOX_HEIGHT + (n - 1) * ROW_GAP;
    return (height - block) / 2 + i * (BOX_HEIGHT + ROW_GAP);
  };
  const placed = new Map<string, PlacedNode>();
  for (const n of shown) {
    const c = n.side * n.hops;
    placed.set(n.id, { ...n, x: Math.round(colX(c)), y: Math.round(colY(c, order.get(n.id)!)) });
  }
  for (const m of more) {
    m.x = Math.round(colX(m.column));
    m.y = Math.round(colY(m.column, rows.get(m.column)! - 1));
  }
  const edges: PlacedEdge[] = [];
  for (const e of topo.edges) {
    const a = placed.get(e.from);
    const b = placed.get(e.to);
    if (!a || !b) continue;
    // From the parent's side facing the child to the child's side facing the parent.
    const right = b.x > a.x;
    edges.push({
      ...e,
      x1: right ? a.x + boxWidth : a.x,
      y1: a.y + BOX_HEIGHT / 2,
      x2: right ? b.x : b.x + boxWidth,
      y2: b.y + BOX_HEIGHT / 2,
    });
  }
  return { width, height, boxWidth, boxHeight: BOX_HEIGHT, nodes: [...placed.values()], edges, more, hidden };
}
