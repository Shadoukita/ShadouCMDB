<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useChangeHistogram } from "../../api/queries";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import { formatNumber, t } from "../../i18n";
import { SERIES, bucketTotal, formatBucket, layout, niceMax, peak, segmentPath, type HistogramBucket } from "../../lib/changeHistogram";
import { changeWindows, type Period } from "../../lib/dashboard";

/**
 * Changes to CIs over the period, stacked into updated, created and status changed (--c-viz-1…3, design §0
 * step 12e). Shares its request with the "Changes this period" card. The plot is a picture with a summary
 * as its name; every count is in the values table behind "Show values".
 */
const props = defineProps<{ period: Period; now: number }>();
const PLOT_H = 200;

const win = computed(() => changeWindows(props.period, props.now).current);
const query = useChangeHistogram(() => win.value);
const buckets = computed<HistogramBucket[]>(() => query.data.value?.buckets ?? []);
const bucket = computed(() => query.data.value?.bucket ?? win.value.bucket);
const max = computed(() => niceMax(peak(buckets.value) ? bucketTotal(peak(buckets.value)!) : 0));

const plot = ref<HTMLElement>();
const width = ref(0);
watch(plot, (el, _, onCleanup) => {
  if (!el) return;
  width.value = el.clientWidth;
  if (typeof ResizeObserver === "undefined") return;
  const observer = new ResizeObserver(() => (width.value = el.clientWidth));
  observer.observe(el);
  onCleanup(() => observer.disconnect());
});
const columns = computed(() => layout(buckets.value, width.value, PLOT_H, max.value));

const label = (b: HistogramBucket) => formatBucket(b.start, bucket.value);
/** First, middle and last bucket under the plot. */
const ticks = computed(() => {
  const n = buckets.value.length;
  if (n === 0) return [];
  return [...new Set([0, Math.floor((n - 1) / 2), n - 1])].map((i) => label(buckets.value[i]!));
});
const summary = computed(() => {
  const top = peak(buckets.value);
  const total = query.data.value?.total ?? 0;
  return top
    ? t("dashboard.changes.summary", { total, peak: label(top), peakTotal: bucketTotal(top) })
    : t("dashboard.changes.none");
});
const showTable = ref(false);
</script>

<template>
  <section class="panel dash-card changes-chart" data-testid="changes-chart">
    <div class="panel-header">
      <h2>{{ t(`dashboard.changes.title.${period}`) }}</h2>
      <ul class="chart-legend">
        <li v-for="s in SERIES" :key="s"><span :class="['legend-key', `s-${s}`]" aria-hidden="true" />{{ t(`histogram.series.${s}`) }}</li>
      </ul>
    </div>
    <div class="panel-body">
      <ErrorAlert v-if="query.isError.value" :error="query.error.value" :on-retry="() => query.refetch()" />
      <LoadingState v-else-if="!query.data.value" />
      <template v-else>
        <div ref="plot" class="changes-plot" role="img" :aria-label="summary">
          <svg width="100%" :height="PLOT_H" :viewBox="`0 0 ${Math.max(1, width)} ${PLOT_H}`" preserveAspectRatio="none" aria-hidden="true" focusable="false">
            <line class="histogram-baseline" x1="0" :x2="width" :y1="PLOT_H - 0.5" :y2="PLOT_H - 0.5" />
            <g v-for="c in columns" :key="c.index">
              <title>{{ label(buckets[c.index]!) }}: {{ formatNumber(bucketTotal(buckets[c.index]!)) }}</title>
              <rect class="changes-slot" :x="c.slotX" y="0" :width="c.slotWidth" :height="PLOT_H" />
              <path v-for="s in c.segments" :key="s.series" :class="['histogram-bar', `s-${s.series}`]" :d="segmentPath(c.x, s.y, c.width, s.height, s.top)" />
            </g>
          </svg>
        </div>
        <div class="changes-axis" aria-hidden="true">
          <span v-for="(tick, i) in ticks" :key="i">{{ tick }}</span>
        </div>
        <div class="changes-foot">
          <span class="muted">{{ bucket === "day" ? t("histogram.utc") : "" }}</span>
          <button type="button" class="btn btn-sm btn-ghost" :aria-expanded="showTable" aria-controls="changes-table" @click="showTable = !showTable">
              {{ showTable ? t("histogram.table.hide") : t("histogram.table.show") }}
          </button>
        </div>
        <table v-if="showTable" id="changes-table" class="data histogram-table" :aria-label="t('histogram.table.label')">
          <thead>
            <tr>
              <th scope="col">{{ t("histogram.col.time") }}</th>
              <th v-for="s in SERIES" :key="s" scope="col" class="num">{{ t(`histogram.series.${s}`) }}</th>
              <th scope="col" class="num">{{ t("histogram.col.total") }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="b in buckets" :key="b.start">
              <th scope="row">{{ label(b) }}</th>
              <td v-for="s in SERIES" :key="s" class="num">{{ formatNumber(b[s]) }}</td>
              <td class="num">{{ formatNumber(bucketTotal(b)) }}</td>
            </tr>
          </tbody>
        </table>
      </template>
    </div>
  </section>
</template>
