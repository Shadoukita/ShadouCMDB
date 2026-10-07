<script setup lang="ts">
import { computed } from "vue";
import { RouterLink } from "vue-router";
import ErrorAlert from "../../components/ErrorAlert.vue";
import Icon from "../../components/Icon.vue";
import LoadingState from "../../components/LoadingState.vue";
import { formatNumber, t } from "../../i18n";

export interface CountRow {
  id: string;
  label: string;
  count: number | undefined;
  to: string;
  /** Optional "New" action per row. */
  newTo?: string;
  newLabel?: string;
}

/**
 * Counts per class or per lookup value, largest first, each with its share of all CIs as a bar in the first
 * data-viz series colour (design §2.3) and as a percentage, so the bar is never the only reading.
 */
const props = defineProps<{ title: string; labelHeader: string; rows: CountRow[]; total: number; loading: boolean; error: unknown }>();
const sorted = computed(() =>
  [...props.rows].sort((a, b) => (b.count ?? -1) - (a.count ?? -1) || a.label.localeCompare(b.label)),
);
const hasActions = computed(() => props.rows.some((r) => r.newTo));
const share = (count: number | undefined) => (props.total && count ? (count / props.total) * 100 : 0);
/** A count above 0 always shows a sliver of bar. */
const barWidth = (count: number | undefined) => `${count ? Math.max(1, share(count)) : 0}%`;
const percent = (count: number | undefined) => {
  const p = share(count);
  return p > 0 && p < 1 ? t("dashboard.share.under1") : t("dashboard.share.pct", { pct: formatNumber(Math.round(p)) });
};
</script>

<template>
  <section class="panel">
    <div class="panel-header">
      <h2>{{ title }}</h2>
    </div>
    <div class="panel-body flush">
      <LoadingState v-if="loading" />
      <div v-if="error != null" class="panel-body">
        <ErrorAlert :error="error" :title="t('dashboard.countsFailed')" />
      </div>
      <div v-if="$slots.note" class="panel-body"><slot name="note" /></div>
      <table v-if="!loading && sorted.length > 0" class="data count-table">
        <thead>
          <tr>
            <th scope="col">{{ labelHeader }}</th>
            <th scope="col" class="num">{{ t("dashboard.col.count") }}</th>
            <th scope="col" class="share">{{ t("dashboard.col.share") }}</th>
            <th v-if="hasActions" scope="col" class="row-actions"><span class="sr-only">{{ t("dashboard.col.actions") }}</span></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="r in sorted" :key="r.id">
            <td dir="auto"><RouterLink :to="r.to">{{ r.label }}</RouterLink></td>
            <td class="num mono">
              <span v-if="r.count === undefined" class="spinner" :aria-label="t('common.loading')" />
              <template v-else>{{ formatNumber(r.count) }}</template>
            </td>
            <td class="share">
              <span class="share-track" aria-hidden="true"><span class="share-bar" :style="{ width: barWidth(r.count) }" /></span>
              <span v-if="r.count !== undefined" class="share-pct">{{ percent(r.count) }}</span>
            </td>
            <td v-if="hasActions" class="row-actions">
              <RouterLink v-if="r.newTo" :to="r.newTo" class="btn btn-sm btn-ghost btn-icon" :aria-label="r.newLabel" :title="r.newLabel">
                <Icon name="plus" :size="14" />
              </RouterLink>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>
</template>
