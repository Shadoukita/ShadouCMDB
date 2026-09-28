<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch } from "vue";
import { useClassAttributes, type CiClass } from "../../../api/queries";
import type { UiSettingsDocument } from "../../../api/uiSettings";
import ConfirmDialog from "../../../components/ConfirmDialog.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import {
  addSection,
  addTab,
  adoptFields,
  allSections,
  canRemoveSection as canRemoveSectionIn,
  canRemoveTab as canRemoveTabIn,
  findSection,
  hideField,
  isCore,
  locate,
  materialize,
  moveFieldBy,
  moveSection,
  moveSectionToTab,
  moveTab,
  placeField,
  removalSummary,
  removeSection,
  removeTab,
  setColumns,
  setWidth,
  type LayoutSection,
  type LayoutTab,
} from "../../../lib/layoutDesign";
import { ATTRIBUTE_PREFIX, attributeKey, CORE_FIELDS, fieldLabel, gridClass, MAX_COLUMNS, resolveLayout } from "../../../lib/uiSettings";
import ClassPicker from "./ClassPicker.vue";
import DesignerField from "./designer/DesignerField.vue";
import OpenOnCiLink from "./designer/OpenOnCiLink.vue";

/**
 * Customization › Detail and form layout: a visual designer for one class's
 * layout (format v2: tabs → sections → fields on a grid), shared by the CI form
 * and the detail page. The canvas is the class's form as it will look: drag
 * fields between sections and tabs, drag a field's right edge to change its
 * width, and resize the preview to check smaller screens. Everything also works
 * from the keyboard and from the side panel. Fields the layout does not place
 * show at the end of the first tab, as on the real form. Ident, valid from and
 * valid until can be moved but not hidden.
 */
const props = defineProps<{ doc: UiSettingsDocument }>();
const cls = ref<CiClass>();
const layout = computed(() => (cls.value ? props.doc.layouts.find((l) => l.classKey === cls.value!.key) : undefined));
const attrs = useClassAttributes(() => cls.value?.id);
const defs = computed(() => (attrs.data.value ?? []).filter((d) => d.isActive));
const defFor = (f: string) => defs.value.find((d) => d.key === attributeKey(f));
const labelOf = (f: string) => fieldLabel(f, defs.value);
/** Fields the CI form edits: the core fields and the class's attributes. */
const formField = (f: string) => CORE_FIELDS.includes(f) || !!defFor(f);

function customize() {
  if (!cls.value) return;
  props.doc.layouts.push(materialize(cls.value.key, defs.value));
  select(null);
}
const confirmBuiltIn = ref(false);
function useBuiltIn() {
  props.doc.layouts = props.doc.layouts.filter((l) => l !== layout.value);
  confirmBuiltIn.value = false;
  select(null);
}

// ---------- What the canvas shows ----------

const tabs = computed<LayoutTab[]>(() => layout.value?.tabs ?? []);
const activeTabKey = ref("");
const activeTab = computed(() => tabs.value.find((t) => t.key === activeTabKey.value) ?? tabs.value[0]);
const onFirstTab = computed(() => !!activeTab.value && activeTab.value === tabs.value[0]);
/** What the layout leaves to the built-in placement: automatic sections at the end of the first tab. */
const autoSections = computed(() => {
  if (!layout.value) return [];
  const all = resolveLayout(layout.value, defs.value, CORE_FIELDS, [], true);
  return (all[0]?.sections ?? []).filter((s) => s.auto);
});
const hidden = computed(() => (layout.value?.hiddenFields ?? []).filter((f) => !isCore(f) && (!f.startsWith(ATTRIBUTE_PREFIX) || !!defFor(f))));
const isReadOnly = (f: string) => !!layout.value?.readOnlyFields?.includes(f);
const shownFields = (s: LayoutSection) => (s.fields ?? []).filter((f) => !f.field.startsWith(ATTRIBUTE_PREFIX) || !!defFor(f.field));

// ---------- Selection and announcements ----------

type Selection = { kind: "field"; field: string } | { kind: "section"; key: string } | { kind: "tab"; key: string } | null;
const selection = ref<Selection>(null);
const announcement = ref("");
function select(s: Selection) {
  selection.value = s;
}
function say(text: string) {
  announcement.value = "";
  void nextTick(() => (announcement.value = text));
}
const selField = computed(() => (selection.value?.kind === "field" ? selection.value.field : null));
const selFieldPlace = computed(() => (layout.value && selField.value ? locate(layout.value, selField.value) : undefined));
const selSection = computed(() => (layout.value && selection.value?.kind === "section" ? findSection(layout.value, selection.value.key) : undefined));
const selTab = computed(() => (selection.value?.kind === "tab" ? tabs.value.find((t) => t.key === (selection.value as { key: string }).key) : undefined));
const sectionOptions = computed(() => (layout.value ? allSections(layout.value) : []));
// A new class starts with nothing selected, on its first tab.
watch(cls, () => {
  select(null);
  activeTabKey.value = "";
});

function focusField(field: string) {
  void nextTick(() => document.getElementById(`designer-field-${field}`)?.focus());
}
function showTabOf(field: string) {
  const at = layout.value && locate(layout.value, field);
  if (at) activeTabKey.value = at.tab.key;
}

// ---------- Field actions (shared by drag and drop, the keyboard and the side panel) ----------

function moveField(field: string, delta: -1 | 1) {
  if (!layout.value) return;
  const into = moveFieldBy(layout.value, field, delta);
  if (!into) return say(`${labelOf(field)} is already ${delta < 0 ? "first" : "last"}.`);
  showTabOf(field);
  const at = locate(layout.value, field)!;
  say(`${labelOf(field)} moved to position ${at.index + 1} of ${into.fields?.length} in ${into.label}.`);
  focusField(field);
}
function resizeField(field: string, width: number) {
  if (!layout.value) return;
  const at = locate(layout.value, field);
  if (!at) return;
  const w = setWidth(layout.value, field, width);
  say(`${labelOf(field)}: ${w} of ${at.section.columns} columns.`);
}
function hide(field: string) {
  if (!layout.value) return;
  if (!hideField(layout.value, field)) return say(`${labelOf(field)} belongs to every CI: it can be moved, not hidden.`);
  if (selField.value === field) select(null);
  say(`${labelOf(field)} hidden. It is listed under Hidden fields.`);
}
function show(field: string) {
  if (!layout.value) return;
  const target = selFieldPlace.value?.section ?? selSection.value?.section ?? activeTab.value?.sections?.[0] ?? allSections(layout.value)[0]?.section;
  if (!target) return;
  placeField(layout.value, field, target.key);
  showTabOf(field);
  select({ kind: "field", field });
  say(`${labelOf(field)} added to ${target.label}.`);
  focusField(field);
}
function toSection(field: string, key: string) {
  if (!layout.value) return;
  placeField(layout.value, field, key);
  showTabOf(field);
  say(`${labelOf(field)} moved to ${findSection(layout.value, key)?.section.label}.`);
}
function toggleReadOnly(field: string, on: boolean) {
  if (!layout.value) return;
  const cur = layout.value.readOnlyFields ?? [];
  layout.value.readOnlyFields = on ? [...cur.filter((f) => f !== field), field] : cur.filter((f) => f !== field);
}
function placeAuto(label: string, fields: string[]) {
  if (!layout.value) return;
  const s = adoptFields(layout.value, label, fields);
  select({ kind: "section", key: s.key });
  say(`Section ${s.label} added with ${fields.length} field${fields.length === 1 ? "" : "s"}.`);
}

// ---------- Tabs and sections ----------

function onAddTab() {
  if (!layout.value) return;
  const t = addTab(layout.value, `Tab ${tabs.value.length + 1}`);
  activeTabKey.value = t.key;
  select({ kind: "tab", key: t.key });
  say(`Tab ${t.label} added. Rename it in the side panel.`);
}
function onAddSection() {
  if (!layout.value || !activeTab.value) return;
  const s = addSection(layout.value, activeTab.value, "New section");
  select({ kind: "section", key: s.key });
  say(`Section added to ${activeTab.value.label}.`);
}
function onSectionTab(section: LayoutSection, key: string) {
  const t = tabs.value.find((x) => x.key === key);
  if (!layout.value || !t) return;
  moveSectionToTab(layout.value, section, t);
  activeTabKey.value = key;
}
const confirmRemove = ref<{ kind: "tab"; tab: LayoutTab } | { kind: "section"; section: LayoutSection } | null>(null);
const removeTitle = computed(() => {
  const c = confirmRemove.value;
  return !c ? "" : c.kind === "tab" ? `Remove the tab ${c.tab.label}?` : `Remove the section ${c.section.label}?`;
});
const removeText = computed(() => {
  const c = confirmRemove.value;
  if (!c || !layout.value) return "";
  return removalSummary(layout.value, c.kind === "tab" ? { tab: c.tab } : { section: c.section });
});
function doRemove() {
  const c = confirmRemove.value;
  confirmRemove.value = null;
  if (!c || !layout.value) return;
  if (c.kind === "tab") {
    removeTab(layout.value, c.tab);
    say(`Tab ${c.tab.label} removed.`);
  } else {
    removeSection(layout.value, c.section);
    say(`Section ${c.section.label} removed.`);
  }
  select(null);
}
const canRemoveTab = (t: LayoutTab) => !!layout.value && canRemoveTabIn(layout.value, t);
const canRemoveSection = (s: LayoutSection) => !!layout.value && canRemoveSectionIn(layout.value, s);
const sectionIndexInTab = (s: LayoutSection) => sectionOptions.value.find((x) => x.section === s)?.tab.sections?.indexOf(s) ?? -1;

// ---------- Drag and drop ----------

const dragField = ref<string | null>(null);
const dropAt = ref<{ section: string; index: number } | null>(null);
const dropTab = ref<string | null>(null);
function onDragStart(field: string, e: DragEvent) {
  dragField.value = field;
  if (e.dataTransfer) {
    e.dataTransfer.effectAllowed = "move";
    e.dataTransfer.setData("text/plain", field);
  }
}
function onDragEnd() {
  dragField.value = null;
  dropAt.value = null;
  dropTab.value = null;
}
/** Where in a section's grid a drop lands: before the first field the pointer is above or left of, else at the end. */
function onGridOver(section: LayoutSection, e: DragEvent) {
  if (!dragField.value) return;
  e.preventDefault();
  if (e.dataTransfer) e.dataTransfer.dropEffect = "move";
  const cells = [...(e.currentTarget as HTMLElement).querySelectorAll<HTMLElement>(".designer-field")];
  let index = cells.length;
  for (let i = 0; i < cells.length; i++) {
    const r = cells[i].getBoundingClientRect();
    if (e.clientY < r.top || (e.clientY <= r.bottom && e.clientX < r.left + r.width / 2)) {
      index = i;
      break;
    }
  }
  dropAt.value = { section: section.key, index };
}
function onGridDrop(section: LayoutSection, e: DragEvent) {
  e.preventDefault();
  const field = dragField.value;
  const at = dropAt.value;
  onDragEnd();
  if (!field || !layout.value) return;
  placeField(layout.value, field, section.key, at?.section === section.key ? at.index : undefined);
  select({ kind: "field", field });
  say(`${labelOf(field)} moved to ${section.label}.`);
}
function onTabOver(t: LayoutTab, e: DragEvent) {
  if (!dragField.value) return;
  e.preventDefault();
  dropTab.value = t.key;
}
/** Dropping on a tab puts the field at the end of the tab's last section (a tab without one gets one). */
function onTabDrop(t: LayoutTab, e: DragEvent) {
  e.preventDefault();
  const field = dragField.value;
  onDragEnd();
  if (!field || !layout.value) return;
  const sections = t.sections ?? [];
  const into = sections[sections.length - 1] ?? addSection(layout.value, t, t.label);
  placeField(layout.value, field, into.key);
  activeTabKey.value = t.key;
  select({ kind: "field", field });
  say(`${labelOf(field)} moved to ${t.label} › ${into.label}.`);
}
function onHiddenOver(e: DragEvent) {
  if (dragField.value && !isCore(dragField.value)) e.preventDefault();
}
function onHiddenDrop(e: DragEvent) {
  e.preventDefault();
  const field = dragField.value;
  onDragEnd();
  if (field) hide(field);
}

// ---------- Preview width ----------

const PRESETS = [
  { label: "Full width", width: null },
  { label: "Laptop 1024", width: 1024 },
  { label: "Tablet 768", width: 768 },
  { label: "Phone 390", width: 390 },
] as const;
const previewWidth = ref<number | null>(null);
const frame = ref<HTMLElement>();
const measured = ref(0);
let observer: ResizeObserver | undefined;
watch(frame, (el) => {
  observer?.disconnect();
  if (!el) return;
  observer = new ResizeObserver(([entry]) => (measured.value = Math.round(entry.contentRect.width)));
  observer.observe(el);
});
onBeforeUnmount(() => observer?.disconnect());
function setPreview(w: number | null) {
  previewWidth.value = w;
  // A width dragged with the frame's own handle is an inline style; a preset replaces it.
  if (frame.value) frame.value.style.width = w ? `${w}px` : "";
}
</script>

<template>
  <section class="panel">
    <div class="panel-header"><h2>Detail and form layout</h2><span class="muted">Tabs, sections and fields of one class, for the form and the detail page</span></div>
    <div class="panel-body">
      <ClassPicker v-model:selected="cls" :customized="doc.layouts.map((l) => l.classKey)" noun="layout" />
      <OpenOnCiLink v-if="cls" :key="cls.id" :cls="cls" />
    </div>
    <div v-if="cls && !layout" class="panel-body">
      <p>
        {{ cls.name }} uses the built-in layout: one tab with a General section (ident, validity period and the attributes
        without a group), then the class's other attribute groups.
      </p>
      <LoadingState v-if="attrs.isLoading.value" label="Loading attributes…" />
      <ErrorAlert v-else-if="attrs.isError.value" :error="attrs.error.value" title="Could not load the class's attributes" :on-retry="() => attrs.refetch()" />
      <button v-else type="button" class="btn btn-primary" @click="customize">Customize the {{ cls.name }} layout</button>
    </div>
  </section>

  <template v-if="cls && layout">
    <LoadingState v-if="attrs.isLoading.value" label="Loading attributes…" />
    <ErrorAlert v-else-if="attrs.isError.value" :error="attrs.error.value" title="Could not load the class's attributes" :on-retry="() => attrs.refetch()" />
    <div v-else class="designer">
      <aside class="designer-side" aria-label="Layout properties">
        <div class="panel">
          <div class="panel-header"><h2>Properties</h2></div>
          <div class="panel-body designer-props">
            <template v-if="selField && selFieldPlace">
              <p><strong>{{ labelOf(selField) }}</strong> <code class="muted">{{ selField }}</code></p>
              <p v-if="isCore(selField)" class="hint">Core field of every CI: it can be moved, not hidden.</p>
              <div class="field">
                <label for="designer-width">Width</label>
                <select id="designer-width" :value="selFieldPlace.section.fields![selFieldPlace.index].width ?? 1" @change="resizeField(selField, Number(($event.target as HTMLSelectElement).value))">
                  <option v-for="n in selFieldPlace.section.columns ?? 3" :key="n" :value="n">{{ n }} of {{ selFieldPlace.section.columns ?? 3 }} columns</option>
                </select>
              </div>
              <div class="field">
                <label for="designer-section">Section</label>
                <select id="designer-section" :value="selFieldPlace.section.key" @change="toSection(selField, ($event.target as HTMLSelectElement).value)">
                  <optgroup v-for="t in tabs" :key="t.key" :label="t.label">
                    <option v-for="s in t.sections" :key="s.key" :value="s.key">{{ s.label }}</option>
                  </optgroup>
                </select>
              </div>
              <span class="row-actions">
                <button type="button" class="btn btn-sm" @click="moveField(selField, -1)">Move earlier</button>
                <button type="button" class="btn btn-sm" @click="moveField(selField, 1)">Move later</button>
              </span>
              <label v-if="formField(selField)" class="check">
                <input type="checkbox" :checked="isReadOnly(selField)" @change="toggleReadOnly(selField, ($event.target as HTMLInputElement).checked)" />
                Read-only on the form
              </label>
              <p v-if="defFor(selField)?.isRequired && isReadOnly(selField)" class="hint">Required: stays editable on new CIs, which cannot be saved without it.</p>
              <span><button type="button" class="btn btn-sm" :disabled="isCore(selField)" @click="hide(selField)">Hide field</button></span>
            </template>

            <template v-else-if="selSection">
              <div class="field">
                <label for="designer-section-label">Section heading</label>
                <input id="designer-section-label" v-model="selSection.section.label" type="text" maxlength="100" required />
              </div>
              <div class="field">
                <label for="designer-columns">Columns</label>
                <select id="designer-columns" :value="selSection.section.columns ?? 3" @change="setColumns(selSection.section, Number(($event.target as HTMLSelectElement).value))">
                  <option v-for="n in MAX_COLUMNS" :key="n" :value="n">{{ n }}</option>
                </select>
              </div>
              <label class="check"><input v-model="selSection.section.collapsed" type="checkbox" /> Start collapsed on the detail page</label>
              <div class="field">
                <label for="designer-section-tab">Tab</label>
                <select id="designer-section-tab" :value="selSection.tab.key" @change="onSectionTab(selSection.section, ($event.target as HTMLSelectElement).value)">
                  <option v-for="t in tabs" :key="t.key" :value="t.key">{{ t.label }}</option>
                </select>
              </div>
              <span class="row-actions">
                <button type="button" class="btn btn-sm" :disabled="sectionIndexInTab(selSection.section) <= 0" @click="moveSection(layout, selSection.section, -1)">Move up</button>
                <button
                  type="button"
                  class="btn btn-sm"
                  :disabled="sectionIndexInTab(selSection.section) >= (selSection.tab.sections?.length ?? 0) - 1"
                  @click="moveSection(layout, selSection.section, 1)"
                >
                  Move down
                </button>
              </span>
              <span><button type="button" class="btn btn-sm" :disabled="!canRemoveSection(selSection.section)" @click="confirmRemove = { kind: 'section', section: selSection.section }">Remove section</button></span>
              <p v-if="!canRemoveSection(selSection.section)" class="hint">The only section cannot be removed.</p>
            </template>

            <template v-else-if="selTab">
              <div class="field">
                <label for="designer-tab-label">Tab name</label>
                <input id="designer-tab-label" v-model="selTab.label" type="text" maxlength="100" required />
              </div>
              <span class="row-actions">
                <button type="button" class="btn btn-sm" :disabled="tabs.indexOf(selTab) === 0" @click="moveTab(layout, selTab, -1)">Move left</button>
                <button type="button" class="btn btn-sm" :disabled="tabs.indexOf(selTab) === tabs.length - 1" @click="moveTab(layout, selTab, 1)">Move right</button>
              </span>
              <p v-if="tabs.indexOf(selTab) === 0" class="hint">The first tab also shows the fields the layout does not place.</p>
              <span><button type="button" class="btn btn-sm" :disabled="!canRemoveTab(selTab)" @click="confirmRemove = { kind: 'tab', tab: selTab }">Remove tab</button></span>
            </template>

            <p v-else class="muted">Select a field, a section heading or a tab to change it.</p>
          </div>
        </div>

        <div class="panel designer-hidden" :class="{ 'drop-target': dragField && !isCore(dragField) }" data-testid="designer-hidden" @dragover="onHiddenOver" @drop="onHiddenDrop">
          <div class="panel-header"><h2>Hidden fields</h2></div>
          <div class="panel-body">
            <p class="hint">Not on the form or the detail page. Drop a field here to hide it.</p>
            <ul v-if="hidden.length > 0" class="designer-hidden-list" aria-label="Hidden fields">
              <li v-for="f in hidden" :key="f" draggable="true" @dragstart="onDragStart(f, $event)" @dragend="onDragEnd">
                <span>
                  {{ labelOf(f) }}
                  <span v-if="defFor(f)?.isRequired" class="badge">required</span>
                </span>
                <button type="button" class="btn btn-sm" :aria-label="`Show ${labelOf(f)}`" @click="show(f)">Show</button>
              </li>
            </ul>
            <p v-else class="muted">None.</p>
          </div>
        </div>

      </aside>
      <div class="designer-main">
        <div class="designer-toolbar" role="toolbar" aria-label="Preview width">
          <span class="muted">Preview</span>
          <button v-for="p in PRESETS" :key="p.label" type="button" class="btn btn-sm" :aria-pressed="previewWidth === p.width" @click="setPreview(p.width)">
            {{ p.label }}
          </button>
          <span class="muted" data-testid="designer-width">{{ measured }} px wide</span>
          <span class="muted">· drag the frame's lower right corner to resize</span>
          <button type="button" class="btn btn-sm designer-builtin" @click="confirmBuiltIn = true">Use the built-in layout for {{ cls.name }}</button>
        </div>

        <div ref="frame" class="designer-frame" data-testid="designer-frame" role="region" :aria-label="`Preview of the ${cls.name} form`">
          <div class="layout-container">
            <div class="tabs designer-tabs" role="tablist" aria-label="Tabs of the layout">
              <button
                v-for="t in tabs"
                :id="`designer-tab-${t.key}`"
                :key="t.key"
                type="button"
                role="tab"
                :aria-selected="t === activeTab"
                :class="{ 'drop-target': dropTab === t.key, selected: selection?.kind === 'tab' && selection.key === t.key }"
                @click="
                  activeTabKey = t.key;
                  select({ kind: 'tab', key: t.key });
                "
                @dragover="onTabOver(t, $event)"
                @dragleave="dropTab = null"
                @drop="onTabDrop(t, $event)"
              >
                {{ t.label }}
              </button>
              <button type="button" class="btn btn-sm designer-add-tab" @click="onAddTab">+ Add tab</button>
            </div>

            <div class="layout-panels" role="tabpanel" :aria-label="activeTab?.label">
              <section
                v-for="s in activeTab?.sections ?? []"
                :key="s.key"
                :class="['panel', 'designer-section', { selected: selection?.kind === 'section' && selection.key === s.key }]"
                :aria-label="`Section ${s.label}`"
              >
                <div class="panel-header">
                  <h2>
                    <button type="button" class="btn-link" :aria-pressed="selection?.kind === 'section' && selection.key === s.key" @click="select({ kind: 'section', key: s.key })">
                      {{ s.label }}
                    </button>
                  </h2>
                  <span class="muted">{{ s.columns }} column{{ s.columns === 1 ? "" : "s" }}<template v-if="s.collapsed"> · collapsed on the detail page</template></span>
                </div>
                <div class="panel-body">
                  <div
                    :class="[gridClass(s.columns ?? 3), 'designer-grid', { 'drop-end': dropAt?.section === s.key && dropAt.index === shownFields(s).length }]"
                    :data-section="s.key"
                    @dragover="onGridOver(s, $event)"
                    @drop="onGridDrop(s, $event)"
                  >
                    <DesignerField
                      v-for="(f, i) in shownFields(s)"
                      :key="f.field"
                      :field="f.field"
                      :label="labelOf(f.field)"
                      :def="defFor(f.field)"
                      :width="f.width ?? 1"
                      :columns="s.columns ?? 3"
                      :selected="selField === f.field"
                      :read-only="isReadOnly(f.field)"
                      :core="isCore(f.field)"
                      :detail-only="!formField(f.field)"
                      :drop-before="dropAt?.section === s.key && dropAt.index === i && dragField !== f.field"
                      :dragging="dragField === f.field"
                      @select="select({ kind: 'field', field: f.field })"
                      @move="(d) => moveField(f.field, d)"
                      @resize="(w) => resizeField(f.field, w)"
                      @hide="hide(f.field)"
                      @dragstart="(e) => onDragStart(f.field, e)"
                      @dragend="onDragEnd"
                    />
                    <p v-if="shownFields(s).length === 0" :class="['designer-empty', 'lg-cell', `lg-w-${s.columns ?? 3}`]">Drop fields here</p>
                  </div>
                </div>
              </section>

              <template v-if="onFirstTab">
                <section v-for="a in autoSections" :key="a.key" class="panel designer-section auto" :aria-label="`Not placed: ${a.label}`">
                  <div class="panel-header">
                    <h2>{{ a.label }}</h2>
                    <span class="muted">Not placed by this layout: shown here automatically</span>
                    <button type="button" class="btn btn-sm" @click="placeAuto(a.label, a.fields.map((f) => f.field))">Make this a section</button>
                  </div>
                  <div class="panel-body">
                    <div :class="[gridClass(a.columns), 'designer-grid']">
                      <DesignerField
                        v-for="f in a.fields"
                        :key="f.field"
                        :field="f.field"
                        :label="labelOf(f.field)"
                        :def="defFor(f.field)"
                        :width="1"
                        :columns="a.columns"
                        :selected="false"
                        :read-only="isReadOnly(f.field)"
                        :core="isCore(f.field)"
                        :dragging="dragField === f.field"
                        @select="show(f.field)"
                        @move="() => show(f.field)"
                        @resize="() => show(f.field)"
                        @hide="hide(f.field)"
                        @dragstart="(e) => onDragStart(f.field, e)"
                        @dragend="onDragEnd"
                      />
                    </div>
                  </div>
                </section>
              </template>
              <div>
                <button type="button" class="btn btn-sm" @click="onAddSection">+ Add section to {{ activeTab?.label }}</button>
              </div>
            </div>
          </div>
        </div>
        <p id="designer-keys" class="hint designer-keys">
          Drag a field to move it, or drag its right edge to resize it. Keyboard: Tab to a field, Enter selects it, Alt+↑ / Alt+↓
          move it, Alt+← / Alt+→ make it narrower or wider, Delete hides it.
        </p>
      </div>

      <div class="sr-only" aria-live="assertive">{{ announcement }}</div>
    </div>
  </template>

  <ConfirmDialog :open="!!confirmRemove" :title="removeTitle" confirm-label="Remove" @confirm="doRemove" @cancel="confirmRemove = null">
    {{ removeText }} Nothing is saved until you press Save.
  </ConfirmDialog>
  <ConfirmDialog :open="confirmBuiltIn" :title="`Use the built-in layout for ${cls?.name}?`" confirm-label="Use the built-in layout" @confirm="useBuiltIn" @cancel="confirmBuiltIn = false">
    The class's tabs, sections, widths, hidden and read-only fields are dropped from the draft. Nothing is saved until you press Save.
  </ConfirmDialog>
</template>

<style scoped>
.designer {
  display: flex;
  flex-direction: column;
  gap: var(--sp-3);
  margin-top: var(--sp-4);
}
.designer-toolbar {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--sp-2);
  margin-bottom: var(--sp-2);
}
.designer-toolbar [aria-pressed="true"] {
  border-color: var(--c-primary);
  color: var(--c-primary);
}
.designer-frame {
  resize: horizontal;
  overflow: auto;
  min-width: 320px;
  max-width: 100%;
  border: 1px dashed var(--c-border-strong);
  border-radius: var(--radius);
  padding: var(--sp-3);
  background: var(--c-bg);
}
.designer-tabs {
  align-items: center;
  flex-wrap: wrap;
}
.designer-tabs button[role="tab"].drop-target {
  background: var(--c-row-hover);
  outline: 2px dashed var(--c-primary);
}
.designer-tabs button[role="tab"].selected {
  color: var(--c-primary);
}
.designer-add-tab {
  margin-left: var(--sp-2);
}
.designer-section.selected {
  border-color: var(--c-primary);
  box-shadow: 0 0 0 1px var(--c-primary);
}
.designer-section.auto {
  border-style: dashed;
}
.designer-section .panel-header {
  gap: var(--sp-3);
}
.designer-section h2 .btn-link {
  font: inherit;
}
.designer-grid {
  min-height: 56px;
  padding: 2px;
}
.designer-grid.drop-end {
  outline: 2px dashed var(--c-primary);
  outline-offset: 2px;
}
.designer-empty {
  margin: 0;
  padding: var(--sp-3);
  border: 1px dashed var(--c-border-strong);
  border-radius: var(--radius);
  color: var(--c-text-muted);
  text-align: center;
}
.designer-keys {
  margin-top: var(--sp-2);
}
/* Properties and hidden fields stay in view above the preview, which keeps the real form's full width. */
.designer-side {
  position: sticky;
  top: 0;
  z-index: 3;
  display: grid;
  grid-template-columns: minmax(0, 2fr) minmax(240px, 1fr);
  gap: var(--sp-3);
  align-items: stretch;
  background: var(--c-bg);
  padding: var(--sp-2) 0;
}
.designer-side > .panel {
  margin: 0;
}
.designer-props {
  display: flex;
  flex-wrap: wrap;
  align-items: flex-end;
  gap: var(--sp-2) var(--sp-4);
  min-height: 64px;
}
.designer-props p {
  margin: 0;
}
.designer-props .field select,
.designer-props .field input {
  min-width: 160px;
}
.designer-builtin {
  margin-left: auto;
}
.designer-hidden.drop-target {
  outline: 2px dashed var(--c-primary);
}
.designer-hidden-list {
  list-style: none;
  margin: 0;
  padding: 0;
  max-height: 120px;
  overflow: auto;
}
.designer-hidden-list li {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: var(--sp-2);
  padding: var(--sp-1) 0;
  border-bottom: 1px solid var(--c-divider);
  cursor: grab;
}
@media (max-width: 1100px) {
  .designer-side {
    position: static;
    grid-template-columns: minmax(0, 1fr);
  }
}
</style>
