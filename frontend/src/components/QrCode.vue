<script setup lang="ts">
import { computed } from "vue";
import { qrMatrix } from "../lib/qr";

/**
 * A QR code drawn as one SVG path, rendered in the browser (the value never
 * leaves it). Always dark on white with a quiet zone, whatever the theme, so
 * phone cameras read it.
 */
const props = withDefaults(defineProps<{ value: string; label: string; size?: number }>(), { size: 200 });

const code = computed(() => qrMatrix(props.value));
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
