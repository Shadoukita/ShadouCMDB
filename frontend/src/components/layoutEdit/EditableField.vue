<script setup lang="ts">
import { computed, ref } from "vue";
import { startGridResize } from "../../lib/gridResize";
import { cellClass } from "../../lib/uiSettings";
import Icon from "../Icon.vue";

/**
 * One field of the real page in layout edit mode: the page's own rendering of
 * the field (the slot, inert), a grip to drag it, a handle on its right edge to
 * resize it, and a toolbar (shown on hover and keyboard focus) with the same
 * actions for the keyboard. On the grip: Alt+↑/↓ move it, Alt+←/→ make it
 * narrower or wider, Delete hides it. A field the layout does not place
 * (`auto`) can only be placed into a section or hidden.
 */
export interface SectionOption {
  key: string;
  label: string;
  tab: string;
}
const props = defineProps<{
  field: string;
  label: string;
  width: number;
  columns: number;
  core: boolean;
  required?: boolean;
  readOnly?: boolean;
  /** Offer "read-only on the form" (the form's editor). */
  canReadOnly?: boolean;
  /** Whether the grip row shows the label (the detail page); the form's slot has its own. */
  showLabel?: boolean;
  auto?: boolean;
  section?: string;
  sections: SectionOption[];
  dropBefore?: boolean;
  dragging?: boolean;
}>();
const emit = defineEmits<{
  move: [delta: -1 | 1];
  /** A new width; `drag` while the right edge is dragged (one undo step until resizeEnd). */
  resize: [width: number, drag?: boolean];
  resizeEnd: [];
  hide: [];
  place: [section: string];
  readOnly: [on: boolean];
  dragstart: [e: DragEvent];
  dragend: [];
}>();

const el = ref<HTMLElement>();
const tabs = computed(() => [...new Set(props.sections.map((s) => s.tab))]);
const describe = computed(() =>
  [`${props.label}`, props.auto ? "not placed by the layout" : `${props.width} of ${props.columns} columns`, props.core ? "core field" : "", props.readOnly ? "read-only on the form" : ""]
    .filter(Boolean)
    .join(", "),
);

function onKey(e: KeyboardEvent) {
  if (e.altKey && (e.key === "ArrowUp" || e.key === "ArrowDown") && !props.auto) {
    e.preventDefault();
    emit("move", e.key === "ArrowUp" ? -1 : 1);
  } else if (e.altKey && (e.key === "ArrowLeft" || e.key === "ArrowRight") && !props.auto) {
    e.preventDefault();
    emit("resize", props.width + (e.key === "ArrowLeft" ? -1 : 1));
  } else if (e.key === "Delete") {
    // Core fields refuse; the editor says why.
    e.preventDefault();
    emit("hide");
  }
}
/** The width shown by the live guide while the right edge is dragged. */
const guide = ref<number | null>(null);
function onResizeStart(e: PointerEvent) {
  guide.value = props.width;
  startGridResize(
    e,
    el.value,
    props.columns,
    () => props.width,
    (w) => {
      guide.value = w;
      emit("resize", w, true);
    },
    () => {
      guide.value = null;
      emit("resizeEnd");
    },
  );
}
</script>

<template>
  <div
    ref="el"
    :class="[cellClass(auto ? 1 : width, columns), 'le-field', { 'drop-before': dropBefore, dragging, auto }]"
    :data-field="field"
    draggable="true"
    @dragstart="emit('dragstart', $event)"
    @dragend="emit('dragend')"
  >
    <div class="le-field-head">
      <button :id="`le-field-${field}`" type="button" class="le-grip" :aria-label="describe" aria-describedby="le-keys" title="Drag to move" @keydown="onKey"><Icon name="grip-vertical" /></button>
      <span v-if="showLabel" class="le-field-label">{{ label }}<span v-if="required" class="req" aria-hidden="true">*</span></span>
      <span v-if="core" class="badge">core</span>
      <span v-if="readOnly" class="badge">read-only</span>
    </div>
    <div class="le-toolbar" role="toolbar" :aria-label="`${label}: layout`">
      <template v-if="!auto">
        <button type="button" class="btn btn-sm btn-icon" :aria-label="`Move ${label} earlier`" title="Move earlier" @click="emit('move', -1)"><Icon name="arrow-up" /></button>
        <button type="button" class="btn btn-sm btn-icon" :aria-label="`Move ${label} later`" title="Move later" @click="emit('move', 1)"><Icon name="arrow-down" /></button>
        <button type="button" class="btn btn-sm btn-icon" :aria-label="`Make ${label} narrower`" title="Narrower" :disabled="width <= 1" @click="emit('resize', width - 1)"><Icon name="chevron-left" /></button>
        <button type="button" class="btn btn-sm" :aria-label="`Make ${label} wider`" title="Wider" :disabled="width >= columns" @click="emit('resize', width + 1)"><Icon name="chevron-right" /></button>
      </template>
      <select :aria-label="`Move ${label} to section`" :value="section ?? ''" @change="emit('place', ($event.target as HTMLSelectElement).value)">
        <option v-if="auto" value="" disabled>Place in…</option>
        <optgroup v-for="t in tabs" :key="t" :label="t">
          <option v-for="s in sections.filter((x) => x.tab === t)" :key="s.key" :value="s.key">{{ s.label }}</option>
        </optgroup>
      </select>
      <label v-if="canReadOnly" class="check le-ro">
        <input type="checkbox" :checked="readOnly" @change="emit('readOnly', ($event.target as HTMLInputElement).checked)" />
        Read-only
      </label>
      <button type="button" class="btn btn-sm" :aria-label="`Hide ${label}`" :disabled="core" :title="core ? 'Core field of every CI: it can be moved, not hidden' : 'Hide'" @click="emit('hide')">
        Hide
      </button>
    </div>
    <div class="le-field-body" inert>
      <slot />
    </div>
    <span v-if="!auto" class="resize-handle" aria-hidden="true" title="Drag to resize (snaps to the section's columns)" @pointerdown="onResizeStart" @click.stop />
    <span v-if="guide !== null" class="lg-size-guide" role="status">{{ guide }} / {{ columns }}</span>
  </div>
</template>

<style scoped>
.le-field {
  position: relative;
  border: 1px solid var(--c-border);
  border-radius: var(--radius-sm);
  padding: var(--space-0_5) var(--space-2) var(--space-1) var(--space-1);
  background: var(--c-surface);
  cursor: grab;
  min-width: 0;
}
.le-field.auto {
  border-style: dashed;
}
.le-field:hover,
.le-field:focus-within {
  border-color: var(--c-primary);
}
.le-field.dragging {
  opacity: 0.45;
}
.le-field.drop-before::before {
  content: "";
  position: absolute;
  left: -9px;
  top: 0;
  bottom: 0;
  width: 3px;
  border-radius: var(--radius-xs);
  background: var(--c-primary);
}
.le-field-head {
  display: flex;
  align-items: center;
  gap: var(--space-0_5);
  font-size: var(--fs-sm);
  font-weight: var(--fw-semibold);
  min-width: 0;
}
.le-field-label {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.le-field-head .badge {
  font-weight: var(--fw-regular);
}
.le-grip {
  border: 0;
  background: none;
  padding: 0 2px;
  color: var(--c-text-secondary);
  cursor: grab;
  font: inherit;
}
.le-grip:focus-visible {
  outline: 2px solid var(--c-focus);
  outline-offset: 1px;
}
.req {
  color: var(--c-danger-text);
  margin-left: 2px;
}
/* The toolbar floats above the field's top edge while the field is hovered or holds the keyboard focus. */
.le-toolbar {
  position: absolute;
  right: 4px;
  bottom: 100%;
  z-index: var(--z-handle);
  display: flex;
  align-items: center;
  gap: 2px;
  padding: 2px;
  border: 1px solid var(--c-primary);
  border-radius: var(--radius-sm);
  background: var(--c-surface);
  box-shadow: var(--shadow-md);
  opacity: 0;
  pointer-events: none;
  white-space: nowrap;
}
.le-field:hover > .le-toolbar,
.le-field:focus-within > .le-toolbar {
  opacity: 1;
  pointer-events: auto;
}
.le-toolbar select {
  max-width: 150px;
  font-size: var(--fs-sm);
  padding: 1px 4px;
}
.le-ro {
  font-size: var(--fs-sm);
  padding: 0 4px;
}
.le-field-body {
  pointer-events: none;
  min-width: 0;
}
.resize-handle {
  position: absolute;
  top: 0;
  right: -4px;
  bottom: 0;
  width: 8px;
  cursor: ew-resize;
  touch-action: none;
}
.resize-handle::after {
  content: "";
  position: absolute;
  top: 30%;
  bottom: 30%;
  left: 3px;
  width: 2px;
  border-radius: var(--radius-xs);
  background: var(--c-border-strong);
}
.le-field:hover .resize-handle::after,
.le-field:focus-within .resize-handle::after {
  background: var(--c-primary);
}
</style>
