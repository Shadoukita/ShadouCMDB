<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import { RouterLink } from "vue-router";
import type { AttributeDefinition } from "../../../api/datamodel";
import { useWorkflowActions } from "../../../api/workflows";
import { t } from "../../../i18n";
import { keyError } from "../../../lib/keys";
import { describeTrigger } from "../../../lib/workflowActions";
import type { Draft, DraftTransition, PlacedProblem } from "../../../lib/workflowDraft";
import ApprovalStepsEditor from "./ApprovalStepsEditor.vue";
import ConditionGroupEditor from "./ConditionGroupEditor.vue";
import ProblemList from "./ProblemList.vue";
import SetAttributesEditor from "./SetAttributesEditor.vue";

/**
 * One transition of the draft, on two tabs. Rules: key, name, its two states, whether a comment is
 * required, the fields the operator fills in when running it, the conditions that must hold and the
 * approval steps. Actions: the fields it sets on the CI (in the draft, so they take effect when it is
 * published), and the workflow's notifications about it (saved on the Notifications tab, at once).
 */
const props = defineProps<{
  workflowId: string;
  draft: Draft;
  transition: DraftTransition;
  problems: PlacedProblem[];
  /** The fields of the workflow's type, own and inherited. */
  fields: AttributeDefinition[];
  /** The key of the workflow's state field: its states set it, so it cannot be a transition field. */
  stateFieldKey?: string;
  /** Transition keys granted to a profile: renaming one of them loses its grants. */
  grantedKeys: Set<string>;
  /** The Person type: the only type an attribute action may set a reference to. */
  personClassIds: Set<string>;
}>();
const emit = defineEmits<{ renamed: [key: string]; remove: [] }>();

const keyText = ref(props.transition.key);
const keyProblem = ref<string>();
watch(
  () => props.transition.key,
  (k) => {
    keyText.value = k;
    keyProblem.value = undefined;
  },
);
function commitKey() {
  const next = keyText.value.trim();
  if (next === props.transition.key) return;
  const err =
    keyError(next) ??
    (next === "_cancel" ? t("wfDesign.tr.keyReserved", { key: next }) : props.draft.transitions.some((x) => x.key === next) ? t("wfDesign.tr.keyTaken", { key: next }) : undefined);
  keyProblem.value = err;
  if (err) return;
  // Steps of other transitions that exclude this one's actors follow the new key.
  for (const tr of props.draft.transitions) for (const st of tr.approval) st.excludeActorsOf = st.excludeActorsOf.map((k) => (k === props.transition.key ? next : k));
  props.transition.key = next;
  emit("renamed", next);
}

const fieldByKey = computed(() => new Map(props.fields.map((f) => [f.key, f])));
const unusedFields = computed(() => props.fields.filter((f) => !props.transition.fields.some((x) => x.attribute === f.key)));
const toAdd = ref("");
function addField() {
  if (!toAdd.value) return;
  props.transition.fields.push({ attribute: toAdd.value, required: true });
  toAdd.value = "";
}
const fid = (f: string) => `wf-tr-${f}`;
const otherTransitions = computed(() => props.draft.transitions.filter((x) => x !== props.transition).map((x) => ({ key: x.key, name: x.name })));

// ---------- Tabs ----------

const tab = ref<"rules" | "actions">("rules");
const TABS = ["rules", "actions"] as const;
async function onTabKey(e: KeyboardEvent) {
  const at = TABS.indexOf(tab.value);
  const to = { ArrowRight: at + 1, ArrowLeft: at + 1, Home: 0, End: 1 }[e.key];
  if (to === undefined) return;
  e.preventDefault();
  tab.value = TABS[to % 2];
  await nextTick();
  document.getElementById(`wf-tr-tab-${tab.value}`)?.focus();
}
const actionProblems = computed(() => props.problems.filter((p) => /\.setAttributes(\[|$)/.test(p.path)));
const actionsQ = useWorkflowActions(() => props.workflowId);
/** The workflow's notifications that fire on this transition (by key, so a renamed key loses them). */
const notifications = computed(() => (actionsQ.data.value?.actions ?? []).filter((a) => a.transition === props.transition.key));
</script>

<template>
  <section class="panel wf-inspector" aria-labelledby="wf-tr-title">
    <div class="panel-header">
      <h2 id="wf-tr-title">{{ t("wfDesign.tr.title", { name: transition.name || transition.key }) }}</h2>
    </div>
    <div class="tabs wf-inspector-tabs" role="tablist" :aria-label="t('wfActions.inspector.tabs')">
      <button
        v-for="k in TABS"
        :id="`wf-tr-tab-${k}`"
        :key="k"
        type="button"
        role="tab"
        :aria-selected="tab === k"
        aria-controls="wf-tr-panel"
        :tabindex="tab === k ? 0 : -1"
        :data-testid="`wf-tr-tab-${k}`"
        @click="tab = k"
        @keydown="onTabKey"
      >
        {{ t(`wfActions.inspector.tab.${k}`) }}
        <span v-if="k === 'actions' && transition.setAttributes.length + notifications.length" class="badge spaced">
          {{ transition.setAttributes.length + notifications.length }}
        </span>
        <span v-if="k === 'actions' && actionProblems.some((p) => p.severity === 'error')" class="badge danger spaced">{{ t("wfApproval.error") }}</span>
      </button>
    </div>
    <div v-if="tab === 'actions'" id="wf-tr-panel" class="panel-body stack" role="tabpanel" aria-labelledby="wf-tr-tab-actions">
      <SetAttributesEditor
        :list="transition.setAttributes"
        :transition-key="transition.key"
        :fields="fields"
        :state-field-key="stateFieldKey"
        :transition-fields="transition.fields.map((f) => f.attribute)"
        :person-class-ids="personClassIds"
        :problems="actionProblems"
      />
      <fieldset class="group" data-testid="wf-tr-notifications">
        <legend>{{ t("wfActions.inspector.notifications") }}</legend>
        <p class="hint no-margin">{{ t("wfActions.inspector.notificationsIntro") }}</p>
        <ul v-if="notifications.length" class="no-margin">
          <li v-for="a in notifications" :key="a.key">
            {{ a.name }} <span class="badge spaced">{{ t(`wfActions.kind.${a.kind}`) }}</span>
            <span class="muted">{{ describeTrigger({ trigger: a.trigger, transition: a.transition ?? null }, () => transition.name) }}</span>
            <span v-if="!a.enabled" class="badge off spaced">{{ t("wfActions.disabled") }}</span>
          </li>
        </ul>
        <p v-else-if="!actionsQ.isLoading.value" class="muted no-margin">{{ t("wfActions.inspector.noNotifications") }}</p>
        <div>
          <RouterLink class="btn btn-sm" :to="{ query: { tab: 'actions', transition: transition.key } }" data-testid="wf-tr-notifications-link">
            {{ t("wfActions.inspector.manage") }}
          </RouterLink>
        </div>
      </fieldset>
    </div>
    <div v-else id="wf-tr-panel" class="panel-body stack" role="tabpanel" aria-labelledby="wf-tr-tab-rules">
      <ProblemList :problems="problems" />
      <div class="form-grid">
        <div class="field">
          <label :for="fid('name')">{{ t("wfDesign.col.name") }}<span class="req" aria-hidden="true">*</span></label>
          <input :id="fid('name')" v-model="transition.name" type="text" maxlength="100" aria-required="true" autocomplete="off" :aria-describedby="`${fid('name')}-hint`" />
          <span :id="`${fid('name')}-hint`" class="hint">{{ t("wfDesign.tr.nameHint") }}</span>
        </div>
        <div class="field">
          <label :for="fid('key')">{{ t("wfDesign.col.key") }}<span class="req" aria-hidden="true">*</span></label>
          <input
            :id="fid('key')"
            v-model="keyText"
            class="mono"
            type="text"
            maxlength="63"
            spellcheck="false"
            autocomplete="off"
            aria-required="true"
            :aria-invalid="!!keyProblem"
            :aria-describedby="keyProblem ? `${fid('key')}-err` : `${fid('key')}-hint`"
            @change="commitKey"
          />
          <span v-if="keyProblem" :id="`${fid('key')}-err`" class="error">{{ keyProblem }}</span>
          <span v-else :id="`${fid('key')}-hint`" class="hint">
            {{ grantedKeys.has(transition.key) ? t("wfDesign.tr.keyHintGranted") : t("wfDesign.tr.keyHint") }}
          </span>
        </div>
        <div class="field">
          <label :for="fid('from')">{{ t("wfDesign.tr.from") }}</label>
          <select :id="fid('from')" v-model="transition.from">
            <option v-for="s in draft.states" :key="s.key" :value="s.key">{{ s.name }}</option>
          </select>
        </div>
        <div class="field">
          <label :for="fid('to')">{{ t("wfDesign.tr.to") }}</label>
          <select :id="fid('to')" v-model="transition.to">
            <option v-for="s in draft.states" :key="s.key" :value="s.key" :disabled="s.key === transition.from">{{ s.name }}</option>
          </select>
        </div>
      </div>
      <label class="checkbox-row"><input v-model="transition.requiresComment" type="checkbox" /> {{ t("wfDesign.tr.comment") }}</label>

      <fieldset class="group">
        <legend>{{ t("wfDesign.tr.fields") }}</legend>
        <p class="hint no-margin">{{ t("wfDesign.tr.fieldsHint") }}</p>
        <table v-if="transition.fields.length" class="data wf-fields">
          <thead>
            <tr>
              <th scope="col">{{ t("wfDesign.tr.field") }}</th>
              <th scope="col">{{ t("common.required") }}</th>
              <th scope="col"><span class="sr-only">{{ t("wfApproval.remove") }}</span></th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="(f, i) in transition.fields" :key="f.attribute || i">
              <td>
                {{ fieldByKey.get(f.attribute)?.label ?? f.attribute }} <span class="mono muted">{{ f.attribute }}</span>
                <span v-if="!fieldByKey.has(f.attribute)" class="badge danger">{{ t("wfDesign.tr.notField") }}</span>
              </td>
              <td><input v-model="f.required" type="checkbox" :aria-label="t('wfDesign.tr.isRequired', { field: fieldByKey.get(f.attribute)?.label ?? f.attribute })" /></td>
              <td class="row-actions">
                <button type="button" class="btn btn-sm btn-quiet-danger" @click="transition.fields.splice(i, 1)">{{ t("wfApproval.remove") }}</button>
              </td>
            </tr>
          </tbody>
        </table>
        <div class="inline-control">
          <select v-model="toAdd" :aria-label="t('wfDesign.tr.fieldToAdd')">
            <option value="">{{ t("wfDesign.tr.addField") }}</option>
            <option v-for="f in unusedFields" :key="f.key" :value="f.key" :disabled="f.key === stateFieldKey">
              {{ f.key === stateFieldKey ? t("wfDesign.tr.stateField", { field: f.label }) : f.label }}
            </option>
          </select>
          <button type="button" class="btn btn-sm" :disabled="!toAdd" @click="addField">{{ t("wfDesign.tr.add") }}</button>
        </div>
      </fieldset>

      <fieldset class="group">
        <legend>{{ t("wfDesign.tr.conditions") }}</legend>
        <p class="hint no-margin">{{ t("wfDesign.tr.conditionsHint") }}</p>
        <ConditionGroupEditor :group="transition.conditions" :fields="fields" :depth="1" :id-prefix="`wf-cond-${transition.key}`" />
      </fieldset>

      <ApprovalStepsEditor :steps="transition.approval" :transition-key="transition.key" :others="otherTransitions" :problems="problems" />
    </div>
    <div class="panel-body">
      <div class="inline-actions">
        <button type="button" class="btn btn-sm btn-quiet-danger" @click="emit('remove')">{{ t("wfDesign.tr.delete") }}</button>
      </div>
    </div>
  </section>
</template>
