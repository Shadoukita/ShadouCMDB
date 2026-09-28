<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useLocationsAdmin, type LocationListQuery } from "../../../api/datamodel";
import { useLookup } from "../../../api/queries";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import PaginationBar from "../../../components/PaginationBar.vue";
import { useDebounced } from "../../../lib/composables";
import { useListQuery } from "../../../lib/listQuery";

/** The retired locations table, read only: a tree (region › site › building › room › rack). Searched, filtered, sorted and paged by the API; state in the URL. */
const LOCATION_TYPES = [
  { value: "region", label: "Region" },
  { value: "site", label: "Site" },
  { value: "building", label: "Building" },
  { value: "floor", label: "Floor" },
  { value: "room", label: "Room" },
  { value: "rack", label: "Rack" },
  { value: "cloud_region", label: "Cloud region" },
  { value: "other", label: "Other" },
];
const typeLabel = (t: string) => LOCATION_TYPES.find((x) => x.value === t)?.label ?? t;

const lq = useListQuery({ sort: "name" });
const { get, limit, offset, update } = lq;
const query = computed<LocationListQuery>(() => ({
  q: get("q") || undefined,
  locationType: (get("locationType") || undefined) as LocationListQuery["locationType"],
  isActive: get("isActive") === "true" || get("isActive") === "false" ? (get("isActive") as "true" | "false") : undefined,
  sort: lq.sort.value as LocationListQuery["sort"],
  limit: limit.value,
  offset: offset.value,
}));
const list = useLocationsAdmin(query);
/** Every location (up to the API's page limit) for parent names and the parent picker. */
const tree = useLookup("locations");
const nameOf = (id: string | null) => (id ? (tree.data.value?.find((l) => l.id === id)?.name ?? "") : "");
const rows = computed(() => list.data.value?.data ?? []);
const total = computed(() => list.data.value?.page.total ?? 0);
const filtered = computed(() => !!(get("q") || get("locationType") || get("isActive")));

const qText = ref(get("q"));
const debouncedQ = useDebounced(qText, 300);
watch(debouncedQ, (v) => v !== get("q") && update({ q: v || undefined }));
watch(
  () => get("q"),
  (v) => (qText.value = v),
);
</script>

<template>
  <section class="panel" aria-label="Locations">
    <div class="panel-header">
      <h2>Locations</h2>
      <span v-if="list.data.value" class="muted">{{ total.toLocaleString() }}</span>
      <span v-if="list.isFetching.value && !list.isLoading.value" class="spinner" aria-label="Refreshing" />
    </div>
    <form class="toolbar" role="search" @submit.prevent>
      <div class="field search">
        <label for="loc-q">Search</label>
        <input id="loc-q" v-model="qText" type="search" placeholder="Name, key, address…" />
      </div>
      <div class="field">
        <label for="loc-type">Type</label>
        <select id="loc-type" :value="get('locationType')" @change="update({ locationType: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">Any type</option>
          <option v-for="t in LOCATION_TYPES" :key="t.value" :value="t.value">{{ t.label }}</option>
        </select>
      </div>
      <div class="field">
        <label for="loc-active">Status</label>
        <select id="loc-active" :value="get('isActive')" @change="update({ isActive: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">Any status</option>
          <option value="true">Active</option>
          <option value="false">Archived</option>
        </select>
      </div>
      <button v-if="filtered" type="button" class="btn" @click="(qText = ''), update({ q: undefined, locationType: undefined, isActive: undefined })">Clear filters</button>
    </form>
    <div v-if="list.isError.value" class="panel-body"><ErrorAlert :error="list.error.value" :on-retry="() => list.refetch()" /></div>
    <LoadingState v-if="list.isLoading.value" />
    <EmptyState v-if="list.data.value && total === 0" :title="filtered ? 'No locations match these filters' : 'No locations yet'">
      {{ filtered ? "Adjust or clear the filters above." : "The former locations table has no rows." }}
    </EmptyState>
    <template v-if="rows.length > 0">
      <div class="table-wrap">
        <table :class="['data', { loading: list.isPlaceholderData.value }]">
          <thead>
            <tr>
              <th scope="col" :aria-sort="lq.ariaSort('name')"><button type="button" class="sort" @click="lq.toggleSort('name')">Name {{ lq.sortIndicator("name") }}</button></th>
              <th scope="col" :aria-sort="lq.ariaSort('key')"><button type="button" class="sort" @click="lq.toggleSort('key')">Key {{ lq.sortIndicator("key") }}</button></th>
              <th scope="col">Type</th>
              <th scope="col">Inside</th>
              <th scope="col">Address</th>
              <th scope="col">Status</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="l in rows" :key="l.id" :class="{ disabled: !l.isActive }">
              <td>{{ l.name }}</td>
              <td class="mono">{{ l.key }}</td>
              <td>{{ typeLabel(l.locationType) }}</td>
              <td>{{ nameOf(l.parentId) }}</td>
              <td class="muted">{{ l.address ?? "" }}</td>
              <td>
                <span v-if="l.isActive" class="badge ok">Active</span>
                <span v-else class="badge off">Archived</span>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <PaginationBar :total="total" :limit="limit" :offset="offset" @change="lq.onPage" />
    </template>
  </section>
</template>
