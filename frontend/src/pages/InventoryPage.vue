<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { RouterLink, useRoute, useRouter, type LocationQueryRaw } from "vue-router";
import { useAllLookupListValues, useAreas, useLookupLists } from "../api/datamodel";
import { useCiClasses, useCiList, useClassAttributes, type CiListQuery } from "../api/queries";
import Breadcrumbs from "../components/Breadcrumbs.vue";
import CiCell from "../components/CiCell.vue";
import DataModelEmpty from "../components/DataModelEmpty.vue";
import EmptyState from "../components/EmptyState.vue";
import ErrorAlert from "../components/ErrorAlert.vue";
import LoadingState from "../components/LoadingState.vue";
import PaginationBar from "../components/PaginationBar.vue";
import { useAppSettings } from "../lib/appSettings";
import { useDebounced, useDocumentTitle } from "../lib/composables";
import { isInAppNavigation } from "../lib/navigation";
import { groupByArea } from "../lib/areas";
import { viewableClasses } from "../lib/permissions";
import { flattenTree } from "../lib/tree";
import { attributeKey, BUILTIN, DEFAULT_COLUMNS, fieldLabel, hasFilters, isSortableAttribute, listViewFor, lookupValueIds, sortParam } from "../lib/uiSettings";
import { useSessionStore } from "../stores/session";

/**
 * CI inventory. Every filter, the sort and the page live in the URL
 * (/cis?classId=…&lookupValueId=…&q=…&sort=-updatedAt&offset=50), so a view survives
 * reload and can be bookmarked or shared. Filtering and paging happen in the API.
 *
 * A class's list view (Administration › Customization › List views) sets its
 * columns, default sort and page size, and default filters that are written
 * into the URL when the operator navigates to the class list without any.
 */
type SortField = NonNullable<CiListQuery["sort"]>;

const FILTER_KEYS = ["q", "classId", "lookupValueId", "active", "deleted"] as const;
const DEFAULT_LIMIT = 50;

const route = useRoute();
const router = useRouter();
const get = (k: string) => {
  const v = route.query[k];
  return typeof v === "string" ? v : "";
};
const classes = useCiClasses();
const currentClass = computed(() => classes.data.value?.find((c) => c.id === get("classId")));
const settings = useAppSettings();
const view = computed(() => listViewFor(settings.doc.value, currentClass.value?.key));
const defaultLimit = computed(() => view.value?.pageSize ?? DEFAULT_LIMIT);

const limit = computed(() => clampInt(get("limit"), defaultLimit.value, 1, 200));
const offset = computed(() => clampInt(get("offset"), 0, 0, Number.MAX_SAFE_INTEGER));
const sort = computed(() => get("sort") || sortParam(view.value?.defaultSort) || "label");
const deleted = computed(() => (get("deleted") === "include" ? "include" : get("deleted") === "only" ? "only" : undefined));
/** Validity: only active CIs (the API's default), all, or only inactive ones. */
const active = computed(() => (get("active") === "all" ? "all" : get("active") === "false" ? "false" : undefined));

const query = computed<CiListQuery>(() => ({
  q: get("q") || undefined,
  classId: get("classId") || undefined,
  lookupValueId: get("lookupValueId") || undefined,
  active: active.value,
  deleted: deleted.value,
  sort: sort.value as SortField,
  limit: limit.value,
  offset: offset.value,
}));
/**
 * Whether the class's list view is known. Its default sort and page size feed the
 * query, so running it earlier would fetch the page (and its total) with the fallback
 * sort only to fetch it again when the view arrives. A URL that names both needs no view.
 */
const viewSettled = computed(
  () =>
    (!!get("sort") && !!get("limit")) ||
    !get("classId") ||
    (classes.isFetched.value && settings.query.isFetched.value),
);
const list = useCiList(query, viewSettled);
const areas = useAreas();
const session = useSessionStore();
// The class filter offers only what the user may view; the API would answer any other class with an empty list.
const classOptions = computed(() => viewableClasses(classes.data.value ?? [], (id) => session.canOnClass(id, "view")));
const classDenied = computed(() => !!currentClass.value && !classOptions.value.some((c) => c.id === currentClass.value!.id));
const classTree = computed(() => flattenTree(classOptions.value));
const classGroups = computed(() => groupByArea(classTree.value, (n) => n.item.areaId, areas.data.value ?? []));
const currentArea = computed(() => areas.data.value?.find((a) => a.id === currentClass.value?.areaId));

const columns = computed(() => (view.value?.columns?.length ? view.value.columns : DEFAULT_COLUMNS));
const attrColumns = computed(() => columns.value.some((c) => attributeKey(c) !== null));
const attrs = useClassAttributes(() => (attrColumns.value ? currentClass.value?.id : undefined));
const attrDefs = computed(() => attrs.data.value ?? []);
const columnLabel = (field: string) => fieldLabel(field, attrDefs.value);
/**
 * The sort a column header toggles: a built-in field's, or the attribute's own when
 * the list is of one class (the API sorts by an attribute only within a class) and
 * the attribute is not a reference.
 */
const columnSort = (field: string): string | undefined => {
  const a = attributeKey(field);
  if (a === null) return BUILTIN.get(field)?.sort;
  const def = currentClass.value ? attrDefs.value.find((d) => d.key === a) : undefined;
  return def && isSortableAttribute(def) ? field : undefined;
};

// Default filters: navigating to a class list (menu, links) with nothing but the class in the URL
// writes the view's filters into it, so they show in the toolbar and the operator can change them.
// A reload or Back shows the URL as it is, so a cleared filter stays cleared.
const lookupLists = useLookupLists();
const lookupValues = useAllLookupListValues();
const defaultsFor = ref<string | null>(null);
watch(
  () => get("classId"),
  (id) => {
    defaultsFor.value = id && isInAppNavigation() && Object.keys(route.query).every((k) => k === "classId") ? id : null;
  },
  { immediate: true },
);
watch(
  () => [defaultsFor.value, currentClass.value, settings.query.isFetched.value, view.value, lookupLists.data.value, lookupValues.data.value] as const,
  ([classId]) => {
    if (!classId || classId !== get("classId") || !currentClass.value || !settings.query.isFetched.value) return;
    const f = view.value?.defaultFilters;
    if (!hasFilters(f)) {
      defaultsFor.value = null;
      return;
    }
    const usesLookups = Object.keys(f!.lookups ?? {}).length > 0;
    if (usesLookups && (!lookupLists.data.value || !lookupValues.data.value)) return; // lookups still loading
    const lookupValueId = usesLookups ? lookupValueIds(f!.lookups, lookupLists.data.value!, lookupValues.data.value!) : undefined;
    defaultsFor.value = null;
    const next: LocationQueryRaw = { classId };
    if (f!.q) next.q = f!.q;
    if (lookupValueId) next.lookupValueId = lookupValueId;
    router.replace({ path: "/cis", query: next });
  },
  { immediate: true },
);

useDocumentTitle(() => currentClass.value?.name ?? "Inventory");

function update(patch: Record<string, string | undefined>, resetPage = true) {
  const next: LocationQueryRaw = { ...route.query };
  for (const [k, v] of Object.entries(patch)) {
    if (v) next[k] = v;
    else delete next[k];
  }
  // An attribute sort belongs to its class: another class may not have the attribute, and no class cannot sort by one.
  if ("classId" in patch && !("sort" in patch) && isAttributeSort(get("sort"))) delete next.sort;
  if (resetPage) delete next.offset;
  const to = { path: "/cis", query: next };
  if ("q" in patch) router.replace(to);
  else router.push(to);
}

// Search box: local state for typing, debounced into the URL.
const qText = ref(get("q"));
const debouncedQ = useDebounced(qText, 300);
watch(debouncedQ, (v) => {
  if (v !== get("q")) update({ q: v || undefined });
});
watch(
  () => get("q"),
  (v) => (qText.value = v), // back/forward
);

const activeFilters = computed(() => FILTER_KEYS.filter((k) => get(k)));
/** The lookup values the list is filtered by (from a dashboard link or a list view's default filters), by name. */
const lookupFilterNames = computed(() =>
  get("lookupValueId")
    .split(",")
    .filter(Boolean)
    .map((id) => lookupValues.data.value?.find((v) => v.id === id)?.name ?? (lookupValues.isLoading.value ? "…" : "Unknown value")),
);
const total = computed(() => list.data.value?.page.total ?? 0);
const rows = computed(() => list.data.value?.data ?? []);
const newTo = computed(() =>
  query.value.classId && !currentClass.value?.isAbstract ? `/cis/new?classId=${query.value.classId}` : "/cis/new",
);
const canCreate = computed(() =>
  query.value.classId && currentClass.value && !currentClass.value.isAbstract
    ? session.canOnClass(query.value.classId, "create")
    : session.canOnAnyClass("create"),
);
const newLabel = computed(() => (currentClass.value && !currentClass.value.isAbstract ? currentClass.value.name : "CI"));
const crumbs = computed(() =>
  currentClass.value
    ? [{ label: "Inventory", to: "/cis" }, ...(currentArea.value ? [{ label: currentArea.value.name }] : []), { label: currentClass.value.name }]
    : [{ label: "Inventory" }],
);

const toggleSort = (field: string) => update({ sort: sort.value === field ? `-${field}` : field }, true);

function clearFilters() {
  qText.value = "";
  const next: LocationQueryRaw = {};
  if (get("sort") && !isAttributeSort(get("sort"))) next.sort = get("sort");
  if (get("limit")) next.limit = get("limit");
  router.push({ path: "/cis", query: next });
}

function onPage(p: { limit: number; offset: number }) {
  update(
    { limit: p.limit === defaultLimit.value ? undefined : String(p.limit), offset: p.offset ? String(p.offset) : undefined },
    false,
  );
}

function isAttributeSort(sortValue: string): boolean {
  return attributeKey(sortValue.replace(/^-/, "")) !== null;
}

function clampInt(raw: string, fallback: number, min: number, max: number): number {
  const n = raw ? Number.parseInt(raw, 10) : NaN;
  return Number.isFinite(n) ? Math.min(max, Math.max(min, n)) : fallback;
}

function sortIndicator(field: string): string {
  if (sort.value === field) return "▲";
  if (sort.value === `-${field}`) return "▼";
  return "";
}

function ariaSort(field: string): "ascending" | "descending" | "none" {
  if (sort.value === field) return "ascending";
  if (sort.value === `-${field}`) return "descending";
  return "none";
}
</script>

<template>
  <Breadcrumbs :items="crumbs" />
  <div class="page-header">
    <div class="title">
      <h1>{{ currentClass ? currentClass.name : "Configuration items" }}</h1>
      <span v-if="list.data.value" class="muted">{{ total.toLocaleString() }} total</span>
      <span v-if="list.isFetching.value && !list.isLoading.value" class="spinner" aria-label="Refreshing" />
    </div>
    <div v-if="canCreate" class="actions">
      <RouterLink class="btn btn-primary" :to="newTo">+ New {{ newLabel }}</RouterLink>
    </div>
  </div>

  <section class="panel" aria-label="Inventory">
    <form class="toolbar" role="search" @submit.prevent>
      <div class="field search">
        <label for="f-q">Search</label>
        <input id="f-q" v-model="qText" type="search" placeholder="Label, ident, attribute values…" />
      </div>
      <div class="field">
        <label for="f-class">Class</label>
        <select id="f-class" :value="get('classId')" @change="update({ classId: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">All classes</option>
          <option v-if="classDenied && currentClass" :value="currentClass.id">{{ currentClass.name }}</option>
          <optgroup v-for="g in classGroups" :key="g.area?.id ?? '-'" :label="g.area?.name ?? 'Other'">
            <option v-for="n in g.items" :key="n.item.id" :value="n.item.id">
              {{ "\u00a0\u00a0".repeat(n.depth) }}{{ n.item.name }}{{ n.item.isAbstract ? " (incl. subclasses)" : "" }}{{ n.item.isActive ? "" : " (archived)" }}
            </option>
          </optgroup>
        </select>
      </div>
      <div v-if="lookupFilterNames.length > 0" class="field">
        <span class="label">Lookup values</span>
        <span class="checkbox-row">
          {{ lookupFilterNames.join(", ") }}
          <button type="button" class="btn btn-sm" aria-label="Remove the lookup value filter" @click="update({ lookupValueId: undefined })">×</button>
        </span>
      </div>
      <div class="field">
        <label for="f-active">Validity</label>
        <select id="f-active" :value="active ?? ''" @change="update({ active: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">Active only</option>
          <option value="all">Show inactive</option>
          <option value="false">Only inactive</option>
        </select>
      </div>
      <div class="field">
        <label for="f-deleted">Deleted CIs</label>
        <select id="f-deleted" :value="deleted ?? ''" @change="update({ deleted: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">Hide</option>
          <option value="include">Include</option>
          <option value="only">Only deleted</option>
        </select>
      </div>
      <button v-if="activeFilters.length > 0" type="button" class="btn" @click="clearFilters">Clear filters</button>
    </form>

    <div v-if="list.isError.value" class="panel-body">
      <ErrorAlert :error="list.error.value" :on-retry="() => list.refetch()" />
    </div>
    <LoadingState v-if="list.isPending.value" label="Loading inventory…" />

    <EmptyState v-if="classDenied" title="Permission denied">
      None of your permission profiles allows viewing {{ currentClass?.name }} configuration items, so none are listed here.
      <template #actions><RouterLink class="btn" to="/cis">Back to inventory</RouterLink></template>
    </EmptyState>
    <DataModelEmpty v-else-if="list.data.value && total === 0 && activeFilters.length === 0 && classes.data.value?.length === 0" />
    <EmptyState v-else-if="list.data.value && total === 0 && activeFilters.length === 0" title="The inventory is empty">
      Configuration items are the servers, VMs, applications, databases, network devices and locations you track. Create
      one, then relate it to others from its detail page.
      <template v-if="canCreate" #actions>
        <RouterLink class="btn btn-primary" to="/cis/new">+ Create your first configuration item</RouterLink>
      </template>
    </EmptyState>
    <EmptyState v-else-if="list.data.value && total === 0 && activeFilters.length > 0" title="No configuration items match these filters">
      Adjust or clear the filters above.
    </EmptyState>
    <EmptyState v-if="list.data.value && total > 0 && rows.length === 0" title="This page is past the end of the results">
      <template #actions><button class="btn" @click="update({}, true)">Go to first page</button></template>
    </EmptyState>

    <template v-if="rows.length > 0">
      <div class="table-wrap">
        <table :class="['data', { loading: list.isPlaceholderData.value }]">
          <thead>
            <tr>
              <th v-for="c in columns" :key="c" scope="col" :aria-sort="columnSort(c) ? ariaSort(columnSort(c)!) : undefined">
                <button v-if="columnSort(c)" type="button" class="sort" @click="toggleSort(columnSort(c)!)">
                  {{ columnLabel(c) }} {{ sortIndicator(columnSort(c)!) }}
                </button>
                <template v-else>{{ columnLabel(c) }}</template>
              </th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="ci in rows" :key="ci.id" :class="{ deleted: ci.deletedAt }">
              <td v-for="c in columns" :key="c"><CiCell :ci="ci" :field="c" :defs="attrDefs" /></td>
            </tr>
          </tbody>
        </table>
      </div>
      <PaginationBar :total="total" :limit="limit" :offset="offset" @change="onPage" />
    </template>
  </section>
</template>
