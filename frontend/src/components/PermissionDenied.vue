<script setup lang="ts">
import { RouterLink } from "vue-router";
import type { GlobalPermission } from "../api/admin";
import { t } from "../i18n";
import Breadcrumbs, { type Crumb } from "./Breadcrumbs.vue";
import EmptyState from "./EmptyState.vue";
import Icon from "./Icon.vue";

/**
 * A screen the operator may not open (design document §2.7, step 10-7): the page head band with a lock tile, the
 * title, an "Error 403" badge and what is missing — the global permissions by name with their key in mono, or the
 * class right in words — then a panel with the explanation and the way on. The breadcrumb names the screen asked for.
 * Without an actions slot the panel offers the dashboard.
 */
withDefaults(
  defineProps<{
    crumbs?: Crumb[];
    /** Global permission keys, any one of which would open the screen. */
    permissions?: GlobalPermission[];
    /** The missing right in words, when it is not a global permission (a class right, the Administrator profile). */
    requirement?: string;
    panelTitle: string;
  }>(),
  { crumbs: () => [], permissions: () => [], requirement: undefined },
);
</script>

<template>
  <div class="record-head record-head-plain" data-testid="permission-denied">
    <Breadcrumbs v-if="crumbs.length" :items="crumbs" />
    <div class="page-header record-header">
      <div class="record-heading">
        <span class="class-tile class-tile-lg" aria-hidden="true"><Icon name="lock" class="class-icon" /></span>
        <div class="record-title">
          <div class="title">
            <h1>{{ t("denied.title") }}</h1>
          </div>
          <p class="record-meta" data-testid="record-meta">
            <span class="badge danger">{{ t("denied.status") }}</span>
            <span v-if="permissions.length" class="record-meta-line" data-testid="denied-needs">
              {{ t("denied.needs", { n: permissions.length }) }}
              <span v-for="p in permissions" :key="p" class="denied-permission">
                {{ t(`permission.${p}`) }} <code class="mono" dir="ltr">{{ p }}</code>
              </span>
            </span>
            <span v-else-if="requirement" class="record-meta-line" data-testid="denied-needs">{{ requirement }}</span>
          </p>
        </div>
      </div>
    </div>
  </div>
  <section class="panel">
    <EmptyState :title="panelTitle" icon="lock">
      <slot />
      <template #actions>
        <slot name="actions">
          <RouterLink class="btn btn-primary" to="/"><Icon name="layout-dashboard" />{{ t("notFound.toDashboard") }}</RouterLink>
        </slot>
      </template>
    </EmptyState>
  </section>
</template>

<style scoped>
/* The title names a state, not a record, so it stays in the UI font (as on the 404 page). */
.record-heading h1 {
  font-family: var(--font-sans);
}
.denied-permission + .denied-permission::before {
  content: "· ";
}
</style>
