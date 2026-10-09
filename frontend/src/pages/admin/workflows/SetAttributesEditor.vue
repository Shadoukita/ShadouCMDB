<script setup lang="ts">
import { computed, ref } from "vue";
import type { AttributeDefinition } from "../../../api/datamodel";
import { t } from "../../../i18n";
import { MAX_SET_ATTRIBUTES, defaultMode, modesFor, setTargetRefusal, type DraftSetAttribute, type SetMode } from "../../../lib/workflowActions";
import type { PlacedProblem } from "../../../lib/workflowDraft";
import ConditionValueInput from "./ConditionValueInput.vue";

/**
 * The attribute actions of one transition (SHAA-2725 §3.4): fields of the CI the transition sets when
 * it runs, to a literal or to the date, the time, the person running it, or nothing. They are part of
 * the draft and take effect when it is published; the publish lint refuses the state field, identifying
 * and read-only fields, fields the transition asks for, and references to anything but a Person.
 */
const props = defineProps<{
  list: DraftSetAttribute[];
  transitionKey: string;
  /** The fields of the workflow's type, own and inherited. */
  fields: AttributeDefinition[];
  stateFieldKey?: string;
  /** Keys of the fields the transition asks the operator for. */
  transitionFields: string[];
  /** The Person type (and its subtypes): the only references an action may set. */
  personClassIds: Set<string>;
  /** The transition's problems: the ones about an attribute action show on its row. */
  problems: PlacedProblem[];
}>();

const fieldByKey = computed(() => new Map(props.fields.map((f) => [f.key, f])));
const refusal = (f: AttributeDefinition) =>
  setTargetRefusal(f, { stateFieldKey: props.stateFieldKey, transitionFields: props.transitionFields, personClassIds: props.personClassIds });
const choices = computed(() =>
  props.fields
    .filter((f) => f.isActive && !props.list.some((s) => s.attribute === f.key))
    .map((f) => ({ field: f, refused: refusal(f) })),
);

const toAdd = ref("");
function add() {
  const f = fieldByKey.value.get(toAdd.value);
  if (!f || props.list.length >= MAX_SET_ATTRIBUTES) return;
  const mode = defaultMode(f);
  props.list.push({ attribute: f.key, mode, value: mode === "literal" && f.dataType === "boolean" ? true : "" });
  toAdd.value = "";
}
function setMode(s: DraftSetAttribute, mode: SetMode) {
  s.mode = mode;
  if (mode !== "literal") s.value = "";
  else if (fieldByKey.value.get(s.attribute)?.dataType === "boolean") s.value = true;
}

const id = (j: number, f: string) => `wf-set-${props.transitionKey}-${j}-${f}`;
const label = (key: string) => fieldByKey.value.get(key)?.label ?? key;
/** Problems about row j (path `transitions[i].setAttributes[j]…`). */
function problemsOf(j: number) {
  const re = new RegExp(`\\.setAttributes\\[${j}\\]`);
  return props.problems.filter((p) => re.test(p.path));
}
const listProblems = computed(() => props.problems.filter((p) => /\.setAttributes$/.test(p.path)));
</script>

<template>
  <fieldset class="group" data-testid="wf-set-attributes">
    <legend>
      {{ t("wfActions.set.legend") }}
      <span class="badge info spaced">{{ t("wfActions.set.onPublish") }}</span>
    </legend>
    <p class="hint no-margin">{{ t("wfActions.set.intro") }}</p>
    <ul v-if="listProblems.length" class="wf-problems">
      <li v-for="(p, i) in listProblems" :key="i" :class="p.severity">{{ p.message }}</li>
    </ul>
    <table v-if="list.length" class="data wf-fields">
      <caption class="sr-only">{{ t("wfActions.set.caption") }}</caption>
      <thead>
        <tr>
          <th scope="col">{{ t("wfActions.set.field") }}</th>
          <th scope="col">{{ t("wfActions.set.mode") }}</th>
          <th scope="col">{{ t("wfActions.set.value") }}</th>
          <th scope="col"><span class="sr-only">{{ t("wfApproval.remove") }}</span></th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="(s, j) in list" :key="s.attribute || j" :data-testid="`wf-set-${s.attribute}`">
          <td class="wrap">
            {{ label(s.attribute) }} <span class="mono muted">{{ s.attribute }}</span>
            <span v-if="!fieldByKey.has(s.attribute)" class="badge danger">{{ t("wfActions.set.notOnType") }}</span>
            <span v-for="(p, k) in problemsOf(j)" :key="k" :class="p.severity === 'error' ? 'error' : 'hint'">{{ p.message }}</span>
          </td>
          <td>
            <select
              :id="id(j, 'mode')"
              :value="s.mode"
              :aria-label="t('wfActions.set.modeOf', { field: label(s.attribute) })"
              @change="setMode(s, ($event.target as HTMLSelectElement).value as SetMode)"
            >
              <option v-for="m in fieldByKey.get(s.attribute) ? modesFor(fieldByKey.get(s.attribute)!) : [s.mode]" :key="m" :value="m">
                {{ t(`wfActions.set.mode.${m}`) }}
              </option>
            </select>
          </td>
          <td>
            <ConditionValueInput
              v-if="s.mode === 'literal'"
              :leaf="s"
              :field="fieldByKey.get(s.attribute)"
              :id="id(j, 'value')"
              :label="t('wfActions.set.valueOf', { field: label(s.attribute) })"
            />
            <span v-else class="muted">{{ t(`wfActions.set.modeHint.${s.mode as Exclude<SetMode, "literal">}`) }}</span>
          </td>
          <td class="row-actions">
            <button
              type="button"
              class="btn btn-sm btn-quiet-danger"
              :aria-label="t('wfActions.set.removeLabel', { field: label(s.attribute) })"
              @click="list.splice(j, 1)"
            >
              {{ t("wfApproval.remove") }}
            </button>
          </td>
        </tr>
      </tbody>
    </table>
    <p v-else class="muted no-margin">{{ t("wfActions.set.none") }}</p>
    <div class="inline-control">
      <select v-model="toAdd" :aria-label="t('wfActions.set.add')" :disabled="list.length >= MAX_SET_ATTRIBUTES" data-testid="wf-set-add-field">
        <option value="">{{ t("wfActions.set.addPlaceholder") }}</option>
        <option v-for="c in choices" :key="c.field.key" :value="c.field.key" :disabled="!!c.refused">
          {{ c.refused ? `${c.field.label} (${c.refused})` : c.field.label }}
        </option>
      </select>
      <button type="button" class="btn btn-sm" :disabled="!toAdd" @click="add">{{ t("wfActions.set.addButton") }}</button>
    </div>
  </fieldset>
</template>
