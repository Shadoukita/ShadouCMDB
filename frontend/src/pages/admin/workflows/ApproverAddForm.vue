<script setup lang="ts">
import { computed, ref, useId } from "vue";
import type { Principal } from "../../../api/services";
import PrincipalCombobox from "../../../components/PrincipalCombobox.vue";
import { t } from "../../../i18n";
import { SOURCES, type ApproverRole, type ApproverSource, type AttributeChoice, type DraftApprover, type Ref, type ServiceOwnerRole } from "../../../lib/workflowApprovals";

/**
 * Adds one assignment to a step: the role (approver, or escalation once overdue), the source, and
 * what it names: a permission profile, a user group, a user, a Person field of the CI, or the
 * technical or business owners of the CI's business services.
 */
const props = defineProps<{
  transitionKey: string;
  stepKey: string;
  /** Permission profiles to choose from; null when the caller may not list them (enter a name instead). */
  profiles: Ref[] | null;
  /** Reference fields of the workflow's type that point at the Person type. */
  attributes: AttributeChoice[];
  /** What the step already has, to refuse a duplicate. */
  taken: (a: DraftApprover) => boolean;
}>();
const emit = defineEmits<{ add: [a: DraftApprover] }>();

const uid = useId();
const fid = (f: string) => `wf-apadd-${uid}-${f}`;
const role = ref<ApproverRole>("approver");
const source = ref<ApproverSource>("profile");
const ref_ = ref<Ref | null>(null);
const name = ref("");
const profileId = ref("");
const attributeId = ref("");
const ownerRole = ref<ServiceOwnerRole>("business");
/** /principals needs edit on business services or users.manage: without it, groups and users are named. */
const byName = ref(false);

const candidate = computed<DraftApprover>(() => {
  const base = { transitionKey: props.transitionKey, stepKey: props.stepKey, role: role.value, source: source.value, ref: null, attribute: null, serviceOwnerRole: null };
  switch (source.value) {
    case "profile": {
      if (props.profiles === null) return { ...base, ref: name.value.trim() ? { id: name.value.trim(), name: name.value.trim() } : null };
      const p = props.profiles.find((x) => x.id === profileId.value);
      return { ...base, ref: p ?? null };
    }
    case "group":
    case "user":
      if (byName.value) return { ...base, ref: name.value.trim() ? { id: name.value.trim(), name: name.value.trim() } : null };
      return { ...base, ref: ref_.value };
    case "ci_attribute":
      return { ...base, attribute: props.attributes.find((a) => a.id === attributeId.value) ?? null };
    default:
      return { ...base, serviceOwnerRole: ownerRole.value };
  }
});
const complete = computed(() => {
  const c = candidate.value;
  return c.source === "ci_attribute" ? !!c.attribute : c.source === "service_owner" ? true : !!c.ref;
});
const duplicate = computed(() => complete.value && props.taken(candidate.value));

function pickPrincipal(p: Principal) {
  ref_.value = { id: p.id, name: p.username ? `${p.displayName} (${p.username})` : p.displayName };
}
function changeSource() {
  ref_.value = null;
  name.value = "";
}
function add() {
  if (!complete.value || duplicate.value) return;
  emit("add", candidate.value);
  ref_.value = null;
  name.value = "";
  profileId.value = "";
  attributeId.value = "";
}
</script>

<template>
  <div class="wf-approver-add" role="group" :aria-label="t('wfApprovers.add.group')">
    <div class="field">
      <label :for="fid('role')">{{ t("wfApprovers.role") }}</label>
      <select :id="fid('role')" v-model="role">
        <option value="approver">{{ t("wfApprovers.role.approver") }}</option>
        <option value="escalation">{{ t("wfApprovers.role.escalation") }}</option>
      </select>
    </div>
    <div class="field">
      <label :for="fid('source')">{{ t("wfApprovers.source") }}</label>
      <select :id="fid('source')" v-model="source" @change="changeSource">
        <option v-for="s in SOURCES" :key="s" :value="s">{{ t(`wfApproval.source.${s}`) }}</option>
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
    <template v-else-if="source === 'group' || source === 'user'">
      <PrincipalCombobox
        :label="t(source === 'group' ? 'wfApprovers.group' : 'wfApprovers.user')"
        :kind="source"
        :hint="ref_ ? t('wfApprovers.picked', { name: ref_.name }) : undefined"
        @select="pickPrincipal"
        @forbidden="byName = true"
      />
    </template>
    <div v-else-if="source === 'ci_attribute'" class="field">
      <label :for="fid('attr')">{{ t("wfApprovers.attribute") }}</label>
      <select :id="fid('attr')" v-model="attributeId" :disabled="attributes.length === 0" :aria-describedby="`${fid('attr')}-hint`">
        <option value="">{{ t("wfApprovers.choose") }}</option>
        <option v-for="a in attributes" :key="a.id" :value="a.id">{{ a.label }} ({{ a.key }})</option>
      </select>
      <span :id="`${fid('attr')}-hint`" class="hint">{{ attributes.length ? t("wfApprovers.attributeHint") : t("wfApprovers.noAttributes") }}</span>
    </div>
    <div v-else class="field">
      <label :for="fid('owner')">{{ t("wfApprovers.ownerRole") }}</label>
      <select :id="fid('owner')" v-model="ownerRole" :aria-describedby="`${fid('owner')}-hint`">
        <option value="business">{{ t("wfApprovers.ownerRole.business") }}</option>
        <option value="technical">{{ t("wfApprovers.ownerRole.technical") }}</option>
      </select>
      <span :id="`${fid('owner')}-hint`" class="hint">{{ t("wfApprovers.ownerHint") }}</span>
    </div>

    <div class="field wf-approver-add-action">
      <span class="label" aria-hidden="true">&nbsp;</span>
      <div class="inline-control">
        <button type="button" class="btn" :disabled="!complete || duplicate" @click="add">{{ t("wfApprovers.add") }}</button>
        <span v-if="duplicate" class="hint" role="status">{{ t("wfApprovers.duplicate") }}</span>
      </div>
    </div>
  </div>
</template>
