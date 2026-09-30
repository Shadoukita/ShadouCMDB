<script setup lang="ts">
import { computed } from "vue";
import { criticalityTone, NOT_SET } from "../lib/impact";

/**
 * A CI's criticality (the system lookup list `criticality`): always its text, never colour alone.
 * The tone follows the rank (1 most critical); every tone is a badge token pair with AA contrast.
 * Accepts the CI's `CriticalityRef` (name) or the impact analysis's (label).
 */
const props = defineProps<{ value: { rank: number; name?: string; label?: string } | null | undefined; showUnset?: boolean }>();
const text = computed(() => props.value?.label ?? props.value?.name ?? "");
</script>

<template>
  <span v-if="value" :class="['badge', 'criticality', criticalityTone(value.rank)]" :title="`Criticality: ${text}`" dir="auto">{{ text }}</span>
  <span v-else-if="showUnset" class="muted">{{ NOT_SET }}</span>
</template>
