<script setup lang="ts">
import { adminCrumbs } from "../sections";
import { computed, nextTick } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { ApiError } from "../../../api/client";
import { useWorkflow } from "../../../api/workflows";
import Breadcrumbs from "../../../components/Breadcrumbs.vue";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import Icon from "../../../components/Icon.vue";
import LoadingState from "../../../components/LoadingState.vue";
import { useDocumentTitle } from "../../../lib/composables";
import { formatDateTime, formatRelative } from "../../../lib/format";
import { t, tAround } from "../../../i18n";
import { useCiClasses } from "../../../api/queries";
import WorkflowActions from "./WorkflowActions.vue";
import WorkflowApprovers from "./WorkflowApprovers.vue";
import WorkflowDesigner from "./WorkflowDesigner.vue";
import WorkflowGrantsMatrix from "./WorkflowGrantsMatrix.vue";
import WorkflowSettingsForm from "./WorkflowSettingsForm.vue";
import WorkflowVersions from "./WorkflowVersions.vue";

/**
 * Administration › Workflows › new / one workflow. Tabs (in the URL as ?tab=): Settings, Designer
 * (the draft graph with its live lint and publishing), Versions (retire), Grants and Approvers (who
 * decides each approval step, with the approvers lint and a preview for one CI) and Notifications (the
 * workflow's inbox, e-mail and webhook actions, saved at once, with their lint and a recipient preview).
 * The record head band carries the name, its status and version chips, and the tabs on its lower edge.
 */
const route = useRoute();
const router = useRouter();
const id = computed(() => (route.path.endsWith("/new") ? undefined : String(route.params.id ?? "")));
const isNew = computed(() => !id.value);
const wf = useWorkflow(id);
useDocumentTitle(() => (isNew.value ? t("wfAdmin.newTitle") : wf.data.value?.name));
const classes = useCiClasses();
const className = computed(() => classes.data.value?.find((c) => c.id === wf.data.value?.classId)?.name ?? wf.data.value?.classKey);

const TABS = [
  ["settings", t("wfAdmin.tab.settings")],
  ["designer", t("wfAdmin.tab.designer")],
  ["versions", t("wfAdmin.tab.versions")],
  ["grants", t("wfAdmin.tab.grants")],
  ["approvers", t("wfApprovers.tab")],
  ["actions", t("wfActions.tab")],
] as const;
type Tab = (typeof TABS)[number][0];
const current = computed<Tab>(() => {
  const t = route.query.tab;
  return TABS.some(([k]) => k === t) ? (t as Tab) : "settings";
});

/** A tab with unsaved changes (Designer, Grants) may ask first and keep the current tab. */
async function selectTab(key: Tab) {
  if (key === current.value) return;
  await router.push({ path: route.path, query: key === "settings" ? {} : { tab: key } });
}
async function onTabKey(e: KeyboardEvent) {
  const keys = TABS.map(([k]) => k);
  const at = keys.indexOf(current.value);
  const to = { ArrowRight: at + 1, ArrowLeft: at - 1 + keys.length, Home: 0, End: keys.length - 1 }[e.key];
  if (to === undefined) return;
  e.preventDefault();
  await selectTab(keys[to % keys.length]);
  // The tab now shown: the one asked for, or the same one when leaving it was cancelled.
  await nextTick();
  document.getElementById(`wf-tab-${current.value}`)?.focus();
}

const crumbs = computed(() => adminCrumbs("workflows", { label: isNew.value ? t("wfAdmin.newTitle") : (wf.data.value?.name ?? "…") }));
const notFoundParts = computed(() => tAround("wfAdmin.notFound.body", "id"));
const notFound = computed(() => {
  const e = wf.error.value;
  return e instanceof ApiError && (e.code === "NOT_FOUND" || (e.code === "VALIDATION_ERROR" && e.details.some((d) => d.in === "params")));
});
</script>

<template>
  <Breadcrumbs v-if="!isNew && (wf.isLoading.value || (wf.isError.value && !wf.data.value))" :items="crumbs" />
  <LoadingState v-if="!isNew && wf.isLoading.value" :label="t('wfAdmin.loading')" />
  <template v-else-if="!isNew && wf.isError.value && !wf.data.value">
    <EmptyState v-if="notFound" icon="search" :title="t('wfAdmin.notFound.title')">
      {{ notFoundParts[0] }}<code>{{ id }}</code>{{ notFoundParts[1] }}
      <template #actions><RouterLink class="btn" to="/admin/workflows">{{ t("wfAdmin.back") }}</RouterLink></template>
    </EmptyState>
    <ErrorAlert v-else :error="wf.error.value" :on-retry="() => wf.refetch()" />
  </template>
  <template v-else-if="isNew">
    <div class="record-head record-head-plain">
      <Breadcrumbs :items="crumbs" />
      <div class="page-header record-header">
        <div class="record-heading">
          <span class="class-tile class-tile-lg" aria-hidden="true"><Icon name="network" class="class-icon" /></span>
          <div class="record-title">
            <div class="title"><h1>{{ t("wfAdmin.newTitle") }}</h1></div>
            <p class="record-meta"><span class="record-meta-line">{{ t("wfAdmin.newLead") }}</span></p>
          </div>
        </div>
      </div>
    </div>
    <WorkflowSettingsForm />
  </template>
  <template v-else-if="wf.data.value">
    <div class="record-head record-head-plain wf-head">
      <Breadcrumbs :items="crumbs" />
      <div class="page-header record-header">
        <div class="record-heading">
          <span class="class-tile class-tile-lg" aria-hidden="true"><Icon name="network" class="class-icon" /></span>
          <div class="record-title">
            <div class="title">
              <h1 dir="auto">{{ wf.data.value.name }}</h1>
            </div>
            <p class="record-meta" data-testid="record-meta">
              <span v-if="wf.data.value.isActive" class="badge ok"><span class="status-dot" aria-hidden="true" />{{ t("common.active") }}</span>
              <span v-else class="badge off"><span class="status-dot" aria-hidden="true" />{{ t("wfAdmin.inactive") }}</span>
              <span class="badge" dir="auto">{{ className }}</span>
              <span v-if="wf.data.value.currentVersionNo !== null" class="badge mono">{{ t("wfAdmin.currentChip", { n: wf.data.value.currentVersionNo }) }}</span>
              <span v-if="wf.data.value.draftVersionNo !== null" class="badge info mono">{{ t("wfAdmin.draftChip", { n: wf.data.value.draftVersionNo }) }}</span>
              <span class="record-meta-line">
                <span class="ident">{{ wf.data.value.key }}</span>
                <span class="sep" aria-hidden="true">·</span>
                <time :datetime="wf.data.value.updatedAt" :title="formatDateTime(wf.data.value.updatedAt)">
                  {{ t("record.meta.updated", { when: formatRelative(wf.data.value.updatedAt) }) }}
                </time>
              </span>
            </p>
          </div>
        </div>
        <div class="actions">
          <RouterLink class="btn" :to="{ path: '/workflows', query: { workflow: wf.data.value.key } }">{{ t("wfAdmin.openInstances") }}</RouterLink>
        </div>
      </div>
      <div class="tabs record-tabs" role="tablist" :aria-label="t('wfAdmin.tabs')">
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
    </div>
    <div :id="`wf-panel-${current}`" role="tabpanel" :aria-labelledby="`wf-tab-${current}`">
      <WorkflowSettingsForm v-if="current === 'settings'" :workflow="wf.data.value" />
      <WorkflowDesigner v-else-if="current === 'designer'" :workflow="wf.data.value" />
      <WorkflowVersions v-else-if="current === 'versions'" :workflow="wf.data.value" />
      <WorkflowApprovers v-else-if="current === 'approvers'" :workflow="wf.data.value" />
      <WorkflowActions v-else-if="current === 'actions'" :workflow="wf.data.value" />
      <WorkflowGrantsMatrix v-else :workflow="wf.data.value" />
    </div>
  </template>
</template>
