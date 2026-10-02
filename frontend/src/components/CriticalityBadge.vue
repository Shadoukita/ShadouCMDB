<script setup lang="ts">
import { computed } from "vue";
import { t } from "../i18n";
import { criticalityNotSet } from "../lib/ciState";
import { criticalityTone } from "../lib/impact";

/**
 * A CI's criticality (the system lookup list `criticality`): always its text, never colour alone.
 * The tone follows the rank (1 most critical); every tone is a badge token pair with AA contrast.
 * Accepts the CI's `CriticalityRef` (name) or the impact analysis's (label).
 */
const props = defineProps<{ value: { rank: number; name?: string; label?: string } | null | undefined; showUnset?: boolean }>();
const text = computed(() => props.value?.label ?? props.value?.name ?? "");
</script>

<template>
  <span v-if="value" :class="['badge', 'criticality', criticalityTone(value.rank)]" :title="t('criticality.title', { name: text })" dir="auto">{{ text }}</span>
  <span v-else-if="showUnset" class="muted">{{ criticalityNotSet() }}</span>
</template>
