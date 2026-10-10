<script setup lang="ts">
import { computed } from "vue";
import type { ActionsSummary } from "../../../api/actionDeliveries";
import { formatNumber, t } from "../../../i18n";
import { formatDateTime, formatRelative } from "../../../lib/format";

/**
 * What needs an administrator's attention in the outbox (SHAA-2725 §4.4, §4.6): the whole queue overloaded
 * (new runs are suppressed), dead deliveries of this workflow, runs suppressed in the last day, and workers
 * that stopped counting. Nothing renders while all is well.
 */
const props = defineProps<{ summary: ActionsSummary }>();
const emit = defineEmits<{ showDead: []; retryDead: [] }>();

/** Workers refresh the queue state every tick; a count much older means no process runs them. */
const STALE_MS = 10 * 60_000;

const dead = computed(() => props.summary.actions.reduce((n, a) => n + a.last7d.dead, 0));
const suppressed = computed(() => props.summary.actions.reduce((n, a) => n + a.suppressed24h, 0));
const queue = computed(() => props.summary.queue);
const stale = computed(() => Date.now() - new Date(queue.value.checkedAt).getTime() > STALE_MS);
</script>

<template>
  <div v-if="queue.overloaded || dead > 0 || suppressed > 0 || stale" class="stack" data-testid="deliveries-banner">
    <div v-if="queue.overloaded" class="alert alert-error" role="alert">
      <strong>{{ t("deliveries.banner.overloaded") }}</strong>
      <div>{{ t("deliveries.banner.overloadedBody", { backlog: formatNumber(queue.backlog), max: formatNumber(queue.queueMax) }) }}</div>
    </div>
    <div v-if="dead > 0" class="alert alert-warn" role="status">
      <strong>{{ t("deliveries.banner.dead", { n: dead }) }}</strong>
      <div>{{ t("deliveries.banner.deadBody", { attempts: queue.maxAttempts, hours: queue.maxAgeHours }) }}</div>
      <div class="actions">
        <button type="button" class="btn btn-sm" @click="emit('showDead')">{{ t("deliveries.banner.showDead") }}</button>
        <button type="button" class="btn btn-sm" @click="emit('retryDead')">{{ t("deliveries.banner.retryDead") }}</button>
      </div>
    </div>
    <div v-if="suppressed > 0" class="alert alert-warn" role="status">
      <strong>{{ t("deliveries.banner.suppressed", { n: suppressed }) }}</strong>
      <div>{{ t("deliveries.banner.suppressedBody", { perHour: queue.maxPerInstancePerHour }) }}</div>
    </div>
    <div v-if="stale" class="alert alert-warn" role="status">
      <strong>{{ t("deliveries.banner.stale") }}</strong>
      <div>
        {{ t("deliveries.banner.staleBody") }}
        <time :datetime="queue.checkedAt" :title="formatDateTime(queue.checkedAt)">{{ formatRelative(queue.checkedAt) }}</time>
      </div>
    </div>
  </div>
</template>
