<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { RouterLink } from "vue-router";
import { useImportSettings, useUpdateImportSettings } from "../../api/imports";
import Breadcrumbs from "../../components/Breadcrumbs.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import { useDocumentTitle } from "../../lib/composables";
import { formatBytes } from "../../lib/format";

/**
 * Administration › Import (Administrator): the instance switch for bulk import. It is off after installation
 * (D4); turning it on or off is audited. The server configuration can forbid import altogether
 * (IMPORT_ALLOWED=false), and then the switch is locked off. The limits are server configuration too and only
 * shown here.
 */
useDocumentTitle("Import");
const settings = useImportSettings();
const update = useUpdateImportSettings();

const enabled = ref(false);
watch(
  () => settings.data.value?.enabled,
  (v) => (enabled.value = !!v),
  { immediate: true },
);
const locked = computed(() => !!settings.data.value?.locked);
const dirty = computed(() => !!settings.data.value && enabled.value !== settings.data.value.enabled);
const saved = ref<string | null>(null);

async function save() {
  saved.value = null;
  try {
    const s = await update.mutateAsync(enabled.value);
    saved.value = s.enabled ? "Bulk import is turned on." : "Bulk import is turned off.";
  } catch {
    // shown by the ErrorAlert below
  }
}
</script>

<template>
  <Breadcrumbs :items="[{ label: 'Administration', to: '/admin' }, { label: 'System' }, { label: 'Import' }]" />
  <div class="page-header">
    <div class="title"><h1>Import</h1></div>
  </div>

  <LoadingState v-if="settings.isPending.value" label="Loading import settings…" />
  <ErrorAlert v-else-if="settings.isError.value" :error="settings.error.value" :on-retry="() => settings.refetch()" />
  <template v-else-if="settings.data.value">
    <section class="panel">
      <div class="panel-header"><h2>Bulk import</h2></div>
      <form class="panel-body" @submit.prevent="save">
        <p>
          Bulk import creates and updates configuration items from CSV and Excel files. Users also need the
          <strong>Bulk import</strong> permission (<code>cis.import</code>) in one of their
          <RouterLink to="/admin/profiles">permission profiles</RouterLink>, and every row is still limited by their
          class rights. Turning import on or off is recorded in the audit log.
        </p>
        <div v-if="locked" class="alert" role="status">
          <strong>Bulk import is disabled by the server configuration.</strong>
          The server sets <code>IMPORT_ALLOWED=false</code>, so import stays off whatever is set here. Ask the operator of
          this installation to change it.
        </div>
        <label class="checkbox-row">
          <input v-model="enabled" type="checkbox" :disabled="locked || update.isPending.value" aria-describedby="import-enabled-hint" />
          Bulk import enabled
        </label>
        <p id="import-enabled-hint" class="muted">
          When off, nobody can upload, map, check or commit a file. Users can still see, cancel and delete their
          remaining imports, so uploaded files can be removed.
        </p>
        <ErrorAlert v-if="update.isError.value" :error="update.error.value" title="The setting was not saved" />
        <p v-if="saved && !dirty" class="alert" role="status">{{ saved }}</p>
        <div>
          <button type="submit" class="btn btn-primary" :disabled="!dirty || locked || update.isPending.value">
            {{ update.isPending.value ? "Saving…" : "Save" }}
          </button>
        </div>
      </form>
    </section>

    <section class="panel" style="margin-top: var(--sp-4)">
      <div class="panel-header"><h2>Limits</h2></div>
      <div class="panel-body">
        <p class="muted">Set in the server configuration (see the deployment guide); shown here for reference.</p>
        <dl class="props">
          <dt>Largest file</dt>
          <dd>{{ formatBytes(settings.data.value.limits.maxFileBytes) }}</dd>
          <dt>Rows per file</dt>
          <dd>{{ settings.data.value.limits.maxRows.toLocaleString() }}</dd>
          <dt>Columns per file</dt>
          <dd>{{ settings.data.value.limits.maxColumns.toLocaleString() }}</dd>
          <dt>Characters per cell</dt>
          <dd>{{ settings.data.value.limits.maxCellChars.toLocaleString() }}</dd>
        </dl>
      </div>
    </section>
  </template>
</template>
