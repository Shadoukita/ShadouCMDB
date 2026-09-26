<script setup lang="ts">
import { useQueries, useQuery } from "@tanstack/vue-query";
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { ciCountQuery, useCiClasses, useCiList, useLookup } from "../api/queries";
import Breadcrumbs from "../components/Breadcrumbs.vue";
import EmptyState from "../components/EmptyState.vue";
import ErrorAlert from "../components/ErrorAlert.vue";
import LoadingState from "../components/LoadingState.vue";
import StatusBadge from "../components/StatusBadge.vue";
import { useDocumentTitle } from "../lib/composables";
import { formatRelative } from "../lib/format";
import { useSessionStore } from "../stores/session";
import CountTable, { type CountRow } from "./dashboard/CountTable.vue";

/**
 * Operational overview. Counts are server-side (each is a limit=1 list request
 * reading page.total), so they stay correct at any inventory size.
 */
useDocumentTitle("Dashboard");
const session = useSessionStore();
const total = useQuery(ciCountQuery({}));
const recent = useCiList({ sort: "-updatedAt", limit: 12 });

const classes = useCiClasses();
const concrete = computed(() => (classes.data.value ?? []).filter((c) => !c.isAbstract));
const classCounts = useQueries({ queries: computed(() => concrete.value.map((c) => ciCountQuery({ classId: c.id }))) });
const classRows = computed<CountRow[]>(() =>
  concrete.value.map((c, i) => ({
    id: c.id,
    label: c.name,
    count: classCounts.value[i]?.data,
    to: `/cis?classId=${c.id}`,
    ...(c.isActive && session.canOnClass(c.id, "create") ? { newTo: `/cis/new?classId=${c.id}`, newLabel: `New ${c.name}` } : {}),
  })),
);

const statuses = useLookup("statuses");
const statusList = computed(() => statuses.data.value ?? []);
const statusCounts = useQueries({ queries: computed(() => statusList.value.map((s) => ciCountQuery({ statusId: s.id }))) });
const statusRows = computed<CountRow[]>(() =>
  statusList.value.map((s, i) => ({ id: s.id, label: s.name, count: statusCounts.value[i]?.data, to: `/cis?statusId=${s.id}` })),
);
</script>

<template>
  <Breadcrumbs :items="[]" />
  <template v-if="total.isError.value">
    <div class="page-header"><h1>Dashboard</h1></div>
    <ErrorAlert :error="total.error.value" :on-retry="() => total.refetch()" />
  </template>
  <template v-else>
    <div class="page-header">
      <div class="title"><h1>Dashboard</h1></div>
      <div class="actions">
        <RouterLink class="btn" to="/cis">Open inventory</RouterLink>
        <RouterLink v-if="session.canOnAnyClass('create')" class="btn btn-primary" to="/cis/new">+ New CI</RouterLink>
      </div>
    </div>

    <LoadingState v-if="total.isLoading.value" />
    <section v-if="total.data.value === 0" class="panel">
      <EmptyState title="Welcome to ShadouCMDB — the inventory is empty">
        Start with the things everything else depends on: a location, then the servers in it, then the applications and
        databases that run on them. Relate them from each CI's detail page.
        <template v-if="session.canOnAnyClass('create')" #actions>
          <RouterLink class="btn btn-primary" to="/cis/new">+ Create your first configuration item</RouterLink>
        </template>
      </EmptyState>
    </section>
    <template v-if="total.data.value !== undefined && total.data.value > 0">
      <div class="kpis">
        <div class="kpi">
          <div class="value">{{ total.data.value.toLocaleString() }}</div>
          <div class="label">Configuration items</div>
        </div>
      </div>
      <div class="grid-2">
        <CountTable
          title="By class"
          :rows="classRows"
          :total="total.data.value"
          :loading="classes.isLoading.value"
          :error="classes.error.value ?? classCounts.find((c) => c.error)?.error"
        />
        <CountTable
          title="By status"
          :rows="statusRows"
          :total="total.data.value"
          :loading="statuses.isLoading.value"
          :error="statuses.error.value ?? statusCounts.find((c) => c.error)?.error"
        />
      </div>
      <div style="height: var(--sp-4)" />
      <section class="panel">
        <div class="panel-header">
          <h2>Recently changed</h2>
          <RouterLink to="/cis?sort=-updatedAt">View all</RouterLink>
        </div>
        <div class="panel-body flush">
          <LoadingState v-if="recent.isLoading.value" />
          <div v-if="recent.isError.value" class="panel-body">
            <ErrorAlert :error="recent.error.value" :on-retry="() => recent.refetch()" />
          </div>
          <table v-if="recent.data.value" class="data">
            <thead>
              <tr>
                <th scope="col">Name</th>
                <th scope="col">Class</th>
                <th scope="col">Status</th>
                <th scope="col">Environment</th>
                <th scope="col">Owner</th>
                <th scope="col">Changed</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="ci in recent.data.value.data" :key="ci.id">
                <td><RouterLink :to="`/cis/${ci.id}`">{{ ci.name }}</RouterLink></td>
                <td>{{ ci.class.name }}</td>
                <td><StatusBadge :status="ci.status" /></td>
                <td>{{ ci.environment?.name ?? "" }}</td>
                <td>{{ ci.owner?.name ?? "" }}</td>
                <td :title="ci.updatedAt">{{ formatRelative(ci.updatedAt) }}</td>
              </tr>
            </tbody>
          </table>
        </div>
      </section>
    </template>
  </template>
</template>
