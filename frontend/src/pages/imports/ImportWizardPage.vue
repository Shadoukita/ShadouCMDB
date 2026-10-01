<script setup lang="ts">
import { computed, nextTick, watch } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { ApiError } from "../../api/client";
import { useImportJob, type ImportJob } from "../../api/imports";
import Breadcrumbs from "../../components/Breadcrumbs.vue";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import { useDocumentTitle } from "../../lib/composables";
import { plural } from "../../lib/format";
import { lastReachableStep, shownStep, stepOf, type WizardStep } from "../../lib/imports";
import { useImportAccess } from "../../lib/useImportAccess";
import { useSessionStore } from "../../stores/session";
import CheckStep from "./CheckStep.vue";
import CommitStep from "./CommitStep.vue";
import FileStep from "./FileStep.vue";
import ImportStatusBadge from "./ImportStatusBadge.vue";
import ImportStepper from "./ImportStepper.vue";
import MappingStep from "./MappingStep.vue";
import UploadStep from "./UploadStep.vue";

/**
 * /imports/new and /imports/:id: the import wizard. There is no job until a file is uploaded (/imports/new);
 * after that the step comes from the job's status, so a reload or a shared link resumes at the right place.
 * `?step=` reopens an earlier step. The job is polled while it runs (api/imports.ts), not in hidden tabs.
 */
const route = useRoute();
const router = useRouter();
const session = useSessionStore();
const { permitted, settings, available } = useImportAccess();

/** No id on /imports/new: nothing is uploaded yet. */
const id = computed(() => (typeof route.params.id === "string" && route.params.id ? route.params.id : undefined));
const job = useImportJob(computed(() => (permitted.value ? id.value : undefined)));
const data = computed(() => job.data.value);

const notFound = computed(() => job.error.value instanceof ApiError && job.error.value.status === 404);
/** A poll failed but the last answer is still on screen: the server keeps working, the UI keeps retrying. */
const pollFailing = computed(() => !!data.value && job.isError.value && !notFound.value);

const step = computed<WizardStep>(() => (data.value ? shownStep(data.value, route.query.step) : 1));
const last = computed<WizardStep>(() => (data.value ? lastReachableStep(data.value) : 1));
const linkTo = (s: WizardStep) => {
  if (!id.value) return undefined;
  return data.value && s === stepOf(data.value) ? `/imports/${id.value}` : `/imports/${id.value}?step=${s}`;
};

useDocumentTitle(() => (data.value ? `${data.value.file.name} · Import` : "New import"));
const crumbs = computed(() => [
  { label: "Inventory", to: "/cis" },
  { label: "Imports", to: "/imports" },
  { label: data.value?.file.name ?? (id.value ? "Import" : "New import") },
]);

// When the step changes (not on the first render), focus moves to the step's heading (§1.4).
watch(step, async () => {
  await nextTick();
  document.getElementById("step-heading")?.focus();
});

function onUploaded(j: ImportJob) {
  // The class chosen in the inventory, and the job a corrected file replaces, travel on to the mapping step.
  const query: Record<string, string> = {};
  if (presetClassKey.value) query.classKey = presetClassKey.value;
  if (presetFromJob.value) query.fromJob = presetFromJob.value;
  router.replace({ path: `/imports/${j.id}`, query });
}
function toMapping() {
  router.push({ path: `/imports/${id.value}`, query: { ...route.query, step: "2" } });
}
/** The check or the import started: the job's own step takes over, with no stale filters of an earlier check. */
function toJobStep() {
  router.replace({ path: `/imports/${id.value}` });
}
const presetClassKey = computed(() => (typeof route.query.classKey === "string" ? route.query.classKey : undefined));
const presetFromJob = computed(() => (typeof route.query.fromJob === "string" ? route.query.fromJob : undefined));

/** Settings loaded and import off: the job's step actions would all be refused with `403 import_disabled`. */
const importOff = computed(() => !!settings.data.value && !available.value);

const committedRows = computed(() => {
  const c = data.value?.summary?.committed;
  return c ? c.created + c.updated + c.unchanged : 0;
});
</script>

<template>
  <Breadcrumbs :items="crumbs" />

  <EmptyState v-if="!permitted" title="Permission denied">
    You need the <strong>Bulk import</strong> permission. Ask an administrator for access.
    <template #actions><RouterLink class="btn" to="/cis">Back to inventory</RouterLink></template>
  </EmptyState>

  <EmptyState v-else-if="notFound" title="Import not found">
    This import does not exist or belongs to another user.
    <template #actions><RouterLink class="btn" to="/imports">Back to imports</RouterLink></template>
  </EmptyState>

  <template v-else>
    <div class="page-header">
      <div class="title">
        <h1>{{ data ? data.file.name : "New import" }}</h1>
        <ImportStatusBadge v-if="data" :status="data.status" />
        <span v-if="data && session.isAdministrator && data.createdBy.id !== session.user?.id" class="muted">by {{ data.createdBy.name }}</span>
      </div>
    </div>

    <ImportStepper :current="step" :last="last" :link-to="linkTo" />

    <div v-if="pollFailing" class="alert alert-warn" role="status">
      <strong>Lost connection to the server, retrying…</strong> The import keeps running on the server.
    </div>

    <!-- A new import: the upload form, once the settings say import is on. -->
    <template v-if="!id">
      <LoadingState v-if="settings.isPending.value" label="Loading import settings…" />
      <ErrorAlert v-else-if="settings.isError.value" :error="settings.error.value" :on-retry="() => settings.refetch()" />
      <EmptyState v-else-if="!available" title="Bulk import is turned off">
        <template v-if="settings.data.value?.locked">Bulk import is disabled by the server configuration.</template>
        <template v-else>
          Bulk import is turned off for this instance. An administrator can turn it on under
          <RouterLink v-if="session.isAdministrator" to="/admin/import">Administration › Import</RouterLink>
          <template v-else>Administration › Import</template>.
        </template>
        <template #actions><RouterLink class="btn" to="/imports">Back to imports</RouterLink></template>
      </EmptyState>
      <section v-else class="panel" aria-labelledby="step-heading">
        <div class="panel-header"><h2 id="step-heading" tabindex="-1">Upload</h2></div>
        <div class="panel-body">
          <UploadStep :limits="settings.data.value!.limits" @uploaded="onUploaded" />
        </div>
      </section>
    </template>

    <LoadingState v-else-if="job.isPending.value" label="Loading import…" />
    <ErrorAlert v-else-if="!data" :error="job.error.value" :on-retry="() => job.refetch()" />

    <template v-else>
      <div v-if="data.status === 'expired'" class="alert" role="status">
        <strong>This import expired.</strong> Uploaded files are kept for 24 hours after the last step; the file and its
        error report are gone. The counts below stay.
      </div>
      <div v-else-if="data.status === 'failed' && data.phase !== 'analyse'" class="alert alert-error" role="alert">
        <strong>The import stopped: {{ data.error?.message ?? "an unexpected error" }}.</strong>
        <template v-if="data.phase === 'commit'">
          Rows imported before the stop stay imported ({{ plural(committedRows, 'row') }}). Run the same file again
          to finish; rows already imported will be unchanged.
        </template>
      </div>
      <div v-else-if="data.status === 'cancelled' && data.phase !== 'analyse'" class="alert" role="status">
        <strong>Cancelled after {{ data.progress.done.toLocaleString() }} of {{ data.progress.total.toLocaleString() }} rows.</strong>
        <template v-if="data.phase === 'commit'">
          Rows already imported stay imported. Run the same file again to finish; they will show as unchanged.
        </template>
      </div>

      <!-- While import is off the server refuses every step (W3): only cancel and delete remain, on /imports. -->
      <div v-if="importOff" class="alert alert-warn" role="status">
        <strong v-if="settings.data.value?.locked">Bulk import is disabled by the server configuration.</strong>
        <template v-else>
          <strong>Bulk import is turned off for this instance.</strong>
          An administrator can turn it on under
          <RouterLink v-if="session.isAdministrator" to="/admin/import">Administration › Import</RouterLink>
          <template v-else>Administration › Import</template>.
        </template>
        This import cannot continue. You can still stop or delete it under
        <RouterLink to="/imports">Imports</RouterLink>. Its row problems and error report can be read again once an
        administrator turns bulk import back on.
      </div>
      <ErrorAlert v-else-if="settings.isError.value" :error="settings.error.value" :on-retry="() => settings.refetch()" />
      <LoadingState v-else-if="!settings.data.value" label="Loading import settings…" />
      <FileStep v-else-if="step === 1" :job="data" @next="toMapping" />
      <MappingStep v-else-if="step === 2" :job="data" :preset-class-key="presetClassKey" :preset-from-job="presetFromJob" @checking="toJobStep" />
      <CheckStep v-else-if="step === 3" :job="data" @committing="toJobStep" />
      <CommitStep v-else :job="data" />
    </template>
  </template>
</template>
