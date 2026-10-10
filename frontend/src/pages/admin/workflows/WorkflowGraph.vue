<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { categoryLabel, NODE_H, NODE_W, type Draft, type PlacedProblem, type Position } from "../../../lib/workflowDraft";
import { EDGE_LABEL_MAX, edgeGeometry, shorten } from "../../../lib/workflowGraph";

/**
 * The workflow as a diagram: states are boxes (drag them, or focus one and move it with the arrow
 * keys), transitions are arrows between them. Selecting either opens it in the inspector. Problems
 * of the lint mark the box or arrow they are about. Read-only for published versions.
 */
const props = defineProps<{
  draft: Draft;
  selected?: { kind: "state" | "transition"; key: string } | null;
  problems?: PlacedProblem[];
  readonly?: boolean;
  /** The state field's lookup values, to show a state's value by its name rather than its key. */
  stateValues?: { key: string; name: string }[] | null;
}>();
const emit = defineEmits<{
  select: [target: { kind: "state" | "transition"; key: string }];
  move: [key: string, pos: Position];
}>();

const PAD = 32;
const STEP = 8;

function pos(key: string): Position {
  return props.draft.positions[key] ?? { x: PAD, y: PAD };
}

const size = computed(() => {
  let w = 640;
  let h = 280;
  for (const s of props.draft.states) {
    const p = pos(s.key);
    w = Math.max(w, p.x + NODE_W + PAD);
    h = Math.max(h, p.y + NODE_H + PAD);
  }
  return { w, h };
});

function severity(kind: "state" | "transition", key: string): "error" | "warning" | undefined {
  const mine = (props.problems ?? []).filter((p) => p.target.kind === kind && p.target.key === key);
  if (mine.some((p) => p.severity === "error")) return "error";
  return mine.length ? "warning" : undefined;
}

const valueName = (key: string) => props.stateValues?.find((v) => v.key === key)?.name ?? key;
const nameOf = (key: string) => props.draft.states.find((s) => s.key === key)?.name ?? key;

const edges = computed(() =>
  edgeGeometry(
    props.draft.transitions.filter((t) => props.draft.states.some((s) => s.key === t.from) && props.draft.states.some((s) => s.key === t.to) && t.from !== t.to),
    pos,
    { w: NODE_W, h: NODE_H },
  ),
);

const isSelected = (kind: "state" | "transition", key: string) => props.selected?.kind === kind && props.selected.key === key;

// ---------- Overflow ----------

/** Which sides of the diagram are scrolled out of view: shaded, and named under the canvas, so a narrow panel does not hide states silently. */
const canvas = ref<HTMLElement | null>(null);
const more = ref({ left: false, right: false, down: false });
function measure() {
  const el = canvas.value;
  if (!el) return;
  more.value = {
    left: el.scrollLeft > 1,
    right: el.scrollLeft + el.clientWidth < el.scrollWidth - 1,
    down: el.scrollTop + el.clientHeight < el.scrollHeight - 1,
  };
}
let observer: ResizeObserver | undefined;
onMounted(() => {
  measure();
  if (typeof ResizeObserver !== "undefined" && canvas.value) {
    observer = new ResizeObserver(measure);
    observer.observe(canvas.value);
  }
});
onBeforeUnmount(() => observer?.disconnect());
watch(size, () => void nextTick(measure));

// ---------- Dragging ----------

const drag = ref<{ key: string; startX: number; startY: number; origin: Position; moved: boolean; pointerId: number } | null>(null);

function onPointerDown(e: PointerEvent, key: string) {
  if (e.button !== 0) return;
  if (props.readonly) {
    emit("select", { kind: "state", key });
    return;
  }
  (e.currentTarget as Element).setPointerCapture(e.pointerId);
  drag.value = { key, startX: e.clientX, startY: e.clientY, origin: { ...pos(key) }, moved: false, pointerId: e.pointerId };
}

function onPointerMove(e: PointerEvent) {
  const d = drag.value;
  if (!d || e.pointerId !== d.pointerId) return;
  const dx = e.clientX - d.startX;
  const dy = e.clientY - d.startY;
  if (!d.moved && Math.hypot(dx, dy) < 4) return;
  d.moved = true;
  emit("move", d.key, {
    x: Math.max(PAD, Math.round((d.origin.x + dx) / STEP) * STEP),
    y: Math.max(0, Math.round((d.origin.y + dy) / STEP) * STEP),
  });
}

function onPointerUp(e: PointerEvent) {
  const d = drag.value;
  if (!d || e.pointerId !== d.pointerId) return;
  drag.value = null;
  if (!d.moved) emit("select", { kind: "state", key: d.key });
}

function onNodeKey(e: KeyboardEvent, key: string) {
  if (e.key === "Enter" || e.key === " ") {
    e.preventDefault();
    emit("select", { kind: "state", key });
    return;
  }
  if (props.readonly) return;
  const step = e.shiftKey ? STEP * 5 : STEP;
  const delta = { ArrowLeft: [-step, 0], ArrowRight: [step, 0], ArrowUp: [0, -step], ArrowDown: [0, step] }[e.key];
  if (!delta) return;
  e.preventDefault();
  const p = pos(key);
  emit("move", key, { x: Math.max(PAD, p.x + delta[0]), y: Math.max(0, p.y + delta[1]) });
}

function onEdgeKey(e: KeyboardEvent, key: string) {
  if (e.key !== "Enter" && e.key !== " ") return;
  e.preventDefault();
  emit("select", { kind: "transition", key });
}
</script>

<template>
  <div :class="['wf-canvas-frame', { 'more-left': more.left, 'more-right': more.right }]">
    <div ref="canvas" class="wf-canvas" @scroll.passive="measure">
      <svg
        :width="size.w"
        :height="size.h"
        :viewBox="`0 0 ${size.w} ${size.h}`"
        role="group"
        :aria-label="readonly ? 'Workflow diagram' : 'Workflow diagram. Drag a state, or focus it and use the arrow keys, to move it.'"
        @pointermove="onPointerMove"
        @pointerup="onPointerUp"
        @pointercancel="drag = null"
      >
        <defs>
          <marker id="wf-arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="8" markerHeight="8" orient="auto-start-reverse">
            <path d="M0,0 L10,5 L0,10 z" class="wf-arrowhead" />
          </marker>
          <marker id="wf-arrow-selected" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="8" markerHeight="8" orient="auto-start-reverse">
            <path d="M0,0 L10,5 L0,10 z" class="wf-arrowhead selected" />
          </marker>
        </defs>

        <g
          v-for="s in draft.states"
          :key="s.key"
          :transform="`translate(${pos(s.key).x}, ${pos(s.key).y})`"
          :class="['wf-node', `cat-${s.category}`, severity('state', s.key), { selected: isSelected('state', s.key), terminal: s.terminal, readonly }]"
          :tabindex="0"
          role="button"
          :aria-label="`State ${s.name}, ${categoryLabel(s.category)}${s.terminal ? ', terminal' : ''}${draft.initialState === s.key ? ', initial' : ''}${severity('state', s.key) ? `, has ${severity('state', s.key)}s` : ''}`"
          :aria-pressed="isSelected('state', s.key)"
          @pointerdown="onPointerDown($event, s.key)"
          @keydown="onNodeKey($event, s.key)"
        >
          <title>{{ s.name }}</title>
          <path v-if="draft.initialState === s.key" :d="`M -26 ${NODE_H / 2} L -4 ${NODE_H / 2}`" class="wf-initial" marker-end="url(#wf-arrow)" />
          <rect :width="NODE_W" :height="NODE_H" rx="6" class="wf-node-box" />
          <rect v-if="s.terminal" x="3" y="3" :width="NODE_W - 6" :height="NODE_H - 6" rx="4" class="wf-node-inner" />
          <rect width="5" :height="NODE_H" rx="2" class="wf-node-stripe" />
          <text x="14" y="22" class="wf-node-name">{{ shorten(s.name, 22) }}</text>
          <text x="14" y="41" class="wf-node-meta">{{ categoryLabel(s.category) }}{{ s.stateValue ? ` · ${shorten(valueName(s.stateValue), 16)}` : "" }}</text>
        </g>

        <!-- Arrows after the boxes, so a label stays readable where a box is dragged close to an arrow. -->
        <g
          v-for="e in edges"
          :key="e.key"
          :class="['wf-edge', severity('transition', e.key), { selected: isSelected('transition', e.key) }]"
          :tabindex="0"
          role="button"
          :aria-label="`Transition ${e.name}: ${nameOf(e.from)} to ${nameOf(e.to)}${severity('transition', e.key) ? `, has ${severity('transition', e.key)}s` : ''}`"
          :aria-pressed="isSelected('transition', e.key)"
          @click="emit('select', { kind: 'transition', key: e.key })"
          @keydown="onEdgeKey($event, e.key)"
        >
          <title>{{ e.name }}</title>
          <path :d="e.path" class="wf-edge-hit" />
          <path :d="e.path" class="wf-edge-line" :marker-end="isSelected('transition', e.key) ? 'url(#wf-arrow-selected)' : 'url(#wf-arrow)'" />
          <text :x="e.label.x" :y="e.label.y" class="wf-edge-label" text-anchor="middle" dominant-baseline="middle">{{ shorten(e.name, EDGE_LABEL_MAX) }}</text>
        </g>
      </svg>
    </div>
  </div>
  <p v-if="more.left || more.right || more.down" class="wf-canvas-hint" data-testid="wf-canvas-hint">
    The diagram is larger than this panel: scroll it to see the other states.
  </p>
</template>
