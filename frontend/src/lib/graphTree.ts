// Rows of GraphTree (pages/detail/GraphTree.vue), the indented tree of the relationship map and
// of the impact analysis: flattened depth-first, each with its level and how its edge reads.
import type { RelationshipGraph } from "../api/queries";
import { t } from "../i18n";
import type { ImpactTreeRow } from "./impact";

export interface TreeRow {
  key: string;
  /** 1 for the root's children. */
  level: number;
  /** How the edge from the parent row reads ("runs on", "hosts"). */
  edgeLabel: string;
  node: { id: string; label: string; classId: string; className: string; active: boolean; deletedAt?: string | null };
  hasChildren: boolean;
  /** Shown already higher up: not expanded again. */
  repeat?: boolean;
  /** Criticality badge (impact analysis); undefined: no criticality column. */
  criticality?: { label: string; rank: number } | null;
  /** A short note after the row ("also reached via 2 other relationships"). */
  note?: string;
}

type Edge = RelationshipGraph["edges"][number];

/**
 * The relationship graph as tree rows (App → runs on → VM → runs on → Server → is located in →
 * Rack). Each row is one hop further out than its parent (the API's hop count): relationships back
 * towards the root or between CIs of the same hop are not rows. A CI reached from two parents is
 * shown once and marked "(shown above)" under the second instead of expanding again.
 */
export function graphRows(graph: RelationshipGraph, rootId: string, direction: "both" | "outgoing" | "incoming"): TreeRow[] {
  const nodes = new Map(graph.nodes.map((n) => [n.id, n]));
  const hops = (id: string) => nodes.get(id)?.depth ?? (id === rootId ? 0 : -1);
  const adjacency = new Map<string, { edge: Edge; otherId: string; label: string }[]>();
  const add = (from: string, item: { edge: Edge; otherId: string; label: string }) => adjacency.set(from, [...(adjacency.get(from) ?? []), item]);
  for (const e of graph.edges) {
    if (direction !== "incoming") add(e.sourceCiId, { edge: e, otherId: e.targetCiId, label: e.type.forwardLabel });
    if (direction !== "outgoing") add(e.targetCiId, { edge: e, otherId: e.sourceCiId, label: e.type.isDirectional ? e.type.reverseLabel : e.type.forwardLabel });
  }
  const out: TreeRow[] = [];
  const seen = new Set<string>([rootId]);
  const walk = (id: string, level: number) => {
    for (const { edge, otherId, label } of adjacency.get(id) ?? []) {
      const node = nodes.get(otherId);
      if (!node || node.depth !== hops(id) + 1) continue;
      const repeat = seen.has(otherId);
      seen.add(otherId);
      const row: TreeRow = {
        key: `${edge.id}-${otherId}-${out.length}`,
        level,
        edgeLabel: label,
        node: { id: node.id, label: node.label, classId: node.classId, className: node.class.name, active: node.active, deletedAt: node.deletedAt },
        hasChildren: false,
        repeat,
      };
      out.push(row);
      if (!repeat) {
        const before = out.length;
        walk(otherId, level + 1);
        row.hasChildren = out.length > before;
      }
    }
  };
  walk(rootId, 1);
  return out;
}

/** An impact analysis's tree rows (lib/impact `impactTree`) as GraphTree rows, with their criticality. */
export function impactRows(rows: readonly ImpactTreeRow[]): TreeRow[] {
  return rows.map((r) => ({
    key: r.key,
    level: r.level,
    edgeLabel: r.label,
    node: { id: r.item.id, label: r.item.name, classId: r.item.classId, className: r.item.className, active: r.item.active },
    hasChildren: r.hasChildren,
    criticality: r.item.criticality,
    note: r.item.reachedByCount > 1 ? t("record.tree.alsoReachedVia", { n: r.item.reachedByCount - 1 }) : undefined,
  }));
}

/** The rows shown while some are collapsed: everything below a collapsed row is left out. */
export function visibleRows<T extends { key: string; level: number }>(rows: readonly T[], collapsed: ReadonlySet<string>): T[] {
  const out: T[] = [];
  let hideBelow: number | null = null;
  for (const r of rows) {
    if (hideBelow !== null && r.level > hideBelow) continue;
    hideBelow = collapsed.has(r.key) ? r.level : null;
    out.push(r);
  }
  return out;
}

/** The index of a row's parent in `rows` (the closest earlier row one level up), or -1 at the top. */
export function parentIndex(rows: readonly { level: number }[], i: number): number {
  for (let j = i - 1; j >= 0; j--) if (rows[j].level < rows[i].level) return j;
  return -1;
}
