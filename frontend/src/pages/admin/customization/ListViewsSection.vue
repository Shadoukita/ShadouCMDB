<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useCiList, useClassAttributes, type CiClass, type CiListQuery } from "../../../api/queries";
import type { UiSettingsDocument } from "../../../api/uiSettings";
import CiCell from "../../../components/CiCell.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import {
  ATTRIBUTE_PREFIX,
  attributeSortFields,
  BUILTIN_FIELDS,
  DEFAULT_COLUMNS,
  EMPTY_FILTERS,
  fieldLabel,
  listColumns,
  SORT_FIELDS,
  sortParam,
  unavailableSortLabel,
} from "../../../lib/uiSettings";
import ClassPicker from "./ClassPicker.vue";
import FieldListEditor from "./FieldListEditor.vue";
import LookupFilterEditor from "./LookupFilterEditor.vue";

/**
 * Customization › List views: per class, the inventory's columns, default sort,
 * default filters and page size. The preview lists the first CIs of the class
 * with the draft columns.
 */
const props = defineProps<{ doc: UiSettingsDocument }>();
const cls = ref<CiClass>();
const view = computed(() => (cls.value ? props.doc.listViews.find((v) => v.classKey === cls.value!.key) : undefined));
const attrs = useClassAttributes(() => cls.value?.id);
const attrDefs = computed(() => (attrs.data.value ?? []).filter((d) => d.isActive));
const columnOptions = computed(() => [
  ...BUILTIN_FIELDS.map((f) => ({ key: f.key, label: f.label })),
  ...attrDefs.value.map((d) => ({ key: `${ATTRIBUTE_PREFIX}${d.key}`, label: `${d.label} (attribute)` })),
]);
/** The built-in sorts and the class's attributes (not references: the API cannot sort by them). */
const sortOptions = computed(() => [...SORT_FIELDS, ...attributeSortFields(attrDefs.value)]);
/** A stored sort the class no longer offers (e.g. its attribute was archived); the settings API drops it. */
const staleSort = computed(() => (attrs.data.value ? unavailableSortLabel(view.value?.defaultSort?.field, sortOptions.value) : null));
/**
 * The columns the editor works on. A view stored without columns (migration 0020, the API, an
 * import) shows the default columns, so the editor lists them and the first change starts from
 * them: adding a column must not drop Label and the others.
 */
const editorColumns = computed({
  get: () => (view.value?.columns?.length ? view.value.columns : [...DEFAULT_COLUMNS]),
  set: (v: string[]) => {
    if (view.value) view.value.columns = v;
  },
});

function customize() {
  if (!cls.value) return;
  props.doc.listViews.push({ classKey: cls.value.key, columns: [...DEFAULT_COLUMNS], defaultSort: null, defaultFilters: { ...EMPTY_FILTERS, lookups: {} }, pageSize: null });
}
function removeView() {
  props.doc.listViews = props.doc.listViews.filter((v) => v !== view.value);
}
function setSortField(field: string) {
  if (!view.value) return;
  view.value.defaultSort = field ? { field, direction: view.value.defaultSort?.direction ?? "asc" } : null;
}
// The typed page size is kept here until it is committed on change: a re-render
// (the preview reloading) would otherwise put the stored value back into the box.
const pageSizeText = ref("");
watch(
  () => [view.value, view.value?.pageSize] as const,
  ([v]) => (pageSizeText.value = v?.pageSize == null ? "" : String(v.pageSize)),
  { immediate: true },
);
function setPageSize(v: string) {
  if (!view.value) return;
  const n = Number.parseInt(v, 10);
  view.value.pageSize = Number.isFinite(n) ? Math.min(200, Math.max(10, n)) : null;
  pageSizeText.value = view.value.pageSize == null ? "" : String(view.value.pageSize);
}

// ---------- Preview ----------
const previewQuery = computed<CiListQuery>(() => ({
  classId: cls.value?.id,
  limit: 5,
  sort: ((staleSort.value ? null : sortParam(view.value?.defaultSort)) ?? "label") as CiListQuery["sort"],
}));
const preview = useCiList(previewQuery);
const columns = computed(() => listColumns(view.value?.columns));
</script>

<template>
  <section class="panel">
    <div class="panel-header"><h2>List views</h2><span class="muted">The inventory of one class: columns, sort, filters, page size</span></div>
    <div class="panel-body">
      <ClassPicker v-model:selected="cls" :customized="doc.listViews.map((v) => v.classKey)" noun="list view" />
    </div>
    <div v-if="cls" class="panel-body">
      <template v-if="!view">
        <p>{{ cls.name }} uses the default list: every built-in column, sorted by label, no filters, 50 rows a page.</p>
        <button type="button" class="btn btn-primary" @click="customize">Customize the {{ cls.name }} list</button>
      </template>
      <template v-else>
        <LoadingState v-if="attrs.isLoading.value" label="Loading attributes…" />
        <ErrorAlert v-if="attrs.isError.value" :error="attrs.error.value" title="Could not load the class's attributes" :on-retry="() => attrs.refetch()" />
        <div class="editor-row">
          <div class="field">
            <span class="label">Columns, in order</span>
            <FieldListEditor v-model="editorColumns" :options="columnOptions" label="Columns" id-prefix="lv-col" />
            <p v-if="!view.columns?.length" class="hint">No columns chosen: the default columns are shown.</p>
            <p v-else-if="!view.columns.includes('label')" class="hint">
              Label is not chosen: it is shown as the first column anyway, as it is the link that opens each CI.
            </p>
          </div>
          <div class="form-grid" style="grid-template-columns: 1fr">
            <div class="field">
              <label for="lv-sort">Default sort</label>
              <div class="inline-control">
                <select id="lv-sort" :value="view.defaultSort?.field ?? ''" @change="setSortField(($event.target as HTMLSelectElement).value)">
                  <option value="">Default (label, ascending)</option>
                  <option v-for="s in sortOptions" :key="s.field" :value="s.field">{{ s.label }}</option>
                  <option v-if="staleSort" :value="view.defaultSort!.field">{{ staleSort }}</option>
                </select>
                <select v-if="view.defaultSort" v-model="view.defaultSort.direction" aria-label="Sort direction">
                  <option value="asc">Ascending</option>
                  <option value="desc">Descending</option>
                </select>
              </div>
              <p v-if="staleSort" class="alert alert-warn" role="alert">
                {{ cls.name }} cannot be sorted by {{ view.defaultSort!.field }} (no such attribute, or it is archived or a reference): the list is sorted by label until another sort is chosen.
              </p>
            </div>
            <div class="field">
              <label for="lv-size">Rows per page (10-200)</label>
              <input id="lv-size" type="number" min="10" max="200" placeholder="50" :value="pageSizeText" @input="pageSizeText = ($event.target as HTMLInputElement).value" @change="setPageSize(pageSizeText)" />
            </div>
            <div class="field">
              <label for="lv-q">Default search text</label>
              <input id="lv-q" type="text" maxlength="200" :value="view.defaultFilters!.q ?? ''" @input="view.defaultFilters!.q = ($event.target as HTMLInputElement).value || null" />
            </div>
            <LookupFilterEditor :filters="view.defaultFilters!" legend-prefix="Default: " />
            <p class="hint">
              Default filters are filled in when an operator opens the {{ cls.name }} list without filters of their own;
              they can change or clear them there.
            </p>
          </div>
        </div>
        <div style="margin-top: var(--sp-4)">
          <button type="button" class="btn" @click="removeView">Use the default list for {{ cls.name }}</button>
        </div>
      </template>
    </div>
  </section>

  <section v-if="cls" class="preview-frame" style="margin-top: var(--sp-4)" aria-label="List preview">
    <p class="preview-label">Preview: the first {{ cls.name }} CIs with these columns</p>
    <LoadingState v-if="preview.isLoading.value" />
    <ErrorAlert v-else-if="preview.isError.value" :error="preview.error.value" :on-retry="() => preview.refetch()" />
    <p v-else-if="preview.data.value?.data.length === 0" class="muted">No {{ cls.name }} CIs yet.</p>
    <div v-else class="table-wrap">
      <table class="data">
        <thead>
          <tr><th v-for="c in columns" :key="c" scope="col">{{ fieldLabel(c, attrDefs) }}</th></tr>
        </thead>
        <tbody>
          <tr v-for="ci in preview.data.value?.data ?? []" :key="ci.id">
            <td v-for="c in columns" :key="c"><CiCell :ci="ci" :field="c" :defs="attrDefs" /></td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>
</template>
