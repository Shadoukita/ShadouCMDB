<script setup lang="ts">
import { computed, ref, watch } from "vue";
import type { AttributeDefinition } from "../../../api/datamodel";
import { t } from "../../../i18n";
import { keyError } from "../../../lib/keys";
import {
  CLOSED_STATUSES,
  CONTENT_LEVELS,
  KINDS,
  MAX_INCLUDE_ATTRIBUTES,
  MAX_RECIPIENTS,
  PLACEHOLDERS,
  TRIGGERS,
  notifiesPeople,
  participantsFor,
  recipientIdentity,
  recipientLabel,
  triggerHasTransition,
  unplacedProblems,
  type ActionProblem,
  type ClosedStatus,
  type DraftAction,
  type DraftRecipient,
} from "../../../lib/workflowActions";
import type { AttributeChoice, Ref } from "../../../lib/workflowApprovals";
import ActionRecipientAdd from "./ActionRecipientAdd.vue";

/**
 * One notification action: what it is (name, key, kind), when it fires (trigger and transition, the
 * outcomes for a closed approval), who it tells (recipient sources) and what it says (e-mail: content
 * level, subject and intro in English and German) or where it goes (webhook: endpoint and the CI fields
 * the payload carries). Problems with a path into this action show next to their field.
 */
const props = defineProps<{
  action: DraftAction;
  /** Position in the list: the paths of the problems name it. */
  index: number;
  transitions: { key: string; name: string; orphan: boolean }[];
  /** The fields of the workflow's type, own and inherited. */
  fields: AttributeDefinition[];
  profiles: Ref[] | null;
  personFields: AttributeChoice[];
  /** Problems found in this action (refusals and lint), paths `actions[index]…`. */
  problems: ActionProblem[];
}>();

const a = computed(() => props.action);
const fid = (f: string) => `wf-action-${props.index}-${f}`;
const at = (field: string) => props.problems.filter((p) => p.path === `actions[${props.index}].${field}`);
const errorAt = (field: string) => at(field).find((p) => p.severity === "error")?.message;
const warningsAt = (field: string) => at(field).filter((p) => p.severity === "warning");
/** Problems no field below shows: about the action as a whole, a field without a control, or a hidden one. */
const otherProblems = computed(() => unplacedProblems(props.problems, props.index, props.action));
const describedBy = (field: string, hint?: string) => (errorAt(field) ? `${fid(field)}-err` : hint);
/** The field's error and every warning, plus its hint when it has one. */
const describedByAll = (field: string, hint?: string) =>
  [hint, errorAt(field) && `${fid(field)}-err`, ...warningsAt(field).map((_, k) => `${fid(field)}-warn-${k}`)].filter(Boolean).join(" ") || undefined;
/** A subject or intro field: its own error or the placeholder hint, and the warnings about both languages. */
const textDescribedBy = (text: "subject" | "intro", lang: "en" | "de") =>
  [errorAt(`settings.${text}.${lang}`) ? `${fid(`${text}-${lang}`)}-err` : fid("placeholders"), warningsAt(`settings.${text}`).length && `${fid(text)}-warn`]
    .filter(Boolean)
    .join(" ");

// The key: kept as typed until it is valid, like the transition's key.
const keyText = ref(props.action.key);
const keyProblem = ref<string>();
watch(
  () => props.action.key,
  (k) => {
    keyText.value = k;
    keyProblem.value = undefined;
  },
);
function commitKey() {
  const next = keyText.value.trim();
  keyProblem.value = keyError(next);
  if (!keyProblem.value) props.action.key = next;
}

function onTrigger() {
  if (!triggerHasTransition(a.value.trigger)) a.value.transition = null;
  else if (!a.value.transition) a.value.transition = props.transitions.find((x) => !x.orphan)?.key ?? null;
  // The requester and the approvers exist only for approval triggers.
  const allowed = participantsFor(a.value.trigger);
  a.value.recipients = a.value.recipients.filter((r) => r.source !== "participant" || !r.participant || allowed.includes(r.participant));
}
function onKind() {
  if (a.value.kind !== "email") a.value.recipients = a.value.recipients.filter((r) => r.source !== "address");
}

/** No status listed means every outcome; at least one stays chosen. */
function toggleStatus(s: ClosedStatus, on: boolean, box: HTMLInputElement) {
  const now = (a.value.statuses.length ? a.value.statuses : CLOSED_STATUSES).filter((x) => x !== s);
  if (!on && now.length === 0) {
    box.checked = true;
    return;
  }
  const next = on ? [...now, s] : now;
  a.value.statuses = next.length === CLOSED_STATUSES.length ? [] : CLOSED_STATUSES.filter((x) => next.includes(x));
}
const allStatuses = computed(() => a.value.statuses.length === 0);

const taken = (r: DraftRecipient) => a.value.recipients.some((x) => recipientIdentity(x) === recipientIdentity(r));
function addRecipient(r: DraftRecipient) {
  a.value.recipients.push(r);
}
const recipientErrors = (j: number) => props.problems.filter((p) => p.path === `actions[${props.index}].recipients[${j}]` || p.path.startsWith(`actions[${props.index}].recipients[${j}].`));

/** Webhook payload fields: every active field of the type; the lint refuses keys that are not. */
const payloadFields = computed(() => props.fields.filter((f) => f.isActive));
function toggleInclude(key: string, on: boolean) {
  const now = a.value.includeAttributes.filter((k) => k !== key);
  a.value.includeAttributes = on ? [...now, key] : now;
}
</script>

<template>
  <div class="stack" :data-testid="`wf-action-editor-${index}`">
    <ul v-if="otherProblems.length" class="wf-problems">
      <li v-for="(p, i) in otherProblems" :key="i" :class="p.severity">
        <span :class="['badge', p.severity === 'error' ? 'danger' : 'warn']">{{ p.severity === "error" ? t("wfApproval.error") : t("wfApproval.warning") }}</span>
        {{ p.message }}
      </li>
    </ul>
    <div class="form-grid">
      <div class="field">
        <label :for="fid('name')">{{ t("wfActions.field.name") }}<span class="req" aria-hidden="true">*</span></label>
        <input
          :id="fid('name')"
          v-model="action.name"
          type="text"
          maxlength="100"
          autocomplete="off"
          aria-required="true"
          :aria-invalid="!!errorAt('name')"
          :aria-describedby="describedBy('name')"
        />
        <span v-if="errorAt('name')" :id="`${fid('name')}-err`" class="error">{{ errorAt("name") }}</span>
      </div>
      <div class="field">
        <label :for="fid('key')">{{ t("wfActions.field.key") }}<span class="req" aria-hidden="true">*</span></label>
        <input
          :id="fid('key')"
          v-model="keyText"
          class="mono"
          type="text"
          maxlength="63"
          spellcheck="false"
          autocomplete="off"
          aria-required="true"
          :aria-invalid="!!(keyProblem || errorAt('key'))"
          :aria-describedby="keyProblem || errorAt('key') ? `${fid('key')}-err` : `${fid('key')}-hint`"
          @change="commitKey"
        />
        <span v-if="keyProblem || errorAt('key')" :id="`${fid('key')}-err`" class="error">{{ keyProblem ?? errorAt("key") }}</span>
        <span v-else :id="`${fid('key')}-hint`" class="hint">{{ t("wfActions.field.keyHint") }}</span>
      </div>
      <div class="field">
        <label :for="fid('kind')">{{ t("wfActions.field.kind") }}</label>
        <select :id="fid('kind')" v-model="action.kind" :aria-invalid="!!errorAt('kind')" :aria-describedby="describedBy('kind', `${fid('kind')}-hint`)" @change="onKind">
          <option v-for="k in KINDS" :key="k" :value="k">{{ t(`wfActions.kind.${k}`) }}</option>
        </select>
        <span v-if="errorAt('kind')" :id="`${fid('kind')}-err`" class="error">{{ errorAt("kind") }}</span>
        <span v-else :id="`${fid('kind')}-hint`" class="hint">{{ t(`wfActions.kindHint.${action.kind}`) }}</span>
      </div>
      <div class="field">
        <label :for="fid('trigger')">{{ t("wfActions.field.trigger") }}</label>
        <select :id="fid('trigger')" v-model="action.trigger" :aria-invalid="!!errorAt('trigger')" :aria-describedby="describedBy('trigger')" @change="onTrigger">
          <option v-for="tr in TRIGGERS" :key="tr" :value="tr">{{ t(`wfActions.trigger.${tr}`) }}</option>
        </select>
        <span v-if="errorAt('trigger')" :id="`${fid('trigger')}-err`" class="error">{{ errorAt("trigger") }}</span>
      </div>
      <div v-if="triggerHasTransition(action.trigger)" class="field">
        <label :for="fid('transition')">{{ t("wfActions.field.transition") }}<span class="req" aria-hidden="true">*</span></label>
        <select
          :id="fid('transition')"
          v-model="action.transition"
          aria-required="true"
          :aria-invalid="!!errorAt('transition')"
          :aria-describedby="describedByAll('transition')"
        >
          <option :value="null" disabled>{{ t("wfApprovers.choose") }}</option>
          <option v-for="x in transitions" :key="x.key" :value="x.key">{{ x.orphan ? t("wfActions.field.orphanTransition", { key: x.key }) : `${x.name} (${x.key})` }}</option>
        </select>
        <span v-if="errorAt('transition')" :id="`${fid('transition')}-err`" class="error">{{ errorAt("transition") }}</span>
        <span v-for="(w, k) in warningsAt('transition')" :id="`${fid('transition')}-warn-${k}`" :key="k" class="hint">{{ w.message }}</span>
      </div>
    </div>
    <label class="checkbox-row"><input v-model="action.enabled" type="checkbox" /> {{ t("wfActions.field.enabled") }}</label>

    <fieldset v-if="action.trigger === 'approval_closed'" class="group">
      <legend>{{ t("wfActions.field.statuses") }}</legend>
      <p class="hint no-margin">{{ t("wfActions.field.statusesHint") }}</p>
      <div class="wf-choices">
        <label v-for="s in CLOSED_STATUSES" :key="s" class="checkbox-row">
          <input type="checkbox" :checked="allStatuses || action.statuses.includes(s)" @change="toggleStatus(s, ($event.target as HTMLInputElement).checked, $event.target as HTMLInputElement)" />
          {{ t(`wfActions.status.${s}`) }}
        </label>
      </div>
      <span v-if="errorAt('settings.statuses')" class="error">{{ errorAt("settings.statuses") }}</span>
    </fieldset>

    <!-- Who it tells: inbox and e-mail. -->
    <fieldset v-if="notifiesPeople(action.kind)" class="group" :aria-describedby="describedByAll('recipients', `${fid('recipients')}-hint`)" data-testid="wf-action-recipients">
      <legend>{{ t("wfActions.field.recipients") }}<span class="req" aria-hidden="true">*</span></legend>
      <p :id="`${fid('recipients')}-hint`" class="hint no-margin">{{ t("wfActions.field.recipientsHint", { n: MAX_RECIPIENTS }) }}</p>
      <span v-if="errorAt('recipients')" :id="`${fid('recipients')}-err`" class="error" role="alert">{{ errorAt("recipients") }}</span>
      <p v-for="(w, k) in warningsAt('recipients')" :id="`${fid('recipients')}-warn-${k}`" :key="k" class="hint no-margin" data-testid="wf-action-recipients-warn">
        <span class="badge warn">{{ t("wfApproval.warning") }}</span> {{ w.message }}
      </p>
      <table v-if="action.recipients.length" class="data wf-approver-table">
        <caption class="sr-only">{{ t("wfActions.field.recipientsCaption", { name: action.name }) }}</caption>
        <thead>
          <tr>
            <th scope="col">{{ t("wfActions.field.recipient") }}</th>
            <th scope="col"><span class="sr-only">{{ t("wfApproval.remove") }}</span></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="(r, j) in action.recipients" :key="recipientIdentity(r)">
            <td class="wrap">
              {{ recipientLabel(r) }}
              <span v-if="r.source === 'address'" class="badge spaced">{{ t("wfActions.recipient.minimalOnly") }}</span>
              <span v-for="(p, k) in recipientErrors(j)" :key="k" :class="p.severity === 'error' ? 'error' : 'hint'">{{ p.message }}</span>
            </td>
            <td class="row-actions">
              <button
                type="button"
                class="btn btn-sm btn-quiet-danger"
                :aria-label="t('wfActions.recipient.removeLabel', { who: recipientLabel(r) })"
                @click="action.recipients.splice(j, 1)"
              >
                {{ t("wfApproval.remove") }}
              </button>
            </td>
          </tr>
        </tbody>
      </table>
      <ActionRecipientAdd
        :kind="action.kind"
        :trigger="action.trigger"
        :profiles="profiles"
        :attributes="personFields"
        :taken="taken"
        :disabled="action.recipients.length >= MAX_RECIPIENTS"
        @add="addRecipient"
      />
      <label class="checkbox-row"><input v-model="action.excludeActor" type="checkbox" /> {{ t("wfActions.field.excludeActor") }}</label>
    </fieldset>

    <!-- What it says: e-mail. -->
    <fieldset v-if="action.kind === 'email'" class="group" data-testid="wf-action-email">
      <legend>{{ t("wfActions.field.message") }}</legend>
      <fieldset class="wf-choices">
        <legend class="label">{{ t("wfActions.field.content") }}</legend>
        <label v-for="c in CONTENT_LEVELS" :key="c" class="checkbox-row">
          <input v-model="action.content" type="radio" :name="fid('content')" :value="c" :aria-describedby="`${fid('content')}-${c}`" />
          {{ t(`wfActions.content.${c}`) }}
          <span :id="`${fid('content')}-${c}`" class="hint">{{ t(`wfActions.contentHint.${c}`) }}</span>
        </label>
      </fieldset>
      <div class="form-grid">
        <div v-for="lang in ['en', 'de'] as const" :key="`s-${lang}`" class="field">
          <label :for="fid(`subject-${lang}`)">{{ t(`wfActions.field.subject.${lang}`) }}</label>
          <input
            :id="fid(`subject-${lang}`)"
            v-model="action.subject[lang]"
            type="text"
            maxlength="200"
            autocomplete="off"
            :lang="lang"
            :aria-invalid="!!errorAt(`settings.subject.${lang}`)"
            :aria-describedby="textDescribedBy('subject', lang)"
          />
          <span v-if="errorAt(`settings.subject.${lang}`)" :id="`${fid(`subject-${lang}`)}-err`" class="error">{{ errorAt(`settings.subject.${lang}`) }}</span>
          <span v-for="(w, k) in warningsAt(`settings.subject.${lang}`)" :key="k" class="hint">{{ w.message }}</span>
        </div>
        <!-- Warnings about the subject in both languages: under the pair, on a row of their own. -->
        <div v-if="warningsAt('settings.subject').length" :id="`${fid('subject')}-warn`" class="field wide" data-testid="wf-action-subject-warn">
          <span v-for="(w, k) in warningsAt('settings.subject')" :key="k" class="hint">{{ w.message }}</span>
        </div>
        <div v-for="lang in ['en', 'de'] as const" :key="`i-${lang}`" class="field">
          <label :for="fid(`intro-${lang}`)">{{ t(`wfActions.field.intro.${lang}`) }}</label>
          <textarea
            :id="fid(`intro-${lang}`)"
            v-model="action.intro[lang]"
            rows="3"
            maxlength="2000"
            :lang="lang"
            :aria-invalid="!!errorAt(`settings.intro.${lang}`)"
            :aria-describedby="textDescribedBy('intro', lang)"
          />
          <span v-if="errorAt(`settings.intro.${lang}`)" :id="`${fid(`intro-${lang}`)}-err`" class="error">{{ errorAt(`settings.intro.${lang}`) }}</span>
          <span v-for="(w, k) in warningsAt(`settings.intro.${lang}`)" :key="k" class="hint">{{ w.message }}</span>
        </div>
        <div v-if="warningsAt('settings.intro').length" :id="`${fid('intro')}-warn`" class="field wide" data-testid="wf-action-intro-warn">
          <span v-for="(w, k) in warningsAt('settings.intro')" :key="k" class="hint">{{ w.message }}</span>
        </div>
      </div>
      <p :id="fid('placeholders')" class="hint no-margin">
        {{ t("wfActions.field.placeholders") }} <span class="mono">{{ PLACEHOLDERS.join(" ") }}</span>
      </p>
    </fieldset>

    <!-- Where it goes: webhook. -->
    <fieldset v-if="action.kind === 'webhook'" class="group" data-testid="wf-action-webhook">
      <legend>{{ t("wfActions.field.webhook") }}</legend>
      <div class="field">
        <label :for="fid('endpoint')">{{ t("wfActions.field.endpoint") }}<span class="req" aria-hidden="true">*</span></label>
        <input
          :id="fid('endpoint')"
          v-model.trim="action.endpoint"
          class="mono"
          type="text"
          maxlength="63"
          spellcheck="false"
          autocomplete="off"
          aria-required="true"
          :aria-invalid="!!errorAt('endpoint')"
          :aria-describedby="errorAt('endpoint') ? `${fid('endpoint')}-err` : `${fid('endpoint')}-hint`"
        />
        <span v-if="errorAt('endpoint')" :id="`${fid('endpoint')}-err`" class="error">{{ errorAt("endpoint") }}</span>
        <span v-else :id="`${fid('endpoint')}-hint`" class="hint">{{ t("wfActions.field.endpointHint") }}</span>
      </div>
      <fieldset class="wf-choices" :aria-describedby="`${fid('include')}-hint`">
        <legend class="label">{{ t("wfActions.field.includeAttributes") }}</legend>
        <span :id="`${fid('include')}-hint`" class="hint">{{ t("wfActions.field.includeAttributesHint", { n: MAX_INCLUDE_ATTRIBUTES }) }}</span>
        <label v-for="f in payloadFields" :key="f.key" class="checkbox-row">
          <input type="checkbox" :checked="action.includeAttributes.includes(f.key)" @change="toggleInclude(f.key, ($event.target as HTMLInputElement).checked)" />
          {{ f.label }} <span class="mono muted">{{ f.key }}</span>
        </label>
        <span v-if="errorAt('settings.includeAttributes')" class="error">{{ errorAt("settings.includeAttributes") }}</span>
      </fieldset>
    </fieldset>
  </div>
</template>
