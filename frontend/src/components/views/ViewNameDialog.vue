<script setup lang="ts">
import { nextTick, ref, useId, watch } from "vue";
import type { SaveFailure } from "../../lib/savedViews";

/**
 * The name dialog of saved views: Save as new view, Rename, Copy to my views and
 * Share a copy. The parent sends the request and passes its failure back: a name
 * error shows under Name and takes focus, definition errors are listed, the view
 * limit links to Manage views. Modal (<dialog>): focus stays inside, starts on
 * Name, and returns to the control that opened it.
 */
const props = defineProps<{
  open: boolean;
  title: string;
  submitLabel: string;
  /** What the dialog does, in a sentence under the title. */
  intro?: string;
  name: string;
  description?: string | null;
  /** Rename and Save as edit the description too; the copies take the source's. */
  withDescription?: boolean;
  /** Offer "Share with everyone" (Save as, for users with views.share). */
  withShare?: boolean;
  busy?: boolean;
  failure?: SaveFailure | null;
}>();
const emit = defineEmits<{ submit: [value: { name: string; description: string | null; share: boolean }]; cancel: []; manage: [] }>();

const uid = useId();
const ids = { title: `vn-title-${uid}`, name: `vn-name-${uid}`, nameErr: `vn-name-err-${uid}`, desc: `vn-desc-${uid}`, descErr: `vn-desc-err-${uid}` };
const dialog = ref<HTMLDialogElement>();
const nameInput = ref<HTMLInputElement>();
const name = ref("");
const description = ref("");
const share = ref(false);
const clientError = ref<string | null>(null);
let opener: HTMLElement | null = null;

watch(
  () => props.open,
  async (open) => {
    const d = dialog.value;
    if (!d) return;
    if (open && !d.open) {
      opener = document.activeElement as HTMLElement | null;
      name.value = props.name;
      description.value = props.description ?? "";
      share.value = false;
      clientError.value = null;
      d.showModal();
      await nextTick();
      nameInput.value?.select();
    } else if (!open && d.open) {
      d.close();
      opener?.focus();
      opener = null;
    }
  },
  { flush: "post" },
);

// A name error (duplicate, empty) puts focus back on the field (§1.6).
watch(
  () => (props.failure?.kind === "fields" ? props.failure.name : undefined),
  async (err) => {
    if (!err) return;
    await nextTick();
    nameInput.value?.focus();
  },
);

const nameError = () => clientError.value ?? (props.failure?.kind === "fields" ? props.failure.name : undefined);
const descriptionError = () => (props.failure?.kind === "fields" ? props.failure.description : undefined);

function submit() {
  const n = name.value.trim();
  if (!n) {
    clientError.value = "Enter a name.";
    nameInput.value?.focus();
    return;
  }
  clientError.value = null;
  emit("submit", { name: n, description: description.value.trim() || null, share: share.value });
}
function cancel(e?: Event) {
  e?.preventDefault();
  if (!props.busy) emit("cancel");
}
</script>

<template>
  <Teleport to="body">
    <dialog ref="dialog" class="confirm view-dialog" :aria-labelledby="ids.title" aria-modal="true" @cancel="cancel">
      <form novalidate @submit.prevent="submit">
        <h2 :id="ids.title">{{ title }}</h2>
        <div class="body stack">
          <p v-if="intro" class="muted dialog-intro">{{ intro }}</p>
          <div v-if="failure?.kind === 'limit'" class="alert alert-error" role="alert">
            {{ failure.message }}
            <button type="button" class="btn btn-sm spaced" @click="emit('manage')">Manage views</button>
          </div>
          <div v-else-if="failure?.kind === 'fields' && (failure.definition.length > 0 || failure.other)" class="alert alert-error" role="alert">
            <strong>The view cannot be saved as it is.</strong>
            <ul v-if="failure.definition.length > 0">
              <li v-for="(m, i) in failure.definition" :key="i">{{ m }}</li>
            </ul>
            <p v-if="failure.other" class="dialog-intro">{{ failure.other }}</p>
          </div>
          <div class="field">
            <label :for="ids.name">Name<span class="req" aria-hidden="true">*</span></label>
            <input
              :id="ids.name"
              ref="nameInput"
              v-model="name"
              type="text"
              maxlength="100"
              required
              autocomplete="off"
              :aria-invalid="!!nameError()"
              :aria-describedby="nameError() ? ids.nameErr : undefined"
            />
            <span v-if="nameError()" :id="ids.nameErr" class="error">{{ nameError() }}</span>
          </div>
          <div v-if="withDescription" class="field">
            <label :for="ids.desc">Description <span class="muted">(optional)</span></label>
            <textarea
              :id="ids.desc"
              v-model="description"
              rows="2"
              maxlength="500"
              :aria-invalid="!!descriptionError()"
              :aria-describedby="descriptionError() ? ids.descErr : undefined"
            />
            <span v-if="descriptionError()" :id="ids.descErr" class="error">{{ descriptionError() }}</span>
          </div>
          <label v-if="withShare" class="checkbox-row">
            <input v-model="share" type="checkbox" /> Share with everyone
            <span class="muted">(all users who may view its classes)</span>
          </label>
        </div>
        <div class="footer">
          <button type="button" class="btn" :disabled="busy" @click="cancel()">Cancel</button>
          <button type="submit" class="btn btn-primary" :disabled="busy">{{ busy ? "Saving…" : submitLabel }}</button>
        </div>
      </form>
    </dialog>
  </Teleport>
</template>
