<script setup lang="ts">
import { computed, ref } from "vue";
import { useRouter } from "vue-router";
import { useCancelImport, useDeleteImport, type ImportJob } from "../../api/imports";
import ConfirmDialog from "../../components/ConfirmDialog.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import Icon from "../../components/Icon.vue";
import RowMenu, { type RowMenuItem } from "../../components/RowMenu.vue";
import { formatNumber, t } from "../../i18n";
import { RUNNING } from "../../lib/imports";

/**
 * The wizard's actions in the page head (audit M2: one Stop pattern). While the job runs, in any phase, a
 * secondary Stop import button; once it has stopped, Delete import in the head's menu. Both ask first, with the
 * same dialogs as the Imports list. Stop and Delete stay available while import is turned off (W3) and on another
 * user's job for an administrator, as the server allows both.
 */
const props = defineProps<{ job: ImportJob }>();

const router = useRouter();
const cancel = useCancelImport();
const del = useDeleteImport();

const running = computed(() => RUNNING.has(props.job.status) && props.job.status !== "uploading");
const deletable = computed(() => !RUNNING.has(props.job.status));

const stopping = ref(false);
const deleting = ref(false);

function openStop() {
  cancel.reset();
  stopping.value = true;
}
async function confirmStop() {
  try {
    await cancel.mutateAsync(props.job.id);
    stopping.value = false;
  } catch {
    // shown in the dialog
  }
}
async function confirmDelete() {
  try {
    await del.mutateAsync(props.job.id);
    deleting.value = false;
    router.push("/imports");
  } catch {
    // shown in the dialog
  }
}

const menu = computed<RowMenuItem[]>(() => [
  {
    label: t("imports.delete"),
    danger: true,
    action: () => {
      del.reset();
      deleting.value = true;
    },
  },
]);
</script>

<template>
  <div v-if="running || deletable" class="actions">
    <button v-if="running" type="button" class="btn" :disabled="cancel.isPending.value" @click="openStop">
      <Icon name="square" :size="14" /> {{ t("imports.stop") }}
    </button>
    <RowMenu v-else :label="t('record.actions.more')" :items="menu" large />
  </div>

  <ConfirmDialog
    :open="stopping"
    :title="t('imports.stopDialog.title', { file: job.file.name })"
    :confirm-label="t('imports.stop')"
    :busy="cancel.isPending.value"
    @cancel="stopping = false"
    @confirm="confirmStop"
  >
    <ErrorAlert v-if="cancel.isError.value" :error="cancel.error.value" :title="t('imports.stopDialog.failed')" />
    <template v-if="job.phase === 'commit'">
      <p>{{ t("imports.stopDialog.progress", { done: formatNumber(job.progress.done), total: formatNumber(job.progress.total) }) }}</p>
      <p>{{ t("imports.stopDialog.again") }}</p>
    </template>
    <p v-else>{{ t("imports.stopDialog.nothingYet") }}</p>
  </ConfirmDialog>

  <ConfirmDialog
    :open="deleting"
    :title="t('imports.deleteDialog.title', { file: job.file.name })"
    :confirm-label="t('imports.delete')"
    :busy="del.isPending.value"
    @cancel="deleting = false"
    @confirm="confirmDelete"
  >
    <ErrorAlert v-if="del.isError.value" :error="del.error.value" :title="t('imports.deleteDialog.failed')" />
    <p>{{ t("imports.deleteDialog.body") }}</p>
  </ConfirmDialog>
</template>
