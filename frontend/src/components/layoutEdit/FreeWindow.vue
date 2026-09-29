<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref } from "vue";
import { dragBox, describeBox, fromBox, LAYER_MOVES, minHeightOf, MOVE, toBox, type Box, type Edges, type Frame, type LayerMove, type SnapLine } from "../../lib/freeLayout";
import type { LayoutSection } from "../../lib/layoutDesign";

/**
 * A section of a free tab in the layout editor, as a desktop window (the section
 * itself is the slot): drag it by its title bar, resize it from any edge or
 * corner, and put it over or under the others. While the pointer moves, a
 * readout shows its position and size in px; its edges snap to the other
 * windows' edges and a fine guide grid unless snapping is off or Alt is held.
 * Pressing on a window selects it (the editor brings it to the front). The right
 * mouse button, or Shift+F10 / the context menu key on its grip, opens the
 * layering menu.
 *
 * Keyboard, on the grip: arrows move the window by 8 px (Shift: 64 px, Alt:
 * 1 px), Ctrl+arrows resize it, Ctrl+PageUp / Ctrl+PageDown move it one layer up
 * or down, with Shift to the front or the back.
 */
const props = defineProps<{
  section: LayoutSection;
  frame: Frame;
  /** The width of the tab's area in px (x and w are fractions of it). */
  areaWidth: number;
  /** The other windows of the tab, in px, to snap to. */
  others: readonly Box[];
  layer: { index: number; count: number };
  selected: boolean;
  snap: boolean;
  /** The tab is narrower than the tablet breakpoint: windows stack in reading order and do not move. */
  stacked: boolean;
  idPrefix: string;
  keysId: string;
}>();
const emit = defineEmits<{
  frame: [frame: Frame, drag: boolean];
  gestureStart: [];
  gestureEnd: [text: string | null];
  select: [];
  layer: [move: LayerMove];
  guides: [lines: SnapLine[]];
}>();

const el = ref<HTMLElement>();
const box = computed(() => toBox(props.frame, props.areaWidth));
/** The box while a drag or resize is under way: the live readout. */
const live = ref<Box | null>(null);
const label = computed(() => props.section.label);
const gripLabel = computed(() => `Window ${label.value}: ${describeBox(box.value)}, layer ${props.layer.index} of ${props.layer.count}`);

// ---------- Pointer: move by the title bar, resize by the edges ----------

/** What keeps its own use in the title bar: controls (the section's name moves the window when dragged, and renames it when clicked). */
const INTERACTIVE = "input, select, textarea, a, [contenteditable], button:not(.le-section-label), .le-field, [data-no-drag]";
let stop: (() => void) | null = null;
/** Set after a drag: the click that ends it must not rename the section or press a button. */
let swallowClick = false;

function begin(e: PointerEvent, edges: Edges, threshold: number) {
  const x0 = e.clientX;
  const y0 = e.clientY;
  const start = { ...box.value };
  const minH = minHeightOf(props.frame);
  let started = threshold === 0;
  let frame = 0;
  let last: PointerEvent | null = null;
  if (started) emit("gestureStart");
  const apply = () => {
    frame = 0;
    const ev = last!;
    const r = dragBox(start, edges, ev.clientX - x0, ev.clientY - y0, { width: props.areaWidth, minH, others: props.others, snap: props.snap && !ev.altKey });
    live.value = r.box;
    emit("guides", r.lines);
    emit("frame", fromBox(r.box, props.areaWidth, props.frame), true);
  };
  const onMove = (ev: PointerEvent) => {
    if (!started) {
      if (Math.hypot(ev.clientX - x0, ev.clientY - y0) < threshold) return;
      started = true;
      emit("gestureStart");
    }
    ev.preventDefault();
    last = ev;
    if (!frame) frame = requestAnimationFrame(apply);
  };
  const onUp = () => {
    stop?.();
    if (frame) {
      cancelAnimationFrame(frame);
      apply();
    }
    if (!started) return;
    swallowClick = true;
    setTimeout(() => (swallowClick = false), 0);
    const text = live.value ? `Window ${label.value}: ${describeBox(live.value)}.` : null;
    live.value = null;
    emit("guides", []);
    emit("gestureEnd", text);
  };
  stop = () => {
    window.removeEventListener("pointermove", onMove);
    window.removeEventListener("pointerup", onUp);
    window.removeEventListener("pointercancel", onUp);
    stop = null;
  };
  window.addEventListener("pointermove", onMove);
  window.addEventListener("pointerup", onUp);
  window.addEventListener("pointercancel", onUp);
}
onBeforeUnmount(() => stop?.());

function onPointerDown(e: PointerEvent) {
  if (e.button !== 0 || props.stacked) return;
  const target = e.target as HTMLElement;
  emit("select");
  // The title bar (and the grip) move the window; its buttons still work when the pointer does not move.
  if (target.closest(INTERACTIVE) || !target.closest(".panel-header, .win-grip")) return;
  begin(e, MOVE, 4);
}
function onEdgeDown(e: PointerEvent, edges: Edges) {
  if (e.button !== 0) return;
  e.preventDefault();
  e.stopPropagation();
  emit("select");
  begin(e, edges, 0);
}
function onClickCapture(e: MouseEvent) {
  if (!swallowClick) return;
  e.preventDefault();
  e.stopPropagation();
  swallowClick = false;
}

const HANDLES: { key: string; edges: Edges; title: string }[] = [
  { key: "n", edges: { n: true }, title: "Drag to change the height (top edge)" },
  { key: "s", edges: { s: true }, title: "Drag to change the height" },
  { key: "w", edges: { w: true }, title: "Drag to change the width (left edge)" },
  { key: "e", edges: { e: true }, title: "Drag to change the width" },
  { key: "nw", edges: { n: true, w: true }, title: "Drag to resize" },
  { key: "ne", edges: { n: true, e: true }, title: "Drag to resize" },
  { key: "sw", edges: { s: true, w: true }, title: "Drag to resize" },
  { key: "se", edges: { s: true, e: true }, title: "Drag to resize" },
];

// ---------- Keyboard ----------

function onKey(e: KeyboardEvent) {
  if (props.stacked) return;
  const arrows: Record<string, [number, number]> = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] };
  const ctrl = e.ctrlKey || e.metaKey;
  if (e.key === "ContextMenu" || (e.key === "F10" && e.shiftKey)) {
    e.preventDefault();
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    openMenu(r.left, r.bottom);
    return;
  }
  if (ctrl && (e.key === "PageUp" || e.key === "PageDown")) {
    e.preventDefault();
    emit("layer", e.key === "PageUp" ? (e.shiftKey ? "front" : "forward") : e.shiftKey ? "back" : "backward");
    return;
  }
  const dir = arrows[e.key];
  if (!dir) return;
  e.preventDefault();
  const step = e.altKey ? 1 : e.shiftKey ? 64 : 8;
  const [dx, dy] = [dir[0] * step, dir[1] * step];
  const edges: Edges = ctrl ? { e: dx !== 0, s: dy !== 0 } : MOVE;
  // Keys move by exact steps: no snapping.
  const r = dragBox(box.value, edges, dx, dy, { width: props.areaWidth, minH: minHeightOf(props.frame), others: [], snap: false });
  emit("frame", fromBox(r.box, props.areaWidth, props.frame), false);
  emit("gestureEnd", `Window ${label.value}: ${describeBox(r.box)}.`);
}

// ---------- The layering menu ----------

const menu = ref<{ x: number; y: number } | null>(null);
function openMenu(clientX: number, clientY: number) {
  const r = el.value!.getBoundingClientRect();
  menu.value = { x: clientX - r.left, y: clientY - r.top };
  emit("select");
  void nextTick(() => el.value?.querySelector<HTMLElement>(".win-menu [role=menuitem]:not([disabled])")?.focus());
  window.addEventListener("pointerdown", onOutside, true);
}
function closeMenu(refocus = true) {
  menu.value = null;
  window.removeEventListener("pointerdown", onOutside, true);
  if (refocus) document.getElementById(`${props.idPrefix}-wgrip-${props.section.key}`)?.focus();
}
function onOutside(e: PointerEvent) {
  if (!(e.target as HTMLElement).closest(".win-menu")) closeMenu(false);
}
onBeforeUnmount(() => window.removeEventListener("pointerdown", onOutside, true));
function onContextMenu(e: MouseEvent) {
  if (props.stacked || (e.target as HTMLElement).closest("input, textarea, select")) return;
  e.preventDefault();
  openMenu(e.clientX, e.clientY);
}
function onMenuKey(e: KeyboardEvent) {
  const items = [...(e.currentTarget as HTMLElement).querySelectorAll<HTMLElement>("[role=menuitem]:not([disabled])")];
  const i = items.indexOf(document.activeElement as HTMLElement);
  if (e.key === "Escape" || e.key === "Tab") {
    e.preventDefault();
    closeMenu();
  } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    e.preventDefault();
    items[(i + (e.key === "ArrowDown" ? 1 : items.length - 1)) % items.length]?.focus();
  } else if (e.key === "Home" || e.key === "End") {
    e.preventDefault();
    items[e.key === "Home" ? 0 : items.length - 1]?.focus();
  }
}
function pick(move: LayerMove) {
  closeMenu();
  emit("layer", move);
}
const canRaise = computed(() => props.layer.index < props.layer.count);
const canLower = computed(() => props.layer.index > 1);
</script>

<template>
  <div
    ref="el"
    :class="['le-win', { selected, stacked, moving: live !== null }]"
    :style="
      stacked
        ? frame.minH
          ? { minHeight: `${frame.minH}px` }
          : undefined
        : { left: `${frame.x * 100}%`, top: `${frame.y}px`, width: `${frame.w * 100}%`, height: `${frame.h}px`, zIndex: frame.z }
    "
    :data-section-shell="section.key"
    :data-window="section.key"
    :data-x="frame.x"
    :data-y="frame.y"
    :data-w="frame.w"
    :data-h="frame.h"
    :data-z="frame.z"
    @pointerdown="onPointerDown"
    @click.capture="onClickCapture"
    @contextmenu="onContextMenu"
  >
    <slot />
    <span
      :id="`${idPrefix}-wgrip-${section.key}`"
      role="button"
      tabindex="0"
      class="win-grip"
      :aria-label="gripLabel"
      :aria-describedby="keysId"
      aria-haspopup="menu"
      :title="stacked ? 'Windows stack on narrow screens: widen the preview to move them' : 'Drag to move the window; right-click for the layers'"
      @keydown="onKey"
    >
      <span aria-hidden="true">⠿⠿</span>
    </span>
    <template v-if="!stacked">
      <span v-for="h in HANDLES" :key="h.key" :class="['win-edge', h.key]" aria-hidden="true" :title="h.title" :data-testid="`window-edge-${h.key}`" @pointerdown="onEdgeDown($event, h.edges)" />
    </template>
    <span v-if="live" class="win-readout" role="status" data-testid="window-readout">{{ describeBox(live) }}</span>
    <div v-if="menu" class="win-menu" role="menu" :aria-label="`Layers of ${label}`" :style="{ left: `${menu.x}px`, top: `${menu.y}px` }" data-no-drag @keydown="onMenuKey">
      <button
        v-for="m in LAYER_MOVES"
        :key="m.move"
        type="button"
        role="menuitem"
        :disabled="m.move === 'front' || m.move === 'forward' ? !canRaise : !canLower"
        @click="pick(m.move)"
      >
        {{ m.label }}<kbd>{{ m.keys }}</kbd>
      </button>
    </div>
  </div>
</template>

<style scoped>
.le-win {
  position: absolute;
  min-width: 0;
  border-radius: var(--radius);
  touch-action: none;
}
.le-win.stacked {
  position: static;
  touch-action: auto;
}
.le-win > :deep(.panel) {
  margin: 0;
  height: 100%;
  overflow: auto;
  box-shadow: 0 1px 4px rgb(0 0 0 / 12%);
}
.le-win.stacked > :deep(.panel) {
  height: auto;
  overflow: visible;
}
.le-win:not(.stacked) > :deep(.panel) > .panel-header {
  position: sticky;
  top: 0;
  z-index: 2;
  background: var(--c-surface);
  cursor: move;
}
.le-win.selected > :deep(.panel) {
  outline: 2px solid var(--c-primary);
  outline-offset: 1px;
}
.le-win.moving > :deep(.panel) {
  box-shadow: 0 6px 18px rgb(0 0 0 / 22%);
}
.win-grip {
  position: absolute;
  top: -9px;
  left: var(--sp-5);
  z-index: 3;
  padding: 0 var(--sp-2);
  line-height: 16px;
  font-size: 11px;
  letter-spacing: -1px;
  border: 1px solid var(--c-border-strong);
  border-radius: 8px;
  background: var(--c-surface);
  color: var(--c-text-muted);
  cursor: move;
}
.stacked .win-grip {
  cursor: default;
}
.win-grip:hover,
.win-grip:focus-visible,
.selected .win-grip {
  border-color: var(--c-primary);
  color: var(--c-primary);
}
.win-grip:focus-visible {
  outline: 2px solid var(--c-focus);
}
/* Resize handles: strips along the edges and squares at the corners, just outside the window. */
.win-edge {
  position: absolute;
  z-index: 4;
}
.win-edge.n,
.win-edge.s {
  left: 8px;
  right: 8px;
  height: 8px;
  cursor: ns-resize;
}
.win-edge.n {
  top: -4px;
}
.win-edge.s {
  bottom: -4px;
}
.win-edge.w,
.win-edge.e {
  top: 8px;
  bottom: 8px;
  width: 8px;
  cursor: ew-resize;
}
.win-edge.w {
  left: -4px;
}
.win-edge.e {
  right: -4px;
}
.win-edge.nw,
.win-edge.ne,
.win-edge.sw,
.win-edge.se {
  width: 12px;
  height: 12px;
}
.win-edge.nw {
  top: -5px;
  left: -5px;
  cursor: nwse-resize;
}
.win-edge.se {
  bottom: -5px;
  right: -5px;
  cursor: nwse-resize;
}
.win-edge.ne {
  top: -5px;
  right: -5px;
  cursor: nesw-resize;
}
.win-edge.sw {
  bottom: -5px;
  left: -5px;
  cursor: nesw-resize;
}
.le-win:hover > .win-edge.se,
.le-win.selected > .win-edge.se {
  border-right: 3px solid var(--c-primary);
  border-bottom: 3px solid var(--c-primary);
  border-radius: 0 0 3px 0;
}
.win-readout {
  position: absolute;
  top: 50%;
  left: 50%;
  transform: translate(-50%, -50%);
  z-index: 5;
  padding: var(--sp-1) var(--sp-3);
  border-radius: var(--radius);
  background: var(--c-primary);
  color: #fff;
  font-weight: 600;
  font-variant-numeric: tabular-nums;
  white-space: nowrap;
  pointer-events: none;
}
.win-menu {
  position: absolute;
  z-index: 10;
  display: flex;
  flex-direction: column;
  min-width: 220px;
  padding: var(--sp-1) 0;
  border: 1px solid var(--c-border-strong);
  border-radius: var(--radius);
  background: var(--c-surface);
  box-shadow: 0 6px 18px rgb(0 0 0 / 22%);
}
.win-menu [role="menuitem"] {
  display: flex;
  justify-content: space-between;
  gap: var(--sp-4);
  padding: var(--sp-1) var(--sp-3);
  border: 0;
  background: none;
  font: inherit;
  color: inherit;
  text-align: left;
  cursor: pointer;
}
.win-menu [role="menuitem"]:hover:not(:disabled),
.win-menu [role="menuitem"]:focus-visible {
  background: var(--c-row-hover);
  outline: none;
}
.win-menu [role="menuitem"]:disabled {
  color: var(--c-text-muted);
  cursor: default;
}
.win-menu kbd {
  color: var(--c-text-muted);
  font-size: var(--fs-sm);
}
</style>
