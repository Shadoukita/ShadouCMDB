<script setup lang="ts">
import { computed, ref } from "vue";
import { useAllLookupListValues } from "../api/datamodel";
import { useCiClasses, useCiFacets, type Facet, type FacetsQuery } from "../api/queries";
import { formatNumber, t } from "../i18n";
import { isFacetParam, toggleId, visibleValues } from "../lib/facets";
import type { useInventoryQueryState } from "../lib/useInventoryQueryState";
import ClassBadge from "./ClassBadge.vue";
import Icon from "./Icon.vue";
import SkeletonRows from "./SkeletonRows.vue";

/**
 * The inventory's facet panel (design document §2.7, SHAA-1670 rollout 5f): per class, criticality
 * and lookup list (status, environment, location, ...), the values with how many CIs match. Each
 * facet is counted by the API with its own filter left out, so a count is what ticking one more
 * value adds. Ticking a value writes the same URL parameter as the toolbar, the query bar and the
 * chips, so they stay in step and the view survives a reload.
 */
const props = defineProps<{
  state: ReturnType<typeof useInventoryQueryState>;
  /** The list's filters (GET /configuration-items without sort and paging). */
  filters: FacetsQuery;
  /** Whether the list's query is settled (see useInventoryQueryState). */
  enabled: boolean;
  /** Keys of the groups the operator collapsed. */
  collapsed: readonly string[];
}>();
const emit = defineEmits<{ toggleGroup: [key: string] }>();

const query = useCiFacets(
  () => props.filters,
  () => props.enabled,
);
const classes = useCiClasses();
const lookupValues = useAllLookupListValues();
const classById = computed(() => new Map((classes.data.value ?? []).map((c) => [c.id, c])));
const colorById = computed(() => new Map((lookupValues.data.value ?? []).map((v) => [v.id, v.color ?? undefined])));
const classOf = (id: string) => classById.value.get(id);
const colorOf = (id: string) => colorById.value.get(id);

/** The facets the URL can hold, with something to show. */
const facets = computed(() => (query.data.value?.facets ?? []).filter((f) => isFacetParam(f.param) && f.values.length > 0));

const expanded = ref(new Set<string>());
function toggleExpanded(key: string) {
  const next = new Set(expanded.value);
  if (!next.delete(key)) next.add(key);
  expanded.value = next;
}

function title(f: Facet): string {
  if (f.kind === "class") return t("filters.class");
  if (f.kind === "criticality") return t("filters.criticality");
  return f.label;
}
const domId = (f: Facet) => `facet-${f.key.replace(/[^A-Za-z0-9_-]/g, "-")}`;

function onTick(f: Facet, id: string, e: Event) {
  const on = (e.target as HTMLInputElement).checked;
  void props.state.update({ [f.param]: toggleId(props.state.get(f.param), id, on) });
}
</script>

<template>
  <section id="facets" class="facets" aria-labelledby="facets-title">
    <h2 id="facets-title" class="sr-only">{{ t("facets.title") }}</h2>
    <SkeletonRows v-if="query.isPending.value && enabled" :label="t('facets.loading')" :rows="6" />
    <p v-else-if="query.isError.value" class="facets-error">
      <Icon name="circle-alert" />{{ t("facets.error") }}
      <button type="button" class="btn btn-sm" @click="query.refetch()">{{ t("common.retry") }}</button>
    </p>
    <p v-else-if="query.data.value && facets.length === 0" class="facets-empty">{{ t("facets.none") }}</p>
    <div v-else :class="['facets-groups', { stale: query.isPlaceholderData.value }]">
      <section v-for="f in facets" :key="f.key" class="facet" :data-facet="f.key">
        <h3 class="facet-title">
          <button
            :id="`${domId(f)}-title`"
            type="button"
            class="facet-toggle"
            :aria-expanded="!collapsed.includes(f.key)"
            :aria-controls="`${domId(f)}-values`"
            @click="emit('toggleGroup', f.key)"
          >
            <Icon :name="collapsed.includes(f.key) ? 'chevron-right' : 'chevron-down'" :size="14" /><bdi>{{ title(f) }}</bdi>
          </button>
        </h3>
        <template v-if="!collapsed.includes(f.key)">
          <div :id="`${domId(f)}-values`" class="facet-values" role="group" :aria-label="t('facets.group', { name: title(f) })">
            <label v-for="v in visibleValues(f.values, expanded.has(f.key))" :key="v.id" class="facet-row" :class="{ selected: v.selected }">
              <input type="checkbox" :checked="v.selected" @change="onTick(f, v.id, $event)" />
              <ClassBadge v-if="f.kind === 'class'" :icon="classOf(v.id)?.icon" :color="classOf(v.id)?.color" />
              <span v-else-if="colorOf(v.id)" class="class-swatch" aria-hidden="true" :style="{ background: colorOf(v.id) }" />
              <bdi class="facet-label" :title="v.label">{{ v.label }}</bdi>
              <span class="facet-count">{{ formatNumber(v.count) }}</span>
            </label>
          </div>
          <button
            v-if="f.values.length > visibleValues(f.values, false).length"
            type="button"
            class="btn btn-sm btn-ghost facet-more"
            :aria-expanded="expanded.has(f.key)"
            :aria-controls="`${domId(f)}-values`"
            @click="toggleExpanded(f.key)"
          >
            {{ expanded.has(f.key) ? t("facets.less") : t("facets.more", { n: formatNumber(f.values.length - visibleValues(f.values, false).length) }) }}
          </button>
          <p v-if="f.truncated && (expanded.has(f.key) || f.values.length <= visibleValues(f.values, false).length)" class="facet-note">
            {{ t("facets.truncated", { n: formatNumber(f.values.length) }) }}
          </p>
        </template>
      </section>
    </div>
  </section>
</template>
