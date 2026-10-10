// Impact analysis (the Impact tab of a CI, GET /configuration-items/{id}/impact): its URL state,
// and the paths, groups and tree built from the response. The API returns a bounded flat list;
// each item names the CI one hop closer to the root (`via.parentId`), so paths and the tree are
// built here from `items` plus the root, never from ids outside the response.
import type { LocationQuery, LocationQueryRaw } from "vue-router";
import type { Schemas } from "../api/client";
import { t } from "../i18n";

export type ImpactAnalysis = Schemas["ImpactAnalysis"];
export type ImpactItem = Schemas["ImpactItem"];
export type ImpactVia = Schemas["ImpactVia"];
export type ImpactCriticality = Schemas["ImpactCriticality"];
export type ImpactDirection = "downstream" | "upstream" | "both";
export type ImpactView = "list" | "tree";
export type ImpactGroup = "none" | "class" | "criticality" | "hops";
export type ImpactSortField = "name" | "class" | "criticality" | "hops" | "direction" | "via" | "status" | "active";

// Labels are getters, so they follow the active locale.
export const DIRECTIONS: { value: ImpactDirection; readonly label: string; readonly hint: string }[] = (
  ["downstream", "upstream", "both"] as const
).map((value) => ({
  value,
  get label() {
    return t(`impact.dir.${value}`);
  },
  get hint() {
    return t(`impact.dir.${value}.hint`);
  },
}));
export const GROUPS: { value: ImpactGroup; readonly label: string }[] = (["none", "class", "criticality", "hops"] as const).map((value) => ({
  value,
  get label() {
    return t(`impact.group.${value}`);
  },
}));
export const DEFAULT_DEPTH = 3;
const SORT_FIELDS: ImpactSortField[] = ["name", "class", "criticality", "hops", "direction", "via", "status", "active"];
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
/** The API's limit on relationshipTypeId. */
export const MAX_TYPES = 50;
/** `types` in the URL when the operator unchecked every relationship type (absent means all of them). */
const NO_TYPES = "none";

/** The Impact tab's state, as the URL holds it (§1.2 of the impact analysis spec). */
export interface ImpactState {
  direction: ImpactDirection;
  depth: number;
  /** Relationship types to follow; null: every type that propagates impact (the API's default); empty: none chosen. */
  types: string[] | null;
  includeInactive: boolean;
  view: ImpactView;
  group: ImpactGroup;
  /** List sort: a field, "-" first for descending. */
  sort: string;
}

export const DEFAULT_STATE: ImpactState = {
  direction: "downstream",
  depth: DEFAULT_DEPTH,
  types: null,
  includeInactive: true,
  view: "list",
  group: "class",
  sort: "hops",
};

const one = (q: LocationQuery | LocationQueryRaw, k: string): string => {
  const v = q[k];
  return typeof v === "string" ? v : "";
};

/**
 * The state the URL asks for, and the state keys of the parameters it names that cannot be used (they fall back to
 * their default, and the tab says so). `maxDepth` is the server's limit; unknown until the settings load.
 */
export function parseImpactQuery(
  query: LocationQuery | LocationQueryRaw,
  maxDepth?: number,
  defaults: ImpactState = DEFAULT_STATE,
): { state: ImpactState; invalid: string[] } {
  const invalid: string[] = [];
  const s: ImpactState = { ...defaults };
  const direction = one(query, "direction");
  if (direction) {
    if (direction === "downstream" || direction === "upstream" || direction === "both") s.direction = direction;
    else invalid.push("direction");
  }
  const depth = one(query, "depth");
  if (depth) {
    const n = /^\d+$/.test(depth) ? Number(depth) : NaN;
    if (Number.isInteger(n) && n >= 1 && (maxDepth === undefined || n <= maxDepth)) s.depth = n;
    else invalid.push("depth");
  }
  if (maxDepth !== undefined && s.depth > maxDepth) s.depth = maxDepth;
  const types = one(query, "types");
  if (types === NO_TYPES) s.types = [];
  else if (types) {
    const ids = [...new Set(types.split(",").filter(Boolean))];
    if (ids.length > 0 && ids.length <= MAX_TYPES && ids.every((id) => UUID.test(id))) s.types = ids;
    else invalid.push("types");
  }
  const inactive = one(query, "inactive");
  if (inactive) {
    if (inactive === "1" || inactive === "0") s.includeInactive = inactive === "1";
    else invalid.push("includeInactive");
  }
  const view = one(query, "view");
  if (view) {
    if (view === "list" || view === "tree") s.view = view;
    else invalid.push("view");
  }
  const group = one(query, "group");
  if (group) {
    if (GROUPS.some((g) => g.value === group)) s.group = group as ImpactGroup;
    else invalid.push("group");
  }
  const sort = one(query, "sort");
  if (sort) {
    if (SORT_FIELDS.includes(sort.replace(/^-/, "") as ImpactSortField)) s.sort = sort;
    else invalid.push("sort");
  }
  return { state: s, invalid };
}

/**
 * The URL query for a state: only what differs from the defaults, so a plain link stays plain. A business
 * service's tab defaults to Upstream (`defaults`), so its plain link means Upstream; a link naming the
 * direction still wins.
 */
export function impactQuery(s: ImpactState, defaults: ImpactState = DEFAULT_STATE): Record<string, string> {
  const q: Record<string, string> = {};
  if (s.direction !== defaults.direction) q.direction = s.direction;
  if (s.depth !== DEFAULT_STATE.depth) q.depth = String(s.depth);
  if (s.types) q.types = s.types.length > 0 ? s.types.join(",") : NO_TYPES;
  if (s.includeInactive !== DEFAULT_STATE.includeInactive) q.inactive = s.includeInactive ? "1" : "0";
  if (s.view !== DEFAULT_STATE.view) q.view = s.view;
  if (s.group !== DEFAULT_STATE.group) q.group = s.group;
  if (s.sort !== DEFAULT_STATE.sort) q.sort = s.sort;
  return q;
}

/** The query URL keys a request parameter comes from, to reset the right control when the API refuses one. */
export const PARAM_KEYS: Record<string, keyof ImpactState> = {
  direction: "direction",
  depth: "depth",
  relationshipTypeId: "types",
  includeInactive: "includeInactive",
};

const STATE_KEYS: (keyof ImpactState)[] = ["direction", "depth", "types", "includeInactive", "view", "group", "sort"];
/** How the reset notice names a state key, in the active locale. */
export const STATE_LABELS = Object.defineProperties(
  {} as Record<keyof ImpactState, string>,
  Object.fromEntries(STATE_KEYS.map((k) => [k, { enumerable: true, get: () => t(`impact.param.${k}`) }])),
);

/** The request parameters (GET …/impact and …/impact/export) for a state. */
export function impactParams(s: ImpactState) {
  return {
    direction: s.direction,
    depth: s.depth,
    ...(s.types && s.types.length > 0 ? { relationshipTypeId: s.types.join(",") } : {}),
    includeInactive: s.includeInactive ? ("true" as const) : ("false" as const),
  };
}

// ---------- Criticality ----------

/** Badge tone for a criticality rank: 1 critical, 2 high, 3 medium, lower ones and custom values muted. */
export function criticalityTone(rank: number | null | undefined): "danger" | "warn" | "neutral" | "off" {
  if (rank == null) return "off";
  return rank <= 1 ? "danger" : rank === 2 ? "warn" : rank === 3 ? "neutral" : "off";
}

const notSet = () => t("common.notSet");

// ---------- Paths ----------

/** How an edge reads from `fromId` to the other end: "runs on" from the source, "hosts" from the target. */
export function edgeLabel(via: ImpactVia, fromId: string): string {
  return via.edgeSourceId === fromId ? via.relationshipType.forwardLabel : via.relationshipType.reverseLabel;
}

/** Which walk an item's `via` belongs to (in both mode, the shorter one). */
export function chosenWay(item: ImpactItem): "downstream" | "upstream" {
  if (item.directions.length === 1) return item.directions[0];
  if (item.upstreamVia) return "downstream";
  if (item.downstreamVia) return "upstream";
  return item.directions.includes("downstream") ? "downstream" : "upstream";
}

/** The last hop of the item's path in one walk, or null when that walk did not reach it. */
export function viaFor(item: ImpactItem, way: "downstream" | "upstream"): ImpactVia | null {
  if (!item.directions.includes(way)) return null;
  if (chosenWay(item) === way) return item.via;
  return (way === "downstream" ? item.downstreamVia : item.upstreamVia) ?? item.via;
}

export interface PathStep {
  id: string;
  name: string;
  /** How the edge into this step reads from the step before it; absent for the root. */
  label?: string;
}

/**
 * The shortest visible path root → … → item (its `via` chain), for "Show path". Stops rather than
 * loops if the chain is broken or cyclic, which the API rules out.
 */
export function pathTo(
  item: ImpactItem,
  root: { id: string; name: string },
  byId: ReadonlyMap<string, ImpactItem>,
  way: "downstream" | "upstream" = chosenWay(item),
): PathStep[] {
  const steps: PathStep[] = [];
  const seen = new Set<string>();
  let current: ImpactItem | undefined = item;
  while (current && !seen.has(current.id)) {
    seen.add(current.id);
    const via = viaFor(current, way) ?? current.via;
    steps.unshift({ id: current.id, name: current.name, label: edgeLabel(via, via.parentId) });
    if (via.parentId === root.id) {
      steps.unshift({ id: root.id, name: root.name });
      return steps;
    }
    current = byId.get(via.parentId);
  }
  return steps;
}

// ---------- List ----------

export interface ImpactGroupRows {
  key: string;
  label: string;
  items: ImpactItem[];
}

const DIRECTION_ORDER = (i: ImpactItem) => (i.directions.length > 1 ? 2 : i.directions[0] === "downstream" ? 0 : 1);

export function compareItems(a: ImpactItem, b: ImpactItem, sort: string, viaName: (i: ImpactItem) => string): number {
  const desc = sort.startsWith("-");
  const field = sort.replace(/^-/, "") as ImpactSortField;
  const text = (x: string, y: string) => x.localeCompare(y, undefined, { sensitivity: "base" });
  // Not set sorts after every rank, whichever way.
  const rank = (i: ImpactItem) => i.criticality?.rank ?? Number.MAX_SAFE_INTEGER;
  let r = 0;
  switch (field) {
    case "name":
      r = text(a.name, b.name);
      break;
    case "class":
      r = text(a.className, b.className);
      break;
    case "criticality":
      r = rank(a) - rank(b);
      break;
    case "hops":
      r = a.hops - b.hops;
      break;
    case "direction":
      r = DIRECTION_ORDER(a) - DIRECTION_ORDER(b);
      break;
    case "via":
      r = text(viaName(a), viaName(b));
      break;
    case "status":
      r = text(a.status?.label ?? "￿", b.status?.label ?? "￿");
      break;
    case "active":
      r = Number(b.active) - Number(a.active);
      break;
  }
  if (desc) r = -r;
  // Ties: hops, then name (the API's order).
  return r || a.hops - b.hops || text(a.name, b.name);
}

/** The rows of the list view, grouped (§1.4): groups in a meaningful order, rows sorted within. */
export function groupItems(items: readonly ImpactItem[], group: ImpactGroup, sort: string, viaName: (i: ImpactItem) => string): ImpactGroupRows[] {
  const sorted = [...items].sort((a, b) => compareItems(a, b, sort, viaName));
  if (group === "none") return [{ key: "all", label: "All affected CIs", items: sorted }];
  const groups = new Map<string, ImpactGroupRows & { order: number | string }>();
  for (const i of sorted) {
    let key: string;
    let label: string;
    let order: number | string;
    if (group === "class") {
      key = `class:${i.classId}`;
      label = i.className;
      order = i.className.toLocaleLowerCase();
    } else if (group === "criticality") {
      key = `crit:${i.criticality?.key ?? ""}`;
      label = i.criticality?.label ?? notSet();
      order = i.criticality?.rank ?? Number.MAX_SAFE_INTEGER;
    } else {
      key = `hops:${i.hops}`;
      label = t("impact.depthOption", { n: i.hops });
      order = i.hops;
    }
    const g = groups.get(key) ?? { key, label, items: [], order };
    g.items.push(i);
    groups.set(key, g);
  }
  return [...groups.values()]
    .sort((a, b) => (typeof a.order === "number" && typeof b.order === "number" ? a.order - b.order : String(a.order).localeCompare(String(b.order))))
    .map(({ key, label, items }) => ({ key, label, items }));
}

// ---------- Tree ----------

export interface ImpactTreeRow {
  key: string;
  /** 1 for the root's children. */
  level: number;
  item: ImpactItem;
  /** How the edge reads from the parent row, in the direction impact travels. */
  label: string;
  hasChildren: boolean;
}

export interface ImpactSubtree {
  way: "downstream" | "upstream";
  title: string;
  rows: ImpactTreeRow[];
}

/**
 * The tree view (§1.5): each CI once, under the CI its shortest path comes from. In both mode two
 * subtrees, "Affected by this CI" and "This CI depends on"; a CI reached both ways shows in each.
 * Siblings keep the API's order (hops, then name).
 */
export function impactTree(analysis: Pick<ImpactAnalysis, "root" | "items" | "parameters">): ImpactSubtree[] {
  const ways: ("downstream" | "upstream")[] =
    analysis.parameters.direction === "both" ? ["downstream", "upstream"] : [analysis.parameters.direction];
  return ways.map((way) => {
    const members = analysis.items.filter((i) => i.directions.includes(way));
    const ids = new Set(members.map((i) => i.id));
    const children = new Map<string, { item: ImpactItem; via: ImpactVia }[]>();
    for (const item of members) {
      const via = viaFor(item, way)!;
      // A parent outside this walk cannot happen; should it, the CI still shows, under the root.
      const parent = via.parentId === analysis.root.id || ids.has(via.parentId) ? via.parentId : analysis.root.id;
      children.set(parent, [...(children.get(parent) ?? []), { item, via }]);
    }
    const rows: ImpactTreeRow[] = [];
    const seen = new Set<string>([analysis.root.id]);
    const walk = (id: string, level: number) => {
      for (const { item, via } of children.get(id) ?? []) {
        if (seen.has(item.id)) continue;
        seen.add(item.id);
        rows.push({ key: `${way}:${item.id}`, level, item, label: edgeLabel(via, via.parentId), hasChildren: (children.get(item.id)?.length ?? 0) > 0 });
        walk(item.id, level + 1);
      }
    };
    walk(analysis.root.id, 1);
    return { way, get title() {
        return t(`impact.dir.${way}.hint`);
      },
      rows };
  });
}

// ---------- Header ----------

/** Why a result is incomplete, in plain words (§1.3). `timeoutMs` is the server's deadline, when known. */
export function truncationMessage(a: ImpactAnalysis, timeoutMs?: number): string {
  switch (a.truncatedReason) {
    case "max_nodes":
      return t("impact.truncated.maxNodes", { n: a.parameters.maxNodes });
    case "max_edges":
      return t("impact.truncated.maxEdges");
    case "timeout":
      return timeoutMs ? t("impact.truncated.timeout", { n: timeoutMs / 1000 }) : t("impact.truncated.timeLimit");
    default:
      return t("impact.truncated.other");
  }
}



/** "42 CIs affected downstream", "7 CIs upstream", "12 CIs in both directions". */
export function summaryPhrase(total: number, direction: ImpactDirection): string {
  return t(`impact.summary.count.${direction}`, { n: total });
}

/** "3 critical · 11 high": the counts of the ranked criticality values, most critical first. */
export function criticalityCounts(analysis: ImpactAnalysis): string {
  const labels = new Map<string, string>();
  for (const i of analysis.items) if (i.criticality) labels.set(i.criticality.key, i.criticality.label);
  return analysis.summary.byCriticality
    .filter((c) => c.key !== null && c.count > 0)
    .map((c) => `${c.count.toLocaleString()} ${(labels.get(c.key!) ?? c.key!).toLocaleLowerCase()}`)
    .join(" · ");
}
