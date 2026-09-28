<script setup lang="ts">
import { computed } from "vue";
import { useAllLookupListValues, useLookupLists } from "../../../api/datamodel";
import type { UiListFilters } from "../../../api/uiSettings";
import KeyChecklist from "./KeyChecklist.vue";

/**
 * Lookup filters of a list view or saved search: per lookup list, the values a CI must hold one of.
 * Lists and values are stored by key; a list with nothing ticked is dropped.
 */
const props = defineProps<{ filters: UiListFilters; legendPrefix?: string }>();
const lists = useLookupLists();
const values = useAllLookupListValues();
const options = computed(() =>
  (lists.data.value ?? []).map((l) => ({
    list: l,
    values: (values.data.value ?? []).filter((v) => v.listId === l.id).map((v) => ({ key: v.key, label: v.name + (v.isActive ? "" : " (retired)") })),
  })),
);

function set(listKey: string, keys: string[]) {
  const next = { ...props.filters.lookups };
  if (keys.length > 0) next[listKey] = keys;
  else delete next[listKey];
  props.filters.lookups = next;
}
</script>

<template>
  <KeyChecklist
    v-for="o in options"
    :key="o.list.id"
    :model-value="filters.lookups?.[o.list.key] ?? []"
    :legend="`${legendPrefix ?? ''}${o.list.name}`"
    :hint="`None ticked: any ${o.list.name.toLowerCase()}`"
    :options="o.values"
    @update:model-value="(v) => set(o.list.key, v)"
  />
  <p v-if="lists.data.value && lists.data.value.length === 0" class="hint">No lookup lists are defined, so there is nothing else to filter by.</p>
</template>
