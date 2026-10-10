<script setup lang="ts">
import { computed, ref } from "vue";
import { APPROVAL_STATUS_TONES, useInstanceApprovalRequests, type WorkflowApprovalRequestItem } from "../../api/workflowRuntime";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import PaginationBar from "../../components/PaginationBar.vue";
import { t } from "../../i18n";
import { approvalStatusLabel, closeReasonLabel } from "../../lib/approvalRuntime";
import { formatDateTime, formatRelative } from "../../lib/format";
import ApprovalRequestDialog from "./ApprovalRequestDialog.vue";

/**
 * Every approval request an instance had (GET /workflow-instances/{id}/approval-requests), newest first:
 * approved, rejected, withdrawn or cancelled, and the pending one. Each opens the request with its steps and
 * decisions. Hidden while the instance never had one, so workflows without approvals keep their page as it was.
 */
const props = defineProps<{ instanceId: string; stateName: (key: string) => string }>();

const limit = ref(20);
const offset = ref(0);
const q = useInstanceApprovalRequests(() => props.instanceId, limit, offset);
const rows = computed(() => q.data.value?.data ?? []);
const total = computed(() => q.data.value?.page.total ?? 0);
const open = ref<string | null>(null);

function stepText(r: WorkflowApprovalRequestItem) {
  const s = r.currentStep;
  return t("approvalRun.history.step", { step: s.stepNo, steps: r.stepCount, name: s.name, n: Number(s.approvals), required: s.requiredApprovals });
}
</script>

<template>
  <section v-if="q.isLoading.value || q.isError.value || total > 0" class="panel" aria-labelledby="wfi-approvals" data-testid="wf-approval-history">
    <div class="panel-header">
      <h2 id="wfi-approvals">{{ t("approvalRun.history.title") }}</h2>
      <span v-if="q.data.value" class="meta">{{ t("approvalRun.history.meta", { n: total }) }}</span>
    </div>
    <LoadingState v-if="q.isLoading.value" :label="t('approvalRun.history.loading')" />
    <div v-else-if="q.isError.value" class="panel-body">
      <ErrorAlert :error="q.error.value" :title="t('approvalRun.history.loadFailed')" :on-retry="() => q.refetch()" />
    </div>
    <template v-else>
      <div class="table-wrap">
        <table :class="['data', { loading: q.isPlaceholderData.value }]">
          <thead>
            <tr>
              <th scope="col" class="num">{{ t("approvalRun.history.no") }}</th>
              <th scope="col">{{ t("approvalRun.history.transition") }}</th>
              <th scope="col">{{ t("approvalRun.steps.status") }}</th>
              <th scope="col">{{ t("approvalRun.history.progress") }}</th>
              <th scope="col">{{ t("approvalRun.dialog.requested") }}</th>
              <th scope="col" style="width: 100%">{{ t("approvalRun.dialog.closed") }}</th>
              <th scope="col"><span class="sr-only">{{ t("approvalRun.history.actions") }}</span></th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="r in rows" :key="r.id">
              <td class="num">{{ r.requestNo }}</td>
              <td dir="auto">
                {{ r.transitionName }} <span class="muted">{{ t("approvalRun.history.to", { state: stateName(r.toState) }) }}</span>
              </td>
              <td>
                <span :class="['badge', APPROVAL_STATUS_TONES[r.status]]">{{ approvalStatusLabel(r.status) }}</span>
                <span v-if="r.status === 'pending' && r.currentStep.overdue" class="badge warn">{{ t("approvalRun.overdue") }}</span>
              </td>
              <td>{{ stepText(r) }}</td>
              <td :title="formatDateTime(r.requestedAt)">{{ formatRelative(r.requestedAt) }}, {{ r.requestedBy.name }}</td>
              <td style="white-space: normal">
                <template v-if="r.closedAt">
                  <span :title="formatDateTime(r.closedAt)">{{ formatRelative(r.closedAt) }}</span
                  ><template v-if="r.closedByName">, {{ r.closedByName }}</template>
                  <span v-if="closeReasonLabel(r.closeReason)" class="muted"> ({{ closeReasonLabel(r.closeReason) }})</span>
                </template>
                <span v-else class="muted">–</span>
              </td>
              <td>
                <button type="button" class="btn btn-sm" :aria-label="t('approvalRun.history.openLabel', { no: r.requestNo })" @click="open = r.id">
                  {{ t("approvalRun.history.open") }}
                </button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <PaginationBar
        v-if="total > limit"
        :total="total"
        :limit="limit"
        :offset="offset"
        @change="
          (p) => {
            limit = p.limit;
            offset = p.offset;
          }
        "
      />
    </template>
    <Teleport to="body">
      <ApprovalRequestDialog :open="!!open" :request-id="open" @close="open = null" />
    </Teleport>
  </section>
</template>
