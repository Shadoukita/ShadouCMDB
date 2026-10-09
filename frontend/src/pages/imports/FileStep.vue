<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useUpdateImportFileOptions, type ImportFileOptions, type ImportJob } from "../../api/imports";
import ErrorAlert from "../../components/ErrorAlert.vue";
import { t } from "../../i18n";
import { formatBytes } from "../../lib/format";
import { DELIMITERS, ENCODINGS, defaultSheet, errorPlace } from "../../lib/imports";
import ImportProgress from "./ImportProgress.vue";

/**
 * Step 1 once the file is uploaded: reading it, then what was read. Workbooks offer the sheet, CSV files the
 * encoding and delimiter the server detected; changing any of them, or the header row, reads the file again. The
 * preview shows the first 20 rows as parsed, so wrong umlauts or one-value-per-row are seen at once.
 */
const props = defineProps<{ job: ImportJob }>();
const emit = defineEmits<{ next: [] }>();

const update = useUpdateImportFileOptions();
const file = computed(() => props.job.file);
const reading = computed(() => ["uploading", "analysing"].includes(props.job.status) || (props.job.status === "queued" && props.job.phase === "analyse"));
const analysisFailed = computed(() => props.job.status === "failed" && props.job.phase === "analyse");
/** The server reads the file again only in `ready` or after a failed analysis. */
const editable = computed(() => props.job.status === "ready" || analysisFailed.value);

const sheet = ref<string>();
const encoding = ref<ImportFileOptions["encoding"]>();
const delimiter = ref<string>();
const hasHeaderRow = ref(true);
function resetForm() {
  sheet.value = defaultSheet(file.value);
  encoding.value = file.value.encoding ?? "utf-8";
  delimiter.value = file.value.delimiter ?? ",";
  hasHeaderRow.value = file.value.hasHeaderRow;
}
watch(() => [props.job.status, file.value.sheet, file.value.encoding, file.value.delimiter, file.value.hasHeaderRow], resetForm, { immediate: true });

const changes = computed<ImportFileOptions>(() => {
  const out: ImportFileOptions = {};
  if (file.value.format === "xlsx" && sheet.value && sheet.value !== file.value.sheet) out.sheet = sheet.value;
  if (file.value.format === "csv" && encoding.value !== (file.value.encoding ?? "utf-8")) out.encoding = encoding.value;
  if (file.value.format === "csv" && delimiter.value !== (file.value.delimiter ?? ",")) out.delimiter = delimiter.value;
  if (hasHeaderRow.value !== file.value.hasHeaderRow) out.hasHeaderRow = hasHeaderRow.value;
  return out;
});
const dirty = computed(() => Object.keys(changes.value).length > 0);

async function apply() {
  try {
    await update.mutateAsync({ id: props.job.id, options: changes.value });
  } catch {
    // shown below the options
  }
}

const headers = computed(() => props.job.columns.map((c) => c.header));
const previewWidth = computed(() => Math.max(headers.value.length, ...file.value.previewRows.map((r) => r.cells.length), 0));
const header = (i: number) => headers.value[i] ?? t("imports.file.columnFallback", { n: i + 1 });
</script>

<template>
  <section class="panel" aria-labelledby="step-heading">
    <div class="panel-header"><h2 id="step-heading" tabindex="-1">{{ t("imports.step.upload") }}</h2></div>
    <div class="panel-body">
      <dl class="props import-file-facts">
        <dt>{{ t("imports.col.file") }}</dt>
        <dd>{{ file.name }}</dd>
        <dt>{{ t("imports.file.format") }}</dt>
        <dd>{{ file.format === "xlsx" ? t("imports.file.formatXlsx") : "CSV" }}, {{ formatBytes(file.size) }}</dd>
        <template v-if="file.rowCount != null">
          <dt>{{ t("imports.col.rows") }}</dt>
          <dd>{{ t("imports.file.rowsColumns", { rows: file.rowCount, columns: file.columnCount ?? 0 }) }}</dd>
        </template>
      </dl>

      <p v-if="job.status === 'queued' && job.phase === 'analyse'" role="status">
        {{ t("imports.step.waiting", { n: job.progress.queuePosition ?? 1 }) }}
      </p>
      <ImportProgress v-else-if="reading" :label="t('imports.file.reading')" :done="job.progress.done" :total="null" />
      <p v-if="reading" class="muted">{{ t("imports.file.readingHint") }}</p>

      <div v-if="analysisFailed && job.error" id="import-analysis-error" class="alert alert-error" role="alert">
        <strong>{{ t("imports.file.cannotRead") }}</strong>
        <div>
          {{ errorPlace(job.error) ? t("imports.file.errorAt", { place: errorPlace(job.error), message: job.error.message }) : job.error.message }}
        </div>
        <div class="muted">{{ t(file.format === "csv" ? "imports.file.fixOrReread" : "imports.file.fix") }}</div>
      </div>
      <p v-if="job.status === 'cancelled' && job.phase === 'analyse'" class="alert" role="status">
        {{ t("imports.file.cancelled") }}
      </p>

      <form v-if="editable || file.rowCount != null" class="import-file-options" @submit.prevent="apply">
        <div v-if="file.format === 'xlsx' && file.sheets.length > 0" class="field">
          <label for="import-sheet">{{ t("imports.file.sheet") }}</label>
          <select id="import-sheet" v-model="sheet" :disabled="!editable || update.isPending.value">
            <option v-for="s in file.sheets" :key="s" :value="s">{{ file.hiddenSheets.includes(s) ? t("imports.file.hiddenSheet", { sheet: s }) : s }}</option>
          </select>
        </div>
        <template v-if="file.format === 'csv'">
          <div class="field">
            <label for="import-encoding">{{ t("imports.file.encoding") }}</label>
            <select id="import-encoding" v-model="encoding" :disabled="!editable || update.isPending.value">
              <option v-for="e in ENCODINGS" :key="e.value" :value="e.value">{{ t(e.label) }}</option>
            </select>
          </div>
          <div class="field">
            <label for="import-delimiter">{{ t("imports.file.delimiter") }}</label>
            <select id="import-delimiter" v-model="delimiter" :disabled="!editable || update.isPending.value">
              <option v-for="d in DELIMITERS" :key="d.value" :value="d.value">{{ t(d.label) }}</option>
            </select>
          </div>
        </template>
        <div class="field">
          <label class="checkbox-row">
            <input v-model="hasHeaderRow" type="checkbox" :disabled="!editable || update.isPending.value" />
            {{ t("imports.file.headerRow") }}
          </label>
        </div>
        <div v-if="editable" class="field">
          <button type="submit" class="btn" :disabled="!dirty || update.isPending.value">
            {{ update.isPending.value ? t("imports.file.rereading") : t("imports.file.reread") }}
          </button>
        </div>
      </form>
      <p v-if="editable && dirty && job.mapping" class="muted">{{ t("imports.file.rereadDropsMapping") }}</p>
      <p v-else-if="!editable && !reading && file.rowCount != null" class="muted">
        {{ t("imports.file.optionsLocked") }}
      </p>
      <ErrorAlert v-if="update.isError.value" :error="update.error.value" :title="t('imports.file.optionsFailed')" />
    </div>

    <div v-if="file.previewRows.length > 0" class="table-wrap import-preview">
      <table class="data">
        <caption>{{ t("imports.file.previewCaption", { n: file.previewRows.length }) }}</caption>
        <thead>
          <tr>
            <th scope="col" class="num">{{ t("imports.file.col.row") }}</th>
            <th v-for="i in previewWidth" :key="i" scope="col">{{ header(i - 1) }}</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="r in file.previewRows" :key="r.row">
            <th scope="row" class="num">{{ r.row }}</th>
            <td v-for="i in previewWidth" :key="i" :title="r.cells[i - 1]"><span class="cell-clip">{{ r.cells[i - 1] ?? "" }}</span></td>
          </tr>
        </tbody>
      </table>
    </div>

    <div v-if="job.status === 'ready'" class="form-footer">
      <button type="button" class="btn btn-primary" :disabled="dirty" @click="emit('next')">{{ t("imports.file.next") }}</button>
      <span v-if="dirty" class="muted">{{ t("imports.file.rereadFirst") }}</span>
    </div>
  </section>
</template>
