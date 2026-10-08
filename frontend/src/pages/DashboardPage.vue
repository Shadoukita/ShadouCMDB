<script setup lang="ts">
import { useQuery } from "@tanstack/vue-query";
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
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
import { currentLocale, t } from "../i18n";
import { useAppSettings } from "../lib/appSettings";
import { useDocumentTitle } from "../lib/composables";
import { DEFAULT_PERIOD, PERIODS, parsePeriod, partOfDay, type Period } from "../lib/dashboard";
import { builtInWidgets } from "../lib/uiSettings";
import { useSessionStore } from "../stores/session";
import { useBrandingStore } from "../stores/branding";
import ChangesChart from "./dashboard/ChangesChart.vue";
import DashboardKpis from "./dashboard/DashboardKpis.vue";
import DashboardWidgets from "./dashboard/DashboardWidgets.vue";
import NeedsAttention from "./dashboard/NeedsAttention.vue";

/**
 * Operational overview (design §0 step 12e): the date and a greeting, the period switch, the KPI cards, then
 * one widget grid that starts with the changes chart. Every figure is counted by the server, so it stays
 * correct at any inventory size. The period is in the URL (`?period=24h|14d|90d`), so a view can be shared.
 * Customization › Dashboard can replace the built-in widgets with its own; both render through the same grid.
 * The dark "Needs attention" panel (gap G3) sits beside the recent activity, with or without customized widgets.
 */
useDocumentTitle(() => t("dashboard.title"));
const session = useSessionStore();
const branding = useBrandingStore();
const settings = useAppSettings();
const route = useRoute();
const router = useRouter();
const total = useQuery(ciCountQuery({}));

const period = computed(() => parsePeriod(route.query.period));
function setPeriod(p: Period) {
  void router.replace({ query: { ...route.query, period: p === DEFAULT_PERIOD ? undefined : p } });
}

// The windows move with the clock: re-read it every minute, so a new hour or day joins the figures.
const now = ref(Date.now());
let clock: ReturnType<typeof setInterval> | undefined;
onMounted(() => (clock = setInterval(() => (now.value = Date.now()), 60_000)));
onBeforeUnmount(() => clearInterval(clock));

const today = computed(() =>
  new Intl.DateTimeFormat(currentLocale() === "de" ? "de" : undefined, { weekday: "long", day: "numeric", month: "long" }).format(now.value),
);
const greeting = computed(() => t(`dashboard.greeting.${partOfDay(new Date(now.value).getHours())}`));

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
  <div class="page-header dash-head">
    <div class="title">
      <p class="overline">{{ today }}</p>
      <!-- The page's name first, for screen readers and the heading list; the greeting is what shows. -->
      <h1 class="display"><span class="sr-only">{{ t("dashboard.title") }}: </span>{{ greeting }}</h1>
    </div>
    <div v-if="hasCis" class="segmented period-switch" role="radiogroup" :aria-label="t('dashboard.period')">
      <label v-for="p in PERIODS" :key="p">
        <input type="radio" name="dashboard-period" :value="p" :checked="period === p" @change="setPeriod(p)" />{{ t(`dashboard.period.${p}`) }}
      </label>
    </div>
  </div>

  <ErrorAlert v-if="total.isError.value" :error="total.error.value" :on-retry="() => total.refetch()" />
  <template v-else>
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
      <DashboardKpis :period="period" :now="now" />
      <DashboardWidgets v-if="widgets && widgets.length > 0" :widgets="widgets">
        <template v-if="session.can('audit.view')" #lead>
          <div class="widget widget-medium" data-widget="changes"><ChangesChart :period="period" :now="now" /></div>
        </template>
        <template #aside><NeedsAttention /></template>
      </DashboardWidgets>
      <template v-else-if="widgets">
        <div class="widgets">
          <div v-if="session.can('audit.view')" class="widget widget-beside-aside" data-widget="changes"><ChangesChart :period="period" :now="now" /></div>
          <div :class="['widget', session.can('audit.view') ? 'widget-aside' : 'widget-medium']"><NeedsAttention /></div>
        </div>
        <section class="panel">
          <EmptyState :title="t('dashboard.noWidgets.title')" icon="layout-dashboard">
            {{ t("dashboard.noWidgets.body") }}
            <template v-if="session.can('customization.manage')" #actions>
              <RouterLink class="btn" to="/admin/customization/dashboard">{{ t("dashboard.noWidgets.add") }}</RouterLink>
            </template>
          </EmptyState>
        </section>
      </template>
      <LoadingState v-else />
    </template>
  </template>
</template>
