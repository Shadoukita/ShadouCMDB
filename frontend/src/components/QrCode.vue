<script setup lang="ts">
import qrcode from "qrcode-generator";
import { computed } from "vue";

/**
 * A QR code drawn as one SVG path, rendered in the browser (the value never
 * leaves it). Always dark on white with a quiet zone, whatever the theme, so
 * phone cameras read it.
 */
const props = withDefaults(defineProps<{ value: string; label: string; size?: number }>(), { size: 200 });
const QUIET = 4;

const code = computed(() => {
  const qr = qrcode(0, "M");
  qr.addData(props.value);
  qr.make();
  const n = qr.getModuleCount();
  let d = "";
  for (let r = 0; r < n; r++) for (let c = 0; c < n; c++) if (qr.isDark(r, c)) d += `M${c + QUIET} ${r + QUIET}h1v1h-1z`;
  return { d, extent: n + 2 * QUIET };
});
</script>

<template>
  <svg
    class="qr"
    role="img"
    :aria-label="label"
    :width="size"
    :height="size"
    :viewBox="`0 0 ${code.extent} ${code.extent}`"
    shape-rendering="crispEdges"
  >
    <rect width="100%" height="100%" fill="#fff" />
    <path :d="code.d" fill="#000" />
  </svg>
</template>

<style scoped>
.qr {
  display: block;
  border: 1px solid var(--c-border);
  border-radius: var(--radius);
}
</style>
