<script setup lang="ts">
import Icon from "./Icon.vue";

/**
 * The sort direction of a column header, from the same value as its aria-sort. A sortable column
 * that is not sorted (`none`) gets a neutral icon that shows on hover and keyboard focus only, so
 * sortable headers can be told from plain ones (audit I3). Nothing for a column that cannot sort.
 */
defineProps<{ dir: "ascending" | "descending" | "none" | undefined }>();
</script>

<template>
  <Icon v-if="dir === 'ascending'" name="arrow-up" :size="14" class="sort-icon" />
  <Icon v-else-if="dir === 'descending'" name="arrow-down" :size="14" class="sort-icon" />
  <Icon v-else-if="dir === 'none'" name="arrow-up-down" :size="14" class="sort-icon idle" />
</template>
