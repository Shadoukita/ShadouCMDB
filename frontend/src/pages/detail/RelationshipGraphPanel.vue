<script setup lang="ts">
import { computed, ref } from "vue";
import { RouterLink } from "vue-router";
import { useRelTypeList } from "../../api/datamodel";
import { useGraph, useImpact, useImpactSettings, type Ci } from "../../api/queries";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import { t } from "../../i18n";
import { graphRows, impactRows } from "../../lib/graphTree";
import { DEFAULT_STATE, impactParams, impactTree, truncationMessage } from "../../lib/impact";
import { topologyFromGraph, topologyFromImpact } from "../../lib/topology";
import type { TrailStep } from "../../lib/trail";
import GraphTree from "./GraphTree.vue";
import TopologyCanvas from "./TopologyCanvas.vue";

/**
 * The Relationship map tab: the topology panel (design §2.7). 1 hop and 2 hops walk
 * GET /configuration-items/{id}/graph in the chosen direction; Impact shows what the CI takes down
 * (GET …/impact, downstream), or on a business service what can take it down (upstream). The canvas is the picture, the tree under it the same CIs as text,
 * where every node is a link and has its "Analyse impact" action.
 */
const props = defineProps<{ ci: Ci; self: TrailStep; trail: TrailStep[]; impactDirection?: "downstream" | "upstream" }>();
type Range = "1" | "2" | "impact";
const RANGES: Range[] = ["1", "2", "impact"];
const DIRECTIONS = ["both", "outgoing", "incoming"] as const;
/** How far the Impact view walks: deep enough to see the knock-on effect, shallow enough to draw. */
const IMPACT_DEPTH = 2;

const range = ref<Range>("1");
const direction = ref<(typeof DIRECTIONS)[number]>("both");
const depth = computed(() => (range.value === "2" ? 2 : 1));
const onImpact = computed(() => range.value === "impact");

const graph = useGraph(() => props.ci.id, depth, direction, () => !onImpact.value);
const relTypes = useRelTypeList();
const propagates = computed(() => {
  const types = new Map((relTypes.data.value?.data ?? []).map((ty) => [ty.id, ty.impactDirection !== "none"]));
  return (id: string) => types.get(id);
});

const up = computed(() => props.impactDirection === "upstream");
const impactSettings = useImpactSettings();
const notConfigured = computed(() => impactSettings.data.value?.anyTypePropagates === false);
const impact = useImpact(() => props.ci.id, () => impactParams({ ...DEFAULT_STATE, direction: up.value ? "upstream" : "downstream", depth: IMPACT_DEPTH }), () => onImpact.value && impactSettings.isFetched.value && !notConfigured.value);

const query = computed(() => (onImpact.value ? impact : graph));
const loading = computed(() => (onImpact.value ? !notConfigured.value && (impact.isLoading.value || !impactSettings.isFetched.value) : graph.isLoading.value));
const topology = computed(() => {
  if (onImpact.value) return impact.data.value ? topologyFromImpact(impact.data.value, props.ci.classId) : null;
  return graph.data.value ? topologyFromGraph(graph.data.value, propagates.value) : null;
});
const rows = computed(() => {
  if (onImpact.value) return impact.data.value ? impactRows(impactTree(impact.data.value)[0]?.rows ?? []) : [];
  return graph.data.value ? graphRows(graph.data.value, props.ci.id, direction.value) : [];
});
const related = computed(() => Math.max(0, (topology.value?.nodes.length ?? 1) - 1));
const hops = computed(() => (onImpact.value ? IMPACT_DEPTH : depth.value));
const meta = computed(() =>
  t(onImpact.value ? (up.value ? "topology.meta.upstream" : "topology.meta.impact") : "topology.meta", { count: related.value, hops: hops.value }),
);
const summary = computed(() =>
  t(onImpact.value ? (up.value ? "topology.summary.upstream" : "topology.summary.impact") : "topology.summary", { name: props.ci.label, count: related.value, hops: hops.value }),
);
const truncated = computed(() => {
  if (onImpact.value) return impact.data.value?.truncated ? truncationMessage(impact.data.value, impactSettings.data.value?.timeoutMs) : null;
  return graph.data.value?.truncated ? t("topology.truncated") : null;
});
const hasDashed = computed(() => !!topology.value?.edges.some((e) => e.dashed));
</script>

<template>
  <section class="panel topology" aria-labelledby="topology-title">
    <div class="panel-header">
      <div class="topology-heading">
        <h2 id="topology-title">{{ t("topology.title") }}</h2>
        <span v-if="topology" class="meta">{{ meta }}</span>
        <span v-if="query.isFetching.value && !loading" class="spinner" :aria-label="t('topology.refreshing')" />
      </div>
      <div class="topology-controls">
        <div v-if="!onImpact" class="topology-direction">
          <label for="g-dir">{{ t("topology.direction") }}</label>
          <select id="g-dir" v-model="direction">
            <option v-for="d in DIRECTIONS" :key="d" :value="d">{{ t(`topology.direction.${d}`) }}</option>
          </select>
        </div>
        <div class="segmented" role="radiogroup" :aria-label="t('topology.range')">
          <label v-for="r in RANGES" :key="r">
            <input v-model="range" type="radio" name="topology-range" :value="r" />{{ t(`topology.range.${r}`) }}
          </label>
        </div>
      </div>
    </div>

    <EmptyState v-if="onImpact && notConfigured" :title="t('topology.impact.notConfigured')">
      <RouterLink :to="`/cis/${ci.id}/impact`">{{ t("topology.impact.open") }}</RouterLink>
    </EmptyState>
    <LoadingState v-else-if="loading" :label="t('topology.loading')" />
    <div v-else-if="query.isError.value" class="panel-body">
      <ErrorAlert :error="query.error.value" :on-retry="() => query.refetch()" />
    </div>
    <EmptyState v-else-if="topology && related === 0" :title="onImpact ? t(up ? 'topology.empty.upstream' : 'topology.empty.impact', { name: ci.label, hops }) : t('topology.empty')">
      <template v-if="!onImpact">{{ t("topology.empty.hint") }}</template>
      <RouterLink v-else :to="`/cis/${ci.id}/impact`">{{ t("topology.impact.open") }}</RouterLink>
    </EmptyState>
    <div v-else-if="topology" class="panel-body">
      <div v-if="truncated" class="alert alert-warn" role="status">{{ truncated }}</div>
      <TopologyCanvas :topology="topology" :summary="summary" :self="self" :trail="trail" />
      <div class="topology-footer">
        <!-- Only once both kinds are drawn: without the relationship types every edge is solid. -->
        <ul v-if="hasDashed" class="topology-legend" :aria-label="t('topology.legend')">
          <li><span class="topology-swatch" aria-hidden="true" />{{ t("topology.legend.solid") }}</li>
          <li><span class="topology-swatch dashed" aria-hidden="true" />{{ t("topology.legend.dashed") }}</li>
        </ul>
        <RouterLink v-if="onImpact" :to="`/cis/${ci.id}/impact`">{{ t("topology.impact.open") }}</RouterLink>
      </div>
      <h3 class="topology-list-title">{{ t("topology.list") }}</h3>
      <GraphTree :rows="rows" :root="{ label: ci.label, className: ci.class.name, classId: ci.classId }" :label="t('topology.list')" :self="self" :trail="trail">
        <template #actions="{ row, tabindex }">
          <RouterLink class="tree-action" :to="`/cis/${row.node.id}/impact`" :tabindex="tabindex" :title="t('topology.analyse.title', { name: row.node.label })">{{
            t("topology.analyse")
          }}</RouterLink>
        </template>
      </GraphTree>
    </div>
  </section>
</template>
