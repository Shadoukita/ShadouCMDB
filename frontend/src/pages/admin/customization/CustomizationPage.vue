<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { onBeforeRouteLeave, RouterLink, useRoute } from "vue-router";
import { ApiError } from "../../../api/client";
import { useSaveUiSettings, useUiSettings, useUiSettingsVersion, type UiSettingsDocument } from "../../../api/uiSettings";
import Breadcrumbs from "../../../components/Breadcrumbs.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import { useNavPreviewStore } from "../../../lib/appSettings";
import { useDocumentTitle } from "../../../lib/composables";
import { normalizeDocument } from "../../../lib/uiSettings";
import { useBrandingStore } from "../../../stores/branding";
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
 * editor is open; the other sections preview inline.
 */
const SECTIONS = [
  { key: "branding", label: "Branding" },
  { key: "navigation", label: "Navigation" },
  { key: "dashboard", label: "Dashboard" },
  { key: "list-views", label: "List views" },
  { key: "layouts", label: "Detail and form layout" },
  { key: "history", label: "History" },
] as const;

const route = useRoute();
const section = computed(() => String(route.params.section ?? "branding"));
const current = computed(() => SECTIONS.find((s) => s.key === section.value));
useDocumentTitle(() => `${current.value?.label ?? "Customization"} · Customization`);

const branding = useBrandingStore();
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
const saved = ref<string | null>(null);

// (Re)start from the stored document whenever a new version is loaded and nothing is being edited.
watch(
  () => stored.data.value,
  (s) => {
    if (!s || (draft.value && dirty.value && loadedVersion.value !== null)) return;
    reset();
  },
);
function reset() {
  const s = stored.data.value;
  if (!s) return;
  draft.value = normalizeDocument(s.settings);
  baseline.value = JSON.stringify(draft.value);
  loadedVersion.value = s.version;
  error.value = null;
}
const dirty = computed(() => !!draft.value && JSON.stringify(draft.value) !== baseline.value);
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
  saved.value = null;
  try {
    const result = await save.mutateAsync({ version: loadedVersion.value, settings: draft.value, comment: comment.value.trim() || null });
    comment.value = "";
    // Show exactly what was saved; the stored copy of the new version arrives with the refetch.
    baseline.value = JSON.stringify(draft.value);
    loadedVersion.value = result.version;
    saved.value = `Saved as version ${result.version}.`;
    await branding.load();
  } catch (e) {
    error.value = e;
  }
}

function discard() {
  reset();
  saved.value = null;
}

async function reloadLatest() {
  await settings.refetch();
  loadedVersion.value = null;
  draft.value = null;
  if (stored.data.value?.version === version.value) reset();
}

onBeforeRouteLeave(() => (dirty.value ? window.confirm("Discard your unsaved customization changes?") : true));
</script>

<template>
  <Breadcrumbs :items="[{ label: 'Administration', to: '/admin' }, { label: 'Customization', to: '/admin/customization' }, { label: current?.label ?? section }]" />
  <div class="page-header">
    <div class="title">
      <h1>Customization</h1>
      <span v-if="settings.data.value" class="muted">
        version {{ settings.data.value.version }}<template v-if="settings.data.value.updatedBy"> · saved by {{ settings.data.value.updatedBy }}</template>
      </span>
    </div>
  </div>
  <p class="muted page-intro">
    How the application looks and is arranged for every user: name, colours and logo, the menu, the dashboard, and per
    class the inventory columns and the detail and form layout. Every save is kept as a version you can restore.
  </p>
  <nav class="tabs" aria-label="Customization">
    <RouterLink v-for="s in SECTIONS" :key="s.key" :to="`/admin/customization/${s.key}`" :aria-current="s.key === section ? 'page' : undefined">
      {{ s.label }}
    </RouterLink>
  </nav>

  <LoadingState v-if="settings.isLoading.value || (settings.data.value && stored.isLoading.value)" label="Loading the settings…" />
  <ErrorAlert v-else-if="settings.isError.value" :error="settings.error.value" :on-retry="() => settings.refetch()" />
  <ErrorAlert v-else-if="stored.isError.value" :error="stored.error.value" :on-retry="() => stored.refetch()" />
  <template v-else-if="draft && settings.data.value">
    <div v-if="section !== 'history'" class="save-bar" role="region" aria-label="Save changes">
      <span v-if="dirty" class="badge warn">Unsaved changes</span>
      <span v-else class="muted">No unsaved changes</span>
      <label class="sr-only" for="cust-comment">Comment for this version</label>
      <input id="cust-comment" v-model="comment" type="text" maxlength="500" placeholder="Comment for this version (optional)" :disabled="!dirty" />
      <button type="button" class="btn btn-primary" :disabled="!dirty || save.isPending.value" @click="onSave">
        {{ save.isPending.value ? "Saving…" : "Save" }}
      </button>
      <button type="button" class="btn" :disabled="!dirty || save.isPending.value" @click="discard">Discard</button>
    </div>
    <div v-if="saved && !dirty" class="alert" role="status">{{ saved }}</div>
    <div v-if="conflict || (stale && dirty)" class="alert alert-warn" role="alert">
      <strong>Someone else saved the settings while you were editing.</strong>
      <div>
        Version {{ version }} is now current; yours is based on version {{ loadedVersion }}. Your changes were not saved.
        <button type="button" class="btn btn-sm" @click="reloadLatest">Load the latest version</button> (discards your changes)
      </div>
    </div>
    <ErrorAlert v-else-if="error" :error="error" title="Settings not saved" />
    <div v-if="settings.data.value.issues.length > 0 && section !== 'history'" class="alert alert-warn" role="status">
      <strong>{{ settings.data.value.issues.length }} setting{{ settings.data.value.issues.length === 1 ? "" : "s" }} not applied or worth a second look</strong>
      <ul>
        <li v-for="i in settings.data.value.issues" :key="i.path"><code>{{ i.path }}</code> {{ i.message }}</li>
      </ul>
    </div>

    <BrandingSection v-if="section === 'branding'" :doc="draft" :assets="settings.data.value.assets" />
    <NavigationSection v-else-if="section === 'navigation'" :doc="draft" />
    <DashboardSection v-else-if="section === 'dashboard'" :doc="draft" />
    <ListViewsSection v-else-if="section === 'list-views'" :doc="draft" />
    <LayoutsSection v-else-if="section === 'layouts'" :doc="draft" />
    <HistorySection v-else-if="section === 'history'" :current="settings.data.value.version" :dirty="dirty" @restored="reloadLatest" />
    <div v-else class="alert alert-error" role="alert">
      There is no section called <code>{{ section }}</code>. <RouterLink to="/admin/customization/branding">Open Branding</RouterLink>.
    </div>
  </template>
</template>
