import { computed, ref, toValue, watch, type MaybeRefOrGetter } from "vue";
import { useRoute, useRouter, type LocationQueryRaw, type RouteLocationNormalizedLoaded, type Router } from "vue-router";
import type { UiListFilters, UiListView } from "../api/uiSettings";
import {
  clampInt,
  clearedQuery,
  columnsParam,
  DEFAULT_LIMIT,
  DEFAULT_SORT,
  effectiveColumns,
  FILTER_KEYS,
  isUsableColumn,
  param,
  parseActive,
  parseColumns,
  parseDeleted,
  patchQuery,
  resolveBaseline,
  toggleColumn,
  type QueryContext,
} from "./inventoryQuery";
import { isInAppNavigation } from "./navigation";
import { attributeKey, BUILTIN, hasFilters } from "./uiSettings";

export interface QueryStateOptions {
  context: QueryContext;
  /** The CI classes; undefined until they have loaded. */
  classes: MaybeRefOrGetter<readonly { id: string; key: string }[] | undefined>;
  /** Whether the UI settings (the classes' list views) have loaded, or failed to: until then a class's baseline is unknown. */
  settingsLoaded: MaybeRefOrGetter<boolean>;
  /** The class's list view from UI settings, if it has one. */
  listViewFor: (classKey: string | undefined) => UiListView | undefined;
  /** A list view's default lookup filters as `lookupValueId`; null while the lookup lists load. */
  lookupValueIds?: (lookups: UiListFilters["lookups"]) => string | undefined | null;
  /** Attribute keys of the listed class, or null while they load (or with no single class). */
  attributeKeys?: MaybeRefOrGetter<ReadonlySet<string> | null>;
  /**
   * True while a saved view is about to fill the URL (a `view=` link, the user's default:
   * lib/useSavedViewState): the list is not queried and no list view filters are written meanwhile.
   */
  hold?: MaybeRefOrGetter<boolean>;
  /** For tests; the app uses the current route and router. */
  route?: Pick<RouteLocationNormalizedLoaded, "query">;
  router?: Pick<Router, "push" | "replace">;
  inAppNavigation?: () => boolean;
}

/**
 * The query state of the inventory or of the search page, read from and written
 * to the URL (see lib/inventoryQuery for the precedence). Both pages share it,
 * so their filters behave the same and a saved view can hold either.
 */
export function useInventoryQueryState(options: QueryStateOptions) {
  const route = options.route ?? useRoute();
  const router = options.router ?? useRouter();
  const inApp = options.inAppNavigation ?? isInAppNavigation;
  const path = options.context === "inventory" ? "/cis" : "/search";
  const get = (k: string) => param(route.query, k);

  const classId = computed(() => get("classId"));
  /** The one class the list is of, when the URL names exactly one (attribute sorts and columns need it). */
  const currentClass = computed(() => {
    const id = classId.value;
    return id && !id.includes(",") ? toValue(options.classes)?.find((c) => c.id === id) : undefined;
  });
  const listView = computed(() => options.listViewFor(currentClass.value?.key));
  const baseline = computed(() => resolveBaseline(listView.value));

  const defaultLimit = computed(() => baseline.value.pageSize ?? DEFAULT_LIMIT);
  const limit = computed(() => clampInt(get("limit"), defaultLimit.value, 1, 200));
  const offset = computed(() => clampInt(get("offset"), 0, 0, Number.MAX_SAFE_INTEGER));
  const sort = computed(() => get("sort") || baseline.value.sort || DEFAULT_SORT);
  const active = computed(() => parseActive(get("active")));
  const deleted = computed(() => parseDeleted(get("deleted")));

  /** The columns the URL names; empty when it names none (the baseline's apply). */
  const urlColumns = computed(() => parseColumns(get("columns")));
  const usableColumn = (field: string) => isUsableColumn(field, !!currentClass.value, toValue(options.attributeKeys) ?? null);
  const columns = computed(() => effectiveColumns(urlColumns.value, baseline.value, usableColumn));
  const columnsCustomized = computed(() => urlColumns.value.length > 0);

  // Default filters: navigating to a class list (menu, links) with nothing but the class in the URL
  // writes its list view's filters into it, so they show in the toolbar and the operator can change them.
  // A reload or Back shows the URL as it is, so a cleared filter stays cleared.
  const defaultsFor = ref<string | null>(null);
  if (options.context === "inventory") {
    watch(
      classId,
      (id) => {
        defaultsFor.value = id && inApp() && Object.keys(route.query).every((k) => k === "classId") ? id : null;
      },
      { immediate: true },
    );
    /** The list view's default lookup filters as value ids; null while the lookup lists load. */
    const defaultLookupIds = computed(() => {
      const lookups = listView.value?.defaultFilters?.lookups;
      return lookups && Object.keys(lookups).length > 0 ? options.lookupValueIds?.(lookups) : undefined;
    });
    const onlyClass = computed(() => Object.keys(route.query).every((k) => k === "classId"));
    watch(
      () => [defaultsFor.value, currentClass.value, toValue(options.settingsLoaded), listView.value, defaultLookupIds.value, toValue(options.hold), onlyClass.value] as const,
      ([forId]) => {
        if (!forId) return;
        // A saved view (the user's default for this list) filled the URL instead: it replaces the list view's filters.
        if (forId !== classId.value || !onlyClass.value) return void (defaultsFor.value = null);
        if (toValue(options.hold)) return;
        if (!currentClass.value || !toValue(options.settingsLoaded)) return;
        const f = listView.value?.defaultFilters;
        if (!hasFilters(f)) return void (defaultsFor.value = null);
        if (defaultLookupIds.value === null) return; // lookups still loading
        const lookupValueId = defaultLookupIds.value;
        const next: LocationQueryRaw = { classId: forId };
        if (f!.q) next.q = f!.q;
        if (lookupValueId) next.lookupValueId = lookupValueId;
        // Pending until the URL has them, so the list is queried once, with the filters.
        void router.replace({ path, query: next }).finally(() => {
          if (defaultsFor.value === forId) defaultsFor.value = null;
        });
      },
      { immediate: true },
    );
  }

  /**
   * Whether the list may be queried: its baseline (sort, page size) is known and no
   * default filters are about to be written into the URL. Querying earlier would fetch
   * the page and its total once with the built-in sort and again with the list view's
   * (GH#167). A URL that names both the sort and the page size needs no list view.
   */
  const settled = computed(
    () =>
      !toValue(options.hold) &&
      defaultsFor.value === null &&
      ((!!get("sort") && !!get("limit")) || !classId.value || (toValue(options.classes) !== undefined && toValue(options.settingsLoaded))),
  );

  const filters = computed(() => ({
    q: get("q") || undefined,
    classId: classId.value || undefined,
    lookupValueId: get("lookupValueId") || undefined,
    criticalityValueId: get("criticalityValueId") || undefined,
    ipWithin: get("ipWithin") || undefined,
    active: active.value,
    deleted: deleted.value,
  }));
  /** Parameters of the list request (GET /configuration-items). */
  const listQuery = computed(() => ({ ...filters.value, sort: sort.value, limit: limit.value, offset: offset.value }));
  /** Parameters of the search request (GET /search), without the term. */
  const searchFilters = computed(() => ({ ...filters.value, limit: limit.value, offset: offset.value }));

  /** The filters set (on the search page the term is the search itself, not a filter). */
  const activeFilters = computed(() => FILTER_KEYS.filter((k) => get(k) && !(options.context === "search" && k === "q")));

  function update(patch: Record<string, string | undefined>, resetPage = true) {
    const to = { path, query: patchQuery(route.query, patch, resetPage) };
    // Typing a search term replaces the entry, so Back does not step through every keystroke.
    return "q" in patch ? router.replace(to) : router.push(to);
  }

  const setColumns = (next: readonly string[]) => update({ columns: columnsParam(next) || undefined }, false);
  const toggle = (field: string) => setColumns(toggleColumn(columns.value, field));
  const resetColumns = () => update({ columns: undefined }, false);

  const clearFilters = () => router.push({ path, query: clearedQuery(route.query, options.context === "search" ? ["q"] : []) });
  /** The sort and page size in effect, which a URL without them stands for (comparing with a saved view). */
  const stateDefaults = computed(() => ({ sort: sort.value, limit: limit.value }));

  function onPage(p: { limit: number; offset: number }) {
    update({ limit: p.limit === defaultLimit.value ? undefined : String(p.limit), offset: p.offset ? String(p.offset) : undefined }, false);
  }

  /**
   * The sort a column header toggles: a built-in field's, or the attribute's own when
   * the list is of one class (the API sorts by an attribute only within a class) and
   * `sortable` says the attribute is not a reference.
   */
  function columnSort(field: string, sortable: (key: string) => boolean): string | undefined {
    const a = attributeKey(field);
    if (a === null) return BUILTIN.get(field)?.sort;
    return currentClass.value && sortable(a) ? field : undefined;
  }
  const toggleSort = (field: string) => update({ sort: sort.value === field ? `-${field}` : field });
  const sortIndicator = (field: string) => (sort.value === field ? "▲" : sort.value === `-${field}` ? "▼" : "");
  const ariaSort = (field: string): "ascending" | "descending" | "none" =>
    sort.value === field ? "ascending" : sort.value === `-${field}` ? "descending" : "none";

  return {
    get,
    classId,
    currentClass,
    baseline,
    defaultLimit,
    limit,
    offset,
    sort,
    active,
    deleted,
    columns,
    columnsCustomized,
    settled,
    listQuery,
    searchFilters,
    activeFilters,
    update,
    setColumns,
    toggleColumn: toggle,
    resetColumns,
    clearFilters,
    stateDefaults,
    onPage,
    columnSort,
    toggleSort,
    sortIndicator,
    ariaSort,
  };
}
