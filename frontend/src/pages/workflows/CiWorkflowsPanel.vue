<script setup lang="ts">
import { computed, ref } from "vue";
import { RouterLink } from "vue-router";
import { ApiError } from "../../api/client";
import type { Ci } from "../../api/queries";
import { useCiWorkflows, useStartWorkflow } from "../../api/workflowRuntime";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import FormDialog from "../../components/FormDialog.vue";
import Icon from "../../components/Icon.vue";
import SkeletonRows from "../../components/SkeletonRows.vue";
import { t } from "../../i18n";
import { formatDateTime, formatRelative } from "../../lib/format";
import { useFlashStore } from "../../stores/flash";
import FormField from "../form/FormField.vue";
import WorkflowActions from "./WorkflowActions.vue";
import WorkflowStateBadge from "./WorkflowStateBadge.vue";
import WorkflowStatusBadge from "./WorkflowStatusBadge.vue";

/**
 * The CI's Workflows tab (GET /configuration-items/{id}/workflows): its running instances with the transitions
 * the caller may run, then the ones that ended last; and the workflows of its type the caller may start.
 */
const props = defineProps<{ ci: Ci }>();
const flash = useFlashStore();
const wf = useCiWorkflows(() => props.ci.id);
const rows = computed(() => wf.data.value?.data ?? []);
const startable = computed(() => wf.data.value?.startable ?? []);

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
    flash.show(t("wfRun.start.done", { workflow: d.definitionName, ci: props.ci.label }));
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
      <h2 id="ci-wf-title">{{ t("wfRun.list.title") }}</h2>
      <button v-if="startable.length > 0" type="button" class="btn btn-sm" @click="openStart"><Icon name="plus" />{{ t("wfRun.start.open") }}</button>
    </div>
    <SkeletonRows v-if="wf.isLoading.value" :label="t('wfRun.panel.loading')" :rows="3" />
    <div v-else-if="wf.isError.value" class="panel-body">
      <ErrorAlert :error="wf.error.value" :title="t('wfRun.panel.failed')" :on-retry="() => wf.refetch()" />
    </div>
    <EmptyState v-else-if="rows.length === 0" icon="circle-check" :title="t('wfRun.panel.empty.title')">
      <template v-if="startable.length > 0">{{ t("wfRun.panel.empty.start") }}</template>
      <template v-else-if="ci.deletedAt">{{ t("wfRun.panel.empty.deleted") }}</template>
      <template v-else>{{ t("wfRun.panel.empty.none") }}</template>
      <template v-if="startable.length > 0" #actions>
        <button type="button" class="btn btn-primary" @click="openStart"><Icon name="plus" />{{ t("wfRun.start.open") }}</button>
      </template>
    </EmptyState>
    <div v-else class="table-wrap">
      <table class="data">
        <thead>
          <tr>
            <th scope="col">{{ t("wfRun.col.workflow") }}</th>
            <th scope="col">{{ t("wfRun.col.state") }}</th>
            <th scope="col">{{ t("wfRun.col.status") }}</th>
            <th scope="col">{{ t("wfRun.col.started") }}</th>
            <th scope="col">{{ t("wfRun.col.lastStep") }}</th>
            <th scope="col" class="wf-fill">{{ t("wfRun.col.actions") }}</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="v in rows" :key="v.instance.id" :class="{ disabled: v.instance.status !== 'active' }">
            <td>
              <RouterLink :to="`/workflows/${v.instance.id}`" dir="auto">{{ v.instance.definitionName }}</RouterLink>
              <span class="muted mono"> v{{ v.instance.versionNo }}</span>
            </td>
            <td><WorkflowStateBadge :state="v.instance.state" /></td>
            <td><WorkflowStatusBadge :status="v.instance.status" /></td>
            <td>
              <time :datetime="v.instance.startedAt" :title="formatDateTime(v.instance.startedAt)">{{
                t("wfRun.instance.startedBy", { when: formatRelative(v.instance.startedAt), name: v.instance.startedByName })
              }}</time>
            </td>
            <td>
              <time
                :datetime="v.instance.endedAt ?? v.instance.lastTransitionAt"
                :title="formatDateTime(v.instance.endedAt ?? v.instance.lastTransitionAt)"
                >{{ v.instance.endedAt ? t("wfRun.panel.ended", { when: formatRelative(v.instance.endedAt) }) : formatRelative(v.instance.lastTransitionAt) }}</time
              >
            </td>
            <td class="wf-wrap">
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
              <span v-if="v.instance.status === 'active' && v.availableTransitions.length === 0 && !v.canCancel" class="muted">{{
                t("wfRun.actions.noneShort")
              }}</span>
            </td>
          </tr>
        </tbody>
      </table>
    </div>

    <Teleport to="body">
      <FormDialog :open="starting" :title="t('wfRun.start.title')" :submit-label="t('wfRun.start.submit')" :busy="start.isPending.value" @submit="confirmStart" @cancel="starting = false">
        <ErrorAlert v-if="start.isError.value && !startCommentError" :error="start.error.value" :title="t('wfRun.start.failed')" />
        <FormField id="wf-start-def" v-slot="p" :label="t('wfRun.col.workflow')" required>
          <select :id="p.id" v-model="startId">
            <option v-for="s in startable" :key="s.definitionId" :value="s.definitionId" dir="auto">{{ s.definitionName }} (v{{ s.versionNo }})</option>
          </select>
        </FormField>
        <FormField id="wf-start-comment" v-slot="p" :label="t('wfRun.comment')" :error="startCommentError" :hint="t('wfRun.commentOptional')">
          <textarea :id="p.id" v-model="startComment" rows="3" maxlength="4000" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
        </FormField>
        <p class="hint">{{ t("wfRun.start.hint", { ci: ci.label }) }}</p>
      </FormDialog>
    </Teleport>
  </section>
</template>
