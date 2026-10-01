<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { ApiError } from "../../api/client";
import {
  importDownloads,
  useCancelImport,
  useCommitImport,
  useImportIssues,
  useStartImportDryRun,
  type ImportIssueQuery,
  type ImportJob,
} from "../../api/imports";
import { useCiClasses, useClassAttributes } from "../../api/queries";
import ConfirmDialog from "../../components/ConfirmDialog.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import PaginationBar from "../../components/PaginationBar.vue";
import { plural } from "../../lib/format";
import { columnLetter, refusalCode } from "../../lib/imports";
import { changeText } from "../../lib/importMapping";
import ImportProgress from "./ImportProgress.vue";

/**
 * Step 3: the dry run. While it runs: progress, elapsed time and Cancel. Then the counts, a sample of the
 * planned changes, and the row problems as a server-paged table whose filters live in the URL, so a reload or a
 * shared link shows the same page.
 */
/** `off`: bulk import is turned off; the server refuses the row problems, the report and every next step. */
const props = defineProps<{ job: ImportJob; off?: boolean }>();
const emit = defineEmits<{ committing: [] }>();

const route = useRoute();
const router = useRouter();
const cancel = useCancelImport();
const again = useStartImportDryRun();
const commit = useCommitImport();

const checking = computed(() => props.job.status === "validating" || (props.job.status === "queued" && props.job.phase === "validate"));
const checked = computed(() => props.job.status === "validated" || props.job.status === "expired");
const summary = computed(() => props.job.summary);
const stale = computed(() => !!props.job.dryRun?.stale || props.job.status === "expired");
/** Rows the planned-changes sample is drawn from: unchanged rows are only counted. */
const rowsChanging = computed(() => (summary.value ? summary.value.create + summary.value.update + summary.value.errorRows : 0));

// Elapsed time, ticking only while the check runs.
const now = ref(Date.now());
let timer: ReturnType<typeof setInterval> | undefined;
watch(
  checking,
  (on) => {
    clearInterval(timer);
    if (on) timer = setInterval(() => (now.value = Date.now()), 1000);
  },
  { immediate: true },
);
onBeforeUnmount(() => clearInterval(timer));
const elapsed = computed(() => {
  const start = Date.parse(props.job.progress.startedAt ?? "");
  if (Number.isNaN(start)) return "";
  const s = Math.max(0, Math.round((now.value - start) / 1000));
  return s < 60 ? `${s} s` : `${Math.floor(s / 60)} min ${s % 60} s`;
});
const finished = computed(() => {
  if (!checked.value || !summary.value) return undefined;
  const n = summary.value.errorRows;
  return n === 0 ? "Check finished: no errors" : `Check finished: ${n.toLocaleString()} ${n === 1 ? "row" : "rows"} with errors`;
});

// ---------- Row problems: filters and page in the URL ----------

const q = (k: string) => (typeof route.query[k] === "string" ? (route.query[k] as string) : "");
const issueQuery = computed<ImportIssueQuery>(() => {
  const out: ImportIssueQuery = { limit: Number(q("issueLimit")) || 25, offset: Number(q("issueOffset")) || 0 };
  const sev = q("issueSeverity");
  if (sev === "error" || sev === "warning") out.severity = sev;
  if (q("issueCode")) out.code = q("issueCode");
  if (q("issueColumn") !== "" && Number.isInteger(Number(q("issueColumn")))) out.column = Number(q("issueColumn"));
  return out;
});
const run = computed(() => (!props.off && checked.value && (summary.value?.issuesTotal ?? 0) > 0 ? props.job.dryRun?.finishedAt : undefined));
const issues = useImportIssues(() => props.job.id, run, issueQuery);

function setQuery(patch: Record<string, string | number | undefined>) {
  const next: Record<string, unknown> = { ...route.query };
  for (const [k, v] of Object.entries(patch)) {
    if (v === undefined || v === "") delete next[k];
    else next[k] = String(v);
  }
  router.replace({ query: next as Record<string, string> });
}
const codeInput = ref(q("issueCode"));
watch(() => q("issueCode"), (c) => (codeInput.value = c));

const hasIssueFilter = computed(() => !!(q("issueSeverity") || q("issueCode") || q("issueColumn")));
const columnName = (i: number | null | undefined, header?: string | null) =>
  i == null ? "–" : `${columnLetter(i)}${header ?? props.job.columns[i]?.header ? ` · ${header ?? props.job.columns[i]?.header}` : ""}`;

// ---------- Import (commit) ----------

/** Rows the import writes or confirms: every row without an error. */
const validRows = computed(() => (summary.value ? summary.value.create + summary.value.update + summary.value.unchanged : 0));
const skipping = ref(false);
const commitError = ref<unknown>();

async function startCommit(skipErrorRows: boolean) {
  commitError.value = undefined;
  try {
    await commit.mutateAsync({ id: props.job.id, skipErrorRows });
    skipping.value = false;
    emit("committing");
  } catch (e) {
    skipping.value = false;
    commitError.value = e;
  }
}
const commitErrorTitle = computed(() => {
  const e = commitError.value;
  if (!(e instanceof ApiError)) return "The import did not start";
  switch (refusalCode(e)) {
    case "dry_run_stale":
      return "The check is out of date";
    case "has_error_rows":
      return "The check found rows with errors";
    case "import_busy":
      return "Another import of yours is running";
    default:
      return "The import did not start";
  }
});

// ---------- Error report ----------

const downloading = ref(false);
const downloadError = ref<unknown>();
async function download() {
  downloading.value = true;
  downloadError.value = undefined;
  try {
    await importDownloads.errorReport(props.job);
  } catch (e) {
    downloadError.value = e;
  } finally {
    downloading.value = false;
  }
}

const OUTCOME = {
  create: { icon: "+", label: "Create" },
  update: { icon: "✎", label: "Update" },
  unchanged: { icon: "=", label: "Unchanged" },
  error: { icon: "!", label: "Error" },
} as const;
// Changed fields are named by key (`attributes.os`); show the class's labels.
const classes = useCiClasses();
const classId = computed(() => classes.data.value?.find((c) => c.key === props.job.classKey)?.id);
const attributes = useClassAttributes(classId);
const attributeLabels = computed(() => new Map((attributes.data.value ?? []).map((a) => [a.key, a.label])));
const fieldLabel = (f: string) => {
  if (f.startsWith("attributes.")) {
    const label = attributeLabels.value.get(f.slice("attributes.".length));
    if (label) return label;
  }
  const key = f.replace(/^attributes\./, "").replace(/^relationships\./, "");
  if (f === "ident") return "Ident";
  if (f === "validFrom") return "Valid from";
  if (f === "validUntil") return "Valid until";
  return key;
};
</script>

<template>
  <section class="panel" aria-labelledby="step-heading">
    <div class="panel-header"><h2 id="step-heading" tabindex="-1">Check</h2></div>
    <div class="panel-body">
      <p v-if="job.status === 'queued' && job.phase === 'validate'" role="status">
        Waiting for {{ job.progress.queuePosition ?? 1 }} other {{ (job.progress.queuePosition ?? 1) === 1 ? "import" : "imports" }} to finish…
      </p>
      <template v-if="checking">
        <ImportProgress
          v-if="job.status === 'validating'"
          :label="`Checking row ${job.progress.done.toLocaleString()} of ${job.progress.total.toLocaleString()}`"
          :done="job.progress.done"
          :total="job.progress.total || null"
        />
        <p v-if="elapsed" class="muted">Elapsed: {{ elapsed }}</p>
        <div class="inline-control">
          <button type="button" class="btn" :disabled="cancel.isPending.value" @click="cancel.mutate(job.id)">Cancel</button>
          <span class="muted">You can leave this page. The check continues and you can come back from Imports.</span>
        </div>
        <ErrorAlert v-if="cancel.isError.value" :error="cancel.error.value" title="Not cancelled" />
      </template>
      <!-- Only the end is announced; ImportProgress speaks the 10 % steps while it is shown. -->
      <div class="sr-only" aria-live="polite">{{ finished }}</div>

      <template v-if="summary && !checking">
        <div v-if="stale && job.status !== 'expired'" class="alert alert-warn" role="status">
          <strong>This check is out of date.</strong>
          {{
            job.dryRun?.staleReason === "model_changed"
              ? "The data model changed after the file was checked."
              : "The check is more than 24 hours old."
          }}
          Check the file again before importing.
        </div>

        <ul class="import-counts" aria-label="Check result">
          <li><span aria-hidden="true">+</span> <strong>Create</strong> {{ summary.create.toLocaleString() }}</li>
          <li><span aria-hidden="true">✎</span> <strong>Update</strong> {{ summary.update.toLocaleString() }}</li>
          <li><span aria-hidden="true">=</span> <strong>Unchanged</strong> {{ summary.unchanged.toLocaleString() }}</li>
          <li :class="{ 'import-count-error': summary.errorRows > 0 }">
            <span aria-hidden="true">!</span> <strong>Errors</strong> {{ summary.errorRows.toLocaleString() }}
            {{ summary.errorRows === 1 ? "row" : "rows" }}
          </li>
          <li><span aria-hidden="true">↔</span> <strong>Relationships to add</strong> {{ summary.relationshipsToAdd.toLocaleString() }}</li>
          <li><span aria-hidden="true">⚠</span> <strong>Warnings</strong> {{ summary.warnings.toLocaleString() }}</li>
        </ul>
        <p v-if="summary.issuesTotal > 10000" class="muted">
          Showing the first 10,000 of {{ summary.issuesTotal.toLocaleString() }} problems. Fix the mapping or the file.
        </p>
        <div v-if="summary.issuesTotal > 0 && job.status !== 'expired' && !off" class="inline-control">
          <button type="button" class="btn" :disabled="downloading" @click="download">
            {{ downloading ? "Preparing the report…" : "Download error report" }}
          </button>
          <span class="muted">A CSV file with each problem and the row's original columns, to fix and upload again.</span>
        </div>
        <ErrorAlert v-if="downloadError" :error="downloadError" title="The error report was not downloaded" />
      </template>
    </div>

    <template v-if="summary && !checking">
      <div v-if="job.preview.length" class="table-wrap import-preview">
        <table class="data">
          <caption>
            Planned changes: {{ job.preview.length < rowsChanging ? `showing the first ${job.preview.length} of` : "all" }}
            {{ rowsChanging.toLocaleString() }} {{ rowsChanging === 1 ? "row" : "rows" }} that create, update or fail
          </caption>
          <thead>
            <tr>
              <th scope="col" class="num">Row</th>
              <th scope="col">Outcome</th>
              <th scope="col">CI</th>
              <th scope="col">Changes</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="r in job.preview" :key="r.row">
              <th scope="row" class="num">{{ r.row }}</th>
              <td><span aria-hidden="true">{{ OUTCOME[r.outcome].icon }} </span>{{ OUTCOME[r.outcome].label }}</td>
              <td>
                <RouterLink v-if="r.ciId" :to="`/cis/${r.ciId}`">{{ r.ciLabel ?? r.ciId }}</RouterLink>
                <template v-else>{{ r.ciLabel ?? "–" }}</template>
              </td>
              <td class="wrap">
                <template v-if="r.changes.length">
                  <div v-for="c in r.changes" :key="c.field">
                    <strong>{{ fieldLabel(c.field) }}:</strong> {{ r.outcome === "update" ? `${changeText(c.old)} → ` : "" }}{{ changeText(c.new) }}
                  </div>
                </template>
                <span v-else class="muted">–</span>
              </td>
            </tr>
          </tbody>
        </table>
      </div>

      <div v-if="summary.issuesTotal > 0 && off" class="panel-body">
        <h3>Row problems</h3>
        <p class="muted">The row problems and the error report can't be opened while bulk import is off.</p>
      </div>
      <div v-else-if="summary.issuesTotal > 0" class="panel-body">
        <h3 id="import-issues-title">Row problems</h3>
        <form class="import-file-options" @submit.prevent="setQuery({ issueCode: codeInput.trim(), issueOffset: undefined })">
          <div class="field">
            <label for="import-issue-severity">Severity</label>
            <select
              id="import-issue-severity"
              :value="q('issueSeverity')"
              @change="setQuery({ issueSeverity: ($event.target as HTMLSelectElement).value, issueOffset: undefined })"
            >
              <option value="">Errors and warnings</option>
              <option value="error">Errors</option>
              <option value="warning">Warnings</option>
            </select>
          </div>
          <div class="field">
            <label for="import-issue-column">Column</label>
            <select
              id="import-issue-column"
              :value="q('issueColumn')"
              @change="setQuery({ issueColumn: ($event.target as HTMLSelectElement).value, issueOffset: undefined })"
            >
              <option value="">All columns</option>
              <option v-for="c in job.columns" :key="c.index" :value="String(c.index)">{{ columnName(c.index, c.header) }}</option>
            </select>
          </div>
          <div class="field">
            <label for="import-issue-code">Problem code</label>
            <input id="import-issue-code" v-model="codeInput" placeholder="e.g. not_found" />
          </div>
          <div class="field">
            <button type="submit" class="btn">Filter</button>
          </div>
          <div v-if="hasIssueFilter" class="field">
            <button
              type="button"
              class="btn btn-link"
              @click="setQuery({ issueSeverity: undefined, issueCode: undefined, issueColumn: undefined, issueOffset: undefined })"
            >
              Clear filters
            </button>
          </div>
        </form>
      </div>
      <ErrorAlert v-if="!off && issues.isError.value" :error="issues.error.value" :on-retry="() => issues.refetch()" />
      <div v-else-if="summary.issuesTotal > 0 && !off" class="table-wrap import-preview">
        <table class="data" aria-labelledby="import-issues-title">
          <caption>
            <template v-if="issues.isPending.value">Loading the row problems…</template>
            <template v-else>Row numbers count like the spreadsheet: row 1 is the header.</template>
          </caption>
          <thead>
            <tr>
              <th scope="col" class="num">Row</th>
              <th scope="col">Column</th>
              <th scope="col">Value</th>
              <th scope="col">Problem</th>
              <th scope="col">Code</th>
            </tr>
          </thead>
          <tbody>
            <tr v-if="issues.data.value && issues.data.value.data.length === 0">
              <td colspan="5" class="muted">No problems match these filters.</td>
            </tr>
            <tr v-for="(p, n) in issues.data.value?.data ?? []" :key="`${p.row}-${p.column}-${p.code}-${n}`">
              <th scope="row" class="num">{{ p.row.toLocaleString() }}</th>
              <td>{{ columnName(p.column, p.header) }}</td>
              <td class="cell-clip" :title="p.value ?? undefined">{{ p.value ?? "" }}</td>
              <td class="wrap">
                <span aria-hidden="true">{{ p.severity === "error" ? "✕ " : "⚠ " }}</span>
                <span class="sr-only">{{ p.severity === "error" ? "Error: " : "Warning: " }}</span>{{ p.message }}
              </td>
              <td><code>{{ p.code }}</code></td>
            </tr>
          </tbody>
        </table>
        <PaginationBar
          v-if="issues.data.value"
          :total="issues.data.value.page.total"
          :limit="issues.data.value.page.limit"
          :offset="issues.data.value.page.offset"
          @change="(p) => setQuery({ issueLimit: p.limit === 25 ? undefined : p.limit, issueOffset: p.offset || undefined })"
        />
      </div>

      <div v-if="job.status === 'validated' && !off" class="form-footer">
        <template v-if="stale">
          <button type="button" class="btn btn-primary" :disabled="again.isPending.value" @click="again.mutate(job.id)">
            {{ again.isPending.value ? "Starting the check…" : "Check again" }}
          </button>
        </template>
        <template v-else-if="summary.errorRows === 0">
          <button type="button" class="btn btn-primary" :disabled="commit.isPending.value || validRows === 0" @click="startCommit(false)">
            {{ commit.isPending.value ? "Starting the import…" : `Import ${validRows.toLocaleString()} ${validRows === 1 ? "row" : "rows"}` }}
          </button>
          <RouterLink class="btn" :to="{ path: `/imports/${job.id}`, query: { step: '2' } }">Back to mapping</RouterLink>
        </template>
        <template v-else>
          <RouterLink class="btn btn-primary" :to="{ path: `/imports/${job.id}`, query: { step: '2' } }">Back to mapping</RouterLink>
          <RouterLink
            class="btn"
            :to="{ path: '/imports/new', query: { ...(job.classKey ? { classKey: job.classKey } : {}), fromJob: job.id } }"
          >
            Upload a corrected file
          </RouterLink>
          <button v-if="validRows > 0" type="button" class="btn" :disabled="commit.isPending.value" @click="skipping = true">
            Import {{ validRows.toLocaleString() }} valid {{ validRows === 1 ? "row" : "rows" }} and skip
            {{ summary.errorRows.toLocaleString() }}…
          </button>
        </template>
        <span v-if="!stale" class="muted">Going back to the mapping discards this check.</span>
      </div>
      <ErrorAlert v-if="again.isError.value" :error="again.error.value" title="The check did not start" />
      <ErrorAlert v-if="commitError" :error="commitError" :title="commitErrorTitle" />
    </template>
  </section>

  <ConfirmDialog
    :open="skipping"
    :title="`Skip ${plural(summary?.errorRows ?? 0, 'row')} with errors?`"
    :confirm-label="`Import ${plural(validRows, 'row')}`"
    :busy="commit.isPending.value"
    @cancel="skipping = false"
    @confirm="startCommit(true)"
  >
    <p>
      {{ summary?.errorRows.toLocaleString() }} {{ summary?.errorRows === 1 ? "row has errors and" : "rows have errors and" }}
      will not be imported. They are listed in the error report. The other {{ validRows.toLocaleString() }}
      {{ validRows === 1 ? "row" : "rows" }} will be imported. Continue?
    </p>
  </ConfirmDialog>
</template>
