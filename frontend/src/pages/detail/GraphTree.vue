<script setup lang="ts">
import { computed } from "vue";
import type { RelationshipGraph } from "../../api/queries";
import CiLink from "../../components/CiLink.vue";
import CiStateBadge from "../../components/CiStateBadge.vue";
import type { TrailStep } from "../../lib/trail";

type Node = RelationshipGraph["nodes"][number];
type Edge = RelationshipGraph["edges"][number];
type Direction = "both" | "outgoing" | "incoming";

interface Row {
  key: string;
  label: string;
  node: Node;
  level: number;
  repeat: boolean;
}

/**
 * The graph as an indented tree (App → runs on → VM → runs on → Server → is
 * located in → Rack). Flattened depth-first into rows; a CI reached twice is
 * shown once and marked "(shown above)" instead of expanding again.
 */
const props = defineProps<{ graph: RelationshipGraph; rootId: string; direction: Direction; self: TrailStep; trail: TrailStep[] }>();

const root = computed(() => props.graph.nodes.find((n) => n.id === props.rootId));
const rows = computed<Row[]>(() => {
  const nodes = new Map(props.graph.nodes.map((n) => [n.id, n]));
  const adjacency = new Map<string, { edge: Edge; otherId: string; label: string }[]>();
  const add = (from: string, item: { edge: Edge; otherId: string; label: string }) =>
    adjacency.set(from, [...(adjacency.get(from) ?? []), item]);
  for (const e of props.graph.edges) {
    if (props.direction !== "incoming") add(e.sourceCiId, { edge: e, otherId: e.targetCiId, label: e.type.forwardLabel });
    if (props.direction !== "outgoing")
      add(e.targetCiId, { edge: e, otherId: e.sourceCiId, label: e.type.isDirectional ? e.type.reverseLabel : e.type.forwardLabel });
  }
  const out: Row[] = [];
  const seen = new Set<string>([props.rootId]);
  const walk = (id: string, level: number) => {
    for (const { edge, otherId, label } of adjacency.get(id) ?? []) {
      const node = nodes.get(otherId);
      if (!node) continue;
      const repeat = seen.has(otherId);
      seen.add(otherId);
      out.push({ key: `${edge.id}-${otherId}-${out.length}`, label, node, level, repeat });
      if (!repeat) walk(otherId, level + 1);
    }
  };
  walk(props.rootId, 0);
  return out;
});
</script>

<template>
  <div>
    <div v-if="root" style="font-weight: 600; margin-bottom: 4px">
      <bdi>{{ root.label }}</bdi> <span class="muted">(<bdi>{{ root.class.name }}</bdi>)</span>
    </div>
    <ul style="list-style: none; margin: 0; padding: 0">
      <li v-for="r in rows" :key="r.key" :style="{ padding: '3px 0', paddingLeft: `${r.level * 22}px` }">
        <span class="muted"><bdi>{{ r.label }}</bdi> → </span>
        <CiLink :id="r.node.id" :from="self" :trail="trail">{{ r.node.label }}</CiLink>
        {{ " " }}<span class="muted" dir="auto">{{ r.node.class.name }}</span> <CiStateBadge :ci="r.node" />
        <span v-if="r.repeat" class="muted"> (shown above)</span>
      </li>
    </ul>
  </div>
</template>
