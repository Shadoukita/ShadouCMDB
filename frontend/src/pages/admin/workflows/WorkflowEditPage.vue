<script setup lang="ts">
import { computed, nextTick } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { ApiError } from "../../../api/client";
import { useWorkflow } from "../../../api/workflows";
import Breadcrumbs from "../../../components/Breadcrumbs.vue";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import { useDocumentTitle } from "../../../lib/composables";
import WorkflowDesigner from "./WorkflowDesigner.vue";
import WorkflowGrantsMatrix from "./WorkflowGrantsMatrix.vue";
import WorkflowSettingsForm from "./WorkflowSettingsForm.vue";
import WorkflowVersions from "./WorkflowVersions.vue";

/**
 * Administration › Workflows › new / one workflow. Tabs (in the URL as ?tab=): Settings, Designer
 * (the draft graph with its live lint and publishing), Versions (retire) and Grants.
 */
const route = useRoute();
const router = useRouter();
const id = computed(() => (route.path.endsWith("/new") ? undefined : String(route.params.id ?? "")));
const isNew = computed(() => !id.value);
const wf = useWorkflow(id);
useDocumentTitle(() => (isNew.value ? "New workflow" : wf.data.value?.name));

const TABS = [
  ["settings", "Settings"],
  ["designer", "Designer"],
  ["versions", "Versions"],
  ["grants", "Grants"],
] as const;
type Tab = (typeof TABS)[number][0];
const current = computed<Tab>(() => {
  const t = route.query.tab;
  return TABS.some(([k]) => k === t) ? (t as Tab) : "settings";
});

function selectTab(key: Tab) {
  if (key === current.value) return;
  void router.push({ path: route.path, query: key === "settings" ? {} : { tab: key } });
}
function onTabKey(e: KeyboardEvent) {
  const keys = TABS.map(([k]) => k);
  const at = keys.indexOf(current.value);
  const to = { ArrowRight: at + 1, ArrowLeft: at - 1 + keys.length, Home: 0, End: keys.length - 1 }[e.key];
  if (to === undefined) return;
  e.preventDefault();
  const next = keys[to % keys.length];
  selectTab(next);
  void nextTick(() => document.getElementById(`wf-tab-${next}`)?.focus());
}

const crumbs = computed(() => [
  { label: "Administration", to: "/admin" },
  { label: "Workflows", to: "/admin/workflows" },
  { label: isNew.value ? "New workflow" : (wf.data.value?.name ?? "…") },
]);
const notFound = computed(() => {
  const e = wf.error.value;
  return e instanceof ApiError && (e.code === "NOT_FOUND" || (e.code === "VALIDATION_ERROR" && e.details.some((d) => d.in === "params")));
});
</script>

<template>
  <Breadcrumbs :items="crumbs" />
  <LoadingState v-if="!isNew && wf.isLoading.value" label="Loading workflow…" />
  <template v-else-if="!isNew && wf.isError.value && !wf.data.value">
    <EmptyState v-if="notFound" title="Workflow not found">
      No workflow has the id {{ id }}. It may have been deleted.
      <template #actions><RouterLink class="btn" to="/admin/workflows">Back to workflows</RouterLink></template>
    </EmptyState>
    <ErrorAlert v-else :error="wf.error.value" :on-retry="() => wf.refetch()" />
  </template>
  <template v-else-if="isNew">
    <div class="page-header">
      <div class="title"><h1>New workflow</h1></div>
    </div>
    <WorkflowSettingsForm />
  </template>
  <template v-else-if="wf.data.value">
    <div class="page-header">
      <div class="title">
        <h1>{{ wf.data.value.name }}</h1>
        <span class="mono muted">{{ wf.data.value.key }}</span>
        <span v-if="wf.data.value.isActive" class="badge ok">Active</span>
        <span v-else class="badge off">Inactive</span>
        <span v-if="wf.data.value.currentVersionNo !== null" class="badge">v{{ wf.data.value.currentVersionNo }} current</span>
        <span v-if="wf.data.value.draftVersionNo !== null" class="badge info">v{{ wf.data.value.draftVersionNo }} draft</span>
      </div>
    </div>

    <div class="tabs" role="tablist" aria-label="Workflow sections">
      <button
        v-for="[key, label] in TABS"
        :id="`wf-tab-${key}`"
        :key="key"
        type="button"
        role="tab"
        :aria-selected="current === key"
        :aria-controls="`wf-panel-${key}`"
        :tabindex="current === key ? 0 : -1"
        @click="selectTab(key)"
        @keydown="onTabKey"
      >
        {{ label }}
      </button>
    </div>
    <div :id="`wf-panel-${current}`" role="tabpanel" :aria-labelledby="`wf-tab-${current}`">
      <WorkflowSettingsForm v-if="current === 'settings'" :workflow="wf.data.value" />
      <WorkflowDesigner v-else-if="current === 'designer'" :workflow="wf.data.value" />
      <WorkflowVersions v-else-if="current === 'versions'" :workflow="wf.data.value" />
      <WorkflowGrantsMatrix v-else :workflow="wf.data.value" />
    </div>
  </template>
</template>
