<script setup lang="ts">
/** Picks any number of keys (classes, statuses, environments, locations) with checkboxes. */
defineProps<{ options: { key: string; label: string }[]; legend: string; hint?: string }>();
const model = defineModel<string[]>({ required: true });

function toggle(key: string, on: boolean) {
  model.value = on ? [...model.value, key] : model.value.filter((k) => k !== key);
}
</script>

<template>
  <fieldset class="key-checklist">
    <legend>{{ legend }}</legend>
    <p v-if="hint" class="hint">{{ hint }}</p>
    <label v-for="o in options" :key="o.key" class="check">
      <input type="checkbox" :checked="model.includes(o.key)" @change="toggle(o.key, ($event.target as HTMLInputElement).checked)" />
      {{ o.label }}
    </label>
    <p v-if="options.length === 0" class="muted">Nothing to choose from yet.</p>
  </fieldset>
</template>
