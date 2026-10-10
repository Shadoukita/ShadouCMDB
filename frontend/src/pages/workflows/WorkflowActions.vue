<script setup lang="ts">
import { computed, ref } from "vue";
import { ApiError } from "../../api/client";
import type { Ci } from "../../api/queries";
import { useCancelWorkflow, type WorkflowAvailableTransition, type WorkflowInstance } from "../../api/workflowRuntime";
import ErrorAlert from "../../components/ErrorAlert.vue";
import FormDialog from "../../components/FormDialog.vue";
import Icon from "../../components/Icon.vue";
import { useFlashStore } from "../../stores/flash";
import FormField from "../form/FormField.vue";
import TransitionDialog from "./TransitionDialog.vue";

/**
 * What the caller may do with a running instance: the transitions they are granted (a blocked one is offered
 * too, marked, with its reasons; the dialog lists them) and cancelling it. The API leaves out transitions the
 * caller is not granted, so an operator without a grant sees none. `reload` asks the page to load the
 * instance again after a version conflict. While an approval request is pending the API refuses every
 * transition (409 WORKFLOW_APPROVAL_PENDING), so none is offered: the pending banner shows the request.
 */
const props = defineProps<{
  instance: WorkflowInstance;
  transitions: WorkflowAvailableTransition[];
  canCancel: boolean;
  classId?: string;
  ci?: Ci;
  compact?: boolean;
}>();
const emit = defineEmits<{ reload: [] }>();

const flash = useFlashStore();
const runnable = computed(() => (props.instance.pendingApproval ? [] : props.transitions));
const running = ref<WorkflowAvailableTransition | null>(null);
const cancelling = ref(false);
const reason = ref("");
const cancel = useCancelWorkflow();
const cancelError = computed(() => (cancel.error.value instanceof ApiError ? cancel.error.value : null));
const reasonMissing = ref(false);
const reasonError = computed(() => {
  if (reasonMissing.value) return "Give a reason.";
  if (cancelError.value?.code === "VALIDATION_ERROR") return cancelError.value.fieldErrors().reason;
  return undefined;
});
const blockedTitle = (t: WorkflowAvailableTransition) =>
  t.blockedBy.length > 0 ? `Cannot run as the CI stands: ${t.blockedBy.map((b) => b.message).join("; ")}` : `Move to ${t.toState.name}`;

function openCancel() {
  reason.value = "";
  reasonMissing.value = false;
  cancel.reset();
  cancelling.value = true;
}
async function confirmCancel() {
  reasonMissing.value = !reason.value.trim();
  if (reasonMissing.value) return;
  try {
    await cancel.mutateAsync({ id: props.instance.id, ciId: props.instance.ciId, expectedVersion: props.instance.version, reason: reason.value.trim() });
    flash.show(`Cancelled ${props.instance.definitionName} on ${props.instance.ciLabel}.`);
    cancelling.value = false;
  } catch {
    // shown in the dialog
  }
}
function reload() {
  running.value = null;
  cancelling.value = false;
  emit("reload");
}
</script>

<template>
  <div class="wf-actions">
    <button
      v-for="t in runnable"
      :key="t.key"
      type="button"
      :class="['btn', compact ? 'btn-sm' : '', t.blockedBy.length > 0 ? 'wf-blocked' : '']"
      :title="blockedTitle(t)"
      :data-testid="`wf-transition-${t.key}`"
      @click="running = t"
    >
      <Icon v-if="t.blockedBy.length > 0" name="triangle-alert" :size="14" />
      {{ t.name }}
      <span v-if="t.blockedBy.length > 0" class="sr-only">(blocked: {{ t.blockedBy.map((b) => b.message).join("; ") }})</span>
    </button>
    <button v-if="canCancel" type="button" :class="['btn', 'btn-ghost', compact ? 'btn-sm' : '']" @click="openCancel">Cancel workflow</button>
    <span v-if="runnable.length === 0 && !instance.pendingApproval && !canCancel && !compact" class="muted">No transition you may run from this state.</span>

    <Teleport to="body">
      <TransitionDialog :open="!!running" :instance="instance" :transition="running" :class-id="classId" :ci="ci" @close="running = null" @reload="reload" />
    <FormDialog
      :open="cancelling"
      :title="`Cancel ${instance.definitionName}?`"
      submit-label="Cancel workflow"
      :busy="cancel.isPending.value"
      @submit="confirmCancel"
      @cancel="cancelling = false"
    >
      <div v-if="cancelError?.code === 'VERSION_CONFLICT'" class="alert alert-warn" role="alert">
        <strong>This workflow moved on since you opened it.</strong>
        <div>Nothing was cancelled. <button type="button" class="btn btn-sm" @click="reload">Reload the workflow</button></div>
      </div>
      <ErrorAlert v-else-if="cancel.isError.value && !reasonError" :error="cancel.error.value" title="The workflow was not cancelled" />
      <p>
        The workflow stops in <strong dir="auto">{{ instance.state.name }}</strong> on <strong dir="auto">{{ instance.ciLabel }}</strong>. The CI and its
        fields stay as they are; the instance and its history are kept.
      </p>
      <FormField id="wf-cancel-reason" v-slot="p" label="Reason" required :error="reasonError" hint="Kept in the workflow history and the audit log.">
        <textarea :id="p.id" v-model="reason" rows="3" maxlength="4000" required :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
      </FormField>
    </FormDialog>
    </Teleport>
  </div>
</template>
