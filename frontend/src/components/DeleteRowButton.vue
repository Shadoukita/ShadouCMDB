<script setup lang="ts">
import { computed, ref } from "vue";
import { useRemove, useUsage, type Resource } from "../api/datamodel";
import ConfirmDialog from "./ConfirmDialog.vue";
import ErrorAlert from "./ErrorAlert.vue";
import LoadingState from "./LoadingState.vue";

/**
 * Delete for any data model or lookup row. Opening the dialog asks the API what
 * still refers to the row (GET …/usage) and says exactly what goes with it. Rows
 * in use cannot be deleted; the dialog offers archiving instead (`archive` event)
 * when the row can be archived.
 */
const props = defineProps<{
  resource: Resource;
  id: string;
  /** "status “In service”" */
  label: string;
  archivable?: boolean;
  archived?: boolean;
  small?: boolean;
}>();
const emit = defineEmits<{ deleted: []; archive: [] }>();
const open = ref(false);
const usage = useUsage(props.resource, () => props.id, open);
const del = useRemove(props.resource);

const counts = computed(() => (usage.data.value?.data ?? []).filter((u) => u.count > 0));
const blocking = computed(() => counts.value.filter((u) => u.blocking));
const cascades = computed(() => counts.value.filter((u) => !u.blocking));
const inUse = computed(() => !!usage.data.value?.inUse);

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
  if (!usage.data.value) return "Delete";
  if (inUse.value) return props.archivable && !props.archived ? "Archive instead" : "Close";
  return cascades.value.length ? "Delete with the rows listed" : "Delete";
});
</script>

<template>
  <button type="button" :class="small ? 'btn btn-sm btn-quiet-danger' : 'btn btn-danger'" :aria-label="`Delete ${label}`" @click="open = true">
    Delete
  </button>
  <ConfirmDialog
    :open="open"
    :title="`Delete ${label}?`"
    :confirm-label="confirmLabel"
    :busy="del.isPending.value || usage.isLoading.value"
    @cancel="cancel"
    @confirm="confirm"
  >
    <LoadingState v-if="usage.isLoading.value" label="Checking what uses it…" />
    <ErrorAlert v-else-if="usage.isError.value" :error="usage.error.value" title="Could not check what uses it" />
    <template v-else-if="usage.data.value">
      <template v-if="inUse">
        <p><strong>It cannot be deleted while it is in use:</strong></p>
        <ul>
          <li v-for="u in blocking" :key="u.kind">{{ u.count.toLocaleString() }} {{ u.label }}</li>
        </ul>
        <p v-if="archivable && !archived">
          Archive it instead: existing records keep it, but it can no longer be chosen for new ones.
        </p>
        <p v-else-if="archivable">It is already archived, so nobody can choose it for new records.</p>
        <p v-else>Change or remove those records first.</p>
      </template>
      <template v-else>
        <p v-if="cascades.length === 0">Nothing refers to it. This cannot be undone.</p>
        <template v-else>
          <p>These go with it:</p>
          <ul>
            <li v-for="u in cascades" :key="u.kind">{{ u.count.toLocaleString() }} {{ u.label }}</li>
          </ul>
          <p>This cannot be undone.</p>
        </template>
      </template>
    </template>
    <ErrorAlert v-if="del.isError.value" :error="del.error.value" title="Delete failed" />
  </ConfirmDialog>
</template>
