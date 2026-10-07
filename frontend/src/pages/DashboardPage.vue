<script setup lang="ts">
import { useQuery } from "@tanstack/vue-query";
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { useLookupLists } from "../api/datamodel";
import { ciCountQuery, useCiClasses } from "../api/queries";
import type { UiWidget } from "../api/uiSettings";
import { dataModelEmpty } from "../lib/dataModel";
import Breadcrumbs from "../components/Breadcrumbs.vue";
import DataModelEmpty from "../components/DataModelEmpty.vue";
import EmptyState from "../components/EmptyState.vue";
import ErrorAlert from "../components/ErrorAlert.vue";
import Icon from "../components/Icon.vue";
import LoadingState from "../components/LoadingState.vue";
import { t } from "../i18n";
import { useAppSettings } from "../lib/appSettings";
import { useDocumentTitle } from "../lib/composables";
import { builtInWidgets } from "../lib/uiSettings";
import { useSessionStore } from "../stores/session";
import { useBrandingStore } from "../stores/branding";
import DashboardStats from "./dashboard/DashboardStats.vue";
import DashboardWidgets from "./dashboard/DashboardWidgets.vue";

/**
 * Operational overview: stat tiles, then one widget grid. Counts are server-side (each is a limit=1 list
 * request reading page.total), so they stay correct at any inventory size. Customization › Dashboard can
 * replace the built-in widgets with its own; both render through the same grid (audit D1).
 */
useDocumentTitle(() => t("dashboard.title"));
const session = useSessionStore();
const branding = useBrandingStore();
const settings = useAppSettings();
const total = useQuery(ciCountQuery({}));

const classes = useCiClasses();
/** A fresh install: no classes but the built-in ones yet, so the first step is the data model, not a CI. */
const noClasses = computed(() => !!classes.data.value && dataModelEmpty(classes.data.value));
const hasCis = computed(() => !noClasses.value && total.data.value !== undefined && total.data.value > 0);

// The built-in widgets count by status when there is a lookup list with key "status" (the starter templates').
const lookupLists = useLookupLists();
/** Null while loading: neither the built-in widgets nor the status widget pop in and out. */
const widgets = computed<UiWidget[] | null>(() => {
  if (settings.query.isLoading.value) return null;
  const custom = settings.doc.value.dashboard.widgets;
  if (custom) return custom;
  if (lookupLists.isLoading.value) return null;
  return builtInWidgets(lookupLists.data.value?.find((l) => l.key === "status")?.key);
});
</script>

<template>
  <Breadcrumbs :items="[]" />
  <template v-if="total.isError.value">
    <div class="page-header"><h1>{{ t("dashboard.title") }}</h1></div>
    <ErrorAlert :error="total.error.value" :on-retry="() => total.refetch()" />
  </template>
  <template v-else>
    <div class="page-header">
      <div class="title"><h1>{{ t("dashboard.title") }}</h1></div>
      <div class="actions">
        <RouterLink class="btn" to="/cis">{{ t("dashboard.openInventory") }}</RouterLink>
        <RouterLink v-if="session.canOnAnyClass('create') && !noClasses" class="btn btn-primary" to="/cis/new"><Icon name="plus" :size="16" />{{ t("shell.newCi") }}</RouterLink>
      </div>
    </div>

    <LoadingState v-if="total.isLoading.value" />
    <section v-if="noClasses" class="panel callout">
      <DataModelEmpty />
    </section>
    <section v-else-if="total.data.value === 0 && classes.data.value" class="panel">
      <EmptyState :title="t('dashboard.welcome.title', { app: branding.effective.appName })" icon="server">
        {{ t("dashboard.welcome.body") }}
        <template v-if="session.canOnAnyClass('create')" #actions>
          <RouterLink class="btn btn-primary" to="/cis/new"><Icon name="plus" :size="16" />{{ t("dashboard.welcome.create") }}</RouterLink>
        </template>
      </EmptyState>
    </section>
    <template v-if="hasCis">
      <DashboardStats />
      <DashboardWidgets v-if="widgets && widgets.length > 0" :widgets="widgets" />
      <section v-else-if="widgets" class="panel">
        <EmptyState :title="t('dashboard.noWidgets.title')" icon="layout-dashboard">
          {{ t("dashboard.noWidgets.body") }}
          <template v-if="session.can('customization.manage')" #actions>
            <RouterLink class="btn" to="/admin/customization/dashboard">{{ t("dashboard.noWidgets.add") }}</RouterLink>
          </template>
        </EmptyState>
      </section>
      <LoadingState v-else />
    </template>
  </template>
</template>
