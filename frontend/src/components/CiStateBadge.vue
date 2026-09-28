<script setup lang="ts">
/**
 * "Deleted" or "Inactive" (outside its validity period) badge of a CI; nothing for an active one
 * unless `showActive` is set.
 */
defineProps<{ ci: { active: boolean; deletedAt?: string | null }; showActive?: boolean }>();
</script>

<template>
  <span v-if="ci.deletedAt" class="badge danger">Deleted</span>
  <span v-else-if="!ci.active" class="badge off" title="Outside its validity period">Inactive</span>
  <span v-else-if="showActive" class="badge ok">Active</span>
</template>
