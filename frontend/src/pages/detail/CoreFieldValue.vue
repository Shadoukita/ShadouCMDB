<script setup lang="ts">
import { RouterLink } from "vue-router";
import type { Ci } from "../../api/queries";
import CiStateBadge from "../../components/CiStateBadge.vue";
import { formatDateTime } from "../../lib/format";

/** Read-only value of one built-in CI field. The class links to the inventory filtered by it. */
defineProps<{ ci: Ci; field: string }>();
</script>

<template>
  <bdi v-if="field === 'label'">{{ ci.label }}</bdi>
  <span v-else-if="field === 'ident'" class="mono">{{ ci.ident }}</span>
  <RouterLink v-else-if="field === 'class'" :to="`/cis?classId=${ci.classId}`" dir="auto">{{ ci.class.name }}</RouterLink>
  <template v-else-if="field === 'validFrom'">{{ formatDateTime(ci.validFrom) }}</template>
  <template v-else-if="field === 'validUntil'">
    <template v-if="ci.validUntil">{{ formatDateTime(ci.validUntil) }}</template><span v-else class="muted">Open-ended</span>
  </template>
  <CiStateBadge v-else-if="field === 'active'" :ci="ci" show-active />
  <template v-else-if="field === 'createdAt'">{{ formatDateTime(ci.createdAt) }}</template>
  <template v-else-if="field === 'updatedAt'">{{ formatDateTime(ci.updatedAt) }} <span class="muted">· version {{ ci.version }}</span></template>
</template>
