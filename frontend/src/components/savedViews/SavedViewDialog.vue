<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import { ApiError } from "../../api/client";
import ErrorAlert from "../ErrorAlert.vue";
import FormDialog from "../FormDialog.vue";
import { t } from "../../i18n";

export type SavedViewDialogMode = "create" | "rename" | "copy" | "share";
export interface SavedViewDialogValues {
  name: string;
  description: string | null;
  shared: boolean;
}

/**
 * Name (and description) of a view: Save as new view, Rename, Copy to my views
 * and Share a copy. The API's field errors show under their fields; a name error
 * moves focus to the name (§1.6). The limit and anything else show above the fields.
 */
const props = defineProps<{
  open: boolean;
  mode: SavedViewDialogMode;
  initial: { name: string; description?: string | null };
  /** The user has `views.share`: Save as offers "Share with everyone". */
  canShare: boolean;
  busy: boolean;
  error: unknown;
}>();
const emit = defineEmits<{ submit: [values: SavedViewDialogValues]; cancel: []; manage: [] }>();

const name = ref("");
const description = ref("");
const shared = ref(false);
const nameInput = ref<HTMLInputElement>();
const localError = ref<string | null>(null);

watch(
  () => props.open,
  (open) => {
    if (!open) return;
    name.value = props.initial.name;
    description.value = props.initial.description ?? "";
    shared.value = props.mode === "share";
    localError.value = null;
  },
  { immediate: true },
);

const title = computed(() => t(`views.dialog.${props.mode}`));
const submitLabel = computed(() => t(`views.dialog.submit.${props.mode}`));
const withDescription = computed(() => props.mode === "create" || props.mode === "rename");

const apiError = computed(() => (props.error instanceof ApiError ? props.error : null));
const errorCode = computed(() => apiError.value?.details[0]?.code);
const limitReached = computed(() => apiError.value?.code === "CONFLICT" && errorCode.value === "limit_reached");
const fieldErrors = computed(() => apiError.value?.fieldErrors() ?? {});
const nameError = computed(() => localError.value ?? fieldErrors.value.name ?? (errorCode.value === "duplicate_name" ? apiError.value?.message : undefined));
const descriptionError = computed(() => fieldErrors.value.description);
/** Problems with the view's filters, sort or columns, listed in the dialog. */
const definitionErrors = computed(() =>
  (apiError.value?.details ?? []).filter((d) => d.field.startsWith("definition")).map((d) => d.message),
);
const otherError = computed(
  () => !!props.error && !limitReached.value && !nameError.value && !descriptionError.value && definitionErrors.value.length === 0,
);

watch(nameError, async (e) => {
  if (e && props.open) {
    await nextTick();
    nameInput.value?.focus();
  }
});

function submit() {
  const n = name.value.trim();
  if (!n) {
    localError.value = t("views.dialog.nameRequired");
    return void nameInput.value?.focus();
  }
  localError.value = null;
  emit("submit", { name: n, description: description.value.trim() || null, shared: shared.value });
}
</script>

<template>
  <FormDialog :open="open" :title="title" :submit-label="submitLabel" :busy="busy" @submit="submit" @cancel="emit('cancel')">
    <div class="stack">
      <div v-if="limitReached" class="alert alert-error" role="alert">
        {{ apiError?.message }}
        <div><button type="button" class="btn btn-sm" @click="emit('manage')">{{ t("views.action.manage") }}</button></div>
      </div>
      <ErrorAlert v-else-if="otherError" :error="error" :title="t('views.error.notSaved')" />
      <div v-if="definitionErrors.length > 0" class="alert alert-error" role="alert">
        {{ t("views.dialog.invalid") }}
        <ul>
          <li v-for="m in definitionErrors" :key="m">{{ m }}</li>
        </ul>
      </div>
      <p v-if="mode === 'share'" class="muted">{{ t("views.dialog.shareNote") }}</p>
      <div class="field">
        <label for="sv-name">{{ t("views.dialog.name") }}<span class="req" aria-hidden="true">*</span></label>
        <input
          id="sv-name"
          ref="nameInput"
          v-model="name"
          type="text"
          maxlength="100"
          required
          autocomplete="off"
          :aria-invalid="!!nameError"
          :aria-describedby="nameError ? 'sv-name-err' : undefined"
        />
        <span v-if="nameError" id="sv-name-err" class="error">{{ nameError }}</span>
      </div>
      <div v-if="withDescription" class="field">
        <label for="sv-description">{{ t("views.dialog.description") }}</label>
        <textarea
          id="sv-description"
          v-model="description"
          rows="2"
          maxlength="500"
          :aria-invalid="!!descriptionError"
          :aria-describedby="descriptionError ? 'sv-description-err' : undefined"
        />
        <span v-if="descriptionError" id="sv-description-err" class="error">{{ descriptionError }}</span>
      </div>
      <label v-if="mode === 'create' && canShare" class="checkbox-row" for="sv-shared">
        <input id="sv-shared" v-model="shared" type="checkbox" /> {{ t("views.dialog.shareWithEveryone") }}
      </label>
    </div>
  </FormDialog>
</template>
