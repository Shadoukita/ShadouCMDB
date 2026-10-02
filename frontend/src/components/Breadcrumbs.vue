<script setup lang="ts">
import { RouterLink, type RouteLocationRaw } from "vue-router";
import { t } from "../i18n";

export interface Crumb {
  label: string;
  to?: RouteLocationRaw;
}

defineProps<{ items: Crumb[] }>();
</script>

<template>
  <nav class="breadcrumbs" :aria-label="t('shell.breadcrumb')">
    <ol>
      <li><RouterLink to="/">{{ t("nav.page.dashboard") }}</RouterLink></li>
      <li v-for="(c, i) in items" :key="i">
        <RouterLink v-if="c.to && i < items.length - 1" :to="c.to" dir="auto">{{ c.label }}</RouterLink>
        <span v-else :aria-current="i === items.length - 1 ? 'page' : undefined" dir="auto">{{ c.label }}</span>
      </li>
    </ol>
  </nav>
</template>
