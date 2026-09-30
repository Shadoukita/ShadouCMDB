<script setup lang="ts">
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { useCiClasses, useSearch } from "../api/queries";
import Breadcrumbs from "../components/Breadcrumbs.vue";
import EmptyState from "../components/EmptyState.vue";
import ErrorAlert from "../components/ErrorAlert.vue";
import InventoryFilters from "../components/InventoryFilters.vue";
import LoadingState from "../components/LoadingState.vue";
import PaginationBar from "../components/PaginationBar.vue";
import CiStateBadge from "../components/CiStateBadge.vue";
import { useDocumentTitle } from "../lib/composables";
import { plural } from "../lib/format";
import { useInventoryQueryState } from "../lib/useInventoryQueryState";

/**
 * Full global-search results (ranked by the API), with the field that matched.
 * The term and the filters live in the URL (/search?q=…&classId=…&active=all),
 * in the same state as the inventory's (lib/inventoryQuery): search has no sort
 * (it is ranked) and no column choice.
 */
const classes = useCiClasses();
const state = useInventoryQueryState({
  context: "search",
  classes: () => classes.data.value,
  settingsLoaded: true,
  listViewFor: () => undefined,
});
const { limit, offset, activeFilters } = state;
const q = computed(() => state.get("q"));
const filters = computed(() => {
  const { q: _q, limit: _l, offset: _o, ...f } = state.searchFilters.value;
  return f;
});
const search = useSearch(q, limit, offset, filters);
useDocumentTitle(() => (q.value ? `Search: ${q.value}` : "Search"));
const rows = computed(() => search.data.value?.data ?? []);
/** The same term and filters as a sortable, pageable inventory list. */
const inventoryLink = computed(() => {
  const query: Record<string, string> = {};
  for (const k of ["q", ...activeFilters.value]) if (state.get(k)) query[k] = state.get(k);
  return { path: "/cis", query };
});
</script>

<template>
  <Breadcrumbs :items="[{ label: 'Search' }]" />
  <div class="page-header">
    <div class="title">
      <h1>{{ q ? `Results for “${q}”` : "Search" }}</h1>
      <span v-if="search.data.value" class="muted">{{ plural(search.data.value.page.total, "match", "matches") }}</span>
    </div>
    <RouterLink v-if="q" class="btn" :to="inventoryLink">Open as filterable inventory</RouterLink>
  </div>
  <section class="panel" aria-label="Search results">
    <div v-if="q" class="toolbar" role="group" aria-label="Filter the results">
      <InventoryFilters :state="state" id-prefix="s" />
      <button v-if="activeFilters.length > 0" type="button" class="btn" @click="state.clearFilters()">Clear filters</button>
    </div>
    <EmptyState v-if="!q" title="Type in the search box above">
      Search covers labels, idents and attribute values, including IP addresses and networks.
    </EmptyState>
    <LoadingState v-if="search.isLoading.value" label="Searching…" />
    <div v-if="search.isError.value" class="panel-body">
      <ErrorAlert :error="search.error.value" :on-retry="() => search.refetch()" />
    </div>
    <EmptyState v-if="search.data.value && rows.length === 0 && activeFilters.length > 0" :title="`No configuration item matches “${q}” with these filters`">
      Adjust or clear the filters above.
    </EmptyState>
    <EmptyState v-else-if="search.data.value && rows.length === 0" :title="`No configuration item matches “${q}”`">
      Try a shorter term, an IP address, or a CIDR like 10.0.0.0/24.
    </EmptyState>
    <template v-if="rows.length > 0">
      <div class="table-wrap">
        <table :class="['data', { loading: search.isPlaceholderData.value }]">
          <thead>
            <tr>
              <th scope="col">Label</th>
              <th scope="col">Ident</th>
              <th scope="col">Class</th>
              <th scope="col">Matched on</th>
              <th scope="col"><span class="sr-only">Actions</span></th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="{ item, matches } in rows" :key="item.id">
              <td><RouterLink :to="`/cis/${item.id}`" dir="auto">{{ item.label }}</RouterLink> <CiStateBadge :ci="item" /></td>
              <td class="mono">{{ item.ident }}</td>
              <td>{{ item.class.name }}</td>
              <td :title="matches.map((m) => `${m.label}: ${m.value}`).join('\n')">
                <span v-for="(m, i) in matches.slice(0, 2)" :key="i">
                  <template v-if="i > 0">, </template>
                  <span class="muted"><bdi>{{ m.label }}</bdi>:</span> <span class="mono" dir="auto">{{ m.value }}</span>
                </span>
              </td>
              <td class="row-actions">
                <RouterLink v-if="!item.deletedAt" class="btn btn-sm" :to="`/cis/${item.id}/impact`" :title="`Impact analysis of ${item.label}`">Impact</RouterLink>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <PaginationBar :total="search.data.value!.page.total" :limit="limit" :offset="offset" @change="state.onPage" />
    </template>
  </section>
</template>
