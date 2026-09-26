<script setup lang="ts">
import { computed } from "vue";
import { classIcon } from "../lib/classIcons";

/** A CI class's icon in its colour (a plain swatch when it has a colour but no known icon), optionally followed by the name. */
const props = defineProps<{ icon?: string | null; color?: string | null; name?: string }>();
const glyph = computed(() => classIcon(props.icon));
</script>

<template>
  <span class="class-badge">
    <svg v-if="glyph" class="class-icon" viewBox="0 0 16 16" aria-hidden="true" :style="color ? { color } : undefined">
      <path v-for="(d, i) in glyph.paths" :key="i" :d="d" />
    </svg>
    <span v-else-if="color" class="class-swatch" aria-hidden="true" :style="{ background: color }" />
    <span v-if="name">{{ name }}</span>
  </span>
</template>
