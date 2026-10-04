<script setup lang="ts">
import { computed } from "vue";
import { useAreas } from "../api/datamodel";
import { t } from "../i18n";
import { useCiClasses, useCriticalityValues } from "../api/queries";
import { groupByArea } from "../lib/areas";
import { viewableClasses } from "../lib/permissions";
import { flattenTree } from "../lib/tree";
import type { useInventoryQueryState } from "../lib/useInventoryQueryState";
import { useSessionStore } from "../stores/session";

/**
 * The class, criticality, validity and deleted filters of the inventory and of
 * the search page, bound to their shared URL state. `idPrefix` keeps the field
 * ids unique per page. Filters without a control of their own (lookup values, IP
 * network, layout) are chips under the toolbar: InventoryFilterChips.
 */
const props = defineProps<{ state: ReturnType<typeof useInventoryQueryState>; idPrefix: string }>();
const s = computed(() => props.state);

const classes = useCiClasses();
const areas = useAreas();
const session = useSessionStore();
// The class filter offers only what the user may view; the API would answer any other class with an empty list.
const classOptions = computed(() => viewableClasses(classes.data.value ?? [], (id) => session.canOnClass(id, "view")));
const selected = computed(() => classes.data.value?.find((c) => c.id === s.value.classId.value));
const selectedDenied = computed(() => !!selected.value && !classOptions.value.some((c) => c.id === selected.value!.id));
const classGroups = computed(() => groupByArea(flattenTree(classOptions.value), (n) => n.item.areaId, areas.data.value ?? []));

const value = (e: Event) => (e.target as HTMLSelectElement).value || undefined;

// Criticality (a core field of every CI): one value, or a set from a link (criticalityValueId=a,b).
const criticality = useCriticalityValues();
const criticalityValue = computed(() => s.value.get("criticalityValueId"));
const criticalityMany = computed(() => criticalityValue.value.includes(","));
</script>

<template>
  <div class="field">
    <label :for="`${idPrefix}-class`">{{ t("filters.class") }}</label>
    <select :id="`${idPrefix}-class`" :value="s.classId.value" @change="s.update({ classId: value($event) })">
      <option value="">{{ t("filters.class.all") }}</option>
      <option v-if="selectedDenied && selected" :value="selected.id">{{ selected.name }}</option>
      <optgroup v-for="g in classGroups" :key="g.area?.id ?? '-'" :label="g.area?.name ?? t('filters.class.otherArea')">
        <option v-for="n in g.items" :key="n.item.id" :value="n.item.id">
          {{ "\u00a0\u00a0".repeat(n.depth) }}{{ n.item.name }}{{ n.item.isAbstract ? ` ${t("filters.class.withSubclasses")}` : "" }}{{ n.item.isActive ? "" : ` ${t("filters.class.archived")}` }}
        </option>
      </optgroup>
    </select>
  </div>
  <div v-if="criticality.data.value?.length || criticalityValue" class="field">
    <label :for="`${idPrefix}-criticality`">{{ t("filters.criticality") }}</label>
    <select :id="`${idPrefix}-criticality`" :value="criticalityValue" @change="s.update({ criticalityValueId: value($event) })">
      <option value="">{{ t("filters.any") }}</option>
      <option v-for="v in criticality.data.value ?? []" :key="v.id" :value="v.id">{{ v.name }}{{ v.isActive ? "" : ` ${t("filters.retired")}` }}</option>
      <option v-if="criticalityMany" :value="criticalityValue">{{ t("filters.severalValues") }}</option>
    </select>
  </div>
  <div class="field">
    <label :for="`${idPrefix}-active`">{{ t("filters.validity") }}</label>
    <select :id="`${idPrefix}-active`" :value="s.active.value ?? ''" @change="s.update({ active: value($event) })">
      <option value="">{{ t("filters.validity.active") }}</option>
      <option value="all">{{ t("filters.validity.all") }}</option>
      <option value="false">{{ t("filters.validity.inactive") }}</option>
    </select>
  </div>
  <div class="field">
    <label :for="`${idPrefix}-deleted`">{{ t("filters.deleted") }}</label>
    <select :id="`${idPrefix}-deleted`" :value="s.deleted.value ?? ''" @change="s.update({ deleted: value($event) })">
      <option value="">{{ t("filters.deleted.hide") }}</option>
      <option value="include">{{ t("filters.deleted.include") }}</option>
      <option value="only">{{ t("filters.deleted.only") }}</option>
    </select>
  </div>
</template>
