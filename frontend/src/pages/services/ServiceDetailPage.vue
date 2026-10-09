<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { onBeforeRouteLeave, onBeforeRouteUpdate, RouterLink, useRoute, useRouter, type RouteLocationNormalized } from "vue-router";
import { ApiError } from "../../api/client";
import { useCi, useCiClasses, useClassAttributes } from "../../api/queries";
import { useService, useServiceSettings } from "../../api/services";
import Breadcrumbs, { type Crumb } from "../../components/Breadcrumbs.vue";
import CiStateBadge from "../../components/CiStateBadge.vue";
import ClassBadge from "../../components/ClassBadge.vue";
import CriticalityBadge from "../../components/CriticalityBadge.vue";
import EmptyState from "../../components/EmptyState.vue";
import PermissionDenied from "../../components/PermissionDenied.vue";
import LoadingState from "../../components/LoadingState.vue";
import RowMenu, { type RowMenuItem } from "../../components/RowMenu.vue";
import SaveBar from "../../components/SaveBar.vue";
import { t } from "../../i18n";
import { useAppSettings } from "../../lib/appSettings";
import { useDocumentTitle } from "../../lib/composables";
import { formatDateTime, formatRelative } from "../../lib/format";
import type { TrailStep } from "../../lib/trail";
import { builtInLayout, DETAIL_CORE, DETAIL_RECORD, layoutFor, resolveLayout, withoutKinds } from "../../lib/uiSettings";
import { useFlashStore } from "../../stores/flash";
import { useSessionStore } from "../../stores/session";
import HistoryPanel from "../detail/HistoryPanel.vue";
import ImpactPanel from "../detail/ImpactPanel.vue";
import LayoutPanels from "../detail/LayoutPanels.vue";
import RecordStats from "../detail/RecordStats.vue";
import RelationshipGraphPanel from "../detail/RelationshipGraphPanel.vue";
import { fieldIdFor, useCiDraft } from "../form/ciDraft";
import FormErrorBanner from "../form/FormErrorBanner.vue";
import DeleteServiceDialog from "./DeleteServiceDialog.vue";
import OwnersCard from "./OwnersCard.vue";
import ServiceError from "./ServiceError.vue";
import ServiceMembersPanel from "./ServiceMembersPanel.vue";

/**
 * A business service (spec §5.3): /services/:id, with the tabs Overview, Members, Impact (its own URL,
 * /services/:id/impact, defaulting to Upstream), Relationship map and History. The service view (owners,
 * counts, limits) and the CI record (attributes, validity, version) load in parallel; the CI record drives
 * the class layout, the map, the impact analysis and the history, exactly as on any CI. Like a CI's page
 * (SHAA-1644, GH#588) the Overview's fields open as inputs, and once something was changed a bar offers
 * Save and Discard; leaving the service with unsaved changes asks first.
 *
 * The title row is the record page's (design §2.7, audit B2 and R2): class icon, name and a meta line
 * (state, ident, class, criticality, last update), the views as secondary buttons, Delete in the `⋯`
 * menu; then the stat tiles.
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
const cls = computed(() => classes.data.value?.find((k) => k.id === c.value?.classId));
const classKey = computed(() => cls.value?.key);
const attrs = useClassAttributes(() => c.value?.classId);
const defs = computed(() => (attrs.data.value ?? []).filter((d) => d.isActive || c.value?.attributes[d.key] != null));
const layout = computed(() => layoutFor(settings.doc.value, classKey.value));
const sections = computed(() => {
  const l = layout.value ?? builtInLayout(classKey.value ?? "");
  const shown = session.can("audit.view") ? l : withoutKinds(l, ["history", "audit"]);
  return resolveLayout(shown, defs.value, DETAIL_CORE, DETAIL_RECORD).flatMap((tab) => tab.sections);
});
const self = computed<TrailStep | undefined>(() => (c.value ? { id: c.value.id, name: c.value.label } : undefined));

// The service's values being edited on the Overview (ciDraft.ts, as on a CI's page).
const flash = useFlashStore();
const draft = useCiDraft({
  mode: "edit",
  classId: () => c.value?.classId,
  ci: () => c.value,
  attrs: () => attrs.data.value,
  readOnlyFields: () => layout.value?.readOnlyFields,
  locked: () => !canEdit.value || !!c.value?.deletedAt,
});
const sectionFields = () => new Set(sections.value.flatMap((sec) => sec.fields.map((f) => f.field)));
async function focusField(field: string) {
  if (current.value !== "overview") selectTab("overview");
  await nextTick();
  document.getElementById(fieldIdFor(field))?.focus();
}
async function onSave() {
  draft.error = null;
  // Catch empty required fields before the round trip; everything else is validated by the API.
  const missing = draft.checkRequired(sectionFields());
  if (missing.length > 0) {
    await focusField(missing[0]);
    return;
  }
  try {
    const saved = await draft.save();
    if (!saved) return;
    draft.reset(saved);
    // The header (name, criticality, state) comes from the service view.
    void svc.refetch();
    flash.show(t("services.detail.saved", { name: saved.label }));
  } catch (err) {
    draft.error = err;
    if (current.value !== "overview") selectTab("overview");
    window.scrollTo({ top: 0 });
  }
}
/** After a version conflict: the service as saved now, with the operator's changes dropped. */
async function loadCurrent() {
  const r = await ci.refetch();
  if (r.data) draft.reset(r.data);
}

// Unsaved changes: confirm before leaving this service in the app (its tabs are the same page), and let the
// browser ask before a reload or closing the tab.
const keepChanges = (to: RouteLocationNormalized) =>
  !draft.dirty || String(to.params.id ?? "") === id.value || window.confirm(t("services.detail.leave", { name: s.value?.name ?? "" }));
onBeforeRouteLeave(keepChanges);
onBeforeRouteUpdate(keepChanges);
function onBeforeUnload(e: BeforeUnloadEvent) {
  if (!draft.dirty) return;
  e.preventDefault();
  e.returnValue = "";
}
onMounted(() => window.addEventListener("beforeunload", onBeforeUnload));
onBeforeUnmount(() => window.removeEventListener("beforeunload", onBeforeUnload));

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

// The title row's actions (audit R2): the views as secondary buttons, Delete in the overflow menu.
const deleting = ref(false);
const moreActions = computed<RowMenuItem[]>(() => (canDelete.value ? [{ label: t("common.delete"), danger: true, action: () => (deleting.value = true) }] : []));
const hasTab = (key: Tab) => TABS.value.some(([k]) => k === key);

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
    <PermissionDenied
      v-if="forbidden"
      :crumbs="[{ label: t('services.title'), to: '/services' }]"
      :requirement="t('denied.serviceView')"
      :panel-title="t('services.forbiddenPanelTitle')"
    >
      {{ t("services.forbidden") }}
    </PermissionDenied>
    <template v-else>
      <Breadcrumbs :items="[{ label: t('services.title'), to: '/services' }, { label: notFound ? t('services.notFoundTitle') : t('common.error') }]" />
      <EmptyState v-if="notFound" :title="t('services.notFoundTitle')">
        {{ t("services.notFound") }}
        <template #actions><RouterLink class="btn" to="/services">{{ t("services.backToList") }}</RouterLink></template>
      </EmptyState>
      <ServiceError v-else :error="error" :on-retry="retry" />
    </template>
  </template>
  <template v-else-if="s && c && self">
    <Breadcrumbs :items="crumbs" />
    <div class="page-header record-header">
      <div class="record-heading">
        <div class="title">
          <ClassBadge :icon="cls?.icon" :color="cls?.color" />
          <h1 dir="auto">{{ s.name }}</h1>
        </div>
        <p class="record-meta" data-testid="record-meta">
          <span v-if="c.active" class="status"><span class="status-dot ok" aria-hidden="true" />{{ t("ciState.active") }}</span>
          <CiStateBadge :ci="c" />
          <span class="sep" aria-hidden="true">·</span>
          <span class="ident" :title="t('record.meta.ident')">{{ s.ident }}</span>
          <span class="sep" aria-hidden="true">·</span>
          <RouterLink to="/services" dir="auto">{{ c.class.name }}</RouterLink>
          <template v-if="s.criticality">
            <span class="sep" aria-hidden="true">·</span>
            <CriticalityBadge :value="s.criticality" />
          </template>
          <span class="sep" aria-hidden="true">·</span>
          <time :datetime="s.updatedAt" :title="formatDateTime(s.updatedAt)">{{ t("record.meta.updated", { when: formatRelative(s.updatedAt) }) }}</time>
        </p>
      </div>
      <div class="actions">
        <RouterLink v-if="current !== 'impact'" class="btn" :to="`/services/${s.id}/impact`">{{ t("record.actions.impact") }}</RouterLink>
        <button v-if="current !== 'graph'" type="button" class="btn" @click="selectTab('graph')">{{ t("record.actions.map") }}</button>
        <button v-if="hasTab('history') && current !== 'history'" type="button" class="btn" @click="selectTab('history')">{{ t("record.actions.history") }}</button>
        <RowMenu v-if="moreActions.length > 0" :label="t('record.actions.more')" :items="moreActions" large />
        <DeleteServiceDialog v-if="moreActions.length > 0" v-model:open="deleting" :service="s" />
      </div>
    </div>
    <RecordStats :ci="c" :service="s" />
    <FormErrorBanner v-if="draft.error != null && draft.dirty" :error="draft.error" :unplaced="draft.unplaced" :on-reload="loadCurrent" />

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
        <LayoutPanels v-else :ci="c" :sections="sections" :defs="defs" :self="self" :trail="[]" orphans :draft="draft" />
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
      <RelationshipGraphPanel v-else-if="current === 'graph'" :ci="c" :self="self" :trail="[]" impact-direction="upstream" />
      <HistoryPanel v-else :ci="c" />
    </div>
    <SaveBar v-if="draft.dirty || draft.pending" :label="t('services.detail.unsaved')" dirty :changes="draft.changeCount">
      <button type="button" class="btn" :disabled="draft.pending" @click="draft.reset(c)">{{ t("services.detail.discard") }}</button>
      <button type="button" class="btn btn-primary" :disabled="draft.pending" @click="onSave">
        {{ draft.pending ? t("common.saving") : t("services.detail.save") }}
      </button>
    </SaveBar>
  </template>
</template>
