<script setup lang="ts">
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { useCiClasses } from "../../api/queries";
import {
  STATUS_LABELS,
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
import SkeletonRows from "../../components/SkeletonRows.vue";
import SortIcon from "../../components/SortIcon.vue";
import { formatNumber, t, type MessageKey } from "../../i18n";
import { useDocumentTitle } from "../../lib/composables";
import { formatDateTime, formatRelative } from "../../lib/format";
import { useListQuery } from "../../lib/listQuery";
import { viewableClasses } from "../../lib/permissions";
import { useSessionStore } from "../../stores/session";
import WorkflowStateBadge from "./WorkflowStateBadge.vue";
import WorkflowStatusBadge from "./WorkflowStatusBadge.vue";

/**
 * Workflows: every workflow instance on the CIs the caller may view (GET /workflow-instances), with the running
 * instances counted per workflow and state above (GET /workflow-instances/summary; a count filters the list).
 * Workflow, state, status, CI type, sort and page live in the URL; the API filters and pages. The page uses the
 * inventory's head band (breadcrumb, title with the count, intro, filters) above the summary and the table card.
 */
useDocumentTitle(() => t("wfRun.list.title"));
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
const COLUMNS: { key: string; label: MessageKey; sort?: string; cls?: string }[] = [
  { key: "ci", label: "wfRun.col.ci" },
  { key: "ident", label: "wfRun.col.ident" },
  { key: "class", label: "wfRun.col.class" },
  { key: "workflow", label: "wfRun.col.workflow" },
  { key: "state", label: "wfRun.col.state" },
  { key: "status", label: "wfRun.col.status" },
  { key: "started", label: "wfRun.col.started", sort: "startedAt" },
  { key: "last", label: "wfRun.col.lastStep", sort: "lastTransitionAt" },
];
const crumbs = computed(() => [{ label: t("inventory.crumb"), to: "/cis" }, { label: t("wfRun.list.title") }]);
</script>

<template>
  <div class="list-head">
    <Breadcrumbs :items="crumbs" />
    <div class="page-header">
      <div class="title">
        <h1>{{ t("wfRun.list.title") }}</h1>
        <span v-if="list.data.value" class="count mono">{{ t("wfRun.list.count", { n: total }) }}</span>
        <span v-if="list.isFetching.value && !list.isLoading.value" class="spinner" :aria-label="t('common.refreshing')" />
      </div>
    </div>
    <p class="page-intro">{{ t("wfRun.list.intro") }}</p>
    <form class="toolbar" role="search" :aria-label="t('wfRun.list.filters')" @submit.prevent>
      <div class="field">
        <label for="wfi-workflow">{{ t("wfRun.col.workflow") }}</label>
        <select id="wfi-workflow" :value="get('workflow')" @change="update({ workflow: ($event.target as HTMLSelectElement).value || undefined, state: undefined })">
          <option value="">{{ t("wfRun.filter.allWorkflows") }}</option>
          <option v-for="k in workflowKeys" :key="k" :value="k" dir="auto">{{ workflowName(k) }}</option>
        </select>
      </div>
      <div class="field">
        <label for="wfi-state">{{ t("wfRun.col.state") }}</label>
        <select id="wfi-state" :value="get('state')" @change="update({ state: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">{{ t("wfRun.filter.allStates") }}</option>
          <option v-for="[k, name] in stateOptions" :key="k" :value="k" dir="auto">{{ name }}</option>
        </select>
      </div>
      <div class="field">
        <label for="wfi-status">{{ t("wfRun.col.status") }}</label>
        <select id="wfi-status" :value="get('status')" @change="update({ status: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">{{ t("wfRun.filter.anyStatus") }}</option>
          <option v-for="s in STATUSES" :key="s" :value="s">{{ t(STATUS_LABELS[s]) }}</option>
        </select>
      </div>
      <div class="field">
        <label for="wfi-class">{{ t("wfRun.col.class") }}</label>
        <select id="wfi-class" :value="get('class')" @change="update({ class: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">{{ t("wfRun.filter.allTypes") }}</option>
          <option v-for="c in classOptions" :key="c.id" :value="c.key" dir="auto">{{ c.name }}</option>
        </select>
      </div>
      <button v-if="filtered" type="button" class="btn" @click="clearFilters">{{ t("inventory.clearFilters") }}</button>
    </form>
  </div>

  <section class="panel" aria-labelledby="wf-summary-title" data-testid="wf-summary">
    <div class="panel-header">
      <h2 id="wf-summary-title">{{ t("wfRun.summary.title") }}</h2>
      <span class="meta">{{ t("wfRun.summary.meta") }}</span>
    </div>
    <LoadingState v-if="summary.isLoading.value" :label="t('wfRun.summary.loading')" />
    <div v-else-if="summary.isError.value" class="panel-body">
      <ErrorAlert :error="summary.error.value" :title="t('wfRun.summary.failed')" :on-retry="() => summary.refetch()" />
    </div>
    <EmptyState v-else-if="groups.length === 0" icon="circle-check" :title="t('wfRun.summary.none')" />
    <div v-else class="table-wrap">
      <table class="data wf-summary-table">
        <tbody>
          <tr v-for="g in groups" :key="g.key">
            <th scope="row" dir="auto">
              {{ workflowName(g.key) }} <span v-if="workflowName(g.key) !== g.key" class="muted mono">{{ g.key }}</span>
            </th>
            <td class="num mono">{{ formatNumber(g.total) }}</td>
            <td class="wf-summary-cell">
              <span class="wf-summary-states">
                <button
                  v-for="s in g.states"
                  :key="s.stateKey"
                  type="button"
                  :class="['btn', 'btn-sm', { 'btn-primary': isCurrent(g.key, s.stateKey) }]"
                  :aria-pressed="isCurrent(g.key, s.stateKey)"
                  :title="t('wfRun.summary.countTitle', { workflow: workflowName(g.key), state: s.stateName })"
                  @click="showState(g.key, s.stateKey)"
                >
                  <WorkflowStateBadge :state="{ name: s.stateName, category: s.category }" />
                  <span class="mono">{{ formatNumber(s.count) }}</span>
                </button>
              </span>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>

  <section class="panel explorer" :aria-label="t('wfRun.list.region')">
    <div v-if="list.isError.value" class="panel-body">
      <ErrorAlert :error="list.error.value" :on-retry="() => list.refetch()" />
    </div>
    <SkeletonRows v-else-if="list.isLoading.value" :label="t('wfRun.list.loading')" />
    <EmptyState v-else-if="list.data.value && total === 0 && filtered" icon="search" :title="t('wfRun.list.noMatch')">
      <template #actions><button type="button" class="btn" @click="clearFilters">{{ t("inventory.clearFilters") }}</button></template>
    </EmptyState>
    <EmptyState v-else-if="list.data.value && total === 0" icon="circle-check" :title="t('wfRun.list.empty.title')" data-testid="wf-instances-empty">
      {{ t("wfRun.list.empty.body") }}
      <template v-if="session.can('workflows.manage')" #actions>
        <RouterLink class="btn btn-primary" to="/admin/workflows">{{ t("wfRun.list.empty.manage") }}</RouterLink>
      </template>
    </EmptyState>
    <EmptyState v-else-if="list.data.value && rows.length === 0" :title="t('common.pastEnd')">
      <template #actions><button type="button" class="btn" @click="update({})">{{ t("common.firstPage") }}</button></template>
    </EmptyState>

    <template v-if="rows.length > 0 && !list.isError.value">
      <div class="table-wrap table-scroll">
        <table :class="['data', 'list-table', { loading: list.isPlaceholderData.value }]">
          <caption class="sr-only">{{ t("wfRun.list.caption") }}</caption>
          <thead>
            <tr>
              <th v-for="c in COLUMNS" :key="c.key" scope="col" :aria-sort="c.sort ? lq.ariaSort(c.sort) : undefined">
                <button v-if="c.sort" type="button" class="sort" @click="lq.toggleSort(c.sort)">{{ t(c.label) }} <SortIcon :dir="lq.ariaSort(c.sort)" /></button>
                <template v-else>{{ t(c.label) }}</template>
              </th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="i in rows" :key="i.id">
              <td>
                <span class="cell-clip"><CiLink :id="i.ciId" class="list-name">{{ i.ciLabel }}</CiLink></span>
              </td>
              <td class="mono muted">{{ i.ciIdent }}</td>
              <td dir="auto">{{ className(i.classKey) }}</td>
              <td>
                <RouterLink :to="`/workflows/${i.id}`" dir="auto">{{ i.definitionName }}</RouterLink>
                <span class="muted mono"> {{ t("wfRun.instance.versionChip", { n: i.versionNo }) }}</span>
              </td>
              <td><WorkflowStateBadge :state="i.state" /></td>
              <td><WorkflowStatusBadge :status="i.status" /></td>
              <td>
                <time :datetime="i.startedAt" :title="t('wfRun.startedTitle', { when: formatDateTime(i.startedAt), name: i.startedByName })">{{
                  formatRelative(i.startedAt)
                }}</time>
              </td>
              <td>
                <time :datetime="i.lastTransitionAt" :title="formatDateTime(i.lastTransitionAt)">{{ formatRelative(i.lastTransitionAt) }}</time>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <div class="table-footer">
        <PaginationBar numbered :total="total" :limit="limit" :offset="offset" @change="lq.onPage" />
      </div>
    </template>
  </section>
</template>
