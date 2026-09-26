<script setup lang="ts">
import { RouterLink, type RouteLocationRaw } from "vue-router";

export interface Crumb {
  label: string;
  to?: RouteLocationRaw;
}

defineProps<{ items: Crumb[] }>();
</script>

<template>
  <nav class="breadcrumbs" aria-label="Breadcrumb">
    <ol>
      <li><RouterLink to="/">Dashboard</RouterLink></li>
      <li v-for="(c, i) in items" :key="i">
        <RouterLink v-if="c.to && i < items.length - 1" :to="c.to">{{ c.label }}</RouterLink>
        <span v-else :aria-current="i === items.length - 1 ? 'page' : undefined">{{ c.label }}</span>
      </li>
    </ol>
  </nav>
</template>
