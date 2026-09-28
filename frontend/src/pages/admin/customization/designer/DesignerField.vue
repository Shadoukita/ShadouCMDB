<script setup lang="ts">
import { computed, ref } from "vue";
import type { EffectiveAttribute } from "../../../../api/queries";
import { startGridResize } from "../../../../lib/gridResize";
import { cellClass } from "../../../../lib/uiSettings";

/**
 * One field on the form designer's canvas: the form control as the CI form
 * shows it (inert, only a picture), a grip to drag it and a handle on its right
 * edge to resize it on the section's grid. Keyboard: Enter or Space selects it
 * (the side panel then offers every action), Alt+↑/↓ move it, Alt+←/→ make it
 * narrower or wider, Delete hides it.
 */
const props = defineProps<{
  field: string;
  label: string;
  def?: EffectiveAttribute;
  width: number;
  columns: number;
  selected: boolean;
  readOnly: boolean;
  core: boolean;
  /** Shown on the detail page only (class, timestamps): no form control. */
  detailOnly?: boolean;
  /** An insertion mark before this field while something is dragged over it. */
  dropBefore?: boolean;
  dragging?: boolean;
}>();
const emit = defineEmits<{
  select: [];
  move: [delta: -1 | 1];
  /** A new width; `drag` while the right edge is dragged (one undo step until resizeEnd). */
  resize: [width: number, drag?: boolean];
  resizeEnd: [];
  hide: [];
  dragstart: [e: DragEvent];
  dragend: [];
}>();

const el = ref<HTMLElement>();
const kind = computed(() => {
  if (props.detailOnly) return "none";
  if (props.field === "ident") return "text";
  if (props.field === "validFrom" || props.field === "validUntil") return "datetime";
  switch (props.def?.dataType) {
    case "boolean":
    case "enum":
    case "lookup":
      return "select";
    case "integer":
    case "number":
      return "number";
    case "date":
      return "date";
    case "datetime":
      return "datetime";
    default:
      return "text";
  }
});
const placeholder = computed(() => {
  if (props.field === "ident") return "Generated";
  if (props.def?.dataType === "reference") return "Search a configuration item…";
  if (props.def?.dataType === "ip" || props.def?.dataType === "cidr") return props.def.dataType === "ip" ? "10.0.0.1" : "10.0.0.0/24";
  return "";
});

function onKey(e: KeyboardEvent) {
  if (e.target !== el.value) return;
  if (e.key === "Enter" || e.key === " ") {
    e.preventDefault();
    emit("select");
  } else if (e.altKey && (e.key === "ArrowUp" || e.key === "ArrowDown")) {
    e.preventDefault();
    emit("move", e.key === "ArrowUp" ? -1 : 1);
  } else if (e.altKey && (e.key === "ArrowLeft" || e.key === "ArrowRight")) {
    e.preventDefault();
    emit("resize", props.width + (e.key === "ArrowLeft" ? -1 : 1));
  } else if (e.key === "Delete") {
    // Core fields refuse; the designer says why.
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

defineExpose({ focus: () => el.value?.focus() });
</script>

<template>
  <div
    :id="`designer-field-${field}`"
    ref="el"
    :class="[cellClass(width, columns), 'designer-field', { selected, 'drop-before': dropBefore, dragging }]"
    :data-field="field"
    tabindex="0"
    role="button"
    :aria-pressed="selected"
    :aria-label="`${label}, ${width} of ${columns} columns${core ? ', core field' : ''}${readOnly ? ', read-only' : ''}`"
    aria-describedby="designer-keys"
    draggable="true"
    @click="emit('select')"
    @keydown="onKey"
    @dragstart="emit('dragstart', $event)"
    @dragend="emit('dragend')"
  >
    <div class="designer-field-top">
      <span class="grip" aria-hidden="true">⠿</span>
      <span class="designer-label">{{ label }}<span v-if="def?.isRequired" class="req" aria-hidden="true">*</span></span>
      <span v-if="core" class="badge">core</span>
      <span v-if="readOnly" class="badge">read-only</span>
      <span v-if="detailOnly" class="badge">detail page only</span>
    </div>
    <div class="designer-control" inert>
      <select v-if="kind === 'select'" tabindex="-1" aria-hidden="true">
        <option>— not set —</option>
      </select>
      <input v-else-if="kind === 'number'" type="number" tabindex="-1" aria-hidden="true" />
      <input v-else-if="kind === 'date'" type="date" tabindex="-1" aria-hidden="true" />
      <input v-else-if="kind === 'datetime'" type="datetime-local" tabindex="-1" aria-hidden="true" />
      <input v-else-if="kind === 'text'" type="text" :class="{ mono: field === 'ident' }" :placeholder="placeholder" tabindex="-1" aria-hidden="true" />
      <span v-else class="muted">Value</span>
    </div>
    <span class="resize-handle" aria-hidden="true" title="Drag to resize (snaps to the section's columns)" @pointerdown="onResizeStart" @click.stop />
    <span v-if="guide !== null" class="lg-size-guide" role="status">{{ guide }} / {{ columns }}</span>
  </div>
</template>

<style scoped>
.designer-field {
  position: relative;
  border: 1px dashed var(--c-border-strong);
  border-radius: var(--radius);
  padding: var(--sp-2) var(--sp-3) var(--sp-2) var(--sp-2);
  background: var(--c-surface);
  cursor: grab;
  display: flex;
  flex-direction: column;
  gap: var(--sp-1);
}
.designer-field:hover {
  border-color: var(--c-primary);
}
.designer-field:focus-visible {
  outline: 2px solid var(--c-focus);
  outline-offset: 1px;
}
.designer-field.selected {
  border-style: solid;
  border-color: var(--c-primary);
  box-shadow: 0 0 0 1px var(--c-primary);
}
.designer-field.dragging {
  opacity: 0.45;
}
.designer-field.drop-before::before {
  content: "";
  position: absolute;
  left: -9px;
  top: 0;
  bottom: 0;
  width: 3px;
  border-radius: 2px;
  background: var(--c-primary);
}
.designer-field-top {
  display: flex;
  align-items: center;
  gap: var(--sp-1);
  font-size: var(--fs-sm);
  font-weight: 600;
  min-width: 0;
}
.designer-label {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.designer-field-top .badge {
  font-weight: 400;
}
.grip {
  color: var(--c-text-muted);
}
.req {
  color: var(--c-danger);
  margin-left: 2px;
}
.designer-control {
  pointer-events: none;
}
.designer-control input,
.designer-control select {
  width: 100%;
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
  border-radius: 1px;
  background: var(--c-border-strong);
}
.designer-field:hover .resize-handle::after,
.designer-field.selected .resize-handle::after {
  background: var(--c-primary);
}
</style>
