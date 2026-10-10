<script setup lang="ts">
import { computed, ref } from "vue";
import { ApiError } from "../../api/client";
import { useApprovalRequest, useWithdrawApproval, useWorkflowInstance, type WorkflowInstance, type WorkflowPendingApproval } from "../../api/workflowRuntime";
import ErrorAlert from "../../components/ErrorAlert.vue";
import FormDialog from "../../components/FormDialog.vue";
import { t } from "../../i18n";
import { pendingProgress } from "../../lib/approvalRuntime";
import { formatDateTime, formatRelative } from "../../lib/format";
import { useFlashStore } from "../../stores/flash";
import { useSessionStore } from "../../stores/session";
import FormField from "../form/FormField.vue";
import ApprovalRequestDialog from "./ApprovalRequestDialog.vue";

/**
 * The approval request an instance waits for (`pendingApproval`): which transition, the active step's progress
 * and due date, and what the caller may do: decide it when they may, open it read-only otherwise, and withdraw
 * it when they made it. The request itself (GET /workflow-approval-requests/{id}) says who made it and whether
 * the caller may decide; until it loads, the banner shows the instance's own summary.
 */
const props = defineProps<{ instance: WorkflowInstance & { pendingApproval: WorkflowPendingApproval } }>();
const emit = defineEmits<{ reload: [] }>();

const session = useSessionStore();
const flash = useFlashStore();
const p = computed(() => props.instance.pendingApproval);
const req = useApprovalRequest(() => p.value.requestId);
// The pending request names its transition and target state by key: the version graph has their names.
const detail = useWorkflowInstance(() => props.instance.id);
const graph = computed(() => detail.data.value?.graph);
const transition = computed(() => req.data.value?.transitionName ?? graph.value?.transitions.find((x) => x.key === p.value.transitionKey)?.name ?? p.value.transitionKey);
const toState = computed(() => graph.value?.states.find((s) => s.key === p.value.toState)?.name ?? p.value.toState);
const canDecide = computed(() => !!req.data.value?.myEligibility.canDecide);
const mine = computed(() => !!session.user && req.data.value?.requester.id === session.user.id);

const reviewing = ref(false);
const withdrawing = ref(false);
const comment = ref("");
const withdraw = useWithdrawApproval();
const withdrawError = computed(() => (withdraw.error.value instanceof ApiError ? withdraw.error.value : null));
function openWithdraw() {
  comment.value = "";
  withdraw.reset();
  withdrawing.value = true;
}
async function confirmWithdraw() {
  const r = req.data.value;
  if (!r) return;
  try {
    await withdraw.mutateAsync({ id: r.id, ciId: r.ciId, expectedVersion: r.version, comment: comment.value.trim() });
    flash.show(t("approvalRun.withdraw.done", { transition: r.transitionName, ci: r.ciLabel }));
    withdrawing.value = false;
  } catch {
    // shown in the dialog
  }
}
function reload() {
  withdrawing.value = false;
  void req.refetch();
  emit("reload");
}
</script>

<template>
  <div :class="['alert', 'wf-pending', p.overdue ? 'alert-warn' : '']" data-testid="wf-pending-approval">
    <div class="wf-pending-text">
      <strong>{{ t("approvalRun.banner.title", { workflow: instance.definitionName, transition, to: toState }) }}</strong>
      <div>
        {{ pendingProgress(p) }}.
        <template v-if="p.dueAt">
          <span v-if="p.overdue" class="badge warn">{{ t("approvalRun.overdue") }}</span>
          <span :title="formatDateTime(p.dueAt)">{{ p.overdue ? t("approvalRun.banner.dueWas", { when: formatRelative(p.dueAt) }) : t("approvalRun.banner.due", { when: formatRelative(p.dueAt) }) }}</span>
        </template>
        <template v-if="req.data.value"> {{ t("approvalRun.banner.requestedBy", { name: req.data.value.requester.name }) }}</template>
      </div>
    </div>
    <div class="wf-pending-actions">
      <button type="button" :class="['btn', 'btn-sm', canDecide ? 'btn-primary' : '']" data-testid="wf-pending-open" @click="reviewing = true">
        {{ canDecide ? t("approvalRun.banner.decide") : t("approvalRun.banner.view") }}
      </button>
      <button v-if="mine" type="button" class="btn btn-sm btn-ghost" data-testid="wf-pending-withdraw" @click="openWithdraw">{{ t("approvalRun.withdraw.action") }}</button>
    </div>

    <Teleport to="body">
      <ApprovalRequestDialog :open="reviewing" :request-id="p.requestId" @close="reviewing = false" />
      <FormDialog
        :open="withdrawing"
        :title="t('approvalRun.withdraw.title', { transition })"
        :submit-label="t('approvalRun.withdraw.action')"
        :busy="withdraw.isPending.value"
        @submit="confirmWithdraw"
        @cancel="withdrawing = false"
      >
        <div v-if="withdrawError?.code === 'VERSION_CONFLICT' || withdrawError?.code === 'CONFLICT'" class="alert alert-warn" role="alert">
          <strong>{{ t("approvalRun.moved.title") }}</strong>
          <div>
            {{ t("approvalRun.withdraw.moved") }} <button type="button" class="btn btn-sm" @click="reload">{{ t("approvalRun.moved.reload") }}</button>
          </div>
        </div>
        <ErrorAlert v-else-if="withdraw.isError.value" :error="withdraw.error.value" :title="t('approvalRun.withdraw.failed')" />
        <p>{{ t("approvalRun.withdraw.body", { ci: instance.ciLabel, state: instance.state.name }) }}</p>
        <FormField id="approval-withdraw-comment" v-slot="f" :label="t('approvalRun.decide.comment')" :hint="t('approvalRun.withdraw.commentHint')">
          <textarea :id="f.id" v-model="comment" rows="3" maxlength="4000" :aria-describedby="f.describedBy" />
        </FormField>
      </FormDialog>
    </Teleport>
  </div>
</template>
