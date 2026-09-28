<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import { RouterLink, useRoute } from "vue-router";
import { ApiError } from "../api/client";
import { useAreas } from "../api/datamodel";
import { useCi, useCiClasses, useClassAttributes } from "../api/queries";
import Breadcrumbs, { type Crumb } from "../components/Breadcrumbs.vue";
import EmptyState from "../components/EmptyState.vue";
import ErrorAlert from "../components/ErrorAlert.vue";
import LoadingState from "../components/LoadingState.vue";
import CiStateBadge from "../components/CiStateBadge.vue";
import { useAppSettings } from "../lib/appSettings";
import { useDocumentTitle } from "../lib/composables";
import { formatDateTime } from "../lib/format";
import { useTrail, type TrailStep } from "../lib/trail";
import { builtInLayout, DETAIL_CORE, DETAIL_RECORD, layoutFor, resolveLayout } from "../lib/uiSettings";
import { useFlashStore } from "../stores/flash";
import { useSessionStore } from "../stores/session";
import DeleteCiButton from "./detail/DeleteCiButton.vue";
import HistoryPanel from "./detail/HistoryPanel.vue";
import LayoutPanels from "./detail/LayoutPanels.vue";
import RelationshipGraphPanel from "./detail/RelationshipGraphPanel.vue";
import RelationshipsPanel from "./detail/RelationshipsPanel.vue";

/** The class layout's tabs (`layout:<key>`; a single one is "overview"), then the relationship map and the history. */
type Tab = string;

const route = useRoute();
const id = computed(() => String(route.params.id ?? ""));
const trail = useTrail();
const ci = useCi(id);
const tab = ref<Tab>("");
const flash = useFlashStore();
const session = useSessionStore();
const flashText = computed(() => flash.forCi(id.value));
useDocumentTitle(() => ci.data.value?.label);
// Walking to another CI reuses this component; start each record on its first tab.
watch(id, () => (tab.value = ""));

// A malformed id in the URL is rejected by the API as a params validation error; for the operator it is simply "not found".
const notFound = computed(() => {
  const e = ci.error.value;
  return e instanceof ApiError && (e.code === "NOT_FOUND" || (e.code === "VALIDATION_ERROR" && e.details.some((d) => d.in === "params")));
});
// The API refuses a CI of a class the user may not view; that is a permission limit, not a missing record.
const forbidden = computed(() => ci.error.value instanceof ApiError && ci.error.value.code === "FORBIDDEN");
const c = computed(() => ci.data.value);

// The class's layout from Customization, if it has one; otherwise the built-in one: General (core fields and
// ungrouped attributes), the attribute groups, then the record's class and timestamps.
const settings = useAppSettings();
const classes = useCiClasses();
const areas = useAreas();
const classKey = computed(() => classes.data.value?.find((k) => k.id === c.value?.classId)?.key);
const layout = computed(() => layoutFor(settings.doc.value, classKey.value));
const attrs = useClassAttributes(() => c.value?.classId);
const defs = computed(() => (attrs.data.value ?? []).filter((d) => d.isActive || c.value?.attributes[d.key] != null));
const layoutTabs = computed(() => resolveLayout(layout.value ?? builtInLayout(classKey.value ?? ""), defs.value, DETAIL_CORE, DETAIL_RECORD));
const TABS = computed<[Tab, string][]>(() => [
  ...(layoutTabs.value.length > 1 ? layoutTabs.value.map((t): [Tab, string] => [`layout:${t.key}`, t.label]) : [["overview", "Overview"] as [Tab, string]]),
  ["graph", "Relationship map"],
  // The history is the audit log, which needs audit.view.
  ...(session.can("audit.view") ? [["history", "History"] as [Tab, string]] : []),
]);
/** The tab shown: the chosen one while it exists (a layout can change under the page), else the first. */
const current = computed<Tab>(() => (TABS.value.some(([k]) => k === tab.value) ? tab.value : TABS.value[0][0]));
/** Which layout tab is shown (they come first in TABS), or -1. */
const layoutIndex = computed(() => (current.value === "overview" || current.value.startsWith("layout:") ? TABS.value.findIndex(([k]) => k === current.value) : -1));
/** Arrow keys, Home and End move between the tabs. */
function onTabKey(e: KeyboardEvent) {
  const keys = TABS.value.map(([k]) => k);
  const at = keys.indexOf(current.value);
  const to = { ArrowRight: at + 1, ArrowLeft: at - 1 + keys.length, Home: 0, End: keys.length - 1 }[e.key];
  if (to === undefined) return;
  e.preventDefault();
  tab.value = keys[to % keys.length];
  void nextTick(() => document.getElementById(`tab-${tabId(tab.value)}`)?.focus());
}
const tabId = (k: Tab) => k.replace(":", "-");
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
  <LoadingState v-if="ci.isLoading.value" label="Loading configuration item…" />
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
        <h1>{{ c.label }}</h1>
        <span class="mono muted" title="Ident">{{ c.ident }}</span>
        <RouterLink :to="`/cis?classId=${c.classId}`" class="badge">{{ c.class.name }}</RouterLink>
        <span v-if="c.deletedAt" class="badge danger">Deleted {{ formatDateTime(c.deletedAt) }}</span>
        <CiStateBadge v-else :ci="c" />
      </div>
      <div v-if="!c.deletedAt" class="actions">
        <RouterLink v-if="session.canOnClass(c.classId, 'edit')" class="btn" :to="`/cis/${c.id}/edit`">Edit</RouterLink>
        <DeleteCiButton v-if="session.canOnClass(c.classId, 'delete')" :ci="c" />
      </div>
    </div>
    <div v-if="flashText" class="alert" role="status">{{ flashText }}</div>
    <div v-if="c.deletedAt" class="alert alert-warn">
      This CI was deleted on {{ formatDateTime(c.deletedAt) }}. It is kept read-only for history; its relationships were
      removed with it.
    </div>

    <div class="tabs" role="tablist" aria-label="CI sections">
      <button
        v-for="[key, label] in TABS"
        :id="`tab-${tabId(key)}`"
        :key="key"
        type="button"
        role="tab"
        :aria-selected="current === key"
        :aria-controls="`panel-${tabId(key)}`"
        :tabindex="current === key ? 0 : -1"
        @click="tab = key"
        @keydown="onTabKey"
      >
        {{ label }}
      </button>
    </div>

    <div :id="`panel-${tabId(current)}`" role="tabpanel" :aria-labelledby="`tab-${tabId(current)}`">
      <template v-if="layoutIndex >= 0">
        <LoadingState v-if="attrs.isLoading.value" label="Loading attribute definitions…" />
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
          :self="self"
          :trail="trail"
          :orphans="layoutIndex === 0"
        />
        <template v-if="layoutIndex === 0">
          <div style="height: var(--sp-4)" />
          <RelationshipsPanel :ci="c" :self="self" :trail="trail" />
        </template>
      </template>
      <RelationshipGraphPanel v-else-if="current === 'graph'" :ci="c" :self="self" :trail="trail" />
      <HistoryPanel v-else :ci="c" />
    </div>
  </template>
</template>
