<script setup lang="ts">
import { computed, ref, watch } from "vue";

/**
 * A determinate progress bar for a running phase (§1.4): role=progressbar with the row counts as value text,
 * and a polite live region that speaks at most every 10 % and once at the end, never on every poll.
 * Without a total yet (the file is still being read) the bar is indeterminate.
 */
const props = defineProps<{ label: string; done: number; total: number | null; finished?: string }>();

const percent = computed(() => (props.total ? Math.min(100, Math.floor((props.done / props.total) * 100)) : null));
const valueText = computed(() =>
  props.total ? `${props.done.toLocaleString()} of ${props.total.toLocaleString()} rows` : `${props.done.toLocaleString()} rows so far`,
);

const announced = ref("");
let lastDecile = -1;
watch(
  percent,
  (p) => {
    if (p === null) return;
    const decile = Math.floor(p / 10);
    if (decile > lastDecile && lastDecile >= 0 && p < 100) announced.value = `${props.label}: ${p} %`;
    lastDecile = Math.max(lastDecile, decile);
  },
  { immediate: true },
);
watch(
  () => props.finished,
  (f) => {
    if (f) announced.value = f;
  },
);
</script>

<template>
  <div class="import-progress">
    <div class="import-progress-label">
      <span>{{ label }}</span>
      <span class="muted">{{ valueText }}</span>
    </div>
    <div
      class="import-progress-track"
      role="progressbar"
      :aria-label="label"
      aria-valuemin="0"
      :aria-valuemax="total ?? undefined"
      :aria-valuenow="total ? done : undefined"
      :aria-valuetext="valueText"
    >
      <div v-if="percent !== null" class="import-progress-fill" :style="{ width: `${percent}%` }" />
      <div v-else class="import-progress-fill indeterminate" />
    </div>
    <div class="sr-only" aria-live="polite">{{ announced }}</div>
  </div>
</template>
