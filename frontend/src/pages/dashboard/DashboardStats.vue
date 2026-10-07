<script setup lang="ts">
import { useQueries, useQuery } from "@tanstack/vue-query";
import { computed } from "vue";
import { ciCountQuery, useChangeHistogram, useCiClasses } from "../../api/queries";
import { useServiceList, useServiceSettings } from "../../api/services";
import { formatNumber, t } from "../../i18n";
import { useSessionStore } from "../../stores/session";

/**
 * The stat tiles above the dashboard's widgets (design §2.7): the CIs, their changes in the last 7 days and the
 * business services. Like the record's tiles, a tile shows only what is known: one the caller may not see (no
 * audit.view, no business services) or whose request failed is left out, never shown as 0.
 * The per-class counts share their cache with the "CIs by class" widget, so the note costs no extra request.
 */
const DAYS = 7;
const session = useSessionStore();

const total = useQuery(ciCountQuery({}));
const classes = useCiClasses();
const concrete = computed(() =>
  (classes.data.value ?? []).filter((c) => !c.isAbstract && c.kind === "asset" && session.canOnClass(c.id, "view")),
);
const classCounts = useQueries({ queries: computed(() => concrete.value.map((c) => ciCountQuery({ classId: c.id }))) });
/** Classes holding at least one CI, once every count has loaded. */
const classesInUse = computed(() =>
  classes.data.value && classCounts.value.every((q) => q.data !== undefined)
    ? classCounts.value.filter((q) => (q.data ?? 0) > 0).length
    : undefined,
);

const canAudit = computed(() => session.can("audit.view"));
// One start per mount: a moving `from` would re-key the query on every render.
const from = new Date(Date.now() - DAYS * 86_400_000).toISOString();
const changes = useChangeHistogram({ bucket: "day", from }, canAudit);
const changeSplit = computed(() => {
  const buckets = changes.data.value?.buckets ?? [];
  return {
    created: buckets.reduce((n, b) => n + b.created, 0),
    status: buckets.reduce((n, b) => n + b.statusChanged, 0),
  };
});

const serviceSettings = useServiceSettings();
const canViewServices = computed(() => !!serviceSettings.data.value?.canView);
const services = useServiceList({ limit: 1 }, canViewServices);

interface Tile {
  key: string;
  label: string;
  value?: string;
  note?: string;
  pending: boolean;
}
const tiles = computed<Tile[]>(() => {
  const out: Tile[] = [
    {
      key: "total",
      label: t("dashboard.stat.total"),
      value: total.data.value === undefined ? undefined : formatNumber(total.data.value),
      note: classesInUse.value === undefined ? undefined : t("dashboard.stat.total.note", { n: classesInUse.value }),
      pending: total.isPending.value,
    },
  ];
  if (canAudit.value && !changes.isError.value) {
    out.push({
      key: "changes",
      label: t("dashboard.stat.changes", { days: DAYS }),
      value: changes.data.value === undefined ? undefined : formatNumber(changes.data.value.total),
      note: t("dashboard.stat.changes.note", changeSplit.value),
      pending: changes.isPending.value,
    });
  }
  if (canViewServices.value && !services.isError.value) {
    out.push({
      key: "services",
      label: t("dashboard.stat.services"),
      value: services.data.value === undefined ? undefined : formatNumber(services.data.value.page.total),
      pending: services.isPending.value,
    });
  }
  return out;
});
</script>

<template>
  <section class="stat-grid" :aria-label="t('dashboard.stats')" data-testid="dashboard-stats">
    <div v-for="tile in tiles" :key="tile.key" class="stat" :data-stat="tile.key" :aria-busy="tile.pending">
      <span class="value">
        <span v-if="tile.pending" class="skeleton-line" aria-hidden="true" />
        <template v-else>{{ tile.value }}</template>
      </span>
      <span class="label">{{ tile.label }}</span>
      <span v-if="!tile.pending && tile.note" class="note">{{ tile.note }}</span>
    </div>
  </section>
</template>
