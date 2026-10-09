<script setup lang="ts">
import { computed, ref } from "vue";
import { ApiError } from "../../api/client";
import { newIdempotencyKey, useCreateImport, type ImportJob, type ImportLimits } from "../../api/imports";
import { t, tAround } from "../../i18n";
import { formatBytes } from "../../lib/format";
import { checkFile, uploadErrorMessage, type FileFormat } from "../../lib/imports";

/**
 * Step 1 before there is a job: choose a file and upload it. The file field is the primary control; the drop
 * zone around it only adds drag and drop and is not a tab stop of its own. The browser checks the extension and
 * size first for a quick message; the server checks everything again.
 */
const props = defineProps<{ limits: ImportLimits }>();
const emit = defineEmits<{ uploaded: [job: ImportJob] }>();

const create = useCreateImport();
const file = ref<File | null>(null);
const format = ref<FileFormat | null>(null);
/** One key per chosen file: uploading it again after a dropped connection returns the job it already created. */
let idempotencyKey = "";
const localError = ref("");
const dragging = ref(false);

const serverError = computed(() => (create.error.value instanceof ApiError ? create.error.value : null));
const errorText = computed(() => {
  if (localError.value) return localError.value;
  const e = create.error.value;
  if (!e) return "";
  return e instanceof ApiError ? uploadErrorMessage(e, props.limits) : String(e);
});
const label = computed(() => t("imports.upload.label", { size: formatBytes(props.limits.maxFileBytes), rows: props.limits.maxRows }));
const uploadingText = computed(() =>
  file.value ? tAround("imports.upload.uploading", "file", { size: formatBytes(file.value.size) }) : ["", ""],
);

function choose(f: File | undefined) {
  create.reset();
  localError.value = "";
  file.value = null;
  format.value = null;
  if (!f) return;
  const checked = checkFile(f, props.limits);
  if ("error" in checked) {
    localError.value = checked.error;
    return;
  }
  file.value = f;
  format.value = checked.format;
  idempotencyKey = newIdempotencyKey();
  upload();
}

async function upload() {
  if (!file.value || !format.value) return;
  try {
    const job = await create.mutateAsync({ file: file.value, format: format.value, idempotencyKey });
    emit("uploaded", job);
  } catch {
    // shown at the file field
  }
}

function onChange(e: Event) {
  const el = e.target as HTMLInputElement;
  choose(el.files?.[0]);
  el.value = "";
}
function onDrop(e: DragEvent) {
  dragging.value = false;
  if (create.isPending.value) return;
  choose(e.dataTransfer?.files?.[0]);
}
</script>

<template>
  <div
    :class="['import-drop', { dragging }]"
    @dragover.prevent="dragging = true"
    @dragleave="dragging = false"
    @drop.prevent="onDrop"
  >
    <div class="field">
      <label for="import-file">{{ label }}</label>
      <input
        id="import-file"
        type="file"
        accept=".csv,.xlsx,text/csv,application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        :disabled="create.isPending.value"
        :aria-invalid="errorText ? 'true' : undefined"
        :aria-describedby="errorText ? 'import-file-error import-file-hint' : 'import-file-hint'"
        @change="onChange"
      />
      <p id="import-file-hint" class="hint">{{ t("imports.upload.hint") }}</p>
      <div v-if="errorText" id="import-file-error" class="alert alert-error" role="alert">
        <strong>{{ t("imports.upload.failed") }}</strong>
        <div>{{ errorText }}</div>
        <div v-if="serverError?.requestId" class="meta">{{ t("error.requestId") }} <code>{{ serverError.requestId }}</code></div>
        <button v-if="serverError && (serverError.status === 0 || serverError.status >= 500 || serverError.status === 408) && file" type="button" class="btn btn-sm" @click="upload">
          {{ t("imports.upload.tryAgain") }}
        </button>
      </div>
    </div>
    <div v-if="create.isPending.value && file" class="import-progress" role="status">
      <div class="import-progress-label">
        <span>{{ uploadingText[0] }}<em>{{ file.name }}</em>{{ uploadingText[1] }}</span>
      </div>
      <div class="import-progress-track"><div class="import-progress-fill indeterminate" /></div>
    </div>
  </div>
</template>
