<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { onBeforeRouteLeave, onBeforeRouteUpdate, RouterLink, useRoute, useRouter, type RouteLocationNormalized } from "vue-router";
import { ApiError } from "../api/client";
import { NOTES_PAGE, useCiNotes } from "../api/ciNotes";
import { useAreas } from "../api/datamodel";
import { useAuditLog, useCi, useCiClasses, useClassAttributes, useRelationships } from "../api/queries";
import { useServiceSettings } from "../api/services";
import { useCiWorkflows, useWorkflowCounts } from "../api/workflowRuntime";
import { useCiLayout } from "../api/uiSettings";
import Breadcrumbs, { type Crumb } from "../components/Breadcrumbs.vue";
import EmptyState from "../components/EmptyState.vue";
import PermissionDenied from "../components/PermissionDenied.vue";
import ErrorAlert from "../components/ErrorAlert.vue";
import LoadingState from "../components/LoadingState.vue";
import CiStateBadge from "../components/CiStateBadge.vue";
import ClassBadge from "../components/ClassBadge.vue";
import CriticalityBadge from "../components/CriticalityBadge.vue";
import RowMenu, { type RowMenuItem } from "../components/RowMenu.vue";
import SaveBar from "../components/SaveBar.vue";
import LayoutEditView from "../components/layoutEdit/LayoutEditView.vue";
import EditLayoutButton from "../components/layoutEdit/EditLayoutButton.vue";
import { t, tAround } from "../i18n";
import { useAppSettings } from "../lib/appSettings";
import { useDocumentTitle } from "../lib/composables";
import { useLayoutEditor } from "../lib/layoutEditor";
import { asClassLayout } from "../lib/layoutTemplates";
import { formatDateTime, formatRelative } from "../lib/format";
import { useTrail, type TrailStep } from "../lib/trail";
import { attributeKey, builtInLayout, cellClass, DETAIL_CORE, DETAIL_RECORD, fieldLabel, gridClass, layoutFor, normalizeLayout, placedPanels, resolveLayout, withoutKinds } from "../lib/uiSettings";
import { useFlashStore } from "../stores/flash";
import { useSessionStore } from "../stores/session";
import { fieldIdFor, useCiDraft } from "./form/ciDraft";
import FormErrorBanner from "./form/FormErrorBanner.vue";
import AttributeValue from "./detail/AttributeValue.vue";
import BlockContent from "./detail/BlockContent.vue";
import CoreFieldValue from "./detail/CoreFieldValue.vue";
import DeleteCiDialog from "./detail/DeleteCiDialog.vue";
import FactChips from "./detail/FactChips.vue";
import HistoryPanel from "./detail/HistoryPanel.vue";
import ImpactPanel from "./detail/ImpactPanel.vue";
import LayoutPanels from "./detail/LayoutPanels.vue";
import NotesPanel from "./detail/NotesPanel.vue";
import PartOfServicesPanel from "./detail/PartOfServicesPanel.vue";
import QrLabelDialog from "./detail/QrLabelDialog.vue";
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
 * impact analysis, the workflows, the notes and the history. The Impact tab has its own URL (/cis/:id/impact, with its options
 * in the query); the others are chosen on the page.
 *
 * The page head (design §0 step 12d) is a surface band: the breadcrumb, the class tile, the name in the data
 * font with the class's subtitle field under it (gap G8), the class, state and criticality chips, "ident ·
 * Updated … by …" (gap G16), the actions, and the tabs with their
 * counts. A class without a layout of its own shows the built-in arrangement as the default layout: the field
 * sections in one card beside the relationships and the newest history entries.
 */
type Tab = string;

const route = useRoute();
const router = useRouter();
const id = computed(() => String(route.params.id ?? ""));
/** "No CI has the id <id>…": the text around the id, in the translator's word order. */
const notFoundParts = tAround("record.notFound.body", "id");
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
// General (core fields and ungrouped attributes), the attribute groups, the record details (class and
// timestamps), then the relationships. A layout with tabs shows the record details and the relationships
// only where it places them.
const settings = useAppSettings();
const classes = useCiClasses();
const areas = useAreas();
const cls = computed(() => classes.data.value?.find((k) => k.id === c.value?.classId));
const classKey = computed(() => cls.value?.key);
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
/** Built-in panels the layout places in its tabs: the history has a tab of its own when it is not placed. */
const placed = computed(() => placedPanels(layout.value));
/** The layout has no tabs: the built-in arrangement, with the relationships after the fields. */
const builtInArrangement = computed(() => (shownLayout.value.tabs?.length ?? 0) === 0);

// Edit layout (the layout-editor route, in its own window): the class layout edited on this CI, for users who may customize.
const activeAttrs = computed(() => attrs.data.value?.filter((d) => d.isActive));
const editor = useLayoutEditor({ classKey, attrs: activeAttrs, ciId: id });
const defFor = (f: string) => defs.value.find((d) => d.key === attributeKey(f));
/** The record details as the draft layout shows them: the bookkeeping fields no field section places. */
const recordFields = computed(() =>
  editor.layout ? (resolveLayout(editor.layout, defs.value, DETAIL_CORE, DETAIL_RECORD, true).flatMap((t) => t.sections).find((s) => s.kind === "record")?.fields ?? []) : [],
);

// The Workflows tab, once the CI has run a workflow or the user may start one on it.
const ciWorkflows = useCiWorkflows(() => (c.value ? id.value : undefined));
const hasWorkflows = computed(() => !!ciWorkflows.data.value && (ciWorkflows.data.value.data.length > 0 || ciWorkflows.data.value.startable.length > 0));
// Its count is the CI's running instances (gap G14); none when the API refuses the CI (404), so no false 0.
const ciWorkflowCounts = useWorkflowCounts(() => id.value, () => hasWorkflows.value);
// The tabs' counts: the direct relationships on the map, the running workflow instances, the notes, the history's entries (the
// newest of them also show on the built-in Overview).
const rels = useRelationships(() => id.value, () => !!c.value);
const canAudit = computed(() => session.can("audit.view"));
const recent = useAuditLog(id, { limit: 5, offset: 0 }, [], () => canAudit.value && !!c.value);
// The notes (gap G13): everyone who may view the CI reads them; the tab counts them (the panel's first page, one query).
const notes = useCiNotes(id, { limit: NOTES_PAGE, offset: 0 }, () => !!c.value);
const historyTotal = computed(() => (canAudit.value ? recent.data.value?.page.total : undefined));
/**
 * The newest change of the record itself (gap G16): its time, and who made it for those with audit.view (the API
 * leaves the actor out otherwise). Null once retention removed the CI's audit entries: then the line is left out.
 */
const lastChange = computed(() => c.value?.lastChange ?? null);
const lastActor = computed(() => lastChange.value?.actor?.name ?? undefined);
/** The class's subtitle field (gap G8) and the CI's value in it; none when unset or empty (the class chip follows). */
const subtitleDef = computed(() => (cls.value?.subtitleAttributeId ? attrs.data.value?.find((d) => d.id === cls.value!.subtitleAttributeId) : undefined));
const subtitleValue = computed(() => {
  const v = subtitleDef.value ? c.value?.attributes[subtitleDef.value.key] : undefined;
  return v === null || v === undefined || v === "" ? undefined : v;
});
const TABS = computed<[Tab, string, number?][]>(() => [
  ...(layoutTabs.value.length > 1 ? layoutTabs.value.map((l): [Tab, string] => [`layout:${l.key}`, l.label]) : [["overview", t("record.tab.overview")] as [Tab, string]]),
  ["graph", t("record.actions.map"), rels.data.value?.page.total],
  // A deleted CI has no live relationships to analyse.
  ...(c.value?.deletedAt ? [] : [["impact", t("record.tab.impact")] as [Tab, string]]),
  // Its running and recent workflow instances (a deleted CI keeps their history), counted by the running ones.
  ...(hasWorkflows.value ? [["workflows", t("record.tab.workflows"), ciWorkflowCounts.data.value?.active] as [Tab, string, number?]] : []),
  ["notes", t("record.tab.notes"), notes.data.value?.page.total],
  // The history is the audit log, which needs audit.view.
  ...(canAudit.value && !placed.value.has("history") ? [["history", t("record.actions.history"), historyTotal.value] as [Tab, string, number?]] : []),
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
    flash.show(t("record.saved", { name: saved.label }));
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
  !draft.dirty || String(to.params.id ?? "") === id.value || window.confirm(t("record.leave", { name: c.value?.label ?? t("record.leave.thisCi") }));
onBeforeRouteLeave(keepChanges);
onBeforeRouteUpdate(keepChanges);
function onBeforeUnload(e: BeforeUnloadEvent) {
  if (!draft.dirty) return;
  e.preventDefault();
  e.returnValue = "";
}
onMounted(() => window.addEventListener("beforeunload", onBeforeUnload));
onBeforeUnmount(() => window.removeEventListener("beforeunload", onBeforeUnload));

// The title row's actions (audit R2): the views as secondary buttons, Delete in the overflow menu.
const deleting = ref(false);
const moreActions = computed<RowMenuItem[]>(() =>
  c.value && session.canOnClass(c.value.classId, "delete") ? [{ label: t("common.delete"), danger: true, action: () => (deleting.value = true) }] : [],
);
// Clone (gap G11): the create form prefilled from this CI (CiCreatePage, lib/ciClone), for those who may create CIs of
// its class while it takes new ones. QR label (gap G12): drawn in the browser (QrLabelDialog).
const canClone = computed(() => !!c.value && !!cls.value && cls.value.isActive && !cls.value.isAbstract && session.canOnClass(c.value.classId, "create"));
const qrOpen = ref(false);
const hasTab = (key: Tab) => TABS.value.some(([k]) => k === key);
const classTile = computed(() => (cls.value?.color ? { "--tile-c": cls.value.color } : undefined));

const self = computed<TrailStep | undefined>(() => (c.value ? { id: c.value.id, name: c.value.label } : undefined));
const crumbs = computed<Crumb[]>(() => {
  if (!c.value) return [];
  const out: Crumb[] = [{ label: t("inventory.crumb"), to: "/cis" }];
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
  <LoadingState v-if="ci.isLoading.value || serviceSettings.isLoading.value || redirecting" :label="t('record.loading')" />
  <template v-else-if="ci.isError.value">
    <PermissionDenied
      v-if="forbidden"
      :crumbs="[{ label: t('inventory.crumb'), to: '/cis' }]"
      :requirement="t('denied.classView')"
      :panel-title="t('denied.ci.panelTitle')"
    >
      {{ t("denied.ci.view") }}
      <template #actions><RouterLink class="btn btn-primary" to="/cis">{{ t("inventory.denied.back") }}</RouterLink></template>
    </PermissionDenied>
    <template v-else>
      <Breadcrumbs :items="[{ label: t('inventory.crumb'), to: '/cis' }, { label: notFound ? t('record.crumb.notFound') : t('common.error') }]" />
      <EmptyState v-if="notFound" :title="t('record.notFound.title')">
        {{ notFoundParts[0] }}<code>{{ id }}</code>{{ notFoundParts[1] }}
        <template #actions><RouterLink class="btn" to="/cis">{{ t("inventory.denied.back") }}</RouterLink></template>
      </EmptyState>
      <ErrorAlert v-else :error="ci.error.value" :on-retry="() => ci.refetch()" />
    </template>
  </template>
  <template v-else-if="c && self">
    <div class="record-head">
      <Breadcrumbs :items="crumbs" />
      <div class="page-header record-header">
        <div class="record-heading">
          <span class="class-tile class-tile-lg" :style="classTile" aria-hidden="true"><ClassBadge :icon="cls?.icon" :color="cls?.color" /></span>
          <div class="record-title">
            <div class="title">
              <h1 dir="auto" class="mono">{{ c.label }}</h1>
              <span v-if="ownLayout" class="badge ci-own-layout" data-testid="ci-own-layout" :title="ownLayout.title">{{ ownLayout.label }}</span>
            </div>
            <p v-if="subtitleDef && subtitleValue !== undefined" class="record-subtitle" data-testid="record-subtitle" :title="subtitleDef.label">
              <AttributeValue :def="subtitleDef" :value="subtitleValue" :ref-info="c.attributeReferences[subtitleDef.key]" :self="self" :trail="trail" />
            </p>
            <p class="record-meta" data-testid="record-meta">
              <RouterLink class="badge record-class-chip" :to="`/cis?classId=${c.classId}`" dir="auto">{{ c.class.name }}</RouterLink>
              <span v-if="c.deletedAt" class="badge danger">{{ t("record.deletedBadge", { when: formatDateTime(c.deletedAt) }) }}</span>
              <template v-else>
                <span v-if="c.active" class="badge ok"><span class="status-dot ok" aria-hidden="true" />{{ t("ciState.active") }}</span>
                <CiStateBadge :ci="c" />
              </template>
              <CriticalityBadge v-if="c.criticality" :value="c.criticality" />
              <span class="record-meta-line">
                <span class="ident" :title="t('record.meta.ident')">{{ c.ident }}</span>
                <template v-if="lastChange">
                  <span class="sep" aria-hidden="true">·</span>
                  <time :datetime="lastChange.at" :title="formatDateTime(lastChange.at)">{{
                    lastActor ? t("record.meta.updatedBy", { when: formatRelative(lastChange.at), actor: lastActor }) : t("record.meta.updated", { when: formatRelative(lastChange.at) })
                  }}</time>
                </template>
              </span>
            </p>
          </div>
        </div>
        <div v-if="!c.deletedAt" class="actions">
          <EditLayoutButton v-if="editor.allowed && !editor.active" :editor="editor" />
          <template v-if="!editor.active">
            <RouterLink
              v-if="canClone"
              class="btn"
              :to="{ path: '/cis/new', query: { classId: c.classId, cloneFrom: c.id } }"
              :title="t('record.actions.cloneTitle', { class: c.class.name })"
              data-testid="ci-clone"
              >{{ t("record.actions.clone") }}</RouterLink
            >
            <button type="button" class="btn" data-testid="ci-qr" @click="qrOpen = true">{{ t("record.actions.qr") }}</button>
            <RouterLink v-if="!onImpactRoute" class="btn" :to="`/cis/${c.id}/impact`">{{ t("record.actions.impact") }}</RouterLink>
            <button v-if="current !== 'graph'" type="button" class="btn" @click="selectTab('graph')">{{ t("record.actions.map") }}</button>
            <button v-if="hasTab('history') && current !== 'history'" type="button" class="btn" @click="selectTab('history')">{{ t("record.actions.history") }}</button>
          </template>
          <RowMenu v-if="moreActions.length > 0" :label="t('record.actions.more')" :items="moreActions" large />
          <DeleteCiDialog v-if="moreActions.length > 0" v-model:open="deleting" :ci="c" />
          <QrLabelDialog v-model:open="qrOpen" :ci="c" />
        </div>
      </div>
      <FactChips v-if="!editor.active" :ci="c" :defs="defs" />
      <div v-if="!editor.active" class="tabs record-tabs" role="tablist" :aria-label="t('record.tabs')">
        <button
          v-for="[key, label, n] in TABS"
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
          <!-- The count is generated content: part of the tab's accessible name, not of its text (the tab is named by its label). -->
          {{ label }}<span v-if="n !== undefined" class="tab-count mono" :data-count="n.toLocaleString()" /><span v-if="tabErrorCount(key) > 0" class="badge danger tab-errors">{{ t("record.tabErrors", { n: tabErrorCount(key) }) }}</span>
        </button>
      </div>
    </div>
    <FormErrorBanner v-if="draft.error != null && draft.dirty && !editor.active" :error="draft.error" :unplaced="draft.unplaced" :on-reload="loadCurrent" />
    <div v-if="c.deletedAt" class="alert alert-warn">
      {{ t("record.deletedNotice", { when: formatDateTime(c.deletedAt) }) }}
    </div>

    <LayoutEditView v-if="editor.active && classKey" :editor="editor" :class-name="c.class.name" :attrs="activeAttrs" :attrs-error="attrs.error.value">
      <template #field="{ field }">
        <AttributeValue v-if="defFor(field)" :def="defFor(field)!" :value="c.attributes[defFor(field)!.key]" :ref-info="c.attributeReferences[defFor(field)!.key]" :self="self" :trail="trail" />
        <CoreFieldValue v-else :ci="c" :field="field" />
      </template>
      <template #panel="{ kind }">
        <div v-if="kind === 'record'" class="panel-body">
          <div :class="gridClass(3)">
            <div v-for="f in recordFields" :key="f.field" :class="['field', 'field-ro', cellClass(1, 3)]">
              <span class="label">{{ fieldLabel(f.field, defs) }}</span>
              <div class="ro-value"><CoreFieldValue :ci="c" :field="f.field" /></div>
            </div>
            <div :class="['field', 'field-ro', cellClass(1, 3)]">
              <span class="label">ID</span>
              <div class="ro-value mono">{{ c.id }}</div>
            </div>
          </div>
        </div>
        <BlockContent v-else-if="kind === 'relations' || session.can('audit.view')" :kind="kind" :ci="c" :self="self" :trail="trail" />
        <p v-else class="hint panel-body">{{ t("record.auditPreviewHidden") }}</p>
      </template>
    </LayoutEditView>

    <div v-if="!editor.active" :id="`panel-${tabId(current)}`" role="tabpanel" :aria-labelledby="`tab-${tabId(current)}`">
      <template v-if="layoutIndex >= 0">
        <LoadingState v-if="attrs.isLoading.value || layoutLoading" :label="t('record.loadingAttrs')" />
        <ErrorAlert
          v-else-if="attrs.isError.value"
          :error="attrs.error.value"
          :title="t('record.attrsFailed')"
          :on-retry="() => attrs.refetch()"
        />
        <!-- The built-in arrangement, the default layout: the field sections in one card beside the relationships and the newest history. -->
        <template v-else-if="layoutIndex === 0 && builtInArrangement">
          <div class="record-overview">
            <LayoutPanels
              class="record-fields"
              :ci="c"
              :sections="layoutTabs[0]?.sections ?? []"
              :defs="defs"
              :archived="archivedDefs"
              :self="self"
              :trail="trail"
              orphans
              stacked
              :draft="draft"
            />
            <div class="record-side">
              <RelationshipsPanel :ci="c" :self="self" :trail="trail" />
              <SignInAccountPanel :ci="c" />
              <PartOfServicesPanel :ci="c" :self="self" :trail="trail" />
              <HistoryPanel v-if="canAudit" :ci="c" :preview="5" @more="selectTab('history')" />
            </div>
          </div>
        </template>
        <template v-else>
          <LayoutPanels
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
        </template>
      </template>
      <RelationshipGraphPanel v-else-if="current === 'graph'" :ci="c" :self="self" :trail="trail" />
      <ImpactPanel v-else-if="current === 'impact'" :ci="c" :self="self" :trail="trail" />
      <CiWorkflowsPanel v-else-if="current === 'workflows'" :ci="c" />
      <NotesPanel v-else-if="current === 'notes'" :ci="c" />
      <HistoryPanel v-else :ci="c" />
    </div>
    <SaveBar v-if="!editor.active && (draft.dirty || draft.pending)" :label="t('record.save.unsaved')" dirty :changes="draft.changeCount">
      <button type="button" class="btn" :disabled="draft.pending" @click="draft.reset(c)">{{ t("record.save.discard") }}</button>
      <button type="button" class="btn btn-primary" :disabled="draft.pending" @click="onSave">{{ draft.pending ? t("common.saving") : t("record.save.save") }}</button>
    </SaveBar>
  </template>
</template>
