<script setup lang="ts">
import { computed, ref } from "vue";
import { useRestoreUiSettings, useUiSettingsVersion, useUiSettingsVersions } from "../../../api/uiSettings";
import ConfirmDialog from "../../../components/ConfirmDialog.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import PaginationBar from "../../../components/PaginationBar.vue";
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
const ACTORS = { system: "system", user: "", api_client: "API client", import: "import" } as const;
const shownJson = computed(() => (shownVersion.data.value ? JSON.stringify(shownVersion.data.value.settings, null, 2) : ""));

async function onRestore() {
  if (confirming.value === null) return;
  error.value = null;
  try {
    await restore.mutateAsync({ restore: confirming.value, version: props.current, comment: `Restored version ${confirming.value}` });
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
    <div class="panel-header"><h2>Saved versions</h2><span class="muted">Newest first</span></div>
    <ErrorAlert v-if="error" :error="error" title="Not restored" />
    <LoadingState v-if="versions.isLoading.value" />
    <div v-else-if="versions.isError.value" class="panel-body"><ErrorAlert :error="versions.error.value" :on-retry="() => versions.refetch()" /></div>
    <div v-else-if="versions.data.value" class="panel-body flush">
      <table class="data">
        <thead>
          <tr>
            <th scope="col">Version</th>
            <th scope="col">Saved</th>
            <th scope="col">By</th>
            <th scope="col">Comment</th>
            <th scope="col"><span class="sr-only">Actions</span></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="v in versions.data.value.data" :key="v.version" :class="{ selected: shown === v.version }">
            <td>{{ v.version }} <span v-if="v.isCurrent" class="badge ok">current</span></td>
            <td>{{ formatDateTime(v.createdAt) }}</td>
            <td>{{ v.actorName ?? "" }} <span v-if="ACTORS[v.actorType]" class="muted">({{ ACTORS[v.actorType] }})</span></td>
            <td>{{ v.comment ?? "" }}</td>
            <td class="row-actions">
              <button type="button" class="btn btn-sm" @click="shown = shown === v.version ? undefined : v.version">{{ shown === v.version ? "Hide" : "Show" }}</button>
              <button v-if="!v.isCurrent" type="button" class="btn btn-sm" @click="confirming = v.version">Restore</button>
            </td>
          </tr>
        </tbody>
      </table>
      <PaginationBar :total="versions.data.value.page.total" :limit="page.limit" :offset="page.offset" @change="(p) => (page = p)" />
    </div>
  </section>
  <section v-if="shown !== undefined" class="panel" style="margin-top: var(--sp-4)">
    <div class="panel-header"><h2>Version {{ shown }} as stored</h2></div>
    <div class="panel-body">
      <LoadingState v-if="shownVersion.isLoading.value" />
      <ErrorAlert v-else-if="shownVersion.isError.value" :error="shownVersion.error.value" />
      <pre v-else class="json">{{ shownJson }}</pre>
    </div>
  </section>

  <ConfirmDialog :open="confirming !== null" :title="`Restore version ${confirming}?`" confirm-label="Restore" :busy="restore.isPending.value" @confirm="onRestore" @cancel="confirming = null">
    Version {{ confirming }} is saved again as the newest version and applies to every user. The current version
    ({{ current }}) stays in the history.
    <strong v-if="dirty"> Your unsaved changes in the other sections are discarded.</strong>
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
