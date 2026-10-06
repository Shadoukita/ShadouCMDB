<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, useId } from "vue";
import { useCiClasses } from "../../api/queries";
import CiLink from "../../components/CiLink.vue";
import ClassBadge from "../../components/ClassBadge.vue";
import { t } from "../../i18n";
import { ciStateParts } from "../../lib/ciState";
import { isHostLike } from "../../lib/format";
import { layoutTopology, type PlacedEdge, type PlacedNode, type Topology } from "../../lib/topology";
import type { TrailStep } from "../../lib/trail";

/**
 * The topology as a picture (design §2.7): node boxes in columns around the CI, edges between them,
 * solid where the relationship propagates impact and dashed where it does not. For assistive
 * technology it is one image (the edge layer, named by `summary`); the tree under it (GraphTree) is
 * the same content as text and the keyboard path, so the boxes are links for the mouse only, in a
 * layer of their own that is hidden from assistive technology and out of the tab order.
 */
const props = defineProps<{ topology: Topology; summary: string; self: TrailStep; trail: TrailStep[] }>();
const classes = useCiClasses();
const cls = (id: string) => classes.data.value?.find((k) => k.id === id);

/** Narrower than this, the columns get too tight to read: the list alone is shown. */
const MIN_WIDTH = 560;
const box = ref<HTMLElement | null>(null);
const width = ref(0);
let observer: ResizeObserver | undefined;
onMounted(() => {
  // On the next frame: the canvas's own height can bring in a scrollbar and change the width it was measured at.
  observer = new ResizeObserver(([entry]) => {
    const w = Math.floor(entry.contentRect.width);
    requestAnimationFrame(() => (width.value = w));
  });
  if (box.value) observer.observe(box.value);
});
onBeforeUnmount(() => observer?.disconnect());

const layout = computed(() => (width.value >= MIN_WIDTH ? layoutTopology(props.topology, width.value) : null));
const arrow = `topology-arrow-${useId()}`;

/** A horizontal S-curve from the parent's facing side to the child's. */
function path(e: PlacedEdge): string {
  const dx = (e.x2 - e.x1) / 2;
  return `M ${e.x1} ${e.y1} C ${e.x1 + dx} ${e.y1}, ${e.x2 - dx} ${e.y2}, ${e.x2} ${e.y2}`;
}
/** The arrow sits at the relationship's target: the child when the parent is the source. */
const markerEnd = (e: PlacedEdge) => (e.directional && e.outward ? `url(#${arrow})` : undefined);
const markerStart = (e: PlacedEdge) => (e.directional && !e.outward ? `url(#${arrow})` : undefined);
const labelStyle = (e: PlacedEdge) => ({
  left: `${(e.x1 + e.x2) / 2}px`,
  top: `${(e.y1 + e.y2) / 2}px`,
  maxWidth: `${Math.max(48, Math.abs(e.x2 - e.x1) - 12)}px`,
});
const boxStyle = (n: { x: number; y: number }) => ({ left: `${n.x}px`, top: `${n.y}px`, width: `${layout.value?.boxWidth}px` });

/** The node's state in words when it is not simply active: "Inactive", "Deleted". */
const stateWord = (n: PlacedNode) => ciStateParts(n).find((p) => p.tone === "danger" || p.tone === "off")?.text;
const tone = (n: PlacedNode) => (n.deletedAt ? "danger" : n.active ? "ok" : "off");
</script>

<template>
  <div ref="box" class="topology-canvas">
    <div v-if="layout" class="topology-stage" :style="{ height: `${layout.height}px` }">
      <svg class="topology-edges" role="img" :aria-label="summary" :width="layout.width" :height="layout.height" :viewBox="`0 0 ${layout.width} ${layout.height}`">
        <defs>
          <marker :id="arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
            <path d="M 0 1 L 9 5 L 0 9 z" class="topology-arrow" />
          </marker>
        </defs>
        <path
          v-for="e in layout.edges"
          :key="`${e.from}>${e.to}`"
          :d="path(e)"
          :class="['topology-edge', { dashed: e.dashed }]"
          :marker-end="markerEnd(e)"
          :marker-start="markerStart(e)"
        />
      </svg>
      <div class="topology-nodes" aria-hidden="true">
        <span v-for="e in layout.edges" :key="`l-${e.from}>${e.to}`" class="topology-edge-label" :style="labelStyle(e)" :title="e.label" dir="auto">{{ e.label }}</span>
        <template v-for="n in layout.nodes" :key="n.id">
          <div v-if="n.hops === 0" class="topology-node root" :style="boxStyle(n)">
            <span class="topology-name">
              <ClassBadge :icon="cls(n.classId)?.icon" :color="cls(n.classId)?.color" />
              <bdi :class="{ mono: isHostLike(n.label) }">{{ n.label }}</bdi>
            </span>
            <span class="topology-sub"><bdi>{{ n.className }}</bdi> · {{ t("topology.thisCi") }}</span>
          </div>
          <CiLink v-else :id="n.id" :class="['topology-node', tone(n)]" :style="boxStyle(n)" :from="self" :trail="trail" tabindex="-1" :title="n.label">
            <span class="topology-name">
              <ClassBadge :icon="cls(n.classId)?.icon" :color="cls(n.classId)?.color" />
              <bdi :class="{ mono: isHostLike(n.label) }">{{ n.label }}</bdi>
            </span>
            <span class="topology-sub">
              <span :class="['status-dot', tone(n)]" />
              <bdi>{{ n.className }}</bdi><template v-if="stateWord(n)"> · {{ stateWord(n) }}</template>
            </span>
          </CiLink>
        </template>
        <div v-for="m in layout.more" :key="`more-${m.column}`" class="topology-node more" :style="boxStyle(m)">
          {{ t("topology.more", { count: m.count }) }}
        </div>
      </div>
    </div>
    <p v-else-if="width > 0" class="topology-narrow">{{ t("topology.narrow") }}</p>
  </div>
</template>
