<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { ApiError } from "../../api/client";
import {
  importDownloads,
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
import { formatNumber, t, type MessageKey } from "../../i18n";
import { columnLetter, refusalCode } from "../../lib/imports";
import ChangeValue from "./ChangeValue.vue";
import ImportProgress from "./ImportProgress.vue";
import Icon from "../../components/Icon.vue";
import type { IconName } from "../../icons/lucide";

/**
 * Step 3: the dry run. While it runs: progress and elapsed time (stopping it is the page head's Stop import). Then the counts, a sample of the
 * planned changes, and the row problems as a server-paged table whose filters live in the URL, so a reload or a
 * shared link shows the same page.
 */
const props = defineProps<{
  job: ImportJob;
  /** Import is off: the result and the row problems stay readable (GETs), every step action is hidden. */
  readOnly?: boolean;
}>();
const emit = defineEmits<{ committing: [] }>();

const route = useRoute();
const router = useRouter();
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
  return s < 60 ? t("imports.check.seconds", { s }) : t("imports.check.minutes", { m: Math.floor(s / 60), s: s % 60 });
});
const finished = computed(() => {
  if (!checked.value || !summary.value) return undefined;
  const n = summary.value.errorRows;
  return n === 0 ? t("imports.check.finishedNoErrors") : t("imports.check.finishedErrors", { n });
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
const run = computed(() => (checked.value && (summary.value?.issuesTotal ?? 0) > 0 ? props.job.dryRun?.finishedAt : undefined));
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
  if (!(e instanceof ApiError)) return t("imports.check.commitFailed");
  switch (refusalCode(e)) {
    case "dry_run_stale":
      return t("imports.check.commitStale");
    case "has_error_rows":
      return t("imports.check.commitErrorRows");
    case "import_busy":
      return t("imports.check.commitBusy");
    default:
      return t("imports.check.commitFailed");
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
  create: { icon: "plus", label: "imports.check.outcome.create" },
  update: { icon: "pencil", label: "imports.check.outcome.update" },
  unchanged: { icon: "equal", label: "imports.check.outcome.unchanged" },
  error: { icon: "circle-alert", label: "common.error" },
} as const satisfies Record<string, { icon: IconName; label: MessageKey }>;
// Changed fields are named by key (`attributes.os`); show the class's labels.
const classes = useCiClasses();
const classId = computed(() => classes.data.value?.find((c) => c.key === props.job.classKey)?.id);
const attributes = useClassAttributes(classId);
const attributeDefs = computed(() => new Map((attributes.data.value ?? []).map((a) => [a.key, a])));
const attributeDef = (f: string) => (f.startsWith("attributes.") ? attributeDefs.value.get(f.slice("attributes.".length)) : undefined);
const fieldLabel = (f: string) => {
  const label = attributeDef(f)?.label;
  if (label) return label;
  const key = f.replace(/^attributes\./, "").replace(/^relationships\./, "");
  if (f === "ident") return t("ciField.ident");
  if (f === "validFrom") return t("ciField.validFrom");
  if (f === "validUntil") return t("ciField.validUntil");
  return key;
};

const plannedCaption = computed(() =>
  props.job.preview.length < rowsChanging.value
    ? t("imports.check.plannedSome", { shown: formatNumber(props.job.preview.length), n: rowsChanging.value })
    : t("imports.check.plannedAll", { n: rowsChanging.value }),
);
</script>

<template>
  <section class="panel" aria-labelledby="step-heading">
    <div class="panel-header"><h2 id="step-heading" tabindex="-1">{{ t("imports.step.check") }}</h2></div>
    <div class="panel-body">
      <p v-if="job.status === 'queued' && job.phase === 'validate'" role="status">
        {{ t("imports.step.waiting", { n: job.progress.queuePosition ?? 1 }) }}
      </p>
      <template v-if="checking">
        <ImportProgress
          v-if="job.status === 'validating'"
          :label="t('imports.check.progress', { done: formatNumber(job.progress.done), total: formatNumber(job.progress.total) })"
          :done="job.progress.done"
          :total="job.progress.total || null"
        />
        <p v-if="elapsed" class="muted">{{ t("imports.check.elapsed", { time: elapsed }) }}</p>
        <p v-if="!readOnly" class="muted">{{ t("imports.check.checkingHint") }}</p>
      </template>
      <!-- Only the end is announced; ImportProgress speaks the 10 % steps while it is shown. -->
      <div class="sr-only" aria-live="polite">{{ finished }}</div>

      <template v-if="summary && !checking">
        <div v-if="stale && job.status !== 'expired' && !readOnly" class="alert alert-warn" role="status">
          <strong>{{ t("imports.check.staleTitle") }}</strong>
          {{ t(job.dryRun?.staleReason === "model_changed" ? "imports.check.staleModel" : "imports.check.staleAge") }}
          {{ t("imports.check.staleAction") }}
        </div>

        <ul class="import-counts" :aria-label="t('imports.check.result')">
          <li><Icon name="plus" /> <strong>{{ t("imports.check.outcome.create") }}</strong> {{ formatNumber(summary.create) }}</li>
          <li><Icon name="pencil" /> <strong>{{ t("imports.check.outcome.update") }}</strong> {{ formatNumber(summary.update) }}</li>
          <li><Icon name="equal" /> <strong>{{ t("imports.check.outcome.unchanged") }}</strong> {{ formatNumber(summary.unchanged) }}</li>
          <li :class="{ 'import-count-error': summary.errorRows > 0 }">
            <Icon name="circle-alert" /> <strong>{{ t("imports.check.errors") }}</strong> {{ t("imports.check.rows", { n: summary.errorRows }) }}
          </li>
          <li><Icon name="arrow-left-right" /> <strong>{{ t("imports.check.relationshipsToAdd") }}</strong> {{ formatNumber(summary.relationshipsToAdd) }}</li>
          <li><Icon name="triangle-alert" /> <strong>{{ t("imports.check.warnings") }}</strong> {{ formatNumber(summary.warnings) }}</li>
        </ul>
        <p v-if="summary.issuesTotal > 10000" class="muted">
          {{ t("imports.check.issuesTruncated", { limit: formatNumber(10000), n: formatNumber(summary.issuesTotal) }) }}
        </p>
        <div v-if="summary.issuesTotal > 0 && job.status !== 'expired'" class="inline-control">
          <button type="button" class="btn" :disabled="downloading" @click="download">
            {{ downloading ? t("imports.check.reportPreparing") : t("imports.check.reportDownload") }}
          </button>
          <span class="muted">{{ t("imports.check.reportHint") }}</span>
        </div>
        <ErrorAlert v-if="downloadError" :error="downloadError" :title="t('imports.check.reportFailed')" />
      </template>
    </div>

    <template v-if="summary && !checking">
      <div v-if="job.preview.length" class="table-wrap import-preview">
        <table class="data">
          <caption>{{ plannedCaption }}</caption>
          <thead>
            <tr>
              <th scope="col" class="num">{{ t("imports.check.col.row") }}</th>
              <th scope="col">{{ t("imports.check.col.outcome") }}</th>
              <th scope="col">{{ t("imports.check.col.ci") }}</th>
              <th scope="col">{{ t("imports.check.col.changes") }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="r in job.preview" :key="r.row">
              <th scope="row" class="num">{{ r.row }}</th>
              <td><Icon :name="OUTCOME[r.outcome].icon" /> {{ t(OUTCOME[r.outcome].label) }}</td>
              <td>
                <RouterLink v-if="r.ciId" :to="`/cis/${r.ciId}`">{{ r.ciLabel ?? r.ciId }}</RouterLink>
                <template v-else>{{ r.ciLabel ?? "–" }}</template>
              </td>
              <td class="wrap">
                <template v-if="r.changes.length">
                  <div v-for="c in r.changes" :key="c.field">
                    <strong>{{ fieldLabel(c.field) }}:</strong>{{ " " }}
                    <template v-if="r.outcome === 'update'"><ChangeValue :def="attributeDef(c.field)" :value="c.old" />{{ " → " }}</template>
                    <ChangeValue :def="attributeDef(c.field)" :value="c.new" />
                  </div>
                </template>
                <span v-else class="muted">–</span>
              </td>
            </tr>
          </tbody>
        </table>
      </div>

      <div v-if="summary.issuesTotal > 0" class="panel-body">
        <h3 id="import-issues-title">{{ t("imports.check.problems") }}</h3>
        <form class="import-file-options" @submit.prevent="setQuery({ issueCode: codeInput.trim(), issueOffset: undefined })">
          <div class="field">
            <label for="import-issue-severity">{{ t("imports.check.severity") }}</label>
            <select
              id="import-issue-severity"
              :value="q('issueSeverity')"
              @change="setQuery({ issueSeverity: ($event.target as HTMLSelectElement).value, issueOffset: undefined })"
            >
              <option value="">{{ t("imports.check.severityAll") }}</option>
              <option value="error">{{ t("imports.check.errors") }}</option>
              <option value="warning">{{ t("imports.check.warnings") }}</option>
            </select>
          </div>
          <div class="field">
            <label for="import-issue-column">{{ t("imports.check.col.column") }}</label>
            <select
              id="import-issue-column"
              :value="q('issueColumn')"
              @change="setQuery({ issueColumn: ($event.target as HTMLSelectElement).value, issueOffset: undefined })"
            >
              <option value="">{{ t("imports.check.allColumns") }}</option>
              <option v-for="c in job.columns" :key="c.index" :value="String(c.index)">{{ columnName(c.index, c.header) }}</option>
            </select>
          </div>
          <div class="field">
            <label for="import-issue-code">{{ t("imports.check.code") }}</label>
            <input id="import-issue-code" v-model="codeInput" :placeholder="t('imports.check.codePlaceholder')" />
          </div>
          <div class="field">
            <button type="submit" class="btn">{{ t("imports.check.filter") }}</button>
          </div>
          <div v-if="hasIssueFilter" class="field">
            <button
              type="button"
              class="btn btn-link"
              @click="setQuery({ issueSeverity: undefined, issueCode: undefined, issueColumn: undefined, issueOffset: undefined })"
            >
              {{ t("imports.check.clearFilters") }}
            </button>
          </div>
        </form>
      </div>
      <ErrorAlert v-if="issues.isError.value" :error="issues.error.value" :on-retry="() => issues.refetch()" />
      <div v-else-if="summary.issuesTotal > 0" class="table-wrap import-preview">
        <table class="data" aria-labelledby="import-issues-title">
          <caption>
            {{ issues.isPending.value ? t("imports.check.problemsLoading") : t("imports.check.rowNumbers") }}
          </caption>
          <thead>
            <tr>
              <th scope="col" class="num">{{ t("imports.check.col.row") }}</th>
              <th scope="col">{{ t("imports.check.col.column") }}</th>
              <th scope="col">{{ t("imports.check.col.value") }}</th>
              <th scope="col">{{ t("imports.check.col.problem") }}</th>
              <th scope="col">{{ t("imports.check.col.code") }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-if="issues.data.value && issues.data.value.data.length === 0">
              <td colspan="5" class="muted">{{ t("imports.check.noMatch") }}</td>
            </tr>
            <tr v-for="(p, n) in issues.data.value?.data ?? []" :key="`${p.row}-${p.column}-${p.code}-${n}`">
              <th scope="row" class="num">{{ formatNumber(p.row) }}</th>
              <td>{{ columnName(p.column, p.header) }}</td>
              <td :title="p.value ?? undefined"><span class="cell-clip">{{ p.value ?? "" }}</span></td>
              <td class="wrap">
                <Icon :name="p.severity === 'error' ? 'circle-x' : 'triangle-alert'" /> 
                <span class="sr-only">{{ t(p.severity === "error" ? "imports.check.srError" : "imports.check.srWarning") }}{{ " " }}</span>{{ p.message }}
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

      <div v-if="job.status === 'validated' && !readOnly" class="form-footer">
        <template v-if="stale">
          <button type="button" class="btn btn-primary" :disabled="again.isPending.value" @click="again.mutate(job.id)">
            {{ again.isPending.value ? t("imports.check.starting") : t("imports.check.again") }}
          </button>
        </template>
        <template v-else-if="summary.errorRows === 0">
          <button type="button" class="btn btn-primary" :disabled="commit.isPending.value || validRows === 0" @click="startCommit(false)">
            {{ commit.isPending.value ? t("imports.check.commitStarting") : t("imports.check.importRows", { n: validRows }) }}
          </button>
          <RouterLink class="btn" :to="{ path: `/imports/${job.id}`, query: { step: '2' } }">{{ t("imports.check.backToMapping") }}</RouterLink>
        </template>
        <template v-else>
          <RouterLink class="btn btn-primary" :to="{ path: `/imports/${job.id}`, query: { step: '2' } }">{{ t("imports.check.backToMapping") }}</RouterLink>
          <RouterLink
            class="btn"
            :to="{ path: '/imports/new', query: { ...(job.classKey ? { classKey: job.classKey } : {}), fromJob: job.id } }"
          >
            {{ t("imports.check.uploadCorrected") }}
          </RouterLink>
          <button v-if="validRows > 0" type="button" class="btn" :disabled="commit.isPending.value" @click="skipping = true">
            {{ t("imports.check.importValidAndSkip", { n: validRows, errors: formatNumber(summary.errorRows) }) }}
          </button>
        </template>
        <span v-if="!stale" class="muted">{{ t("imports.check.backDiscards") }}</span>
      </div>
      <ErrorAlert v-if="again.isError.value" :error="again.error.value" :title="t('imports.check.againFailed')" />
      <ErrorAlert v-if="commitError" :error="commitError" :title="commitErrorTitle" />
    </template>
  </section>

  <ConfirmDialog
    :open="skipping"
    :title="t('imports.check.skipDialog.title', { n: summary?.errorRows ?? 0 })"
    :confirm-label="t('imports.check.importRows', { n: validRows })"
    :busy="commit.isPending.value"
    @cancel="skipping = false"
    @confirm="startCommit(true)"
  >
    <p>{{ t("imports.check.skipDialog.body", { errors: summary?.errorRows ?? 0, valid: validRows }) }}</p>
  </ConfirmDialog>
</template>
