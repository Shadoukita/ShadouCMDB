<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { RouterLink } from "vue-router";
import { useCiClasses } from "../../../api/queries";
import { useWorkflowList, type WorkflowListQuery } from "../../../api/workflows";
import Breadcrumbs from "../../../components/Breadcrumbs.vue";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import PaginationBar from "../../../components/PaginationBar.vue";
import { useDebounced, useDocumentTitle } from "../../../lib/composables";
import { formatRelative } from "../../../lib/format";
import { useListQuery } from "../../../lib/listQuery";
import { useFlashStore } from "../../../stores/flash";

/**
 * Administration › Workflows: every workflow definition, filtered by the CI type it runs on.
 * Search, type, active flag, sort and page live in the URL; the API filters and pages.
 */
useDocumentTitle("Workflows");
type SortField = NonNullable<WorkflowListQuery["sort"]>;

const COLUMNS: { key: string; label: string; sort?: string; num?: boolean }[] = [
  { key: "name", label: "Name", sort: "name" },
  { key: "key", label: "Key", sort: "key" },
  { key: "class", label: "CI type" },
  { key: "stateField", label: "State field" },
  { key: "status", label: "Status" },
  { key: "version", label: "Current version", num: true },
  { key: "draft", label: "Draft" },
  { key: "updated", label: "Updated", sort: "updatedAt" },
];

const lq = useListQuery({ sort: "name" });
const { get, limit, offset, update } = lq;
const query = computed<WorkflowListQuery>(() => ({
  q: get("q") || undefined,
  classKey: get("class") || undefined,
  active: (get("active") || undefined) as WorkflowListQuery["active"],
  sort: lq.sort.value as SortField,
  limit: limit.value,
  offset: offset.value,
}));
const list = useWorkflowList(query);
const classes = useCiClasses();
const classByKey = computed(() => new Map((classes.data.value ?? []).map((c) => [c.key, c])));
const flash = useFlashStore();
const flashText = computed(() => flash.forCi("workflows"));

const qText = ref(get("q"));
const debouncedQ = useDebounced(qText, 300);
watch(debouncedQ, (v) => v !== get("q") && update({ q: v || undefined }));
watch(
  () => get("q"),
  (v) => (qText.value = v),
);

const filtered = computed(() => !!(get("q") || get("class") || get("active")));
const total = computed(() => list.data.value?.page.total ?? 0);
const rows = computed(() => list.data.value?.data ?? []);
const newHref = computed(() => {
  const c = classByKey.value.get(get("class"));
  return c ? `/admin/workflows/new?classId=${c.id}` : "/admin/workflows/new";
});

function clearFilters() {
  qText.value = "";
  update({ q: undefined, class: undefined, active: undefined });
}
</script>

<template>
  <Breadcrumbs :items="[{ label: 'Administration', to: '/admin' }, { label: 'Workflows' }]" />
  <div class="page-header">
    <div class="title">
      <h1>Workflows</h1>
      <span v-if="list.data.value" class="muted">{{ total.toLocaleString() }} total</span>
      <span v-if="list.isFetching.value && !list.isLoading.value" class="spinner" aria-label="Refreshing" />
    </div>
    <div class="actions">
      <RouterLink class="btn btn-primary" :to="newHref">+ New workflow</RouterLink>
    </div>
  </div>
  <p class="page-intro muted">
    A workflow moves the CIs of one type through states by named transitions. Edit a draft, check it, publish it as a version,
    then decide which permission profiles may run each transition.
  </p>
  <div v-if="flashText" class="alert" role="status">{{ flashText }}</div>

  <section class="panel" aria-label="Workflows">
    <form class="toolbar" role="search" @submit.prevent>
      <div class="field search">
        <label for="wf-q">Search</label>
        <input id="wf-q" v-model="qText" type="search" placeholder="Name, key or description" />
      </div>
      <div class="field">
        <label for="wf-class">CI type</label>
        <select id="wf-class" :value="get('class')" @change="update({ class: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">All types</option>
          <option v-for="c in classes.data.value ?? []" :key="c.id" :value="c.key">{{ c.name }}</option>
        </select>
      </div>
      <div class="field">
        <label for="wf-active">Status</label>
        <select id="wf-active" :value="get('active')" @change="update({ active: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">Active and inactive</option>
          <option value="true">Active</option>
          <option value="false">Inactive</option>
        </select>
      </div>
      <button v-if="filtered" type="button" class="btn" @click="clearFilters">Clear filters</button>
    </form>

    <div v-if="list.isError.value" class="panel-body">
      <ErrorAlert :error="list.error.value" :on-retry="() => list.refetch()" />
    </div>
    <LoadingState v-if="list.isLoading.value" label="Loading workflows…" />
    <EmptyState v-if="list.data.value && total === 0 && filtered" title="No workflow matches these filters">
      <template #actions><button type="button" class="btn" @click="clearFilters">Clear filters</button></template>
    </EmptyState>
    <EmptyState v-else-if="list.data.value && total === 0" title="No workflows yet" data-testid="workflows-empty">
      Create a workflow for a CI type, for example a change or decommissioning process, and design its states and transitions.
      <template #actions><RouterLink class="btn btn-primary" :to="newHref">New workflow</RouterLink></template>
    </EmptyState>
    <EmptyState v-if="list.data.value && total > 0 && rows.length === 0" title="This page is past the end of the list">
      <template #actions><button type="button" class="btn" @click="update({})">First page</button></template>
    </EmptyState>

    <template v-if="rows.length > 0">
      <div class="table-wrap">
        <table :class="['data', { loading: list.isPlaceholderData.value }]">
          <thead>
            <tr>
              <th v-for="c in COLUMNS" :key="c.key" scope="col" :class="{ num: c.num }" :aria-sort="c.sort ? lq.ariaSort(c.sort) : undefined">
                <button v-if="c.sort" type="button" class="sort" @click="lq.toggleSort(c.sort)">{{ c.label }} {{ lq.sortIndicator(c.sort) }}</button>
                <template v-else>{{ c.label }}</template>
              </th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="w in rows" :key="w.id" :class="{ disabled: !w.isActive }">
              <td><RouterLink :to="`/admin/workflows/${w.id}`">{{ w.name }}</RouterLink></td>
              <td class="mono">{{ w.key }}</td>
              <td>
                {{ classByKey.get(w.classKey)?.name ?? w.classKey }}
                <span v-if="w.includeSubclasses" class="muted"> and subtypes</span>
              </td>
              <td :class="{ muted: !w.stateAttributeKey }">{{ w.stateAttributeKey ?? "None" }}</td>
              <td>
                <span v-if="w.isActive" class="badge ok">Active</span>
                <span v-else class="badge off">Inactive</span>
              </td>
              <td class="num">{{ w.currentVersionNo ?? "–" }}</td>
              <td>
                <span v-if="w.draftVersionNo !== null" class="badge info">v{{ w.draftVersionNo }} draft</span>
                <span v-else class="muted">None</span>
              </td>
              <td :title="`${w.updatedAt} by ${w.updatedByName}`">{{ formatRelative(w.updatedAt) }}</td>
            </tr>
          </tbody>
        </table>
      </div>
      <PaginationBar :total="total" :limit="limit" :offset="offset" @change="lq.onPage" />
    </template>
  </section>
</template>
