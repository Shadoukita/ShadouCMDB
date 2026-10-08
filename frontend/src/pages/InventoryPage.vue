<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useQuery } from "@tanstack/vue-query";
import { RouterLink, useRouter } from "vue-router";
import { useAllLookupListValues, useAreas, useLookupLists } from "../api/datamodel";
import { ApiError } from "../api/client";
import { fileStamp } from "../api/download";
import { ciCountQuery, downloadInventoryCsv, useCiClasses, useCiList, useClassAttributes, useCriticalityValues, type CiListQuery } from "../api/queries";
import { dataModelEmpty } from "../lib/dataModel";
import BulkEditDialog from "../components/BulkEditDialog.vue";
import Breadcrumbs from "../components/Breadcrumbs.vue";
import CiCell from "../components/CiCell.vue";
import ColumnsPopover from "../components/ColumnsPopover.vue";
import DataModelEmpty from "../components/DataModelEmpty.vue";
import EmptyState from "../components/EmptyState.vue";
import ErrorAlert from "../components/ErrorAlert.vue";
import AddFilterPopover from "../components/AddFilterPopover.vue";
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
import { exportFileName, exportUnsupported, inventoryExportQuery, type ExportDelimiter } from "../lib/inventoryExport";
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
import { BULK_EDIT_LIMIT, bulkEditBlocked, retryable, type BulkOutcome } from "../lib/bulkEdit";
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
/** All CIs the user may view (the rail's count): the title reads "x of y" while filters narrow the list. */
const allCount = useQuery(ciCountQuery({}));
const ofAll = computed(() => {
  const all = allCount.data.value;
  return activeFilters.value.length > 0 && all !== undefined && total.value <= all ? all : undefined;
});
const criticalityLevels = computed(() => criticality.data.value?.length || 4);
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
// The histogram endpoint takes no data-quality check (`quality`, `endOfLifeWithinDays`): under one
// ("Needs attention") the strip says so instead of counting a wider set than the list shows (GH#788).
// The query parser keeps `endOfLifeWithinDays` only with quality=end_of_life; both are checked anyway.
const histogramFilters = computed(() => {
  const { quality: _quality, endOfLifeWithinDays: _days, ...filters } = listFilters.value;
  return filters;
});
const histogramUnavailable = computed(() =>
  listFilters.value.quality || listFilters.value.endOfLifeWithinDays !== undefined ? t("histogram.unavailable.quality") : undefined,
);
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

// Row selection (design document §0, step 12c). It survives paging and is cleared when the filters change,
// so "n selected" never counts rows the list no longer holds. Each selected row's label is kept to name it
// in the bulk edit's result, also once the operator has paged away from it.
const selected = ref(new Set<string>());
const selectedLabels = new Map<string, string>();
watch(
  () => JSON.stringify(listFilters.value),
  () => (selected.value = new Set()),
);
const pageIds = computed(() => rows.value.map((r) => r.id));
const pageSelected = computed(() => pageIds.value.filter((id) => selected.value.has(id)).length);
function remember(id: string) {
  const ci = rows.value.find((r) => r.id === id);
  if (ci) selectedLabels.set(id, ci.label);
}
function toggleRow(id: string, on: boolean) {
  if (on) remember(id);
  const next = new Set(selected.value);
  if (on) next.add(id);
  else next.delete(id);
  selected.value = next;
}
function togglePage(on: boolean) {
  const next = new Set(selected.value);
  for (const id of pageIds.value) {
    if (on) remember(id);
    if (on) next.add(id);
    else next.delete(id);
  }
  selected.value = next;
}

// Bulk edit (gap G10): the selection in one request (POST /configuration-items/bulk-update, at most
// BULK_EDIT_LIMIT CIs). With one class shown, its attributes can be set (every selected CI has them);
// otherwise only the criticality. Afterwards the updated CIs leave the selection and the refused ones stay,
// except those that no longer exist.
const bulkOpen = ref(false);
const selectedIds = computed(() => [...selected.value]);
const bulkBlocked = computed(() => {
  const n = selected.value.size;
  const why = bulkEditBlocked(n, session.canOnAnyClass("edit"));
  if (why === "tooMany") return t("inventory.bulkEdit.tooMany", { max: formatNumber(BULK_EDIT_LIMIT), n: formatNumber(n) });
  return why === "noPermission" ? t("inventory.bulkEdit.noPermission") : null;
});
function openBulkEdit() {
  if (!bulkBlocked.value) bulkOpen.value = true;
}
function closeBulkEdit(outcome: BulkOutcome | null) {
  bulkOpen.value = false;
  if (outcome?.committed) selected.value = new Set(outcome.refused.filter((r) => retryable(r.code)).map((r) => r.id));
}

const viewMenu = ref<InstanceType<typeof SavedViewMenu>>();

// CSV export (gap G9): every CI of the current query (filters, sort, the visible columns in their order), not just
// this page. The server streams and audits it, and caps how many run at once (429 per user, 503 per server).
const exporting = ref(false);
const exportError = ref<unknown>(null);
const exportBlocked = computed(() => exportUnsupported(state.listQuery.value as Record<string, unknown>));
const exportItems = computed(() => [
  { label: t("inventory.export.comma"), action: () => void exportCsv("comma") },
  { label: t("inventory.export.semicolon"), action: () => void exportCsv("semicolon") },
]);
const exportTitle = computed(() => {
  const e = exportError.value instanceof ApiError ? exportError.value : null;
  if (e?.code === "RATE_LIMITED") return t("inventory.export.busyUser");
  if (e?.code === "SERVER_BUSY") return t("inventory.export.busyServer");
  return t("inventory.export.failed");
});
let lastDelimiter: ExportDelimiter = "comma";
async function exportCsv(delimiter: ExportDelimiter) {
  if (exporting.value) return;
  lastDelimiter = delimiter;
  exporting.value = true;
  exportError.value = null;
  try {
    const query = inventoryExportQuery(state.listQuery.value as Record<string, unknown>, columns.value, delimiter);
    await downloadInventoryCsv(query, exportFileName(currentClass.value?.key, fileStamp()));
  } catch (e) {
    exportError.value = e;
  } finally {
    exporting.value = false;
  }
}

function clearFilters() {
  selection.skipNextDefault();
  state.clearFilters();
}
</script>

<template>
  <div class="inventory-head">
    <Breadcrumbs :items="crumbs" />
    <div class="page-header">
      <div class="title">
        <h1>{{ currentClass ? currentClass.name : t("inventory.title") }}</h1>
        <span v-if="list.data.value" class="count mono">
          {{ ofAll !== undefined ? t("inventory.countOf", { n: formatNumber(total), all: formatNumber(ofAll) }) : t("common.total", { n: formatNumber(total) }) }}
        </span>
        <span v-if="list.isFetching.value && !list.isLoading.value" class="spinner" :aria-label="t('common.refreshing')" />
      </div>
      <div v-if="!classDenied || canCreate || importAccess.available.value" class="actions">
        <RowMenu
          v-if="!classDenied"
          :label="t('inventory.export.menu')"
          :text="exporting ? t('inventory.export.running') : t('inventory.export')"
          icon="arrow-down-to-line"
          :items="exportItems"
          :disabled="exporting || exportBlocked || settledTotal === 0"
          :title="exportBlocked ? t('inventory.export.quality') : undefined"
          large
        />
        <RouterLink v-if="importAccess.available.value" class="btn" :to="importTo"><Icon name="upload" />{{ t("inventory.import") }}</RouterLink>
        <RouterLink v-if="canCreate" class="btn btn-primary" :to="newTo"><Icon name="plus" />{{ t("inventory.new", { name: newLabel }) }}</RouterLink>
      </div>
    </div>
    <ErrorAlert v-if="exportError" class="export-error" :error="exportError" :title="exportTitle" :on-retry="() => exportCsv(lastDelimiter)" />

    <form class="toolbar inventory-toolbar" role="search" @submit.prevent>
      <SavedViewMenu ref="viewMenu" context="inventory" :state="state" :selection="selection" :classes="classes.data.value" :catalogue="catalogue" :total="settledTotal" />
      <QueryBar :state="state" :catalogue="barCatalogue" />
      <div class="inventory-filters">
        <InventoryFilterChips :state="state" all />
        <AddFilterPopover :state="state" id-prefix="f" />
        <button v-if="activeFilters.length > 0" type="button" class="btn btn-ghost" @click="clearFilters"><Icon name="x" />{{ t("inventory.clearFilters") }}</button>
        <button type="button" class="btn btn-ghost save-view" :disabled="viewMenu?.saveAsDisabled" @click="viewMenu?.saveAs()">{{ t("views.saveView") }}</button>
      </div>
      <div class="toolbar-end">
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
        <ColumnsPopover
          ref="columnsPopover"
          :columns="columns"
          :fields="fieldChoices"
          :attributes="attributeChoices"
          :class-name="currentClass?.name"
          :customized="state.columnsCustomized.value"
          @toggle="state.toggleColumn"
          @reorder="state.setColumns"
          @reset="state.resetColumns"
        />
      </div>
    </form>
  </div>

  <section class="explorer inventory" :aria-label="t('inventory.region')">
    <div class="explorer-body">
      <FacetPanel
        v-if="showFacets && facetsOpen"
        :state="state"
        :filters="listFilters"
        :enabled="state.settled.value"
        :collapsed="facetPref.collapsed"
        @toggle-group="toggleFacetGroup"
      />
      <div class="explorer-main panel">
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
          <ChangeHistogram v-if="showHistogram" :filters="histogramFilters" :unavailable="histogramUnavailable" />
          <div class="table-wrap table-scroll">
            <table :class="['data', 'inventory-table', { loading: list.isPlaceholderData.value }]" aria-describedby="inventory-keys">
              <thead>
                <tr>
                  <th scope="col" class="select-cell">
                    <input
                      type="checkbox"
                      :checked="pageSelected > 0 && pageSelected === rows.length"
                      :indeterminate="pageSelected > 0 && pageSelected < rows.length"
                      :aria-label="t('inventory.select.page')"
                      @change="togglePage(($event.target as HTMLInputElement).checked)"
                    />
                  </th>
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
                <tr v-for="ci in rows" :key="ci.id" :data-id="ci.id" :class="{ deleted: ci.deletedAt, selected: selected.has(ci.id) }">
                  <td class="select-cell">
                    <input
                      type="checkbox"
                      :checked="selected.has(ci.id)"
                      :aria-label="t('inventory.select.row', { name: ci.label })"
                      @change="toggleRow(ci.id, ($event.target as HTMLInputElement).checked)"
                    />
                  </td>
                  <td v-for="c in columns" :key="c" :class="{ 'name-cell': c === 'label' }">
                    <CiCell :ci="ci" :field="c" :defs="attrDefs" :class-of="classById" rich :criticality-levels="criticalityLevels" :class-column="columns.includes('class')" />
                  </td>
                  <td class="row-actions">
                    <RowMenu :label="t('inventory.rowMenu', { name: ci.label })" :items="ciRowMenu(ci)" />
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
          <div class="table-footer">
            <div class="selection-status">
              <span role="status">{{ selected.size > 0 ? t("inventory.selected", { n: formatNumber(selected.size) }) : "" }}</span>
              <template v-if="selected.size > 0">
                <!-- Refused before sending (over the limit, no edit right): shown, focusable, with the reason. -->
                <button
                  type="button"
                  class="btn btn-sm btn-ghost"
                  :aria-disabled="bulkBlocked ? 'true' : undefined"
                  :title="bulkBlocked ?? undefined"
                  :aria-describedby="bulkBlocked ? 'bulk-edit-reason' : undefined"
                  @click="openBulkEdit"
                >
                  {{ t("inventory.bulkEdit") }}
                </button>
                <span v-if="bulkBlocked" id="bulk-edit-reason" class="hint">{{ bulkBlocked }}</span>
                <button type="button" class="btn btn-sm btn-ghost" @click="selected = new Set()">{{ t("inventory.select.clear") }}</button>
              </template>
            </div>
            <PaginationBar numbered :total="total" :limit="limit" :offset="offset" @change="state.onPage" />
          </div>
          <KeyboardHints id="inventory-keys" :edit="anyEditable" columns />
        </template>
        <BulkEditDialog
          :open="bulkOpen"
          :ids="selectedIds"
          :labels="selectedLabels"
          :defs="currentClass ? attrDefs : []"
          :class-name="currentClass?.name"
          @close="closeBulkEdit"
        />
      </div>
    </div>
  </section>
</template>
