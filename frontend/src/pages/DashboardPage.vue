<script setup lang="ts">
import { useQueries, useQuery } from "@tanstack/vue-query";
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { useLookupLists, useLookupListValues } from "../api/datamodel";
import { ciCountQuery, useCiClasses, useCiList } from "../api/queries";
import { dataModelEmpty } from "../lib/dataModel";
import Breadcrumbs from "../components/Breadcrumbs.vue";
import DataModelEmpty from "../components/DataModelEmpty.vue";
import EmptyState from "../components/EmptyState.vue";
import ErrorAlert from "../components/ErrorAlert.vue";
import LoadingState from "../components/LoadingState.vue";
import CiStateBadge from "../components/CiStateBadge.vue";
import { useAppSettings } from "../lib/appSettings";
import { useDocumentTitle } from "../lib/composables";
import { formatRelative } from "../lib/format";
import { useSessionStore } from "../stores/session";
import { useBrandingStore } from "../stores/branding";
import CountTable, { type CountRow } from "./dashboard/CountTable.vue";
import DashboardWidgets from "./dashboard/DashboardWidgets.vue";

/**
 * Operational overview. Counts are server-side (each is a limit=1 list request
 * reading page.total), so they stay correct at any inventory size.
 * Customization › Dashboard can replace the built-in panels with its own widgets.
 */
useDocumentTitle("Dashboard");
const session = useSessionStore();
const branding = useBrandingStore();
const settings = useAppSettings();
const widgets = computed(() => settings.doc.value.dashboard.widgets ?? null);
const total = useQuery(ciCountQuery({}));
const recent = useCiList({ sort: "-updatedAt", limit: 12 });

const classes = useCiClasses();
/** A fresh install: no classes but the built-in ones yet, so the first step is the data model, not a CI. */
const noClasses = computed(() => !!classes.data.value && dataModelEmpty(classes.data.value));
// Only classes the user may view: the API leaves the others out of every count, which would read as 0.
const concrete = computed(() => (classes.data.value ?? []).filter((c) => !c.isAbstract && session.canOnClass(c.id, "view")));
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

// "By status" counts the values of the lookup list with key "status" (the starter templates' status); without one it is left out.
const lookupLists = useLookupLists();
const statusListId = computed(() => lookupLists.data.value?.find((l) => l.key === "status")?.id);
const statuses = useLookupListValues(statusListId);
const statusList = computed(() => (statusListId.value ? (statuses.data.value ?? []) : []));
const statusCounts = useQueries({ queries: computed(() => statusList.value.map((s) => ciCountQuery({ lookupValueId: s.id }))) });
const statusRows = computed<CountRow[]>(() =>
  statusList.value.map((s, i) => ({ id: s.id, label: s.name, count: statusCounts.value[i]?.data, to: `/cis?lookupValueId=${s.id}` })),
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
        <RouterLink v-if="session.canOnAnyClass('create') && !noClasses" class="btn btn-primary" to="/cis/new">+ New CI</RouterLink>
      </div>
    </div>

    <LoadingState v-if="total.isLoading.value" />
    <section v-if="total.data.value === 0 && noClasses" class="panel callout">
      <DataModelEmpty />
    </section>
    <section v-else-if="total.data.value === 0 && classes.data.value" class="panel">
      <EmptyState :title="`Welcome to ${branding.effective.appName} — the inventory is empty`">
        Start with the things everything else depends on: a location, then the servers in it, then the applications and
        databases that run on them. Relate them from each CI's detail page.
        <template v-if="session.canOnAnyClass('create')" #actions>
          <RouterLink class="btn btn-primary" to="/cis/new">+ Create your first configuration item</RouterLink>
        </template>
      </EmptyState>
    </section>
    <template v-if="total.data.value !== undefined && total.data.value > 0 && widgets">
      <DashboardWidgets v-if="widgets.length > 0" :widgets="widgets" />
      <EmptyState v-else title="This dashboard has no widgets">
        An administrator removed every widget under Administration › Customization › Dashboard.
        <template v-if="session.can('customization.manage')" #actions>
          <RouterLink class="btn" to="/admin/customization/dashboard">Add widgets</RouterLink>
        </template>
      </EmptyState>
    </template>
    <LoadingState v-else-if="settings.query.isLoading.value" />
    <template v-else-if="total.data.value !== undefined && total.data.value > 0">
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
          v-if="statusListId"
          title="By status"
          :rows="statusRows"
          :total="total.data.value"
          :loading="lookupLists.isLoading.value || statuses.isLoading.value"
          :error="lookupLists.error.value ?? statuses.error.value ?? statusCounts.find((c) => c.error)?.error"
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
                <th scope="col">Label</th>
                <th scope="col">Ident</th>
                <th scope="col">Class</th>
                <th scope="col">Changed</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="ci in recent.data.value.data" :key="ci.id">
                <td><RouterLink :to="`/cis/${ci.id}`">{{ ci.label }}</RouterLink> <CiStateBadge :ci="ci" /></td>
                <td class="mono">{{ ci.ident }}</td>
                <td>{{ ci.class.name }}</td>
                <td :title="ci.updatedAt">{{ formatRelative(ci.updatedAt) }}</td>
              </tr>
            </tbody>
          </table>
        </div>
      </section>
    </template>
  </template>
</template>
