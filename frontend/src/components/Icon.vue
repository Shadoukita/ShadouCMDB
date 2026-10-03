<script setup lang="ts">
import { computed } from "vue";
import { ICONS, type IconName } from "../icons/lucide";

/**
 * One icon from the vendored Lucide subset (src/icons/), drawn inline in currentColor.
 * Decorative by default (aria-hidden): the button or text next to it carries the meaning.
 * Pass `label` only when the icon stands alone and conveys something, e.g. a status.
 */
const props = withDefaults(defineProps<{ name: IconName; size?: number; label?: string }>(), { size: 16, label: undefined });

const nodes = computed(() => ICONS[props.name]);
// Lucide's 2 px stroke is drawn for 24 px; thinner strokes keep small icons from looking heavy.
const stroke = computed(() => (props.size >= 20 ? 1.6 : 1.75));
</script>

<template>
  <svg
    class="icon"
    :width="size"
    :height="size"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    :stroke-width="stroke"
    stroke-linecap="round"
    stroke-linejoin="round"
    focusable="false"
    :role="label ? 'img' : undefined"
    :aria-label="label"
    :aria-hidden="label ? undefined : 'true'"
  >
    <component :is="tag" v-for="([tag, attrs], i) in nodes" :key="i" v-bind="attrs" />
  </svg>
</template>
