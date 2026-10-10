<script setup lang="ts">
import { computed, ref } from "vue";
import { RouterLink } from "vue-router";
import { useApprovalInbox, type ApprovalInboxQuery, type ApprovalInboxView } from "../../api/approvals";
import { APPROVAL_STATUS_TONES, useWorkflowCounts, type WorkflowApprovalStatus } from "../../api/workflowRuntime";
import Breadcrumbs from "../../components/Breadcrumbs.vue";
import CiLink from "../../components/CiLink.vue";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import PaginationBar from "../../components/PaginationBar.vue";
import SortIcon from "../../components/SortIcon.vue";
import { formatNumber, t } from "../../i18n";
import { approvalStatusLabel } from "../../lib/approvalRuntime";
import { useDocumentTitle } from "../../lib/composables";
import { formatDateTime, formatRelative } from "../../lib/format";
import { useListQuery } from "../../lib/listQuery";
import ApprovalRequestDialog from "../workflows/ApprovalRequestDialog.vue";

/**
 * Approvals (GET /workflow-approval-requests): what waits for your decision (`actionable`, the earliest due
 * first, so overdue requests lead), the requests you made, and those you decided a step of. The API leaves out
 * requests on CIs of types you may not view, from the page and from its total, so the count in the navigation
 * (the same total) matches what you can open here. View, status, overdue, sort and page live in the URL.
 */
useDocumentTitle(() => t("approvals.title"));
const VIEWS: ApprovalInboxView[] = ["actionable", "requested", "decided"];
const STATUSES: WorkflowApprovalStatus[] = ["pending", "approved", "rejected", "withdrawn", "cancelled"];
const lq = useListQuery({ sort: "dueAt" });
const { get, limit, offset, update } = lq;
const view = computed<ApprovalInboxView>(() => (VIEWS as string[]).includes(get("view")) ? (get("view") as ApprovalInboxView) : "actionable");
const actionable = computed(() => view.value === "actionable");
// Your own and decided requests are mostly closed: newest first unless a sort is chosen.
const sort = computed(() => get("sort") || (actionable.value ? "dueAt" : "-requestedAt"));

const query = computed<ApprovalInboxQuery>(() => ({
  view: view.value,
  status: !actionable.value && (STATUSES as string[]).includes(get("status")) ? (get("status") as WorkflowApprovalStatus) : undefined,
  overdue: get("overdue") === "true" ? "true" : undefined,
  sort: sort.value as ApprovalInboxQuery["sort"],
  limit: limit.value,
  offset: offset.value,
}));
const list = useApprovalInbox(query);
const counts = useWorkflowCounts();
const rows = computed(() => list.data.value?.data ?? []);
const total = computed(() => list.data.value?.page.total ?? 0);
const filtered = computed(() => !!((!actionable.value && get("status")) || get("overdue")));
const opened = ref<string | null>(null);

const viewTo = (v: ApprovalInboxView) => ({ query: v === "actionable" ? {} : { view: v } });
const ariaSort = (field: string) => (sort.value === field ? "ascending" : sort.value === `-${field}` ? "descending" : "none");
const toggleSort = (field: string) => update({ sort: sort.value === field ? `-${field}` : field });
const COLUMNS = computed(() => [
  { key: "no", label: t("approvalRun.history.no") },
  { key: "ci", label: t("approvals.col.ci") },
  { key: "change", label: t("approvals.col.change") },
  { key: "step", label: t("approvals.col.step") },
  { key: "due", label: t("approvalRun.steps.due"), sort: "dueAt" },
  { key: "requested", label: t("approvalRun.dialog.requested"), sort: "requestedAt" },
  ...(actionable.value ? [] : [{ key: "status", label: t("approvalRun.steps.status"), sort: undefined }]),
]);
</script>

<template>
  <div class="list-head">
    <Breadcrumbs :items="[{ label: t('workflows.nav'), to: '/workflows' }, { label: t('approvals.title') }]" />
    <div class="page-header">
      <div class="title">
        <h1>{{ t("approvals.title") }}</h1>
        <span v-if="list.isFetching.value && !list.isPending.value" class="spinner" :aria-label="t('common.refreshing')" />
      </div>
      <div class="actions">
        <RouterLink class="btn" to="/account/delegations">{{ t("delegations.title") }}</RouterLink>
      </div>
    </div>
    <p class="page-intro">{{ t("approvals.intro") }}</p>
  </div>

  <nav class="tabs" :aria-label="t('approvals.views')">
    <RouterLink v-for="v in VIEWS" :key="v" :to="viewTo(v)" :aria-current="view === v ? 'page' : undefined" :data-testid="`approvals-view-${v}`">
      {{ t(`approvals.view.${v}`) }}
      <span v-if="v === 'actionable' && counts.data.value" class="count">{{ formatNumber(counts.data.value.awaitingMyDecision) }}</span>
    </RouterLink>
  </nav>

  <section class="panel" :aria-label="t(`approvals.view.${view}`)">
    <form class="toolbar" role="search" @submit.prevent>
      <div v-if="!actionable" class="field">
        <label for="approvals-status">{{ t("approvalRun.steps.status") }}</label>
        <select id="approvals-status" :value="get('status')" @change="update({ status: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">{{ t("approvals.anyStatus") }}</option>
          <option v-for="s in STATUSES" :key="s" :value="s">{{ approvalStatusLabel(s) }}</option>
        </select>
      </div>
      <label class="checkbox-row">
        <input type="checkbox" :checked="get('overdue') === 'true'" @change="update({ overdue: ($event.target as HTMLInputElement).checked ? 'true' : undefined })" />
        {{ t("approvals.overdueOnly") }}
      </label>
      <span v-if="list.data.value" class="muted">{{ t("approvals.total", { n: total }) }}</span>
      <button v-if="filtered" type="button" class="btn" @click="update({ status: undefined, overdue: undefined })">{{ t("approvals.clearFilters") }}</button>
    </form>

    <div v-if="list.isError.value" class="panel-body">
      <ErrorAlert :error="list.error.value" :on-retry="() => list.refetch()" />
    </div>
    <LoadingState v-else-if="list.isLoading.value" :label="t('approvals.loading')" />
    <EmptyState v-else-if="list.data.value && total === 0" :title="filtered ? t('approvals.emptyFiltered') : t(`approvals.empty.${view}`)" data-testid="approvals-empty">
      <template v-if="!filtered">{{ t(`approvals.emptyBody.${view}`) }}</template>
    </EmptyState>
    <EmptyState v-else-if="list.data.value && rows.length === 0" :title="t('approvals.pastEnd')">
      <template #actions><button type="button" class="btn" @click="update({})">{{ t("approvals.firstPage") }}</button></template>
    </EmptyState>
    <template v-else-if="rows.length > 0">
      <div class="table-wrap">
        <table :class="['data', { loading: list.isPlaceholderData.value }]" data-testid="approvals-table">
          <thead>
            <tr>
              <th v-for="c in COLUMNS" :key="c.key" scope="col" :class="{ num: c.key === 'no' }" :aria-sort="c.sort ? ariaSort(c.sort) : undefined">
                <button v-if="c.sort" type="button" class="sort" @click="toggleSort(c.sort)">{{ c.label }} <SortIcon :dir="ariaSort(c.sort)" /></button>
                <template v-else>{{ c.label }}</template>
              </th>
              <th scope="col"><span class="sr-only">{{ t("approvalRun.history.actions") }}</span></th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="r in rows" :key="r.id">
              <td class="num">{{ r.requestNo }}</td>
              <td>
                <CiLink :id="r.ciId">{{ r.ciLabel }}</CiLink> <span class="muted mono">{{ r.ciIdent }}</span>
              </td>
              <td dir="auto">
                <RouterLink :to="`/workflows/${r.instanceId}`">{{ r.definitionName }}</RouterLink>: {{ r.transitionName }}
              </td>
              <td dir="auto">
                {{ t("approvalRun.history.step", { step: r.currentStep.stepNo, steps: r.stepCount, name: r.currentStep.name, n: Number(r.currentStep.approvals), required: r.currentStep.requiredApprovals }) }}
              </td>
              <td>
                <template v-if="r.status === 'pending' && r.currentStep.dueAt">
                  <span v-if="r.currentStep.overdue" class="badge warn">{{ t("approvalRun.overdue") }}</span>
                  <span :title="formatDateTime(r.currentStep.dueAt)">{{ formatRelative(r.currentStep.dueAt) }}</span>
                </template>
                <span v-else class="muted">–</span>
              </td>
              <td :title="formatDateTime(r.requestedAt)">{{ formatRelative(r.requestedAt) }}, {{ r.requestedBy.name }}</td>
              <td v-if="!actionable">
                <span :class="['badge', APPROVAL_STATUS_TONES[r.status]]">{{ approvalStatusLabel(r.status) }}</span>
              </td>
              <td>
                <button
                  type="button"
                  :class="['btn', 'btn-sm', { 'btn-primary': actionable }]"
                  :aria-label="t(actionable ? 'approvals.decideLabel' : 'approvalRun.history.openLabel', { no: r.requestNo, ci: r.ciLabel })"
                  @click="opened = r.id"
                >
                  {{ actionable ? t("approvals.decide") : t("approvalRun.history.open") }}
                </button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <PaginationBar :total="total" :limit="limit" :offset="offset" @change="lq.onPage" />
    </template>
  </section>

  <ApprovalRequestDialog :open="!!opened" :request-id="opened" @close="opened = null" />
</template>
