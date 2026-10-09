<script setup lang="ts">
import { adminCrumbs } from "./sections";
import { computed, ref, watch } from "vue";
import { RouterLink } from "vue-router";
import { useImportSettings, useUpdateImportSettings } from "../../api/imports";
import Breadcrumbs from "../../components/Breadcrumbs.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import Icon from "../../components/Icon.vue";
import LoadingState from "../../components/LoadingState.vue";
import SaveBar from "../../components/SaveBar.vue";
import { formatNumber, t, tAround } from "../../i18n";
import { useDocumentTitle } from "../../lib/composables";
import { formatBytes } from "../../lib/format";
import { useFlashStore } from "../../stores/flash";

/**
 * Administration › Import (Administrator): the instance switch for bulk import. It is off after installation
 * (D4); turning it on or off is audited. The server configuration can forbid import altogether
 * (IMPORT_ALLOWED=false), and then the switch is locked off. The limits are server configuration too and only
 * shown here. The page has the CI page's head band without tabs and saves through the shared save bar
 * (design §2.7, audit A3), like every other administration edit page.
 */
useDocumentTitle(() => t("admin.section.import"));
const settings = useImportSettings();
const update = useUpdateImportSettings();
const flash = useFlashStore();

const enabled = ref(false);
watch(
  () => settings.data.value?.enabled,
  (v) => (enabled.value = !!v),
  { immediate: true },
);
const locked = computed(() => !!settings.data.value?.locked);
const dirty = computed(() => !!settings.data.value && enabled.value !== settings.data.value.enabled);
const intro = computed(() => tAround("importSettings.intro", "link"));

function discard() {
  enabled.value = !!settings.data.value?.enabled;
}

async function save() {
  if (!dirty.value) return;
  try {
    const s = await update.mutateAsync(enabled.value);
    flash.show(s.enabled ? t("importSettings.savedOn") : t("importSettings.savedOff"));
  } catch {
    // shown by the ErrorAlert above the form
  }
}
</script>

<template>
  <div class="record-head record-head-plain">
    <Breadcrumbs :items="adminCrumbs('import')" />
    <div class="page-header record-header">
      <div class="record-heading">
        <span class="class-tile class-tile-lg" aria-hidden="true"><Icon name="upload" class="class-icon" /></span>
        <div class="record-title">
          <div class="title">
            <h1>{{ t("admin.section.import") }}</h1>
          </div>
          <p v-if="settings.data.value" class="record-meta" data-testid="record-meta">
            <span v-if="locked" class="badge warn"><span class="status-dot" aria-hidden="true" />{{ t("importSettings.status.locked") }}</span>
            <span v-else :class="['badge', settings.data.value.enabled ? 'ok' : 'off']"
              ><span class="status-dot" aria-hidden="true" />{{ settings.data.value.enabled ? t("importSettings.status.on") : t("importSettings.status.off") }}</span
            >
            <span class="badge">{{ t("importSettings.adminOnly") }}</span>
            <span class="record-meta-line">{{ t("importSettings.audited") }}</span>
          </p>
        </div>
      </div>
    </div>
  </div>

  <LoadingState v-if="settings.isPending.value" :label="t('imports.settingsLoading')" />
  <ErrorAlert v-else-if="settings.isError.value" :error="settings.error.value" :on-retry="() => settings.refetch()" />
  <template v-else-if="settings.data.value">
    <ErrorAlert v-if="update.isError.value" :error="update.error.value" :title="t('importSettings.notSaved')" />
    <form id="import-settings-form" class="stack" :aria-label="t('importSettings.formLabel')" @submit.prevent="save">
      <section class="panel" aria-labelledby="import-switch-title">
        <div class="panel-header"><h2 id="import-switch-title">{{ t("importSettings.switch.title") }}</h2></div>
        <div class="panel-body stack">
          <p>
            {{ intro[0] }}<RouterLink to="/admin/profiles">{{ t("importSettings.introLink") }}</RouterLink>{{ intro[1] }}
          </p>
          <div v-if="locked" class="alert alert-warn" role="status">
            <strong>{{ t("imports.off.locked") }}</strong>
            {{ t("importSettings.lockedBody") }}
          </div>
          <div class="field">
            <label class="checkbox-row">
              <input v-model="enabled" type="checkbox" :disabled="locked || update.isPending.value" aria-describedby="import-enabled-hint" />
              {{ t("importSettings.switch.label") }}
            </label>
            <span id="import-enabled-hint" class="hint">{{ t("importSettings.switch.hint") }}</span>
          </div>
        </div>
      </section>

      <section class="panel" aria-labelledby="import-limits-title">
        <div class="panel-header"><h2 id="import-limits-title">{{ t("importSettings.limits.title") }}</h2></div>
        <div class="panel-body stack">
          <p class="muted">{{ t("importSettings.limits.hint") }}</p>
          <dl class="props">
            <dt>{{ t("importSettings.limits.file") }}</dt>
            <dd class="mono">{{ formatBytes(settings.data.value.limits.maxFileBytes) }}</dd>
            <dt>{{ t("importSettings.limits.rows") }}</dt>
            <dd class="mono">{{ formatNumber(settings.data.value.limits.maxRows) }}</dd>
            <dt>{{ t("importSettings.limits.columns") }}</dt>
            <dd class="mono">{{ formatNumber(settings.data.value.limits.maxColumns) }}</dd>
            <dt>{{ t("importSettings.limits.cell") }}</dt>
            <dd class="mono">{{ formatNumber(settings.data.value.limits.maxCellChars) }}</dd>
          </dl>
        </div>
      </section>
    </form>

    <SaveBar :label="t('record.save.region')" :dirty="dirty">
      <button v-if="dirty" type="button" class="btn" :disabled="update.isPending.value" @click="discard">{{ t("record.save.discard") }}</button>
      <button type="submit" form="import-settings-form" class="btn btn-primary" :disabled="!dirty || locked || update.isPending.value">
        {{ update.isPending.value ? t("common.saving") : t("common.saveChanges") }}
      </button>
    </SaveBar>
  </template>
</template>
