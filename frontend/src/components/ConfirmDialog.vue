<script setup lang="ts">
import { onMounted, ref, watch } from "vue";

/** Modal confirmation for destructive actions. Uses <dialog> for focus trapping and Esc handling. */
const props = defineProps<{ open: boolean; title: string; confirmLabel: string; busy?: boolean }>();
const emit = defineEmits<{ confirm: []; cancel: [] }>();
const dialog = ref<HTMLDialogElement>();

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
  <dialog ref="dialog" class="confirm" aria-labelledby="confirm-title" @cancel="onCancel">
    <h2 id="confirm-title">{{ title }}</h2>
    <div class="body"><slot /></div>
    <div class="footer">
      <button type="button" class="btn" :disabled="busy" autofocus @click="emit('cancel')">Cancel</button>
      <button type="button" class="btn btn-danger" :disabled="busy" @click="emit('confirm')">
        {{ busy ? "Working…" : confirmLabel }}
      </button>
    </div>
  </dialog>
</template>
