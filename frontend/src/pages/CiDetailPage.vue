<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { RouterLink, useRoute } from "vue-router";
import { ApiError } from "../api/client";
import { useAreas } from "../api/datamodel";
import { useCi, useCiClasses, useClassAttributes } from "../api/queries";
import Breadcrumbs, { type Crumb } from "../components/Breadcrumbs.vue";
import EmptyState from "../components/EmptyState.vue";
import ErrorAlert from "../components/ErrorAlert.vue";
import LoadingState from "../components/LoadingState.vue";
import StatusBadge from "../components/StatusBadge.vue";
import { useAppSettings } from "../lib/appSettings";
import { useDocumentTitle } from "../lib/composables";
import { formatDateTime } from "../lib/format";
import { useTrail, type TrailStep } from "../lib/trail";
import { DETAIL_BUILTINS, layoutFor, resolveLayout } from "../lib/uiSettings";
import { useFlashStore } from "../stores/flash";
import { useSessionStore } from "../stores/session";
import AttributesPanel from "./detail/AttributesPanel.vue";
import CorePanel from "./detail/CorePanel.vue";
import DeleteCiButton from "./detail/DeleteCiButton.vue";
import HistoryPanel from "./detail/HistoryPanel.vue";
import LayoutPanels from "./detail/LayoutPanels.vue";
import RelationshipGraphPanel from "./detail/RelationshipGraphPanel.vue";
import RelationshipsPanel from "./detail/RelationshipsPanel.vue";

type Tab = "overview" | "graph" | "history";
const ALL_TABS: [Tab, string][] = [
  ["overview", "Overview"],
  ["graph", "Relationship map"],
  ["history", "History"],
];

const route = useRoute();
const id = computed(() => String(route.params.id ?? ""));
const trail = useTrail();
const ci = useCi(id);
const tab = ref<Tab>("overview");
const flash = useFlashStore();
const session = useSessionStore();
// The history is the audit log, which needs audit.view.
const TABS = computed(() => ALL_TABS.filter(([key]) => key !== "history" || session.can("audit.view")));
const flashText = computed(() => flash.forCi(id.value));
useDocumentTitle(() => ci.data.value?.name);
// Walking to another CI reuses this component; start each record on its overview.
watch(id, () => (tab.value = "overview"));

// A malformed id in the URL is rejected by the API as a params validation error; for the operator it is simply "not found".
const notFound = computed(() => {
  const e = ci.error.value;
  return e instanceof ApiError && (e.code === "NOT_FOUND" || (e.code === "VALIDATION_ERROR" && e.details.some((d) => d.in === "params")));
});
const c = computed(() => ci.data.value);

// The class's layout from Customization, if it has one; otherwise the built-in General + attributes panels.
const settings = useAppSettings();
const classes = useCiClasses();
const areas = useAreas();
const classKey = computed(() => classes.data.value?.find((k) => k.id === c.value?.classId)?.key);
const layout = computed(() => layoutFor(settings.doc.value, classKey.value));
const attrs = useClassAttributes(() => (layout.value ? c.value?.classId : undefined));
const defs = computed(() => (attrs.data.value ?? []).filter((d) => d.isActive || c.value?.attributes[d.key] != null));
const panels = computed(() => (attrs.data.value ? resolveLayout(layout.value, defs.value, DETAIL_BUILTINS) : null));
const self = computed<TrailStep | undefined>(() => (c.value ? { id: c.value.id, name: c.value.name } : undefined));
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
  out.push({ label: c.value.name });
  return out;
});
</script>

<template>
  <LoadingState v-if="ci.isLoading.value" label="Loading configuration item…" />
  <template v-else-if="ci.isError.value">
    <Breadcrumbs :items="[{ label: 'Inventory', to: '/cis' }, { label: 'Not found' }]" />
    <EmptyState v-if="notFound" title="Configuration item not found">
      No CI has the id <code>{{ id }}</code>. It may have been removed, or the link is wrong.
      <template #actions><RouterLink class="btn" to="/cis">Back to inventory</RouterLink></template>
    </EmptyState>
    <ErrorAlert v-else :error="ci.error.value" :on-retry="() => ci.refetch()" />
  </template>
  <template v-else-if="c && self">
    <Breadcrumbs :items="crumbs" />
    <div class="page-header">
      <div class="title">
        <h1>{{ c.name }}</h1>
        <RouterLink :to="`/cis?classId=${c.classId}`" class="badge">{{ c.class.name }}</RouterLink>
        <span v-if="c.deletedAt" class="badge danger">Deleted {{ formatDateTime(c.deletedAt) }}</span>
        <StatusBadge v-else :status="c.status" />
        <span v-if="c.environment" class="badge">{{ c.environment.name }}</span>
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
        :id="`tab-${key}`"
        :key="key"
        type="button"
        role="tab"
        :aria-selected="tab === key"
        :aria-controls="`panel-${key}`"
        @click="tab = key"
      >
        {{ label }}
      </button>
    </div>

    <div :id="`panel-${tab}`" role="tabpanel" :aria-labelledby="`tab-${tab}`">
      <template v-if="tab === 'overview'">
        <LayoutPanels v-if="panels" :ci="c" :panels="panels" :defs="defs" :self="self" :trail="trail" />
        <LoadingState v-else-if="layout && attrs.isLoading.value" label="Loading attribute definitions…" />
        <div v-else class="grid-2">
          <CorePanel :ci="c" />
          <AttributesPanel :ci="c" :self="self" :trail="trail" />
        </div>
        <div style="height: var(--sp-4)" />
        <RelationshipsPanel :ci="c" :self="self" :trail="trail" />
      </template>
      <RelationshipGraphPanel v-else-if="tab === 'graph'" :ci="c" :self="self" :trail="trail" />
      <HistoryPanel v-else :ci="c" />
    </div>
  </template>
</template>
