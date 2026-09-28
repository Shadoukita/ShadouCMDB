<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useOwnersAdmin, type OwnerListQuery } from "../../../api/datamodel";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import PaginationBar from "../../../components/PaginationBar.vue";
import { useDebounced } from "../../../lib/composables";
import { useListQuery } from "../../../lib/listQuery";

/** The retired owners table, read only: the people and teams responsible for CIs. Searched, filtered, sorted and paged by the API; state in the URL. */
const KINDS = [
  { value: "team", label: "Team" },
  { value: "person", label: "Person" },
];

const lq = useListQuery({ sort: "name" });
const { get, limit, offset, update } = lq;
const query = computed<OwnerListQuery>(() => ({
  q: get("q") || undefined,
  kind: (get("kind") || undefined) as OwnerListQuery["kind"],
  isActive: get("isActive") === "true" || get("isActive") === "false" ? (get("isActive") as "true" | "false") : undefined,
  sort: lq.sort.value as OwnerListQuery["sort"],
  limit: limit.value,
  offset: offset.value,
}));
const list = useOwnersAdmin(query);
const rows = computed(() => list.data.value?.data ?? []);
const total = computed(() => list.data.value?.page.total ?? 0);
const filtered = computed(() => !!(get("q") || get("kind") || get("isActive")));

const qText = ref(get("q"));
const debouncedQ = useDebounced(qText, 300);
watch(debouncedQ, (v) => v !== get("q") && update({ q: v || undefined }));
watch(
  () => get("q"),
  (v) => (qText.value = v),
);
</script>

<template>
  <section class="panel" aria-label="Owners">
    <div class="panel-header">
      <h2>Owners</h2>
      <span v-if="list.data.value" class="muted">{{ total.toLocaleString() }}</span>
      <span v-if="list.isFetching.value && !list.isLoading.value" class="spinner" aria-label="Refreshing" />
    </div>
    <form class="toolbar" role="search" @submit.prevent>
      <div class="field search">
        <label for="own-q">Search</label>
        <input id="own-q" v-model="qText" type="search" placeholder="Name, email…" />
      </div>
      <div class="field">
        <label for="own-kind">Kind</label>
        <select id="own-kind" :value="get('kind')" @change="update({ kind: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">People and teams</option>
          <option v-for="k in KINDS" :key="k.value" :value="k.value">{{ k.label }}</option>
        </select>
      </div>
      <div class="field">
        <label for="own-active">Status</label>
        <select id="own-active" :value="get('isActive')" @change="update({ isActive: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">Any status</option>
          <option value="true">Active</option>
          <option value="false">Archived</option>
        </select>
      </div>
      <button v-if="filtered" type="button" class="btn" @click="(qText = ''), update({ q: undefined, kind: undefined, isActive: undefined })">Clear filters</button>
    </form>
    <div v-if="list.isError.value" class="panel-body"><ErrorAlert :error="list.error.value" :on-retry="() => list.refetch()" /></div>
    <LoadingState v-if="list.isLoading.value" />
    <EmptyState v-if="list.data.value && total === 0" :title="filtered ? 'No owners match these filters' : 'No owners yet'">
      {{ filtered ? "Adjust or clear the filters above." : "The former owners table has no rows." }}
    </EmptyState>
    <template v-if="rows.length > 0">
      <div class="table-wrap">
        <table :class="['data', { loading: list.isPlaceholderData.value }]">
          <thead>
            <tr>
              <th scope="col" :aria-sort="lq.ariaSort('name')"><button type="button" class="sort" @click="lq.toggleSort('name')">Name {{ lq.sortIndicator("name") }}</button></th>
              <th scope="col" :aria-sort="lq.ariaSort('kind')"><button type="button" class="sort" @click="lq.toggleSort('kind')">Kind {{ lq.sortIndicator("kind") }}</button></th>
              <th scope="col" :aria-sort="lq.ariaSort('email')"><button type="button" class="sort" @click="lq.toggleSort('email')">Email {{ lq.sortIndicator("email") }}</button></th>
              <th scope="col">External reference</th>
              <th scope="col">Status</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="o in rows" :key="o.id" :class="{ disabled: !o.isActive }">
              <td>{{ o.name }}</td>
              <td>{{ o.kind === "team" ? "Team" : "Person" }}</td>
              <td>{{ o.email ?? "" }}</td>
              <td class="mono">{{ o.externalRef ?? "" }}</td>
              <td>
                <span v-if="o.isActive" class="badge ok">Active</span>
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
