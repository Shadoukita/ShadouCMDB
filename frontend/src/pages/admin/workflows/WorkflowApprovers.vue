<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { onBeforeRouteLeave, onBeforeRouteUpdate } from "vue-router";
import { useAllProfiles } from "../../../api/admin";
import { ApiError } from "../../../api/client";
import { useCiClasses, useClassAttributes } from "../../../api/queries";
import {
  useSaveApprovers,
  useWorkflowApprovers,
  useWorkflowDraft,
  useWorkflowVersion,
  type WorkflowApprovers,
  type WorkflowDefinitionDetail,
} from "../../../api/workflows";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import SaveBar from "../../../components/SaveBar.vue";
import { t } from "../../../i18n";
import { useFlashStore } from "../../../stores/flash";
import {
  approverFromApi,
  approverIdentity,
  approverLabel,
  approversBody,
  otherProblems,
  stepProblems,
  stepRows,
  type AttributeChoice,
  type DraftApprover,
  type StepRow,
} from "../../../lib/workflowApprovals";
import ApproverAddForm from "./ApproverAddForm.vue";
import ApproverPreview from "./ApproverPreview.vue";

/**
 * Who decides each approval step (SHAA-1869 §2.1, §10.1): assignments by transition and step key,
 * on the workflow itself rather than in a version, so staffing changes need no new version. The
 * whole set is saved at once, guarded by the workflow's version; the answer carries the approvers
 * lint (a step nobody may approve, approvers who cannot view the type, too few for the quorum).
 */
const props = defineProps<{ workflow: WorkflowDefinitionDetail }>();
const wid = computed(() => props.workflow.id);
const approvers = useWorkflowApprovers(wid);
const draft = useWorkflowDraft(wid);
const current = useWorkflowVersion(wid, () => props.workflow.currentVersionNo);
const profilesQ = useAllProfiles();
const cannotListProfiles = computed(() => profilesQ.error.value instanceof ApiError && profilesQ.error.value.status === 403);
const profiles = computed(() => (cannotListProfiles.value ? null : (profilesQ.data.value?.data ?? []).map((p) => ({ id: p.id, name: p.name }))));

const classes = useCiClasses();
const attrs = useClassAttributes(() => props.workflow.classId);
/** Reference fields of the type (own and inherited) to the Person type: the only fields an assignment may name. */
const personFields = computed<AttributeChoice[]>(() => {
  const person = new Set((classes.data.value ?? []).filter((c) => c.systemRole === "person").map((c) => c.id));
  return (attrs.data.value ?? [])
    .filter((a) => a.isActive && a.dataType === "reference" && !!a.referenceClassId && person.has(a.referenceClassId))
    .map((a) => ({ id: a.id, key: a.key, label: a.label }));
});

// ---------- Local edits ----------

const list = ref<DraftApprover[]>([]);
const serialize = (l: DraftApprover[]) => JSON.stringify(approversBody(l));
const base = ref(serialize([]));
const dirty = computed(() => serialize(list.value) !== base.value);
function seed(a: WorkflowApprovers) {
  list.value = a.approvers.map(approverFromApi);
  base.value = serialize(list.value);
}
watch(
  () => approvers.data.value,
  (a) => {
    if (a && !dirty.value) seed(a);
  },
  { immediate: true },
);

const rows = computed(() => stepRows([draft.data.value?.transitions ?? [], current.data.value?.transitions ?? []], list.value));
const loading = computed(() => approvers.isLoading.value || draft.isLoading.value || current.isLoading.value);
const rowKey = (r: StepRow) => `${r.transitionKey}|${r.stepKey}`;
const ofRow = (r: StepRow) => list.value.filter((a) => a.transitionKey === r.transitionKey && a.stepKey === r.stepKey);
const taken = (a: DraftApprover) => list.value.some((x) => approverIdentity(x) === approverIdentity(a));

function add(a: DraftApprover) {
  list.value = [...list.value, a];
}
function remove(a: DraftApprover) {
  list.value = list.value.filter((x) => x !== a);
}

const problems = computed(() => approvers.data.value?.problems ?? []);
const generalProblems = computed(() => otherProblems(problems.value, rows.value));

// ---------- Saving ----------

const save = useSaveApprovers();
const flash = useFlashStore();
const error = ref<unknown>(null);
const conflict = computed(() => error.value instanceof ApiError && error.value.code === "VERSION_CONFLICT");

async function submit() {
  const a = approvers.data.value;
  if (!a) return;
  error.value = null;
  try {
    const next = await save.mutateAsync({ id: wid.value, body: { version: a.version, approvers: approversBody(list.value) } });
    seed(next);
    flash.show(next.problems.length ? t("wfApprovers.savedWithWarnings", { n: next.problems.length }) : t("wfApprovers.saved"));
  } catch (e) {
    error.value = e;
  }
}
async function reload() {
  error.value = null;
  const res = await approvers.refetch();
  if (res.data) seed(res.data);
}
function reset() {
  if (approvers.data.value) seed(approvers.data.value);
  error.value = null;
}

/** A 400 names `approvers[i]…` of the body sent: show it on the assignment's row. */
const sentBody = computed(() => approversBody(list.value));
function fieldErrorsOf(a: DraftApprover): string[] {
  const e = error.value;
  if (!(e instanceof ApiError) || e.code !== "VALIDATION_ERROR") return [];
  const i = sentBody.value.findIndex((b) => b.transitionKey === a.transitionKey && b.stepKey === a.stepKey && JSON.stringify(b) === JSON.stringify(approversBody([a])[0]));
  return i < 0 ? [] : e.details.filter((d) => d.field === `approvers[${i}]` || d.field.startsWith(`approvers[${i}].`)).map((d) => d.message);
}

const keepChanges = () => !dirty.value || window.confirm(t("wfApprovers.leave"));
onBeforeRouteLeave(keepChanges);
onBeforeRouteUpdate(keepChanges);
function onBeforeUnload(e: BeforeUnloadEvent) {
  if (!dirty.value) return;
  e.preventDefault();
  e.returnValue = "";
}
onMounted(() => window.addEventListener("beforeunload", onBeforeUnload));
onBeforeUnmount(() => window.removeEventListener("beforeunload", onBeforeUnload));
</script>

<template>
  <section class="panel" aria-labelledby="wf-approvers-title" data-testid="wf-approvers">
    <div class="panel-header">
      <h2 id="wf-approvers-title">{{ t("wfApprovers.title") }}</h2>
      <span v-if="!dirty && problems.length" class="badge warn">{{ t("wfApprovers.warnings", { n: problems.length }) }}</span>
    </div>
    <div class="panel-body stack">
      <p class="muted no-margin">{{ t("wfApprovers.intro") }}</p>
      <div v-if="conflict" class="alert alert-warn" role="alert">
        <div>{{ t("wfApprovers.conflict") }}</div>
        <div><button type="button" class="btn btn-sm" @click="reload">{{ t("wfApprovers.reload") }}</button></div>
      </div>
      <ErrorAlert v-else-if="error" :error="error" :title="t('wfApprovers.notSaved')" />
      <div v-if="cannotListProfiles" class="alert" role="note">{{ t("wfApprovers.noProfileList") }}</div>
      <ul v-if="generalProblems.length && !dirty" class="wf-problems" :aria-label="t('wfApprovers.lint')">
        <li v-for="(p, i) in generalProblems" :key="i" :class="p.severity">
          <span :class="['badge', p.severity === 'error' ? 'danger' : 'warn']">{{ p.severity === "error" ? t("wfApproval.error") : t("wfApproval.warning") }}</span>
          {{ p.message }}
        </li>
      </ul>
    </div>
    <LoadingState v-if="loading" :label="t('wfApprovers.loading')" />
    <div v-else-if="approvers.isError.value" class="panel-body"><ErrorAlert :error="approvers.error.value" :on-retry="() => approvers.refetch()" /></div>
    <EmptyState v-else-if="rows.length === 0" :title="t('wfApprovers.empty.title')">{{ t("wfApprovers.empty.body") }}</EmptyState>
    <template v-if="!loading && approvers.data.value && rows.length">
      <div class="panel-body stack">
        <section v-for="r in rows" :key="rowKey(r)" class="wf-approver-step" :aria-labelledby="`wf-aps-${rowKey(r)}`" :data-testid="`wf-approvers-${r.transitionKey}-${r.stepKey}`">
          <h3 :id="`wf-aps-${rowKey(r)}`" class="wf-approver-step-title">
            {{ r.transitionName }} › {{ r.stepName }}
            <span class="mono muted">{{ r.transitionKey }}.{{ r.stepKey }}</span>
            <span v-if="!r.orphan" class="badge">{{ t("wfApprovers.needs", { n: r.requiredApprovals }) }}</span>
            <span v-else class="badge off" :title="t('wfApprovers.orphanHint')">{{ t("wfApprovers.orphan") }}</span>
          </h3>
          <ul v-if="!dirty && stepProblems(problems, r.transitionKey, r.stepKey).length" class="wf-problems">
            <li v-for="(p, i) in stepProblems(problems, r.transitionKey, r.stepKey)" :key="i" :class="p.severity">
              <span :class="['badge', p.severity === 'error' ? 'danger' : 'warn']">{{ p.severity === "error" ? t("wfApproval.error") : t("wfApproval.warning") }}</span>
              {{ p.message }}
            </li>
          </ul>
          <table v-if="ofRow(r).length" class="data wf-approver-table">
            <caption class="sr-only">{{ t("wfApprovers.caption", { step: r.stepName }) }}</caption>
            <thead>
              <tr>
                <th scope="col">{{ t("wfApprovers.role") }}</th>
                <th scope="col">{{ t("wfApprovers.who") }}</th>
                <th scope="col"><span class="sr-only">{{ t("wfApproval.remove") }}</span></th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="a in ofRow(r)" :key="approverIdentity(a)">
                <td>
                  <span :class="['badge', a.role === 'escalation' ? 'info' : '']">{{ t(`wfApprovers.role.${a.role}`) }}</span>
                </td>
                <td class="wrap">
                  {{ approverLabel(a) }}
                  <span v-for="(m, i) in fieldErrorsOf(a)" :key="i" class="error">{{ m }}</span>
                </td>
                <td class="row-actions">
                  <button type="button" class="btn btn-sm btn-quiet-danger" :aria-label="t('wfApprovers.removeLabel', { who: approverLabel(a), step: r.stepName })" @click="remove(a)">
                    {{ t("wfApproval.remove") }}
                  </button>
                </td>
              </tr>
            </tbody>
          </table>
          <p v-else class="muted no-margin">{{ t("wfApprovers.noneYet") }}</p>
          <ApproverAddForm :transition-key="r.transitionKey" :step-key="r.stepKey" :profiles="profiles" :attributes="personFields" :taken="taken" @add="add" />
        </section>
      </div>
    </template>
  </section>
  <ApproverPreview v-if="!loading && approvers.data.value && rows.some((r) => !r.orphan)" :workflow-id="wid" :class-id="workflow.classId" :rows="rows.filter((r) => !r.orphan)" :dirty="dirty" />
  <SaveBar v-if="!loading && approvers.data.value && rows.length" :label="t('record.save.region')" :dirty="dirty">
    <button v-if="dirty" type="button" class="btn" :disabled="save.isPending.value" @click="reset">{{ t("record.save.discard") }}</button>
    <button type="button" class="btn btn-primary" :disabled="!dirty || save.isPending.value" data-testid="wf-approvers-save" @click="submit">
      {{ save.isPending.value ? t("wfApprovers.saving") : t("wfApprovers.save") }}
    </button>
  </SaveBar>
</template>
