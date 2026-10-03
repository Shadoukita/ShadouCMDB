import type { LocationQuery, LocationQueryRaw } from "vue-router";
import type { SavedView, SavedViewDefinition } from "../api/savedViews";
import { DEFAULT_LIMIT, DEFAULT_SORT, FILTER_KEYS, param, parseColumns, type QueryContext } from "./inventoryQuery";

/**
 * Saved views and the URL (saved-views spec §1.3, D6). A view is stored by key
 * (classes, lookup lists and values) and comes back from the API resolved into
 * URL parameters (`resolved.query`, `resolved.columns`). Applying a view writes
 * every parameter into the URL next to `view=<id>`, so the URL alone says what
 * the list shows; saving turns the URL's ids back into keys.
 */

/** Parameters that hold comma-separated ids: their order does not matter. */
const ID_LISTS = new Set(["classId", "lookupValueId", "criticalityValueId"]);

/** The URL query that shows a view, with `view=<id>`; null when the view cannot be applied (`unavailable`). */
export function viewUrlQuery(view: SavedView, context: QueryContext): LocationQueryRaw | null {
  const r = view.resolved.query;
  if (view.resolved.state === "unavailable" || !r) return null;
  const out: LocationQueryRaw = { view: view.id };
  const set = (k: string, v: string | null | undefined) => {
    if (v) out[k] = v;
  };
  set("q", r.q);
  set("classId", r.classId);
  if (r.classId && r.includeSubclasses === "false") out.includeSubclasses = "false";
  set("lookupValueId", r.lookupValueId);
  set("criticalityValueId", r.criticalityValueId);
  if (r.active && r.active !== "true") out.active = r.active;
  if (r.deleted && r.deleted !== "exclude") out.deleted = r.deleted;
  set("ipWithin", r.ipWithin);
  if (context === "inventory") {
    // Explicit, so the class's list view (the baseline) does not change what the view shows.
    out.sort = r.sort || DEFAULT_SORT;
    if (view.resolved.columns.length > 0) out.columns = view.resolved.columns.join(",");
  }
  out.limit = String(r.limit ?? DEFAULT_LIMIT);
  return out;
}

/** What a list shows, in comparable form: the filters, plus the effective sort, page size and columns. */
export interface ComparableState {
  filters: Record<string, string>;
  sort?: string;
  limit: number;
  columns?: string;
}

function normalise(key: string, raw: string): string {
  if (ID_LISTS.has(key)) return raw.split(",").filter(Boolean).sort().join(",");
  if ((key === "active" && raw === "true") || (key === "deleted" && raw === "exclude") || (key === "includeSubclasses" && raw === "true")) return "";
  return raw;
}

const COMPARED = [...FILTER_KEYS, "includeSubclasses"] as const;

/** The URL's state; `sort` and `limit` are the effective ones (the URL may leave them to the baseline). */
export function urlState(query: LocationQuery | LocationQueryRaw, context: QueryContext, effective: { sort: string; limit: number }): ComparableState {
  const filters: Record<string, string> = {};
  for (const k of COMPARED) filters[k] = normalise(k, param(query, k));
  if (!filters.classId) filters.includeSubclasses = "";
  return context === "inventory"
    ? { filters, sort: effective.sort, limit: effective.limit, columns: parseColumns(param(query, "columns")).join(",") }
    : { filters, limit: effective.limit };
}

/** The state a view stands for; null when it cannot be applied. */
export function viewState(view: SavedView, context: QueryContext): ComparableState | null {
  const q = viewUrlQuery(view, context);
  if (!q) return null;
  return urlState(q, context, { sort: param(q, "sort"), limit: Number(param(q, "limit")) });
}

export function sameState(a: ComparableState, b: ComparableState): boolean {
  return (
    a.sort === b.sort &&
    a.limit === b.limit &&
    a.columns === b.columns &&
    COMPARED.every((k) => (a.filters[k] ?? "") === (b.filters[k] ?? ""))
  );
}

export interface DefinitionCatalogue {
  classes: readonly { id: string; key: string }[];
  lists: readonly { id: string; key: string }[];
  values: readonly { id: string; listId: string; key: string }[];
}

export type DefinitionResult = { ok: true; definition: SavedViewDefinition } | { ok: false; message: string };

/**
 * The definition to save for what the URL shows. `sort` and `limit` are the effective
 * ones, saved explicitly so the view shows the same list wherever it is opened; the
 * columns only when the operator chose them (else the class's list view columns follow).
 * An id the catalogue does not know is refused rather than dropped: dropping a filter
 * value would save a view that shows more than the list on screen.
 */
export function definitionFromUrl(
  query: LocationQuery | LocationQueryRaw,
  context: QueryContext,
  cat: DefinitionCatalogue,
  effective: { sort: string; limit: number },
): DefinitionResult {
  const ids = (k: string) => param(query, k).split(",").filter(Boolean);
  if (param(query, "ownLayout") || param(query, "layoutTemplate")) {
    return { ok: false, message: "A saved view cannot hold the layout filter. Remove that filter, then save again." };
  }
  const classKeys: string[] = [];
  for (const id of ids("classId")) {
    const c = cat.classes.find((x) => x.id === id);
    if (!c) return { ok: false, message: "The class filter names a class that no longer exists. Change the class filter, then save again." };
    classKeys.push(c.key);
  }
  const lookups: Record<string, string[]> = {};
  for (const id of [...ids("lookupValueId"), ...ids("criticalityValueId")]) {
    const v = cat.values.find((x) => x.id === id);
    const list = v && cat.lists.find((l) => l.id === v.listId);
    if (!v || !list) return { ok: false, message: "A lookup filter names a value that no longer exists. Remove that filter, then save again." };
    const keys = (lookups[list.key] ??= []);
    if (!keys.includes(v.key)) keys.push(v.key);
  }
  const filters: NonNullable<SavedViewDefinition["filters"]> = {};
  const q = param(query, "q").trim();
  if (q) filters.q = q;
  if (Object.keys(lookups).length > 0) filters.lookups = lookups;
  const active = param(query, "active");
  if (active === "all" || active === "false") filters.active = active;
  const deleted = param(query, "deleted");
  if (deleted === "include" || deleted === "only") filters.deleted = deleted;
  const ipWithin = param(query, "ipWithin");
  if (ipWithin) filters.ipWithin = ipWithin;

  const definition: SavedViewDefinition = {
    classKeys,
    includeSubclasses: param(query, "includeSubclasses") !== "false",
    filters,
    pageSize: effective.limit,
  };
  if (context === "inventory") {
    const desc = effective.sort.startsWith("-");
    definition.sort = { field: desc ? effective.sort.slice(1) : effective.sort, direction: desc ? "desc" : "asc" };
    definition.columns = parseColumns(param(query, "columns"));
  }
  return { ok: true, definition };
}

/** The list a view is the default of, for the screen: its class's name, or the unscoped inventory. */
export function homeName(view: Pick<SavedView, "home">, classes: readonly { key: string; name: string }[] | undefined): string {
  if (!view.home) return "Inventory";
  return classes?.find((c) => c.key === view.home)?.name ?? view.home;
}

/**
 * What resolution dropped, for the banner over a degraded view. Readers of a shared
 * view they cannot change see only how many items went (§1.5): the names could point
 * at classes they may not see, and they cannot fix the view anyway.
 */
export function droppedSummary(view: SavedView): { messages: string[]; count: number; notes: string[] } {
  const warnings = view.resolved.issues.filter((i) => i.severity === "warning");
  const notes = view.resolved.issues.filter((i) => i.severity === "info").map((i) => i.message);
  const detailed = view.visibility === "personal" || view.canEdit;
  return { messages: detailed ? warnings.map((i) => i.message) : [], count: warnings.length, notes };
}

/** Group the views for the menu: the caller's own, then the shared ones (the API already sorts each by name). */
export function groupViews(views: readonly SavedView[], filter = ""): { personal: SavedView[]; shared: SavedView[] } {
  const f = filter.trim().toLocaleLowerCase();
  const shown = f ? views.filter((v) => v.name.toLocaleLowerCase().includes(f)) : views;
  return { personal: shown.filter((v) => v.visibility === "personal"), shared: shown.filter((v) => v.visibility === "shared") };
}
