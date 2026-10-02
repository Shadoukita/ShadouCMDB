import type { LocationQuery, LocationQueryRaw } from "vue-router";
import type { ApiError } from "../api/client";
import type { SavedView, SavedViewContext, SavedViewDefinition } from "../api/savedViews";
import { columnsParam, param, parseColumns, STATE_KEYS } from "./inventoryQuery";

/**
 * Saved views (the View menu of /cis and /search) as the screens apply them.
 * A view is a stored query: applying it writes its resolved parameters into the
 * URL (`view=<id>` plus every parameter), so the URL stays the source of truth
 * and the list and search endpoints apply the caller's rights to every request.
 * Saving goes the other way: the URL's ids become the definition's keys.
 */

type Query = LocationQuery | LocationQueryRaw;

/** The parameters a view stands for, ready for the URL, with `view=<id>`. Null for an unavailable view: it is never applied. */
export function viewQuery(view: SavedView): LocationQueryRaw | null {
  const q = view.resolved.query;
  if (view.resolved.state === "unavailable" || !q) return null;
  const out: LocationQueryRaw = { view: view.id };
  const set = (k: string, v: string | number | null | undefined) => {
    if (v !== null && v !== undefined && v !== "") out[k] = String(v);
  };
  set("q", q.q);
  set("classId", q.classId);
  set("lookupValueId", q.lookupValueId);
  set("criticalityValueId", q.criticalityValueId);
  if (q.active && q.active !== "true") set("active", q.active);
  if (q.deleted && q.deleted !== "exclude") set("deleted", q.deleted);
  set("ipWithin", q.ipWithin);
  set("limit", q.limit);
  if (view.context === "inventory") {
    // Written even when it is the built-in sort: a view without one sorts by label, whatever the class's list view says.
    set("sort", q.sort || "label");
    if (view.resolved.columns.length > 0) set("columns", columnsParam(view.resolved.columns));
  }
  return out;
}

/** What the comparison treats as "not set": the list's effective sort and page size. */
export interface StateDefaults {
  sort?: string;
  limit?: number;
}

const LISTS = new Set(["classId", "lookupValueId", "criticalityValueId"]);
const normalized = (k: string, v: string) => (LISTS.has(k) ? v.split(",").filter(Boolean).sort().join(",") : v);

/**
 * Whether the URL's state differs from the view's (the Modified marker, §1.3 step 1).
 * Id lists compare as sets; a missing sort or page size compares as the effective one.
 */
export function isModified(url: Query, view: SavedView, defaults: StateDefaults = {}): boolean {
  const target = viewQuery(view);
  if (!target) return false;
  const fill = (q: Query, k: string) => {
    const v = param(q, k);
    if (v) return k === "columns" ? columnsParam(parseColumns(v)) : normalized(k, v);
    if (k === "sort") return defaults.sort ?? "";
    if (k === "limit") return defaults.limit !== undefined ? String(defaults.limit) : "";
    return "";
  };
  return STATE_KEYS[view.context].some((k) => fill(url, k) !== fill(target, k));
}

/** The URL's state as the definition stores it: classes and lookup values by key, never by id (D3). */
export interface KeyCatalog {
  classes: readonly { id: string; key: string }[];
  lists: readonly { id: string; key: string }[];
  values: readonly { id: string; listId: string; key: string }[];
}

export interface DefinitionResult {
  definition: SavedViewDefinition;
  /** Ids the catalog does not know (a class or value removed since the link was made); they are left out. */
  unknown: string[];
}

const ids = (q: Query, k: string) => param(q, k).split(",").filter(Boolean);

/** The definition of the list the URL shows, for "Save view" and "Save as new view". */
export function definitionFromUrl(url: Query, context: SavedViewContext, catalog: KeyCatalog): DefinitionResult {
  const unknown: string[] = [];
  const classKeys: string[] = [];
  for (const id of ids(url, "classId")) {
    const c = catalog.classes.find((x) => x.id === id);
    if (c) classKeys.push(c.key);
    else unknown.push(id);
  }
  const lookups: Record<string, string[]> = {};
  for (const id of [...ids(url, "lookupValueId"), ...ids(url, "criticalityValueId")]) {
    const v = catalog.values.find((x) => x.id === id);
    const list = v && catalog.lists.find((l) => l.id === v.listId);
    if (!v || !list) {
      unknown.push(id);
      continue;
    }
    const keys = (lookups[list.key] ??= []);
    if (!keys.includes(v.key)) keys.push(v.key);
  }
  const filters: NonNullable<SavedViewDefinition["filters"]> = {};
  const q = param(url, "q").trim();
  if (q) filters.q = q;
  if (Object.keys(lookups).length > 0) filters.lookups = lookups;
  const active = param(url, "active");
  if (active === "all" || active === "false") filters.active = active;
  const deleted = param(url, "deleted");
  if (deleted === "include" || deleted === "only") filters.deleted = deleted;
  const ip = param(url, "ipWithin");
  if (ip) filters.ipWithin = ip;

  const definition: SavedViewDefinition = { classKeys, includeSubclasses: true, filters };
  if (context === "inventory") {
    const sort = param(url, "sort");
    if (sort) {
      const desc = sort.startsWith("-");
      definition.sort = { field: sort.replace(/^-/, "") as NonNullable<SavedViewDefinition["sort"]>["field"], direction: desc ? "desc" : "asc" };
    }
    // Only columns the operator chose: none follows the class's list view, as the URL does.
    const columns = parseColumns(param(url, "columns"));
    if (columns.length > 0) definition.columns = columns;
  }
  const limit = Number.parseInt(param(url, "limit"), 10);
  if (Number.isFinite(limit)) definition.pageSize = Math.min(200, Math.max(10, limit));
  return { definition, unknown };
}

/** The default slot (`home`) of the list the URL shows: its one class's key, else the unscoped inventory (null). */
export function homeOf(url: Query, classes: readonly { id: string; key: string }[]): string | null {
  const id = param(url, "classId");
  if (!id || id.includes(",")) return null;
  return classes.find((c) => c.id === id)?.key ?? null;
}

/** The caller's default for a home, if it can be applied. */
export function defaultView(views: readonly SavedView[], home: string | null): SavedView | undefined {
  return views.find((v) => v.context === "inventory" && v.isDefault && (v.home ?? null) === home && v.resolved.state !== "unavailable");
}

const byName = (a: SavedView, b: SavedView) => a.name.localeCompare(b.name, undefined, { sensitivity: "base", numeric: true });

/** The menu's two groups, each alphabetical. */
export function groupViews(views: readonly SavedView[]): { mine: SavedView[]; shared: SavedView[] } {
  return {
    mine: views.filter((v) => v.visibility === "personal").sort(byName),
    shared: views.filter((v) => v.visibility === "shared").sort(byName),
  };
}

/** Views whose name contains the filter text (the menu's filter, shown with more than 10 views). */
export function filterViews(views: readonly SavedView[], text: string): SavedView[] {
  const t = text.trim().toLocaleLowerCase();
  return t ? views.filter((v) => v.name.toLocaleLowerCase().includes(t)) : [...views];
}

/** More views than this get a filter input at the top of the menu (§1.2). */
export const MENU_FILTER_THRESHOLD = 10;

/**
 * The menu item a typed character moves to: the next one after `from` whose label
 * starts with it (case-insensitive), wrapping around; -1 when none does.
 */
export function typeaheadIndex(labels: readonly string[], from: number, char: string): number {
  const c = char.toLocaleLowerCase();
  for (let i = 1; i <= labels.length; i++) {
    const at = (from + i) % labels.length;
    if (labels[at].trim().toLocaleLowerCase().startsWith(c)) return at;
  }
  return -1;
}

/** Which actions the menu offers for the current view (§1.2). */
export interface ViewActions {
  save: boolean;
  saveAs: boolean;
  rename: boolean;
  setDefault: boolean;
  clearDefault: boolean;
  copyToMine: boolean;
  shareCopy: boolean;
  delete: boolean;
}

export function viewActions(view: SavedView | undefined, opts: { canShare: boolean; context: SavedViewContext; home: string | null }): ViewActions {
  const inventory = opts.context === "inventory";
  // A view can be the default only of its own home: a Server view is not the default of the Network list.
  const homeMatches = !!view && (view.home ?? null) === opts.home;
  return {
    save: !!view && view.canEdit && view.resolved.state !== "unavailable",
    saveAs: true,
    rename: !!view && view.canEdit,
    setDefault: inventory && !!view && homeMatches && !view.isDefault && view.resolved.state !== "unavailable",
    clearDefault: inventory && !!view && view.isDefault,
    copyToMine: !!view && view.visibility === "shared",
    shareCopy: !!view && view.visibility === "personal" && opts.canShare,
    delete: !!view && view.canEdit,
  };
}

/** The list a view's default slot opens: "Server" or "Inventory". */
export function homeLabel(home: string | null | undefined, classes: readonly { key: string; name: string }[]): string {
  if (!home) return "Inventory";
  return classes.find((c) => c.key === home)?.name ?? home;
}

/** What the delete confirmation says beyond "Delete view X?" (§1.5): exactly what goes, nothing about CIs changes. */
export function deleteConsequence(view: SavedView, homeName: string): string {
  if (view.visibility === "shared") {
    const n = view.defaultCount ?? 0;
    const users = n === 1 ? "1 user" : `${n.toLocaleString()} users`;
    return n > 0
      ? `It is shared with everyone and is the default for ${users}. They will return to the standard list. CIs are not affected.`
      : "It is shared with everyone. CIs are not affected. This cannot be undone.";
  }
  if (view.isDefault) {
    return `This cannot be undone. It is your default for ${homeName}. The ${homeName} list will open with the standard columns and filters.`;
  }
  return "This cannot be undone.";
}

/** The kinds of failure the save, rename and copy dialogs present differently (§1.5). */
export type SaveFailure =
  | { kind: "conflict"; message: string }
  | { kind: "limit"; message: string }
  | { kind: "fields"; name?: string; description?: string; definition: string[]; other?: string };

/**
 * An API error as the dialogs show it: a version conflict and the view limit get
 * their own dialog or notice; per-field errors go next to Name and Description,
 * errors in the definition are listed in the dialog.
 */
export function saveFailure(err: Pick<ApiError, "code" | "message" | "details">): SaveFailure {
  if (err.code === "VERSION_CONFLICT") return { kind: "conflict", message: err.message };
  if (err.details.some((d) => d.code === "limit_reached")) return { kind: "limit", message: err.message };
  const out: Extract<SaveFailure, { kind: "fields" }> = { kind: "fields", definition: [] };
  for (const d of err.details) {
    if (d.field === "name") out.name = out.name ? `${out.name}; ${d.message}` : d.message;
    else if (d.field === "description") out.description = d.message;
    else if (d.field.startsWith("definition")) out.definition.push(definitionMessage(d.field, d.message));
  }
  if (!out.name && !out.description && out.definition.length === 0) out.other = err.message;
  return out;
}

/** A definition error in the words of the screen: "Sort: …", "Column 3: …". */
function definitionMessage(field: string, message: string): string {
  const col = /^definition\.columns\.(\d+)$/.exec(field);
  if (col) return `Column ${Number(col[1]) + 1}: ${message}`;
  if (field.startsWith("definition.sort")) return `Sort: ${message}`;
  if (field.startsWith("definition.filters.lookups")) return `Lookup filter: ${message}`;
  if (field.startsWith("definition.classKeys")) return `Class filter: ${message}`;
  return message;
}

/**
 * The degraded-view banner's lines (§1.5): what resolution dropped, in plain words.
 * Readers of a shared view they cannot edit see only how many items were dropped.
 */
export function degradedLines(view: SavedView): string[] {
  const issues = view.resolved.issues.filter((i) => i.severity === "warning");
  if (issues.length === 0) return [];
  if (!view.canEdit && view.visibility === "shared") {
    return [issues.length === 1 ? "1 part of this shared view is not available and was left out." : `${issues.length} parts of this shared view are not available and were left out.`];
  }
  return issues.map((i) => i.message);
}

// ---------- Which state a list opens with (§1.3) ----------

/** A banner about the `view=` link the page was opened with. */
export type ViewNotice = { kind: "notAvailable" } | { kind: "unavailable"; name: string };

export type OpenDecision =
  /** Wait: the view or the defaults are still loading (the list is not queried yet, so it is queried once). */
  | { kind: "wait" }
  /** Show the URL as it is (with the class's list view, then the built-in defaults, for what it leaves out). */
  | { kind: "none" }
  /** Replace the URL with this query (a view's state, or nothing when the linked view is not available). */
  | { kind: "replace"; query: LocationQueryRaw; notice?: ViewNotice };

export interface OpenInput {
  query: Query;
  context: SavedViewContext;
  /** The CI classes; undefined while they load. */
  classes: readonly { id: string; key: string }[] | undefined;
  /** The view list; undefined while it loads. */
  views: readonly SavedView[] | undefined;
  /** The view list failed to load: no default can apply, the list opens as without one. */
  viewsFailed: boolean;
  /** The `view=` view fetched by id: undefined while it loads, `failed` once it answered with an error (404 included). */
  linked: { view?: SavedView; failed?: boolean } | undefined;
}

/** The parameters the URL sets, other than the page. */
const urlKeys = (q: Query) => Object.keys(q).filter((k) => k !== "offset" && param(q, k) !== "");

/**
 * What the list shows when it opens (§1.3): 1. a URL with state shows exactly that;
 * 2. a `view=<id>`-only link applies the view; 3. on the inventory, a URL with no
 * state or just one class applies the user's default for that list; 4. and 5.
 * (the class's list view, the built-in defaults) are the query state's own baseline.
 */
export function decideOpen(input: OpenInput): OpenDecision {
  const { query, context } = input;
  const keys = urlKeys(query);
  const id = param(query, "view");
  if (id) {
    if (keys.some((k) => k !== "view")) return { kind: "none" };
    const view = input.views?.find((v) => v.id === id) ?? input.linked?.view;
    if (!view) return input.linked?.failed ? { kind: "replace", query: {}, notice: { kind: "notAvailable" } } : { kind: "wait" };
    if (view.context !== context) return { kind: "replace", query: {}, notice: { kind: "notAvailable" } };
    const q = viewQuery(view);
    return q ? { kind: "replace", query: q } : { kind: "replace", query: {}, notice: { kind: "unavailable", name: view.name } };
  }
  // Search views are opened explicitly, never as a default (D7).
  if (context !== "inventory" || !keys.every((k) => k === "classId")) return { kind: "none" };
  const classId = param(query, "classId");
  if (classId.includes(",")) return { kind: "none" };
  if (classId && !input.classes) return { kind: "wait" };
  const home = homeOf(query, input.classes ?? []);
  if (classId && home === null) return { kind: "none" };
  if (!input.views) return input.viewsFailed ? { kind: "none" } : { kind: "wait" };
  const view = defaultView(input.views, home);
  const q = view && viewQuery(view);
  return q ? { kind: "replace", query: q } : { kind: "none" };
}
