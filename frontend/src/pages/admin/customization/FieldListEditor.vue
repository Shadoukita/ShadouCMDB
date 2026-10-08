<script setup lang="ts">
import { computed, ref } from "vue";
import { moveItem } from "../../../lib/reorder";
import Icon from "../../../components/Icon.vue";
import { t } from "../../../i18n";

/**
 * An ordered list of fields (list columns, a layout panel's fields): move up or
 * down, remove, and add from the fields not in the list yet.
 */
const props = defineProps<{ options: { key: string; label: string }[]; label: string; idPrefix: string; emptyText?: string }>();
const model = defineModel<string[]>({ required: true });
const labelOf = (k: string) => props.options.find((o) => o.key === k)?.label ?? k;
const available = computed(() => props.options.filter((o) => !model.value.includes(o.key)));
const adding = ref("");

function add() {
  if (!adding.value) return;
  model.value = [...model.value, adding.value];
  adding.value = "";
}
const move = (i: number, to: number) => (model.value = moveItem(model.value, i, to));
const remove = (i: number) => (model.value = model.value.filter((_, j) => j !== i));
</script>

<template>
  <div class="field-list">
    <ol v-if="model.length > 0" :aria-label="label">
      <li v-for="(f, i) in model" :key="f">
        <span class="field-list-name">{{ labelOf(f) }} <code class="muted">{{ f }}</code></span>
        <span class="row-actions">
          <button type="button" class="btn btn-sm btn-icon" :disabled="i === 0" :aria-label="t('customization.moveUp', { name: labelOf(f) })" @click="move(i, i - 1)"><Icon name="arrow-up" /></button>
          <button type="button" class="btn btn-sm btn-icon" :disabled="i === model.length - 1" :aria-label="t('customization.moveDown', { name: labelOf(f) })" @click="move(i, i + 1)"><Icon name="arrow-down" /></button>
          <button type="button" class="btn btn-sm" :aria-label="t('customization.removeOf', { name: labelOf(f) })" @click="remove(i)">{{ t("customization.remove") }}</button>
        </span>
      </li>
    </ol>
    <p v-else class="muted">{{ emptyText ?? t("customization.fields.empty") }}</p>
    <div class="inline-control">
      <label class="sr-only" :for="`${idPrefix}-add`">{{ t("customization.fields.addTo", { list: label }) }}</label>
      <select :id="`${idPrefix}-add`" v-model="adding" :disabled="available.length === 0">
        <option value="">{{ available.length ? t("customization.fields.addField") : t("customization.fields.allIn") }}</option>
        <option v-for="o in available" :key="o.key" :value="o.key">{{ o.label }}</option>
      </select>
      <button type="button" class="btn btn-sm" :disabled="!adding" @click="add">{{ t("customization.add") }}</button>
    </div>
  </div>
</template>
