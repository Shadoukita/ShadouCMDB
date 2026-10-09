<script setup lang="ts">
import { RouterLink, useRoute } from "vue-router";
import Breadcrumbs from "../components/Breadcrumbs.vue";
import EmptyState from "../components/EmptyState.vue";
import Icon from "../components/Icon.vue";
import { t } from "../i18n";
import { useDocumentTitle } from "../lib/composables";

// An address that matches no screen: the page head band names it, and the panel below offers the way back (audit F3).
const route = useRoute();
useDocumentTitle(() => t("notFound.documentTitle"));
</script>

<template>
  <div class="record-head record-head-plain">
    <Breadcrumbs :items="[{ label: t('notFound.documentTitle') }]" />
    <div class="page-header record-header">
      <div class="record-heading">
        <span class="class-tile class-tile-lg" aria-hidden="true"><Icon name="circle-alert" class="class-icon" /></span>
        <div class="record-title">
          <div class="title">
            <h1>{{ t("notFound.title") }}</h1>
          </div>
          <p class="record-meta" data-testid="record-meta">
            <span class="badge">{{ t("notFound.status") }}</span>
            <span class="record-meta-line"><span class="mono" dir="ltr" data-testid="not-found-path">{{ route.fullPath }}</span></span>
          </p>
        </div>
      </div>
    </div>
  </div>
  <section class="panel">
    <EmptyState :title="t('notFound.panelTitle')" icon="search">
      {{ t("notFound.body") }}
      <template #actions>
        <RouterLink class="btn btn-primary" to="/"><Icon name="layout-dashboard" />{{ t("notFound.toDashboard") }}</RouterLink>
        <RouterLink class="btn" to="/cis"><Icon name="list" />{{ t("notFound.toInventory") }}</RouterLink>
      </template>
    </EmptyState>
  </section>
</template>

<style scoped>
/* The title names a state, not a record, so it stays in the UI font; the address in the meta line is the data. */
.record-heading h1 {
  font-family: var(--font-sans);
}
</style>
