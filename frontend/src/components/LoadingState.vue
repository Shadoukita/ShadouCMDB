<script setup lang="ts">
withDefaults(defineProps<{ label?: string }>(), { label: "Loading…" });
</script>

<template>
  <div class="state" role="status" aria-live="polite"><span class="spinner" aria-hidden="true" /> {{ label }}</div>
</template>
