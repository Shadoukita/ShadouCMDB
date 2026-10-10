<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { ApiError } from "../../../api/client";
import {
  useMigrateInstances,
  useWorkflowVersion,
  type WorkflowDefinitionDetail,
  type WorkflowMigrationReport,
  type WorkflowVersionSummary,
} from "../../../api/workflows";
import ConfirmDialog from "../../../components/ConfirmDialog.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import { formatNumber, t, tAround } from "../../../i18n";

/**
 * Moving the running instances of an older version to a newer published one
 * (POST /admin/workflow-definitions/{id}/instance-migrations). Every non-terminal state of the old version gets a
 * target state: by default the state of the same key, when the target version has one that is not terminal. It always
 * previews first (a dry run: instances per state, pending approvals), and the real run is confirmed with those numbers.
 * A run cut short leaves the moved batches moved; running it again moves the rest.
 */
const props = defineProps<{ workflow: WorkflowDefinitionDetail; from: WorkflowVersionSummary; versions: WorkflowVersionSummary[] }>();
const emit = defineEmits<{ close: [] }>();
const wid = computed(() => props.workflow.id);

/** Published versions newer than the source, newest first (the current one leads). */
const targets = computed(() => props.versions.filter((v) => v.status === "published" && v.versionNo > props.from.versionNo));
const toNo = ref<number | null>(targets.value[0]?.versionNo ?? null);
const fromVersion = useWorkflowVersion(wid, () => props.from.versionNo);
const toVersion = useWorkflowVersion(wid, toNo);

const sources = computed(() => (fromVersion.data.value?.states ?? []).filter((s) => !s.terminal));
const targetStates = computed(() => (toVersion.data.value?.states ?? []).filter((s) => !s.terminal));
/** What the API picks for a state left out of `stateMap`: the target's non-terminal state of the same key, if any. */
const sameKey = (key: string) => (targetStates.value.some((s) => s.key === key) ? key : "");

const stateMap = ref<Record<string, string>>({});
const pendingApprovals = ref<"skip" | "cancel">("skip");
watch(
  [sources, targetStates],
  () => {
    // A choice the (new) target version still has is kept; the rest fall back to the same key.
    const valid = (k: string | undefined) => !!k && targetStates.value.some((t) => t.key === k);
    const before = stateMap.value;
    stateMap.value = Object.fromEntries(sources.value.map((s) => [s.key, valid(before[s.key]) ? before[s.key] : sameKey(s.key)]));
  },
  { immediate: true },
);

const migrate = useMigrateInstances();
const preview = ref<WorkflowMigrationReport | null>(null);
const done = ref<WorkflowMigrationReport | null>(null);
const confirming = ref(false);

// Any change to what would run makes the preview's numbers stale.
watch([toNo, stateMap, pendingApprovals], () => {
  preview.value = null;
  confirming.value = false;
}, { deep: true });

const fieldErrors = computed(() => (migrate.error.value instanceof ApiError ? migrate.error.value.fieldErrors() : {}));
const rowError = (key: string) => fieldErrors.value[`stateMap.${key}`];
const versionError = computed(() => fieldErrors.value.toVersionNo ?? fieldErrors.value.fromVersionNo);

function body(dryRun: boolean) {
  // Only the states that differ from the same-key default are sent, so the report tells explicit choices apart.
  const map = Object.fromEntries(Object.entries(stateMap.value).filter(([k, v]) => v && v !== sameKey(k)));
  return { fromVersionNo: props.from.versionNo, toVersionNo: toNo.value!, stateMap: map, dryRun, pendingApprovals: pendingApprovals.value };
}

function runPreview() {
  done.value = null;
  migrate.mutate(
    { id: wid.value, body: body(true) },
    {
      onSuccess: (r) => (preview.value = r),
      onError: () => (preview.value = null),
    },
  );
}

function runMigration() {
  migrate.mutate(
    { id: wid.value, body: body(false) },
    {
      onSuccess: (r) => {
        done.value = r;
        preview.value = null;
        confirming.value = false;
      },
      onError: () => (confirming.value = false),
    },
  );
}

const countOf = (key: string) => preview.value?.states.find((s) => s.fromState === key)?.count;
const stateName = (key: string) => toVersion.data.value?.states.find((s) => s.key === key)?.name ?? key;
/** Instances a real run would move: with `skip`, those waiting on an approval stay. */
const moving = computed(() => (preview.value ? preview.value.total - (pendingApprovals.value === "skip" ? preview.value.pendingApprovals : 0) : 0));
const n = (v: number) => formatNumber(v);
const confirmParts = computed(() => tAround("wfAdmin.migrate.confirmBody", "name", { from: preview.value?.fromVersionNo ?? "", to: preview.value?.toVersionNo ?? "" }));
</script>

<template>
  <section class="panel" aria-labelledby="wf-migrate-title" data-testid="wf-migrate">
    <div class="panel-header">
      <h2 id="wf-migrate-title">{{ t("wfAdmin.migrate.title", { n: from.versionNo }) }}</h2>
      <button type="button" class="btn btn-sm" @click="emit('close')">{{ t("wfAdmin.migrate.close") }}</button>
    </div>
    <div class="panel-body stack">
      <p class="muted no-margin">{{ t("wfAdmin.migrate.intro", { n: from.versionNo }) }}</p>
      <div v-if="targets.length === 0" class="alert" role="status">{{ t("wfAdmin.migrate.noTarget") }}</div>
      <template v-else>
        <ErrorAlert v-if="migrate.isError.value" :error="migrate.error.value" :title="t('wfAdmin.migrate.failed')" />
        <div v-if="done" class="alert" role="status" data-testid="wf-migrate-done">
          <strong>{{ t("wfAdmin.migrate.done", { n: done.migrated, from: done.fromVersionNo, to: done.toVersionNo }) }}</strong>
          <div>
            <template v-if="done.skipped > 0">{{ t("wfAdmin.migrate.doneSkipped", { n: done.skipped, from: done.fromVersionNo }) }} </template>
            <template v-if="done.migrated + done.skipped < done.total">{{ t("wfAdmin.migrate.doneGone", { n: done.total - done.migrated - done.skipped }) }} </template>
            {{ t("wfAdmin.migrate.again") }}
          </div>
        </div>

        <div class="form-grid">
          <div class="field">
            <label for="wf-migrate-to">{{ t("wfAdmin.migrate.target") }}</label>
            <select id="wf-migrate-to" v-model="toNo" :aria-invalid="!!versionError" :aria-describedby="versionError ? 'wf-migrate-to-err' : undefined">
              <option v-for="v in targets" :key="v.versionNo" :value="v.versionNo">
                {{ v.isCurrent ? t("wfAdmin.migrate.targetCurrent", { n: v.versionNo }) : t("wfAdmin.versionN", { n: v.versionNo }) }}
              </option>
            </select>
            <span v-if="versionError" id="wf-migrate-to-err" class="error">{{ versionError }}</span>
          </div>
          <fieldset class="group">
            <legend>{{ t("wfAdmin.migrate.pending") }}</legend>
            <label class="checkbox-row">
              <input v-model="pendingApprovals" type="radio" name="wf-migrate-pending" value="skip" />
              {{ t("wfAdmin.migrate.pendingSkip", { n: from.versionNo }) }}
            </label>
            <label class="checkbox-row">
              <input v-model="pendingApprovals" type="radio" name="wf-migrate-pending" value="cancel" />
              {{ t("wfAdmin.migrate.pendingCancel") }}
            </label>
          </fieldset>
        </div>

        <LoadingState v-if="fromVersion.isLoading.value || toVersion.isLoading.value" :label="t('wfAdmin.migrate.loading')" />
        <ErrorAlert v-else-if="fromVersion.isError.value" :error="fromVersion.error.value" :on-retry="() => fromVersion.refetch()" />
        <ErrorAlert v-else-if="toVersion.isError.value" :error="toVersion.error.value" :on-retry="() => toVersion.refetch()" />
        <div v-else class="table-wrap">
          <table class="data" :aria-label="t('wfAdmin.migrate.map')">
            <thead>
              <tr>
                <th scope="col">{{ t("wfAdmin.migrate.col.from", { n: from.versionNo }) }}</th>
                <th scope="col" class="num">{{ t("wfAdmin.versions.col.running") }}</th>
                <th scope="col">{{ t("wfAdmin.migrate.col.to", { n: toNo ?? "" }) }}</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="s in sources" :key="s.key">
                <td><span dir="auto">{{ s.name }}</span> <span class="mono muted">{{ s.key }}</span></td>
                <td class="num mono">{{ countOf(s.key) === undefined ? (preview ? "0" : "–") : n(countOf(s.key)!) }}</td>
                <td>
                  <div class="field">
                    <label :for="`wf-migrate-map-${s.key}`" class="sr-only">{{ t("wfAdmin.migrate.mapLabel", { name: s.name }) }}</label>
                    <select
                      :id="`wf-migrate-map-${s.key}`"
                      v-model="stateMap[s.key]"
                      :aria-invalid="!!rowError(s.key)"
                      :aria-describedby="rowError(s.key) ? `wf-migrate-map-${s.key}-err` : undefined"
                    >
                      <option v-if="!sameKey(s.key)" value="">{{ t("wfAdmin.migrate.choose") }}</option>
                      <option v-for="ts in targetStates" :key="ts.key" :value="ts.key">
                        {{ ts.key === s.key ? t("wfAdmin.migrate.sameKey", { name: ts.name, key: ts.key }) : `${ts.name} (${ts.key})` }}
                      </option>
                    </select>
                    <span v-if="rowError(s.key)" :id="`wf-migrate-map-${s.key}-err`" class="error">{{ rowError(s.key) }}</span>
                  </div>
                </td>
              </tr>
            </tbody>
          </table>
        </div>

        <dl v-if="preview" class="props" data-testid="wf-migrate-summary">
          <dt>{{ t("wfAdmin.migrate.sum.running", { n: preview.fromVersionNo }) }}</dt>
          <dd class="mono">{{ n(preview.total) }}</dd>
          <dt>{{ t("wfAdmin.migrate.pending") }}</dt>
          <dd>
            <span class="mono">{{ n(preview.pendingApprovals) }}</span>
            <template v-if="preview.pendingApprovals > 0">: {{ pendingApprovals === "skip" ? t("wfAdmin.migrate.sum.stay") : t("wfAdmin.migrate.sum.closed") }}</template>
          </dd>
          <dt>{{ t("wfAdmin.migrate.sum.wouldMove") }}</dt>
          <dd>{{ t("wfAdmin.migrate.sum.toVersion", { n: n(moving), to: preview.toVersionNo }) }}</dd>
        </dl>

        <div class="inline-actions">
          <button type="button" class="btn" :disabled="!toNo || migrate.isPending.value" @click="runPreview">
            {{ migrate.isPending.value && !confirming ? t("wfAdmin.counting") : preview ? t("wfAdmin.previewAgain") : t("wfAdmin.migrate.preview") }}
          </button>
          <button v-if="preview" type="button" class="btn btn-primary" :disabled="moving === 0 || migrate.isPending.value" @click="confirming = true">
            {{ moving === 0 ? t("wfAdmin.nothingToMove") : t("wfAdmin.migrate.run", { n: moving }) }}
          </button>
        </div>
      </template>
    </div>
  </section>

  <ConfirmDialog
    :open="confirming"
    :title="preview ? t('wfAdmin.migrate.confirmTitle', { n: moving, to: toNo ?? '' }) : t('wfAdmin.migrate.confirmTitleAll', { to: toNo ?? '' })"
    :confirm-label="t('wfAdmin.migrate.confirm')"
    :busy-label="t('wfAdmin.migrate.busy')"
    :busy="migrate.isPending.value"
    @cancel="confirming = false"
    @confirm="runMigration"
  >
    <template v-if="preview">
      <p>{{ confirmParts[0] }}<strong dir="auto">{{ workflow.name }}</strong>{{ confirmParts[1] }}</p>
      <ul>
        <li v-for="s in preview.states.filter((x) => x.count > 0)" :key="s.fromState">
          {{ t("wfAdmin.migrate.confirmIn", { n: s.count }) }} <span class="mono">{{ s.fromState }}</span> → <span dir="auto">{{ stateName(s.toState) }}</span> <span class="mono">({{ s.toState }})</span>
        </li>
      </ul>
      <p v-if="preview.pendingApprovals > 0 && pendingApprovals === 'cancel'">
        <strong>{{ t("wfAdmin.migrate.confirmClosed", { n: preview.pendingApprovals }) }}</strong>
      </p>
      <p v-else-if="preview.pendingApprovals > 0">{{ t("wfAdmin.migrate.confirmStay", { n: preview.pendingApprovals, from: preview.fromVersionNo }) }}</p>
    </template>
    <p>{{ t("wfAdmin.migrate.confirmDiffer") }}</p>
  </ConfirmDialog>
</template>
