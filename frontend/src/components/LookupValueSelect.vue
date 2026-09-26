<script setup lang="ts">
import { computed } from "vue";
import { useLookupListValues } from "../api/datamodel";

/** <select> over the values of an admin-defined lookup list (`lookup` attributes). Retired values are hidden unless selected. */
const props = defineProps<{ listId: string | null; id: string; invalid?: boolean; describedBy?: string }>();
const model = defineModel<string>({ required: true });
const values = useLookupListValues(() => props.listId);
const options = computed(() => (values.data.value ?? []).filter((v) => v.isActive || v.id === model.value));
</script>

<template>
  <select :id="id" v-model="model" :aria-invalid="invalid || undefined" :aria-describedby="describedBy" :disabled="values.isLoading.value">
    <option value="">{{ values.isLoading.value ? "Loading…" : values.isError.value ? "Could not load the list" : "— not set —" }}</option>
    <option v-for="v in options" :key="v.id" :value="v.id">{{ v.name }}{{ v.isActive ? "" : " (retired)" }}</option>
    <option v-if="model !== '' && values.data.value && !options.some((o) => o.id === model)" :value="model">Unknown value</option>
  </select>
</template>
