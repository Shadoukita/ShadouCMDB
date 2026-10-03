<script setup lang="ts">
import { computed } from "vue";
import { classIcon } from "../lib/classIcons";
import Icon from "./Icon.vue";

/**
 * A CI class's icon in its colour (a plain swatch when it has a colour but no known icon), optionally followed by the name.
 * `plain` draws the icon in the surrounding text colour instead, for dark surfaces such as the sidebar, where a
 * class colour picked for light backgrounds may not be visible.
 */
const props = defineProps<{ icon?: string | null; color?: string | null; name?: string; plain?: boolean }>();
const glyph = computed(() => classIcon(props.icon));
</script>

<template>
  <span class="class-badge">
    <Icon v-if="glyph" :name="glyph.icon" class="class-icon" :style="color && !plain ? { color } : undefined" />
    <span v-else-if="color" class="class-swatch" aria-hidden="true" :style="{ background: color }" />
    <span v-if="name">{{ name }}</span>
  </span>
</template>
