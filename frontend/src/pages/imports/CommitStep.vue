<script setup lang="ts">
import { useQueryClient } from "@tanstack/vue-query";
import { computed, ref, watch } from "vue";
import { RouterLink } from "vue-router";
import { importDownloads, useCancelImport, type ImportJob } from "../../api/imports";
import { keys, useCiClasses } from "../../api/queries";
import ConfirmDialog from "../../components/ConfirmDialog.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import { FINAL, RUNNING } from "../../lib/imports";
import { useSessionStore } from "../../stores/session";
import ImportProgress from "./ImportProgress.vue";

/**
 * Step 4: the import itself. While it runs: progress and Stop import (after the current batch of at most 500
 * rows). Then the result counts, links to the imported CIs and the audit entry, and the error report when rows
 * were skipped or failed.
 */
const props = defineProps<{ job: ImportJob }>();

const session = useSessionStore();
const classes = useCiClasses();
const cancel = useCancelImport();

// Once the import ends, the inventory, its counts (the sidebar's too) and relationships have changed.
const qc = useQueryClient();
watch(
  () => props.job.status,
  (now, before) => {
    if (before && RUNNING.has(before) && FINAL.has(now)) {
      qc.invalidateQueries({ queryKey: keys.cis });
      qc.invalidateQueries({ queryKey: ["relationships"] });
    }
  },
);

const running = computed(() => props.job.status === "committing" || (props.job.status === "queued" && props.job.phase === "commit"));
const counts = computed(() => props.job.summary?.committed ?? null);
const n = (v: number) => v.toLocaleString();

const result = computed(() => {
  const c = counts.value;
  if (!c || running.value) return undefined;
  const parts = [`${n(c.created)} created`, `${n(c.updated)} updated`, `${n(c.unchanged)} unchanged`];
  if (c.skipped) parts.push(`${n(c.skipped)} skipped`);
  if (c.failed) parts.push(`${n(c.failed)} failed`);
  parts.push(`${n(c.relationshipsAdded)} ${c.relationshipsAdded === 1 ? "relationship" : "relationships"} added`);
  const head = props.job.status === "completed" || props.job.status === "completed_with_errors" ? "Import finished" : "Imported before the stop";
  return `${head}: ${parts.join(", ")}.`;
});
const hasReport = computed(() => !!counts.value && counts.value.skipped + counts.value.failed > 0 && props.job.status !== "expired");

const classId = computed(() => classes.data.value?.find((c) => c.key === props.job.classKey)?.id);
const inventoryTo = computed(() => (classId.value ? { path: "/cis", query: { classId: classId.value } } : { path: "/cis" }));
const auditTo = computed(() => ({
  path: "/admin/audit",
  query: { entityType: "import_jobs", entityId: props.job.id, action: "import.commit" },
}));

// ---------- Stop ----------

const stopping = ref(false);
function confirmStop() {
  cancel.mutate(props.job.id, { onSuccess: () => (stopping.value = false) });
}

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
</script>

<template>
  <section class="panel" aria-labelledby="step-heading">
    <div class="panel-header"><h2 id="step-heading" tabindex="-1">Import</h2></div>
    <div class="panel-body">
      <p v-if="job.status === 'queued' && job.phase === 'commit'" role="status">
        Waiting for {{ job.progress.queuePosition ?? 1 }} other {{ (job.progress.queuePosition ?? 1) === 1 ? "import" : "imports" }} to finish…
      </p>
      <template v-if="job.status === 'committing'">
        <ImportProgress
          :label="`Importing row ${n(job.progress.done)} of ${n(job.progress.total)}`"
          :done="job.progress.done"
          :total="job.progress.total || null"
        />
        <div class="inline-control">
          <button type="button" class="btn btn-danger" :disabled="cancel.isPending.value" @click="stopping = true">Stop import</button>
          <span class="muted">You can leave this page. The import continues and you can come back from Imports.</span>
        </div>
      </template>

      <!-- Announced once, when the import ends. -->
      <div class="sr-only" aria-live="polite">{{ result }}</div>

      <template v-if="result">
        <p class="import-result" :class="{ 'import-count-error': hasReport }">
          <span aria-hidden="true">{{ hasReport ? "! " : "✓ " }}</span><strong>{{ result }}</strong>
        </p>
        <p v-if="counts && counts.failed > 0" class="muted">
          Failed rows changed on the server after the check (for example, a referenced CI was deleted). The error report
          names the reason for each row. Fix them and run the file again; rows already imported will be unchanged.
        </p>
        <div class="form-footer">
          <RouterLink class="btn btn-primary" :to="inventoryTo">Open inventory</RouterLink>
          <RouterLink v-if="session.can('audit.view')" class="btn" :to="auditTo">View in audit log</RouterLink>
          <button v-if="hasReport" type="button" class="btn" :disabled="downloading" @click="download">
            {{ downloading ? "Preparing the report…" : "Download error report" }}
          </button>
          <RouterLink class="btn btn-link" to="/imports/new">Import another file</RouterLink>
        </div>
        <ErrorAlert v-if="downloadError" :error="downloadError" title="The error report was not downloaded" />
      </template>
    </div>
  </section>

  <ConfirmDialog
    :open="stopping"
    :title="`Stop the import of “${job.file.name}”?`"
    confirm-label="Stop import"
    :busy="cancel.isPending.value"
    @cancel="stopping = false"
    @confirm="confirmStop"
  >
    <ErrorAlert v-if="cancel.isError.value" :error="cancel.error.value" title="Not stopped" />
    <p>
      {{ n(job.progress.done) }} of {{ n(job.progress.total) }} rows are processed so far. Rows already imported stay
      imported. The import stops after the current batch of at most 500 rows.
    </p>
    <p>You can run the same file again later; rows already imported will show as unchanged.</p>
  </ConfirmDialog>
</template>
