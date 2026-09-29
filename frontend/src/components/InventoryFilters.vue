<script setup lang="ts">
import { computed } from "vue";
import { useAllLookupListValues, useAreas } from "../api/datamodel";
import { useCiClasses } from "../api/queries";
import { groupByArea } from "../lib/areas";
import { viewableClasses } from "../lib/permissions";
import { flattenTree } from "../lib/tree";
import type { useInventoryQueryState } from "../lib/useInventoryQueryState";
import { useSessionStore } from "../stores/session";

/**
 * The class, lookup value, validity and deleted filters of the inventory and of
 * the search page, bound to their shared URL state. `idPrefix` keeps the field
 * ids unique per page.
 */
const props = defineProps<{ state: ReturnType<typeof useInventoryQueryState>; idPrefix: string }>();
const s = computed(() => props.state);

const classes = useCiClasses();
const areas = useAreas();
const session = useSessionStore();
const lookupValues = useAllLookupListValues();
// The class filter offers only what the user may view; the API would answer any other class with an empty list.
const classOptions = computed(() => viewableClasses(classes.data.value ?? [], (id) => session.canOnClass(id, "view")));
const selected = computed(() => classes.data.value?.find((c) => c.id === s.value.classId.value));
const selectedDenied = computed(() => !!selected.value && !classOptions.value.some((c) => c.id === selected.value!.id));
const classGroups = computed(() => groupByArea(flattenTree(classOptions.value), (n) => n.item.areaId, areas.data.value ?? []));

/** The lookup values the list is filtered by (from a dashboard link or a list view's default filters), by name. */
const lookupFilterNames = computed(() =>
  s.value
    .get("lookupValueId")
    .split(",")
    .filter(Boolean)
    .map((id) => lookupValues.data.value?.find((v) => v.id === id)?.name ?? (lookupValues.isLoading.value ? "…" : "Unknown value")),
);
const value = (e: Event) => (e.target as HTMLSelectElement).value || undefined;
</script>

<template>
  <div class="field">
    <label :for="`${idPrefix}-class`">Class</label>
    <select :id="`${idPrefix}-class`" :value="s.classId.value" @change="s.update({ classId: value($event) })">
      <option value="">All classes</option>
      <option v-if="selectedDenied && selected" :value="selected.id">{{ selected.name }}</option>
      <optgroup v-for="g in classGroups" :key="g.area?.id ?? '-'" :label="g.area?.name ?? 'Other'">
        <option v-for="n in g.items" :key="n.item.id" :value="n.item.id">
          {{ "  ".repeat(n.depth) }}{{ n.item.name }}{{ n.item.isAbstract ? " (incl. subclasses)" : "" }}{{ n.item.isActive ? "" : " (archived)" }}
        </option>
      </optgroup>
    </select>
  </div>
  <div v-if="lookupFilterNames.length > 0" class="field">
    <span class="label">Lookup values</span>
    <span class="checkbox-row">
      {{ lookupFilterNames.join(", ") }}
      <button type="button" class="btn btn-sm" aria-label="Remove the lookup value filter" @click="s.update({ lookupValueId: undefined })">×</button>
    </span>
  </div>
  <div v-if="s.get('ipWithin')" class="field">
    <span class="label">IP within</span>
    <span class="checkbox-row">
      <span class="mono">{{ s.get("ipWithin") }}</span>
      <button type="button" class="btn btn-sm" aria-label="Remove the IP network filter" @click="s.update({ ipWithin: undefined })">×</button>
    </span>
  </div>
  <div class="field">
    <label :for="`${idPrefix}-active`">Validity</label>
    <select :id="`${idPrefix}-active`" :value="s.active.value ?? ''" @change="s.update({ active: value($event) })">
      <option value="">Active only</option>
      <option value="all">Show inactive</option>
      <option value="false">Only inactive</option>
    </select>
  </div>
  <div class="field">
    <label :for="`${idPrefix}-deleted`">Deleted CIs</label>
    <select :id="`${idPrefix}-deleted`" :value="s.deleted.value ?? ''" @change="s.update({ deleted: value($event) })">
      <option value="">Hide</option>
      <option value="include">Include</option>
      <option value="only">Only deleted</option>
    </select>
  </div>
</template>
