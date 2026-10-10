<script setup lang="ts">
import { ref } from "vue";
import { DELEGATION_STATUS_TONES, useRevokeDelegation, type ApprovalDelegation, type ApprovalDelegationUser } from "../../api/approvals";
import { ApiError } from "../../api/client";
import ConfirmDialog from "../../components/ConfirmDialog.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import { t } from "../../i18n";
import { canRevoke, delegationScope } from "../../lib/approvalDelegations";
import { formatDateTime } from "../../lib/format";
import { useFlashStore } from "../../stores/flash";
import { useSessionStore } from "../../stores/session";

/**
 * Delegations as a table, newest window first: whose approvals, to whom, which workflows, the window, where it
 * stands now, and who made it. A scheduled or active one may be revoked (the principal, the delegate or, on the
 * admin page, users.manage); it stays listed as revoked and the decisions made through it stand.
 */
const props = defineProps<{ rows: ApprovalDelegation[]; admin: boolean; loading?: boolean; label: string }>();

const session = useSessionStore();
const flash = useFlashStore();
const revoke = useRevokeDelegation(props.admin);
const revoking = ref<ApprovalDelegation | null>(null);

const name = (u: ApprovalDelegationUser) => (u.id && u.id === session.user?.id ? t("delegations.you", { name: u.name }) : u.name);
function openRevoke(r: ApprovalDelegation) {
  revoke.reset();
  revoking.value = r;
}
async function confirmRevoke() {
  const r = revoking.value;
  if (!r) return;
  try {
    await revoke.mutateAsync(r.id);
    flash.show(t("delegations.revoked", { principal: r.principal.name, delegate: r.delegate.name }));
    revoking.value = null;
  } catch {
    // shown in the dialog
  }
}
</script>

<template>
  <div class="table-wrap">
    <table :class="['data', { loading }]" :aria-label="label">
      <thead>
        <tr>
          <th scope="col">{{ t("delegations.principal") }}</th>
          <th scope="col">{{ t("delegations.delegate") }}</th>
          <th scope="col">{{ t("delegations.workflow") }}</th>
          <th scope="col">{{ t("delegations.window") }}</th>
          <th scope="col">{{ t("delegations.status") }}</th>
          <th scope="col" style="width: 100%">{{ t("delegations.reason") }}</th>
          <th scope="col">{{ t("delegations.createdBy") }}</th>
          <th scope="col"><span class="sr-only">{{ t("delegations.actions") }}</span></th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="r in rows" :key="r.id" :data-testid="`delegation-${r.id}`">
          <td dir="auto">{{ name(r.principal) }}</td>
          <td dir="auto">{{ name(r.delegate) }}</td>
          <td dir="auto">{{ delegationScope(r) }}</td>
          <td>{{ t("delegations.range", { from: formatDateTime(r.startsAt), to: formatDateTime(r.endsAt) }) }}</td>
          <td>
            <span :class="['badge', DELEGATION_STATUS_TONES[r.status]]">{{ t(`delegations.statusName.${r.status}`) }}</span>
            <div v-if="r.revokedAt" class="muted" :title="formatDateTime(r.revokedAt)">{{ t("delegations.revokedBy", { name: r.revokedByName ?? "–" }) }}</div>
          </td>
          <td style="white-space: normal" dir="auto">
            <template v-if="r.reason">{{ r.reason }}</template><span v-else class="muted">–</span>
          </td>
          <td :title="formatDateTime(r.createdAt)" dir="auto">{{ name(r.createdBy) }}</td>
          <td>
            <button
              v-if="canRevoke(r.status)"
              type="button"
              class="btn btn-sm btn-ghost"
              :aria-label="t('delegations.revokeLabel', { principal: r.principal.name, delegate: r.delegate.name })"
              @click="openRevoke(r)"
            >
              {{ t("delegations.revoke") }}
            </button>
          </td>
        </tr>
      </tbody>
    </table>
  </div>
  <ConfirmDialog
    :open="!!revoking"
    :title="t('delegations.revokeTitle')"
    :confirm-label="t('delegations.revoke')"
    :busy="revoke.isPending.value"
    @confirm="confirmRevoke"
    @cancel="revoking = null"
  >
    <ErrorAlert
      v-if="revoke.isError.value"
      :error="revoke.error.value"
      :title="revoke.error.value instanceof ApiError && revoke.error.value.code === 'CONFLICT' ? t('delegations.revokeMoved') : t('delegations.revokeFailed')"
    />
    <p v-if="revoking">{{ t("delegations.revokeBody", { principal: revoking.principal.name, delegate: revoking.delegate.name }) }}</p>
  </ConfirmDialog>
</template>
