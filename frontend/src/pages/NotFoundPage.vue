<script setup lang="ts">
import { RouterLink } from "vue-router";
import Breadcrumbs from "../components/Breadcrumbs.vue";
import EmptyState from "../components/EmptyState.vue";
import { useDocumentTitle } from "../lib/composables";

useDocumentTitle("Not found");
</script>

<template>
  <Breadcrumbs :items="[{ label: 'Not found' }]" />
  <EmptyState title="Page not found">
    This address does not match any screen.
    <template #actions><RouterLink class="btn" to="/">Go to the dashboard</RouterLink></template>
  </EmptyState>
</template>
