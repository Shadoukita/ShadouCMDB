<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { ApiError } from "../../api/client";
import { useCi, useCiClasses, useClassAttributes } from "../../api/queries";
import { useService, useServiceSettings } from "../../api/services";
import Breadcrumbs, { type Crumb } from "../../components/Breadcrumbs.vue";
import CriticalityBadge from "../../components/CriticalityBadge.vue";
import EmptyState from "../../components/EmptyState.vue";
import LoadingState from "../../components/LoadingState.vue";
import { t } from "../../i18n";
import { useAppSettings } from "../../lib/appSettings";
import { useDocumentTitle } from "../../lib/composables";
import type { TrailStep } from "../../lib/trail";
import { builtInLayout, DETAIL_CORE, DETAIL_RECORD, layoutFor, resolveLayout, withoutKinds } from "../../lib/uiSettings";
import { useSessionStore } from "../../stores/session";
import HistoryPanel from "../detail/HistoryPanel.vue";
import ImpactPanel from "../detail/ImpactPanel.vue";
import LayoutPanels from "../detail/LayoutPanels.vue";
import RelationshipGraphPanel from "../detail/RelationshipGraphPanel.vue";
import DeleteServiceButton from "./DeleteServiceButton.vue";
import OwnersCard from "./OwnersCard.vue";
import ServiceError from "./ServiceError.vue";
import ServiceMembersPanel from "./ServiceMembersPanel.vue";

/**
 * A business service (spec §5.3): /services/:id, with the tabs Overview, Members, Impact (its own URL,
 * /services/:id/impact, defaulting to Upstream), Relationship map and History. The service view (owners,
 * counts, limits) and the CI record (attributes, validity, version) load in parallel; the CI record drives
 * the class layout, the map, the impact analysis and the history, exactly as on any CI.
 */
type Tab = "overview" | "members" | "impact" | "graph" | "history";

const route = useRoute();
const router = useRouter();
const session = useSessionStore();
const id = computed(() => String(route.params.id ?? ""));
const onImpactRoute = computed(() => /\/impact\/?$/.test(route.path));
const svc = useService(id);
const ci = useCi(id);
const s = computed(() => svc.data.value);
const c = computed(() => ci.data.value);
useDocumentTitle(() => s.value?.name ?? t("services.title"));

/** The first failure decides the page: the service view's (403/404 for the class or record) before the CI's. */
const error = computed(() => svc.error.value ?? ci.error.value);
const code = computed(() => (error.value instanceof ApiError ? error.value.code : ""));
const forbidden = computed(() => code.value === "FORBIDDEN");
// A malformed id is a params validation error; for the operator it is simply "not found".
const notFound = computed(
  () => code.value === "NOT_FOUND" || (code.value === "VALIDATION_ERROR" && (error.value as ApiError).details.some((d) => d.in === "params")),
);
function retry() {
  void svc.refetch();
  void ci.refetch();
}

const canEdit = computed(() => !!s.value && session.canOnClass(s.value.classId, "edit"));
// The member limits, when the service view does not carry them.
const serviceSettings = useServiceSettings();
const canDelete = computed(() => !!s.value && session.canOnClass(s.value.classId, "delete"));

// The class layout's sections, all on the Overview (a service's tabs are fixed): from Customization if the
// class has a layout, else the built-in one. History and audit sections need audit.view.
const settings = useAppSettings();
const classes = useCiClasses();
const classKey = computed(() => classes.data.value?.find((k) => k.id === c.value?.classId)?.key);
const attrs = useClassAttributes(() => c.value?.classId);
const defs = computed(() => (attrs.data.value ?? []).filter((d) => d.isActive || c.value?.attributes[d.key] != null));
const sections = computed(() => {
  const l = layoutFor(settings.doc.value, classKey.value) ?? builtInLayout(classKey.value ?? "");
  const shown = session.can("audit.view") ? l : withoutKinds(l, ["history", "audit"]);
  return resolveLayout(shown, defs.value, DETAIL_CORE, DETAIL_RECORD).flatMap((tab) => tab.sections);
});
const self = computed<TrailStep | undefined>(() => (c.value ? { id: c.value.id, name: c.value.label } : undefined));

const TABS = computed<[Tab, string][]>(() => [
  ["overview", t("services.tab.overview")],
  ["members", t("services.tab.members", { n: (s.value?.memberCount ?? 0).toLocaleString() })],
  ["impact", t("services.tab.impact")],
  ["graph", t("services.tab.graph")],
  ...(session.can("audit.view") ? [["history", t("services.tab.history")] as [Tab, string]] : []),
]);
const current = computed<Tab>(() => {
  if (onImpactRoute.value) return "impact";
  const q = route.query.tab;
  return TABS.value.some(([k]) => k === q) ? (q as Tab) : "overview";
});
/** Impact on its own URL; the others as `?tab=` on the service's URL (Overview is the plain URL). */
function selectTab(key: Tab) {
  if (key === current.value) return;
  if (key === "impact") void router.push(`/services/${id.value}/impact`);
  else void router.push({ path: `/services/${id.value}`, query: key === "overview" ? {} : { tab: key } });
}
function onTabKey(e: KeyboardEvent) {
  const keys = TABS.value.map(([k]) => k);
  const at = keys.indexOf(current.value);
  const to = { ArrowRight: at + 1, ArrowLeft: at - 1 + keys.length, Home: 0, End: keys.length - 1 }[e.key];
  if (to === undefined) return;
  e.preventDefault();
  const next = keys[to % keys.length];
  selectTab(next);
  void nextTick(() => document.getElementById(`service-tab-${next}`)?.focus());
}

const crumbs = computed<Crumb[]>(() => [{ label: t("services.title"), to: "/services" }, { label: s.value?.name ?? "" }]);

// After "Create business service", the operator lands here with the Owners card open (?edit=owners).
const owners = ref<InstanceType<typeof OwnersCard>>();
watch(
  () => [owners.value, route.query.edit] as const,
  ([card, edit]) => {
    if (!card || edit !== "owners") return;
    // Drop `edit` and keep only the tab; other URL keys are not copied over.
    const tab = route.query.tab;
    void router.replace({ path: route.path, query: typeof tab === "string" ? { tab } : {} });
    if (canEdit.value) card.edit();
  },
  { immediate: true },
);
</script>

<template>
  <LoadingState v-if="svc.isLoading.value || ci.isLoading.value" :label="t('services.detail.loading')" />
  <template v-else-if="error">
    <Breadcrumbs :items="[{ label: t('services.title'), to: '/services' }, { label: forbidden ? t('services.forbiddenTitle') : notFound ? t('services.notFoundTitle') : t('common.error') }]" />
    <EmptyState v-if="forbidden" :title="t('services.forbiddenTitle')">
      {{ t("services.forbidden") }}
      <template #actions><RouterLink class="btn" to="/">{{ t("services.backToDashboard") }}</RouterLink></template>
    </EmptyState>
    <EmptyState v-else-if="notFound" :title="t('services.notFoundTitle')">
      {{ t("services.notFound") }}
      <template #actions><RouterLink class="btn" to="/services">{{ t("services.backToList") }}</RouterLink></template>
    </EmptyState>
    <ServiceError v-else :error="error" :on-retry="retry" />
  </template>
  <template v-else-if="s && c && self">
    <Breadcrumbs :items="crumbs" />
    <div class="page-header">
      <div class="title">
        <h1 dir="auto">{{ s.name }}</h1>
        <span class="mono muted" :title="t('services.col.ident')">{{ s.ident }}</span>
        <span class="badge">{{ t("services.badge") }}</span>
        <CriticalityBadge :value="s.criticality" />
        <span :class="['badge', s.active ? 'ok' : 'off']">{{ s.active ? t("services.state.active") : t("services.state.inactive") }}</span>
      </div>
      <div class="actions">
        <RouterLink v-if="canEdit" class="btn" :to="`/cis/${s.id}/edit`">{{ t("common.edit") }}</RouterLink>
        <DeleteServiceButton v-if="canDelete" :service="s" />
      </div>
    </div>

    <div class="tabs" role="tablist" :aria-label="t('services.tabsLabel')">
      <button
        v-for="[key, label] in TABS"
        :id="`service-tab-${key}`"
        :key="key"
        type="button"
        role="tab"
        :aria-selected="current === key"
        :aria-controls="`service-panel-${key}`"
        :tabindex="current === key ? 0 : -1"
        @click="selectTab(key)"
        @keydown="onTabKey"
      >
        {{ label }}
      </button>
    </div>

    <div :id="`service-panel-${current}`" role="tabpanel" :aria-labelledby="`service-tab-${current}`">
      <template v-if="current === 'overview'">
        <OwnersCard ref="owners" :service="s" :can-edit="canEdit" :on-reload="() => svc.refetch()" />
        <LoadingState v-if="attrs.isLoading.value" :label="t('services.detail.loadingAttributes')" />
        <ServiceError v-else-if="attrs.isError.value" :error="attrs.error.value" :on-retry="() => attrs.refetch()" />
        <LayoutPanels v-else :ci="c" :sections="sections" :defs="defs" :self="self" :trail="[]" orphans />
      </template>
      <ServiceMembersPanel
        v-else-if="current === 'members'"
        :service="s"
        :can-edit="canEdit"
        :limits="s.limits ?? serviceSettings.data.value?.limits"
        :self="self"
        :trail="[]"
      />
      <ImpactPanel v-else-if="current === 'impact'" :ci="c" :self="self" :trail="[]" default-direction="upstream" />
      <RelationshipGraphPanel v-else-if="current === 'graph'" :ci="c" :self="self" :trail="[]" />
      <HistoryPanel v-else :ci="c" />
    </div>
  </template>
</template>
