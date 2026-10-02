<script setup lang="ts">
import { nextTick, ref, watch } from "vue";
import { formatDateTime } from "../../lib/format";

/**
 * Saving a view someone changed since it was loaded (409 VERSION_CONFLICT, §1.5):
 * load their version, keep this list as a new view, or go back to it unsaved.
 */
const props = defineProps<{ open: boolean; name: string; updatedBy?: string; updatedAt?: string }>();
const emit = defineEmits<{ latest: []; saveAs: []; cancel: [] }>();
const dialog = ref<HTMLDialogElement>();
const first = ref<HTMLButtonElement>();
let opener: HTMLElement | null = null;

watch(
  () => props.open,
  async (open) => {
    const d = dialog.value;
    if (!d) return;
    if (open && !d.open) {
      opener = document.activeElement as HTMLElement | null;
      d.showModal();
      await nextTick();
      first.value?.focus();
    } else if (!open && d.open) {
      d.close();
      opener?.focus();
      opener = null;
    }
  },
  { flush: "post" },
);
function cancel(e: Event) {
  e.preventDefault();
  emit("cancel");
}
</script>

<template>
  <Teleport to="body">
    <dialog ref="dialog" class="confirm view-dialog" aria-labelledby="view-conflict-title" aria-modal="true" @cancel="cancel">
      <h2 id="view-conflict-title">The view “{{ name }}” was changed elsewhere</h2>
      <div class="body">
        <p class="dialog-intro">
          This view was changed elsewhere<template v-if="updatedBy"> (by <strong>{{ updatedBy }}</strong><template v-if="updatedAt">, {{ formatDateTime(updatedAt) }}</template>)</template>
          after you loaded it. Your changes were not saved.
        </p>
      </div>
      <div class="footer">
        <button ref="first" type="button" class="btn" @click="emit('latest')">Load latest</button>
        <button type="button" class="btn" @click="emit('saveAs')">Save as new view</button>
        <button type="button" class="btn btn-primary" @click="emit('cancel')">Cancel</button>
      </div>
    </dialog>
  </Teleport>
</template>
