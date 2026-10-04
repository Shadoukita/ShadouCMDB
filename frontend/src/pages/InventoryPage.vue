<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { RouterLink, useRouter } from "vue-router";
import { useAllLookupListValues, useAreas, useLookupLists } from "../api/datamodel";
import { useCiClasses, useCiList, useClassAttributes, useCriticalityValues, type CiListQuery } from "../api/queries";
import { dataModelEmpty } from "../lib/dataModel";
import Breadcrumbs from "../components/Breadcrumbs.vue";
import CiCell from "../components/CiCell.vue";
import ColumnsPopover from "../components/ColumnsPopover.vue";
import DataModelEmpty from "../components/DataModelEmpty.vue";
import EmptyState from "../components/EmptyState.vue";
import ErrorAlert from "../components/ErrorAlert.vue";
import InventoryFilters from "../components/InventoryFilters.vue";
import QueryBar from "../components/QueryBar.vue";
import type { BarCatalogue } from "../lib/queryBar";
import SavedViewMenu from "../components/savedViews/SavedViewMenu.vue";
import PaginationBar from "../components/PaginationBar.vue";
import { useAppSettings } from "../lib/appSettings";
import { useDocumentTitle, useMediaQuery } from "../lib/composables";
import ChangeHistogram from "../components/ChangeHistogram.vue";
import FacetPanel from "../components/FacetPanel.vue";
import { readFacetPref, writeFacetPref } from "../lib/facets";
import { viewableClasses } from "../lib/permissions";
import { ATTRIBUTE_PREFIX, BUILTIN_FIELDS, fieldLabel, isSortableAttribute, listViewFor, lookupValueIds } from "../lib/uiSettings";
import { useInventoryQueryState } from "../lib/useInventoryQueryState";
import { useSavedViews } from "../api/savedViews";
import { useSavedViewSelection } from "../lib/useSavedViewSelection";
import { useImportAccess } from "../lib/useImportAccess";
import { useSessionStore } from "../stores/session";
import SortIcon from "../components/SortIcon.vue";
import Icon from "../components/Icon.vue";
import InventoryFilterChips from "../components/InventoryFilterChips.vue";
import RowMenu from "../components/RowMenu.vue";
import SkeletonRows from "../components/SkeletonRows.vue";
import { ciRowMenu } from "../lib/ciRowMenu";
import { onRowKeydown } from "../lib/rowKeyboard";
import KeyboardHints from "../components/KeyboardHints.vue";
import { formatNumber, t } from "../i18n";

/**
 * CI inventory. Every filter, the sort, the columns and the page live in the URL
 * (/cis?classId=…&lookupValueId=…&q=…&sort=-updatedAt&columns=label,ident&offset=50),
 * so a view survives reload and can be bookmarked or shared. Filtering and paging
 * happen in the API. What the URL leaves out comes from the class's list view
 * (Administration › Customization › List views), then the built-in defaults:
 * see lib/inventoryQuery. Ahead of those come a saved view (`view=<id>`) and the
 * user's default view for the list (lib/useSavedViewSelection).
 */
const classes = useCiClasses();
const settings = useAppSettings();
const lookupLists = useLookupLists();
const lookupValues = useAllLookupListValues();
const attrKeys = computed(() => (attrs.data.value ? new Set(attrs.data.value.map((a) => a.key)) : null));
const savedViews = useSavedViews("inventory");
const selection = useSavedViewSelection({
  context: "inventory",
  views: () => savedViews.data.value?.data,
  failed: () => savedViews.isError.value,
  classes: () => classes.data.value,
});
const state = useInventoryQueryState({
  holdOff: () => selection.pending.value,
  context: "inventory",
  classes: () => classes.data.value,
  settingsLoaded: () => settings.query.isFetched.value,
  listViewFor: (key) => listViewFor(settings.doc.value, key),
  lookupValueIds: (lookups) =>
    lookupLists.data.value && lookupValues.data.value ? (lookupValueIds(lookups, lookupLists.data.value, lookupValues.data.value) ?? undefined) : null,
  attributeKeys: attrKeys,
});
const { limit, offset, columns, activeFilters } = state;
const classById = (id: string) => classes.data.value?.find((c) => c.id === id);
const currentClass = computed(() => (state.currentClass.value ? classById(state.currentClass.value.id) : undefined));

const list = useCiList(() => state.listQuery.value as CiListQuery, state.settled);
const areas = useAreas();
const session = useSessionStore();
const classDenied = computed(
  () => !!currentClass.value && !viewableClasses(classes.data.value ?? [], (id) => session.canOnClass(id, "view")).some((c) => c.id === currentClass.value!.id),
);
const currentArea = computed(() => areas.data.value?.find((a) => a.id === currentClass.value?.areaId));

// The class's attributes: its attribute columns and the Columns popover's choices (only for a list of one class).
const attrs = useClassAttributes(() => currentClass.value?.id);
const attrDefs = computed(() => attrs.data.value ?? []);
const columnLabel = (field: string) => fieldLabel(field, attrDefs.value);
const columnSort = (field: string) =>
  state.columnSort(field, (key) => {
    const def = attrDefs.value.find((d) => d.key === key);
    return !!def && isSortableAttribute(def);
  });
const fieldChoices = BUILTIN_FIELDS.map((f) => ({ key: f.key, label: f.label }));
const attributeChoices = computed(() =>
  attrDefs.value.filter((a) => a.isActive).map((a) => ({ key: `${ATTRIBUTE_PREFIX}${a.key}`, label: a.label })),
);

useDocumentTitle(() => currentClass.value?.name ?? t("inventory.crumb"));

// The query bar resolves class, lookup and criticality keys against these (each undefined while it loads).
const criticality = useCriticalityValues();
const barCatalogue = computed<BarCatalogue>(() => ({
  classes: classes.data.value,
  criticality: criticality.data.value,
  lists: lookupLists.data.value,
  values: lookupValues.data.value,
}));

const total = computed(() => list.data.value?.page.total ?? 0);
const rows = computed(() => list.data.value?.data ?? []);
const classId = computed(() => state.classId.value);
const newTo = computed(() => (classId.value && !currentClass.value?.isAbstract ? `/cis/new?classId=${classId.value}` : "/cis/new"));
const canCreate = computed(() =>
  classId.value && currentClass.value && !currentClass.value.isAbstract ? session.canOnClass(classId.value, "create") : session.canOnAnyClass("create"),
);
const importAccess = useImportAccess();
// With one concrete class shown, the import wizard preselects it in step 2.
const importTo = computed(() =>
  currentClass.value && !currentClass.value.isAbstract ? `/imports/new?classKey=${encodeURIComponent(currentClass.value.key)}` : "/imports/new",
);
const newLabel = computed(() => (currentClass.value && !currentClass.value.isAbstract ? currentClass.value.name : t("inventory.ci")));
const crumbs = computed(() =>
  currentClass.value
    ? [{ label: t("inventory.crumb"), to: "/cis" }, ...(currentArea.value ? [{ label: currentArea.value.name }] : []), { label: currentClass.value.name }]
    : [{ label: t("inventory.crumb") }],
);

/** Keys of the classes, lookup lists and values, to save the URL's ids in a view. */
const catalogue = computed(() =>
  classes.data.value && lookupLists.data.value && lookupValues.data.value
    ? { classes: classes.data.value, lists: lookupLists.data.value, values: lookupValues.data.value }
    : null,
);
/** The total once the list for this URL has loaded (not the previous list's, kept while it loads). */
const settledTotal = computed(() => (list.data.value && !list.isPlaceholderData.value && !list.isFetching.value ? total.value : undefined));

// The change histogram and the facet panel count the list's filters.
const wide = useMediaQuery("(min-width: 821px)");
const listFilters = computed(() => {
  const { sort: _sort, limit: _limit, offset: _offset, ...filters } = state.listQuery.value as CiListQuery;
  return filters;
});
// The histogram counts the audit log (audit.view); wide screens only.
const showHistogram = computed(() => wide.value && session.can("audit.view") && !classDenied.value && rows.value.length > 0);

// The facet panel: open on wide screens until the operator chooses (remembered per browser), with
// nothing to narrow on an empty inventory.
const facetPref = ref(readFacetPref());
watch(facetPref, (p) => writeFacetPref(p), { deep: true });
const facetsOpen = computed(() => facetPref.value.open ?? wide.value);
const showFacets = computed(() => !classDenied.value && !(list.data.value && total.value === 0 && activeFilters.value.length === 0));
function toggleFacetGroup(key: string) {
  const c = facetPref.value.collapsed;
  facetPref.value.collapsed = c.includes(key) ? c.filter((k) => k !== key) : [...c, key];
}

// Keyboard rows (lib/rowKeyboard): ↑/↓ between rows, Enter opens, `e` edits, `c` opens Columns.
const router = useRouter();
const columnsPopover = ref<InstanceType<typeof ColumnsPopover>>();
const canEdit = (ci: (typeof rows.value)[number]) => !ci.deletedAt && session.canOnClass(ci.classId, "edit");
const anyEditable = computed(() => rows.value.some(canEdit));
const rowKeys = {
  edit: (id: string) => {
    const ci = rows.value.find((r) => r.id === id);
    if (ci && canEdit(ci)) void router.push(`/cis/${id}/edit`);
  },
  columns: () => void columnsPopover.value?.show(),
};

function clearFilters() {
  selection.skipNextDefault();
  state.clearFilters();
}
</script>

<template>
  <Breadcrumbs :items="crumbs" />
  <div class="page-header">
    <div class="title">
      <h1>{{ currentClass ? currentClass.name : t("inventory.title") }}</h1>
      <span v-if="list.data.value" class="muted count">{{ t("common.total", { n: formatNumber(total) }) }}</span>
      <span v-if="list.isFetching.value && !list.isLoading.value" class="spinner" :aria-label="t('common.refreshing')" />
    </div>
    <div v-if="canCreate || importAccess.available.value" class="actions">
      <RouterLink v-if="importAccess.available.value" class="btn" :to="importTo"><Icon name="upload" />{{ t("inventory.import") }}</RouterLink>
      <RouterLink v-if="canCreate" class="btn btn-primary" :to="newTo"><Icon name="plus" />{{ t("inventory.new", { name: newLabel }) }}</RouterLink>
    </div>
  </div>

  <section class="panel explorer" :aria-label="t('inventory.region')">
    <form class="toolbar" role="search" @submit.prevent>
      <button
        v-if="showFacets"
        type="button"
        class="btn btn-ghost facets-toggle"
        :aria-expanded="facetsOpen"
        aria-controls="facets"
        @click="facetPref.open = !facetsOpen"
      >
        <Icon :name="facetsOpen ? 'panel-left-close' : 'panel-left-open'" />{{ facetsOpen ? t("facets.hide") : t("facets.show") }}
      </button>
      <SavedViewMenu context="inventory" :state="state" :selection="selection" :classes="classes.data.value" :catalogue="catalogue" :total="settledTotal" />
      <QueryBar :state="state" :catalogue="barCatalogue" />
      <InventoryFilters :state="state" id-prefix="f" />
      <button v-if="activeFilters.length > 0" type="button" class="btn btn-ghost" @click="clearFilters"><Icon name="x" />{{ t("inventory.clearFilters") }}</button>
      <ColumnsPopover
        ref="columnsPopover"
        class="toolbar-end"
        :columns="columns"
        :fields="fieldChoices"
        :attributes="attributeChoices"
        :class-name="currentClass?.name"
        :customized="state.columnsCustomized.value"
        @toggle="state.toggleColumn"
        @reorder="state.setColumns"
        @reset="state.resetColumns"
      />
    </form>
    <InventoryFilterChips :state="state" />

    <div class="explorer-body">
      <FacetPanel
        v-if="showFacets && facetsOpen"
        :state="state"
        :filters="listFilters"
        :enabled="state.settled.value"
        :collapsed="facetPref.collapsed"
        @toggle-group="toggleFacetGroup"
      />
      <div class="explorer-main">
        <div v-if="list.isError.value" class="panel-body">
          <ErrorAlert :error="list.error.value" :on-retry="() => list.refetch()" />
        </div>
        <SkeletonRows v-if="list.isPending.value" :label="t('inventory.loading')" />

        <EmptyState v-if="classDenied" icon="lock" :title="t('inventory.denied.title')">
          {{ t("inventory.denied.body", { name: currentClass?.name ?? "" }) }}
          <template #actions><RouterLink class="btn" to="/cis">{{ t("inventory.denied.back") }}</RouterLink></template>
        </EmptyState>
        <DataModelEmpty v-else-if="list.data.value && total === 0 && activeFilters.length === 0 && classes.data.value && dataModelEmpty(classes.data.value)" />
        <EmptyState v-else-if="list.data.value && total === 0 && activeFilters.length === 0" :title="t('inventory.empty.title')">
          {{ t("inventory.empty.body") }}
          <template v-if="canCreate || importAccess.available.value" #actions>
            <RouterLink v-if="canCreate" class="btn btn-primary" to="/cis/new"><Icon name="plus" />{{ t("inventory.empty.create") }}</RouterLink>
            <RouterLink v-if="importAccess.available.value" class="btn" :to="importTo"><Icon name="upload" />{{ t("inventory.empty.import") }}</RouterLink>
          </template>
        </EmptyState>
        <EmptyState v-else-if="list.data.value && total === 0 && activeFilters.length > 0" icon="search" :title="t('inventory.noMatch.title')">
          {{ t("inventory.noMatch.body") }}
        </EmptyState>
        <EmptyState v-if="list.data.value && total > 0 && rows.length === 0" :title="t('common.pastEnd')">
          <template #actions><button class="btn" @click="state.update({}, true)">{{ t("common.firstPage") }}</button></template>
        </EmptyState>

        <template v-if="rows.length > 0">
          <ChangeHistogram v-if="showHistogram" :filters="listFilters" />
          <div class="table-wrap table-scroll">
            <table :class="['data', { loading: list.isPlaceholderData.value }]" aria-describedby="inventory-keys">
              <thead>
                <tr>
                  <th v-for="c in columns" :key="c" scope="col" :aria-sort="columnSort(c) ? state.ariaSort(columnSort(c)!) : undefined">
                    <button v-if="columnSort(c)" type="button" class="sort" @click="state.toggleSort(columnSort(c)!)">
                      {{ columnLabel(c) }} <SortIcon :dir="state.ariaSort(columnSort(c)!)" />
                    </button>
                    <template v-else>{{ columnLabel(c) }}</template>
                  </th>
                  <th scope="col" class="row-actions"><span class="sr-only">{{ t("inventory.actions") }}</span></th>
                </tr>
              </thead>
              <tbody @keydown="onRowKeydown($event, rowKeys)">
                <tr v-for="ci in rows" :key="ci.id" :data-id="ci.id" :class="{ deleted: ci.deletedAt }">
                  <td v-for="c in columns" :key="c"><CiCell :ci="ci" :field="c" :defs="attrDefs" :class-of="classById" /></td>
                  <td class="row-actions">
                    <RowMenu :label="t('inventory.rowMenu', { name: ci.label })" :items="ciRowMenu(ci)" />
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
          <PaginationBar :total="total" :limit="limit" :offset="offset" @change="state.onPage" />
          <KeyboardHints id="inventory-keys" :edit="anyEditable" columns />
        </template>
      </div>
    </div>
  </section>
</template>
