<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { RouterLink } from "vue-router";
import { useAllLookupListValues, useLookupLists } from "../api/datamodel";
import { useCiClasses, useSearch } from "../api/queries";
import { useSavedViews } from "../api/savedViews";
import Breadcrumbs from "../components/Breadcrumbs.vue";
import EmptyState from "../components/EmptyState.vue";
import ErrorAlert from "../components/ErrorAlert.vue";
import InventoryFilters from "../components/InventoryFilters.vue";
import SavedViewMenu from "../components/savedViews/SavedViewMenu.vue";
import PaginationBar from "../components/PaginationBar.vue";
import CiStateBadge from "../components/CiStateBadge.vue";
import ClassBadge from "../components/ClassBadge.vue";
import Icon from "../components/Icon.vue";
import InventoryFilterChips from "../components/InventoryFilterChips.vue";
import RowMenu from "../components/RowMenu.vue";
import SkeletonRows from "../components/SkeletonRows.vue";
import { t } from "../i18n";
import { ciRowMenu } from "../lib/ciRowMenu";
import { onRowKeydown } from "../lib/rowKeyboard";
import KeyboardHints from "../components/KeyboardHints.vue";
import { useDebounced, useDocumentTitle } from "../lib/composables";
import { highlight } from "../lib/highlight";
import { useInventoryQueryState } from "../lib/useInventoryQueryState";
import { useSavedViewSelection } from "../lib/useSavedViewSelection";

/**
 * Full global-search results (ranked by the API), with the field that matched.
 * The term and the filters live in the URL (/search?q=…&classId=…&active=all),
 * in the same state as the inventory's (lib/inventoryQuery): search has no sort
 * (it is ranked) and no column choice. A saved search view (`view=<id>`) holds the
 * term and the filters; search views are never a default (saved-views spec D7).
 */
const classes = useCiClasses();
const lookupLists = useLookupLists();
const lookupValues = useAllLookupListValues();
const savedViews = useSavedViews("search");
const selection = useSavedViewSelection({
  context: "search",
  views: () => savedViews.data.value?.data,
  failed: () => savedViews.isError.value,
  classes: () => classes.data.value,
});
const state = useInventoryQueryState({
  holdOff: () => selection.pending.value,
  context: "search",
  classes: () => classes.data.value,
  settingsLoaded: true,
  listViewFor: () => undefined,
});
const { limit, offset, activeFilters } = state;
const q = computed(() => state.get("q"));
const filters = computed(() => {
  const { q: _q, limit: _l, offset: _o, ...f } = state.searchFilters.value;
  return f;
});
const search = useSearch(q, limit, offset, filters);
useDocumentTitle(() => (q.value ? t("search.docTitle", { q: q.value }) : t("search.title")));
const classById = (id: string) => classes.data.value?.find((c) => c.id === id);

// The term box on the page: local state for typing, debounced into the URL (as in the inventory).
const qText = ref(q.value);
const debouncedQ = useDebounced(qText, 300);
watch(debouncedQ, (v) => {
  if (v !== q.value) state.update({ q: v.trim() ? v : undefined });
});
watch(q, (v) => (qText.value = v)); // back/forward and the header search
const rows = computed(() => search.data.value?.data ?? []);
const total = computed(() => search.data.value?.page.total ?? 0);
const catalogue = computed(() =>
  classes.data.value && lookupLists.data.value && lookupValues.data.value
    ? { classes: classes.data.value, lists: lookupLists.data.value, values: lookupValues.data.value }
    : null,
);
const settledTotal = computed(() =>
  search.data.value && !search.isPlaceholderData.value && !search.isFetching.value ? search.data.value.page.total : undefined,
);
/** The same term and filters as a sortable, pageable inventory list. */
const inventoryLink = computed(() => {
  const query: Record<string, string> = {};
  for (const k of ["q", ...activeFilters.value]) if (state.get(k)) query[k] = state.get(k);
  return { path: "/cis", query };
});
</script>

<template>
  <div class="list-head search-head">
    <Breadcrumbs :items="[{ label: t('search.title') }]" />
    <div class="page-header">
      <div class="title">
        <h1>{{ q ? t("search.resultsFor", { q }) : t("search.title") }}</h1>
        <span v-if="search.data.value" class="count mono">{{ t("search.matches", { n: search.data.value.page.total }) }}</span>
        <span v-if="search.isFetching.value && !search.isLoading.value" class="spinner" :aria-label="t('common.refreshing')" />
      </div>
      <div v-if="q" class="actions">
        <RouterLink class="btn" :to="inventoryLink"><Icon name="list" />{{ t("search.openAsInventory") }}</RouterLink>
      </div>
    </div>
    <p class="page-intro">{{ t("search.intro") }}</p>
    <div class="toolbar" role="group" :aria-label="t('search.filterGroup')">
      <SavedViewMenu context="search" :state="state" :selection="selection" :classes="classes.data.value" :catalogue="catalogue" :total="settledTotal" />
      <div class="field search">
        <label for="s-q">{{ t("search.term") }}</label>
        <span class="input-icon">
          <Icon name="search" />
          <input id="s-q" v-model="qText" type="search" :placeholder="t('search.term.placeholder')" />
        </span>
      </div>
      <template v-if="q">
        <InventoryFilters :state="state" id-prefix="s" />
        <button v-if="activeFilters.length > 0" type="button" class="btn btn-ghost" @click="state.clearFilters()"><Icon name="x" />{{ t("inventory.clearFilters") }}</button>
      </template>
    </div>
    <InventoryFilterChips v-if="q" :state="state" />
  </div>

  <section class="panel explorer" :aria-label="t('search.region')">
    <EmptyState v-if="!q" icon="search" :title="t('search.empty.title')">
      {{ t("search.empty.body") }}
    </EmptyState>
    <div v-else-if="search.isError.value" class="panel-body">
      <ErrorAlert :error="search.error.value" :on-retry="() => search.refetch()" />
    </div>
    <SkeletonRows v-else-if="search.isLoading.value" :label="t('common.searching')" />
    <EmptyState v-else-if="search.data.value && total === 0 && activeFilters.length > 0" icon="search" :title="t('search.noMatchFiltered', { q })">
      {{ t("inventory.noMatch.body") }}
      <template #actions><button type="button" class="btn" @click="state.clearFilters()">{{ t("inventory.clearFilters") }}</button></template>
    </EmptyState>
    <EmptyState v-else-if="search.data.value && total === 0" icon="search" :title="t('search.noMatch', { q })">
      {{ t("search.noMatch.body") }}
    </EmptyState>
    <EmptyState v-else-if="search.data.value && rows.length === 0" :title="t('common.pastEnd')">
      <template #actions><button type="button" class="btn" @click="state.update({}, true)">{{ t("common.firstPage") }}</button></template>
    </EmptyState>
    <template v-if="q && rows.length > 0 && !search.isError.value">
      <div class="table-wrap table-scroll">
        <table :class="['data', 'list-table', 'search-table', { loading: search.isPlaceholderData.value }]" aria-describedby="search-keys">
          <caption class="sr-only">{{ t("search.region") }}</caption>
          <thead>
            <tr>
              <th scope="col">{{ t("search.col.label") }}</th>
              <th scope="col">{{ t("search.col.ident") }}</th>
              <th scope="col">{{ t("search.col.class") }}</th>
              <th scope="col">{{ t("search.col.matched") }}</th>
              <th scope="col" class="row-actions"><span class="sr-only">{{ t("inventory.actions") }}</span></th>
            </tr>
          </thead>
          <tbody @keydown="onRowKeydown($event)">
            <tr v-for="{ item, matches } in rows" :key="item.id" :data-id="item.id">
              <td>
                <span class="cell-clip"><RouterLink :to="`/cis/${item.id}`" class="list-name" dir="auto">{{ item.label }}</RouterLink></span>
                <CiStateBadge :ci="item" />
              </td>
              <td class="mono muted">{{ item.ident }}</td>
              <td>
                <ClassBadge v-if="classById(item.classId)" :icon="classById(item.classId)!.icon" :color="classById(item.classId)!.color" :name="item.class.name" />
                <bdi v-else>{{ item.class.name }}</bdi>
              </td>
              <td class="matches" :title="matches.map((m) => `${m.label}: ${m.value}`).join('\n')">
                <span v-for="(m, i) in matches.slice(0, 2)" :key="i" class="match">
                  <span class="match-key"><bdi>{{ m.label }}</bdi></span>
                  <span class="match-value mono" dir="auto"><template v-for="(part, j) in highlight(m.value, q)" :key="j"><mark v-if="part.match">{{ part.text }}</mark><template v-else>{{ part.text }}</template></template></span>
                </span>
                <span v-if="matches.length > 2" class="badge"><span aria-hidden="true">+{{ matches.length - 2 }}</span><span class="sr-only">{{ t("search.moreMatches", { n: matches.length - 2 }) }}</span></span>
              </td>
              <td class="row-actions">
                <RowMenu :label="t('inventory.rowMenu', { name: item.label })" :items="ciRowMenu(item)" />
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <div class="table-footer">
        <PaginationBar numbered :total="total" :limit="limit" :offset="offset" @change="state.onPage" />
      </div>
      <KeyboardHints id="search-keys" />
    </template>
  </section>
</template>
