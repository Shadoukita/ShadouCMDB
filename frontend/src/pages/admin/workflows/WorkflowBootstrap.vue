<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useBootstrapWorkflow, type WorkflowBootstrapResult, type WorkflowDefinitionDetail } from "../../../api/workflows";
import ConfirmDialog from "../../../components/ConfirmDialog.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";

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
  if (w.currentVersionNo === null) return "Publish a version first: instances run the current published version.";
  if (!w.isActive) return "Activate the workflow first: an inactive workflow starts no instances.";
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
const n = (v: number) => v.toLocaleString();
const cis = (v: number) => `${n(v)} ${v === 1 ? "CI" : "CIs"}`;
</script>

<template>
  <section id="wf-bootstrap" class="panel" aria-labelledby="wf-bootstrap-title" tabindex="-1" data-testid="wf-bootstrap">
    <div class="panel-header"><h2 id="wf-bootstrap-title">Adopt existing CIs</h2></div>
    <div class="panel-body stack">
      <p class="muted no-margin">
        While this workflow is active, its state field is locked on every CI it covers. The bootstrap starts an instance on each covered
        CI that has none, in the state matching the CI's current state field value. Preview it first: nothing is written until you confirm.
      </p>
      <div v-if="blocked" class="alert" role="status">{{ blocked }}</div>
      <ErrorAlert v-if="bootstrap.isError.value" :error="bootstrap.error.value" title="The bootstrap did not run" />
      <div v-if="done" class="alert" role="status" data-testid="wf-bootstrap-done">
        <strong>Started {{ n(done.started) }} {{ done.started === 1 ? "instance" : "instances" }} on version {{ done.versionNo }}.</strong>
        <div>
          {{ cis(done.alreadyRunning) }} already ran the workflow; {{ cis(done.skippedTerminal) }} in a final state and
          {{ cis(done.skippedUnmapped) }} without a mapped value were skipped. Run it again to finish a run that stopped part way.
        </div>
      </div>
      <div>
        <button type="button" class="btn" :disabled="!!blocked || bootstrap.isPending.value" @click="runPreview">
          {{ bootstrap.isPending.value && !confirming ? "Counting…" : preview ? "Preview again" : "Preview bootstrap" }}
        </button>
      </div>

      <template v-if="result">
        <dl class="props" data-testid="wf-bootstrap-summary">
          <dt>{{ result.dryRun ? "Would start" : "Started" }}</dt>
          <dd>{{ n(result.started) }} on version {{ result.versionNo }}</dd>
          <dt>Already running</dt>
          <dd>{{ n(result.alreadyRunning) }}</dd>
          <dt>Skipped, final state</dt>
          <dd>{{ n(result.skippedTerminal) }}</dd>
          <dt>Skipped, no mapped value</dt>
          <dd>{{ n(result.skippedUnmapped) }}</dd>
        </dl>
        <div v-if="result.states.length > 0" class="table-wrap">
          <table class="data" aria-label="CIs per state">
            <thead>
              <tr>
                <th scope="col">State</th>
                <th scope="col">State field value</th>
                <th scope="col" class="num">CIs</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="s in result.states" :key="s.stateKey" :class="{ disabled: s.terminal }">
                <td>{{ s.stateName }} <span v-if="s.terminal" class="badge off spaced">Final: skipped</span></td>
                <td class="mono">{{ s.valueKey }}</td>
                <td class="num">{{ n(s.count) }}</td>
              </tr>
            </tbody>
          </table>
        </div>
        <div v-if="result.unmapped.length > 0" class="alert alert-warn" role="status" data-testid="wf-bootstrap-unmapped">
          <strong>{{ cis(result.skippedUnmapped) }} skipped: no state maps their value</strong>
          <ul class="no-margin">
            <li v-for="u in result.unmapped" :key="u.valueId ?? ''">
              <template v-if="u.valueId">{{ u.valueName ?? u.valueKey }} <span class="mono muted">{{ u.valueKey }}</span></template>
              <em v-else>No value</em>: {{ cis(u.count) }}
            </li>
          </ul>
          <div>Their state field stays locked. Map the values to states in a new version, or set a mapped value on them, then run the bootstrap again.</div>
        </div>
        <div v-if="preview">
          <button type="button" class="btn btn-primary" :disabled="preview.started === 0 || bootstrap.isPending.value" @click="confirming = true">
            {{ preview.started === 0 ? "Nothing to start" : `Start ${n(preview.started)} ${preview.started === 1 ? "instance" : "instances"}…` }}
          </button>
        </div>
      </template>
    </div>
  </section>

  <ConfirmDialog
    :open="confirming"
    :title="`Start ${workflow.name} on ${preview ? cis(preview.started) : 'the existing CIs'}?`"
    confirm-label="Start instances"
    busy-label="Starting…"
    tone="primary"
    :busy="bootstrap.isPending.value"
    @cancel="confirming = false"
    @confirm="runBootstrap"
  >
    <p v-if="preview">
      This starts an instance of <strong>{{ workflow.name }}</strong> (version {{ preview.versionNo }}) on {{ cis(preview.started) }}, each in the
      state matching its current state field value. Each start is recorded in the CI's workflow history and the audit log.
    </p>
    <p v-if="preview && preview.skippedTerminal + preview.skippedUnmapped > 0">
      {{ cis(preview.skippedTerminal + preview.skippedUnmapped) }} are skipped and keep a locked state field.
    </p>
    <p>The numbers can differ from the preview if CIs changed since.</p>
  </ConfirmDialog>
</template>
