<script setup lang="ts">
import { computed, ref } from "vue";
import { useLookupLists } from "../../../api/datamodel";
import { useAttributesOfClasses, useCiClasses } from "../../../api/queries";
import type { UiListFilters, UiSettingsDocument, UiWidget, UiWidgetType } from "../../../api/uiSettings";
import { moveItem } from "../../../lib/reorder";
import { attributeSortFields, sharedSortAttributes, SORT_FIELDS, unavailableSortLabel, WIDGET_TYPES, widgetLabel } from "../../../lib/uiSettings";
import DashboardWidgets from "../../dashboard/DashboardWidgets.vue";
import KeyChecklist from "./KeyChecklist.vue";
import LookupFilterEditor from "./LookupFilterEditor.vue";

/**
 * Customization › Dashboard: the built-in dashboard, or the administrator's
 * widgets in order. The preview below renders the draft widgets with live data.
 */
const props = defineProps<{ doc: UiSettingsDocument }>();
const widgets = computed(() => props.doc.dashboard.widgets ?? null);
const classes = useCiClasses();
const lookupLists = useLookupLists();
const classOptions = computed(() => (classes.data.value ?? []).map((c) => ({ key: c.key, label: c.name + (c.isAbstract ? " (abstract)" : "") })));
/** The built-in dashboard counts by status when there is a lookup list with key "status". */
const statusListKey = computed(() => lookupLists.data.value?.find((l) => l.key === "status")?.key);

/** Starts from the built-in dashboard's panels, so switching to custom widgets changes nothing until edited. */
function useCustom() {
  props.doc.dashboard.widgets = [
    { id: "by_class", type: "count_by_class", title: null, size: "medium" },
    ...(statusListKey.value ? [{ id: "by_status", type: "count_by_lookup", title: "By status", size: "medium", lookupListKey: statusListKey.value } as UiWidget] : []),
    { id: "recent", type: "recent_changes", title: null, size: "large", limit: 12 },
  ];
}
function useBuiltIn() {
  props.doc.dashboard.widgets = null;
}

const addType = ref<UiWidgetType>("count_by_class");
function addWidget() {
  const list = widgets.value ?? [];
  const taken = new Set(list.map((w) => w.id));
  let id: string = addType.value;
  for (let n = 2; taken.has(id); n++) id = `${addType.value}_${n}`;
  const w: UiWidget = { id, type: addType.value, title: null, size: "medium" };
  if (addType.value === "recent_changes") w.limit = 10;
  if (addType.value === "count_by_lookup") w.lookupListKey = lookupLists.data.value?.[0]?.key;
  if (addType.value === "saved_search") {
    w.limit = 10;
    w.search = { classKeys: [], includeSubclasses: true, filters: { q: null, lookups: {} }, sort: { field: "updatedAt", direction: "desc" } };
  }
  props.doc.dashboard.widgets = [...list, w];
}
const move = (i: number, to: number) => (props.doc.dashboard.widgets = moveItem(widgets.value ?? [], i, to));
const remove = (i: number) => (props.doc.dashboard.widgets = (widgets.value ?? []).filter((_, j) => j !== i));

/** Saved searches always carry filters (normalizeDocument and addWidget fill them in). */
const filters = (w: UiWidget): UiListFilters => w.search!.filters!;
function setLimit(w: UiWidget, v: string) {
  const n = Number.parseInt(v, 10);
  w.limit = Number.isFinite(n) ? Math.min(50, Math.max(1, n)) : undefined;
}
function setSortField(w: UiWidget, field: string) {
  w.search!.sort = field ? { field, direction: w.search!.sort?.direction ?? "asc" } : null;
}

// A saved search can sort by an attribute its classes share; the list API needs a class to sort by one.
const searchClassIds = (w: UiWidget) => (classes.data.value ?? []).filter((c) => w.search?.classKeys?.includes(c.key)).map((c) => c.id);
const classAttrs = useAttributesOfClasses(() => (widgets.value ?? []).flatMap((w) => (w.type === "saved_search" ? searchClassIds(w) : [])));
function sortOptions(w: UiWidget) {
  const ids = searchClassIds(w);
  const attrs = classAttrs.value && ids.length > 0 ? sharedSortAttributes(ids.map((id) => classAttrs.value!.get(id) ?? [])) : [];
  return [...SORT_FIELDS, ...attributeSortFields(attrs)];
}
/** A stored sort the chosen classes do not offer (once their attributes have loaded). */
const staleSort = (w: UiWidget) => (classAttrs.value ? unavailableSortLabel(w.search?.sort?.field, sortOptions(w)) : null);
</script>

<template>
  <section class="panel">
    <div class="panel-header"><h2>Dashboard</h2></div>
    <div class="panel-body">
      <div class="inline-control" role="radiogroup" aria-label="Dashboard content">
        <label class="check"><input type="radio" name="dash-mode" :checked="widgets === null" @change="useBuiltIn" /> Built-in dashboard</label>
        <label class="check"><input type="radio" name="dash-mode" :checked="widgets !== null" @change="useCustom" /> Choose the widgets</label>
      </div>
      <p v-if="widgets === null" class="hint">
        The built-in dashboard shows the number of CIs, counts by class and by status, and the recently changed CIs.
      </p>
    </div>

    <template v-if="widgets !== null">
      <div class="panel-body flush">
        <table class="data">
          <thead>
            <tr>
              <th scope="col">Widget</th>
              <th scope="col">Title</th>
              <th scope="col">Width</th>
              <th scope="col">Options</th>
              <th scope="col"><span class="sr-only">Actions</span></th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="(w, i) in widgets" :key="w.id">
              <td>{{ widgetLabel(w.type) }} <code class="muted">{{ w.id }}</code></td>
              <td>
                <label class="sr-only" :for="`w-title-${w.id}`">Title of {{ w.id }}</label>
                <input :id="`w-title-${w.id}`" type="text" maxlength="100" :placeholder="widgetLabel(w.type)" :value="w.title ?? ''" @input="w.title = ($event.target as HTMLInputElement).value.trim() ? ($event.target as HTMLInputElement).value : null" />
              </td>
              <td>
                <label class="sr-only" :for="`w-size-${w.id}`">Width of {{ w.id }}</label>
                <select :id="`w-size-${w.id}`" v-model="w.size">
                  <option value="small">A third</option>
                  <option value="medium">Half</option>
                  <option value="large">Full width</option>
                </select>
              </td>
              <td class="widget-options">
                <KeyChecklist
                  v-if="w.type === 'count_by_class'"
                  :model-value="w.classKeys ?? []"
                  legend="Classes"
                  hint="None ticked: every class"
                  :options="classOptions"
                  @update:model-value="(v) => (w.classKeys = v)"
                />
                <div v-if="w.type === 'count_by_lookup'" class="field">
                  <label :for="`w-list-${w.id}`">Lookup list</label>
                  <select :id="`w-list-${w.id}`" v-model="w.lookupListKey">
                    <option v-for="l in lookupLists.data.value ?? []" :key="l.id" :value="l.key">{{ l.name }}</option>
                    <option v-if="w.lookupListKey && !lookupLists.data.value?.some((l) => l.key === w.lookupListKey)" :value="w.lookupListKey">
                      {{ w.lookupListKey }} (does not exist)
                    </option>
                  </select>
                </div>
                <div v-if="w.type === 'recent_changes' || w.type === 'saved_search'" class="field">
                  <label :for="`w-limit-${w.id}`">Rows (1-50)</label>
                  <input :id="`w-limit-${w.id}`" type="number" min="1" max="50" :value="w.limit ?? 10" @change="setLimit(w, ($event.target as HTMLInputElement).value)" />
                </div>
                <template v-if="w.type === 'saved_search' && w.search">
                  <KeyChecklist :model-value="w.search.classKeys ?? []" legend="Classes" @update:model-value="(v) => (w.search!.classKeys = v)" hint="None ticked: every class" :options="classOptions" />
                  <label class="check"><input v-model="w.search.includeSubclasses" type="checkbox" /> Include subclasses</label>
                  <div class="field">
                    <label :for="`w-q-${w.id}`">Search text</label>
                    <input :id="`w-q-${w.id}`" type="text" maxlength="200" :value="filters(w).q ?? ''" @input="filters(w).q = ($event.target as HTMLInputElement).value || null" />
                  </div>
                  <LookupFilterEditor :filters="filters(w)" />
                  <div class="inline-control">
                    <label :for="`w-sort-${w.id}`">Sort by</label>
                    <select :id="`w-sort-${w.id}`" :value="w.search.sort?.field ?? ''" @change="setSortField(w, ($event.target as HTMLSelectElement).value)">
                      <option value="">Default (label, ascending)</option>
                      <option v-for="s in sortOptions(w)" :key="s.field" :value="s.field">{{ s.label }}</option>
                      <option v-if="staleSort(w)" :value="w.search.sort!.field">{{ staleSort(w) }}</option>
                    </select>
                    <select v-if="w.search.sort" v-model="w.search.sort.direction" :aria-label="`Sort direction of ${w.id}`">
                      <option value="asc">Ascending</option>
                      <option value="desc">Descending</option>
                    </select>
                  </div>
                  <p v-if="staleSort(w)" class="alert alert-warn" role="alert">
                    Sorting by an attribute needs classes that all have it: tick such classes or pick another sort.
                  </p>
                  <p v-else class="hint">Attribute sorts list the attributes every ticked class has.</p>
                </template>
              </td>
              <td class="row-actions">
                <button type="button" class="btn btn-sm" :disabled="i === 0" :aria-label="`Move ${w.id} up`" @click="move(i, i - 1)">↑</button>
                <button type="button" class="btn btn-sm" :disabled="i === widgets.length - 1" :aria-label="`Move ${w.id} down`" @click="move(i, i + 1)">↓</button>
                <button type="button" class="btn btn-sm" :aria-label="`Remove ${w.id}`" @click="remove(i)">Remove</button>
              </td>
            </tr>
          </tbody>
        </table>
        <p v-if="widgets.length === 0" class="panel-body muted">No widgets: the dashboard will be empty. Add one below.</p>
      </div>
      <div class="panel-body">
        <form class="inline-control" @submit.prevent="addWidget">
          <label for="dash-add-type">Add a widget</label>
          <select id="dash-add-type" v-model="addType" style="max-width: 260px">
            <option v-for="t in WIDGET_TYPES" :key="t.type" :value="t.type">{{ t.label }} — {{ t.hint }}</option>
          </select>
          <button type="submit" class="btn" :disabled="widgets.length >= 50">Add</button>
        </form>
      </div>
    </template>
  </section>

  <section v-if="widgets !== null && widgets.length > 0" class="preview-frame" style="margin-top: var(--sp-4)" aria-label="Dashboard preview">
    <p class="preview-label">Preview with live data</p>
    <DashboardWidgets :widgets="widgets" />
  </section>
</template>

<style scoped>
.widget-options {
  display: flex;
  flex-direction: column;
  gap: var(--sp-3);
  min-width: 320px;
  /* A flex cell takes table.data's fixed row height literally: let it grow with its options instead of clipping them. */
  height: auto;
  max-width: none;
  overflow: visible;
  white-space: normal;
  padding-block: var(--sp-2);
}
</style>
