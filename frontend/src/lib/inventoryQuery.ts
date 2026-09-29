import type { LocationQuery, LocationQueryRaw } from "vue-router";
import type { UiListView } from "../api/uiSettings";
import { attributeKey, BUILTIN, DEFAULT_COLUMNS, listColumns, sortParam } from "./uiSettings";

/**
 * The query state of the inventory (/cis) and of the search page (/search), and
 * where each part comes from. The URL is the source of truth: whatever it names
 * wins, so a reload, a bookmark or a shared link shows the same list. What it
 * leaves out comes from the list's baseline, then from the built-in defaults:
 *
 *   1. the URL;
 *   2. (later) a saved view (`view=<id>`) or the user's default view;
 *   3. the class's list view in UI settings (Administration › Customization);
 *   4. the built-in defaults below.
 *
 * Saved views plug in as another `ListBaseline` ahead of the list view's
 * (`resolveBaseline`), so the screens and the URL format stay as they are.
 */

export type QueryContext = "inventory" | "search";

/** URL parameters that make up a list's state (not the page: `offset`). Any of them means "the URL says". */
export const STATE_KEYS = {
  inventory: ["q", "classId", "lookupValueId", "active", "deleted", "ipWithin", "sort", "limit", "columns"],
  search: ["q", "classId", "lookupValueId", "active", "deleted", "ipWithin", "limit"],
} as const satisfies Record<QueryContext, readonly string[]>;

/** Filters, which "Clear filters" removes (sort, page size and columns stay). */
export const FILTER_KEYS = ["q", "classId", "lookupValueId", "active", "deleted", "ipWithin"] as const;

export const DEFAULT_LIMIT = 50;
export const DEFAULT_SORT = "label";
/** The most columns a list shows (the saved-view definition's limit). */
export const MAX_COLUMNS = 50;
/** A field name as list views store it (the API's FIELD_PATTERN): a built-in field or `attributes.<key>`. */
const FIELD_PATTERN = /^(?:label|ident|class|validFrom|validUntil|active|createdAt|updatedAt|attributes\.[a-z][a-z0-9_]{0,62})$/;

export type Active = "all" | "false" | undefined;
export type Deleted = "include" | "only" | undefined;

/** What a list shows when the URL does not say: a saved view's or the class list view's choices. */
export interface ListBaseline {
  /** Where it comes from, for the screen and for tests. */
  source: "listView" | "builtIn";
  columns: string[];
  /** A sort parameter (`[-]field`), or undefined for the built-in one. */
  sort: string | undefined;
  pageSize: number | undefined;
}

export const BUILT_IN_BASELINE: ListBaseline = { source: "builtIn", columns: [...DEFAULT_COLUMNS], sort: undefined, pageSize: undefined };

/**
 * The baseline for a list: the class's list view (Customization), else the built-in one.
 * A saved view's resolved state will come first here (§1.3 steps 2 and 3 of the saved-views spec).
 */
export function resolveBaseline(listView: UiListView | undefined): ListBaseline {
  if (!listView) return BUILT_IN_BASELINE;
  return {
    source: "listView",
    columns: listColumns(listView.columns),
    sort: sortParam(listView.defaultSort),
    pageSize: listView.pageSize ?? undefined,
  };
}

/** A query parameter as one string ("" when absent or repeated). */
export function param(query: LocationQuery | LocationQueryRaw, key: string): string {
  const v = query[key];
  return typeof v === "string" ? v : "";
}

/** Whether the URL holds any of the context's state (§1.3 step 1): then it is shown exactly as it is. */
export function hasUrlState(query: LocationQuery | LocationQueryRaw, context: QueryContext): boolean {
  return STATE_KEYS[context].some((k) => param(query, k) !== "");
}

export function clampInt(raw: string, fallback: number, min: number, max: number): number {
  const n = raw ? Number.parseInt(raw, 10) : NaN;
  return Number.isFinite(n) ? Math.min(max, Math.max(min, n)) : fallback;
}

export const parseActive = (raw: string): Active => (raw === "all" ? "all" : raw === "false" ? "false" : undefined);
export const parseDeleted = (raw: string): Deleted => (raw === "include" ? "include" : raw === "only" ? "only" : undefined);

export const isAttributeSort = (sort: string) => attributeKey(sort.replace(/^-/, "")) !== null;

/**
 * The `columns` parameter as a list of fields: well-formed names only, each once,
 * at most MAX_COLUMNS. Empty when absent (the baseline's columns apply).
 */
export function parseColumns(raw: string): string[] {
  const out: string[] = [];
  for (const f of raw.split(",").map((s) => s.trim())) {
    if (f && FIELD_PATTERN.test(f) && !out.includes(f)) out.push(f);
    if (out.length === MAX_COLUMNS) break;
  }
  return out;
}

export const columnsParam = (columns: readonly string[]) => columns.join(",");

/**
 * Whether a field can be a column of this list: a built-in field always, an
 * attribute only when the list is of one class that has it (`attrKeys`, null
 * while the class's attributes load: attribute columns are kept until they are known).
 */
export function isUsableColumn(field: string, singleClass: boolean, attrKeys: ReadonlySet<string> | null): boolean {
  const a = attributeKey(field);
  if (a === null) return BUILTIN.has(field);
  return singleClass && (attrKeys === null || attrKeys.has(a));
}

/**
 * The columns the list shows: the URL's when it names any usable ones, else the
 * baseline's. Label always comes first (it is the row's link to the CI).
 */
export function effectiveColumns(urlColumns: readonly string[], baseline: ListBaseline, usable: (field: string) => boolean): string[] {
  const fromUrl = urlColumns.filter(usable);
  const chosen = fromUrl.length > 0 ? fromUrl : baseline.columns.filter(usable);
  return listColumns(chosen);
}

/**
 * The columns after the operator ticks or unticks one in the Columns popover.
 * Starts from what is shown, so adding a column to a list that shows the default
 * (or list view) columns keeps them (the GH#168 trap: starting from "no columns").
 * Label cannot be removed. A new column goes last.
 */
export function toggleColumn(shown: readonly string[], field: string): string[] {
  if (field === "label") return [...shown];
  if (shown.includes(field)) return shown.filter((f) => f !== field);
  return shown.length >= MAX_COLUMNS ? [...shown] : [...shown, field];
}

/**
 * The URL query after `patch` (a value of undefined or "" removes the key).
 * The page goes back to the first unless `resetPage` is false. Changing the
 * class drops an attribute sort and attribute columns: another class may not
 * have the attribute, and several classes cannot sort by one.
 */
export function patchQuery(query: LocationQuery | LocationQueryRaw, patch: Record<string, string | undefined>, resetPage = true): LocationQueryRaw {
  const next: LocationQueryRaw = { ...query };
  for (const [k, v] of Object.entries(patch)) {
    if (v) next[k] = v;
    else delete next[k];
  }
  if ("classId" in patch && param(query, "classId") !== (patch.classId ?? "")) {
    if (!("sort" in patch) && isAttributeSort(param(query, "sort"))) delete next.sort;
    if (!("columns" in patch) && param(query, "columns")) {
      const kept = parseColumns(param(query, "columns")).filter((f) => attributeKey(f) === null);
      if (kept.length > 0) next.columns = columnsParam(kept);
      else delete next.columns;
    }
  }
  if (resetPage) delete next.offset;
  return next;
}

/**
 * The query after "Clear filters": sort, page size and columns stay, unless the
 * sort or a column needs the class. `keep` names filters that stay too (the search term).
 */
export function clearedQuery(query: LocationQuery | LocationQueryRaw, keep: readonly string[] = []): LocationQueryRaw {
  const next: LocationQueryRaw = {};
  for (const k of keep) if (param(query, k)) next[k] = param(query, k);
  const sort = param(query, "sort");
  if (sort && !isAttributeSort(sort)) next.sort = sort;
  if (param(query, "limit")) next.limit = param(query, "limit");
  const columns = parseColumns(param(query, "columns")).filter((f) => attributeKey(f) === null);
  if (columns.length > 0) next.columns = columnsParam(columns);
  return next;
}
