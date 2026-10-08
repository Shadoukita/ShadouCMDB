<script setup lang="ts">
import { computed } from "vue";
import { RouterLink } from "vue-router";
import ErrorAlert from "../../components/ErrorAlert.vue";
import Icon from "../../components/Icon.vue";
import LoadingState from "../../components/LoadingState.vue";
import { formatNumber, t } from "../../i18n";
import { colourByRank } from "../../lib/dashboard";

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
 * Counts per class or per lookup value, largest first (design §0 step 12e): a colour key, the name as a link
 * to the inventory, a bar scaled to the largest count in a categorical series colour (--c-viz-*), and the
 * count in mono. The count is the reading; the bar and the colour only repeat it. The share of all CIs is
 * in each row's accessible text and tooltip.
 */
const props = defineProps<{ title: string; labelHeader: string; rows: CountRow[]; total: number; loading: boolean; error: unknown; more?: { to: string; label: string } }>();
const sorted = computed(() =>
  [...props.rows].sort((a, b) => (b.count ?? -1) - (a.count ?? -1) || a.label.localeCompare(b.label)),
);
const hasActions = computed(() => props.rows.some((r) => r.newTo));
const top = computed(() => Math.max(0, ...sorted.value.map((r) => r.count ?? 0)));
/** A count above 0 always shows a sliver of bar. */
const barWidth = (count: number | undefined) => `${count && top.value ? Math.max(1, (count / top.value) * 100) : 0}%`;
const percent = (count: number | undefined) => {
  const p = props.total && count ? (count / props.total) * 100 : 0;
  return p > 0 && p < 1 ? t("dashboard.share.under1") : t("dashboard.share.pct", { pct: formatNumber(Math.round(p)) });
};
</script>

<template>
  <section class="panel dash-card">
    <div class="panel-header">
      <h2>{{ title }}</h2>
      <RouterLink v-if="more" :to="more.to">{{ more.label }}</RouterLink>
    </div>
    <div class="panel-body">
      <LoadingState v-if="loading" />
      <ErrorAlert v-if="error != null" :error="error" :title="t('dashboard.countsFailed')" />
      <slot name="note" />
      <ul v-if="!loading && sorted.length > 0" :class="['count-bars', { 'has-actions': hasActions }]" :aria-label="labelHeader">
        <li v-for="(r, i) in sorted" :key="r.id" :class="`viz-${colourByRank(i)}`">
          <RouterLink :to="r.to" class="count-name" dir="auto"><span class="key" aria-hidden="true" />{{ r.label }}</RouterLink>
          <span class="count-track" aria-hidden="true"><span class="count-bar" :style="{ width: barWidth(r.count) }" /></span>
          <span class="count-value" :title="r.count === undefined ? undefined : t('dashboard.share.ofAll', { pct: percent(r.count) })">
            <span v-if="r.count === undefined" class="spinner" :aria-label="t('common.loading')" />
            <template v-else>
              {{ formatNumber(r.count) }}<span class="sr-only">, {{ t("dashboard.share.ofAll", { pct: percent(r.count) }) }}</span>
            </template>
          </span>
          <span v-if="hasActions" class="count-new">
            <RouterLink v-if="r.newTo" :to="r.newTo" class="btn btn-sm btn-ghost btn-icon" :aria-label="r.newLabel" :title="r.newLabel">
              <Icon name="plus" :size="14" />
            </RouterLink>
          </span>
        </li>
      </ul>
    </div>
  </section>
</template>
