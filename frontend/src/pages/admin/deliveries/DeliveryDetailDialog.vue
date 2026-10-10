<script setup lang="ts">
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { useActionDelivery, type ActionDelivery } from "../../../api/actionDeliveries";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import FormDialog from "../../../components/FormDialog.vue";
import { t, type MessageKey } from "../../../i18n";
import { formatDateTime } from "../../../lib/format";
import { deliveryStatusLabel, reasonText, statusBadge } from "../../../lib/outbound";
import DeliveryRecipient from "./DeliveryRecipient.vue";
import { canDiscard, canRetry } from "./deliveryRules";

/**
 * One delivery (SHAA-2725 §11.2): where it stands and why, its last error, the run and the event it tells
 * of, with retry and discard. The id lives in the URL (`?delivery=`), so the view can be shared.
 */
const props = defineProps<{ workflowId: string; deliveryId: string | undefined; busy?: boolean }>();
const emit = defineEmits<{ close: []; retry: [d: ActionDelivery]; discard: [d: ActionDelivery] }>();
const detail = useActionDelivery(() => props.workflowId, () => props.deliveryId);
const d = computed(() => detail.data.value?.delivery);
const kindLabel = (k: string) => t(`wfActions.kind.${k}` as MessageKey);
</script>

<template>
  <FormDialog readonly wide :open="!!deliveryId" :title="t('deliveries.detail.title')" submit-label="" @cancel="emit('close')">
    <ErrorAlert v-if="detail.isError.value" :error="detail.error.value" :on-retry="() => detail.refetch()" />
    <p v-else-if="detail.isPending.value" class="muted">{{ t("common.loading") }}</p>
    <template v-else-if="d && detail.data.value">
      <dl class="props" data-testid="delivery-detail">
        <dt>{{ t("admin.col.status") }}</dt>
        <dd>
          <span :class="['badge', statusBadge(d.status)]"><span class="status-dot" aria-hidden="true" />{{ deliveryStatusLabel(d.status) }}</span>
          <template v-if="d.statusReason">
            {{ reasonText(d.statusReason) }} <code>{{ d.statusReason }}</code>
          </template>
        </dd>
        <dt>{{ t("deliveries.col.action") }}</dt>
        <dd>{{ d.actionName ?? t("deliveries.actionDeleted") }} <code>{{ d.actionKey }}</code> · {{ kindLabel(d.kind) }}</dd>
        <dt>{{ t("deliveries.col.recipient") }}</dt>
        <dd><DeliveryRecipient :recipient="d.recipient" /></dd>
        <dt>{{ t("deliveries.col.ci") }}</dt>
        <dd>
          <RouterLink v-if="d.ciLabel" :to="`/cis/${d.ciId}`" dir="auto">{{ d.ciLabel }}</RouterLink>
          <span v-else class="muted">{{ t("deliveries.ciHidden") }}</span>
          · <RouterLink :to="`/workflows/${d.instanceId}`">{{ t("deliveries.openInstance") }}</RouterLink>
        </dd>
        <dt>{{ t("deliveries.col.attempts") }}</dt>
        <dd>
          {{ d.attempts }}
          <template v-if="d.lastStatusCode"> · {{ t("deliveries.lastCode", { code: d.lastStatusCode }) }}</template>
        </dd>
        <dt>{{ t("deliveries.col.queued") }}</dt>
        <dd>{{ formatDateTime(d.createdAt) }}</dd>
        <template v-if="d.nextAttemptAt">
          <dt>{{ t("deliveries.col.next") }}</dt>
          <dd>{{ formatDateTime(d.nextAttemptAt) }}</dd>
        </template>
        <template v-if="d.completedAt">
          <dt>{{ t("deliveries.col.completed") }}</dt>
          <dd>{{ formatDateTime(d.completedAt) }}</dd>
        </template>
        <template v-if="d.retriedAt">
          <dt>{{ t("deliveries.retriedAt") }}</dt>
          <dd>{{ formatDateTime(d.retriedAt) }}</dd>
        </template>
        <dt>{{ t("deliveries.run") }}</dt>
        <dd>
          <code>#{{ d.runId }}</code> · <code>{{ detail.data.value.runStatus }}</code>
          <template v-if="detail.data.value.runStatusReason"> · {{ reasonText(detail.data.value.runStatusReason) }}</template>
        </dd>
        <dt>{{ t("deliveries.event") }}</dt>
        <dd>
          <template v-if="detail.data.value.event">
            <code>{{ detail.data.value.event.kind }}</code>
            <template v-if="detail.data.value.event.transitionKey"> · <code>{{ detail.data.value.event.transitionKey }}</code></template>
            · <code>{{ detail.data.value.event.fromStateKey ?? "∅" }}</code> → <code>{{ detail.data.value.event.toStateKey }}</code>
            <template v-if="detail.data.value.event.actorName"> · {{ detail.data.value.event.actorName }}</template>
            · {{ formatDateTime(detail.data.value.event.occurredAt) }}
          </template>
          <span v-else class="muted">{{ t("deliveries.eventArchived") }}</span>
        </dd>
      </dl>
      <div v-if="detail.data.value.lastError" class="field">
        <span class="label">{{ t("deliveries.lastError") }}</span>
        <pre class="last-error" data-testid="delivery-last-error">{{ detail.data.value.lastError }}</pre>
      </div>
      <div v-if="canRetry(d) || canDiscard(d)" class="actions">
        <button v-if="canRetry(d)" type="button" class="btn btn-primary" :disabled="busy" data-testid="delivery-retry" @click="emit('retry', d)">{{ t("deliveries.retry") }}</button>
        <button v-if="canDiscard(d)" type="button" class="btn btn-danger" :disabled="busy" @click="emit('discard', d)">{{ t("deliveries.discard") }}</button>
      </div>
    </template>
  </FormDialog>
</template>

<style scoped>
pre.last-error {
  max-height: 240px;
  overflow: auto;
  margin: 0;
  padding: var(--space-2);
  border: 1px solid var(--c-border);
  border-radius: var(--radius-md);
  white-space: pre-wrap;
  word-break: break-word;
  font-family: var(--font-mono);
  font-size: var(--fs-sm);
}
</style>
