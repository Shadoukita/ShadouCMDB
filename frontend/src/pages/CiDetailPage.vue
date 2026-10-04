<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { onBeforeRouteLeave, onBeforeRouteUpdate, RouterLink, useRoute, useRouter, type RouteLocationNormalized } from "vue-router";
import { ApiError } from "../api/client";
import { useAreas } from "../api/datamodel";
import { useCi, useCiClasses, useClassAttributes } from "../api/queries";
import { useServiceSettings } from "../api/services";
import { useCiWorkflows } from "../api/workflowRuntime";
import { useCiLayout } from "../api/uiSettings";
import Breadcrumbs, { type Crumb } from "../components/Breadcrumbs.vue";
import EmptyState from "../components/EmptyState.vue";
import ErrorAlert from "../components/ErrorAlert.vue";
import LoadingState from "../components/LoadingState.vue";
import CiStateBadge from "../components/CiStateBadge.vue";
import CriticalityBadge from "../components/CriticalityBadge.vue";
import LayoutEditView from "../components/layoutEdit/LayoutEditView.vue";
import EditLayoutButton from "../components/layoutEdit/EditLayoutButton.vue";
import { t } from "../i18n";
import { useAppSettings } from "../lib/appSettings";
import { useDocumentTitle } from "../lib/composables";
import { useLayoutEditor } from "../lib/layoutEditor";
import { asClassLayout } from "../lib/layoutTemplates";
import { formatDateTime } from "../lib/format";
import { useTrail, type TrailStep } from "../lib/trail";
import { attributeKey, builtInLayout, DETAIL_CORE, DETAIL_RECORD, layoutFor, normalizeLayout, placedPanels, resolveLayout, withoutKinds } from "../lib/uiSettings";
import { useFlashStore } from "../stores/flash";
import { useSessionStore } from "../stores/session";
import { fieldIdFor, useCiDraft } from "./form/ciDraft";
import FormErrorBanner from "./form/FormErrorBanner.vue";
import AttributeValue from "./detail/AttributeValue.vue";
import BlockContent from "./detail/BlockContent.vue";
import CoreFieldValue from "./detail/CoreFieldValue.vue";
import DeleteCiButton from "./detail/DeleteCiButton.vue";
import HistoryPanel from "./detail/HistoryPanel.vue";
import ImpactPanel from "./detail/ImpactPanel.vue";
import LayoutPanels from "./detail/LayoutPanels.vue";
import PartOfServicesPanel from "./detail/PartOfServicesPanel.vue";
import RelationshipGraphPanel from "./detail/RelationshipGraphPanel.vue";
import RelationshipsPanel from "./detail/RelationshipsPanel.vue";
import SignInAccountPanel from "./detail/SignInAccountPanel.vue";
import CiWorkflowsPanel from "./workflows/CiWorkflowsPanel.vue";

/**
 * A CI's page. Its fields are its form: they open as inputs (SHAA-1644), and once something was changed a
 * bar offers Save and Discard; leaving the CI with unsaved changes asks first. A user without the edit
 * right on the class, a deleted CI, and read-only or managed fields show their values in the same place,
 * read-only (ciDraft.ts, detail/LayoutPanels).
 *
 * The class layout's tabs (`layout:<key>`; a single one is "overview"), then the relationship map, the
 * impact analysis and the history. The Impact tab has its own URL (/cis/:id/impact, with its options
 * in the query); the others are chosen on the page.
 */
type Tab = string;

const route = useRoute();
const router = useRouter();
const id = computed(() => String(route.params.id ?? ""));
const onImpactRoute = computed(() => /\/impact\/?$/.test(route.path));
const trail = useTrail();
const ci = useCi(id);
const tab = ref<Tab>("");
const flash = useFlashStore();
const session = useSessionStore();
useDocumentTitle(() => ci.data.value?.label);
// Walking to another CI reuses this component; start each record on its first tab (or Impact, on its URL).
watch(id, () => (tab.value = ""));

// A malformed id in the URL is rejected by the API as a params validation error; for the operator it is simply "not found".
const notFound = computed(() => {
  const e = ci.error.value;
  return e instanceof ApiError && (e.code === "NOT_FOUND" || (e.code === "VALIDATION_ERROR" && e.details.some((d) => d.in === "params")));
});
// The API refuses a CI of a class the user may not view; that is a permission limit, not a missing record.
const forbidden = computed(() => ci.error.value instanceof ApiError && ci.error.value.code === "FORBIDDEN");
const c = computed(() => ci.data.value);

// A business service has its own page (/services/:id): its CI URL redirects there, the Impact tab to the
// service's (which defaults to Upstream). A deleted service stays here, read-only, and the layout editor
// edits the class's layout on this page.
const serviceSettings = useServiceSettings();
const isService = computed(() => !!c.value && !c.value.deletedAt && c.value.classId === serviceSettings.data.value?.classId);
const redirecting = computed(() => isService.value && !route.meta.layoutEditor);
watch(
  redirecting,
  (go) => {
    if (go) void router.replace({ path: `/services/${id.value}${onImpactRoute.value ? "/impact" : ""}`, query: route.query, hash: route.hash });
  },
  { immediate: true },
);

// The CI's layout (its own, a template chosen for it, or its class's default template; GET
// /configuration-items/{id}/layout), else the class's from the settings; without tabs the built-in one:
// General (core fields and ungrouped attributes), the attribute groups, then the record's class and timestamps.
const settings = useAppSettings();
const classes = useCiClasses();
const areas = useAreas();
const classKey = computed(() => classes.data.value?.find((k) => k.id === c.value?.classId)?.key);
const ciLayout = useCiLayout(() => (c.value ? id.value : undefined));
const layout = computed(() => {
  const own = ciLayout.data.value;
  if (own && own.ciId === id.value) return normalizeLayout(asClassLayout(own.classKey, own.layout));
  return layoutFor(settings.doc.value, classKey.value);
});
/** Waiting for the CI's layout (on an error the class's applies). */
const layoutLoading = computed(() => ciLayout.isLoading.value);
/** For those who may change layouts: the CI does not show its class's default layout. */
const ownLayout = computed(() => {
  const l = ciLayout.data.value;
  if (!session.can("customization.manage") || !l || l.ciId !== id.value || l.source === "class_default") return null;
  return l.source === "custom"
    ? { label: t("ciLayout.badgeCustom"), title: t("ciLayout.badgeCustomTitle", { class: c.value?.class.name ?? "" }) }
    : { label: t("ciLayout.badgeTemplate", { name: l.templateName ?? "" }), title: t("ciLayout.badgeTemplateTitle", { name: l.templateName ?? "", class: c.value?.class.name ?? "" }) };
});
const attrs = useClassAttributes(() => c.value?.classId, { includeInactive: true });
const defs = computed(() => (attrs.data.value ?? []).filter((d) => d.isActive));
// Archived fields are kept out of the layout (one may have been superseded by a core field, GH#369);
// their stored values are listed apart, labelled as archived.
const archivedDefs = computed(() => (attrs.data.value ?? []).filter((d) => !d.isActive && c.value?.attributes[d.key] != null));
// The history and the audit trail are the audit log, which needs audit.view: without it their sections are left out.
const shownLayout = computed(() => {
  const l = layout.value ?? builtInLayout(classKey.value ?? "");
  return session.can("audit.view") ? l : withoutKinds(l, ["history", "audit"]);
});
const layoutTabs = computed(() => resolveLayout(shownLayout.value, defs.value, DETAIL_CORE, DETAIL_RECORD));
/** Built-in panels the layout places in its tabs; the others keep their usual place. */
const placed = computed(() => placedPanels(layout.value));

// Edit layout (the layout-editor route, in its own window): the class layout edited on this CI, for users who may customize.
const activeAttrs = computed(() => attrs.data.value?.filter((d) => d.isActive));
const editor = useLayoutEditor({ classKey, attrs: activeAttrs, ciId: id });
const defFor = (f: string) => defs.value.find((d) => d.key === attributeKey(f));
/** The record section as the draft layout places it (always last on the first tab). */
const recordSection = computed(() =>
  editor.layout ? resolveLayout(editor.layout, defs.value, DETAIL_CORE, DETAIL_RECORD, true)[0]?.sections.filter((s) => s.key === "_record") ?? [] : [],
);

/** Where the panels the draft layout does not place are shown, for the editor. */
const usualPlaces = computed(() => {
  const draft = placedPanels(editor.layout);
  const rest = [
    !draft.has("relations") && "the relationships come after the record details",
    "the relationship map follows the layout's tabs",
    !draft.has("history") && "then the history",
  ].filter(Boolean);
  return `The record details come last on the first tab; ${rest.join(", ")}. + Panel places the relationships, the history or the audit trail in any tab.`;
});

// The Workflows tab, once the CI has run a workflow or the user may start one on it.
const ciWorkflows = useCiWorkflows(() => (c.value ? id.value : undefined));
const hasWorkflows = computed(() => !!ciWorkflows.data.value && (ciWorkflows.data.value.data.length > 0 || ciWorkflows.data.value.startable.length > 0));
const TABS = computed<[Tab, string][]>(() => [
  ...(layoutTabs.value.length > 1 ? layoutTabs.value.map((t): [Tab, string] => [`layout:${t.key}`, t.label]) : [["overview", "Overview"] as [Tab, string]]),
  ["graph", "Relationship map"],
  // A deleted CI has no live relationships to analyse.
  ...(c.value?.deletedAt ? [] : [["impact", "Impact"] as [Tab, string]]),
  // Its running and recent workflow instances (a deleted CI keeps their history).
  ...(hasWorkflows.value ? [["workflows", "Workflows"] as [Tab, string]] : []),
  // The history is the audit log, which needs audit.view.
  ...(session.can("audit.view") && !placed.value.has("history") ? [["history", "History"] as [Tab, string]] : []),
]);
/** The tab shown: Impact on its URL, else the chosen one while it exists (a layout can change under the page), else the first. */
const current = computed<Tab>(() => {
  if (onImpactRoute.value && TABS.value.some(([k]) => k === "impact")) return "impact";
  const chosen = tab.value === "impact" ? "" : tab.value;
  return TABS.value.some(([k]) => k === chosen) ? chosen : TABS.value[0][0];
});
/** Shows a tab: Impact by its URL, the others on the CI's own URL. */
function selectTab(key: Tab) {
  tab.value = key;
  if (key === "impact" && !onImpactRoute.value) void router.push(`/cis/${id.value}/impact`);
  else if (key !== "impact" && onImpactRoute.value) void router.push(`/cis/${id.value}`);
}
/** Which layout tab is shown (they come first in TABS), or -1. */
const layoutIndex = computed(() => (current.value === "overview" || current.value.startsWith("layout:") ? TABS.value.findIndex(([k]) => k === current.value) : -1));
/** Arrow keys, Home and End move between the tabs. */
function onTabKey(e: KeyboardEvent) {
  const keys = TABS.value.map(([k]) => k);
  const at = keys.indexOf(current.value);
  const to = { ArrowRight: at + 1, ArrowLeft: at - 1 + keys.length, Home: 0, End: keys.length - 1 }[e.key];
  if (to === undefined) return;
  e.preventDefault();
  const next = keys[to % keys.length];
  selectTab(next);
  void nextTick(() => document.getElementById(`tab-${tabId(next)}`)?.focus());
}
const tabId = (k: Tab) => k.replace(":", "-");
// The CI's values being edited, on every layout tab (they stay while another tab is shown).
const canEdit = computed(() => !!c.value && !c.value.deletedAt && session.canOnClass(c.value.classId, "edit"));
const draft = useCiDraft({
  mode: "edit",
  classId: () => c.value?.classId,
  ci: () => c.value,
  attrs: () => attrs.data.value,
  readOnlyFields: () => layout.value?.readOnlyFields,
  locked: () => !canEdit.value,
});
/** The layout tab's key in TABS for the layout tab `i`. */
const layoutTabKey = (i: number): Tab => (layoutTabs.value.length > 1 ? `layout:${layoutTabs.value[i].key}` : "overview");
const layoutTabFields = (i: number) => layoutTabs.value[i]?.sections.flatMap((sec) => sec.fields.map((f) => f.field)) ?? [];
const tabErrorCount = (key: Tab) => {
  const i = layoutTabs.value.findIndex((_, j) => layoutTabKey(j) === key);
  return i < 0 ? 0 : layoutTabFields(i).filter((f) => draft.fieldErrors[f]).length;
};
/** Shows the layout tab holding `field` and puts the cursor in it. */
async function focusField(field: string) {
  const i = layoutTabs.value.findIndex((_, j) => layoutTabFields(j).includes(field));
  if (i >= 0) selectTab(layoutTabKey(i));
  await nextTick();
  document.getElementById(fieldIdFor(field))?.focus();
}
async function onSave() {
  draft.error = null;
  // Catch empty required fields before the round trip; everything else is validated by the API.
  const missing = draft.checkRequired(new Set(layoutTabs.value.flatMap((_, i) => layoutTabFields(i))));
  if (missing.length > 0) {
    await focusField(missing[0]);
    return;
  }
  try {
    const saved = await draft.save();
    if (!saved) return;
    draft.reset(saved);
    flash.show(`Saved ${saved.label}.`);
  } catch (err) {
    draft.error = err;
    // Show the first tab with a rejected field, so the message next to it is in view.
    const withError = layoutTabs.value.findIndex((_, i) => tabErrorCount(layoutTabKey(i)) > 0);
    if (withError >= 0) selectTab(layoutTabKey(withError));
    window.scrollTo({ top: 0 });
  }
}
/** After a version conflict: the CI as saved now, with the operator's changes dropped. */
async function loadCurrent() {
  const r = await ci.refetch();
  if (r.data) draft.reset(r.data);
}

// Unsaved changes: confirm before leaving this CI in the app (its Impact tab is the same page), and let the
// browser ask before a reload or closing the tab.
const keepChanges = (to: RouteLocationNormalized) =>
  !draft.dirty || String(to.params.id ?? "") === id.value || window.confirm(`Discard your unsaved changes to ${c.value?.label ?? "this configuration item"}?`);
onBeforeRouteLeave(keepChanges);
onBeforeRouteUpdate(keepChanges);
function onBeforeUnload(e: BeforeUnloadEvent) {
  if (!draft.dirty) return;
  e.preventDefault();
  e.returnValue = "";
}
onMounted(() => window.addEventListener("beforeunload", onBeforeUnload));
onBeforeUnmount(() => window.removeEventListener("beforeunload", onBeforeUnload));

const self = computed<TrailStep | undefined>(() => (c.value ? { id: c.value.id, name: c.value.label } : undefined));
const crumbs = computed<Crumb[]>(() => {
  if (!c.value) return [];
  const out: Crumb[] = [{ label: "Inventory", to: "/cis" }];
  if (trail.value.length > 0) {
    trail.value.forEach((s, i) => out.push({ label: s.name, to: { path: `/cis/${s.id}`, state: { trail: trail.value.slice(0, i) } } }));
  } else {
    const area = areas.data.value?.find((a) => a.id === classes.data.value?.find((k) => k.id === c.value?.classId)?.areaId);
    if (area) out.push({ label: area.name });
    out.push({ label: c.value.class.name, to: `/cis?classId=${c.value.classId}` });
  }
  out.push({ label: c.value.label });
  return out;
});
</script>

<template>
  <LoadingState v-if="ci.isLoading.value || serviceSettings.isLoading.value || redirecting" label="Loading configuration item…" />
  <template v-else-if="ci.isError.value">
    <Breadcrumbs :items="[{ label: 'Inventory', to: '/cis' }, { label: forbidden ? 'Permission denied' : notFound ? 'Not found' : 'Error' }]" />
    <EmptyState v-if="forbidden" title="Permission denied">
      None of your permission profiles allows viewing this configuration item's class, so it cannot be shown.
      <template #actions><RouterLink class="btn" to="/cis">Back to inventory</RouterLink></template>
    </EmptyState>
    <EmptyState v-else-if="notFound" title="Configuration item not found">
      No CI has the id <code>{{ id }}</code>. It may have been removed, or the link is wrong.
      <template #actions><RouterLink class="btn" to="/cis">Back to inventory</RouterLink></template>
    </EmptyState>
    <ErrorAlert v-else :error="ci.error.value" :on-retry="() => ci.refetch()" />
  </template>
  <template v-else-if="c && self">
    <Breadcrumbs :items="crumbs" />
    <div class="page-header">
      <div class="title">
        <h1 dir="auto">{{ c.label }}</h1>
        <span class="mono muted" title="Ident">{{ c.ident }}</span>
        <RouterLink :to="`/cis?classId=${c.classId}`" class="badge" dir="auto">{{ c.class.name }}</RouterLink>
        <span v-if="c.deletedAt" class="badge danger">Deleted {{ formatDateTime(c.deletedAt) }}</span>
        <CiStateBadge v-else :ci="c" />
        <CriticalityBadge :value="c.criticality" />
        <span v-if="ownLayout" class="badge ci-own-layout" data-testid="ci-own-layout" :title="ownLayout.title">{{ ownLayout.label }}</span>
      </div>
      <div v-if="!c.deletedAt" class="actions">
        <EditLayoutButton v-if="editor.allowed && !editor.active" :editor="editor" />
        <RouterLink v-if="!onImpactRoute" class="btn" :to="`/cis/${c.id}/impact`">Impact analysis</RouterLink>
        <DeleteCiButton v-if="session.canOnClass(c.classId, 'delete')" :ci="c" />
      </div>
    </div>
    <FormErrorBanner v-if="draft.error != null && draft.dirty && !editor.active" :error="draft.error" :unplaced="draft.unplaced" :on-reload="loadCurrent" />
    <div v-if="c.deletedAt" class="alert alert-warn">
      This CI was deleted on {{ formatDateTime(c.deletedAt) }}. It is kept read-only for history; its relationships were
      removed with it.
    </div>

    <LayoutEditView v-if="editor.active && classKey" :editor="editor" :class-name="c.class.name" :attrs="activeAttrs" :attrs-error="attrs.error.value">
      <template #field="{ field }">
        <AttributeValue v-if="defFor(field)" :def="defFor(field)!" :value="c.attributes[defFor(field)!.key]" :ref-info="c.attributeReferences[defFor(field)!.key]" :self="self" :trail="trail" />
        <CoreFieldValue v-else :ci="c" :field="field" />
      </template>
      <template #first-tab-end>
        <LayoutPanels :ci="c" :sections="recordSection" :defs="defs" :self="self" :trail="trail" />
        <p class="hint">{{ usualPlaces }}</p>
      </template>
      <template #panel="{ kind }">
        <BlockContent v-if="kind === 'relations' || session.can('audit.view')" :kind="kind" :ci="c" :self="self" :trail="trail" />
        <p v-else class="hint panel-body">Shown to users with the audit.view permission; you do not have it, so no preview.</p>
      </template>
    </LayoutEditView>

    <div v-if="!editor.active" class="tabs" role="tablist" aria-label="CI sections">
      <button
        v-for="[key, label] in TABS"
        :id="`tab-${tabId(key)}`"
        :key="key"
        type="button"
        role="tab"
        :aria-selected="current === key"
        :aria-controls="`panel-${tabId(key)}`"
        :tabindex="current === key ? 0 : -1"
        @click="selectTab(key)"
        @keydown="onTabKey"
      >
        {{ label }}<span v-if="tabErrorCount(key) > 0" class="badge danger tab-errors">{{ tabErrorCount(key) }} error{{ tabErrorCount(key) === 1 ? "" : "s" }}</span>
      </button>
    </div>

    <div v-if="!editor.active" :id="`panel-${tabId(current)}`" role="tabpanel" :aria-labelledby="`tab-${tabId(current)}`">
      <template v-if="layoutIndex >= 0">
        <LoadingState v-if="attrs.isLoading.value || layoutLoading" label="Loading attribute definitions…" />
        <ErrorAlert
          v-else-if="attrs.isError.value"
          :error="attrs.error.value"
          title="Could not load this class's attribute definitions"
          :on-retry="() => attrs.refetch()"
        />
        <LayoutPanels
          v-else
          :ci="c"
          :sections="layoutTabs[layoutIndex]?.sections ?? []"
          :defs="defs"
          :archived="archivedDefs"
          :self="self"
          :trail="trail"
          :orphans="layoutIndex === 0"
          :draft="draft"
        />
        <SignInAccountPanel v-if="layoutIndex === 0" :ci="c" />
        <PartOfServicesPanel v-if="layoutIndex === 0" :ci="c" :self="self" :trail="trail" />
        <template v-if="layoutIndex === 0 && !placed.has('relations')">
          <div style="height: var(--sp-4)" />
          <RelationshipsPanel :ci="c" :self="self" :trail="trail" />
        </template>
      </template>
      <RelationshipGraphPanel v-else-if="current === 'graph'" :ci="c" :self="self" :trail="trail" />
      <ImpactPanel v-else-if="current === 'impact'" :ci="c" :self="self" :trail="trail" />
      <CiWorkflowsPanel v-else-if="current === 'workflows'" :ci="c" />
      <HistoryPanel v-else :ci="c" />
    </div>
    <div v-if="!editor.active && (draft.dirty || draft.pending)" class="save-bar ci-save-bar" role="region" aria-label="Unsaved changes">
      <span class="badge warn">Unsaved changes</span>
      <button type="button" class="btn btn-primary" :disabled="draft.pending" @click="onSave">{{ draft.pending ? "Saving…" : "Save" }}</button>
      <button type="button" class="btn" :disabled="draft.pending" @click="draft.reset(c)">Discard</button>
    </div>
  </template>
</template>
