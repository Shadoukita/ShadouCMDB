<script setup lang="ts">
import { onMounted, ref, useId, watch } from "vue";
import { t } from "../i18n";

/**
 * Modal confirmation for destructive actions. Uses <dialog> for focus trapping and Esc handling.
 * Rendered under <body> so a dialog opened from a table row does not inherit the cell's
 * nowrap/ellipsis/right-align or the row-actions sibling margin that pins it left (GH#279).
 */
const props = defineProps<{
  open: boolean;
  title: string;
  confirmLabel: string;
  busy?: boolean;
  /** Keeps the confirm button disabled (not the cancel one), e.g. while the dialog still checks what it will delete. */
  confirmDisabled?: boolean;
  cancelLabel?: string;
  busyLabel?: string;
  /** "primary" for a confirmation that destroys nothing (load the latest version). */
  tone?: "danger" | "primary";
}>();
const emit = defineEmits<{ confirm: []; cancel: [] }>();
const dialog = ref<HTMLDialogElement>();
/** Unique per dialog: a page can hold several (disable, reset, delete…), each named by its own title. */
const titleId = `confirm-title-${useId()}`;

function sync() {
  const d = dialog.value;
  if (!d) return;
  if (props.open && !d.open) d.showModal();
  if (!props.open && d.open) d.close();
}
onMounted(sync);
watch(() => props.open, sync);

function onCancel(e: Event) {
  e.preventDefault();
  if (!props.busy) emit("cancel");
}
</script>

<template>
  <Teleport to="body">
    <dialog ref="dialog" class="confirm" :aria-labelledby="titleId" @cancel="onCancel">
      <h2 :id="titleId">{{ title }}</h2>
      <div class="body"><slot /></div>
      <div class="footer">
        <button type="button" class="btn" :disabled="busy" autofocus @click="emit('cancel')">{{ cancelLabel ?? t("common.cancel") }}</button>
        <button type="button" :class="['btn', tone === 'primary' ? 'btn-primary' : 'btn-danger']" :disabled="busy || confirmDisabled" @click="emit('confirm')">
          {{ busy ? (busyLabel ?? t("common.working")) : confirmLabel }}
        </button>
      </div>
    </dialog>
  </Teleport>
</template>
