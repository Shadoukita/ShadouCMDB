<script setup lang="ts">
import { computed } from "vue";
import { useAllLookupListValues, useLookupLists } from "../api/datamodel";
import { useCiClasses, useCriticalityValues } from "../api/queries";
import { useUiSettings } from "../api/uiSettings";
import { t } from "../i18n";
import { idsOf } from "../lib/facets";
import { END_OF_LIFE_DAYS } from "../lib/inventoryQuery";
import type { useInventoryQueryState } from "../lib/useInventoryQueryState";
import Icon from "./Icon.vue";

/**
 * The applied filters as removable chips. One chip pattern for every list (audit I6). By default only
 * the filters without a control of their own in the toolbar (lookup values from a dashboard count or
 * the facet panel, an IP network, the layout filters from Customization › Layouts, a data-quality check from
 * the dashboard's "Needs attention"). With `all` (the
 * inventory, whose filter controls sit behind "Add filter"), class, criticality, validity and deleted
 * get a chip as well. Lookup values get one chip per list ("Status: In production, Maintenance").
 */
const props = defineProps<{ state: ReturnType<typeof useInventoryQueryState>; all?: boolean }>();
const s = computed(() => props.state);

interface Chip {
  key: string;
  label: string;
  value: string;
  remove: string;
  mono?: boolean;
  testid?: string;
  clear: () => void;
}

const classes = useCiClasses();
const criticality = useCriticalityValues();
const lookupLists = useLookupLists();
const lookupValues = useAllLookupListValues();
const loading = (q: { isLoading: { value: boolean } }) => (q.isLoading.value ? "…" : t("filters.unknownValue"));

const layoutTemplate = computed(() => s.value.get("layoutTemplate"));
const settings = useUiSettings(() => !!layoutTemplate.value);
const layoutTemplateName = computed(() => settings.data.value?.settings.layoutTemplates.find((x) => x.key === layoutTemplate.value)?.name ?? layoutTemplate.value);

/** The ids of a parameter, by name. */
const names = (raw: string, find: (id: string) => string | undefined, q: { isLoading: { value: boolean } }) =>
  idsOf(raw)
    .map((id) => find(id) ?? loading(q))
    .join(", ");

const chips = computed<Chip[]>(() => {
  const out: Chip[] = [];
  const st = s.value;
  if (props.all) {
    const classId = st.classId.value;
    if (classId) {
      const value = names(classId, (id) => classes.data.value?.find((c) => c.id === id)?.name, classes);
      out.push({ key: "class", label: t("filters.class"), value, remove: t("filters.remove", { name: t("filters.class"), value }), clear: () => void st.update({ classId: undefined }) });
    }
    const crit = st.get("criticalityValueId");
    if (crit) {
      const value = names(crit, (id) => criticality.data.value?.find((v) => v.id === id)?.name, criticality);
      out.push({
        key: "criticality",
        label: t("filters.criticality"),
        value,
        remove: t("filters.remove", { name: t("filters.criticality"), value }),
        clear: () => void st.update({ criticalityValueId: undefined }),
      });
    }
  }

  // Lookup values, one chip per list; removing a chip removes only that list's values.
  const byList = new Map<string, string[]>();
  for (const id of idsOf(st.get("lookupValueId"))) {
    const listId = lookupValues.data.value?.find((v) => v.id === id)?.listId ?? "";
    byList.set(listId, [...(byList.get(listId) ?? []), id]);
  }
  for (const [listId, ids] of byList) {
    const name = lookupLists.data.value?.find((l) => l.id === listId)?.name ?? t("filters.lookupValues");
    const value = names(ids.join(","), (id) => lookupValues.data.value?.find((v) => v.id === id)?.name, lookupValues);
    const rest = idsOf(st.get("lookupValueId")).filter((id) => !ids.includes(id));
    out.push({
      key: `lookup-${listId || "unknown"}`,
      label: name,
      value,
      remove: t("filters.remove", { name, value }),
      testid: "filter-lookup",
      clear: () => void st.update({ lookupValueId: rest.length ? rest.join(",") : undefined }),
    });
  }

  const ip = st.get("ipWithin");
  if (ip) out.push({ key: "ip", label: t("filters.ipWithin"), value: ip, mono: true, remove: t("filters.remove.ip"), clear: () => void st.update({ ipWithin: undefined }) });
  const own = st.get("ownLayout");
  if (own === "true" || own === "false")
    out.push({
      key: "layout",
      label: t("filters.layout"),
      value: own === "true" ? t("filters.layout.own") : t("filters.layout.classDefault"),
      remove: t("filters.remove.layout"),
      testid: "filter-own-layout",
      clear: () => void st.update({ ownLayout: undefined }),
    });
  if (layoutTemplate.value)
    out.push({
      key: "template",
      label: t("filters.layoutTemplate"),
      value: layoutTemplateName.value,
      remove: t("filters.remove.layoutTemplate"),
      testid: "filter-layout-template",
      clear: () => void st.update({ layoutTemplate: undefined }),
    });

  // A data-quality check from the dashboard's "Needs attention"; removing it removes its day count too.
  const quality = st.quality.value;
  if (quality) {
    const days = st.listQuery.value.endOfLifeWithinDays ?? END_OF_LIFE_DAYS.default;
    const value = t(`dashboard.attention.${quality}.title`, { days });
    out.push({
      key: "quality",
      label: t("filters.quality"),
      value,
      remove: t("filters.remove", { name: t("filters.quality"), value }),
      testid: "filter-quality",
      clear: () => void st.update({ quality: undefined, endOfLifeWithinDays: undefined }),
    });
  }

  if (props.all) {
    const active = st.active.value;
    if (active === "all" || active === "false") {
      const value = active === "all" ? t("filters.validity.all") : t("filters.validity.inactive");
      out.push({ key: "validity", label: t("filters.validity"), value, remove: t("filters.remove", { name: t("filters.validity"), value }), clear: () => void st.update({ active: undefined }) });
    }
    const deleted = st.deleted.value;
    if (deleted === "include" || deleted === "only") {
      const value = deleted === "include" ? t("filters.deleted.include") : t("filters.deleted.only");
      out.push({ key: "deleted", label: t("filters.deleted"), value, remove: t("filters.remove", { name: t("filters.deleted"), value }), clear: () => void st.update({ deleted: undefined }) });
    }
  }
  return out;
});
</script>

<template>
  <div v-if="chips.length > 0" class="filter-chips" role="group" :aria-label="t('filters.applied')">
    <span v-for="c in chips" :key="c.key" class="chip" :data-chip="c.key" :data-testid="c.testid">
      <span class="key">{{ c.label }}:</span>
      <bdi :class="{ value: c.mono }" :title="c.value">{{ c.value }}</bdi>
      <button type="button" class="chip-clear" :aria-label="c.remove" :title="c.remove" @click="c.clear()"><Icon name="x" :size="14" /></button>
    </span>
  </div>
</template>
