<script setup lang="ts">
import { computed } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { useSearch } from "../api/queries";
import Breadcrumbs from "../components/Breadcrumbs.vue";
import EmptyState from "../components/EmptyState.vue";
import ErrorAlert from "../components/ErrorAlert.vue";
import LoadingState from "../components/LoadingState.vue";
import PaginationBar from "../components/PaginationBar.vue";
import StatusBadge from "../components/StatusBadge.vue";
import { useDocumentTitle } from "../lib/composables";
import { plural } from "../lib/format";

/** Full global-search results (ranked by the API), with the field that matched. State lives in the URL. */
const route = useRoute();
const router = useRouter();
const q = computed(() => (typeof route.query.q === "string" ? route.query.q : ""));
const limit = computed(() => Number(route.query.limit) || 50);
const offset = computed(() => Number(route.query.offset) || 0);
const search = useSearch(q, limit, offset);
useDocumentTitle(() => (q.value ? `Search: ${q.value}` : "Search"));
const rows = computed(() => search.data.value?.data ?? []);

function onPage(p: { limit: number; offset: number }) {
  router.push({
    path: "/search",
    query: { q: q.value, ...(p.limit !== 50 ? { limit: String(p.limit) } : {}), ...(p.offset ? { offset: String(p.offset) } : {}) },
  });
}
</script>

<template>
  <Breadcrumbs :items="[{ label: 'Search' }]" />
  <div class="page-header">
    <div class="title">
      <h1>{{ q ? `Results for “${q}”` : "Search" }}</h1>
      <span v-if="search.data.value" class="muted">{{ plural(search.data.value.page.total, "match", "matches") }}</span>
    </div>
    <RouterLink v-if="q" class="btn" :to="{ path: '/cis', query: { q } }">Open as filterable inventory</RouterLink>
  </div>
  <section class="panel">
    <EmptyState v-if="!q" title="Type in the search box above">
      Search covers names, hostnames, IPs and networks, serial numbers, notes and attribute values.
    </EmptyState>
    <LoadingState v-if="search.isLoading.value" label="Searching…" />
    <div v-if="search.isError.value" class="panel-body">
      <ErrorAlert :error="search.error.value" :on-retry="() => search.refetch()" />
    </div>
    <EmptyState v-if="search.data.value && rows.length === 0" :title="`No configuration item matches “${q}”`">
      Try a shorter term, an IP address, or a CIDR like 10.0.0.0/24.
    </EmptyState>
    <template v-if="rows.length > 0">
      <div class="table-wrap">
        <table :class="['data', { loading: search.isPlaceholderData.value }]">
          <thead>
            <tr>
              <th scope="col">Name</th>
              <th scope="col">Class</th>
              <th scope="col">Status</th>
              <th scope="col">Matched on</th>
              <th scope="col">Hostname</th>
              <th scope="col">IP address</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="{ item, matches } in rows" :key="item.id">
              <td><RouterLink :to="`/cis/${item.id}`">{{ item.name }}</RouterLink></td>
              <td>{{ item.class.name }}</td>
              <td><StatusBadge :status="item.status" /></td>
              <td :title="matches.map((m) => `${m.label}: ${m.value}`).join('\n')">
                <span v-for="(m, i) in matches.slice(0, 2)" :key="i">
                  <template v-if="i > 0">, </template>
                  <span class="muted">{{ m.label }}:</span> <span class="mono">{{ m.value }}</span>
                </span>
              </td>
              <td class="mono">{{ item.hostname ?? "" }}</td>
              <td class="mono">{{ item.ipAddress ?? "" }}</td>
            </tr>
          </tbody>
        </table>
      </div>
      <PaginationBar :total="search.data.value!.page.total" :limit="limit" :offset="offset" @change="onPage" />
    </template>
  </section>
</template>
