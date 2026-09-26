<script setup lang="ts">
import { RouterLink } from "vue-router";
import type { Ci } from "../../api/queries";
import { formatDateTime } from "../../lib/format";

/** Read-only value of one built-in CI field. Lookups link to the inventory filtered by them. */
defineProps<{ ci: Ci; field: string }>();
</script>

<template>
  <template v-if="field === 'name'">{{ ci.name }}</template>
  <RouterLink v-else-if="field === 'class'" :to="`/cis?classId=${ci.classId}`">{{ ci.class.name }}</RouterLink>
  <RouterLink v-else-if="field === 'status'" :to="`/cis?statusId=${ci.statusId}`">{{ ci.status.name }}</RouterLink>
  <template v-else-if="field === 'environment'">
    <RouterLink v-if="ci.environment" :to="`/cis?environmentId=${ci.environment.id}`">{{ ci.environment.name }}</RouterLink>
    <span v-else class="muted">—</span>
  </template>
  <template v-else-if="field === 'owner'">
    <RouterLink v-if="ci.owner" :to="`/cis?ownerId=${ci.owner.id}`">{{ ci.owner.name }}</RouterLink>
    <span v-else class="muted">—</span>
  </template>
  <template v-else-if="field === 'location'">
    <RouterLink v-if="ci.location" :to="`/cis?locationId=${ci.location.id}`" title="All CIs at this location">{{ ci.location.name }}</RouterLink>
    <span v-else class="muted">—</span>
  </template>
  <template v-else-if="field === 'hostname' || field === 'ipAddress' || field === 'serialNumber'">
    <span v-if="ci[field]" class="mono">{{ ci[field] }}</span><span v-else class="muted">—</span>
  </template>
  <span v-else-if="field === 'notes'" style="white-space: pre-wrap"><template v-if="ci.notes">{{ ci.notes }}</template><span v-else class="muted">—</span></span>
  <template v-else-if="field === 'createdAt'">{{ formatDateTime(ci.createdAt) }}</template>
  <template v-else-if="field === 'updatedAt'">{{ formatDateTime(ci.updatedAt) }} <span class="muted">· version {{ ci.version }}</span></template>
</template>
