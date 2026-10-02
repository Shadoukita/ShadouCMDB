<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { RouterLink } from "vue-router";
import { useAllLookupListValues, useAreas, useLookupLists } from "../api/datamodel";
import { useSavedViewSources } from "../api/savedViews";
import { useCiClasses, useCiList, useClassAttributes, type CiListQuery } from "../api/queries";
import { dataModelEmpty } from "../lib/dataModel";
import Breadcrumbs from "../components/Breadcrumbs.vue";
import CiCell from "../components/CiCell.vue";
import ColumnsPopover from "../components/ColumnsPopover.vue";
import DataModelEmpty from "../components/DataModelEmpty.vue";
import EmptyState from "../components/EmptyState.vue";
import ErrorAlert from "../components/ErrorAlert.vue";
import InventoryFilters from "../components/InventoryFilters.vue";
import LoadingState from "../components/LoadingState.vue";
import PaginationBar from "../components/PaginationBar.vue";
import SavedViewsBar from "../components/views/SavedViewsBar.vue";
import { useAppSettings } from "../lib/appSettings";
import { useDebounced, useDocumentTitle } from "../lib/composables";
import { viewableClasses } from "../lib/permissions";
import { ATTRIBUTE_PREFIX, BUILTIN_FIELDS, fieldLabel, isSortableAttribute, listViewFor, lookupValueIds } from "../lib/uiSettings";
import { useInventoryQueryState } from "../lib/useInventoryQueryState";
import { useImportAccess } from "../lib/useImportAccess";
import { useSavedViewState } from "../lib/useSavedViewState";
import { useSessionStore } from "../stores/session";

/**
 * CI inventory. Every filter, the sort, the columns and the page live in the URL
 * (/cis?classId=…&lookupValueId=…&q=…&sort=-updatedAt&columns=label,ident&offset=50),
 * so a view survives reload and can be bookmarked or shared. Filtering and paging
 * happen in the API. What the URL leaves out comes from the class's list view
 * (Administration › Customization › List views), then the built-in defaults:
 * see lib/inventoryQuery. A saved view (the View menu, a `view=` link, the user's
 * default for the list) fills the URL before the list is queried: lib/useSavedViewState.
 */
const classes = useCiClasses();
const settings = useAppSettings();
const lookupLists = useLookupLists();
const lookupValues = useAllLookupListValues();
const attrKeys = computed(() => (attrs.data.value ? new Set(attrs.data.value.map((a) => a.key)) : null));
const sv = useSavedViewState({
  context: "inventory",
  classes: () => classes.data.value,
  sources: (linkedId) => useSavedViewSources("inventory", linkedId),
});
const state = useInventoryQueryState({
  context: "inventory",
  classes: () => classes.data.value,
  settingsLoaded: () => settings.query.isFetched.value,
  listViewFor: (key) => listViewFor(settings.doc.value, key),
  lookupValueIds: (lookups) =>
    lookupLists.data.value && lookupValues.data.value ? (lookupValueIds(lookups, lookupLists.data.value, lookupValues.data.value) ?? undefined) : null,
  attributeKeys: attrKeys,
  hold: () => sv.holding.value,
});
const { get, limit, offset, columns, activeFilters } = state;
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

useDocumentTitle(() => currentClass.value?.name ?? "Inventory");

// Search box: local state for typing, debounced into the URL.
const qText = ref(get("q"));
const debouncedQ = useDebounced(qText, 300);
watch(debouncedQ, (v) => {
  if (v !== get("q")) state.update({ q: v || undefined });
});
watch(
  () => get("q"),
  (v) => (qText.value = v), // back/forward
);

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
const newLabel = computed(() => (currentClass.value && !currentClass.value.isAbstract ? currentClass.value.name : "CI"));
const crumbs = computed(() =>
  currentClass.value
    ? [{ label: "Inventory", to: "/cis" }, ...(currentArea.value ? [{ label: currentArea.value.name }] : []), { label: currentClass.value.name }]
    : [{ label: "Inventory" }],
);

function clearFilters() {
  qText.value = "";
  state.clearFilters();
}
</script>

<template>
  <Breadcrumbs :items="crumbs" />
  <div class="page-header">
    <div class="title">
      <h1>{{ currentClass ? currentClass.name : "Configuration items" }}</h1>
      <span v-if="list.data.value" class="muted">{{ total.toLocaleString() }} total</span>
      <span v-if="list.isFetching.value && !list.isLoading.value" class="spinner" aria-label="Refreshing" />
    </div>
    <div v-if="canCreate || importAccess.available.value" class="actions">
      <RouterLink v-if="importAccess.available.value" class="btn" :to="importTo">Import</RouterLink>
      <RouterLink v-if="canCreate" class="btn btn-primary" :to="newTo">+ New {{ newLabel }}</RouterLink>
    </div>
  </div>

  <section class="panel" aria-label="Inventory">
    <SavedViewsBar
      context="inventory"
      :sv="sv"
      :defaults="state.stateDefaults.value"
      :classes="classes.data.value ?? []"
      :total="list.data.value?.page.total"
      :fetching="list.isFetching.value"
    >
      <ColumnsPopover
        :columns="columns"
        :fields="fieldChoices"
        :attributes="attributeChoices"
        :class-name="currentClass?.name"
        :customized="state.columnsCustomized.value"
        @toggle="state.toggleColumn"
        @reorder="state.setColumns"
        @reset="state.resetColumns"
      />
    </SavedViewsBar>
    <form class="toolbar" role="search" @submit.prevent>
      <div class="field search">
        <label for="f-q">Search</label>
        <input id="f-q" v-model="qText" type="search" placeholder="Label, ident, attribute values…" />
      </div>
      <InventoryFilters :state="state" id-prefix="f" />
      <button v-if="activeFilters.length > 0" type="button" class="btn" @click="clearFilters">Clear filters</button>
    </form>

    <div v-if="list.isError.value" class="panel-body">
      <ErrorAlert :error="list.error.value" :on-retry="() => list.refetch()" />
    </div>
    <LoadingState v-if="list.isPending.value" :label="sv.holding.value && sv.viewId.value ? 'Opening the saved view…' : 'Loading inventory…'" />

    <EmptyState v-if="classDenied" title="Permission denied">
      None of your permission profiles allows viewing {{ currentClass?.name }} configuration items, so none are listed here.
      <template #actions><RouterLink class="btn" to="/cis">Back to inventory</RouterLink></template>
    </EmptyState>
    <DataModelEmpty v-else-if="list.data.value && total === 0 && activeFilters.length === 0 && classes.data.value && dataModelEmpty(classes.data.value)" />
    <EmptyState v-else-if="list.data.value && total === 0 && activeFilters.length === 0" title="The inventory is empty">
      Configuration items are the servers, VMs, applications, databases, network devices and locations you track. Create
      one, then relate it to others from its detail page.
      <template v-if="canCreate || importAccess.available.value" #actions>
        <RouterLink v-if="canCreate" class="btn btn-primary" to="/cis/new">+ Create your first configuration item</RouterLink>
        <RouterLink v-if="importAccess.available.value" class="btn" :to="importTo">Import them from a spreadsheet</RouterLink>
      </template>
    </EmptyState>
    <EmptyState v-else-if="list.data.value && total === 0 && activeFilters.length > 0" title="No configuration items match these filters">
      Adjust or clear the filters above.
    </EmptyState>
    <EmptyState v-if="list.data.value && total > 0 && rows.length === 0" title="This page is past the end of the results">
      <template #actions><button class="btn" @click="state.update({}, true)">Go to first page</button></template>
    </EmptyState>

    <template v-if="rows.length > 0">
      <div class="table-wrap">
        <table :class="['data', { loading: list.isPlaceholderData.value }]">
          <thead>
            <tr>
              <th v-for="c in columns" :key="c" scope="col" :aria-sort="columnSort(c) ? state.ariaSort(columnSort(c)!) : undefined">
                <button v-if="columnSort(c)" type="button" class="sort" @click="state.toggleSort(columnSort(c)!)">
                  {{ columnLabel(c) }} {{ state.sortIndicator(columnSort(c)!) }}
                </button>
                <template v-else>{{ columnLabel(c) }}</template>
              </th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="ci in rows" :key="ci.id" :class="{ deleted: ci.deletedAt }">
              <td v-for="c in columns" :key="c"><CiCell :ci="ci" :field="c" :defs="attrDefs" /></td>
              <td class="row-actions">
                <RouterLink v-if="!ci.deletedAt" class="btn btn-sm" :to="`/cis/${ci.id}/impact`" :title="`Impact analysis of ${ci.label}`">Impact</RouterLink>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <PaginationBar :total="total" :limit="limit" :offset="offset" @change="state.onPage" />
    </template>
  </section>
</template>
