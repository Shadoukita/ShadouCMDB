<script setup lang="ts">
import { computed } from "vue";
import { useChangeHistogram, useCompleteness, useCountHistory } from "../../api/queries";
import { formatNumber, t } from "../../i18n";
import { formatDateTime } from "../../lib/format";
import {
  barHeights,
  changeWindows,
  completePercent,
  countDelta,
  countWindow,
  group,
  percentChange,
  type Period,
} from "../../lib/dashboard";
import { bucketTotal } from "../../lib/changeHistogram";
import { useSessionStore } from "../../stores/session";

/**
 * The KPI cards (design §0 step 12e): CIs and relationships with their move over the period and their count
 * per day or week (G2), the changes in the period against the one before, and the records complete (G1).
 * A card shows only what is known: one the caller may not see (changes without audit.view) or whose request
 * failed is left out, never shown as 0. The bars are decoration; the chip carries the reading in text.
 */
const props = defineProps<{ period: Period; now: number }>();
const session = useSessionStore();

const counts = computed(() => countWindow(props.period, props.now));
const cis = useCountHistory("cis", counts);
const rels = useCountHistory("relationships", counts);

const canAudit = computed(() => session.can("audit.view"));
const windows = computed(() => changeWindows(props.period, props.now));
const changes = useChangeHistogram(() => windows.value.current, canAudit);
const previous = useChangeHistogram(() => windows.value.previous, canAudit);

const completeness = useCompleteness();

type Tone = "ok" | "warn" | "neutral";
interface Card {
  key: string;
  label: string;
  value?: string;
  delta?: { text: string; tone: Tone; title: string };
  bars?: number[];
  meter?: number;
  note?: string;
  pending: boolean;
}

function signed(n: number, suffix = ""): string {
  if (n === 0) return `±0${suffix}`;
  return `${n > 0 ? "+" : "−"}${formatNumber(Math.abs(n))}${suffix}`;
}

function countCard(key: string, label: string, q: typeof cis): Card | null {
  if (q.isError.value) return null;
  const d = q.data.value ? countDelta(q.data.value, props.period) : null;
  return {
    key,
    label,
    value: d ? formatNumber(d.value) : undefined,
    delta: d
      ? { text: signed(d.delta), tone: d.delta > 0 ? "ok" : "neutral", title: t("dashboard.kpi.since", { delta: signed(d.delta), since: formatDateTime(d.since) }) }
      : undefined,
    bars: q.data.value ? barHeights(q.data.value.buckets.map((b) => b.count), "range") : undefined,
    pending: q.isPending.value,
  };
}

const cards = computed<Card[]>(() => {
  const out: (Card | null)[] = [countCard("total", t("dashboard.stat.total"), cis), countCard("relationships", t("dashboard.stat.relationships"), rels)];
  if (canAudit.value && !changes.isError.value) {
    const cur = changes.data.value;
    const prev = previous.data.value;
    const pct = cur && prev ? percentChange(cur.total, prev.total) : null;
    out.push({
      key: "changes",
      label: t("dashboard.stat.changes"),
      value: cur ? formatNumber(cur.total) : undefined,
      delta: pct === null ? undefined : { text: signed(pct, "%"), tone: "neutral", title: t("dashboard.kpi.vsPrevious", { pct: signed(pct, "%") }) },
      bars: cur ? barHeights(group(cur.buckets.map(bucketTotal)), "zero") : undefined,
      pending: changes.isPending.value,
    });
  }
  if (!completeness.isError.value) {
    const c = completeness.data.value?.overall;
    const pct = c ? completePercent(c.items, c.completeItems) : undefined;
    out.push({
      key: "complete",
      label: t("dashboard.stat.complete"),
      value: pct === undefined ? undefined : t("dashboard.share.pct", { pct: formatNumber(pct) }),
      meter: pct,
      note: c ? t("dashboard.stat.complete.note", { complete: formatNumber(c.completeItems), items: formatNumber(c.items) }) : undefined,
      pending: completeness.isPending.value,
    });
  }
  return out.filter((c): c is Card => c !== null);
});
</script>

<template>
  <section class="kpi-grid" :aria-label="t('dashboard.stats')" data-testid="dashboard-stats">
    <div v-for="card in cards" :key="card.key" class="kpi" :data-stat="card.key" :aria-busy="card.pending">
      <span class="label">{{ card.label }}</span>
      <span class="kpi-figure">
        <span class="value">
          <span v-if="card.pending" class="skeleton-line" aria-hidden="true" />
          <template v-else>{{ card.value }}</template>
        </span>
        <span v-if="!card.pending && card.delta" :class="['delta-chip', `tone-${card.delta.tone}`]" :title="card.delta.title">
          <span aria-hidden="true">{{ card.delta.text }}</span>
          <span class="sr-only">{{ card.delta.title }}</span>
        </span>
      </span>
      <span v-if="card.bars && card.bars.length" class="kpi-bars" aria-hidden="true">
        <span v-for="(h, i) in card.bars" :key="i" :class="{ current: i === card.bars.length - 1 }" :style="{ height: `${h}%` }" />
      </span>
      <span v-else-if="card.meter !== undefined" class="kpi-meter" aria-hidden="true"><span :style="{ width: `${card.meter}%` }" /></span>
      <span v-if="!card.pending && card.note" class="note">{{ card.note }}</span>
    </div>
  </section>
</template>
