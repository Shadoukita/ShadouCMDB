<script setup lang="ts">
import { computed, ref, watch } from "vue";
import CiLink from "../../components/CiLink.vue";
import CiStateBadge from "../../components/CiStateBadge.vue";
import CriticalityBadge from "../../components/CriticalityBadge.vue";
import { edgeLabel, groupItems, pathTo, type ImpactAnalysis, type ImpactGroup, type ImpactItem } from "../../lib/impact";
import type { TrailStep } from "../../lib/trail";
import Icon from "../../components/Icon.vue";
import { t } from "../../i18n";
import SortIcon from "../../components/SortIcon.vue";

/**
 * The impact analysis as a dense table (§1.4): grouped, with a collapsible header per group,
 * sortable columns, the last hop ("runs on db-01") as a link and "Show path" for the whole chain.
 * The result is bounded by the server, so it is sorted and grouped here, never paged.
 */
const props = defineProps<{ analysis: ImpactAnalysis; group: ImpactGroup; sort: string; self: TrailStep; trail: TrailStep[] }>();
const emit = defineEmits<{ sort: [sort: string] }>();

const byId = computed(() => new Map(props.analysis.items.map((i) => [i.id, i])));
const root = computed(() => ({ id: props.analysis.root.id, name: props.analysis.root.name }));
const parentName = (id: string) => (id === root.value.id ? root.value.name : (byId.value.get(id)?.name ?? ""));
const viaName = (i: ImpactItem) => parentName(i.via.parentId);
const groups = computed(() => groupItems(props.analysis.items, props.group, props.sort, viaName));
const both = computed(() => props.analysis.parameters.direction === "both");
const columnCount = computed(() => (both.value ? 9 : 8));

const collapsed = ref(new Set<string>());
const openPaths = ref(new Set<string>());
watch(
  () => [props.analysis, props.group],
  () => {
    collapsed.value = new Set();
    openPaths.value = new Set();
  },
);
const flip = (set: typeof collapsed, key: string) => {
  const next = new Set(set.value);
  if (next.has(key)) next.delete(key);
  else next.add(key);
  set.value = next;
};
const toggleGroup = (key: string) => flip(collapsed, key);
const togglePath = (id: string) => flip(openPaths, id);

const COLUMNS = computed(() => [
  { key: "name", label: t("impact.col.ci") },
  { key: "class", label: t("ciField.class") },
  { key: "criticality", label: t("ciField.criticality") },
  { key: "hops", label: t("impact.services.col.hops") },
  ...(both.value ? [{ key: "direction", label: t("impact.direction") }] : []),
  { key: "via", label: t("impact.services.col.via") },
  { key: "status", label: t("impact.col.status") },
  { key: "active", label: t("ciField.active") },
]);
const field = computed(() => props.sort.replace(/^-/, ""));
const ariaSort = (key: string) => (field.value !== key ? "none" : props.sort.startsWith("-") ? "descending" : "ascending");
const toggleSort = (key: string) => emit("sort", props.sort === key ? `-${key}` : key);

const directionLabel = (i: ImpactItem) =>
  t(i.directions.length > 1 ? "impact.dir.both" : i.directions[0] === "downstream" ? "impact.dir.downstream" : "impact.dir.upstream");
const pathOf = (i: ImpactItem) => pathTo(i, root.value, byId.value);
</script>

<template>
  <div class="table-wrap">
    <table class="data impact-table">
      <caption class="sr-only">{{ t("impact.list.caption") }}</caption>
      <thead>
        <tr>
          <th v-for="c in COLUMNS" :key="c.key" scope="col" :aria-sort="ariaSort(c.key)">
            <button type="button" class="sort" @click="toggleSort(c.key)">{{ c.label }} <SortIcon :dir="ariaSort(c.key)" /></button>
          </th>
          <th scope="col"><span class="sr-only">{{ t("impact.list.path") }}</span></th>
        </tr>
      </thead>
      <tbody v-for="g in groups" :key="g.key">
        <tr v-if="group !== 'none'" class="group-row">
          <th :colspan="columnCount" scope="rowgroup">
            <button type="button" class="group-toggle" :aria-expanded="!collapsed.has(g.key)" @click="toggleGroup(g.key)">
              <Icon :name="collapsed.has(g.key) ? 'chevron-right' : 'chevron-down'" />{{ " " }}<bdi>{{ g.label }}</bdi> <span class="muted">({{ g.items.length.toLocaleString() }})</span>
            </button>
          </th>
        </tr>
        <template v-if="!collapsed.has(g.key)">
          <template v-for="i in g.items" :key="i.id">
            <tr>
              <td>
                <CiLink :id="i.id" :from="self" :trail="trail">{{ i.name }}</CiLink>
                <span class="mono muted impact-ident">{{ i.ident }}</span>
              </td>
              <td><bdi>{{ i.className }}</bdi></td>
              <td><CriticalityBadge :value="i.criticality" show-unset /></td>
              <td class="num">{{ i.hops }}</td>
              <td v-if="both">{{ directionLabel(i) }}</td>
              <td>
                <span class="muted"><bdi>{{ edgeLabel(i.via, i.id) }}</bdi></span>{{ " " }}
                <CiLink :id="i.via.parentId" :from="self" :trail="trail">{{ parentName(i.via.parentId) }}</CiLink>
              </td>
              <td>
                <bdi v-if="i.status">{{ i.status.label }}</bdi><span v-else class="muted">—</span>
              </td>
              <td><CiStateBadge :ci="i" show-active /></td>
              <td>
                <button
                  type="button"
                  class="btn btn-sm"
                  :aria-expanded="openPaths.has(i.id)"
                  :aria-controls="`path-${i.id}`"
                  :aria-label="t(openPaths.has(i.id) ? 'impact.list.hidePathTo' : 'impact.list.showPathTo', { name: i.name })"
                  @click="togglePath(i.id)"
                >
                  {{ openPaths.has(i.id) ? t("impact.list.hidePath") : t("impact.list.showPath") }}
                </button>
              </td>
            </tr>
            <tr v-if="openPaths.has(i.id)" :id="`path-${i.id}`" class="path-row">
              <td :colspan="columnCount">
                <nav :aria-label="t('impact.list.pathTo', { name: i.name })">
                  <ol class="impact-path">
                    <li v-for="s in pathOf(i)" :key="s.id">
                      <span v-if="s.label" class="muted"><bdi>{{ s.label }}</bdi> → </span>
                      <CiLink :id="s.id" :from="self" :trail="trail">{{ s.name }}</CiLink>
                    </li>
                  </ol>
                </nav>
              </td>
            </tr>
          </template>
        </template>
      </tbody>
    </table>
  </div>
</template>
