<script setup lang="ts">
import { computed } from "vue";
import { t } from "../i18n";
import { criticalityTone } from "../lib/impact";
import { meterFill } from "../lib/pagination";

/**
 * A CI's criticality as a bar meter and its name (design document §0, step 12c): rank 1 (most critical)
 * fills every bar. The meter is decorative; the name carries the value, so it never relies on colour.
 * `levels` is the number of ranks in the criticality list.
 */
const props = defineProps<{ value: { rank: number; name: string }; levels: number }>();
const bars = computed(() => Math.max(1, props.levels));
const filled = computed(() => meterFill(props.value.rank, bars.value));
</script>

<template>
  <span :class="['crit-meter', criticalityTone(value.rank)]" :title="t('criticality.title', { name: value.name })">
    <span class="crit-bars" aria-hidden="true"><span v-for="i in bars" :key="i" :class="{ on: i <= filled }" /></span>
    <bdi>{{ value.name }}</bdi>
  </span>
</template>
