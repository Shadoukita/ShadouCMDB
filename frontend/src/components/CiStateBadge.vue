<script setup lang="ts">
import { computed } from "vue";
import { formatDateTime } from "../lib/format";

/**
 * "Deleted" or "Inactive" (outside its validity period) badge of a CI; nothing for an active one
 * unless `showActive` is set. A validity period that starts or ends in the future says when
 * ("Activates on …", "Deactivates on …") when the CI's validFrom/validUntil are given.
 */
const props = defineProps<{
  ci: { active: boolean; deletedAt?: string | null; validFrom?: string; validUntil?: string | null };
  showActive?: boolean;
}>();
const future = (iso: string | null | undefined) => !!iso && new Date(iso).getTime() > Date.now();
const activates = computed(() => (!props.ci.active && future(props.ci.validFrom) ? props.ci.validFrom! : null));
const deactivates = computed(() => (props.ci.active && future(props.ci.validUntil) ? props.ci.validUntil! : null));
</script>

<template>
  <span v-if="ci.deletedAt" class="badge danger">Deleted</span>
  <template v-else-if="!ci.active">
    <span class="badge off" title="Outside its validity period">Inactive</span>
    <span v-if="activates" class="muted"> · activates on {{ formatDateTime(activates) }}</span>
  </template>
  <template v-else-if="showActive || deactivates">
    <span v-if="showActive" class="badge ok">Active</span>
    <span v-if="deactivates" :class="showActive ? 'muted' : 'badge warn'" :title="`Valid until ${formatDateTime(deactivates)}`">
      {{ showActive ? " · deactivates" : "Deactivates" }} on {{ formatDateTime(deactivates) }}
    </span>
  </template>
</template>
