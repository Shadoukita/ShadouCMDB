<script setup lang="ts">
import { adminCrumbs } from "../sections";
import { computed, ref } from "vue";
import { ApiError } from "../../../api/client";
import { configApi, useImportConfig, type ImportResult } from "../../../api/uiSettings";
import Breadcrumbs from "../../../components/Breadcrumbs.vue";
import ConfirmDialog from "../../../components/ConfirmDialog.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import SchemaChangeList from "../../../components/SchemaChangeList.vue";
import { useDocumentTitle } from "../../../lib/composables";
import { plural } from "../../../lib/format";
import { useBrandingStore } from "../../../stores/branding";
import { useSessionStore } from "../../../stores/session";

/**
 * Administration › Export / import: the whole configuration (data model,
 * lookup lists, permission profiles, UI settings) as one JSON file. Importing is
 * always a dry run first: the API runs every change and rolls back, and this
 * page shows exactly what applying would do before the operator applies it.
 */
useDocumentTitle("Export / import");
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
    readError.value = `${file.name} is ${(file.size / 1024 / 1024).toFixed(1)} MiB; a configuration file can be at most 16 MiB.`;
    return;
  }
  try {
    parsed.value = JSON.parse(await file.text());
  } catch {
    readError.value = `${file.name} is not a JSON file. Choose a file downloaded with “Download configuration”.`;
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
const SECTION_NAMES: Record<string, string> = {
  areas: "Areas",
  classes: "CI classes",
  attributes: "Attributes",
  relationshipTypes: "Relationship types",
  relationshipRules: "Relationship rules",
  lookupLists: "Lookup lists",
  lookupListValues: "Lookup list values",
  permissionProfiles: "Permission profiles",
  uiSettings: "UI settings",
  uiAssets: "Logo and favicon",
};
const sectionName = (s: string) => SECTION_NAMES[s] ?? s;
</script>

<template>
  <Breadcrumbs :items="adminCrumbs('config')" />
  <div class="page-header">
    <div class="title"><h1>Export / import</h1></div>
  </div>

  <section class="panel">
    <div class="panel-header"><h2>Export</h2></div>
    <div class="panel-body">
      <p>
        One JSON file with the configuration of this installation: the data model (classes, attributes, relationship
        types and rules), the lookup lists and their values (such as status, environment, location and owner), permission
        profiles and the
        customization, logo and favicon included. It never contains users, passwords, sessions, configuration items or
        their relationships.
      </p>
      <button type="button" class="btn btn-primary" :disabled="exporting" @click="download">{{ exporting ? "Preparing…" : "Download configuration" }}</button>
      <ErrorAlert v-if="exportError" :error="exportError" title="Export failed" />
    </div>
  </section>

  <section class="panel" style="margin-top: var(--space-3)">
    <div class="panel-header"><h2>Import</h2></div>
    <div class="panel-body">
      <p>
        Choose a configuration file to see what importing it would change. Nothing changes until you apply it. Rows are
        matched by key and created or updated; nothing is deleted, except that the customization, logo and favicon are
        replaced by the file's.
      </p>
      <div class="inline-control">
        <label class="btn" for="config-file">{{ importer.isPending.value && !dryRun ? "Checking…" : "Choose a file…" }}</label>
        <input id="config-file" class="sr-only" type="file" accept=".json,application/json" :disabled="importer.isPending.value" @change="onFile" />
        <span v-if="fileName" class="muted">{{ fileName }}</span>
      </div>
      <div v-if="readError" class="alert alert-error" role="alert">{{ readError }}</div>

      <div v-if="validation" class="alert alert-error" role="alert">
        <strong>The file cannot be imported: {{ plural(validation.details.length, "problem") }}.</strong>
        <div>{{ validation.message }} Nothing was changed.</div>
        <ul>
          <li v-for="(d, i) in validation.details" :key="i"><code v-if="d.field">{{ d.field }}</code> {{ d.message }}</li>
        </ul>
      </div>
      <ErrorAlert v-else-if="error" :error="error" :title="dryRun ? 'Import failed; nothing was changed' : 'The dry run failed; nothing was changed'" />

      <div v-if="applied" class="alert alert-success" role="status">
        <strong>Imported {{ fileName }}.</strong>
        {{ plural(totals(applied).created, "row") }} created, {{ plural(totals(applied).updated, "row") }} updated. Every change is in the audit log.
      </div>
      <div v-if="applied && (applied.warnings.length > 0 || applied.uiSettingsIssues.length > 0)" class="alert alert-warn" role="status">
        <strong>Imported with warnings</strong>
        <ul>
          <li v-for="(w, i) in applied.warnings" :key="`w${i}`"><code>{{ w.path }}</code> {{ w.message }}</li>
          <li v-for="(w, i) in applied.uiSettingsIssues" :key="`u${i}`"><code>uiSettings.settings.{{ w.path }}</code> {{ w.message }}</li>
        </ul>
      </div>
    </div>

    <template v-if="dryRun">
      <div class="panel-body">
        <h3 class="subhead">Dry run of {{ fileName }}</h3>
        <p v-if="noChanges" class="alert" role="status">Importing this file changes nothing: this installation already matches it.</p>
        <p v-else>
          Applying it would create {{ plural(pending!.created, "row") }} and update {{ plural(pending!.updated, "row") }}<template v-if="pending!.deleted">
            and remove {{ plural(pending!.deleted, "row") }}</template>.
        </p>
      </div>
      <div class="panel-body flush">
        <table class="data" aria-label="Import summary">
          <thead>
            <tr>
              <th scope="col">Section</th>
              <th scope="col" class="num">Create</th>
              <th scope="col" class="num">Update</th>
              <th scope="col" class="num">Remove</th>
              <th scope="col" class="num">Unchanged</th>
              <th scope="col" class="num">Only here (kept)</th>
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
          <strong>Worth a look before applying</strong>
          <ul>
            <li v-for="(w, i) in dryRun.warnings" :key="`w${i}`"><code>{{ w.path }}</code> {{ w.message }}</li>
            <li v-for="(w, i) in dryRun.uiSettingsIssues" :key="`u${i}`"><code>uiSettings.settings.{{ w.path }}</code> {{ w.message }}</li>
          </ul>
        </div>
      </div>
      <div v-if="dryRun.schemaChanges.length > 0" class="panel-body">
        <h3 class="subhead">Database changes</h3>
        <p class="muted" style="margin-top: 0">
          Areas, classes and attributes are PostgreSQL schemas, tables and columns. Applying the import runs this SQL in the
          same transaction.
        </p>
        <SchemaChangeList :changes="dryRun.schemaChanges" />
      </div>
      <div v-if="dryRun.changes.length > 0" class="panel-body">
        <h3 class="subhead">Changes</h3>
        <details v-for="[sec, changes] in bySection(dryRun)" :key="sec" class="import-changes" open>
          <summary>{{ sectionName(sec) }} ({{ changes.length }})</summary>
          <table class="data">
            <tbody>
              <tr v-for="(c, i) in changes" :key="i">
                <td style="width: 90px">
                  <span :class="['badge', c.action === 'create' ? 'ok' : c.action === 'delete' ? 'danger' : 'warn']">{{ c.action }}</span>
                </td>
                <td class="mono" style="width: 30%">{{ c.key }}</td>
                <td>
                  <div v-for="f in c.fields" :key="f.field">
                    <code>{{ f.field }}</code>: <span class="diff-from">{{ show(f.from) }}</span> → <span class="diff-to">{{ show(f.to) }}</span>
                  </div>
                </td>
              </tr>
            </tbody>
          </table>
        </details>
      </div>
      <div class="form-footer">
        <button type="button" class="btn btn-primary" :disabled="noChanges || importer.isPending.value" @click="confirming = true">Apply import</button>
        <button type="button" class="btn" :disabled="importer.isPending.value" @click="reset">Cancel</button>
      </div>
    </template>
  </section>

  <ConfirmDialog :open="confirming" title="Apply this import?" confirm-label="Apply import" :busy="importer.isPending.value" @confirm="run('apply')" @cancel="confirming = false">
    <template v-if="pending">
      {{ plural(pending.created, "row") }} will be created and {{ plural(pending.updated, "row") }} updated<template v-if="pending.deleted">,
        {{ plural(pending.deleted, "row") }} removed</template>, in one transaction, as the dry run showed. Every change is recorded in the audit
      log under your name.
    </template>
  </ConfirmDialog>
</template>

<style scoped>
.subhead {
  font-size: var(--fs-md);
  margin: 0 0 var(--space-2);
}
.import-changes + .import-changes {
  margin-top: var(--space-2);
}
.import-changes summary {
  cursor: pointer;
  font-weight: var(--fw-semibold);
  padding: var(--space-1) 0;
}
</style>
