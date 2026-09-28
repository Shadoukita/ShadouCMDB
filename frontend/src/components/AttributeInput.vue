<script setup lang="ts">
import { computed } from "vue";
import type { CiSummary } from "../api/queries";
import type { AttributeShape, Validation } from "../lib/attributeValues";
import CiPicker from "./CiPicker.vue";
import LookupValueSelect, { type LookupParent } from "./LookupValueSelect.vue";

/**
 * One input per attribute dataType. The value is always a string; see lib/attributeValues for conversion.
 * (Number inputs bind manually because v-model would cast them to numbers.)
 */
const props = defineProps<{
  def: AttributeShape;
  id: string;
  invalid?: boolean;
  describedBy?: string;
  referenceName?: string;
  /** For a lookup attribute with a parent field: that field's label and current value. */
  lookupParent?: LookupParent | null;
}>();
const model = defineModel<string>({ required: true });
const emit = defineEmits<{ referenceName: [name: string] }>();

const v = computed(() => (props.def.validation ?? {}) as Validation);
const enumValues = computed(() => props.def.enumValues ?? []);
const aria = computed(() => ({ "aria-invalid": props.invalid || undefined, "aria-describedby": props.describedBy }));

function onReference(ci: CiSummary | null) {
  if (ci) emit("referenceName", ci.label);
  model.value = ci ? ci.id : "";
}
</script>

<template>
  <select v-if="def.dataType === 'boolean'" :id="id" v-model="model" v-bind="aria">
    <option value="">— not set —</option>
    <option value="true">Yes</option>
    <option value="false">No</option>
  </select>
  <select v-else-if="def.dataType === 'enum'" :id="id" v-model="model" v-bind="aria">
    <option value="">— not set —</option>
    <option v-for="ev in enumValues" :key="ev" :value="ev">{{ ev }}</option>
    <option v-if="model !== '' && !enumValues.includes(model)" :value="model">{{ model }} (no longer allowed)</option>
  </select>
  <input
    v-else-if="def.dataType === 'integer' || def.dataType === 'number'"
    :id="id"
    v-bind="aria"
    type="number"
    :value="model"
    :step="def.dataType === 'integer' ? 1 : 'any'"
    :min="v.min"
    :max="v.max"
    @input="model = ($event.target as HTMLInputElement).value"
  />
  <input v-else-if="def.dataType === 'date'" :id="id" v-model="model" v-bind="aria" type="date" />
  <input v-else-if="def.dataType === 'datetime'" :id="id" v-model="model" v-bind="aria" type="datetime-local" />
  <CiPicker
    v-else-if="def.dataType === 'reference'"
    :id="id"
    :class-id="def.referenceClassId"
    :selected="model ? { id: model, name: referenceName ?? model } : null"
    :invalid="invalid"
    :described-by="describedBy"
    @select="onReference"
  />
  <LookupValueSelect
    v-else-if="def.dataType === 'lookup'"
    :id="id"
    v-model="model"
    :list-id="def.lookupListId"
    :parent="lookupParent"
    :invalid="invalid"
    :described-by="describedBy"
  />
  <input
    v-else-if="def.dataType === 'ip' || def.dataType === 'cidr'"
    :id="id"
    v-model="model"
    v-bind="aria"
    type="text"
    class="mono"
    spellcheck="false"
  />
  <input v-else :id="id" v-model="model" v-bind="aria" type="text" :maxlength="v.maxLength" />
</template>
