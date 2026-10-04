<script setup lang="ts">
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { useCiClasses } from "../../api/queries";
import {
  STATUS_LABELS,
  STATUS_TONES,
  useWorkflowInstances,
  useWorkflowSummary,
  type WorkflowInstanceListQuery,
  type WorkflowInstanceStatus,
  type WorkflowStateCount,
} from "../../api/workflowRuntime";
import Breadcrumbs from "../../components/Breadcrumbs.vue";
import CiLink from "../../components/CiLink.vue";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import PaginationBar from "../../components/PaginationBar.vue";
import SortIcon from "../../components/SortIcon.vue";
import { useDocumentTitle } from "../../lib/composables";
import { formatDateTime, formatRelative } from "../../lib/format";
import { useListQuery } from "../../lib/listQuery";
import { viewableClasses } from "../../lib/permissions";
import { useSessionStore } from "../../stores/session";
import WorkflowStateBadge from "./WorkflowStateBadge.vue";

/**
 * Workflows: every workflow instance on the CIs the caller may view (GET /workflow-instances), with the running
 * instances counted per workflow and state above (GET /workflow-instances/summary; a count filters the list).
 * Workflow, state, status, CI type, sort and page live in the URL; the API filters and pages.
 */
useDocumentTitle("Workflows");
const session = useSessionStore();
const lq = useListQuery({ sort: "-lastTransitionAt" });
const { get, limit, offset, update } = lq;
const STATUSES: WorkflowInstanceStatus[] = ["active", "completed", "cancelled"];

const query = computed<WorkflowInstanceListQuery>(() => ({
  definitionKey: get("workflow") || undefined,
  stateKey: get("state") || undefined,
  status: (STATUSES as string[]).includes(get("status")) ? (get("status") as WorkflowInstanceStatus) : undefined,
  classKey: get("class") || undefined,
  sort: lq.sort.value as WorkflowInstanceListQuery["sort"],
  limit: limit.value,
  offset: offset.value,
}));
const list = useWorkflowInstances(query);
const summary = useWorkflowSummary(() => get("workflow") || undefined);
const classes = useCiClasses();
const classOptions = computed(() => viewableClasses(classes.data.value ?? [], (id) => session.canOnClass(id, "view")).filter((c) => !c.isAbstract));
const className = (key: string) => classes.data.value?.find((c) => c.key === key)?.name ?? key;

const rows = computed(() => list.data.value?.data ?? []);
const total = computed(() => list.data.value?.page.total ?? 0);
const filtered = computed(() => !!(get("workflow") || get("state") || get("status") || get("class")));

/** Workflow names seen in the list, for the summary and the filter (the summary carries keys only). */
const names = computed(() => {
  const m = new Map<string, string>();
  for (const r of rows.value) m.set(r.definitionKey, r.definitionName);
  return m;
});
const workflowName = (key: string) => names.value.get(key) ?? key;
/** The summary per workflow, its states in the order the API gives them. */
const groups = computed(() => {
  const out = new Map<string, WorkflowStateCount[]>();
  for (const c of summary.data.value?.data ?? []) out.set(c.definitionKey, [...(out.get(c.definitionKey) ?? []), c]);
  return [...out.entries()].map(([key, states]) => ({ key, states, total: states.reduce((n, s) => n + s.count, 0) }));
});
const workflowKeys = computed(() => {
  const keys = new Set(groups.value.map((g) => g.key));
  if (get("workflow")) keys.add(get("workflow"));
  return [...keys].sort((a, b) => workflowName(a).localeCompare(workflowName(b)));
});
const stateOptions = computed(() => {
  const m = new Map<string, string>();
  for (const g of groups.value) for (const s of g.states) if (!m.has(s.stateKey)) m.set(s.stateKey, s.stateName);
  if (get("state") && !m.has(get("state"))) m.set(get("state"), get("state"));
  return [...m.entries()];
});
const isCurrent = (key: string, state: string) => get("workflow") === key && get("state") === state && get("status") === "active";

function showState(key: string, state: string) {
  update({ workflow: key, state, status: "active" });
}
function clearFilters() {
  update({ workflow: undefined, state: undefined, status: undefined, class: undefined });
}
const COLUMNS: { key: string; label: string; sort?: string }[] = [
  { key: "ci", label: "Configuration item" },
  { key: "ident", label: "Ident" },
  { key: "class", label: "CI type" },
  { key: "workflow", label: "Workflow" },
  { key: "state", label: "State" },
  { key: "status", label: "Status" },
  { key: "started", label: "Started", sort: "startedAt" },
  { key: "last", label: "Last step", sort: "lastTransitionAt" },
];
</script>

<template>
  <Breadcrumbs :items="[{ label: 'Inventory', to: '/cis' }, { label: 'Workflows' }]" />
  <div class="page-header">
    <div class="title">
      <h1>Workflows</h1>
      <span v-if="list.data.value" class="muted">{{ total.toLocaleString() }} instances</span>
      <span v-if="list.isFetching.value && !list.isLoading.value" class="spinner" aria-label="Refreshing" />
    </div>
  </div>
  <p class="page-intro muted">Workflows running on your configuration items, and the ones that ended. Open a CI to run its next step.</p>

  <section class="panel" aria-labelledby="wf-summary-title" data-testid="wf-summary">
    <div class="panel-header">
      <h2 id="wf-summary-title">Running now</h2>
      <span class="meta">Per workflow and state; select a count to list those instances.</span>
    </div>
    <LoadingState v-if="summary.isLoading.value" label="Counting running workflows…" />
    <div v-else-if="summary.isError.value" class="panel-body">
      <ErrorAlert :error="summary.error.value" title="Could not count the running workflows" :on-retry="() => summary.refetch()" />
    </div>
    <p v-else-if="groups.length === 0" class="panel-body muted">No workflow is running on a CI you may view.</p>
    <div v-else class="table-wrap">
      <table class="data">
        <tbody>
          <tr v-for="g in groups" :key="g.key">
            <th scope="row" dir="auto">
              {{ workflowName(g.key) }} <span v-if="workflowName(g.key) !== g.key" class="muted mono">{{ g.key }}</span>
            </th>
            <td class="num">{{ g.total.toLocaleString() }}</td>
            <td style="width: 100%; white-space: normal">
              <span class="wf-summary-states">
                <button
                  v-for="s in g.states"
                  :key="s.stateKey"
                  type="button"
                  :class="['btn', 'btn-sm', { 'btn-primary': isCurrent(g.key, s.stateKey) }]"
                  :aria-pressed="isCurrent(g.key, s.stateKey)"
                  :title="`List the running instances of ${workflowName(g.key)} in ${s.stateName}`"
                  @click="showState(g.key, s.stateKey)"
                >
                  <WorkflowStateBadge :state="{ name: s.stateName, category: s.category }" />
                  {{ s.count.toLocaleString() }}
                </button>
              </span>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>

  <section class="panel" aria-label="Workflow instances">
    <form class="toolbar" role="search" @submit.prevent>
      <div class="field">
        <label for="wfi-workflow">Workflow</label>
        <select id="wfi-workflow" :value="get('workflow')" @change="update({ workflow: ($event.target as HTMLSelectElement).value || undefined, state: undefined })">
          <option value="">All workflows</option>
          <option v-for="k in workflowKeys" :key="k" :value="k">{{ workflowName(k) }}</option>
        </select>
      </div>
      <div class="field">
        <label for="wfi-state">State</label>
        <select id="wfi-state" :value="get('state')" @change="update({ state: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">All states</option>
          <option v-for="[k, name] in stateOptions" :key="k" :value="k">{{ name }}</option>
        </select>
      </div>
      <div class="field">
        <label for="wfi-status">Status</label>
        <select id="wfi-status" :value="get('status')" @change="update({ status: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">Any status</option>
          <option v-for="s in STATUSES" :key="s" :value="s">{{ STATUS_LABELS[s] }}</option>
        </select>
      </div>
      <div class="field">
        <label for="wfi-class">CI type</label>
        <select id="wfi-class" :value="get('class')" @change="update({ class: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">All types</option>
          <option v-for="c in classOptions" :key="c.id" :value="c.key">{{ c.name }}</option>
        </select>
      </div>
      <button v-if="filtered" type="button" class="btn" @click="clearFilters">Clear filters</button>
    </form>

    <div v-if="list.isError.value" class="panel-body">
      <ErrorAlert :error="list.error.value" :on-retry="() => list.refetch()" />
    </div>
    <LoadingState v-if="list.isLoading.value" label="Loading workflow instances…" />
    <EmptyState v-if="list.data.value && total === 0 && filtered" title="No workflow instance matches these filters">
      <template #actions><button type="button" class="btn" @click="clearFilters">Clear filters</button></template>
    </EmptyState>
    <EmptyState v-else-if="list.data.value && total === 0" title="No workflows have run yet" data-testid="wf-instances-empty">
      A workflow is started from a CI's Workflows tab, once an administrator has published one for its type.
      <template v-if="session.can('workflows.manage')" #actions><RouterLink class="btn" to="/admin/workflows">Manage workflows</RouterLink></template>
    </EmptyState>
    <EmptyState v-if="list.data.value && total > 0 && rows.length === 0" title="This page is past the end of the list">
      <template #actions><button type="button" class="btn" @click="update({})">First page</button></template>
    </EmptyState>

    <template v-if="rows.length > 0">
      <div class="table-wrap">
        <table :class="['data', { loading: list.isPlaceholderData.value }]">
          <thead>
            <tr>
              <th v-for="c in COLUMNS" :key="c.key" scope="col" :aria-sort="c.sort ? lq.ariaSort(c.sort) : undefined">
                <button v-if="c.sort" type="button" class="sort" @click="lq.toggleSort(c.sort)">{{ c.label }} <SortIcon :dir="lq.ariaSort(c.sort)" /></button>
                <template v-else>{{ c.label }}</template>
              </th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="i in rows" :key="i.id">
              <td><CiLink :id="i.ciId">{{ i.ciLabel }}</CiLink></td>
              <td class="mono muted">{{ i.ciIdent }}</td>
              <td>{{ className(i.classKey) }}</td>
              <td>
                <RouterLink :to="`/workflows/${i.id}`" dir="auto">{{ i.definitionName }}</RouterLink>
                <span class="muted"> v{{ i.versionNo }}</span>
              </td>
              <td><WorkflowStateBadge :state="i.state" /></td>
              <td><span :class="['badge', STATUS_TONES[i.status]]">{{ STATUS_LABELS[i.status] }}</span></td>
              <td :title="`${formatDateTime(i.startedAt)} by ${i.startedByName}`">{{ formatRelative(i.startedAt) }}</td>
              <td :title="formatDateTime(i.lastTransitionAt)">{{ formatRelative(i.lastTransitionAt) }}</td>
            </tr>
          </tbody>
        </table>
      </div>
      <PaginationBar :total="total" :limit="limit" :offset="offset" @change="lq.onPage" />
    </template>
  </section>
</template>
