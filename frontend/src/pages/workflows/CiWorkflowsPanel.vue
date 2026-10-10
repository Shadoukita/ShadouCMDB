<script setup lang="ts">
import { computed, ref } from "vue";
import { RouterLink } from "vue-router";
import { ApiError } from "../../api/client";
import type { Ci } from "../../api/queries";
import { STATUS_LABELS, STATUS_TONES, useCiWorkflows, useStartWorkflow } from "../../api/workflowRuntime";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import FormDialog from "../../components/FormDialog.vue";
import LoadingState from "../../components/LoadingState.vue";
import { formatDateTime, formatRelative } from "../../lib/format";
import { t } from "../../i18n";
import { useFlashStore } from "../../stores/flash";
import FormField from "../form/FormField.vue";
import PendingApprovalBanner from "./PendingApprovalBanner.vue";
import WorkflowActions from "./WorkflowActions.vue";
import WorkflowStateBadge from "./WorkflowStateBadge.vue";

/**
 * The CI's Workflows tab (GET /configuration-items/{id}/workflows): its running instances with the transitions
 * the caller may run, then the ones that ended last; and the workflows of its type the caller may start.
 */
const props = defineProps<{ ci: Ci }>();
const flash = useFlashStore();
const wf = useCiWorkflows(() => props.ci.id);
const rows = computed(() => wf.data.value?.data ?? []);
const startable = computed(() => wf.data.value?.startable ?? []);
/** Running instances waiting for an approval: a banner each above the table. */
const pending = computed(() =>
  rows.value.flatMap((v) => (v.instance.status === "active" && v.instance.pendingApproval ? [{ ...v.instance, pendingApproval: v.instance.pendingApproval }] : [])),
);

const starting = ref(false);
const startId = ref("");
const startComment = ref("");
const start = useStartWorkflow();
function openStart() {
  startId.value = startable.value[0]?.definitionId ?? "";
  startComment.value = "";
  start.reset();
  starting.value = true;
}
async function confirmStart() {
  const d = startable.value.find((s) => s.definitionId === startId.value);
  if (!d) return;
  try {
    await start.mutateAsync({ ciId: props.ci.id, definitionId: d.definitionId, comment: startComment.value.trim() });
    flash.show(`Started ${d.definitionName} on ${props.ci.label}.`);
    starting.value = false;
  } catch {
    // shown in the dialog
  }
}
const startCommentError = computed(() =>
  start.error.value instanceof ApiError && start.error.value.code === "VALIDATION_ERROR" ? start.error.value.fieldErrors().comment : undefined,
);
</script>

<template>
  <section class="panel" aria-labelledby="ci-wf-title" data-testid="ci-workflows">
    <div class="panel-header">
      <h2 id="ci-wf-title">Workflows</h2>
      <button v-if="startable.length > 0" type="button" class="btn btn-sm" @click="openStart">Start workflow</button>
    </div>
    <LoadingState v-if="wf.isLoading.value" label="Loading workflows…" />
    <div v-else-if="wf.isError.value" class="panel-body">
      <ErrorAlert :error="wf.error.value" title="Could not load this CI's workflows" :on-retry="() => wf.refetch()" />
    </div>
    <EmptyState v-else-if="rows.length === 0" title="No workflow has run on this CI">
      <template v-if="startable.length > 0">Start one of the workflows of its type to move it through its states.</template>
      <template v-else-if="ci.deletedAt">A deleted CI runs no workflows.</template>
      <template v-else>No active workflow runs on this CI's type, or you may not start one (that needs the edit right on the type).</template>
      <template v-if="startable.length > 0" #actions><button type="button" class="btn btn-primary" @click="openStart">Start workflow</button></template>
    </EmptyState>
    <div v-if="!wf.isLoading.value && !wf.isError.value && pending.length > 0" class="panel-body">
      <PendingApprovalBanner v-for="i in pending" :key="i.id" :instance="i" @reload="wf.refetch()" />
    </div>
    <div v-if="!wf.isLoading.value && !wf.isError.value && rows.length > 0" class="table-wrap">
      <table class="data">
        <thead>
          <tr>
            <th scope="col">Workflow</th>
            <th scope="col">State</th>
            <th scope="col">Status</th>
            <th scope="col">Started</th>
            <th scope="col">Last step</th>
            <th scope="col" style="width: 100%">Actions</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="v in rows" :key="v.instance.id" :class="{ disabled: v.instance.status !== 'active' }">
            <td>
              <RouterLink :to="`/workflows/${v.instance.id}`" dir="auto">{{ v.instance.definitionName }}</RouterLink>
              <span class="muted"> v{{ v.instance.versionNo }}</span>
            </td>
            <td><WorkflowStateBadge :state="v.instance.state" /></td>
            <td><span :class="['badge', STATUS_TONES[v.instance.status]]">{{ STATUS_LABELS[v.instance.status] }}</span></td>
            <td :title="formatDateTime(v.instance.startedAt)">{{ formatRelative(v.instance.startedAt) }} by {{ v.instance.startedByName }}</td>
            <td :title="formatDateTime(v.instance.lastTransitionAt)">
              {{ v.instance.endedAt ? `Ended ${formatRelative(v.instance.endedAt)}` : formatRelative(v.instance.lastTransitionAt) }}
            </td>
            <td style="white-space: normal">
              <WorkflowActions
                v-if="v.instance.status === 'active'"
                :instance="v.instance"
                :transitions="v.availableTransitions"
                :can-cancel="v.canCancel"
                :class-id="ci.classId"
                :ci="ci"
                compact
                @reload="wf.refetch()"
              />
              <span v-if="v.instance.pendingApproval" class="muted">{{ t("approvalRun.awaiting") }}</span>
              <span v-else-if="v.instance.status === 'active' && v.availableTransitions.length === 0 && !v.canCancel" class="muted"
                >No transition you may run</span
              >
            </td>
          </tr>
        </tbody>
      </table>
    </div>

    <Teleport to="body">
      <FormDialog :open="starting" title="Start a workflow" submit-label="Start" :busy="start.isPending.value" @submit="confirmStart" @cancel="starting = false">
        <ErrorAlert v-if="start.isError.value && !startCommentError" :error="start.error.value" title="The workflow was not started" />
        <FormField id="wf-start-def" v-slot="p" label="Workflow" required>
          <select :id="p.id" v-model="startId">
            <option v-for="s in startable" :key="s.definitionId" :value="s.definitionId">{{ s.definitionName }} (v{{ s.versionNo }})</option>
          </select>
        </FormField>
        <FormField id="wf-start-comment" v-slot="p" label="Comment" :error="startCommentError" hint="Optional; kept in the workflow history.">
          <textarea :id="p.id" v-model="startComment" rows="3" maxlength="4000" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
        </FormField>
        <p class="hint">The workflow starts in its initial state on {{ ci.label }}. If it drives a state field, that field is set.</p>
      </FormDialog>
    </Teleport>
  </section>
</template>
