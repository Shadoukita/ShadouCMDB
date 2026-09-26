<script setup lang="ts">
import { computed } from "vue";
import { useLookup, type LookupKind } from "../api/queries";

/** <select> over a lookup list (status, environment, owner, location). Retired rows are hidden unless selected. */
const props = defineProps<{
  kind: LookupKind;
  id: string;
  emptyLabel: string;
  invalid?: boolean;
  describedBy?: string;
  required?: boolean;
}>();
const model = defineModel<string>({ required: true });
const { data, isLoading, isError } = useLookup(props.kind);
const options = computed(() => (data.value ?? []).filter((o) => o.isActive || o.id === model.value));
/** A filter can hold several ids ("a,b", e.g. from a list view's default filters); name them in one option. */
const multi = computed(() => {
  if (!model.value.includes(",")) return null;
  const ids = model.value.split(",");
  return ids.map((id) => data.value?.find((o) => o.id === id)?.name ?? "?").join(", ");
});
</script>

<template>
  <select
    :id="id"
    v-model="model"
    :aria-invalid="invalid || undefined"
    :aria-describedby="describedBy"
    :required="required"
    :disabled="isLoading"
  >
    <option value="">{{ isLoading ? "Loading…" : isError ? "Could not load options" : emptyLabel }}</option>
    <option v-if="multi" :value="model">{{ multi }}</option>
    <option v-for="o in options" :key="o.id" :value="o.id">
      {{ "\u00a0\u00a0".repeat(o.depth ?? 0) }}{{ o.name }}{{ o.isActive ? "" : " (retired)" }}
    </option>
  </select>
</template>
