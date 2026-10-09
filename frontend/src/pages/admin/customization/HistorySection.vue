<script setup lang="ts">
import { computed, ref } from "vue";
import { useRestoreUiSettings, useUiSettingsVersion, useUiSettingsVersions } from "../../../api/uiSettings";
import ConfirmDialog from "../../../components/ConfirmDialog.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import PaginationBar from "../../../components/PaginationBar.vue";
import { t } from "../../../i18n";
import { formatDateTime } from "../../../lib/format";

/** Customization › History: every saved version; restoring one saves it again as the newest. */
const props = defineProps<{ current: number; dirty: boolean }>();
const emit = defineEmits<{ restored: [] }>();
const page = ref({ limit: 20, offset: 0 });
const versions = useUiSettingsVersions(page);
const restore = useRestoreUiSettings();
const confirming = ref<number | null>(null);
const shown = ref<number | undefined>();
const shownVersion = useUiSettingsVersion(shown);
const error = ref<unknown>(null);
/** How a version was saved; a user's own name says enough. */
const actorLabel = (type: "system" | "user" | "api_client" | "import") => (type === "user" ? "" : t(`customization.history.actor.${type}`));
const shownJson = computed(() => (shownVersion.data.value ? JSON.stringify(shownVersion.data.value.settings, null, 2) : ""));

async function onRestore() {
  if (confirming.value === null) return;
  error.value = null;
  try {
    await restore.mutateAsync({ restore: confirming.value, version: props.current, comment: t("customization.history.restoredComment", { n: confirming.value }) });
    confirming.value = null;
    emit("restored");
  } catch (e) {
    error.value = e;
    confirming.value = null;
  }
}
</script>

<template>
  <section class="panel">
    <div class="panel-header"><h2>{{ t("customization.history.title") }}</h2><span class="muted">{{ t("customization.history.newestFirst") }}</span></div>
    <ErrorAlert v-if="error" :error="error" :title="t('customization.history.notRestored')" />
    <LoadingState v-if="versions.isLoading.value" />
    <div v-else-if="versions.isError.value" class="panel-body"><ErrorAlert :error="versions.error.value" :on-retry="() => versions.refetch()" /></div>
    <div v-else-if="versions.data.value" class="panel-body flush">
      <table class="data list-table">
        <thead>
          <tr>
            <th scope="col">{{ t("customization.history.colVersion") }}</th>
            <th scope="col">{{ t("customization.history.colSaved") }}</th>
            <th scope="col">{{ t("customization.history.colBy") }}</th>
            <th scope="col">{{ t("customization.history.colComment") }}</th>
            <th scope="col"><span class="sr-only">{{ t("customization.actions") }}</span></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="v in versions.data.value.data" :key="v.version" :class="{ selected: shown === v.version }">
            <td><span class="mono">{{ v.version }}</span> <span v-if="v.isCurrent" class="badge ok"><span class="status-dot" aria-hidden="true" />{{ t("customization.history.current") }}</span></td>
            <td>{{ formatDateTime(v.createdAt) }}</td>
            <td>{{ v.actorName ?? "" }} <span v-if="actorLabel(v.actorType)" class="muted">({{ actorLabel(v.actorType) }})</span></td>
            <td>{{ v.comment ?? "" }}</td>
            <td class="row-actions">
              <button type="button" class="btn btn-sm" @click="shown = shown === v.version ? undefined : v.version">{{ shown === v.version ? t("customization.history.hide") : t("customization.history.show") }}</button>
              <button v-if="!v.isCurrent" type="button" class="btn btn-sm" @click="confirming = v.version">{{ t("customization.history.restore") }}</button>
            </td>
          </tr>
        </tbody>
      </table>
      <PaginationBar numbered :total="versions.data.value.page.total" :limit="page.limit" :offset="page.offset" @change="(p) => (page = p)" />
    </div>
  </section>
  <section v-if="shown !== undefined" class="panel space-above">
    <div class="panel-header"><h2>{{ t("customization.history.asStored", { n: shown }) }}</h2></div>
    <div class="panel-body">
      <LoadingState v-if="shownVersion.isLoading.value" />
      <ErrorAlert v-else-if="shownVersion.isError.value" :error="shownVersion.error.value" />
      <pre v-else class="json">{{ shownJson }}</pre>
    </div>
  </section>

  <ConfirmDialog :open="confirming !== null" :title="t('customization.history.restoreTitle', { n: confirming })" :confirm-label="t('customization.history.restore')" :busy="restore.isPending.value" @confirm="onRestore" @cancel="confirming = null">
    {{ t("customization.history.restoreBody", { n: confirming, current }) }}
    <strong v-if="dirty"> {{ t("customization.history.restoreDirty") }}</strong>
  </ConfirmDialog>
</template>

<style scoped>
pre.json {
  max-height: 420px;
  overflow: auto;
  margin: 0;
  font-family: var(--font-mono);
  font-size: var(--fs-sm);
}
</style>
