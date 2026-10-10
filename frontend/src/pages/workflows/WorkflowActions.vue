<script setup lang="ts">
import { computed, ref } from "vue";
import { ApiError } from "../../api/client";
import type { Ci } from "../../api/queries";
import { useCancelWorkflow, type WorkflowAvailableTransition, type WorkflowInstance } from "../../api/workflowRuntime";
import ErrorAlert from "../../components/ErrorAlert.vue";
import FormDialog from "../../components/FormDialog.vue";
import Icon from "../../components/Icon.vue";
import { t } from "../../i18n";
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
  if (reasonMissing.value) return t("wfRun.reasonMissing");
  if (cancelError.value?.code === "VALIDATION_ERROR") return cancelError.value.fieldErrors().reason;
  return undefined;
});
const blockedReasons = (tr: WorkflowAvailableTransition) => tr.blockedBy.map((b) => b.message).join("; ");
const blockedTitle = (tr: WorkflowAvailableTransition) =>
  tr.blockedBy.length > 0 ? t("wfRun.actions.blockedTitle", { reasons: blockedReasons(tr) }) : t("wfRun.actions.moveTo", { state: tr.toState.name });

/** The cancel text around the state and the CI, which are bold. Both catalogs name the state before the CI. */
const cancelBody = computed(() => {
  const [before = "", middle = "", after = ""] = t("wfRun.cancel.body", { state: "\u0000", ci: "\u0000" }).split("\u0000");
  return [before, middle, after];
});
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
    flash.show(t("wfRun.cancel.done", { workflow: props.instance.definitionName, ci: props.instance.ciLabel }));
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
      v-for="tr in runnable"
      :key="tr.key"
      type="button"
      :class="['btn', compact ? 'btn-sm' : '', tr.blockedBy.length > 0 ? 'wf-blocked' : '']"
      :title="blockedTitle(tr)"
      :data-testid="`wf-transition-${tr.key}`"
      @click="running = tr"
    >
      <Icon v-if="tr.blockedBy.length > 0" name="triangle-alert" :size="14" />
      <span dir="auto">{{ tr.name }}</span>
      <span v-if="tr.blockedBy.length > 0" class="sr-only">{{ t("wfRun.actions.blockedSr", { reasons: blockedReasons(tr) }) }}</span>
    </button>
    <button v-if="canCancel" type="button" :class="['btn', 'btn-ghost', compact ? 'btn-sm' : '']" @click="openCancel">{{ t("wfRun.cancel.submit") }}</button>
    <span v-if="runnable.length === 0 && !instance.pendingApproval && !canCancel && !compact" class="muted">{{ t("wfRun.actions.none") }}</span>

    <Teleport to="body">
      <TransitionDialog :open="!!running" :instance="instance" :transition="running" :class-id="classId" :ci="ci" @close="running = null" @reload="reload" />
    <FormDialog
      :open="cancelling"
      :title="t('wfRun.cancel.title', { workflow: instance.definitionName })"
      :submit-label="t('wfRun.cancel.submit')"
      :busy="cancel.isPending.value"
      @submit="confirmCancel"
      @cancel="cancelling = false"
    >
      <div v-if="cancelError?.code === 'VERSION_CONFLICT'" class="alert alert-warn" role="alert">
        <strong>{{ t("wfRun.conflict.title") }}</strong>
        <div>
          {{ t("wfRun.conflict.nothingCancelled") }} <button type="button" class="btn btn-sm" @click="reload">{{ t("wfRun.conflict.reload") }}</button>
        </div>
      </div>
      <ErrorAlert v-else-if="cancel.isError.value && !reasonError" :error="cancel.error.value" :title="t('wfRun.cancel.failed')" />
      <p>
        {{ cancelBody[0] }}<strong dir="auto">{{ instance.state.name }}</strong>{{ cancelBody[1] }}<strong dir="auto">{{ instance.ciLabel }}</strong>{{ cancelBody[2] }}
      </p>
      <FormField id="wf-cancel-reason" v-slot="p" :label="t('wfRun.reason')" required :error="reasonError" :hint="t('wfRun.cancel.reasonHint')">
        <textarea :id="p.id" v-model="reason" rows="3" maxlength="4000" required :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
      </FormField>
    </FormDialog>
    </Teleport>
  </div>
</template>
