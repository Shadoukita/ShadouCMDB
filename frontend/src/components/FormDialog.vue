<script setup lang="ts">
import { onMounted, ref, watch } from "vue";
import { t } from "../i18n";

/**
 * Modal form (create/edit a row) on <dialog>, like ConfirmDialog: focus stays
 * inside, Esc cancels unless a save is running. The fields come from the slot;
 * submitting the form emits `submit`.
 */
const props = defineProps<{ open: boolean; title: string; submitLabel: string; busy?: boolean; wide?: boolean }>();
const emit = defineEmits<{ submit: []; cancel: [] }>();
const dialog = ref<HTMLDialogElement>();

function sync() {
  const d = dialog.value;
  if (!d) return;
  if (props.open && !d.open) d.showModal();
  if (!props.open && d.open) d.close();
}
onMounted(sync);
watch(() => props.open, sync, { flush: "post" });

function onCancel(e: Event) {
  e.preventDefault();
  if (!props.busy) emit("cancel");
}
</script>

<template>
  <dialog ref="dialog" :class="['confirm', 'form-dialog', { wide }]" :aria-label="title" @cancel="onCancel">
    <form novalidate @submit.prevent="emit('submit')">
      <h2>{{ title }}</h2>
      <div v-if="open" class="body"><slot /></div>
      <div class="footer">
        <button type="button" class="btn" :disabled="busy" @click="emit('cancel')">{{ t("common.cancel") }}</button>
        <button type="submit" class="btn btn-primary" :disabled="busy">{{ busy ? t("common.saving") : submitLabel }}</button>
      </div>
    </form>
  </dialog>
</template>
