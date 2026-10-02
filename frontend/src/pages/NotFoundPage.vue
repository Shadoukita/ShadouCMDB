<script setup lang="ts">
import { RouterLink } from "vue-router";
import Breadcrumbs from "../components/Breadcrumbs.vue";
import EmptyState from "../components/EmptyState.vue";
import { t } from "../i18n";
import { useDocumentTitle } from "../lib/composables";

useDocumentTitle(() => t("notFound.documentTitle"));
</script>

<template>
  <Breadcrumbs :items="[{ label: t('notFound.documentTitle') }]" />
  <EmptyState :title="t('notFound.title')">
    {{ t("notFound.body") }}
    <template #actions><RouterLink class="btn" to="/">{{ t("notFound.toDashboard") }}</RouterLink></template>
  </EmptyState>
</template>
