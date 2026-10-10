<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useCiClasses, useClassAttributes } from "../../api/queries";
import {
  APPROVAL_STATUS_TONES,
  useApprovalRequest,
  useDecideApproval,
  useWorkflowInstance,
  type ApprovalDecisionBody,
  type WorkflowApprovalRequestStep,
} from "../../api/workflowRuntime";
import CiLink from "../../components/CiLink.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import FormDialog from "../../components/FormDialog.vue";
import LoadingState from "../../components/LoadingState.vue";
import { t } from "../../i18n";
import { approvalStatusLabel, closeReasonLabel, decisionProblem, refusalMessage } from "../../lib/approvalRuntime";
import { formatDateTime, formatRelative } from "../../lib/format";
import { useFlashStore } from "../../stores/flash";
import FormField from "../form/FormField.vue";
import ChangeValue from "../imports/ChangeValue.vue";

/**
 * One approval request (GET /workflow-approval-requests/{id}): what it would change, its steps and every
 * decision so far, and, when the caller may decide the active step (`myEligibility.canDecide`), the decision:
 * approve or reject, a comment (required to reject) and, for a delegate, whom they decide for. Anyone else sees
 * the request read-only, with the reason they cannot decide it in plain words: the requester gets no approve
 * button (four-eyes). A refused decision is explained the same way; a stale request (the CI changed since it
 * was made) says that nothing was recorded and what to do instead.
 */
const props = defineProps<{ open: boolean; requestId: string | null }>();
const emit = defineEmits<{ close: [] }>();

const flash = useFlashStore();
const q = useApprovalRequest(() => (props.open ? (props.requestId ?? undefined) : undefined));
const r = computed(() => q.data.value);
// The instance's version graph names the states and transitions the request refers to by key.
const inst = useWorkflowInstance(() => r.value?.instanceId);
const stateName = (key: string) => inst.data.value?.graph.states.find((s) => s.key === key)?.name ?? key;
const transitionName = (key: string) => inst.data.value?.graph.transitions.find((x) => x.key === key)?.name ?? key;
const classes = useCiClasses();
const classId = computed(() => classes.data.value?.find((c) => c.key === r.value?.classKey)?.id);
const attrs = useClassAttributes(() => classId.value, { includeInactive: true });
const staged = computed(() =>
  Object.entries((r.value?.stagedFields ?? {}) as Record<string, unknown>).map(([key, value]) => {
    const def = attrs.data.value?.find((a) => a.key === key);
    return { key, label: def?.label ?? key, def, value };
  }),
);

const pending = computed(() => r.value?.status === "pending");
const elig = computed(() => r.value?.myEligibility);
const canDecide = computed(() => pending.value && !!elig.value?.canDecide);
const activeStep = computed(() => r.value?.steps.find((s) => s.stepNo === r.value?.currentStepNo));

const decide = useDecideApproval();
const decision = ref<"approve" | "reject">("approve");
/** "" decides in person; otherwise the principal's user id. */
const onBehalf = ref("");
const comment = ref("");
const commentMissing = ref(false);
watch(
  () => [props.open, props.requestId] as const,
  ([open]) => {
    if (!open) return;
    decision.value = "approve";
    comment.value = "";
    commentMissing.value = false;
    decide.reset();
  },
  { immediate: true },
);
// In person when the caller qualifies in person, otherwise for the first principal (the API defaults the same way).
watch(
  elig,
  (e) => {
    if (e) onBehalf.value = e.inPerson ? "" : (e.onBehalfOf[0]?.userId ?? "");
  },
  { immediate: true },
);

const problem = computed(() => decisionProblem(decide.error.value, transitionName));
const commentError = computed(() => {
  if (commentMissing.value) return t("approvalRun.decide.commentMissing");
  return problem.value?.kind === "comment" ? problem.value.message : undefined;
});

async function submit() {
  const req = r.value;
  const step = activeStep.value;
  if (!req || !step || !canDecide.value) return;
  commentMissing.value = decision.value === "reject" && !comment.value.trim();
  if (commentMissing.value) return;
  const body: ApprovalDecisionBody = { stepKey: step.key, decision: decision.value, expectedVersion: req.version, comment: comment.value.trim() || undefined };
  if (onBehalf.value) body.onBehalfOf = onBehalf.value;
  try {
    const out = await decide.mutateAsync({ id: req.id, ciId: req.ciId, body });
    const p = { transition: req.transitionName, ci: req.ciLabel, step: step.name, state: out.instance.state.name };
    if (decision.value === "reject") flash.show(t("approvalRun.decided.rejected", p));
    else if (out.request.status === "approved") flash.show(t("approvalRun.decided.applied", p));
    else flash.show(t("approvalRun.decided.approved", p));
    emit("close");
  } catch {
    // shown in the dialog
  }
}
function reload() {
  decide.reset();
  void q.refetch();
}

const stepStatus = (s: WorkflowApprovalRequestStep) => t(`approvalRun.stepStatus.${s.status}`);
const title = computed(() => (r.value ? t("approvalRun.dialog.title", { transition: r.value.transitionName, ci: r.value.ciLabel }) : t("approvalRun.dialog.loading")));
</script>

<template>
  <FormDialog
    :open="open"
    :title="title"
    :submit-label="decision === 'reject' ? t('approvalRun.decide.rejectSubmit') : t('approvalRun.decide.approveSubmit')"
    :busy="decide.isPending.value"
    :readonly="!canDecide"
    wide
    @submit="submit"
    @cancel="emit('close')"
  >
    <LoadingState v-if="q.isLoading.value" :label="t('approvalRun.dialog.loading')" />
    <ErrorAlert v-else-if="q.isError.value" :error="q.error.value" :title="t('approvalRun.dialog.loadFailed')" :on-retry="() => q.refetch()" />
    <template v-else-if="r">
      <p class="wf-transition-route">
        <span :class="['badge', APPROVAL_STATUS_TONES[r.status]]" data-testid="approval-status">{{ approvalStatusLabel(r.status) }}</span>
        <span>{{ t("approvalRun.dialog.route", { from: stateName(r.fromState), to: stateName(r.toState), no: r.requestNo }) }}</span>
        <CiLink :id="r.ciId">{{ r.ciLabel }}</CiLink>
        <span class="muted mono">{{ r.ciIdent }}</span>
      </p>

      <div v-if="pending && (!r.requester.active || !r.requester.stillAuthorized)" class="alert alert-warn" data-testid="approval-requester-warning">
        <strong>{{ !r.requester.active ? t("approvalRun.requester.inactive") : t("approvalRun.requester.unauthorized") }}</strong>
        <div>{{ t("approvalRun.requester.advice") }}</div>
      </div>

      <dl class="props">
        <dt>{{ t("approvalRun.dialog.requested") }}</dt>
        <dd>{{ t("approvalRun.dialog.requestedBy", { name: r.requester.name, when: formatDateTime(r.requestedAt) }) }}</dd>
        <dt>{{ t("approvalRun.dialog.comment") }}</dt>
        <dd>
          <span v-if="r.comment" class="wf-comment" dir="auto">{{ r.comment }}</span><span v-else class="muted">{{ t("approvalRun.none") }}</span>
        </dd>
        <dt>{{ t("approvalRun.dialog.changes") }}</dt>
        <dd>
          <ul v-if="staged.length > 0" class="diff">
            <li v-for="f in staged" :key="f.key">
              <span :title="f.key">{{ f.label }}</span>: <ins dir="auto"><ChangeValue :def="f.def" :value="f.value" /></ins>
            </li>
          </ul>
          <span v-else class="muted">{{ t("approvalRun.dialog.noChanges", { to: stateName(r.toState) }) }}</span>
        </dd>
        <template v-if="r.closedAt">
          <dt>{{ t("approvalRun.dialog.closed") }}</dt>
          <dd>
            {{ formatDateTime(r.closedAt) }}<template v-if="r.closedByName">, {{ r.closedByName }}</template>
            <template v-if="closeReasonLabel(r.closeReason)"> ({{ closeReasonLabel(r.closeReason) }})</template>
          </dd>
        </template>
      </dl>

      <div class="table-wrap">
        <table class="data" :aria-label="t('approvalRun.steps.label')">
          <thead>
            <tr>
              <th scope="col">{{ t("approvalRun.steps.step") }}</th>
              <th scope="col">{{ t("approvalRun.steps.status") }}</th>
              <th scope="col" class="num">{{ t("approvalRun.steps.approvals") }}</th>
              <th scope="col">{{ t("approvalRun.steps.due") }}</th>
              <th scope="col" style="width: 100%">{{ t("approvalRun.steps.decisions") }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="s in r.steps" :key="s.key" :aria-current="s.status === 'active' ? 'step' : undefined" style="vertical-align: top">
              <td dir="auto">{{ s.stepNo }}. {{ s.name }}</td>
              <td>
                {{ stepStatus(s) }}
                <span v-if="s.overdue" class="badge warn">{{ t("approvalRun.overdue") }}</span>
                <span v-if="s.understaffed" class="badge warn" :title="t('approvalRun.steps.understaffedHint')">{{ t("approvalRun.steps.understaffed") }}</span>
              </td>
              <td class="num">{{ t("approvalRun.steps.count", { n: Number(s.approvals), required: s.requiredApprovals }) }}</td>
              <td :title="formatDateTime(s.dueAt)">{{ s.dueAt ? formatRelative(s.dueAt) : "–" }}</td>
              <td style="white-space: normal">
                <ul v-if="s.decisions.length > 0" class="wf-decisions">
                  <li v-for="d in s.decisions" :key="d.id">
                    <strong>{{ d.decision === "approve" ? t("approvalRun.decision.approved") : t("approvalRun.decision.rejected") }}</strong>
                    {{ t("approvalRun.decision.by", { name: d.actorName }) }}
                    <template v-if="d.onBehalfOfName">{{ t("approvalRun.decision.for", { name: d.onBehalfOfName }) }}</template>
                    <template v-if="d.credential === 'token'"> {{ t("approvalRun.decision.token") }}</template>,
                    <span :title="formatDateTime(d.decidedAt)">{{ formatRelative(d.decidedAt) }}</span>
                    <div v-if="d.comment" class="wf-comment" dir="auto">{{ d.comment }}</div>
                  </li>
                </ul>
                <span v-else class="muted">–</span>
              </td>
            </tr>
          </tbody>
        </table>
      </div>

      <template v-if="pending">
        <div v-if="!canDecide" class="alert" data-testid="approval-cannot-decide">
          <strong>{{ t("approvalRun.cannotDecide") }}</strong>
          <div>{{ refusalMessage(elig?.reason, elig?.message, transitionName) }}</div>
        </div>
        <template v-else>
          <div v-if="problem?.kind === 'stale'" class="alert alert-error" role="alert" data-testid="approval-stale">
            <strong>{{ t("approvalRun.stale.title") }}</strong>
            <div>{{ t("approvalRun.stale.body") }}</div>
            <ul v-if="problem.details.length > 0">
              <li v-for="(m, i) in problem.details" :key="i">{{ m }}</li>
            </ul>
            <div>{{ t("approvalRun.stale.advice") }}</div>
          </div>
          <div v-else-if="problem?.kind === 'refused'" class="alert alert-error" role="alert" data-testid="approval-refused">
            <strong>{{ t("approvalRun.refused.title") }}</strong>
            <div>{{ problem.message }}</div>
          </div>
          <div v-else-if="problem?.kind === 'moved'" class="alert alert-warn" role="alert">
            <strong>{{ t("approvalRun.moved.title") }}</strong>
            <div>
              {{ problem.message }} <button type="button" class="btn btn-sm" @click="reload">{{ t("approvalRun.moved.reload") }}</button>
            </div>
          </div>
          <ErrorAlert v-else-if="problem?.kind === 'other'" :error="decide.error.value" :title="t('approvalRun.decide.failed')" />

          <div class="form-grid">
            <fieldset class="field">
              <legend>{{ t("approvalRun.decide.legend", { step: activeStep?.name ?? "" }) }}</legend>
              <label class="checkbox-row"><input v-model="decision" type="radio" name="approval-decision" value="approve" /> {{ t("approvalRun.decide.approve") }}</label>
              <label class="checkbox-row"><input v-model="decision" type="radio" name="approval-decision" value="reject" /> {{ t("approvalRun.decide.reject") }}</label>
            </fieldset>
            <FormField
              v-if="elig && elig.onBehalfOf.length > 0"
              id="approval-on-behalf"
              v-slot="p"
              :label="t('approvalRun.decide.onBehalf')"
              :hint="t('approvalRun.decide.onBehalfHint')"
            >
              <select :id="p.id" v-model="onBehalf" :aria-describedby="p.describedBy">
                <option v-if="elig.inPerson" value="">{{ t("approvalRun.decide.inPerson") }}</option>
                <option v-for="o in elig.onBehalfOf" :key="o.userId" :value="o.userId">{{ t("approvalRun.decide.forPrincipal", { name: o.name }) }}</option>
              </select>
            </FormField>
            <FormField
              id="approval-comment"
              v-slot="p"
              :label="t('approvalRun.decide.comment')"
              :required="decision === 'reject'"
              :error="commentError"
              :hint="decision === 'reject' ? t('approvalRun.decide.commentRejectHint') : t('approvalRun.decide.commentHint')"
              wide
            >
              <textarea :id="p.id" v-model="comment" rows="3" maxlength="4000" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
            </FormField>
          </div>
          <p v-if="decision === 'approve' && activeStep && activeStep.stepNo === r.steps.length && Number(activeStep.approvals) + 1 >= activeStep.requiredApprovals" class="hint">
            {{ t("approvalRun.decide.finalHint", { transition: r.transitionName, to: stateName(r.toState) }) }}
          </p>
        </template>
      </template>
    </template>
  </FormDialog>
</template>
