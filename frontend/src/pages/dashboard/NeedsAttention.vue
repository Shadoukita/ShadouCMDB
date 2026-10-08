<script setup lang="ts">
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { useDataQuality, type DataQualityCheck } from "../../api/queries";
import ErrorAlert from "../../components/ErrorAlert.vue";
import SkeletonRows from "../../components/SkeletonRows.vue";
import { formatNumber, t } from "../../i18n";
import { brandVariables } from "../../lib/brandColors";
import { QUALITY_CHECKS } from "../../lib/inventoryQuery";
import { useBrandingStore } from "../../stores/branding";
import { useSessionStore } from "../../stores/session";

/**
 * "Needs attention" (design §0 step 12e, gap G3): the data-quality checks in the dark panel family (§0.3), one
 * row per check with its count, each a link to the inventory filtered to the CIs it finds. The server counts
 * only the CIs the caller may view. A check that cannot find anything for this caller (no class they may view
 * names an owner or end-of-life field) is left out rather than shown as a reassuring 0; a data-model
 * administrator gets a line saying how many are off.
 */
const session = useSessionStore();
const branding = useBrandingStore();
/** The branding's dark-theme colours (focus ring), as the page sets them on <html> for its own theme. */
const brand = computed(() => brandVariables(branding.effective.primaryColor, branding.effective.accentColor, "dark"));
const quality = useDataQuality();

/** The checks this release knows, in the server's order; a newer server's extra checks have no texts yet. */
const known = (c: DataQualityCheck) => (QUALITY_CHECKS as readonly string[]).includes(c.key);
const checks = computed(() => (quality.data.value?.checks ?? []).filter(known));
const rows = computed(() =>
  checks.value
    .filter((c) => c.configured)
    .map((c) => {
      const days = c.filter.endOfLifeWithinDays ?? undefined;
      return {
        key: c.key,
        count: c.count,
        days: days ?? 0,
        to: { path: "/cis", query: { quality: c.filter.quality, ...(days !== undefined ? { endOfLifeWithinDays: String(days) } : {}) } },
      };
    }),
);
const off = computed(() => checks.value.filter((c) => !c.configured).length);
</script>

<template>
  <section class="attention" data-theme-scope="dark" :style="brand" aria-labelledby="attention-title" data-testid="needs-attention">
    <div class="attention-head">
      <h2 id="attention-title">{{ t("dashboard.attention.title") }}</h2>
      <span v-if="quality.data.value" class="attention-n">{{ t("dashboard.attention.checks", { n: rows.length }) }}</span>
    </div>
    <ErrorAlert v-if="quality.isError.value" :error="quality.error.value" :on-retry="() => quality.refetch()" />
    <SkeletonRows v-else-if="quality.isPending.value" :label="t('common.loading')" :rows="4" />
    <template v-else>
      <ul class="attention-list">
        <li v-for="r in rows" :key="r.key" :data-check="r.key">
          <RouterLink :to="r.to" class="attention-row">
            <span :class="['attention-count', r.count === 0 ? 'tone-none' : `tone-${r.key}`]">{{ formatNumber(r.count) }}</span>
            <span class="attention-text">
              <span class="attention-title">{{ t(`dashboard.attention.${r.key}.title`, { days: r.days }) }}</span>
              <span class="attention-desc">{{ t(`dashboard.attention.${r.key}.body`, { days: r.days }) }}</span>
            </span>
          </RouterLink>
        </li>
      </ul>
      <p v-if="off > 0 && session.can('datamodel.manage')" class="attention-off">{{ t("dashboard.attention.off", { n: off }) }}</p>
    </template>
  </section>
</template>
