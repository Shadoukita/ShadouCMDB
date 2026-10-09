<script setup lang="ts">
import { computed, ref, useId, watch } from "vue";
import type { Principal } from "../../../api/services";
import PrincipalCombobox from "../../../components/PrincipalCombobox.vue";
import { t } from "../../../i18n";
import {
  participantsFor,
  recipientComplete,
  sourcesFor,
  type ActionKind,
  type ActionTrigger,
  type DraftRecipient,
  type Participant,
  type RecipientSource,
  type ServiceOwnerRole,
} from "../../../lib/workflowActions";
import type { AttributeChoice, Ref } from "../../../lib/workflowApprovals";

/**
 * Adds one recipient source to a notification action: a permission profile, a user group, a user, the
 * CI's owner, a Person field of the CI, the owners of its business services, a participant of the event,
 * or (e-mail only) a fixed address. Sources are resolved to users when the action runs.
 */
const props = defineProps<{
  kind: ActionKind;
  trigger: ActionTrigger;
  /** Permission profiles to choose from; null when the caller may not list them (enter a name instead). */
  profiles: Ref[] | null;
  /** Reference fields of the workflow's type that point at the Person type. */
  attributes: AttributeChoice[];
  /** Whether the action already has this recipient. */
  taken: (r: DraftRecipient) => boolean;
  disabled?: boolean;
}>();
const emit = defineEmits<{ add: [r: DraftRecipient] }>();

const uid = useId();
const fid = (f: string) => `wf-acadd-${uid}-${f}`;
const sources = computed(() => sourcesFor(props.kind));
const source = ref<RecipientSource>("group");
watch(sources, (list) => {
  if (!list.includes(source.value)) source.value = list[0] ?? "group";
});
const participants = computed(() => participantsFor(props.trigger));

const picked = ref<Ref | null>(null);
const name = ref("");
const profileId = ref("");
const attributeId = ref("");
const ownerRole = ref<ServiceOwnerRole>("business");
const participant = ref<Participant>("starter");
const address = ref("");
/** /principals needs edit on business services or users.manage: without it, groups and users are named. */
const byName = ref(false);

const candidate = computed<DraftRecipient>(() => {
  const base: DraftRecipient = { source: source.value, ref: null, attribute: null, serviceOwnerRole: null, participant: null, address: null };
  const named = name.value.trim() ? { id: name.value.trim(), name: name.value.trim() } : null;
  switch (source.value) {
    case "profile":
      return { ...base, ref: props.profiles === null ? named : (props.profiles.find((p) => p.id === profileId.value) ?? null) };
    case "group":
    case "user":
      return { ...base, ref: byName.value ? named : picked.value };
    case "ci_attribute":
      return { ...base, attribute: props.attributes.find((a) => a.id === attributeId.value) ?? null };
    case "service_owner":
      return { ...base, serviceOwnerRole: ownerRole.value };
    case "participant":
      return { ...base, participant: participants.value.includes(participant.value) ? participant.value : null };
    case "address":
      return { ...base, address: address.value.trim() };
    default:
      return base;
  }
});
const complete = computed(() => recipientComplete(candidate.value));
const duplicate = computed(() => complete.value && props.taken(candidate.value));

function pickPrincipal(p: Principal) {
  picked.value = { id: p.id, name: p.username ? `${p.displayName} (${p.username})` : p.displayName };
}
function changeSource() {
  picked.value = null;
  name.value = "";
}
function add() {
  if (!complete.value || duplicate.value || props.disabled) return;
  emit("add", candidate.value);
  picked.value = null;
  name.value = "";
  profileId.value = "";
  attributeId.value = "";
  address.value = "";
}
</script>

<template>
  <div class="wf-approver-add" role="group" :aria-label="t('wfActions.recipient.addGroup')" data-testid="wf-action-recipient-add">
    <div class="field">
      <label :for="fid('source')">{{ t("wfApprovers.source") }}</label>
      <select :id="fid('source')" v-model="source" @change="changeSource">
        <option v-for="s in sources" :key="s" :value="s">{{ t(`wfActions.source.${s}`) }}</option>
      </select>
    </div>

    <div v-if="source === 'profile' && profiles !== null" class="field">
      <label :for="fid('profile')">{{ t("wfApprovers.profile") }}</label>
      <select :id="fid('profile')" v-model="profileId">
        <option value="">{{ t("wfApprovers.choose") }}</option>
        <option v-for="p in profiles" :key="p.id" :value="p.id">{{ p.name }}</option>
      </select>
    </div>
    <div v-else-if="(source === 'profile' && profiles === null) || ((source === 'group' || source === 'user') && byName)" class="field">
      <label :for="fid('name')">{{ t(`wfApprovers.byName.${source as "profile" | "group" | "user"}`) }}</label>
      <input :id="fid('name')" v-model="name" type="text" maxlength="200" autocomplete="off" :aria-describedby="`${fid('name')}-hint`" @keydown.enter.prevent="add" />
      <span :id="`${fid('name')}-hint`" class="hint">{{ t("wfApprovers.byNameHint") }}</span>
    </div>
    <PrincipalCombobox
      v-else-if="source === 'group' || source === 'user'"
      :key="source"
      :label="t(source === 'group' ? 'wfApprovers.group' : 'wfApprovers.user')"
      :kind="source"
      :hint="picked ? t('wfApprovers.picked', { name: picked.name }) : undefined"
      @select="pickPrincipal"
      @forbidden="byName = true"
    />
    <div v-else-if="source === 'ci_attribute'" class="field">
      <label :for="fid('attr')">{{ t("wfApprovers.attribute") }}</label>
      <select :id="fid('attr')" v-model="attributeId" :disabled="attributes.length === 0" :aria-describedby="`${fid('attr')}-hint`">
        <option value="">{{ t("wfApprovers.choose") }}</option>
        <option v-for="a in attributes" :key="a.id" :value="a.id">{{ a.label }} ({{ a.key }})</option>
      </select>
      <span :id="`${fid('attr')}-hint`" class="hint">{{ attributes.length ? t("wfActions.recipient.attributeHint") : t("wfApprovers.noAttributes") }}</span>
    </div>
    <div v-else-if="source === 'service_owner'" class="field">
      <label :for="fid('owner')">{{ t("wfApprovers.ownerRole") }}</label>
      <select :id="fid('owner')" v-model="ownerRole" :aria-describedby="`${fid('owner')}-hint`">
        <option value="business">{{ t("wfApprovers.ownerRole.business") }}</option>
        <option value="technical">{{ t("wfApprovers.ownerRole.technical") }}</option>
      </select>
      <span :id="`${fid('owner')}-hint`" class="hint">{{ t("wfApprovers.ownerHint") }}</span>
    </div>
    <div v-else-if="source === 'participant'" class="field">
      <label :for="fid('participant')">{{ t("wfActions.recipient.participant") }}</label>
      <select :id="fid('participant')" v-model="participant" :aria-describedby="`${fid('participant')}-hint`">
        <option v-for="p in participants" :key="p" :value="p">{{ t(`wfActions.participant.${p}`) }}</option>
      </select>
      <span :id="`${fid('participant')}-hint`" class="hint">{{ t("wfActions.recipient.participantHint") }}</span>
    </div>
    <div v-else-if="source === 'address'" class="field">
      <label :for="fid('address')">{{ t("wfActions.recipient.address") }}</label>
      <input
        :id="fid('address')"
        v-model="address"
        type="email"
        maxlength="254"
        autocomplete="off"
        spellcheck="false"
        :aria-describedby="`${fid('address')}-hint`"
        @keydown.enter.prevent="add"
      />
      <span :id="`${fid('address')}-hint`" class="hint">{{ t("wfActions.recipient.addressHint") }}</span>
    </div>
    <div v-else class="field">
      <span class="label">{{ t("wfActions.source.ci_owner") }}</span>
      <span class="hint">{{ t("wfActions.recipient.ownerHint") }}</span>
    </div>

    <div class="field wf-approver-add-action">
      <span class="label" aria-hidden="true">&nbsp;</span>
      <div class="inline-control">
        <button type="button" class="btn" :disabled="!complete || duplicate || disabled" data-testid="wf-action-recipient-add-button" @click="add">
          {{ t("wfActions.recipient.add") }}
        </button>
        <span v-if="duplicate" class="hint" role="status">{{ t("wfActions.recipient.duplicate") }}</span>
      </div>
    </div>
  </div>
</template>
