<script setup lang="ts">
/**
 * A table that is still loading: placeholder rows at the row height, so the page does not jump when the
 * data arrives (design document §2.7, audit I9). The label is announced once through the status role.
 */
withDefaults(defineProps<{ label: string; rows?: number }>(), { rows: 8 });
</script>

<template>
  <div class="skeleton-table padded" role="status" aria-live="polite">
    <span class="sr-only">{{ label }}</span>
    <div v-for="i in rows" :key="i" class="skeleton-row" aria-hidden="true" />
  </div>
</template>
