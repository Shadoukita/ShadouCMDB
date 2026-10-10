<script setup lang="ts">
import { computed } from "vue";
import type { AttributeDefinition } from "../../../api/datamodel";
import { t } from "../../../i18n";
import {
  MAX_CONDITION_DEPTH,
  defaultValue,
  opLabel,
  opTakesValue,
  opsFor,
  type ConditionGroup,
  type ConditionLeaf,
  type ConditionOp,
  type FieldDataType,
} from "../../../lib/workflowDraft";
import ConditionValueInput from "./ConditionValueInput.vue";

/**
 * A group of conditions (all must hold / any must hold) with its leaves and nested groups, at most
 * MAX_CONDITION_DEPTH levels deep. Leaves compare one field of the workflow's type (§3.3);
 * relationship conditions are not part of v0.4.0 (Q4).
 */
const props = defineProps<{ group: ConditionGroup; fields: AttributeDefinition[]; depth: number; idPrefix: string; disabled?: boolean }>();
const emit = defineEmits<{ remove: [] }>();

const byKey = computed(() => new Map(props.fields.map((f) => [f.key, f])));
const typeOf = (key: string) => (byKey.value.get(key)?.dataType ?? "text") as FieldDataType;

function addLeaf() {
  const f = props.fields[0];
  props.group.children.push({ kind: "leaf", field: f?.key ?? "", op: "eq", value: defaultValue((f?.dataType ?? "text") as FieldDataType, "eq") });
}
function addGroup() {
  props.group.children.push({ kind: "group", mode: props.group.mode === "all" ? "any" : "all", children: [] });
}
function remove(i: number) {
  props.group.children.splice(i, 1);
}

function setField(leaf: ConditionLeaf, key: string) {
  leaf.field = key;
  const ops = opsFor(typeOf(key));
  if (!ops.includes(leaf.op)) leaf.op = "eq";
  leaf.value = defaultValue(typeOf(key), leaf.op);
}
function setOp(leaf: ConditionLeaf, op: ConditionOp) {
  const wasList = Array.isArray(leaf.value);
  leaf.op = op;
  const nowList = op === "in" || op === "notIn";
  if (!opTakesValue(op)) delete leaf.value;
  else if (leaf.value === undefined || wasList !== nowList) leaf.value = defaultValue(typeOf(leaf.field), op);
}
</script>

<template>
  <div :class="['wf-cond-group', { nested: depth > 1 }]" role="group" :aria-label="depth === 1 ? t('wfDesign.tr.conditions') : t('wfDesign.cond.group')">
    <div class="wf-cond-head">
      <select v-model="group.mode" :aria-label="depth === 1 ? t('wfDesign.cond.modeRoot') : t('wfDesign.cond.modeGroup')" :disabled="disabled">
        <option value="all">{{ t("wfDesign.cond.all") }}</option>
        <option value="any">{{ t("wfDesign.cond.any") }}</option>
      </select>
      <button type="button" class="btn btn-sm" :disabled="disabled || fields.length === 0" @click="addLeaf">{{ t("wfDesign.cond.add") }}</button>
      <button v-if="depth < MAX_CONDITION_DEPTH" type="button" class="btn btn-sm" :disabled="disabled" @click="addGroup">{{ t("wfDesign.cond.addGroup") }}</button>
      <button v-if="depth > 1" type="button" class="btn btn-sm btn-quiet-danger" :disabled="disabled" @click="emit('remove')">{{ t("wfDesign.cond.removeGroup") }}</button>
    </div>
    <p v-if="group.children.length === 0" class="muted no-margin">
      {{ depth === 1 ? t("wfDesign.cond.none") : t("wfDesign.cond.emptyGroup") }}
    </p>
    <template v-for="(c, i) in group.children" :key="i">
      <ConditionGroupEditor
        v-if="c.kind === 'group'"
        :group="c"
        :fields="fields"
        :depth="depth + 1"
        :id-prefix="`${idPrefix}-${i}`"
        :disabled="disabled"
        @remove="remove(i)"
      />
      <div v-else class="wf-cond-leaf">
        <select :value="c.field" :aria-label="t('wfDesign.cond.field', { n: i + 1 })" :disabled="disabled" @change="setField(c, ($event.target as HTMLSelectElement).value)">
          <option v-if="!byKey.has(c.field)" :value="c.field" disabled>{{ c.field || t("wfDesign.cond.chooseField") }}</option>
          <option v-for="f in fields" :key="f.key" :value="f.key">{{ f.label }}</option>
        </select>
        <select :value="c.op" :aria-label="t('wfDesign.cond.op', { n: i + 1 })" :disabled="disabled" @change="setOp(c, ($event.target as HTMLSelectElement).value as ConditionOp)">
          <option v-for="op in opsFor(typeOf(c.field))" :key="op" :value="op">{{ opLabel(op) }}</option>
        </select>
        <div v-if="opTakesValue(c.op)" class="wf-cond-value">
          <ConditionValueInput :id="`${idPrefix}-${i}-value`" :leaf="c" :field="byKey.get(c.field)" :label="t('wfDesign.cond.value', { n: i + 1 })" />
        </div>
        <button type="button" class="btn btn-sm btn-quiet-danger" :aria-label="t('wfDesign.cond.remove', { n: i + 1 })" :disabled="disabled" @click="remove(i)">{{ t("wfApproval.remove") }}</button>
      </div>
    </template>
  </div>
</template>
