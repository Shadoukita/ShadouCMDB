<script setup lang="ts">
import { computed, ref } from "vue";
import { ApiError } from "../../../api/client";
import {
  useDeleteWebhookEndpoint,
  usePauseResumeWebhookEndpoint,
  usePingWebhookEndpoint,
  useWebhookEndpoints,
  type WebhookEndpoint,
  type WebhookPingResult,
} from "../../../api/webhooks";
import Breadcrumbs from "../../../components/Breadcrumbs.vue";
import ConfirmDialog from "../../../components/ConfirmDialog.vue";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import FormDialog from "../../../components/FormDialog.vue";
import Icon from "../../../components/Icon.vue";
import PaginationBar from "../../../components/PaginationBar.vue";
import RowMenu, { type RowMenuItem } from "../../../components/RowMenu.vue";
import SkeletonRows from "../../../components/SkeletonRows.vue";
import { formatNumber, t } from "../../../i18n";
import { useDocumentTitle } from "../../../lib/composables";
import { formatDateTime, formatRelative } from "../../../lib/format";
import { useListQuery } from "../../../lib/listQuery";
import { ENDPOINT_STATUSES, endpointStatusLabel, reasonText, statusBadge } from "../../../lib/outbound";
import { useFlashStore } from "../../../stores/flash";
import { useSessionStore } from "../../../stores/session";
import { adminCrumbs } from "../sections";
import RotateSecretDialog from "./RotateSecretDialog.vue";
import WebhookAllowedHosts from "./WebhookAllowedHosts.vue";
import WebhookEndpointDialog from "./WebhookEndpointDialog.vue";

/**
 * Administration › Webhooks (SHAA-2725 §5, §8): the endpoints workflow actions send to, and the allowlist of
 * hosts they may reach. Everything here needs `webhooks.manage`; a `workflows.manage` holder sees the
 * endpoints by key, name and status only (to choose one in a workflow action) and changes nothing.
 * The status filter and the page live in the URL.
 */
useDocumentTitle(() => t("admin.section.webhooks"));
const session = useSessionStore();
const manage = computed(() => session.can("webhooks.manage"));
const flash = useFlashStore();

const lq = useListQuery({ sort: "" });
const { get, limit, offset, update } = lq;
const status = computed(() => ENDPOINT_STATUSES.find((s) => s === get("status")));
const list = useWebhookEndpoints(() => ({ status: status.value, limit: limit.value, offset: offset.value }));
const rows = computed(() => list.data.value?.data ?? []);
const total = computed(() => list.data.value?.page.total ?? 0);
const pastEnd = computed(() => !!list.data.value && total.value > 0 && rows.value.length === 0);

// ---------- Create, edit, rotate ----------
const editing = ref<WebhookEndpoint | null>(null);
const dialogOpen = ref(false);
const rotating = ref<WebhookEndpoint | null>(null);

function openCreate() {
  editing.value = null;
  dialogOpen.value = true;
}
function openEdit(e: WebhookEndpoint) {
  editing.value = e;
  dialogOpen.value = true;
}

// ---------- Ping ----------
const ping = usePingWebhookEndpoint();
const pinged = ref<{ endpoint: WebhookEndpoint; result: WebhookPingResult | null } | null>(null);

function runPing(e: WebhookEndpoint) {
  ping.reset();
  pinged.value = { endpoint: e, result: null };
  ping.mutate(e.id, {
    onSuccess: (result) => {
      if (pinged.value?.endpoint.id === e.id) pinged.value = { endpoint: e, result };
    },
  });
}

// ---------- Pause, resume ----------
const pauseResume = usePauseResumeWebhookEndpoint();
const runError = ref<{ endpoint: WebhookEndpoint; to: "pause" | "resume"; error: unknown } | null>(null);
/** An imported or restored endpoint, or one whose secret no longer decrypts: a rotation comes first. */
const secretRequired = computed(() => runError.value?.error instanceof ApiError && runError.value.error.code === "SECRET_REQUIRED");

function setRunning(e: WebhookEndpoint, to: "pause" | "resume") {
  pauseResume.mutate(
    { id: e.id, to },
    {
      onSuccess: () => flash.show(t(to === "pause" ? "webhooks.paused" : "webhooks.resumed", { name: e.name })),
      onError: (error) => (runError.value = { endpoint: e, to, error }),
    },
  );
}

// ---------- Delete ----------
const del = useDeleteWebhookEndpoint();
const deleting = ref<WebhookEndpoint | null>(null);

function askDelete(e: WebhookEndpoint) {
  del.reset();
  deleting.value = e;
}
function confirmDelete() {
  const e = deleting.value;
  if (!e) return;
  del.mutate(e.id, {
    onSuccess: () => {
      flash.show(t("webhooks.deleted", { name: e.name }));
      deleting.value = null;
    },
  });
}

const rowMenu = (e: WebhookEndpoint): RowMenuItem[] => [
  { label: t("webhooks.row.ping"), action: () => runPing(e) },
  { label: t("common.edit"), action: () => openEdit(e) },
  { label: t("webhooks.row.rotate"), action: () => (rotating.value = e) },
  e.status === "active" ? { label: t("webhooks.row.pause"), action: () => setRunning(e, "pause") } : { label: t("webhooks.row.resume"), action: () => setRunning(e, "resume") },
  { label: t("common.delete"), action: () => askDelete(e), danger: true },
];

const statusTitle = (e: WebhookEndpoint) => (e.status === "suspended" && e.suspendedReason ? reasonText(e.suspendedReason) : undefined);
</script>

<template>
  <div class="list-head">
    <Breadcrumbs :items="adminCrumbs('webhooks')" />
    <div class="page-header">
      <div class="title">
        <h1>{{ t("admin.section.webhooks") }}</h1>
        <span v-if="list.data.value" class="count mono">{{ t("common.total", { n: formatNumber(total) }) }}</span>
        <span v-if="list.isFetching.value && !list.isPending.value" class="spinner" :aria-label="t('common.refreshing')" />
      </div>
      <div v-if="manage" class="actions">
        <button type="button" class="btn btn-primary" data-testid="webhook-new" @click="openCreate"><Icon name="plus" />{{ t("webhooks.new") }}</button>
      </div>
    </div>
    <p class="page-intro">{{ t("webhooks.intro") }}</p>
    <div v-if="!manage" class="alert alert-info" role="note" data-testid="webhooks-names-only">{{ t("webhooks.namesOnly") }}</div>
    <form class="toolbar" @submit.prevent>
      <div class="field">
        <label for="wh-status">{{ t("admin.col.status") }}</label>
        <select id="wh-status" :value="status ?? ''" @change="update({ status: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">{{ t("admin.filter.anyStatus") }}</option>
          <option v-for="s in ENDPOINT_STATUSES" :key="s" :value="s">{{ endpointStatusLabel(s) }}</option>
        </select>
      </div>
    </form>
  </div>

  <section class="panel explorer" :aria-label="t('webhooks.endpoints')">
    <div v-if="list.isError.value" class="panel-body">
      <ErrorAlert :error="list.error.value" :on-retry="() => list.refetch()" />
    </div>
    <SkeletonRows v-else-if="list.isPending.value" :label="t('common.loading')" />
    <EmptyState v-else-if="total === 0 && status" icon="search" :title="t('webhooks.noMatch')">
      {{ t("admin.filter.noMatchBody") }}
      <template #actions><button type="button" class="btn" @click="update({ status: undefined })">{{ t("admin.filter.clear") }}</button></template>
    </EmptyState>
    <EmptyState v-else-if="total === 0" icon="network" :title="t('webhooks.empty.title')">
      {{ manage ? t("webhooks.empty.body") : t("webhooks.empty.bodyNamesOnly") }}
      <template v-if="manage" #actions>
        <button type="button" class="btn btn-primary" @click="openCreate"><Icon name="plus" />{{ t("webhooks.new") }}</button>
      </template>
    </EmptyState>
    <EmptyState v-else-if="pastEnd" :title="t('common.pastEnd')">
      <template #actions><button type="button" class="btn" @click="update({})">{{ t("common.firstPage") }}</button></template>
    </EmptyState>

    <template v-if="rows.length > 0 && !list.isError.value">
      <div class="table-wrap table-scroll" role="region" tabindex="0" :aria-label="t('webhooks.endpoints')">
        <table :class="['data', 'list-table', { loading: list.isPlaceholderData.value }]" data-testid="webhook-endpoints">
          <thead>
            <tr>
              <th scope="col">{{ t("webhooks.field.name") }}</th>
              <th scope="col">{{ t("webhooks.field.key") }}</th>
              <th scope="col">{{ t("admin.col.status") }}</th>
              <template v-if="manage">
                <th scope="col">{{ t("webhooks.field.url") }}</th>
                <th scope="col">{{ t("webhooks.col.lastSuccess") }}</th>
                <th scope="col">{{ t("webhooks.col.failures") }}</th>
                <th scope="col" class="row-actions"><span class="sr-only">{{ t("inventory.actions") }}</span></th>
              </template>
            </tr>
          </thead>
          <tbody>
            <tr v-for="e in rows" :key="e.id" :data-id="e.id" :class="{ disabled: e.status !== 'active' }">
              <td dir="auto">{{ e.name }}</td>
              <td><code>{{ e.key }}</code></td>
              <td :title="statusTitle(e)">
                <span class="name-badges">
                  <span :class="['badge', statusBadge(e.status)]"><span class="status-dot" aria-hidden="true" />{{ endpointStatusLabel(e.status) }}</span>
                  <span v-if="e.status === 'suspended' && e.suspendedReason" class="muted">{{ reasonText(e.suspendedReason) }}</span>
                </span>
              </td>
              <template v-if="manage">
                <td>
                  <span class="name-badges">
                    <code class="truncate">{{ e.url }}</code>
                    <span v-if="e.unencrypted" class="badge warn">{{ t("webhooks.unencrypted") }}</span>
                    <span v-if="e.authHeaderSet" class="badge" :title="t('webhooks.headerSetTitle')">{{ e.authHeaderName }}</span>
                  </span>
                </td>
                <td>
                  <time v-if="e.lastSuccessAt" :datetime="e.lastSuccessAt" :title="formatDateTime(e.lastSuccessAt)">{{ formatRelative(e.lastSuccessAt) }}</time>
                  <span v-else class="muted">{{ t("admin.never") }}</span>
                </td>
                <td :title="e.lastFailureAt ? t('webhooks.col.lastFailure', { when: formatDateTime(e.lastFailureAt) }) : undefined">
                  <span :class="{ muted: !e.consecutiveFailures }">{{ formatNumber(e.consecutiveFailures ?? 0) }}</span>
                </td>
                <td class="row-actions">
                  <RowMenu :label="t('inventory.rowMenu', { name: e.name })" :items="rowMenu(e)" />
                </td>
              </template>
            </tr>
          </tbody>
        </table>
      </div>
      <div class="table-footer">
        <PaginationBar numbered :total="total" :limit="limit" :offset="offset" @change="lq.onPage" />
      </div>
    </template>
  </section>

  <WebhookAllowedHosts v-if="manage" />

  <template v-if="manage">
    <WebhookEndpointDialog :open="dialogOpen" :endpoint="editing" @close="dialogOpen = false" />
    <RotateSecretDialog :endpoint="rotating" @close="rotating = null" />

    <FormDialog readonly :open="!!pinged" :title="t('webhooks.ping.title', { name: pinged?.endpoint.name ?? '' })" submit-label="" @cancel="pinged = null">
      <div data-testid="webhook-ping-result" aria-live="polite">
        <p v-if="ping.isPending.value" class="muted">{{ t("webhooks.ping.running") }}</p>
        <ErrorAlert v-else-if="ping.isError.value" :error="ping.error.value" :title="t('webhooks.ping.failed')" />
        <template v-else-if="pinged?.result">
          <div v-if="pinged.result.ok" class="alert alert-success" role="status">
            <strong>{{ t("webhooks.ping.ok", { code: pinged.result.statusCode ?? '', ms: pinged.result.durationMs }) }}</strong>
          </div>
          <div v-else class="alert alert-error" role="alert">
            <strong>{{ pinged.result.reason ? reasonText(pinged.result.reason) : t("webhooks.ping.notOk") }}</strong>
            <div>{{ pinged.result.message }}</div>
            <div v-if="pinged.result.reason" class="meta"><code>{{ pinged.result.reason }}</code></div>
          </div>
          <p class="muted no-margin">{{ t("webhooks.ping.note") }}</p>
        </template>
      </div>
    </FormDialog>

    <FormDialog readonly :open="!!runError" :title="t(runError?.to === 'pause' ? 'webhooks.pauseFailed' : 'webhooks.resumeFailed', { name: runError?.endpoint.name ?? '' })" submit-label="" @cancel="runError = null">
      <template v-if="runError">
        <ErrorAlert :error="runError.error" />
        <p v-if="secretRequired" class="no-margin">
          {{ t("webhooks.secretRequired") }}
          <button type="button" class="btn btn-sm" @click="rotating = runError.endpoint; runError = null">{{ t("webhooks.row.rotate") }}</button>
        </p>
      </template>
    </FormDialog>

    <ConfirmDialog
      :open="!!deleting"
      :title="t('webhooks.delete.title', { name: deleting?.name ?? '' })"
      :confirm-label="t('common.delete')"
      :busy="del.isPending.value"
      @cancel="deleting = null"
      @confirm="confirmDelete"
    >
      <template v-if="deleting">
        <ErrorAlert v-if="del.isError.value" :error="del.error.value" :title="t('webhooks.delete.failed')" />
        <p>{{ t("webhooks.delete.body", { key: deleting.key }) }}</p>
      </template>
    </ConfirmDialog>
  </template>
</template>
