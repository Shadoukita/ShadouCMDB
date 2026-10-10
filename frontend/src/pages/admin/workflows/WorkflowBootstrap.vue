<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useBootstrapWorkflow, type WorkflowBootstrapResult, type WorkflowDefinitionDetail } from "../../../api/workflows";
import ConfirmDialog from "../../../components/ConfirmDialog.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import { formatNumber, t, tAround } from "../../../i18n";

/**
 * Adopting the existing CIs (POST /admin/workflow-definitions/{id}/bootstrap): an active workflow that drives a state
 * field locks that field on every CI it covers, and the CIs that existed before it have no instance yet. The bootstrap
 * starts one on each, in the state matching the CI's current value. It always previews first (a dry run: counts per
 * state, values no state maps), and the real run is confirmed with those numbers. A run that stops part way is
 * finished by running it again; CIs that already run the workflow are left alone.
 */
const props = defineProps<{ workflow: WorkflowDefinitionDetail }>();
/** A real run finished: what it started and what it skipped (the CIs still without an instance). */
const emit = defineEmits<{ done: [result: WorkflowBootstrapResult] }>();
const bootstrap = useBootstrapWorkflow();
const preview = ref<WorkflowBootstrapResult | null>(null);
const done = ref<WorkflowBootstrapResult | null>(null);
const confirming = ref(false);

/** Why the bootstrap cannot run now; the API refuses it (409 CONFLICT) in each case. */
const blocked = computed(() => {
  const w = props.workflow;
  if (w.currentVersionNo === null) return t("wfAdmin.bootstrap.needVersion");
  if (!w.isActive) return t("wfAdmin.bootstrap.needActive");
  return null;
});

// Settings saved or a version published since the preview: its numbers may no longer hold.
watch(
  () => [props.workflow.version, props.workflow.currentVersionNo, props.workflow.isActive],
  () => {
    preview.value = null;
    confirming.value = false;
  },
);

function runPreview() {
  done.value = null;
  bootstrap.mutate(
    { id: props.workflow.id, dryRun: true },
    {
      onSuccess: (r) => (preview.value = r),
      onError: () => (preview.value = null),
    },
  );
}

function runBootstrap() {
  bootstrap.mutate(
    { id: props.workflow.id, dryRun: false },
    {
      onSuccess: (r) => {
        done.value = r;
        preview.value = null;
        emit("done", r);
        confirming.value = false;
      },
      onError: () => (confirming.value = false),
    },
  );
}

const result = computed(() => preview.value ?? done.value);
const n = (v: number) => formatNumber(v);
const cis = (v: number) => t("wfAdmin.bootstrap.cis", { n: v });
const confirmParts = computed(() => tAround("wfAdmin.bootstrap.confirmBody", "name", { version: preview.value?.versionNo ?? "", cis: cis(preview.value?.started ?? 0) }));
</script>

<template>
  <section id="wf-bootstrap" class="panel" aria-labelledby="wf-bootstrap-title" tabindex="-1" data-testid="wf-bootstrap">
    <div class="panel-header"><h2 id="wf-bootstrap-title">{{ t("wfAdmin.bootstrap.title") }}</h2></div>
    <div class="panel-body stack">
      <p class="muted no-margin">{{ t("wfAdmin.bootstrap.intro") }}</p>
      <div v-if="blocked" class="alert" role="status">{{ blocked }}</div>
      <ErrorAlert v-if="bootstrap.isError.value" :error="bootstrap.error.value" :title="t('wfAdmin.bootstrap.failed')" />
      <div v-if="done" class="alert" role="status" data-testid="wf-bootstrap-done">
        <strong>{{ t("wfAdmin.bootstrap.done", { n: done.started, version: done.versionNo }) }}</strong>
        <div>
          {{
            t("wfAdmin.bootstrap.doneDetail", { running: cis(done.alreadyRunning), terminal: cis(done.skippedTerminal), unmapped: cis(done.skippedUnmapped) })
          }}
        </div>
      </div>
      <div>
        <button type="button" class="btn" :disabled="!!blocked || bootstrap.isPending.value" @click="runPreview">
          {{ bootstrap.isPending.value && !confirming ? t("wfAdmin.counting") : preview ? t("wfAdmin.previewAgain") : t("wfAdmin.bootstrap.preview") }}
        </button>
      </div>

      <template v-if="result">
        <dl class="props" data-testid="wf-bootstrap-summary">
          <dt>{{ result.dryRun ? t("wfAdmin.bootstrap.sum.wouldStart") : t("wfAdmin.bootstrap.sum.started") }}</dt>
          <dd>{{ t("wfAdmin.bootstrap.sum.onVersion", { n: n(result.started), version: result.versionNo }) }}</dd>
          <dt>{{ t("wfAdmin.bootstrap.sum.running") }}</dt>
          <dd class="mono">{{ n(result.alreadyRunning) }}</dd>
          <dt>{{ t("wfAdmin.bootstrap.sum.terminal") }}</dt>
          <dd class="mono">{{ n(result.skippedTerminal) }}</dd>
          <dt>{{ t("wfAdmin.bootstrap.sum.unmapped") }}</dt>
          <dd class="mono">{{ n(result.skippedUnmapped) }}</dd>
        </dl>
        <div v-if="result.states.length > 0" class="table-wrap">
          <table class="data" :aria-label="t('wfAdmin.bootstrap.perState')">
            <thead>
              <tr>
                <th scope="col">{{ t("wfAdmin.bootstrap.col.state") }}</th>
                <th scope="col">{{ t("wfAdmin.bootstrap.col.value") }}</th>
                <th scope="col" class="num">{{ t("wfAdmin.bootstrap.col.cis") }}</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="s in result.states" :key="s.stateKey" :class="{ disabled: s.terminal }">
                <td>
                  <span dir="auto">{{ s.stateName }}</span> <span v-if="s.terminal" class="badge off spaced">{{ t("wfAdmin.bootstrap.finalSkipped") }}</span>
                </td>
                <td class="mono">{{ s.valueKey }}</td>
                <td class="num mono">{{ n(s.count) }}</td>
              </tr>
            </tbody>
          </table>
        </div>
        <div v-if="result.unmapped.length > 0" class="alert alert-warn" role="status" data-testid="wf-bootstrap-unmapped">
          <strong>{{ t("wfAdmin.bootstrap.unmapped", { n: result.skippedUnmapped }) }}</strong>
          <ul class="no-margin">
            <li v-for="u in result.unmapped" :key="u.valueId ?? ''">
              <template v-if="u.valueId"><span dir="auto">{{ u.valueName ?? u.valueKey }}</span> <span class="mono muted">{{ u.valueKey }}</span></template>
              <em v-else>{{ t("wfAdmin.bootstrap.noValue") }}</em>: {{ cis(u.count) }}
            </li>
          </ul>
          <div>{{ t("wfAdmin.bootstrap.unmappedHelp") }}</div>
        </div>
        <div v-if="preview">
          <button type="button" class="btn btn-primary" :disabled="preview.started === 0 || bootstrap.isPending.value" @click="confirming = true">
            {{ preview.started === 0 ? t("wfAdmin.bootstrap.nothing") : t("wfAdmin.bootstrap.run", { n: preview.started }) }}
          </button>
        </div>
      </template>
    </div>
  </section>

  <ConfirmDialog
    :open="confirming"
    :title="preview ? t('wfAdmin.bootstrap.confirmTitle', { name: workflow.name, cis: cis(preview.started) }) : t('wfAdmin.bootstrap.confirmTitleAll', { name: workflow.name })"
    :confirm-label="t('wfAdmin.bootstrap.confirm')"
    :busy-label="t('wfAdmin.bootstrap.busy')"
    tone="primary"
    :busy="bootstrap.isPending.value"
    @cancel="confirming = false"
    @confirm="runBootstrap"
  >
    <p v-if="preview">{{ confirmParts[0] }}<strong dir="auto">{{ workflow.name }}</strong>{{ confirmParts[1] }}</p>
    <p v-if="preview && preview.skippedTerminal + preview.skippedUnmapped > 0">
      {{ t("wfAdmin.bootstrap.confirmSkipped", { cis: cis(preview.skippedTerminal + preview.skippedUnmapped) }) }}
    </p>
    <p>{{ t("wfAdmin.bootstrap.confirmDiffer") }}</p>
  </ConfirmDialog>
</template>
