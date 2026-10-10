<script setup lang="ts">
import { computed, ref, watch } from "vue";
import type { LookupListValue } from "../../../api/datamodel";
import { t } from "../../../i18n";
import { keyError } from "../../../lib/keys";
import { CATEGORIES, categoryLabel, renameState, type Draft, type DraftState, type PlacedProblem } from "../../../lib/workflowDraft";
import ProblemList from "./ProblemList.vue";

/** One state of the draft: key, name, category, terminal flag, the state field's value it maps to, and whether it is the initial state. */
const props = defineProps<{
  draft: Draft;
  state: DraftState;
  problems: PlacedProblem[];
  /** The state field's values; null when the workflow drives no state field. */
  stateValues: LookupListValue[] | null;
  stateFieldLabel?: string;
}>();
const emit = defineEmits<{ renamed: [key: string]; remove: [] }>();

const keyText = ref(props.state.key);
const keyProblem = ref<string>();
watch(
  () => props.state.key,
  (k) => {
    keyText.value = k;
    keyProblem.value = undefined;
  },
);

function commitKey() {
  const next = keyText.value.trim();
  if (next === props.state.key) return;
  const err = keyError(next) ?? (props.draft.states.some((s) => s.key === next) ? t("wfDesign.state.keyTaken", { key: next }) : undefined);
  keyProblem.value = err;
  if (err) return;
  renameState(props.draft, props.state.key, next);
  emit("renamed", next);
}

const isInitial = computed(() => props.draft.initialState === props.state.key);
const outgoing = computed(() => props.draft.transitions.filter((t) => t.from === props.state.key).length);
const incoming = computed(() => props.draft.transitions.filter((t) => t.to === props.state.key).length);
const unknownValue = computed(() => !!props.state.stateValue && !!props.stateValues && !props.stateValues.some((v) => v.key === props.state.stateValue));
const fid = (f: string) => `wf-state-${f}`;
</script>

<template>
  <section class="panel wf-inspector" aria-labelledby="wf-state-title">
    <div class="panel-header">
      <h2 id="wf-state-title">{{ t("wfDesign.state.title", { name: state.name || state.key }) }}</h2>
      <span v-if="isInitial" class="badge info">{{ t("wfDesign.initial") }}</span>
      <span v-if="state.terminal" class="badge">{{ t("wfDesign.terminal") }}</span>
    </div>
    <div class="panel-body stack">
      <ProblemList :problems="problems" />
      <div class="form-grid">
        <div class="field">
          <label :for="fid('name')">{{ t("wfDesign.col.name") }}<span class="req" aria-hidden="true">*</span></label>
          <input :id="fid('name')" v-model="state.name" type="text" maxlength="100" aria-required="true" autocomplete="off" />
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
          <span v-else :id="`${fid('key')}-hint`" class="hint">{{ t("wfDesign.state.keyHint") }}</span>
        </div>
        <div class="field">
          <label :for="fid('category')">{{ t("wfDesign.col.category") }}</label>
          <select :id="fid('category')" v-model="state.category" :aria-describedby="`${fid('category')}-hint`">
            <option v-for="c in CATEGORIES" :key="c.value" :value="c.value">{{ categoryLabel(c.value) }}</option>
          </select>
          <span :id="`${fid('category')}-hint`" class="hint">{{ t("wfDesign.state.categoryHint") }}</span>
        </div>
        <div class="field">
          <label :for="fid('value')">{{ stateFieldLabel ? t("wfDesign.state.value", { field: stateFieldLabel }) : t("wfDesign.state.valueGeneric") }}</label>
          <select :id="fid('value')" v-model="state.stateValue" :disabled="!stateValues" :aria-describedby="`${fid('value')}-hint`">
            <option :value="null">{{ t("wfAdmin.none") }}</option>
            <option v-if="unknownValue" :value="state.stateValue">{{ t("wfDesign.state.notInList", { value: state.stateValue ?? "" }) }}</option>
            <option v-for="v in stateValues ?? []" :key="v.id" :value="v.key">{{ v.name }}{{ v.isActive ? "" : ` ${t("wfDesign.retired")}` }}</option>
          </select>
          <span :id="`${fid('value')}-hint`" class="hint">
            {{ stateValues ? t("wfDesign.state.valueHint") : t("wfDesign.state.noField") }}
          </span>
        </div>
      </div>
      <label class="checkbox-row"><input v-model="state.terminal" type="checkbox" /> {{ t("wfDesign.state.terminal") }}</label>
      <p class="muted no-margin">{{ t("wfDesign.state.links", { in: incoming, n: outgoing }) }}</p>
      <div class="inline-actions">
        <button type="button" class="btn btn-sm" :disabled="isInitial" @click="draft.initialState = state.key">
          {{ isInitial ? t("wfDesign.state.isInitial") : t("wfDesign.state.makeInitial") }}
        </button>
        <button type="button" class="btn btn-sm btn-quiet-danger" @click="emit('remove')">{{ t("wfDesign.state.delete") }}</button>
      </div>
    </div>
  </section>
</template>
