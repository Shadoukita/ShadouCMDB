<script lang="ts">
/** The key of the section being dragged, known to every shell at once (dragover cannot read the drag's data). */
let dragged: string | null = null;
</script>

<script setup lang="ts">
import { computed, ref } from "vue";
import { startLineDrag } from "../../lib/gridResize";
import type { LayoutSection } from "../../lib/layoutDesign";
import { SECTION_GRID, sectionClass, sectionStyle, sectionWidth } from "../../lib/uiSettings";

/**
 * A section of a layout being edited, on its tab's 12-column grid (the in-page
 * layout editor and the form designer): the section itself is the slot. Around
 * it: a grip on its top edge (left) to drag it (onto the left or right edge of another
 * section to sit beside it, above or below one to move it there), a handle on
 * its right edge to resize it, one on its left edge where it borders the section
 * before it in the same row, a live "6 / 12" guide while resizing, and, on
 * hover, "+" buttons that add a section next to it or below it.
 *
 * Keyboard, on the grip: Alt+↑ / Alt+↓ move the section, Alt+← / Alt+→ make it
 * narrower or wider. The toolbar of the section has the same actions.
 */
export type DropSide = "left" | "right" | "before" | "after";

const props = defineProps<{
  section: LayoutSection;
  /** The section before it in the same row, whose border the left handle moves. */
  hasLeft: boolean;
  /** Start column on the tab's grid (0–11), for the left handle. */
  start: number;
  dragging?: boolean;
  drop?: DropSide | null;
  /** Prefix for element ids (grip `${idPrefix}-grip-${key}`). */
  idPrefix: string;
  /** Id of the element that explains the keys. */
  keysId: string;
}>();
const emit = defineEmits<{
  width: [width: number, drag: boolean];
  border: [line: number];
  gestureEnd: [];
  move: [delta: -1 | 1];
  add: [where: "beside" | "after"];
  dragstart: [e: DragEvent];
  dragend: [];
  dropside: [side: DropSide | null];
  dropped: [side: DropSide];
}>();

const el = ref<HTMLElement>();
const width = computed(() => sectionWidth(props.section));
/** The width shown by the live guide while an edge is dragged. */
const guide = ref<number | null>(null);
const label = computed(() => props.section.label);

function grid() {
  return el.value?.parentElement ?? null;
}
function onGuide(on: boolean) {
  grid()?.classList.toggle("lg-guide", on);
}
function onRightDown(e: PointerEvent) {
  guide.value = width.value;
  onGuide(true);
  startLineDrag(
    e,
    grid(),
    SECTION_GRID,
    (line) => {
      const w = Math.max(1, Math.min(line - props.start, SECTION_GRID));
      guide.value = w;
      if (w !== width.value) emit("width", w, true);
    },
    end,
  );
}
function onLeftDown(e: PointerEvent) {
  guide.value = width.value;
  onGuide(true);
  startLineDrag(
    e,
    grid(),
    SECTION_GRID,
    (line) => {
      emit("border", line);
      guide.value = width.value;
    },
    end,
  );
}
function end() {
  guide.value = null;
  onGuide(false);
  emit("gestureEnd");
}

function onKey(e: KeyboardEvent) {
  if (!e.altKey) return;
  if (e.key === "ArrowUp" || e.key === "ArrowDown") {
    e.preventDefault();
    emit("move", e.key === "ArrowUp" ? -1 : 1);
  } else if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
    e.preventDefault();
    emit("width", width.value + (e.key === "ArrowLeft" ? -1 : 1), false);
  }
}

/** Which part of the section the pointer is over: its outer quarters (at most 80px) left and right, else its upper or lower half. */
function sideAt(e: DragEvent): DropSide {
  const r = el.value!.getBoundingClientRect();
  const edge = Math.min(80, r.width / 4);
  if (e.clientX < r.left + edge) return "left";
  if (e.clientX > r.right - edge) return "right";
  return e.clientY < r.top + r.height / 2 ? "before" : "after";
}
/** Another section is being dragged: this one is a drop target. */
const target = () => dragged !== null && dragged !== props.section.key;
function onDragOver(e: DragEvent) {
  if (!target()) return;
  e.preventDefault();
  if (e.dataTransfer) e.dataTransfer.dropEffect = "move";
  const side = sideAt(e);
  if (side !== props.drop) emit("dropside", side);
}
function onDragLeave(e: DragEvent) {
  if (props.drop && !el.value?.contains(e.relatedTarget as Node | null)) emit("dropside", null);
}
function onDrop(e: DragEvent) {
  if (!target()) return;
  e.preventDefault();
  dragged = null;
  emit("dropped", sideAt(e));
}
function onGripDragStart(e: DragEvent) {
  e.stopPropagation();
  if (e.dataTransfer) {
    e.dataTransfer.effectAllowed = "move";
    e.dataTransfer.setData("text/plain", `section:${props.section.key}`);
    if (el.value) e.dataTransfer.setDragImage(el.value, 24, 12);
  }
  dragged = props.section.key;
  emit("dragstart", e);
}
function onGripDragEnd() {
  dragged = null;
  emit("dragend");
}
</script>

<template>
  <div
    ref="el"
    :class="['sec-shell', ...sectionClass(section), drop ? `drop-${drop}` : '', { dragging, resizing: guide !== null }]"
    :style="sectionStyle(section)"
    :data-section-shell="section.key"
    :data-width="width"
    @dragover="onDragOver"
    @dragleave="onDragLeave"
    @drop="onDrop"
  >
    <slot />
    <!-- A span, not a button: browsers do not start a drag from a button everywhere. -->
    <span
      :id="`${idPrefix}-grip-${section.key}`"
      role="button"
      tabindex="0"
      class="sec-grip"
      draggable="true"
      :aria-label="`Section ${label}, ${width} of ${SECTION_GRID} columns wide`"
      :aria-describedby="keysId"
      title="Drag to move the section: onto another section's left or right edge to place it beside"
      @dragstart="onGripDragStart"
      @dragend="onGripDragEnd"
      @keydown="onKey"
    >
      <span aria-hidden="true">⠿⠿</span>
    </span>
    <span v-if="hasLeft" class="sec-edge left" aria-hidden="true" title="Drag to share the row differently" data-testid="section-edge-left" @pointerdown="onLeftDown" />
    <span class="sec-edge right" aria-hidden="true" title="Drag to resize (snaps to 12 columns)" data-testid="section-edge-right" @pointerdown="onRightDown" />
    <span v-if="guide !== null" class="sec-guide" role="status" data-testid="section-guide">{{ guide }} / {{ SECTION_GRID }}</span>
    <button type="button" class="sec-add beside" :aria-label="`Add a section next to ${label}`" title="Add a section next to this one" @click="emit('add', 'beside')">+</button>
    <button type="button" class="sec-add after" :aria-label="`Add a section below ${label}`" title="Add a section below this one" @click="emit('add', 'after')">+ Section</button>
  </div>
</template>

<style scoped>
.sec-shell {
  position: relative;
  min-width: 0;
  border-radius: var(--radius);
}
.sec-shell.dragging {
  opacity: 0.45;
}
.sec-shell.resizing {
  outline: 2px solid var(--c-primary);
  outline-offset: 2px;
}
.sec-shell > :deep(.panel) {
  margin: 0;
  height: 100%;
}
/* The drop indicator: a bar on the side the dragged section goes to. */
.sec-shell[class*="drop-"]::after {
  content: "";
  position: absolute;
  background: var(--c-primary);
  border-radius: 2px;
  pointer-events: none;
  z-index: 4;
}
.sec-shell.drop-left::after {
  left: -8px;
  top: 0;
  bottom: 0;
  width: 4px;
}
.sec-shell.drop-right::after {
  right: -8px;
  top: 0;
  bottom: 0;
  width: 4px;
}
.sec-shell.drop-before::after {
  top: -8px;
  left: 0;
  right: 0;
  height: 4px;
}
.sec-shell.drop-after::after {
  bottom: -8px;
  left: 0;
  right: 0;
  height: 4px;
}
.sec-grip {
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
  cursor: grab;
}
.sec-grip:hover,
.sec-grip:focus-visible {
  border-color: var(--c-primary);
  color: var(--c-primary);
}
.sec-grip:focus-visible {
  outline: 2px solid var(--c-focus);
}
/* Edge handles: a strip along the edge with a visible bar on hover. */
.sec-edge {
  position: absolute;
  top: 0;
  bottom: 0;
  width: 12px;
  z-index: 2;
  cursor: col-resize;
  touch-action: none;
}
.sec-edge.right {
  right: -7px;
}
.sec-edge.left {
  left: -7px;
}
.sec-edge::before {
  content: "";
  position: absolute;
  top: 50%;
  left: 4px;
  width: 4px;
  height: 40px;
  max-height: 60%;
  transform: translateY(-50%);
  border-radius: 2px;
  background: var(--c-border-strong);
  opacity: 0.6;
}
.sec-shell:hover > .sec-edge::before,
.sec-shell.resizing > .sec-edge::before {
  background: var(--c-primary);
  opacity: 1;
}
.sec-guide {
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
  pointer-events: none;
}
.sec-add {
  position: absolute;
  z-index: 3;
  border: 1px dashed var(--c-primary);
  border-radius: var(--radius);
  background: var(--c-surface);
  color: var(--c-primary);
  font-size: var(--fs-sm);
  cursor: pointer;
  opacity: 0;
  pointer-events: none;
}
.sec-add.beside {
  bottom: 10px;
  right: -12px;
  width: 22px;
  height: 22px;
  padding: 0;
  border-radius: 50%;
}
.sec-add.after {
  bottom: -12px;
  left: 50%;
  transform: translateX(-50%);
  padding: 0 var(--sp-2);
  line-height: 20px;
}
.sec-shell:hover > .sec-add,
.sec-add:focus-visible {
  opacity: 1;
  pointer-events: auto;
}
.sec-add:focus-visible {
  outline: 2px solid var(--c-focus);
}
.sec-shell.resizing > .sec-add {
  display: none;
}
</style>
