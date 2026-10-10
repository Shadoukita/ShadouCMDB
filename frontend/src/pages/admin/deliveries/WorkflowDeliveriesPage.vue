<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { RouterLink, useRouter } from "vue-router";
import {
  useActionDeliveries,
  useActionsSummary,
  useBulkDeliveryAction,
  type ActionDelivery,
  type ActionDeliveryBulkResult,
  type ActionDeliveryFilter,
  type ActionDeliveryListQuery,
  type ActionDeliveryStatus,
} from "../../../api/actionDeliveries";
import { useWorkflowList } from "../../../api/workflows";
import Breadcrumbs from "../../../components/Breadcrumbs.vue";
import ConfirmDialog from "../../../components/ConfirmDialog.vue";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import PaginationBar from "../../../components/PaginationBar.vue";
import SkeletonRows from "../../../components/SkeletonRows.vue";
import SortIcon from "../../../components/SortIcon.vue";
import { formatNumber, t, type MessageKey } from "../../../i18n";
import { useDocumentTitle } from "../../../lib/composables";
import { formatDateTime, formatRelative } from "../../../lib/format";
import { useListQuery } from "../../../lib/listQuery";
import { DELIVERY_STATUSES, deliveryStatusLabel, reasonText, statusBadge } from "../../../lib/outbound";
import { KINDS, type ActionKind } from "../../../lib/workflowActions";
import { useFlashStore } from "../../../stores/flash";
import { adminCrumbs } from "../sections";
import DeliveriesSummaryBanner from "./DeliveriesSummaryBanner.vue";
import DeliveryDetailDialog from "./DeliveryDetailDialog.vue";
import DeliveryRecipient from "./DeliveryRecipient.vue";
import { canDiscard, canRetry } from "./deliveryRules";

/**
 * Administration › Workflow deliveries (SHAA-2725 §4.4, §11.2): one workflow's action deliveries, newest first,
 * with the outbox's state on top. The workflow, filters, sort, page and the open delivery live in the URL, so
 * a view can be bookmarked and shared. Dead and held deliveries can be retried, waiting ones discarded, by
 * selection or, for a whole filter, up to 1,000 per request; each change is one audit row.
 */
useDocumentTitle(() => t("admin.section.deliveries"));
const router = useRouter();
const flash = useFlashStore();
const lq = useListQuery({ sort: "-createdAt" });
const { get, limit, offset, update } = lq;

const workflows = useWorkflowList({ sort: "name", limit: 200 });
const workflowRows = computed(() => workflows.data.value?.data ?? []);
const workflowId = computed(() => get("workflow") || workflowRows.value[0]?.id || "");
const workflow = computed(() => workflowRows.value.find((w) => w.id === workflowId.value));

type SortField = NonNullable<ActionDeliveryListQuery["sort"]>;
const status = computed(() => DELIVERY_STATUSES.find((s) => s === get("status")));
const kind = computed(() => KINDS.find((k) => k === get("kind")));
/** `datetime-local` values (local time) in the URL; the API gets them as instants. */
const toIso = (v: string) => (v && !Number.isNaN(new Date(v).getTime()) ? new Date(v).toISOString() : undefined);
const filter = computed<ActionDeliveryFilter>(() => ({
  actionKey: get("actionKey") || undefined,
  status: status.value ?? null,
  kind: kind.value ?? null,
  from: toIso(get("from")) ?? null,
  to: toIso(get("to")) ?? null,
  instanceId: get("instanceId") || null,
  ciId: get("ciId") || null,
}));
const query = computed<ActionDeliveryListQuery>(() => {
  const f = filter.value;
  return {
    actionKey: f.actionKey,
    status: f.status ?? undefined,
    kind: f.kind ?? undefined,
    from: f.from ?? undefined,
    to: f.to ?? undefined,
    instanceId: f.instanceId ?? undefined,
    ciId: f.ciId ?? undefined,
    sort: lq.sort.value as SortField,
    limit: limit.value,
    offset: offset.value,
  };
});
const list = useActionDeliveries(workflowId, query);
const summary = useActionsSummary(workflowId);
const rows = computed(() => list.data.value?.data ?? []);
const total = computed(() => list.data.value?.page.total ?? 0);
const pastEnd = computed(() => !!list.data.value && total.value > 0 && rows.value.length === 0);
const filtered = computed(() => ["actionKey", "status", "kind", "from", "to", "instanceId", "ciId"].some((k) => !!get(k)));

function setWorkflow(id: string) {
  // Filters name the previous workflow's actions; start clean.
  router.push({ path: "/admin/workflow-deliveries", query: { workflow: id } });
}
function clearFilters() {
  update({ actionKey: undefined, status: undefined, kind: undefined, from: undefined, to: undefined, instanceId: undefined, ciId: undefined });
}

const kindLabel = (k: ActionKind) => t(`wfActions.kind.${k}` as MessageKey);
const COLUMNS: { key: string; label: MessageKey; sort?: string }[] = [
  { key: "queued", label: "deliveries.col.queued", sort: "createdAt" },
  { key: "action", label: "deliveries.col.action", sort: "actionKey" },
  { key: "recipient", label: "deliveries.col.recipient" },
  { key: "ci", label: "deliveries.col.ci" },
  { key: "status", label: "admin.col.status", sort: "status" },
  { key: "attempts", label: "deliveries.col.attempts", sort: "attempts" },
  { key: "next", label: "deliveries.col.nextOrCompleted", sort: "nextAttemptAt" },
];

// ---------- Selection ----------
const selected = ref(new Set<string>());
watch([workflowId, query], () => (selected.value = new Set()));
const selectable = computed(() => rows.value.filter((d) => canRetry(d) || canDiscard(d)));
const allSelected = computed(() => selectable.value.length > 0 && selectable.value.every((d) => selected.value.has(d.id)));
const someSelected = computed(() => !allSelected.value && selectable.value.some((d) => selected.value.has(d.id)));
function toggle(id: string, on: boolean) {
  const next = new Set(selected.value);
  if (on) next.add(id);
  else next.delete(id);
  selected.value = next;
}
function toggleAll(on: boolean) {
  selected.value = on ? new Set(selectable.value.map((d) => d.id)) : new Set();
}
const selectedRows = computed(() => rows.value.filter((d) => selected.value.has(d.id)));
const retryableSelected = computed(() => selectedRows.value.filter(canRetry).length);
const discardableSelected = computed(() => selectedRows.value.filter(canDiscard).length);

// ---------- Retry and discard ----------
const bulk = useBulkDeliveryAction();
/** What the confirm dialog will do: by ids, or every delivery the filter selects. */
const pending = ref<{ op: "retry" | "discard"; ids?: string[]; filter?: ActionDeliveryFilter; count: number } | null>(null);

function report(op: "retry" | "discard", res: ActionDeliveryBulkResult) {
  const parts = [t(op === "retry" ? "deliveries.retried" : "deliveries.discarded", { n: res.changed })];
  if (res.refused.length > 0) parts.push(t("deliveries.refused", { n: res.refused.length }));
  if (res.more) parts.push(t("deliveries.more"));
  flash.show(parts.join(" "));
}

function run(op: "retry" | "discard", body: { ids?: string[]; filter?: ActionDeliveryFilter }) {
  bulk.mutate(
    { id: workflowId.value, op, body },
    {
      onSuccess: (res) => {
        report(op, res);
        selected.value = new Set();
        pending.value = null;
      },
    },
  );
}

/** One delivery, from its detail: the confirmation stacks over it and shows a refusal there. */
function askOne(op: "retry" | "discard", d: ActionDelivery) {
  bulk.reset();
  pending.value = { op, ids: [d.id], count: 1 };
}
function askSelected(op: "retry" | "discard") {
  bulk.reset();
  const ids = selectedRows.value.filter(op === "retry" ? canRetry : canDiscard).map((d) => d.id);
  pending.value = { op, ids, count: ids.length };
}
/** Every dead delivery of the workflow (the summary banner's action), whatever the current filter. */
function askRetryAllDead() {
  bulk.reset();
  const dead = summary.data.value?.actions.reduce((n, a) => n + a.last7d.dead, 0) ?? 0;
  pending.value = { op: "retry", filter: { status: "dead" }, count: dead };
}
function confirmPending() {
  const p = pending.value;
  if (!p) return;
  run(p.op, p.ids ? { ids: p.ids } : { filter: p.filter });
}

function showDead() {
  update({ status: "dead" });
}

// ---------- Detail ----------
const openId = computed(() => get("delivery") || undefined);
const openDelivery = (d: ActionDelivery) => update({ delivery: d.id }, false);
const closeDelivery = () => update({ delivery: undefined }, false);

const statusOf = (s: ActionDeliveryStatus) => ({ label: deliveryStatusLabel(s), badge: statusBadge(s) });
</script>

<template>
  <div class="list-head">
    <Breadcrumbs :items="adminCrumbs('workflow-deliveries')" />
    <div class="page-header">
      <div class="title">
        <h1>{{ t("admin.section.deliveries") }}</h1>
        <span v-if="list.data.value" class="count mono">{{ t("common.total", { n: formatNumber(total) }) }}</span>
        <span v-if="list.isFetching.value && !list.isPending.value" class="spinner" :aria-label="t('common.refreshing')" />
      </div>
      <div class="actions">
        <button type="button" class="btn" :disabled="retryableSelected === 0 || bulk.isPending.value" data-testid="deliveries-retry-selected" @click="askSelected('retry')">
          {{ t("deliveries.retrySelected") }}<template v-if="retryableSelected > 0"> ({{ retryableSelected }})</template>
        </button>
        <button type="button" class="btn" :disabled="discardableSelected === 0 || bulk.isPending.value" @click="askSelected('discard')">
          {{ t("deliveries.discardSelected") }}<template v-if="discardableSelected > 0"> ({{ discardableSelected }})</template>
        </button>
      </div>
    </div>
    <p class="page-intro">{{ t("deliveries.intro") }}</p>
    <form class="toolbar" @submit.prevent>
      <div class="field">
        <label for="dl-workflow">{{ t("deliveries.workflow") }}</label>
        <select id="dl-workflow" :value="workflowId" :disabled="workflowRows.length === 0" @change="setWorkflow(($event.target as HTMLSelectElement).value)">
          <option v-for="w in workflowRows" :key="w.id" :value="w.id">{{ w.name }}</option>
        </select>
      </div>
      <div class="field">
        <label for="dl-action">{{ t("deliveries.col.action") }}</label>
        <select id="dl-action" :value="get('actionKey')" @change="update({ actionKey: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">{{ t("deliveries.filter.anyAction") }}</option>
          <option v-for="a in summary.data.value?.actions ?? []" :key="a.key" :value="a.key">{{ a.name ?? a.key }}</option>
        </select>
      </div>
      <div class="field">
        <label for="dl-status">{{ t("admin.col.status") }}</label>
        <select id="dl-status" :value="status ?? ''" @change="update({ status: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">{{ t("admin.filter.anyStatus") }}</option>
          <option v-for="s in DELIVERY_STATUSES" :key="s" :value="s">{{ deliveryStatusLabel(s) }}</option>
        </select>
      </div>
      <div class="field">
        <label for="dl-kind">{{ t("deliveries.kind") }}</label>
        <select id="dl-kind" :value="kind ?? ''" @change="update({ kind: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">{{ t("deliveries.filter.anyKind") }}</option>
          <option v-for="k in KINDS" :key="k" :value="k">{{ kindLabel(k) }}</option>
        </select>
      </div>
      <div class="field">
        <label for="dl-from">{{ t("deliveries.filter.from") }}</label>
        <input id="dl-from" type="datetime-local" :value="get('from')" @change="update({ from: ($event.target as HTMLInputElement).value || undefined })" />
      </div>
      <div class="field">
        <label for="dl-to">{{ t("deliveries.filter.to") }}</label>
        <input id="dl-to" type="datetime-local" :value="get('to')" @change="update({ to: ($event.target as HTMLInputElement).value || undefined })" />
      </div>
      <button v-if="filtered" type="button" class="btn btn-ghost" @click="clearFilters">{{ t("admin.filter.clear") }}</button>
    </form>
    <p v-if="get('instanceId') || get('ciId')" class="muted">
      {{ get("instanceId") ? t("deliveries.filter.instance") : t("deliveries.filter.ci") }} <code>{{ get("instanceId") || get("ciId") }}</code>
    </p>
  </div>

  <ErrorAlert v-if="workflows.isError.value" :error="workflows.error.value" :on-retry="() => workflows.refetch()" />
  <EmptyState v-else-if="workflows.data.value && workflowRows.length === 0" icon="network" :title="t('deliveries.noWorkflows.title')">
    {{ t("deliveries.noWorkflows.body") }}
    <template #actions><RouterLink to="/admin/workflows" class="btn">{{ t("admin.section.workflows") }}</RouterLink></template>
  </EmptyState>

  <template v-else-if="workflowId">
    <ErrorAlert v-if="summary.isError.value" :error="summary.error.value" :title="t('deliveries.summaryFailed')" :on-retry="() => summary.refetch()" />
    <DeliveriesSummaryBanner v-else-if="summary.data.value" :summary="summary.data.value" @show-dead="showDead" @retry-dead="askRetryAllDead" />
    <ErrorAlert v-if="bulk.isError.value && !pending" :error="bulk.error.value" :title="t('deliveries.bulkFailed')" />

    <section class="panel explorer" :aria-label="t('deliveries.table', { workflow: workflow?.name ?? '' })">
      <div v-if="list.isError.value" class="panel-body">
        <ErrorAlert :error="list.error.value" :on-retry="() => list.refetch()" />
      </div>
      <SkeletonRows v-else-if="list.isPending.value" :label="t('common.loading')" />
      <EmptyState v-else-if="total === 0 && filtered" icon="search" :title="t('deliveries.noMatch')">
        {{ t("admin.filter.noMatchBody") }}
        <template #actions><button type="button" class="btn" @click="clearFilters">{{ t("admin.filter.clear") }}</button></template>
      </EmptyState>
      <EmptyState v-else-if="total === 0" icon="inbox" :title="t('deliveries.empty.title')">
        {{ t("deliveries.empty.body") }}
        <template #actions><RouterLink :to="`/admin/workflows/${workflowId}`" class="btn">{{ t("deliveries.empty.open", { name: workflow?.name ?? '' }) }}</RouterLink></template>
      </EmptyState>
      <EmptyState v-else-if="pastEnd" :title="t('common.pastEnd')">
        <template #actions><button type="button" class="btn" @click="update({})">{{ t("common.firstPage") }}</button></template>
      </EmptyState>

      <template v-if="rows.length > 0 && !list.isError.value">
        <div class="table-wrap table-scroll" role="region" tabindex="0" :aria-label="t('deliveries.table', { workflow: workflow?.name ?? '' })">
          <table :class="['data', 'list-table', { loading: list.isPlaceholderData.value }]" data-testid="deliveries-table">
            <thead>
              <tr>
                <th scope="col" class="select-col">
                  <input
                    type="checkbox"
                    :checked="allSelected"
                    :indeterminate="someSelected"
                    :disabled="selectable.length === 0"
                    :aria-label="t('deliveries.selectPage')"
                    @change="toggleAll(($event.target as HTMLInputElement).checked)"
                  />
                </th>
                <th v-for="c in COLUMNS" :key="c.key" scope="col" :aria-sort="c.sort ? lq.ariaSort(c.sort) : undefined">
                  <button v-if="c.sort" type="button" class="sort" @click="lq.toggleSort(c.sort)">{{ t(c.label) }} <SortIcon :dir="lq.ariaSort(c.sort)" /></button>
                  <template v-else>{{ t(c.label) }}</template>
                </th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="d in rows" :key="d.id" :data-id="d.id" :class="{ selected: selected.has(d.id) }">
                <td class="select-col">
                  <input
                    v-if="canRetry(d) || canDiscard(d)"
                    type="checkbox"
                    :checked="selected.has(d.id)"
                    :aria-label="t('deliveries.select', { action: d.actionName ?? d.actionKey })"
                    @change="toggle(d.id, ($event.target as HTMLInputElement).checked)"
                  />
                </td>
                <td>
                  <button type="button" class="btn btn-link" :title="formatDateTime(d.createdAt)" @click="openDelivery(d)">
                    <time :datetime="d.createdAt">{{ formatRelative(d.createdAt) }}</time>
                  </button>
                </td>
                <td>
                  <span class="name-badges">
                    <span dir="auto">{{ d.actionName ?? d.actionKey }}</span>
                    <span class="badge">{{ kindLabel(d.kind) }}</span>
                  </span>
                </td>
                <td><DeliveryRecipient :recipient="d.recipient" /></td>
                <td>
                  <RouterLink v-if="d.ciLabel" :to="`/cis/${d.ciId}`" dir="auto">{{ d.ciLabel }}</RouterLink>
                  <span v-else class="muted">{{ t("deliveries.ciHidden") }}</span>
                </td>
                <td :title="d.statusReason ? reasonText(d.statusReason) : undefined">
                  <span class="name-badges">
                    <span :class="['badge', statusOf(d.status).badge]"><span class="status-dot" aria-hidden="true" />{{ statusOf(d.status).label }}</span>
                    <code v-if="d.statusReason" class="muted">{{ d.statusReason }}</code>
                  </span>
                </td>
                <td class="mono">{{ d.attempts }}<span v-if="d.lastStatusCode" class="muted"> · {{ d.lastStatusCode }}</span></td>
                <td>
                  <time v-if="d.nextAttemptAt" :datetime="d.nextAttemptAt" :title="formatDateTime(d.nextAttemptAt)">{{ formatRelative(d.nextAttemptAt) }}</time>
                  <time v-else-if="d.completedAt" :datetime="d.completedAt" :title="formatDateTime(d.completedAt)" class="muted">{{ formatRelative(d.completedAt) }}</time>
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

    <DeliveryDetailDialog :workflow-id="workflowId" :delivery-id="openId" :busy="bulk.isPending.value" @close="closeDelivery" @retry="askOne('retry', $event)" @discard="askOne('discard', $event)" />
  </template>

  <ConfirmDialog
    :open="!!pending"
    :title="pending?.op === 'retry' ? t('deliveries.confirmRetry.title', { n: pending.count }) : t('deliveries.confirmDiscard.title', { n: pending?.count ?? 0 })"
    :confirm-label="pending?.op === 'retry' ? t('deliveries.retry') : t('deliveries.discard')"
    :tone="pending?.op === 'retry' ? 'primary' : 'danger'"
    :busy="bulk.isPending.value"
    @cancel="pending = null"
    @confirm="confirmPending"
  >
    <template v-if="pending">
      <ErrorAlert v-if="bulk.isError.value" :error="bulk.error.value" :title="t('deliveries.bulkFailed')" />
      <p>{{ pending.op === "retry" ? t(pending.filter ? "deliveries.confirmRetry.bodyAll" : "deliveries.confirmRetry.body") : t("deliveries.confirmDiscard.body") }}</p>
      <p class="muted">{{ t("deliveries.audited") }}</p>
    </template>
  </ConfirmDialog>
</template>
