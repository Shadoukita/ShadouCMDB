<script setup lang="ts">
import { ref } from "vue";
import { useGraph, type Ci } from "../../api/queries";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import { plural } from "../../lib/format";
import type { TrailStep } from "../../lib/trail";
import GraphTree from "./GraphTree.vue";

/**
 * Multi-hop view from GET /configuration-items/{id}/graph. Every node is a link,
 * so the operator can keep walking.
 */
const props = defineProps<{ ci: Ci; self: TrailStep; trail: TrailStep[] }>();
const depth = ref(3);
const direction = ref<"both" | "outgoing" | "incoming">("outgoing");
const graph = useGraph(() => props.ci.id, depth, direction);
const hops = (d: number) => plural(d, "hop");
</script>

<template>
  <section class="panel">
    <div class="toolbar">
      <div class="field">
        <label for="g-dir">Direction</label>
        <select id="g-dir" v-model="direction" style="width: 260px">
          <option value="outgoing">Outgoing — what this CI needs</option>
          <option value="incoming">Incoming — what needs this CI</option>
          <option value="both">Both directions</option>
        </select>
      </div>
      <div class="field">
        <label for="g-depth">Depth</label>
        <select id="g-depth" v-model.number="depth">
          <option v-for="d in [1, 2, 3, 4, 5, 6]" :key="d" :value="d">{{ hops(d) }}</option>
        </select>
      </div>
      <span v-if="graph.isFetching.value && !graph.isLoading.value" class="spinner" aria-label="Refreshing" />
    </div>
    <LoadingState v-if="graph.isLoading.value" label="Walking the relationship graph…" />
    <div v-if="graph.isError.value" class="panel-body">
      <ErrorAlert :error="graph.error.value" :on-retry="() => graph.refetch()" />
    </div>
    <EmptyState v-if="graph.data.value && graph.data.value.edges.length === 0" title="Nothing connected in this direction">
      Try “Both directions”, or add relationships on the Overview tab.
    </EmptyState>
    <div v-if="graph.data.value && graph.data.value.edges.length > 0" class="panel-body">
      <div v-if="graph.data.value.truncated" class="alert alert-warn">
        The graph was truncated by the API's node limit. Reduce the depth for a complete picture.
      </div>
      <GraphTree :graph="graph.data.value" :root-id="ci.id" :direction="direction" :self="self" :trail="trail" />
      <p class="muted" style="margin-top: var(--sp-4)">
        {{ graph.data.value.nodes.length }} CIs, {{ graph.data.value.edges.length }} relationships within {{ hops(depth) }}.
      </p>
    </div>
  </section>
</template>
