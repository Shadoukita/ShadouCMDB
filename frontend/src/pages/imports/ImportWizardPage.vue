<script setup lang="ts">
import { computed, nextTick, watch } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { ApiError } from "../../api/client";
import { useImportJob, type ImportJob } from "../../api/imports";
import { useCiClasses } from "../../api/queries";
import Breadcrumbs from "../../components/Breadcrumbs.vue";
import EmptyState from "../../components/EmptyState.vue";
import PermissionDenied from "../../components/PermissionDenied.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import Icon from "../../components/Icon.vue";
import LoadingState from "../../components/LoadingState.vue";
import { formatNumber, t, tAround } from "../../i18n";
import { useDocumentTitle } from "../../lib/composables";
import { formatDateTime, formatRelative } from "../../lib/format";
import { lastReachableStep, shownStep, stepOf, type WizardStep } from "../../lib/imports";
import { useImportAccess } from "../../lib/useImportAccess";
import { useSessionStore } from "../../stores/session";
import CheckStep from "./CheckStep.vue";
import CommitStep from "./CommitStep.vue";
import FileStep from "./FileStep.vue";
import ImportHeadActions from "./ImportHeadActions.vue";
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

useDocumentTitle(() => (data.value ? t("imports.wizard.docTitle", { file: data.value.file.name }) : t("imports.new")));
const crumbs = computed(() => [
  { label: t("inventory.crumb"), to: "/cis" },
  { label: t("imports.title"), to: "/imports" },
  { label: data.value?.file.name ?? (id.value ? t("imports.wizard.crumb") : t("imports.new")) },
]);

/** The class chip links to that class's inventory, as on a CI page. */
const classes = useCiClasses();
const jobClass = computed(() => (data.value?.classKey ? classes.data.value?.find((c) => c.key === data.value!.classKey) : undefined));
/** "An administrator can turn it on under ‹Administration › Import›.": the link wraps the menu path. */
const turnOn = computed(() => tAround("imports.off.hintLink", "link"));

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
/** While off, the check and import results stay readable: their row problems and error report are plain reads. */
const readableWhileOff = computed(() => step.value >= 3 && !!data.value?.summary);

/** An administrator looking at another user's job: they may read, cancel or delete it, never change or commit it. */
const othersJob = computed(() => !!data.value && session.isAdministrator && data.value.createdBy.id !== session.user?.id);

const committedRows = computed(() => {
  const c = data.value?.summary?.committed;
  return c ? c.created + c.updated + c.unchanged : 0;
});
</script>

<template>
  <Breadcrumbs v-if="permitted && notFound" :items="crumbs" />

  <PermissionDenied v-if="!permitted" :crumbs="crumbs" :permissions="['cis.import']" :panel-title="t('imports.denied.panelTitle')">
    {{ t("imports.denied.body", { permission: t("permission.cis.import") }) }}
    <template #actions><RouterLink class="btn btn-primary" to="/cis">{{ t("imports.backToInventory") }}</RouterLink></template>
  </PermissionDenied>

  <EmptyState v-else-if="notFound" :title="t('imports.wizard.notFound.title')">
    {{ t("imports.wizard.notFound.body") }}
    <template #actions><RouterLink class="btn" to="/imports">{{ t("imports.wizard.back") }}</RouterLink></template>
  </EmptyState>

  <template v-else>
    <!-- The CI page head band: the file is the record, in mono like a CI name. -->
    <div class="record-head record-head-plain import-head">
      <Breadcrumbs :items="crumbs" />
      <div class="page-header record-header">
        <div class="record-heading">
          <span class="class-tile class-tile-lg" aria-hidden="true"><Icon name="upload" class="class-icon" /></span>
          <div class="record-title">
            <div class="title">
              <h1 dir="auto">{{ data ? data.file.name : t("imports.new") }}</h1>
            </div>
            <p v-if="data" class="record-meta" data-testid="record-meta">
              <ImportStatusBadge :status="data.status" />
              <RouterLink v-if="jobClass" class="badge record-class-chip" :to="`/cis?classId=${jobClass.id}`" dir="auto">{{ jobClass.name }}</RouterLink>
              <span v-if="data.file.rowCount != null" class="badge">{{ t("imports.wizard.rows", { n: data.file.rowCount }) }}</span>
              <span class="record-meta-line">
                <span v-if="othersJob" dir="auto">{{ t("imports.wizard.by", { name: data.createdBy.name }) }}</span>
                <span v-if="othersJob" class="sep" aria-hidden="true">·</span>
                <time :datetime="data.createdAt" :title="formatDateTime(data.createdAt)">{{ t("imports.wizard.started", { when: formatRelative(data.createdAt) }) }}</time>
              </span>
            </p>
            <p v-else class="record-meta">{{ t("imports.intro") }}</p>
          </div>
        </div>
        <ImportHeadActions v-if="data" :job="data" />
      </div>
    </div>

    <ImportStepper :current="step" :last="last" :link-to="linkTo" />

    <div v-if="othersJob" class="alert alert-warn" role="status">
      <strong>{{ t("imports.wizard.others.title", { name: data?.createdBy.name }) }}</strong>
      {{ t("imports.wizard.others.body", { name: data?.createdBy.name }) }}
    </div>

    <div v-if="pollFailing" class="alert alert-warn" role="status">
      <strong>{{ t("imports.wizard.poll.title") }}</strong> {{ t("imports.wizard.poll.body") }}
    </div>

    <!-- A new import: the upload form, once the settings say import is on. -->
    <template v-if="!id">
      <LoadingState v-if="settings.isPending.value" :label="t('imports.settingsLoading')" />
      <ErrorAlert v-else-if="settings.isError.value" :error="settings.error.value" :on-retry="() => settings.refetch()" />
      <EmptyState v-else-if="!available" :title="t('imports.wizard.off.title')">
        <template v-if="settings.data.value?.locked">{{ t("imports.off.locked") }}</template>
        <template v-else>
          {{ t("imports.off.title") }} {{ turnOn[0]
          }}<RouterLink v-if="session.isAdministrator" to="/admin/import">{{ t("imports.off.adminPath") }}</RouterLink
          ><template v-else>{{ t("imports.off.adminPath") }}</template>{{ turnOn[1] }}
        </template>
        <template #actions><RouterLink class="btn" to="/imports">{{ t("imports.wizard.back") }}</RouterLink></template>
      </EmptyState>
      <section v-else class="panel" aria-labelledby="step-heading">
        <div class="panel-header"><h2 id="step-heading" tabindex="-1">{{ t("imports.step.upload") }}</h2></div>
        <div class="panel-body">
          <UploadStep :limits="settings.data.value!.limits" @uploaded="onUploaded" />
        </div>
      </section>
    </template>

    <LoadingState v-else-if="job.isPending.value" :label="t('imports.wizard.loading')" />
    <ErrorAlert v-else-if="!data" :error="job.error.value" :on-retry="() => job.refetch()" />

    <template v-else>
      <div v-if="data.status === 'expired'" class="alert" role="status">
        <strong>{{ t("imports.wizard.expired.title") }}</strong> {{ t("imports.wizard.expired.body") }}
      </div>
      <div v-else-if="data.status === 'failed' && data.phase !== 'analyse'" class="alert alert-error" role="alert">
        <strong>{{ t("imports.wizard.failed.title", { reason: data.error?.message ?? t("imports.wizard.failed.unexpected") }) }}</strong>
        <template v-if="data.phase === 'commit'">{{ " " }}{{ t("imports.wizard.failed.commit", { n: committedRows }) }}</template>
      </div>
      <div v-else-if="data.status === 'cancelled' && data.phase !== 'analyse'" class="alert" role="status">
        <strong>{{ t("imports.wizard.cancelled.title", { done: formatNumber(data.progress.done), total: formatNumber(data.progress.total) }) }}</strong>
        <template v-if="data.phase === 'commit'">{{ " " }}{{ t("imports.wizard.cancelled.commit") }}</template>
      </div>

      <!-- While import is off the server refuses every step (W3): only Stop and Delete remain, in the head.
           The check or import result stays below, read-only, with its row problems and error report. -->
      <template v-if="importOff">
        <div class="alert alert-warn" role="status">
          <strong v-if="settings.data.value?.locked">{{ t("imports.off.locked") }}</strong>
          <template v-else>
            <strong>{{ t("imports.off.title") }}</strong>
            {{ turnOn[0] }}<RouterLink v-if="session.isAdministrator" to="/admin/import">{{ t("imports.off.adminPath") }}</RouterLink
            ><template v-else>{{ t("imports.off.adminPath") }}</template>{{ turnOn[1] }}
          </template>
          {{ t("imports.wizard.off.cannotContinue") }}
          <template v-if="readableWhileOff">{{ t("imports.wizard.off.readOnly") }}</template>
        </div>
        <CheckStep v-if="readableWhileOff && step === 3" :job="data" read-only />
        <CommitStep v-else-if="readableWhileOff" :job="data" read-only />
      </template>
      <ErrorAlert v-else-if="settings.isError.value" :error="settings.error.value" :on-retry="() => settings.refetch()" />
      <LoadingState v-else-if="!settings.data.value" :label="t('imports.settingsLoading')" />
      <FileStep v-else-if="step === 1" :job="data" @next="toMapping" />
      <MappingStep v-else-if="step === 2" :job="data" :preset-class-key="presetClassKey" :preset-from-job="presetFromJob" @checking="toJobStep" />
      <CheckStep v-else-if="step === 3" :job="data" @committing="toJobStep" />
      <CommitStep v-else :job="data" />
    </template>
  </template>
</template>
