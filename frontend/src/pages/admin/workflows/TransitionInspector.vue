<script setup lang="ts">
import { computed, ref, watch } from "vue";
import type { AttributeDefinition } from "../../../api/datamodel";
import { keyError } from "../../../lib/keys";
import type { Draft, DraftTransition, PlacedProblem } from "../../../lib/workflowDraft";
import ConditionGroupEditor from "./ConditionGroupEditor.vue";
import ProblemList from "./ProblemList.vue";

/**
 * One transition of the draft: key, name, its two states, whether a comment is required, the
 * fields the operator fills in when running it, and the conditions that must hold.
 */
const props = defineProps<{
  draft: Draft;
  transition: DraftTransition;
  problems: PlacedProblem[];
  /** The fields of the workflow's type, own and inherited. */
  fields: AttributeDefinition[];
  /** Transition keys granted to a profile: renaming one of them loses its grants. */
  grantedKeys: Set<string>;
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
    (next === "_cancel" ? "_cancel is reserved." : props.draft.transitions.some((t) => t.key === next) ? `Another transition already has the key ${next}.` : undefined);
  keyProblem.value = err;
  if (err) return;
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
</script>

<template>
  <section class="panel wf-inspector" aria-labelledby="wf-tr-title">
    <div class="panel-header">
      <h2 id="wf-tr-title">Transition: {{ transition.name || transition.key }}</h2>
    </div>
    <div class="panel-body stack">
      <ProblemList :problems="problems" />
      <div class="form-grid">
        <div class="field">
          <label :for="fid('name')">Name<span class="req" aria-hidden="true">*</span></label>
          <input :id="fid('name')" v-model="transition.name" type="text" maxlength="100" aria-required="true" autocomplete="off" :aria-describedby="`${fid('name')}-hint`" />
          <span :id="`${fid('name')}-hint`" class="hint">The action operators see, e.g. "Approve".</span>
        </div>
        <div class="field">
          <label :for="fid('key')">Key<span class="req" aria-hidden="true">*</span></label>
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
            {{ grantedKeys.has(transition.key) ? "Grants are given by key: a new key needs its grants again." : "Grants and the API name the transition by its key." }}
          </span>
        </div>
        <div class="field">
          <label :for="fid('from')">From state</label>
          <select :id="fid('from')" v-model="transition.from">
            <option v-for="s in draft.states" :key="s.key" :value="s.key">{{ s.name }}</option>
          </select>
        </div>
        <div class="field">
          <label :for="fid('to')">To state</label>
          <select :id="fid('to')" v-model="transition.to">
            <option v-for="s in draft.states" :key="s.key" :value="s.key" :disabled="s.key === transition.from">{{ s.name }}</option>
          </select>
        </div>
      </div>
      <label class="checkbox-row"><input v-model="transition.requiresComment" type="checkbox" /> A comment is required to run it</label>

      <fieldset class="group">
        <legend>Fields to fill in</legend>
        <p class="hint no-margin">Shown in the transition dialog. A required field must have a value for the transition to run.</p>
        <table v-if="transition.fields.length" class="data wf-fields">
          <thead>
            <tr>
              <th scope="col">Field</th>
              <th scope="col">Required</th>
              <th scope="col"><span class="sr-only">Remove</span></th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="(f, i) in transition.fields" :key="f.attribute || i">
              <td>
                {{ fieldByKey.get(f.attribute)?.label ?? f.attribute }} <span class="mono muted">{{ f.attribute }}</span>
                <span v-if="!fieldByKey.has(f.attribute)" class="badge danger">Not a field of the type</span>
              </td>
              <td><input v-model="f.required" type="checkbox" :aria-label="`${fieldByKey.get(f.attribute)?.label ?? f.attribute} is required`" /></td>
              <td class="row-actions">
                <button type="button" class="btn btn-sm btn-quiet-danger" @click="transition.fields.splice(i, 1)">Remove</button>
              </td>
            </tr>
          </tbody>
        </table>
        <div class="inline-control">
          <select v-model="toAdd" aria-label="Field to add">
            <option value="">Add a field…</option>
            <option v-for="f in unusedFields" :key="f.key" :value="f.key">{{ f.label }}</option>
          </select>
          <button type="button" class="btn btn-sm" :disabled="!toAdd" @click="addField">Add</button>
        </div>
      </fieldset>

      <fieldset class="group">
        <legend>Conditions</legend>
        <p class="hint no-margin">Checked on the CI's current values when the transition runs; it can run only when they hold.</p>
        <ConditionGroupEditor :group="transition.conditions" :fields="fields" :depth="1" :id-prefix="`wf-cond-${transition.key}`" />
      </fieldset>

      <div class="inline-actions">
        <button type="button" class="btn btn-sm btn-quiet-danger" @click="emit('remove')">Delete transition</button>
      </div>
    </div>
  </section>
</template>
