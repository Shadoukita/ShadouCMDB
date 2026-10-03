<script setup lang="ts">
import Icon from "./Icon.vue";

/** The sort direction of a column header, from the same value as its aria-sort. Nothing when the column is not sorted. */
defineProps<{ dir: "ascending" | "descending" | "none" | undefined }>();
</script>

<template>
  <Icon v-if="dir === 'ascending'" name="arrow-up" :size="14" class="sort-icon" />
  <Icon v-else-if="dir === 'descending'" name="arrow-down" :size="14" class="sort-icon" />
</template>
