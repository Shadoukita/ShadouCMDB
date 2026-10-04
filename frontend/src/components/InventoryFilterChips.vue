<script setup lang="ts">
import { computed } from "vue";
import { useAllLookupListValues } from "../api/datamodel";
import { useUiSettings } from "../api/uiSettings";
import { t } from "../i18n";
import type { useInventoryQueryState } from "../lib/useInventoryQueryState";
import Icon from "./Icon.vue";

/**
 * The filters a link or a list view set that have no control of their own in the toolbar (lookup values
 * from a dashboard count, an IP network, the layout filters from Customization › Layouts), as removable
 * chips under the toolbar. One chip pattern for every list (audit I6).
 */
const props = defineProps<{ state: ReturnType<typeof useInventoryQueryState> }>();
const s = computed(() => props.state);

const lookupValues = useAllLookupListValues();
/** The lookup values the list is filtered by, by name. */
const lookupFilterNames = computed(() =>
  s.value
    .get("lookupValueId")
    .split(",")
    .filter(Boolean)
    .map((id) => lookupValues.data.value?.find((v) => v.id === id)?.name ?? (lookupValues.isLoading.value ? "…" : t("filters.unknownValue"))),
);
const ipWithin = computed(() => s.value.get("ipWithin"));
const ownLayout = computed(() => s.value.get("ownLayout"));
const layoutTemplate = computed(() => s.value.get("layoutTemplate"));
const settings = useUiSettings(() => !!layoutTemplate.value);
const layoutTemplateName = computed(() => settings.data.value?.settings.layoutTemplates.find((x) => x.key === layoutTemplate.value)?.name ?? layoutTemplate.value);
const any = computed(() => lookupFilterNames.value.length > 0 || !!ipWithin.value || ownLayout.value === "true" || ownLayout.value === "false" || !!layoutTemplate.value);
</script>

<template>
  <div v-if="any" class="filter-chips" role="group" :aria-label="t('filters.applied')">
    <span v-if="lookupFilterNames.length > 0" class="chip">
      <span class="key">{{ t("filters.lookupValues") }}:</span>
      <span>{{ lookupFilterNames.join(", ") }}</span>
      <button type="button" class="chip-clear" :aria-label="t('filters.remove.lookup')" @click="s.update({ lookupValueId: undefined })"><Icon name="x" :size="14" /></button>
    </span>
    <span v-if="ipWithin" class="chip">
      <span class="key">{{ t("filters.ipWithin") }}:</span>
      <span class="value">{{ ipWithin }}</span>
      <button type="button" class="chip-clear" :aria-label="t('filters.remove.ip')" @click="s.update({ ipWithin: undefined })"><Icon name="x" :size="14" /></button>
    </span>
    <span v-if="ownLayout === 'true' || ownLayout === 'false'" class="chip" data-testid="filter-own-layout">
      <span class="key">{{ t("filters.layout") }}:</span>
      <span>{{ ownLayout === "true" ? t("filters.layout.own") : t("filters.layout.classDefault") }}</span>
      <button type="button" class="chip-clear" :aria-label="t('filters.remove.layout')" @click="s.update({ ownLayout: undefined })"><Icon name="x" :size="14" /></button>
    </span>
    <span v-if="layoutTemplate" class="chip" data-testid="filter-layout-template">
      <span class="key">{{ t("filters.layoutTemplate") }}:</span>
      <span>{{ layoutTemplateName }}</span>
      <button type="button" class="chip-clear" :aria-label="t('filters.remove.layoutTemplate')" @click="s.update({ layoutTemplate: undefined })"><Icon name="x" :size="14" /></button>
    </span>
  </div>
</template>
