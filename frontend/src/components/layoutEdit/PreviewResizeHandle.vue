<script setup lang="ts">
import { ref } from "vue";

/**
 * The grip on the right edge of a layout preview frame (the in-page layout
 * editor and the form designer): drag it to any width, no preset needed. The
 * width presets next to the preview are shortcuts. The frame is `null` wide
 * (the full width) when dragged to the edge of the space it has. Keyboard: ← / →
 * change the width by 10 px (Shift: 50 px), Home is the narrowest, End the full
 * width.
 */
const MIN = 320;
const props = defineProps<{
  /** The frame is centred: dragging one edge moves both. */
  centered?: boolean;
  label: string;
}>();
const width = defineModel<number | null>({ required: true });
const el = ref<HTMLElement>();
const dragging = ref(false);
const shown = ref(0);

const frame = () => el.value?.parentElement ?? null;
const room = () => frame()?.parentElement?.clientWidth ?? window.innerWidth;
function set(w: number) {
  const max = room();
  const next = Math.round(Math.max(MIN, Math.min(w, max)));
  shown.value = next;
  width.value = next >= max ? null : next;
}

function onDown(e: PointerEvent) {
  const f = frame();
  if (!f) return;
  e.preventDefault();
  const handle = e.currentTarget as HTMLElement;
  handle.setPointerCapture(e.pointerId);
  const r = f.getBoundingClientRect();
  const startX = e.clientX;
  const startW = r.width;
  dragging.value = true;
  shown.value = Math.round(startW);
  const onMove = (ev: PointerEvent) => set(startW + (ev.clientX - startX) * (props.centered ? 2 : 1));
  const onUp = () => {
    dragging.value = false;
    handle.removeEventListener("pointermove", onMove);
    handle.removeEventListener("pointerup", onUp);
    handle.removeEventListener("pointercancel", onUp);
  };
  handle.addEventListener("pointermove", onMove);
  handle.addEventListener("pointerup", onUp);
  handle.addEventListener("pointercancel", onUp);
}
function onKey(e: KeyboardEvent) {
  const cur = frame()?.getBoundingClientRect().width ?? room();
  const step = e.shiftKey ? 50 : 10;
  if (e.key === "ArrowLeft") set(cur - step);
  else if (e.key === "ArrowRight") set(cur + step);
  else if (e.key === "Home") set(MIN);
  else if (e.key === "End") set(room());
  else return;
  e.preventDefault();
}
</script>

<template>
  <div
    ref="el"
    class="preview-grip"
    :class="{ dragging }"
    role="separator"
    tabindex="0"
    aria-orientation="vertical"
    :aria-label="label"
    :aria-valuemin="MIN"
    :aria-valuemax="room()"
    :aria-valuenow="width ?? room()"
    title="Drag to resize the preview (← / → with the keyboard)"
    data-testid="preview-grip"
    @pointerdown="onDown"
    @keydown="onKey"
  >
    <span class="preview-grip-bar" aria-hidden="true">⋮</span>
    <span v-if="dragging" class="preview-grip-size" aria-hidden="true">{{ shown }} px</span>
  </div>
</template>

<style scoped>
.preview-grip {
  position: absolute;
  top: 0;
  right: 0;
  bottom: 0;
  width: 14px;
  display: flex;
  flex-direction: column;
  align-items: center;
  cursor: ew-resize;
  touch-action: none;
  z-index: 5; /* under the editor's sticky bar */
}
.preview-grip-bar {
  position: sticky;
  top: 40vh;
  margin-top: 40px;
  display: flex;
  align-items: center;
  justify-content: center;
  width: 14px;
  height: 56px;
  border: 1px solid var(--c-primary);
  border-radius: var(--radius) 0 0 var(--radius);
  background: var(--c-surface);
  color: var(--c-primary);
  font-weight: 700;
  font-size: 16px;
  line-height: 1;
}
.preview-grip:hover .preview-grip-bar,
.preview-grip.dragging .preview-grip-bar,
.preview-grip:focus-visible .preview-grip-bar {
  background: var(--c-primary);
  color: #fff;
}
.preview-grip:focus-visible {
  outline: none;
}
.preview-grip:focus-visible .preview-grip-bar {
  outline: 2px solid var(--c-focus);
  outline-offset: 1px;
}
.preview-grip-size {
  position: sticky;
  top: calc(40vh + 64px);
  margin-top: var(--sp-2);
  padding: 2px var(--sp-2);
  border-radius: var(--radius);
  background: var(--c-primary);
  color: #fff;
  font-size: var(--fs-sm);
  white-space: nowrap;
  font-variant-numeric: tabular-nums;
}
</style>
