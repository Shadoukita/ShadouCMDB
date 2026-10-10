<script setup lang="ts">
import { computed } from "vue";
import { useLookupListValues, type AttributeDefinition } from "../../../api/datamodel";
import CiPicker from "../../../components/CiPicker.vue";
import { t } from "../../../i18n";
import { opTakesList, type ConditionLeaf, type ConditionOp } from "../../../lib/workflowDraft";

/**
 * The value of one condition, in the field's type: a list of keys for dropdown and choice fields,
 * a date or number picker, yes/no, a CI for references. `in`/`notIn` take several values.
 * Dropdown and choice values are stored by key, as the API expects. Without an operator it edits one
 * value, as an attribute action's literal.
 */
const props = defineProps<{ leaf: Pick<ConditionLeaf, "value"> & { op?: ConditionOp }; field?: AttributeDefinition; id: string; label: string }>();

const type = computed(() => props.field?.dataType ?? "text");
const list = computed(() => !!props.leaf.op && opTakesList(props.leaf.op));
const lookupValues = useLookupListValues(() => (type.value === "lookup" ? props.field?.lookupListId : undefined));
const choices = computed<{ key: string; label: string; inactive?: boolean }[]>(() => {
  if (type.value === "enum") return (props.field?.enumValues ?? []).map((v) => ({ key: v, label: v }));
  if (type.value === "lookup") return (lookupValues.data.value ?? []).map((v) => ({ key: v.key, label: v.name, inactive: !v.isActive }));
  return [];
});
const hasChoices = computed(() => type.value === "enum" || type.value === "lookup");

const values = computed<unknown[]>(() => (Array.isArray(props.leaf.value) ? props.leaf.value : []));

function toggleChoice(key: string, on: boolean) {
  const now = values.value.filter((v) => v !== key);
  props.leaf.value = on ? [...now, key] : now;
}

/** Typed from the text box: numbers for number fields; empty text is "no value yet". */
function parseOne(raw: string): unknown {
  const s = raw.trim();
  if (type.value === "number" || type.value === "integer") return s === "" ? "" : Number(s);
  return s;
}

const listText = computed(() => values.value.map(String).join(", "));
function setListText(raw: string) {
  props.leaf.value = raw
    .split(",")
    .map((p) => parseOne(p))
    .filter((v) => v !== "");
}

/** A datetime is stored as an ISO instant; the picker shows it in local time. */
function toLocalInput(iso: unknown): string {
  if (typeof iso !== "string" || !iso) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}T${p(d.getHours())}:${p(d.getMinutes())}`;
}
function setDatetime(raw: string) {
  const d = new Date(raw);
  props.leaf.value = raw && !Number.isNaN(d.getTime()) ? d.toISOString() : "";
}

const refSelected = computed(() => (typeof props.leaf.value === "string" && props.leaf.value ? { id: props.leaf.value, name: props.leaf.value } : null));
</script>

<template>
  <fieldset v-if="list && hasChoices" class="wf-choices" :aria-label="label">
    <label v-for="c in choices" :key="c.key" class="checkbox-row">
      <input type="checkbox" :checked="values.includes(c.key)" @change="toggleChoice(c.key, ($event.target as HTMLInputElement).checked)" />
      {{ c.label }}<span v-if="c.inactive" class="muted"> {{ t("wfDesign.retired") }}</span>
    </label>
    <span v-if="choices.length === 0" class="muted">{{ t("wfDesign.value.noValues") }}</span>
  </fieldset>
  <input
    v-else-if="list"
    :id="id"
    type="text"
    :aria-label="label"
    :value="listText"
    :placeholder="type === 'reference' ? t('wfDesign.value.refList') : t('wfDesign.value.list')"
    @change="setListText(($event.target as HTMLInputElement).value)"
  />
  <select v-else-if="hasChoices" :id="id" v-model="leaf.value" :aria-label="label">
    <option value="" disabled>{{ t("wfDesign.value.choose") }}</option>
    <option v-for="c in choices" :key="c.key" :value="c.key">{{ c.label }}{{ c.inactive ? ` ${t("wfDesign.retired")}` : "" }}</option>
  </select>
  <select v-else-if="type === 'boolean'" :id="id" v-model="leaf.value" :aria-label="label">
    <option :value="true">{{ t("common.yes") }}</option>
    <option :value="false">{{ t("common.no") }}</option>
  </select>
  <input
    v-else-if="type === 'number' || type === 'integer'"
    :id="id"
    type="number"
    :step="type === 'integer' ? 1 : 'any'"
    :aria-label="label"
    :value="leaf.value"
    @input="leaf.value = parseOne(($event.target as HTMLInputElement).value)"
  />
  <input v-else-if="type === 'date'" :id="id" v-model="leaf.value" type="date" :aria-label="label" />
  <input
    v-else-if="type === 'datetime'"
    :id="id"
    type="datetime-local"
    :aria-label="label"
    :value="toLocalInput(leaf.value)"
    @change="setDatetime(($event.target as HTMLInputElement).value)"
  />
  <CiPicker
    v-else-if="type === 'reference'"
    :id="id"
    :class-id="field?.referenceClassId"
    :selected="refSelected"
    :placeholder="t('wfDesign.value.searchCi')"
    @select="leaf.value = $event?.id ?? ''"
  />
  <input v-else :id="id" v-model.trim="leaf.value" type="text" maxlength="1000" :aria-label="label" />
</template>
