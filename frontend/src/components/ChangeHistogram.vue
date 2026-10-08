<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useChangeHistogram, type ChangeHistogramQuery } from "../api/queries";
import { formatNumber, t, type MessageKey } from "../i18n";
import {
  RANGES,
  SERIES,
  bucketTotal,
  dayWindow,
  formatBucket,
  indexAt,
  layout,
  niceMax,
  peak,
  rangeWindow,
  readPref,
  segmentPath,
  writePref,
  type HistogramBucket,
  type HistogramPref,
  type Range,
} from "../lib/changeHistogram";
import Icon from "./Icon.vue";

/**
 * The inventory's change histogram (design document §2.7): an optional strip above the table with
 * the changes per hour or day to the CIs the current filters match, stacked into updated, created
 * and status changed (lib/changeHistogram). The API counts; the strip only draws. Clicking a day's
 * bar, or Enter on it, shows that day by hour. Whether the strip is open and its range are a
 * per-browser choice. The plot is one tab stop: a slider whose value text reads the bucket under
 * it, so every count is reachable from the keyboard, and "Show values" lists them as a table.
 */
const props = defineProps<{
  /** The list's filters (GET /configuration-items without sort and paging). */
  filters: Omit<ChangeHistogramQuery, "from" | "to" | "bucket">;
  /** Set when the API cannot count the list's set (a filter it does not take): the strip says why and sends no request. */
  unavailable?: string;
}>();

const PLOT_H = 80;

const pref = ref<HistogramPref>(readPref());
watch(pref, (p) => writePref(p), { deep: true });
const zoomDay = ref<string | null>(null);
const showTable = ref(false);

// The window moves with the clock: re-read it every minute, so a new hour or day joins the plot.
const now = ref(Date.now());
let clock: ReturnType<typeof setInterval> | undefined;
onMounted(() => (clock = setInterval(() => (now.value = Date.now()), 60_000)));
onBeforeUnmount(() => clearInterval(clock));

const win = computed(() => (zoomDay.value ? dayWindow(zoomDay.value, now.value) : rangeWindow(pref.value.range, now.value)));
const query = useChangeHistogram(
  () => ({ ...props.filters, ...win.value }),
  () => pref.value.open && !props.unavailable,
);
// A changed filter starts again from the range: the zoomed day may not mean much for the new set.
watch(
  () => JSON.stringify(props.filters),
  () => (zoomDay.value = null),
);

const data = computed(() => query.data.value);
const buckets = computed<HistogramBucket[]>(() => data.value?.buckets ?? []);
const bucket = computed(() => data.value?.bucket ?? win.value.bucket);
const max = computed(() => niceMax(Math.max(0, ...buckets.value.map(bucketTotal))));
const top = computed(() => peak(buckets.value));

// ---------- Plot size ----------
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

// ---------- The bucket under the pointer or the keyboard ----------
const active = ref(-1);
watch(buckets, (b) => {
  if (active.value >= b.length) active.value = b.length - 1;
});
const current = computed(() => (active.value >= 0 ? buckets.value[active.value] : undefined));

const readoutOf = (b: HistogramBucket) => t("histogram.readout", { updated: b.updated, created: b.created, status: b.statusChanged });
const label = (b: HistogramBucket) => formatBucket(b.start, bucket.value);
const valueText = computed(() => {
  const b = current.value ?? buckets.value[buckets.value.length - 1];
  return b ? `${label(b)}: ${readoutOf(b)}` : "";
});

function onPointerMove(e: PointerEvent | MouseEvent) {
  const rect = plot.value?.getBoundingClientRect();
  if (rect) active.value = indexAt(e.clientX - rect.left, rect.width, buckets.value.length);
}
// Taken from the click itself: a tap has no pointer movement before it.
function onClick(e: MouseEvent) {
  onPointerMove(e);
  zoom(active.value);
}
function onPointerLeave() {
  if (document.activeElement !== plot.value) active.value = -1;
}
function onFocus() {
  if (active.value < 0) active.value = buckets.value.length - 1;
}
function onBlur() {
  active.value = -1;
}
function zoom(i: number) {
  const b = buckets.value[i];
  if (!b || bucket.value !== "day" || zoomDay.value) return;
  zoomDay.value = b.start;
  active.value = -1;
}
function onKeydown(e: KeyboardEvent) {
  const n = buckets.value.length;
  if (!n) return;
  const at = active.value < 0 ? n - 1 : active.value;
  const next = { ArrowLeft: at - 1, ArrowRight: at + 1, ArrowDown: at - 1, ArrowUp: at + 1, Home: 0, End: n - 1, PageUp: at - 10, PageDown: at + 10 }[e.key];
  if (next !== undefined) {
    e.preventDefault();
    active.value = Math.min(n - 1, Math.max(0, next));
  } else if (e.key === "Enter") {
    e.preventDefault();
    zoom(at);
  }
}

// ---------- Text ----------
const rangeName = (r: Range) => t(`histogram.rangeName.${r}` as MessageKey);
const summary = computed(() => {
  const d = data.value;
  if (!d) return "";
  const base = zoomDay.value
    ? t("histogram.summary.day", { n: d.total, day: formatBucket(zoomDay.value, "day") })
    : t(`histogram.summary.${pref.value.range}` as MessageKey, { n: d.total });
  return top.value ? `${base} · ${t(`histogram.peak.${bucket.value}` as MessageKey, { n: bucketTotal(top.value), time: label(top.value) })}` : base;
});
const rows = computed(() => buckets.value.filter((b) => bucketTotal(b) > 0));

function setRange(r: Range) {
  pref.value.range = r;
  zoomDay.value = null;
  active.value = -1;
}
function toggleOpen() {
  pref.value.open = !pref.value.open;
  if (!pref.value.open) showTable.value = false;
}
</script>

<template>
  <section class="histogram" :class="{ open: pref.open }" aria-labelledby="histogram-title">
    <div class="histogram-head">
      <h2 class="histogram-title">
        <button id="histogram-title" type="button" class="histogram-toggle" :aria-expanded="pref.open" aria-controls="histogram-body" @click="toggleOpen">
          <Icon :name="pref.open ? 'chevron-down' : 'chevron-right'" />{{ t("histogram.title") }}
        </button>
      </h2>
      <template v-if="pref.open && !unavailable">
        <span id="histogram-summary" class="histogram-summary">{{ summary }}</span>
        <ul class="histogram-legend" :aria-label="t('histogram.legend')">
          <li v-for="s in SERIES" :key="s"><span :class="['histogram-swatch', `s-${s}`]" aria-hidden="true" />{{ t(`histogram.series.${s}`) }}</li>
        </ul>
        <button v-if="zoomDay" type="button" class="btn btn-sm btn-ghost" @click="setRange(pref.range)">
          <Icon name="arrow-left" />{{ t("histogram.back", { range: rangeName(pref.range) }) }}
        </button>
        <div v-else class="segmented" role="radiogroup" :aria-label="t('histogram.range')">
          <label v-for="r in RANGES" :key="r">
            <input class="sr-only" type="radio" name="histogram-range" :value="r" :checked="r === pref.range" @change="setRange(r)" />{{ t(`histogram.range.${r}`) }}
          </label>
        </div>
        <button v-if="rows.length" type="button" class="btn btn-sm btn-ghost" :aria-expanded="showTable" aria-controls="histogram-table" @click="showTable = !showTable">
          {{ showTable ? t("histogram.table.hide") : t("histogram.table.show") }}
        </button>
      </template>
    </div>

    <div v-if="pref.open" id="histogram-body" class="histogram-body">
      <p v-if="unavailable" class="histogram-unavailable" data-testid="histogram-unavailable">
        <Icon name="info" />{{ unavailable }}
      </p>
      <p v-else-if="query.isError.value" class="histogram-error">
        <Icon name="circle-alert" />{{ t("histogram.error") }}
        <button type="button" class="btn btn-sm" @click="query.refetch()">{{ t("common.retry") }}</button>
      </p>
      <div v-else-if="!data" class="histogram-plot histogram-loading" role="status" :aria-label="t('histogram.loading')" />
      <template v-else>
        <div
          ref="plot"
          :class="['histogram-plot', { zoomable: bucket === 'day' && !zoomDay, stale: query.isPlaceholderData.value }]"
          role="slider"
          tabindex="0"
          :aria-label="t(`histogram.label.${bucket}`)"
          aria-valuemin="0"
          :aria-valuemax="Math.max(0, buckets.length - 1)"
          :aria-valuenow="active >= 0 ? active : Math.max(0, buckets.length - 1)"
          :aria-valuetext="valueText"
          aria-describedby="histogram-summary histogram-keys"
          @pointermove="onPointerMove"
          @pointerleave="onPointerLeave"
          @click="onClick"
          @focus="onFocus"
          @blur="onBlur"
          @keydown="onKeydown"
        >
          <!-- Fluid width, drawn in px at the measured width: a narrowing window never waits on the observer. -->
          <svg width="100%" height="80" :viewBox="`0 0 ${Math.max(1, width)} 80`" preserveAspectRatio="none" aria-hidden="true" focusable="false">
            <rect v-if="current && columns[active]" class="histogram-hover" :x="columns[active]!.slotX" y="0" :width="columns[active]!.slotWidth" height="80" />
            <line class="histogram-baseline" x1="0" :x2="width" y1="79.5" y2="79.5" />
            <g v-for="c in columns" :key="c.index">
              <path v-for="s in c.segments" :key="s.series" :class="['histogram-bar', `s-${s.series}`]" :d="segmentPath(c.x, s.y, c.width, s.height, s.top)" />
            </g>
          </svg>
          <span class="histogram-max" aria-hidden="true">{{ formatNumber(max) }}</span>
        </div>
        <div class="histogram-axis" aria-hidden="true">
          <span>{{ buckets[0] ? label(buckets[0]) : "" }}</span>
          <span v-if="current" class="histogram-readout">{{ valueText }}</span>
          <span v-else-if="bucket === 'day'">{{ t("histogram.utc") }}</span>
          <span>{{ buckets.length ? label(buckets[buckets.length - 1]!) : "" }}</span>
        </div>
        <span id="histogram-keys" class="sr-only">{{ t(bucket === "day" && !zoomDay ? "histogram.keys.day" : "histogram.keys.hour") }}</span>
        <table v-if="showTable && rows.length" id="histogram-table" class="data histogram-table" :aria-label="t('histogram.table.label')">
          <thead>
            <tr>
              <th scope="col">{{ t("histogram.col.time") }}</th>
              <th v-for="s in SERIES" :key="s" scope="col" class="num">{{ t(`histogram.series.${s}`) }}</th>
              <th scope="col" class="num">{{ t("histogram.col.total") }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="b in rows" :key="b.start">
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
