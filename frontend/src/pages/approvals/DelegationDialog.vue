<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useCreateDelegation, type DelegateCandidate } from "../../api/approvals";
import { ApiError } from "../../api/client";
import type { Principal } from "../../api/services";
import ErrorAlert from "../../components/ErrorAlert.vue";
import FormDialog from "../../components/FormDialog.vue";
import PrincipalCombobox from "../../components/PrincipalCombobox.vue";
import { t } from "../../i18n";
import { delegationBody, delegationProblems, newDelegationDraft, MAX_DELEGATION_DAYS, type DelegationDraft } from "../../lib/approvalDelegations";
import { useFlashStore } from "../../stores/flash";
import { useSessionStore } from "../../stores/session";
import FormField from "../form/FormField.vue";
import DelegateCandidatePicker from "./DelegateCandidatePicker.vue";

/**
 * Delegates approvals for a time window: your own (`admin` false, POST /me/approval-delegations) or, with
 * users.manage, someone else's while they are away (POST /admin/approval-delegations). The delegate decides in
 * the principal's name and the decision records both. The checks the API makes (not yourself, an end after the
 * start and in the future, at most 90 days) run before sending; the API's own field errors go next to the same
 * inputs. Your own delegate comes from GET /me/approval-delegations/candidates, which needs no lookup right: without
 * one you enter the delegate's exact username. Someone else's delegation (users.manage) uses the user lookup.
 */
const props = defineProps<{ open: boolean; admin: boolean }>();
const emit = defineEmits<{ close: [] }>();

const session = useSessionStore();
const flash = useFlashStore();
const create = useCreateDelegation(props.admin);
const draft = ref<DelegationDraft>(newDelegationDraft());
const principalName = ref("");
const delegateName = ref("");
const tried = ref(false);
watch(
  () => props.open,
  (open) => {
    if (!open) return;
    draft.value = newDelegationDraft();
    principalName.value = "";
    delegateName.value = "";
    tried.value = false;
    create.reset();
  },
);

const label = (p: Principal) => (p.username ? `${p.displayName} (${p.username})` : p.displayName);
function pickPrincipal(p: Principal) {
  draft.value.principalId = p.id;
  principalName.value = label(p);
}
function pickDelegate(p: Principal) {
  draft.value.delegateId = p.id;
  delegateName.value = label(p);
}
function pickCandidate(c: DelegateCandidate | null) {
  draft.value.delegateId = c?.id ?? "";
  delegateName.value = c ? `${c.displayName} (${c.username})` : "";
}

const apiErrors = computed(() => (create.error.value instanceof ApiError && create.error.value.code === "VALIDATION_ERROR" ? create.error.value.fieldErrors() : {}));
const problems = computed(() => delegationProblems(draft.value, props.admin, session.user?.id));
const errorFor = (field: string) => (tried.value ? problems.value[field] : undefined) ?? apiErrors.value[field];
const placed = ["principalUserId", "delegateUserId", "startsAt", "endsAt", "definitionKey", "reason"];
const unplaced = computed(() => create.isError.value && !placed.some((f) => apiErrors.value[f]));

async function submit() {
  tried.value = true;
  if (Object.keys(problems.value).length > 0) return;
  try {
    const d = props.admin ? await create.mutateAsync(delegationBody(draft.value, true)) : await create.mutateAsync(delegationBody(draft.value, false));
    flash.show(t("delegations.created", { principal: d.principal.name, delegate: d.delegate.name }));
    emit("close");
  } catch {
    // shown in the dialog
  }
}
</script>

<template>
  <FormDialog
    :open="open"
    :title="admin ? t('delegations.new.adminTitle') : t('delegations.new.title')"
    :submit-label="t('delegations.new.submit')"
    :busy="create.isPending.value"
    wide
    @submit="submit"
    @cancel="emit('close')"
  >
    <p class="muted">{{ t("delegations.new.intro") }}</p>
    <ErrorAlert v-if="unplaced" :error="create.error.value" :title="t('delegations.new.failed')" />
    <div class="form-grid">
      <!-- The combobox is a field of its own (label, hint); a problem replaces its hint, which it is described by. -->
      <div v-if="admin" :class="['delegation-picker', { invalid: !!errorFor('principalUserId') }]" data-testid="delegation-principal">
        <PrincipalCombobox
          :label="t('delegations.principal')"
          kind="user"
          :hint="errorFor('principalUserId') ?? (principalName ? t('delegations.picked', { name: principalName }) : t('delegations.principalHint'))"
          @select="pickPrincipal"
        />
      </div>
      <div :class="['delegation-picker', { invalid: !!errorFor('delegateUserId') }]" data-testid="delegation-delegate">
        <PrincipalCombobox
          v-if="admin"
          :label="t('delegations.delegate')"
          kind="user"
          :hint="errorFor('delegateUserId') ?? (delegateName ? t('delegations.picked', { name: delegateName }) : t('delegations.delegateHint'))"
          @select="pickDelegate"
        />
        <DelegateCandidatePicker v-else :label="t('delegations.delegate')" :error="errorFor('delegateUserId')" :picked="delegateName" @select="pickCandidate" />
      </div>
      <FormField id="delegation-start" v-slot="f" :label="t('delegations.startsAt')" required :error="errorFor('startsAt')" :hint="t('delegations.startsAtHint')">
        <input :id="f.id" v-model="draft.startsAt" type="datetime-local" :aria-invalid="f.invalid || undefined" :aria-describedby="f.describedBy" />
      </FormField>
      <FormField
        id="delegation-end"
        v-slot="f"
        :label="t('delegations.endsAt')"
        required
        :error="errorFor('endsAt')"
        :hint="t('delegations.endsAtHint', { days: MAX_DELEGATION_DAYS })"
      >
        <input :id="f.id" v-model="draft.endsAt" type="datetime-local" :aria-invalid="f.invalid || undefined" :aria-describedby="f.describedBy" />
      </FormField>
      <FormField id="delegation-workflow" v-slot="f" :label="t('delegations.workflow')" :error="errorFor('definitionKey')" :hint="t('delegations.workflowHint')">
        <input :id="f.id" v-model.trim="draft.definitionKey" class="mono" maxlength="63" :aria-invalid="f.invalid || undefined" :aria-describedby="f.describedBy" />
      </FormField>
      <FormField id="delegation-reason" v-slot="f" :label="t('delegations.reason')" :error="errorFor('reason')" :hint="t('delegations.reasonHint')" wide>
        <textarea :id="f.id" v-model="draft.reason" rows="2" maxlength="500" :aria-invalid="f.invalid || undefined" :aria-describedby="f.describedBy" />
      </FormField>
    </div>
  </FormDialog>
</template>
