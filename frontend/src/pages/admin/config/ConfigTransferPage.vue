<script setup lang="ts">
import { adminCrumbs } from "../sections";
import { computed, ref } from "vue";
import { ApiError } from "../../../api/client";
import { configApi, useImportConfig, type ImportResult } from "../../../api/uiSettings";
import Breadcrumbs from "../../../components/Breadcrumbs.vue";
import ConfirmDialog from "../../../components/ConfirmDialog.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import Icon from "../../../components/Icon.vue";
import SaveBar from "../../../components/SaveBar.vue";
import SchemaChangeList from "../../../components/SchemaChangeList.vue";
import { hasMessage, t, type MessageKey } from "../../../i18n";
import { useDocumentTitle } from "../../../lib/composables";
import { useBrandingStore } from "../../../stores/branding";
import { useSessionStore } from "../../../stores/session";

/**
 * Administration › Export / import: the whole configuration (data model,
 * lookup lists, permission profiles, UI settings) as one JSON file. Importing is
 * always a dry run first: the API runs every change and rolls back, and this
 * page shows exactly what applying would do before the operator applies it.
 * The page has the CI page's head band without tabs (the chosen file and its
 * state as chips), and the dry run's Apply / Cancel sit on the shared save bar
 * (design §2.7, audit N4).
 */
useDocumentTitle(() => t("admin.section.config"));
const session = useSessionStore();
const branding = useBrandingStore();
const MAX_BYTES = 16 * 1024 * 1024;

// ---------- Export ----------
const exporting = ref(false);
const exportError = ref<unknown>(null);
async function download() {
  exporting.value = true;
  exportError.value = null;
  try {
    const file = await configApi.export();
    const blob = new Blob([JSON.stringify(file, null, 2)], { type: "application/json" });
    const a = document.createElement("a");
    a.href = URL.createObjectURL(blob);
    a.download = `shadoucmdb-config-${new Date().toISOString().slice(0, 10)}.json`;
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(a.href), 1000);
  } catch (e) {
    exportError.value = e;
  } finally {
    exporting.value = false;
  }
}

// ---------- Import ----------
const importer = useImportConfig();
const fileName = ref("");
const parsed = ref<unknown>(null);
const readError = ref("");
const dryRun = ref<ImportResult | null>(null);
const applied = ref<ImportResult | null>(null);
const error = ref<unknown>(null);
const confirming = ref(false);

async function onFile(e: Event) {
  const input = e.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = "";
  reset();
  if (!file) return;
  fileName.value = file.name;
  if (file.size > MAX_BYTES) {
    readError.value = t("configTransfer.import.tooLarge", { file: file.name, size: (file.size / 1024 / 1024).toFixed(1) });
    return;
  }
  try {
    parsed.value = JSON.parse(await file.text());
  } catch {
    readError.value = t("configTransfer.import.notJson", { file: file.name });
    return;
  }
  await run("dry_run");
}

function reset() {
  fileName.value = "";
  parsed.value = null;
  readError.value = "";
  dryRun.value = null;
  applied.value = null;
  error.value = null;
}

async function run(mode: "dry_run" | "apply") {
  error.value = null;
  try {
    const result = await importer.mutateAsync({ file: parsed.value, mode });
    if (mode === "dry_run") dryRun.value = result;
    else {
      applied.value = result;
      dryRun.value = null;
      // Profiles, branding and settings may have changed under this session.
      await Promise.all([session.refresh(), branding.load()]);
    }
  } catch (e) {
    error.value = e;
  } finally {
    confirming.value = false;
  }
}

const validation = computed(() => (error.value instanceof ApiError && error.value.code === "VALIDATION_ERROR" ? error.value : null));
const totals = (r: ImportResult) => r.summary.reduce((t, s) => ({ created: t.created + s.created, updated: t.updated + s.updated, deleted: t.deleted + s.deleted }), { created: 0, updated: 0, deleted: 0 });
const pending = computed(() => (dryRun.value ? totals(dryRun.value) : null));
const noChanges = computed(() => !!dryRun.value && dryRun.value.changes.length === 0);
const bySection = (r: ImportResult) => {
  const out = new Map<string, ImportResult["changes"]>();
  for (const c of r.changes) out.set(c.section, [...(out.get(c.section) ?? []), c]);
  return [...out.entries()];
};
function show(v: unknown): string {
  if (v === null || v === undefined) return "—";
  const s = typeof v === "string" ? v : JSON.stringify(v);
  return s.length > 120 ? `${s.slice(0, 117)}…` : s;
}
/** The translated name of an export section; a section this version does not know shows as sent. */
function sectionName(s: string): string {
  const key = `configTransfer.section.${s}`;
  return hasMessage(key) ? t(key) : s;
}
function actionLabel(action: string): string {
  const key = `configTransfer.action.${action}`;
  return hasMessage(key) ? t(key) : action;
}
const actionTone = (action: string) => (action === "create" ? "ok" : action === "delete" ? "danger" : "warn");

/** The state chip in the head band: what happened to the chosen file. */
const state = computed<{ tone: string; key: MessageKey } | null>(() => {
  if (applied.value) return { tone: "ok", key: "configTransfer.state.imported" };
  if (validation.value || error.value || readError.value) return { tone: "danger", key: "configTransfer.state.refused" };
  if (noChanges.value) return { tone: "off", key: "configTransfer.state.noChanges" };
  if (dryRun.value) return { tone: "warn", key: "configTransfer.state.dryRun" };
  return null;
});
</script>

<template>
  <div class="record-head record-head-plain">
    <Breadcrumbs :items="adminCrumbs('config')" />
    <div class="page-header record-header">
      <div class="record-heading">
        <span class="class-tile class-tile-lg" aria-hidden="true"><Icon name="arrow-left-right" class="class-icon" /></span>
        <div class="record-title">
          <div class="title">
            <h1>{{ t("admin.section.config") }}</h1>
          </div>
          <p class="record-meta" data-testid="record-meta">
            <span v-if="fileName" class="badge mono" dir="auto">{{ fileName }}</span>
            <span v-if="state" :class="['badge', state.tone]"><span class="status-dot" aria-hidden="true" />{{ t(state.key) }}</span>
            <span class="record-meta-line">{{ t("configTransfer.meta") }}</span>
          </p>
        </div>
      </div>
    </div>
  </div>

  <div class="stack">
    <section class="panel" aria-labelledby="config-export-title">
      <div class="panel-header"><h2 id="config-export-title">{{ t("configTransfer.export.title") }}</h2></div>
      <div class="panel-body stack">
        <p>{{ t("configTransfer.export.body") }}</p>
        <div>
          <button type="button" class="btn btn-primary" :disabled="exporting" @click="download">
            <Icon name="arrow-down-to-line" />{{ exporting ? t("configTransfer.export.preparing") : t("configTransfer.export.download") }}
          </button>
        </div>
        <ErrorAlert v-if="exportError" :error="exportError" :title="t('configTransfer.export.failed')" />
      </div>
    </section>

    <section class="panel" aria-labelledby="config-import-title">
      <div class="panel-header"><h2 id="config-import-title">{{ t("configTransfer.import.title") }}</h2></div>
      <div class="panel-body stack">
        <p>{{ t("configTransfer.import.body") }}</p>
        <div class="inline-control">
          <label class="btn" for="config-file"
            ><Icon name="upload" />{{ importer.isPending.value && !dryRun ? t("configTransfer.import.checking") : t("configTransfer.import.choose") }}</label
          >
          <input id="config-file" class="sr-only" type="file" accept=".json,application/json" :disabled="importer.isPending.value" @change="onFile" />
          <span v-if="fileName" class="muted mono" dir="auto">{{ fileName }}</span>
        </div>
        <div v-if="readError" class="alert alert-error" role="alert">{{ readError }}</div>

        <div v-if="validation" class="alert alert-error" role="alert">
          <strong>{{ t("configTransfer.import.invalid", { n: validation.details.length }) }}</strong>
          <div>{{ validation.message }} {{ t("configTransfer.import.nothingChanged") }}</div>
          <ul>
            <li v-for="(d, i) in validation.details" :key="i"><code v-if="d.field">{{ d.field }}</code> {{ d.message }}</li>
          </ul>
        </div>
        <ErrorAlert v-else-if="error" :error="error" :title="dryRun ? t('configTransfer.import.applyFailed') : t('configTransfer.import.dryRunFailed')" />

        <div v-if="applied" class="alert alert-success" role="status">
          <strong>{{ t("configTransfer.applied.title", { file: fileName }) }}</strong>
          {{ t("configTransfer.applied.body", { created: totals(applied).created, updated: totals(applied).updated }) }}
        </div>
        <div v-if="applied && (applied.warnings.length > 0 || applied.uiSettingsIssues.length > 0)" class="alert alert-warn" role="status">
          <strong>{{ t("configTransfer.applied.warnings") }}</strong>
          <ul>
            <li v-for="(w, i) in applied.warnings" :key="`w${i}`"><code>{{ w.path }}</code> {{ w.message }}</li>
            <li v-for="(w, i) in applied.uiSettingsIssues" :key="`u${i}`"><code>uiSettings.settings.{{ w.path }}</code> {{ w.message }}</li>
          </ul>
        </div>
      </div>
    </section>

    <section v-if="dryRun" class="panel" aria-labelledby="config-dry-run-title">
      <div class="panel-header">
        <h2 id="config-dry-run-title">{{ t("configTransfer.dry.title", { file: fileName }) }}</h2>
      </div>
      <div class="panel-body">
        <p v-if="noChanges" class="alert" role="status">{{ t("configTransfer.dry.noChanges") }}</p>
        <p v-else-if="pending!.deleted">{{ t("configTransfer.dry.summaryRemove", { created: pending!.created, updated: pending!.updated, deleted: pending!.deleted }) }}</p>
        <p v-else>{{ t("configTransfer.dry.summary", { created: pending!.created, updated: pending!.updated }) }}</p>
      </div>
      <div class="panel-body flush table-wrap">
        <table class="data" :aria-label="t('configTransfer.dry.tableLabel')">
          <thead>
            <tr>
              <th scope="col">{{ t("configTransfer.col.section") }}</th>
              <th scope="col" class="num">{{ t("configTransfer.col.create") }}</th>
              <th scope="col" class="num">{{ t("configTransfer.col.update") }}</th>
              <th scope="col" class="num">{{ t("configTransfer.col.remove") }}</th>
              <th scope="col" class="num">{{ t("configTransfer.col.unchanged") }}</th>
              <th scope="col" class="num">{{ t("configTransfer.col.notInFile") }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="s in dryRun.summary" :key="s.section">
              <td>{{ sectionName(s.section) }}</td>
              <td class="num">{{ s.created || "" }}</td>
              <td class="num">{{ s.updated || "" }}</td>
              <td class="num">{{ s.deleted || "" }}</td>
              <td class="num muted">{{ s.unchanged || "" }}</td>
              <td class="num muted">{{ s.notInFile || "" }}</td>
            </tr>
          </tbody>
        </table>
      </div>
      <div v-if="dryRun.warnings.length > 0 || dryRun.uiSettingsIssues.length > 0" class="panel-body">
        <div class="alert alert-warn" role="status">
          <strong>{{ t("configTransfer.dry.warnings") }}</strong>
          <ul>
            <li v-for="(w, i) in dryRun.warnings" :key="`w${i}`"><code>{{ w.path }}</code> {{ w.message }}</li>
            <li v-for="(w, i) in dryRun.uiSettingsIssues" :key="`u${i}`"><code>uiSettings.settings.{{ w.path }}</code> {{ w.message }}</li>
          </ul>
        </div>
      </div>
    </section>

    <section v-if="dryRun && dryRun.schemaChanges.length > 0" class="panel" aria-labelledby="config-ddl-title">
      <div class="panel-header"><h2 id="config-ddl-title">{{ t("configTransfer.ddl.title") }}</h2></div>
      <div class="panel-body stack">
        <p class="muted">{{ t("configTransfer.ddl.hint") }}</p>
        <SchemaChangeList :changes="dryRun.schemaChanges" />
      </div>
    </section>

    <section v-if="dryRun && dryRun.changes.length > 0" class="panel" aria-labelledby="config-changes-title">
      <div class="panel-header"><h2 id="config-changes-title">{{ t("configTransfer.changes.title") }}</h2></div>
      <div class="panel-body">
        <details v-for="[sec, changes] in bySection(dryRun)" :key="sec" class="import-changes" open>
          <summary>{{ sectionName(sec) }} ({{ changes.length }})</summary>
          <div class="table-wrap">
            <table class="data">
              <tbody>
                <tr v-for="(c, i) in changes" :key="i">
                  <td class="config-change-action">
                    <span :class="['badge', actionTone(c.action)]">{{ actionLabel(c.action) }}</span>
                  </td>
                  <td class="mono config-change-key">{{ c.key }}</td>
                  <td class="config-change-fields">
                    <div v-for="f in c.fields" :key="f.field">
                      <code>{{ f.field }}</code>: <span class="diff-from">{{ show(f.from) }}</span> → <span class="diff-to">{{ show(f.to) }}</span>
                    </div>
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
        </details>
      </div>
    </section>
  </div>

  <SaveBar v-if="dryRun" :label="t('configTransfer.applyRegion')">
    <button type="button" class="btn" :disabled="importer.isPending.value" @click="reset">{{ t("common.cancel") }}</button>
    <button type="button" class="btn btn-primary" :disabled="noChanges || importer.isPending.value" @click="confirming = true">
      {{ t("configTransfer.apply") }}
    </button>
  </SaveBar>

  <ConfirmDialog
    :open="confirming"
    :title="t('configTransfer.confirm.title')"
    :confirm-label="t('configTransfer.apply')"
    :busy="importer.isPending.value"
    @confirm="run('apply')"
    @cancel="confirming = false"
  >
    <template v-if="pending">
      {{
        pending.deleted
          ? t("configTransfer.confirm.bodyRemove", { created: pending.created, updated: pending.updated, deleted: pending.deleted })
          : t("configTransfer.confirm.body", { created: pending.created, updated: pending.updated })
      }}
    </template>
  </ConfirmDialog>
</template>

<style scoped>
.import-changes + .import-changes {
  margin-top: var(--space-2);
}
.import-changes summary {
  cursor: pointer;
  font-weight: var(--fw-semibold);
  padding: var(--space-1) 0;
}
.config-change-action {
  width: 7rem;
}
.config-change-key {
  width: 30%;
}
.config-change-fields {
  white-space: normal;
  overflow-wrap: anywhere;
}
</style>
