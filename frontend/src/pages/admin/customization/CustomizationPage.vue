<script setup lang="ts">
import { t } from "../../../i18n";
import { adminCrumbs } from "../sections";
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { onBeforeRouteLeave, RouterLink, useRoute } from "vue-router";
import { ApiError } from "../../../api/client";
import { useSaveUiSettings, useUiSettings, useUiSettingsVersion, type UiSettingsDocument } from "../../../api/uiSettings";
import Breadcrumbs from "../../../components/Breadcrumbs.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import SaveBar from "../../../components/SaveBar.vue";
import { useNavPreviewStore } from "../../../lib/appSettings";
import { useDocumentTitle } from "../../../lib/composables";
import { normalizeDocument } from "../../../lib/uiSettings";
import { useBrandingStore } from "../../../stores/branding";
import { useFlashStore } from "../../../stores/flash";
import BrandingSection from "./BrandingSection.vue";
import DashboardSection from "./DashboardSection.vue";
import HistorySection from "./HistorySection.vue";
import LayoutsSection from "./LayoutsSection.vue";
import ListViewsSection from "./ListViewsSection.vue";
import NavigationSection from "./NavigationSection.vue";

/**
 * Administration › Customization: one settings document for every user, edited
 * in sections and saved as a new version. The editor works on the stored
 * document (GET /ui-settings/versions/{current}), which keeps references to
 * classes that do not exist right now; the screens apply the effective one.
 * Branding and navigation preview live in the real header and menu while the
 * editor is open; the other sections preview inline. The save bar is the shared one,
 * docked to the bottom (design §2.7, audit A3), with the version comment before Save.
 */
const SECTIONS = [
  { key: "branding", label: t("cust.section.branding") },
  { key: "navigation", label: t("cust.section.navigation") },
  { key: "dashboard", label: t("cust.section.dashboard") },
  { key: "list-views", label: t("cust.section.listViews") },
  { key: "layouts", label: t("cust.section.layouts") },
  { key: "history", label: t("cust.section.history") },
] as const;

const route = useRoute();
const section = computed(() => String(route.params.section ?? "branding"));
const current = computed(() => SECTIONS.find((s) => s.key === section.value));
useDocumentTitle(() => `${current.value?.label ?? t("admin.section.customization")} · ${t("admin.section.customization")}`);

const branding = useBrandingStore();
const flash = useFlashStore();
const navPreview = useNavPreviewStore();
const settings = useUiSettings();
const version = computed(() => settings.data.value?.version);
const stored = useUiSettingsVersion(version);
const save = useSaveUiSettings();

const draft = ref<UiSettingsDocument | null>(null);
const baseline = ref("");
const loadedVersion = ref<number | null>(null);
const comment = ref("");
const error = ref<unknown>(null);

function reset() {
  const s = stored.data.value;
  if (!s) return;
  draft.value = normalizeDocument(s.settings);
  baseline.value = JSON.stringify(draft.value);
  loadedVersion.value = s.version;
  error.value = null;
}
const dirty = computed(() => !!draft.value && JSON.stringify(draft.value) !== baseline.value);
// (Re)start from the stored document whenever a new version is loaded and nothing is being edited.
// Immediate: when the page is opened from within the app the stored version is usually cached
// already, so there is no later change to react to and the editor would stay empty.
watch(
  () => stored.data.value,
  (s) => {
    if (!s || (draft.value && dirty.value && loadedVersion.value !== null)) return;
    reset();
  },
  { immediate: true },
);
const conflict = computed(() => error.value instanceof ApiError && error.value.code === "VERSION_CONFLICT");
/** Someone saved a newer version while this editor was open. */
const stale = computed(() => loadedVersion.value !== null && version.value !== undefined && version.value !== loadedVersion.value);

// Live preview in the real shell while editing; the saved look comes back when the editor closes.
watch(
  () => [draft.value?.branding, draft.value?.navigation.entries, dirty.value] as const,
  ([b, entries]) => {
    branding.preview = b && dirty.value ? { ...b } : null;
    navPreview.entries = entries && dirty.value ? entries : null;
  },
  { deep: true },
);
onBeforeUnmount(() => {
  branding.preview = null;
  navPreview.entries = null;
});

async function onSave() {
  if (!draft.value || loadedVersion.value === null) return;
  error.value = null;
  try {
    const result = await save.mutateAsync({ version: loadedVersion.value, settings: draft.value, comment: comment.value.trim() || null });
    comment.value = "";
    // Show exactly what was saved; the stored copy of the new version arrives with the refetch.
    baseline.value = JSON.stringify(draft.value);
    loadedVersion.value = result.version;
    flash.show(t("cust.saved", { version: result.version }));
    await branding.load();
  } catch (e) {
    error.value = e;
  }
}

function discard() {
  reset();
}

async function reloadLatest() {
  await settings.refetch();
  loadedVersion.value = null;
  draft.value = null;
  if (stored.data.value?.version === version.value) reset();
}

// Unsaved changes: confirm before leaving the page in the app, and let the browser ask before a reload or closing the tab.
onBeforeRouteLeave(() => (dirty.value ? window.confirm(t("cust.leave")) : true));
function onBeforeUnload(e: BeforeUnloadEvent) {
  if (!dirty.value) return;
  e.preventDefault();
  e.returnValue = "";
}
onMounted(() => window.addEventListener("beforeunload", onBeforeUnload));
onBeforeUnmount(() => window.removeEventListener("beforeunload", onBeforeUnload));
</script>

<template>
  <Breadcrumbs :items="adminCrumbs('customization', { label: current?.label ?? section })" />
  <div class="page-header">
    <div class="title">
      <h1>{{ t("admin.section.customization") }}</h1>
      <span v-if="settings.data.value" class="muted">
        {{ settings.data.value.updatedBy ? t("cust.versionBy", { version: settings.data.value.version, name: settings.data.value.updatedBy }) : t("cust.version", { version: settings.data.value.version }) }}
      </span>
    </div>
  </div>
  <p class="page-intro">{{ t("cust.intro") }}</p>
  <nav class="tabs" :aria-label="t('admin.section.customization')">
    <RouterLink v-for="s in SECTIONS" :key="s.key" :to="`/admin/customization/${s.key}`" :aria-current="s.key === section ? 'page' : undefined">
      {{ s.label }}
    </RouterLink>
  </nav>

  <LoadingState v-if="settings.isLoading.value || (settings.data.value && stored.isLoading.value)" :label="t('cust.loading')" />
  <ErrorAlert v-else-if="settings.isError.value" :error="settings.error.value" :on-retry="() => settings.refetch()" />
  <ErrorAlert v-else-if="stored.isError.value" :error="stored.error.value" :on-retry="() => stored.refetch()" />
  <template v-else-if="draft && settings.data.value">
    <div v-if="conflict || (stale && dirty)" class="alert alert-warn" role="alert">
      <strong>{{ t("cust.conflict.title") }}</strong>
      <div>
        {{ t("cust.conflict.body", { current: version ?? "", loaded: loadedVersion ?? "" }) }}
        <button type="button" class="btn btn-sm" @click="reloadLatest">{{ t("cust.conflict.reload") }}</button> {{ t("cust.conflict.discards") }}
      </div>
    </div>
    <ErrorAlert v-else-if="error" :error="error" :title="t('cust.notSaved')" />
    <div v-if="settings.data.value.issues.length > 0 && section !== 'history'" class="alert alert-warn" role="status">
      <strong>{{ t("cust.issues", { n: settings.data.value.issues.length }) }}</strong>
      <ul>
        <li v-for="i in settings.data.value.issues" :key="i.path"><code>{{ i.path }}</code> {{ i.message }}</li>
      </ul>
    </div>

    <BrandingSection v-if="section === 'branding'" :doc="draft" :assets="settings.data.value.assets" />
    <NavigationSection v-else-if="section === 'navigation'" :doc="draft" />
    <DashboardSection v-else-if="section === 'dashboard'" :doc="draft" />
    <ListViewsSection v-else-if="section === 'list-views'" :doc="draft" />
    <LayoutsSection v-else-if="section === 'layouts'" :doc="draft" :error="error" />
    <HistorySection v-else-if="section === 'history'" :current="settings.data.value.version" :dirty="dirty" @restored="reloadLatest" />
    <div v-else class="alert alert-error" role="alert">
      {{ t("cust.noSection", { section }) }} <RouterLink to="/admin/customization/branding">{{ t("cust.openBranding") }}</RouterLink>
    </div>

    <SaveBar v-if="current && section !== 'history'" :label="t('cust.saveRegion')" :dirty="dirty">
      <label class="sr-only" for="cust-comment">{{ t("cust.comment") }}</label>
      <input id="cust-comment" v-model="comment" class="save-bar-comment" type="text" maxlength="500" :placeholder="t('cust.commentPlaceholder')" :disabled="!dirty" />
      <button type="button" class="btn" :disabled="!dirty || save.isPending.value" @click="discard">{{ t("record.save.discard") }}</button>
      <button type="button" class="btn btn-primary" :disabled="!dirty || save.isPending.value" @click="onSave">
        {{ save.isPending.value ? t("common.saving") : t("record.save.save") }}
      </button>
    </SaveBar>
  </template>
</template>
