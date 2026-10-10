<script setup lang="ts">
import { computed } from "vue";
import { t } from "../../../i18n";
import {
  DUE_UNITS,
  MAX_REQUIRED,
  MAX_STEPS,
  joinDuration,
  newStep,
  splitDuration,
  type DraftApprovalStep,
  type DueUnit,
} from "../../../lib/workflowApprovals";
import type { PlacedProblem } from "../../../lib/workflowDraft";
import { problemText } from "../../../lib/workflowProblems";

/**
 * The approval policy of one transition (SHAA-1869 A5): its ordered steps, each with a name and key,
 * the approvals it needs (N), an optional due period with what happens when it passes, and the
 * separation-of-duties flags. The steps are part of the draft and published with it; who decides
 * each step is set on the Approvers tab, by transition and step key.
 */
const props = defineProps<{
  steps: DraftApprovalStep[];
  transitionKey: string;
  /** The other transitions of the draft: whose actors a step may exclude. */
  others: { key: string; name: string }[];
  /** The transition's problems: the ones about a step show on it. */
  problems: PlacedProblem[];
}>();

const id = (j: number, f: string) => `wf-ap-${props.transitionKey}-${j}-${f}`;

function add() {
  props.steps.push(newStep(props.steps));
}
function remove(j: number) {
  props.steps.splice(j, 1);
}
function moveStep(j: number, by: -1 | 1) {
  const k = j + by;
  if (k < 0 || k >= props.steps.length) return;
  const [s] = props.steps.splice(j, 1);
  props.steps.splice(k, 0, s);
}

const due = computed(() => props.steps.map((s) => splitDuration(s.dueAfter)));
function setDueAmount(s: DraftApprovalStep, unit: DueUnit, raw: string) {
  // Whole units only; an empty, zero or negative amount means no due date.
  const n = raw.trim() === "" || !Number.isFinite(Number(raw)) ? null : Math.round(Number(raw));
  s.dueAfter = joinDuration(n, unit);
  if (!s.dueAfter && s.onOverdue === "reject") s.onOverdue = "flag";
}
function setDueUnit(s: DraftApprovalStep, amount: number | null, unit: DueUnit) {
  if (amount !== null) s.dueAfter = joinDuration(amount, unit);
}
function toggleExclude(s: DraftApprovalStep, key: string, on: boolean) {
  s.excludeActorsOf = on ? [...new Set([...s.excludeActorsOf, key])] : s.excludeActorsOf.filter((k) => k !== key);
}

/** Problems about step j (path `transitions[i].approval.steps[j]…`), errors first. */
function problemsOf(j: number) {
  const re = new RegExp(`\\.approval\\.steps\\[${j}\\]`);
  return props.problems.filter((p) => re.test(p.path));
}
function fieldError(j: number, field: string) {
  const p = problemsOf(j).find((p) => p.severity === "error" && p.path.endsWith(`.steps[${j}].${field}`));
  return p && problemText(p);
}
const policyProblems = computed(() => props.problems.filter((p) => /\.approval(\.steps)?$/.test(p.path)));
</script>

<template>
  <fieldset class="group wf-approval" data-testid="wf-approval">
    <legend>{{ t("wfApproval.legend") }}</legend>
    <p class="hint no-margin">{{ t("wfApproval.intro") }}</p>
    <ul v-if="policyProblems.length" class="wf-problems">
      <li v-for="(p, i) in policyProblems" :key="i" :class="p.severity">{{ problemText(p) }}</li>
    </ul>
    <p v-if="steps.length === 0" class="muted no-margin">{{ t("wfApproval.none") }}</p>
    <ol class="wf-steps">
      <li v-for="(s, j) in steps" :key="j" class="wf-step" :data-testid="`wf-step-${j}`">
        <fieldset class="wf-step-body">
          <legend>{{ t("wfApproval.step.legend", { n: j + 1, name: s.name || s.key }) }}</legend>
          <ul v-if="problemsOf(j).some((p) => p.severity === 'warning')" class="wf-problems">
            <li v-for="(p, i) in problemsOf(j).filter((p) => p.severity === 'warning')" :key="i" class="warning">
              <span class="badge warn">{{ t("wfApproval.warning") }}</span> {{ problemText(p) }}
            </li>
          </ul>
          <div class="form-grid">
            <div class="field">
              <label :for="id(j, 'name')">{{ t("wfApproval.step.name") }}<span class="req" aria-hidden="true">*</span></label>
              <input
                :id="id(j, 'name')"
                v-model="s.name"
                type="text"
                maxlength="100"
                autocomplete="off"
                aria-required="true"
                :aria-invalid="!!fieldError(j, 'name')"
                :aria-describedby="fieldError(j, 'name') ? `${id(j, 'name')}-err` : undefined"
              />
              <span v-if="fieldError(j, 'name')" :id="`${id(j, 'name')}-err`" class="error">{{ fieldError(j, "name") }}</span>
            </div>
            <div class="field">
              <label :for="id(j, 'key')">{{ t("wfApproval.step.key") }}<span class="req" aria-hidden="true">*</span></label>
              <input
                :id="id(j, 'key')"
                v-model.trim="s.key"
                class="mono"
                type="text"
                maxlength="63"
                spellcheck="false"
                autocomplete="off"
                aria-required="true"
                :aria-invalid="!!fieldError(j, 'key')"
                :aria-describedby="fieldError(j, 'key') ? `${id(j, 'key')}-err` : `${id(j, 'key')}-hint`"
              />
              <span v-if="fieldError(j, 'key')" :id="`${id(j, 'key')}-err`" class="error">{{ fieldError(j, "key") }}</span>
              <span v-else :id="`${id(j, 'key')}-hint`" class="hint">{{ t("wfApproval.step.keyHint") }}</span>
            </div>
            <div class="field">
              <label :for="id(j, 'n')">{{ t("wfApproval.step.required") }}</label>
              <input
                :id="id(j, 'n')"
                v-model.number="s.requiredApprovals"
                type="number"
                min="1"
                :max="MAX_REQUIRED"
                step="1"
                :aria-invalid="!!fieldError(j, 'requiredApprovals')"
                :aria-describedby="fieldError(j, 'requiredApprovals') ? `${id(j, 'n')}-err` : `${id(j, 'n')}-hint`"
              />
              <span v-if="fieldError(j, 'requiredApprovals')" :id="`${id(j, 'n')}-err`" class="error">{{ fieldError(j, "requiredApprovals") }}</span>
              <span v-else :id="`${id(j, 'n')}-hint`" class="hint">{{ t("wfApproval.step.requiredHint") }}</span>
            </div>
            <div class="field">
              <span :id="id(j, 'due-label')" class="label">{{ t("wfApproval.step.due") }}</span>
              <div class="inline-control" role="group" :aria-labelledby="id(j, 'due-label')">
                <input
                  :id="id(j, 'due')"
                  type="number"
                  min="1"
                  step="1"
                  class="wf-due-amount"
                  :value="due[j].amount ?? ''"
                  :aria-label="t('wfApproval.step.dueAmount')"
                  :aria-invalid="!!fieldError(j, 'dueAfter')"
                  :aria-describedby="fieldError(j, 'dueAfter') ? `${id(j, 'due')}-err` : `${id(j, 'due')}-hint`"
                  @change="setDueAmount(s, due[j].unit, ($event.target as HTMLInputElement).value)"
                />
                <select
                  :value="due[j].unit"
                  :aria-label="t('wfApproval.step.dueUnit')"
                  @change="setDueUnit(s, due[j].amount, ($event.target as HTMLSelectElement).value as (typeof DUE_UNITS)[number])"
                >
                  <option v-for="u in DUE_UNITS" :key="u" :value="u">{{ t(`wfApproval.unit.${u}`) }}</option>
                </select>
              </div>
              <span v-if="fieldError(j, 'dueAfter')" :id="`${id(j, 'due')}-err`" class="error">{{ fieldError(j, "dueAfter") }}</span>
              <span v-else :id="`${id(j, 'due')}-hint`" class="hint">{{ t("wfApproval.step.dueHint") }}</span>
            </div>
            <div class="field">
              <label :for="id(j, 'overdue')">{{ t("wfApproval.step.onOverdue") }}</label>
              <select
                :id="id(j, 'overdue')"
                v-model="s.onOverdue"
                :disabled="!s.dueAfter"
                :aria-invalid="!!fieldError(j, 'onOverdue')"
                :aria-describedby="fieldError(j, 'onOverdue') ? `${id(j, 'overdue')}-err` : `${id(j, 'overdue')}-hint`"
              >
                <option value="flag">{{ t("wfApproval.overdue.flag") }}</option>
                <option value="reject">{{ t("wfApproval.overdue.reject") }}</option>
              </select>
              <span v-if="fieldError(j, 'onOverdue')" :id="`${id(j, 'overdue')}-err`" class="error">{{ fieldError(j, "onOverdue") }}</span>
              <span v-else :id="`${id(j, 'overdue')}-hint`" class="hint">{{ s.dueAfter ? t("wfApproval.step.onOverdueHint") : t("wfApproval.step.onOverdueNoDue") }}</span>
            </div>
          </div>
          <label class="checkbox-row"><input v-model="s.distinctFromEarlier" type="checkbox" /> {{ t("wfApproval.step.distinct") }}</label>
          <label class="checkbox-row"><input v-model="s.allowApiTokens" type="checkbox" /> {{ t("wfApproval.step.tokens") }}</label>
          <fieldset v-if="others.length" class="wf-exclude">
            <legend>{{ t("wfApproval.step.exclude") }}</legend>
            <label v-for="o in others" :key="o.key" class="checkbox-row">
              <input type="checkbox" :checked="s.excludeActorsOf.includes(o.key)" @change="toggleExclude(s, o.key, ($event.target as HTMLInputElement).checked)" />
              {{ o.name }} <span class="mono muted">{{ o.key }}</span>
            </label>
            <span v-for="k in s.excludeActorsOf.filter((k) => !others.some((o) => o.key === k))" :key="k" class="checkbox-row">
              <span class="mono">{{ k }}</span>
              <span class="badge danger">{{ t("wfApproval.step.excludeUnknown") }}</span>
              <button type="button" class="btn btn-sm btn-quiet-danger" @click="toggleExclude(s, k, false)">{{ t("wfApproval.remove") }}</button>
            </span>
          </fieldset>
          <div class="inline-actions">
            <button type="button" class="btn btn-sm" :disabled="j === 0" :aria-label="t('wfApproval.step.upLabel', { n: j + 1 })" @click="moveStep(j, -1)">
              {{ t("wfApproval.step.up") }}
            </button>
            <button
              type="button"
              class="btn btn-sm"
              :disabled="j === steps.length - 1"
              :aria-label="t('wfApproval.step.downLabel', { n: j + 1 })"
              @click="moveStep(j, 1)"
            >
              {{ t("wfApproval.step.down") }}
            </button>
            <button type="button" class="btn btn-sm btn-quiet-danger" :aria-label="t('wfApproval.step.removeLabel', { n: j + 1 })" @click="remove(j)">
              {{ t("wfApproval.step.remove") }}
            </button>
          </div>
        </fieldset>
      </li>
    </ol>
    <div class="inline-control">
      <button type="button" class="btn btn-sm" :disabled="steps.length >= MAX_STEPS" data-testid="wf-add-step" @click="add">
        {{ steps.length ? t("wfApproval.addStep") : t("wfApproval.require") }}
      </button>
      <span v-if="steps.length >= MAX_STEPS" class="hint">{{ t("wfApproval.max", { n: MAX_STEPS }) }}</span>
      <span v-else-if="steps.length" class="hint">{{ t("wfApproval.approversHint") }}</span>
    </div>
  </fieldset>
</template>
