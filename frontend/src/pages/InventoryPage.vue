<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { RouterLink, useRoute, useRouter, type LocationQueryRaw } from "vue-router";
import { useCiClasses, useCiList, type CiListQuery } from "../api/queries";
import Breadcrumbs from "../components/Breadcrumbs.vue";
import DataModelEmpty from "../components/DataModelEmpty.vue";
import EmptyState from "../components/EmptyState.vue";
import ErrorAlert from "../components/ErrorAlert.vue";
import LoadingState from "../components/LoadingState.vue";
import LookupSelect from "../components/LookupSelect.vue";
import PaginationBar from "../components/PaginationBar.vue";
import StatusBadge from "../components/StatusBadge.vue";
import { useDebounced, useDocumentTitle } from "../lib/composables";
import { formatRelative } from "../lib/format";
import { flattenTree } from "../lib/tree";
import { useSessionStore } from "../stores/session";

/**
 * CI inventory. Every filter, the sort and the page live in the URL
 * (/cis?classId=…&statusId=…&q=…&sort=-updatedAt&offset=50), so a view survives
 * reload and can be bookmarked or shared. Filtering and paging happen in the API.
 */
type SortField = NonNullable<CiListQuery["sort"]>;

const FILTER_KEYS = ["q", "classId", "statusId", "environmentId", "ownerId", "locationId", "deleted"] as const;
const DEFAULT_LIMIT = 50;

const COLUMNS: { key: string; label: string; sort?: string }[] = [
  { key: "name", label: "Name", sort: "name" },
  { key: "class", label: "Class", sort: "className" },
  { key: "status", label: "Status", sort: "statusName" },
  { key: "environment", label: "Environment" },
  { key: "owner", label: "Owner" },
  { key: "location", label: "Location" },
  { key: "hostname", label: "Hostname", sort: "hostname" },
  { key: "ip", label: "IP address", sort: "ipAddress" },
  { key: "serial", label: "Serial", sort: "serialNumber" },
  { key: "updated", label: "Updated", sort: "updatedAt" },
];

const route = useRoute();
const router = useRouter();
const get = (k: string) => {
  const v = route.query[k];
  return typeof v === "string" ? v : "";
};
const limit = computed(() => clampInt(get("limit"), DEFAULT_LIMIT, 1, 200));
const offset = computed(() => clampInt(get("offset"), 0, 0, Number.MAX_SAFE_INTEGER));
const sort = computed(() => get("sort") || "name");
const deleted = computed(() => (get("deleted") === "include" ? "include" : get("deleted") === "only" ? "only" : undefined));

const query = computed<CiListQuery>(() => ({
  q: get("q") || undefined,
  classId: get("classId") || undefined,
  statusId: get("statusId") || undefined,
  environmentId: get("environmentId") || undefined,
  ownerId: get("ownerId") || undefined,
  locationId: get("locationId") || undefined,
  deleted: deleted.value,
  sort: sort.value as SortField,
  limit: limit.value,
  offset: offset.value,
}));
const list = useCiList(query);
const classes = useCiClasses();
const classTree = computed(() => flattenTree(classes.data.value ?? []));
const currentClass = computed(() => classes.data.value?.find((c) => c.id === query.value.classId));
useDocumentTitle(() => currentClass.value?.name ?? "Inventory");

function update(patch: Record<string, string | undefined>, resetPage = true) {
  const next: LocationQueryRaw = { ...route.query };
  for (const [k, v] of Object.entries(patch)) {
    if (v) next[k] = v;
    else delete next[k];
  }
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
const total = computed(() => list.data.value?.page.total ?? 0);
const rows = computed(() => list.data.value?.data ?? []);
const newTo = computed(() =>
  query.value.classId && !currentClass.value?.isAbstract ? `/cis/new?classId=${query.value.classId}` : "/cis/new",
);
const session = useSessionStore();
const canCreate = computed(() =>
  query.value.classId && currentClass.value && !currentClass.value.isAbstract
    ? session.canOnClass(query.value.classId, "create")
    : session.canOnAnyClass("create"),
);
const newLabel = computed(() => (currentClass.value && !currentClass.value.isAbstract ? currentClass.value.name : "CI"));
const crumbs = computed(() =>
  currentClass.value ? [{ label: "Inventory", to: "/cis" }, { label: currentClass.value.name }] : [{ label: "Inventory" }],
);

const toggleSort = (field: string) => update({ sort: sort.value === field ? `-${field}` : field }, true);

function clearFilters() {
  qText.value = "";
  const next: LocationQueryRaw = {};
  if (get("sort")) next.sort = get("sort");
  if (get("limit")) next.limit = get("limit");
  router.push({ path: "/cis", query: next });
}

function onPage(p: { limit: number; offset: number }) {
  update(
    { limit: p.limit === DEFAULT_LIMIT ? undefined : String(p.limit), offset: p.offset ? String(p.offset) : undefined },
    false,
  );
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
        <input id="f-q" v-model="qText" type="search" placeholder="Name, hostname, IP, serial, notes…" />
      </div>
      <div class="field">
        <label for="f-class">Class</label>
        <select id="f-class" :value="get('classId')" @change="update({ classId: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">All classes</option>
          <option v-for="n in classTree" :key="n.item.id" :value="n.item.id">
            {{ "\u00a0\u00a0".repeat(n.depth) }}{{ n.item.name }}{{ n.item.isAbstract ? " (incl. subclasses)" : "" }}{{ n.item.isActive ? "" : " (archived)" }}
          </option>
        </select>
      </div>
      <div class="field">
        <label for="f-status">Status</label>
        <LookupSelect
          id="f-status"
          kind="statuses"
          :model-value="get('statusId')"
          empty-label="Any status"
          @update:model-value="(v) => update({ statusId: v || undefined })"
        />
      </div>
      <div class="field">
        <label for="f-env">Environment</label>
        <LookupSelect
          id="f-env"
          kind="environments"
          :model-value="get('environmentId')"
          empty-label="Any environment"
          @update:model-value="(v) => update({ environmentId: v || undefined })"
        />
      </div>
      <div class="field">
        <label for="f-owner">Owner</label>
        <LookupSelect
          id="f-owner"
          kind="owners"
          :model-value="get('ownerId')"
          empty-label="Any owner"
          @update:model-value="(v) => update({ ownerId: v || undefined })"
        />
      </div>
      <div class="field">
        <label for="f-location">Location</label>
        <LookupSelect
          id="f-location"
          kind="locations"
          :model-value="get('locationId')"
          empty-label="Any location"
          @update:model-value="(v) => update({ locationId: v || undefined })"
        />
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
    <LoadingState v-if="list.isLoading.value" label="Loading inventory…" />

    <DataModelEmpty v-if="list.data.value && total === 0 && activeFilters.length === 0 && classes.data.value?.length === 0" />
    <EmptyState v-else-if="list.data.value && total === 0 && activeFilters.length === 0" title="The inventory is empty">
      Configuration items are the servers, VMs, applications, databases, network devices and locations you track. Create
      one, then relate it to others from its detail page.
      <template v-if="canCreate" #actions>
        <RouterLink class="btn btn-primary" to="/cis/new">+ Create your first configuration item</RouterLink>
      </template>
    </EmptyState>
    <EmptyState v-if="list.data.value && total === 0 && activeFilters.length > 0" title="No configuration items match these filters">
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
              <th v-for="c in COLUMNS" :key="c.key" scope="col" :aria-sort="c.sort ? ariaSort(c.sort) : undefined">
                <button v-if="c.sort" type="button" class="sort" @click="toggleSort(c.sort)">
                  {{ c.label }} {{ sortIndicator(c.sort) }}
                </button>
                <template v-else>{{ c.label }}</template>
              </th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="ci in rows" :key="ci.id" :class="{ deleted: ci.deletedAt }">
              <td><RouterLink :to="`/cis/${ci.id}`">{{ ci.name }}</RouterLink></td>
              <td>{{ ci.class.name }}</td>
              <td>
                <span v-if="ci.deletedAt" class="badge danger">Deleted</span>
                <StatusBadge v-else :status="ci.status" />
              </td>
              <td><template v-if="ci.environment">{{ ci.environment.name }}</template><span v-else class="muted">—</span></td>
              <td><template v-if="ci.owner">{{ ci.owner.name }}</template><span v-else class="muted">—</span></td>
              <td><template v-if="ci.location">{{ ci.location.name }}</template><span v-else class="muted">—</span></td>
              <td class="mono">{{ ci.hostname ?? "" }}</td>
              <td class="mono">{{ ci.ipAddress ?? "" }}</td>
              <td class="mono">{{ ci.serialNumber ?? "" }}</td>
              <td :title="ci.updatedAt">{{ formatRelative(ci.updatedAt) }}</td>
            </tr>
          </tbody>
        </table>
      </div>
      <PaginationBar :total="total" :limit="limit" :offset="offset" @change="onPage" />
    </template>
  </section>
</template>
