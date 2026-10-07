<script setup lang="ts">
import { computed, ref } from "vue";
import { useRemove, useUsage, type Resource } from "../api/datamodel";
import { formatNumber, t } from "../i18n";
import ConfirmDialog from "./ConfirmDialog.vue";
import ErrorAlert from "./ErrorAlert.vue";
import LoadingState from "./LoadingState.vue";

/**
 * Delete for any data model or lookup row. Opening the dialog asks the API what
 * still refers to the row (GET …/usage) and says exactly what goes with it. Rows
 * in use cannot be deleted; the dialog offers archiving instead (`archive` event)
 * when the row can be archived.
 *
 * `headless` renders no button of its own: the caller opens the dialog with the exposed `open()`, e.g. from a
 * RowMenu item. The dialog, its usage check and its API calls are the same either way.
 */
const props = defineProps<{
  resource: Resource;
  id: string;
  /** "status “In service”" */
  label: string;
  archivable?: boolean;
  archived?: boolean;
  small?: boolean;
  /** No button of its own: opened with the exposed `open()` (a RowMenu item). */
  headless?: boolean;
}>();
const emit = defineEmits<{ deleted: []; archive: [] }>();
const open = ref(false);
defineExpose({ open: () => (open.value = true) });
const usage = useUsage(props.resource, () => props.id, open);
const del = useRemove(props.resource);

type UsageCount = NonNullable<typeof usage.data.value>["data"][number];
// A count over CIs of types the user may not view comes back withheld (null):
// list the kind without a number rather than hide it.
const counts = computed(() => (usage.data.value?.data ?? []).filter((u) => u.withheld || (u.count ?? 0) > 0));
const blocking = computed(() => counts.value.filter((u) => u.blocking));
const cascades = computed(() => counts.value.filter((u) => !u.blocking));
const inUse = computed(() => !!usage.data.value?.inUse);
const line = (u: UsageCount) =>
  u.count === null ? t("dm.deleteRow.withheld", { label: u.label }) : t("dm.deleteRow.count", { n: formatNumber(u.count), label: u.label });

function cancel() {
  del.reset();
  open.value = false;
}

function confirm() {
  if (inUse.value) {
    if (props.archivable && !props.archived) {
      emit("archive");
      open.value = false;
    }
    return;
  }
  del.mutate(props.id, {
    onSuccess: () => {
      open.value = false;
      emit("deleted");
    },
  });
}

const confirmLabel = computed(() => {
  if (!usage.data.value) return t("common.delete");
  if (inUse.value) return props.archivable && !props.archived ? t("dm.deleteRow.archiveInstead") : t("dm.deleteRow.close");
  return cascades.value.length ? t("dm.deleteRow.deleteWithRows") : t("common.delete");
});
</script>

<template>
  <button
    v-if="!headless"
    type="button"
    :class="small ? 'btn btn-sm btn-quiet-danger' : 'btn btn-danger'"
    :aria-label="t('dm.deleteRow.aria', { label })"
    @click="open = true"
  >
    {{ t("common.delete") }}
  </button>
  <ConfirmDialog
    :open="open"
    :title="t('dm.deleteRow.title', { label })"
    :confirm-label="confirmLabel"
    :busy="del.isPending.value"
    :confirm-disabled="usage.isLoading.value"
    @cancel="cancel"
    @confirm="confirm"
  >
    <LoadingState v-if="usage.isLoading.value" :label="t('dm.deleteRow.checking')" />
    <ErrorAlert v-else-if="usage.isError.value" :error="usage.error.value" :title="t('dm.deleteRow.checkFailed')" />
    <template v-else-if="usage.data.value">
      <template v-if="inUse">
        <p><strong>{{ t("dm.deleteRow.inUse") }}</strong></p>
        <ul>
          <li v-for="u in blocking" :key="u.kind">{{ line(u) }}</li>
        </ul>
        <p v-if="archivable && !archived">{{ t("dm.deleteRow.archiveHint") }}</p>
        <p v-else-if="archivable">{{ t("dm.deleteRow.alreadyArchived") }}</p>
        <p v-else>{{ t("dm.deleteRow.changeFirst") }}</p>
      </template>
      <template v-else>
        <p v-if="cascades.length === 0">{{ t("dm.deleteRow.nothingRefers") }}</p>
        <template v-else>
          <p>{{ t("dm.deleteRow.cascades") }}</p>
          <ul>
            <li v-for="u in cascades" :key="u.kind">{{ line(u) }}</li>
          </ul>
          <p>{{ t("dm.deleteRow.cannotUndo") }}</p>
        </template>
      </template>
    </template>
    <ErrorAlert v-if="del.isError.value" :error="del.error.value" :title="t('dm.deleteRow.failed')" />
  </ConfirmDialog>
</template>
