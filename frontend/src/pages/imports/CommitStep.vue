<script setup lang="ts">
import { useQueryClient } from "@tanstack/vue-query";
import { computed, ref, watch } from "vue";
import { RouterLink } from "vue-router";
import { importDownloads, type ImportJob } from "../../api/imports";
import { keys, useCiClasses } from "../../api/queries";
import ErrorAlert from "../../components/ErrorAlert.vue";
import { FINAL, RUNNING } from "../../lib/imports";
import { useSessionStore } from "../../stores/session";
import ImportProgress from "./ImportProgress.vue";
import Icon from "../../components/Icon.vue";
import { formatNumber, t } from "../../i18n";

/**
 * Step 4: the import itself. While it runs: progress (Stop import sits in the page head, ImportStopAction). Then the result counts, links to the imported CIs and the audit entry, and the error report when rows
 * were skipped or failed.
 */
const props = defineProps<{
  job: ImportJob;
  /** Import is off: the result and the error report stay readable, a new import is hidden. */
  readOnly?: boolean;
}>();

const session = useSessionStore();
const classes = useCiClasses();

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
const n = formatNumber;

const result = computed(() => {
  const c = counts.value;
  if (!c || running.value) return undefined;
  const parts = [
    t("imports.commit.created", { n: n(c.created) }),
    t("imports.commit.updated", { n: n(c.updated) }),
    t("imports.commit.unchanged", { n: n(c.unchanged) }),
  ];
  if (c.skipped) parts.push(t("imports.commit.skipped", { n: n(c.skipped) }));
  if (c.failed) parts.push(t("imports.commit.failed", { n: n(c.failed) }));
  parts.push(t("imports.commit.relationships", { n: c.relationshipsAdded }));
  const finished = props.job.status === "completed" || props.job.status === "completed_with_errors";
  return t(finished ? "imports.commit.finished" : "imports.commit.beforeStop", { parts: parts.join(", ") });
});
const hasReport = computed(() => !!counts.value && counts.value.skipped + counts.value.failed > 0 && props.job.status !== "expired");

const classId = computed(() => classes.data.value?.find((c) => c.key === props.job.classKey)?.id);
const inventoryTo = computed(() => (classId.value ? { path: "/cis", query: { classId: classId.value } } : { path: "/cis" }));
const auditTo = computed(() => ({
  path: "/admin/audit",
  query: { entityType: "import_jobs", entityId: props.job.id, action: "import.commit" },
}));

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
    <div class="panel-header"><h2 id="step-heading" tabindex="-1">{{ t("imports.step.import") }}</h2></div>
    <div class="panel-body">
      <p v-if="job.status === 'queued' && job.phase === 'commit'" role="status">
        {{ t("imports.queued", { n: job.progress.queuePosition ?? 1 }) }}
      </p>
      <template v-if="job.status === 'committing'">
        <ImportProgress
          :label="t('imports.commit.progress', { done: n(job.progress.done), total: n(job.progress.total) })"
          :done="job.progress.done"
          :total="job.progress.total || null"
        />
        <p class="muted">{{ t("imports.commit.leave") }}</p>
      </template>

      <!-- Announced once, when the import ends. -->
      <div class="sr-only" aria-live="polite">{{ result }}</div>

      <template v-if="result">
        <p class="import-result" :class="{ 'import-count-error': hasReport }">
          <Icon :name="hasReport ? 'circle-alert' : 'circle-check'" /> <strong>{{ result }}</strong>
        </p>
        <p v-if="counts && counts.failed > 0" class="muted">
          {{ t("imports.commit.failedHint") }}
        </p>
        <div class="form-footer">
          <RouterLink class="btn btn-primary" :to="inventoryTo">{{ t("imports.commit.openInventory") }}</RouterLink>
          <RouterLink v-if="session.can('audit.view')" class="btn" :to="auditTo">{{ t("imports.commit.audit") }}</RouterLink>
          <button v-if="hasReport" type="button" class="btn" :disabled="downloading" @click="download">
            {{ downloading ? t("imports.report.preparing") : t("imports.report.download") }}
          </button>
          <RouterLink v-if="!readOnly" class="btn btn-link" to="/imports/new">{{ t("imports.commit.another") }}</RouterLink>
        </div>
        <ErrorAlert v-if="downloadError" :error="downloadError" :title="t('imports.report.failed')" />
      </template>
    </div>
  </section>
</template>
