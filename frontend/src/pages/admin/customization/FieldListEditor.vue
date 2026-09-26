<script setup lang="ts">
import { computed, ref } from "vue";
import { moveItem } from "../../../lib/reorder";

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
          <button type="button" class="btn btn-sm" :disabled="i === 0" :aria-label="`Move ${labelOf(f)} up`" @click="move(i, i - 1)">↑</button>
          <button type="button" class="btn btn-sm" :disabled="i === model.length - 1" :aria-label="`Move ${labelOf(f)} down`" @click="move(i, i + 1)">↓</button>
          <button type="button" class="btn btn-sm" :aria-label="`Remove ${labelOf(f)}`" @click="remove(i)">Remove</button>
        </span>
      </li>
    </ol>
    <p v-else class="muted">{{ emptyText ?? "None yet." }}</p>
    <div class="inline-control">
      <label class="sr-only" :for="`${idPrefix}-add`">Add to {{ label }}</label>
      <select :id="`${idPrefix}-add`" v-model="adding" :disabled="available.length === 0">
        <option value="">{{ available.length ? "Add a field…" : "Every field is in the list" }}</option>
        <option v-for="o in available" :key="o.key" :value="o.key">{{ o.label }}</option>
      </select>
      <button type="button" class="btn btn-sm" :disabled="!adding" @click="add">Add</button>
    </div>
  </div>
</template>
