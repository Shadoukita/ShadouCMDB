<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import type { EffectiveAttribute } from "../../api/queries";
import {
  addNote,
  addPanel,
  addSection,
  addTab,
  adoptFields,
  allSections,
  canRemoveSection,
  canRemoveTab,
  findSection,
  hideField,
  isCore,
  isFieldSection,
  lastFieldSection,
  locate,
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
} from "../../lib/layoutDesign";
import type { LayoutEditor } from "../../lib/layoutEditor";
import {
  ATTRIBUTE_PREFIX,
  attributeKey,
  CORE_FIELDS,
  fieldLabel,
  gridClass,
  isPanelKind,
  MAX_COLUMNS,
  NOTE_MAX_CHARS,
  PANELS,
  panelLabel,
  placedPanels,
  resolveLayout,
  sectionKind,
  type PanelKind,
} from "../../lib/uiSettings";
import ConfirmDialog from "../ConfirmDialog.vue";
import NoteText from "../NoteText.vue";
import EditableField, { type SectionOption } from "./EditableField.vue";

/**
 * The CI page's fields in layout edit mode: the class layout's tabs, sections
 * and fields, drawn with the page's own rendering of each field (the `field`
 * slot, with the CI's real values) and edited in place. Add a tab at the end of
 * the tab bar and a section between or after sections; click a tab's or a
 * section's name to rename it; drag fields between sections and onto tabs, and
 * a field's right edge to resize it. Every drag has a keyboard or toolbar
 * equivalent. Fields the layout does not place show at the end of the first tab,
 * as on the real page. Next to + Section, + Note adds static text (limited
 * Markdown, edited in place) and + Panel places one of the detail page's
 * built-in panels (relationships, history, audit trail), each once per layout;
 * the page draws a placed panel through the `panel` slot. When the API refuses a
 * save, its messages about a section are listed in that section.
 * Every change goes through the editor (undo, save).
 */
const props = defineProps<{
  editor: LayoutEditor;
  /** The class's active attributes. */
  attrs: readonly EffectiveAttribute[];
  /** The CI form: offers "read-only on the form"; the slot shows each field's own label. */
  form?: boolean;
}>();
defineSlots<{
  field(p: { field: string }): unknown;
  /** After the first tab's sections (the detail page's record details). */
  "first-tab-end"(): unknown;
  /** A built-in panel placed by the layout, as the page shows it (none on the form). */
  panel?(p: { kind: PanelKind }): unknown;
}>();

const layout = computed(() => props.editor.layout);
const defFor = (f: string) => props.attrs.find((d) => d.key === attributeKey(f));
const labelOf = (f: string) => fieldLabel(f, props.attrs);
const known = (f: string) => !f.startsWith(ATTRIBUTE_PREFIX) || !!defFor(f);
const shownFields = (s: LayoutSection) => (s.fields ?? []).filter((f) => known(f.field));
const isReadOnly = (f: string) => !!layout.value?.readOnlyFields?.includes(f);

const tabs = computed<LayoutTab[]>(() => layout.value?.tabs ?? []);
const activeTabKey = ref("");
const activeTab = computed(() => tabs.value.find((t) => t.key === activeTabKey.value) ?? tabs.value[0]);
const onFirstTab = computed(() => !!activeTab.value && activeTab.value === tabs.value[0]);
/** What the layout leaves to the built-in placement: automatic sections at the end of the first tab. */
const autoSections = computed(() => (layout.value ? (resolveLayout(layout.value, props.attrs, CORE_FIELDS, [], true)[0]?.sections ?? []).filter((s) => s.auto) : []));
const hidden = computed(() => (layout.value?.hiddenFields ?? []).filter((f) => !isCore(f) && known(f)));
const sectionOptions = computed<SectionOption[]>(() =>
  layout.value ? allSections(layout.value).filter((x) => isFieldSection(x.section)).map(({ tab, section }) => ({ key: section.key, label: section.label, tab: tab.label })) : [],
);
/** Panels the layout does not place yet: the ones + Panel offers. */
const freePanels = computed(() => {
  const placed = placedPanels(layout.value);
  return PANELS.filter((p) => !placed.has(p.kind));
});
const kindOf = (s: LayoutSection) => sectionKind(s);
const errorsOf = (s: LayoutSection) => props.editor.sectionErrors[s.key] ?? [];

// ---------- Announcements and focus ----------

const announcement = ref("");
function say(text: string) {
  announcement.value = "";
  void nextTick(() => (announcement.value = text));
}
function focus(id: string) {
  void nextTick(() => document.getElementById(id)?.focus());
}
function showTabOf(field: string) {
  const at = layout.value && locate(layout.value, field);
  if (at) activeTabKey.value = at.tab.key;
}

// ---------- Fields ----------

function moveField(field: string, delta: -1 | 1) {
  let into: LayoutSection | undefined;
  props.editor.apply((l) => (into = moveFieldBy(l, field, delta)));
  if (!into || !layout.value) return say(`${labelOf(field)} is already ${delta < 0 ? "first" : "last"}.`);
  showTabOf(field);
  const at = locate(layout.value, field)!;
  say(`${labelOf(field)} moved to position ${at.index + 1} of ${at.section.fields?.length} in ${at.section.label}.`);
  focus(`le-field-${field}`);
}
function resizeField(field: string, width: number) {
  let w = 1;
  props.editor.apply((l) => (w = setWidth(l, field, width)));
  const at = layout.value && locate(layout.value, field);
  if (at) say(`${labelOf(field)}: ${w} of ${at.section.columns} columns.`);
}
function hide(field: string) {
  if (isCore(field)) return say(`${labelOf(field)} belongs to every CI: it can be moved, not hidden.`);
  props.editor.apply((l) => hideField(l, field));
  say(`${labelOf(field)} hidden. It is listed under Hidden fields.`);
}
function place(field: string, key: string, index?: number) {
  if (!key) return;
  props.editor.apply((l) => placeField(l, field, key, index));
  showTabOf(field);
  const at = layout.value && locate(layout.value, field);
  say(`${labelOf(field)} moved to ${at?.tab.label} › ${at?.section.label}.`);
  focus(`le-field-${field}`);
}
/** Shows a hidden field again, in the first field section of the tab in view. */
function show(field: string) {
  const target = activeTab.value?.sections?.find(isFieldSection) ?? (layout.value && allSections(layout.value).find((x) => isFieldSection(x.section))?.section);
  if (target) place(field, target.key);
}
function setReadOnly(field: string, on: boolean) {
  props.editor.apply((l) => {
    const cur = l.readOnlyFields ?? [];
    l.readOnlyFields = on ? [...cur.filter((f) => f !== field), field] : cur.filter((f) => f !== field);
  });
  say(`${labelOf(field)} is ${on ? "read-only" : "editable"} on the form.`);
}
function makeSection(label: string, fields: string[]) {
  let key = "";
  props.editor.apply((l) => (key = adoptFields(l, label, fields).key));
  say(`Section ${label} added with ${fields.length} field${fields.length === 1 ? "" : "s"}.`);
  focus(`le-section-${key}`);
}

// ---------- Tabs and sections ----------

const renaming = ref<{ kind: "tab" | "section"; key: string } | null>(null);
const renameValue = ref("");
function startRename(kind: "tab" | "section", key: string, label: string) {
  renaming.value = { kind, key };
  renameValue.value = label;
  void nextTick(() => {
    const input = document.getElementById("le-rename") as HTMLInputElement | null;
    input?.focus();
    input?.select();
  });
}
function commitRename() {
  const r = renaming.value;
  if (!r) return;
  renaming.value = null;
  const label = renameValue.value.trim();
  const target = r.kind === "tab" ? tabs.value.find((t) => t.key === r.key) : layout.value && findSection(layout.value, r.key)?.section;
  if (target && label && label !== target.label) {
    props.editor.apply((l) => {
      const t = r.kind === "tab" ? l.tabs?.find((x) => x.key === r.key) : findSection(l, r.key)?.section;
      if (!t) return;
      // A tab's only section still named after it (a new tab's) follows the new name.
      const follower = r.kind === "tab" && "sections" in t && t.sections?.length === 1 && t.sections[0].label === t.label ? t.sections[0] : null;
      t.label = label.slice(0, 100);
      if (follower) follower.label = t.label;
    });
    say(`${r.kind === "tab" ? "Tab" : "Section"} renamed to ${label}.`);
  }
  focus(r.kind === "tab" ? `le-tab-${r.key}` : `le-section-${r.key}`);
}
function cancelRename() {
  const r = renaming.value;
  renaming.value = null;
  if (r) focus(r.kind === "tab" ? `le-tab-${r.key}` : `le-section-${r.key}`);
}
function onRenameKey(e: KeyboardEvent) {
  if (e.key === "Enter") {
    e.preventDefault();
    commitRename();
  } else if (e.key === "Escape") {
    e.preventDefault();
    cancelRename();
  }
}

function onTabClick(t: LayoutTab) {
  if (t === activeTab.value) startRename("tab", t.key, t.label);
  else activeTabKey.value = t.key;
}
function onAddTab() {
  let t: LayoutTab | undefined;
  props.editor.apply((l) => (t = addTab(l, `Tab ${(l.tabs?.length ?? 0) + 1}`)));
  if (!t) return;
  activeTabKey.value = t.key;
  say(`Tab ${t.label} added with an empty section. Type its name.`);
  startRename("tab", t.key, t.label);
}
function onMoveTab(t: LayoutTab, delta: -1 | 1) {
  props.editor.apply((l) => {
    const own = l.tabs?.find((x) => x.key === t.key);
    if (own) moveTab(l, own, delta);
  });
  say(`Tab ${t.label} moved ${delta < 0 ? "left" : "right"}.`);
  focus(`le-tab-${t.key}`);
}
function insertSection(index: number) {
  const tab = activeTab.value;
  if (!tab) return;
  let s: LayoutSection | undefined;
  props.editor.apply((l) => {
    const own = l.tabs?.find((x) => x.key === tab.key);
    if (own) s = addSection(l, own, "New section", index);
  });
  if (!s) return;
  say(`Section added to ${tab.label}. Type its name.`);
  startRename("section", s.key, s.label);
}
function insertNote(index: number) {
  const tab = activeTab.value;
  if (!tab) return;
  let s: LayoutSection | undefined;
  props.editor.apply((l) => {
    const own = l.tabs?.find((x) => x.key === tab.key);
    if (own) s = addNote(l, own, "Note", "Write the note here.", index);
  });
  if (!s) return;
  say(`Note added to ${tab.label}. Type its text.`);
  startNote(s);
}
function insertPanel(index: number, e: Event) {
  const select = e.target as HTMLSelectElement;
  const kind = select.value as PanelKind;
  select.value = "";
  const tab = activeTab.value;
  if (!tab || !kind) return;
  let s: LayoutSection | undefined;
  props.editor.apply((l) => {
    const own = l.tabs?.find((x) => x.key === tab.key);
    if (own) s = addPanel(l, own, kind, index);
  });
  if (!s) return say(`The ${panelLabel(kind)} panel is already placed.`);
  say(`${panelLabel(kind)} panel placed in ${tab.label}.`);
  focus(`le-section-${s.key}`);
}

// ---------- Notes ----------

/** The note whose text is being edited, and the text so far. */
const noteKey = ref<string | null>(null);
const noteDraft = ref("");
const noteBlank = computed(() => noteDraft.value.trim() === "");
function startNote(s: LayoutSection) {
  noteKey.value = s.key;
  noteDraft.value = s.text ?? "";
  void nextTick(() => {
    const area = document.getElementById(`le-note-${s.key}`) as HTMLTextAreaElement | null;
    area?.focus();
    area?.select();
  });
}
function commitNote() {
  const key = noteKey.value;
  if (!key || noteBlank.value) return;
  const text = noteDraft.value.slice(0, NOTE_MAX_CHARS);
  noteKey.value = null;
  onSection(key, (_, own) => (own.text = text));
  say("Note text changed.");
  focus(`le-section-${key}`);
}
function cancelNote() {
  const key = noteKey.value;
  noteKey.value = null;
  if (key) focus(`le-section-${key}`);
}
function onNoteKey(e: KeyboardEvent) {
  if (e.key === "Escape") {
    e.preventDefault();
    cancelNote();
  } else if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
    e.preventDefault();
    commitNote();
  }
}

/** Runs `change` on the section `key` of the layout being edited. */
function onSection(key: string, change: (l: NonNullable<typeof layout.value>, s: LayoutSection) => void) {
  props.editor.apply((l) => {
    const s = findSection(l, key)?.section;
    if (s) change(l, s);
  });
}
function onMoveSection(s: LayoutSection, delta: -1 | 1) {
  onSection(s.key, (l, own) => moveSection(l, own, delta));
  say(`Section ${s.label} moved ${delta < 0 ? "up" : "down"}.`);
  focus(`le-section-${s.key}`);
}
function onSectionTab(s: LayoutSection, tabKey: string) {
  onSection(s.key, (l, own) => {
    const t = l.tabs?.find((x) => x.key === tabKey);
    if (t) moveSectionToTab(l, own, t);
  });
  activeTabKey.value = tabKey;
  say(`Section ${s.label} moved to the tab ${tabs.value.find((t) => t.key === tabKey)?.label}.`);
  focus(`le-section-${s.key}`);
}
const sectionIndex = (s: LayoutSection) => activeTab.value?.sections?.indexOf(s) ?? -1;

const confirmRemove = ref<{ tab: LayoutTab } | { section: LayoutSection } | null>(null);
const removeTitle = computed(() => {
  const c = confirmRemove.value;
  return !c ? "" : "tab" in c ? `Remove the tab ${c.tab.label}?` : `Remove the section ${c.section.label}?`;
});
const removeText = computed(() => (confirmRemove.value && layout.value ? removalSummary(layout.value, confirmRemove.value) : ""));
function doRemove() {
  const c = confirmRemove.value;
  confirmRemove.value = null;
  if (!c) return;
  if ("tab" in c) {
    props.editor.apply((l) => {
      const own = l.tabs?.find((x) => x.key === c.tab.key);
      if (own) removeTab(l, own);
    });
    say(`Tab ${c.tab.label} removed.`);
  } else {
    onSection(c.section.key, (l, own) => removeSection(l, own));
    say(`Section ${c.section.label} removed.`);
  }
}

// A class change (another CI, another class) starts on the first tab; a removed tab falls back to it.
watch(
  () => props.editor.layout?.classKey,
  () => (activeTabKey.value = ""),
);

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
  const cells = [...(e.currentTarget as HTMLElement).querySelectorAll<HTMLElement>(":scope > .le-field")];
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
  if (field) place(field, section.key, at?.section === section.key ? at.index : undefined);
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
  if (!field) return;
  props.editor.apply((l) => {
    const own = l.tabs?.find((x) => x.key === t.key);
    if (!own) return;
    placeField(l, field, lastFieldSection(l, own).key);
  });
  activeTabKey.value = t.key;
  const at = layout.value && locate(layout.value, field);
  say(`${labelOf(field)} moved to ${t.label} › ${at?.section.label}.`);
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
</script>

<template>
  <div class="le-canvas">
    <div
      class="le-hidden"
      :class="{ 'drop-target': dragField && !isCore(dragField) }"
      data-testid="le-hidden"
      role="region"
      aria-label="Hidden fields"
      @dragover="onHiddenOver"
      @drop="onHiddenDrop"
    >
      <strong>Hidden fields</strong>
      <ul v-if="hidden.length > 0" aria-label="Hidden fields">
        <li v-for="f in hidden" :key="f" draggable="true" @dragstart="onDragStart(f, $event)" @dragend="onDragEnd">
          {{ labelOf(f) }}<span v-if="defFor(f)?.isRequired" class="badge">required</span>
          <button type="button" class="btn btn-sm" :aria-label="`Show ${labelOf(f)}`" @click="show(f)">Show</button>
        </li>
      </ul>
      <span v-else class="muted">None.</span>
      <span class="hint">Drop a field here to hide it from the form and the detail page.</span>
    </div>

    <div class="le-frame" :style="editor.previewWidth ? { width: `${editor.previewWidth}px` } : undefined" data-testid="le-frame">
      <div class="layout-container">
        <div class="le-tabs" role="group" aria-label="Tabs of the layout">
          <div v-for="t in tabs" :key="t.key" :class="['le-tab', { current: t === activeTab, 'drop-target': dropTab === t.key }]" @dragover="onTabOver(t, $event)" @dragleave="dropTab = null" @drop="onTabDrop(t, $event)">
            <input
              v-if="renaming?.kind === 'tab' && renaming.key === t.key"
              id="le-rename"
              v-model="renameValue"
              class="le-rename"
              type="text"
              maxlength="100"
              aria-label="Tab name"
              @keydown="onRenameKey"
              @blur="commitRename"
            />
            <button v-else :id="`le-tab-${t.key}`" type="button" class="le-tab-label" :aria-pressed="t === activeTab" :title="t === activeTab ? 'Click to rename' : undefined" @click="onTabClick(t)">
              {{ t.label }}
            </button>
            <span v-if="t === activeTab && !(renaming?.kind === 'tab' && renaming.key === t.key)" class="le-tab-tools" role="toolbar" :aria-label="`Tab ${t.label}: layout`">
              <button type="button" class="btn btn-sm" :aria-label="`Rename tab ${t.label}`" title="Rename" @click="startRename('tab', t.key, t.label)">✎</button>
              <button type="button" class="btn btn-sm" :aria-label="`Move tab ${t.label} left`" title="Move left" :disabled="tabs.indexOf(t) === 0" @click="onMoveTab(t, -1)">←</button>
              <button type="button" class="btn btn-sm" :aria-label="`Move tab ${t.label} right`" title="Move right" :disabled="tabs.indexOf(t) === tabs.length - 1" @click="onMoveTab(t, 1)">→</button>
              <button type="button" class="btn btn-sm" :aria-label="`Remove tab ${t.label}`" title="Remove" :disabled="!layout || !canRemoveTab(layout, t)" @click="confirmRemove = { tab: t }">×</button>
            </span>
          </div>
          <button type="button" class="btn btn-sm le-add" @click="onAddTab">+ Tab</button>
        </div>

        <div class="layout-panels" role="region" :aria-label="`Tab ${activeTab?.label ?? ''}`">
          <template v-for="(s, j) in activeTab?.sections ?? []" :key="s.key">
            <div v-if="j > 0" class="le-insert">
              <button type="button" class="btn btn-sm le-add" :aria-label="`Add a section before ${s.label}`" @click="insertSection(j)">+ Section</button>
              <button type="button" class="btn btn-sm le-add" :aria-label="`Add a note before ${s.label}`" @click="insertNote(j)">+ Note</button>
              <select class="btn btn-sm le-add" :aria-label="`Add a panel before ${s.label}`" :disabled="freePanels.length === 0" :title="freePanels.length === 0 ? 'Every panel is placed' : undefined" @change="insertPanel(j, $event)">
                <option value="">+ Panel</option>
                <option v-for="p in freePanels" :key="p.kind" :value="p.kind">{{ p.label }}</option>
              </select>
            </div>
            <section :class="['panel', 'layout-panel', 'le-section', { 'le-block': kindOf(s) !== 'fields', invalid: errorsOf(s).length > 0 }]" :aria-label="`Section ${s.label}`">
              <div class="panel-header">
                <h2>
                  <input
                    v-if="renaming?.kind === 'section' && renaming.key === s.key"
                    id="le-rename"
                    v-model="renameValue"
                    class="le-rename"
                    type="text"
                    maxlength="100"
                    aria-label="Section name"
                    @keydown="onRenameKey"
                    @blur="commitRename"
                  />
                  <button v-else :id="`le-section-${s.key}`" type="button" class="le-section-label" title="Click to rename" @click="startRename('section', s.key, s.label)">{{ s.label }}</button>
                </h2>
                <span v-if="kindOf(s) === 'note'" class="muted"><span class="badge">Note</span><template v-if="s.collapsed"> · starts collapsed</template></span>
                <span v-else-if="isPanelKind(kindOf(s))" class="muted"><span class="badge">{{ panelLabel(kindOf(s) as PanelKind) }} panel</span><template v-if="s.collapsed"> · starts collapsed</template></span>
                <span v-else class="muted">{{ s.columns }} column{{ s.columns === 1 ? "" : "s" }}<template v-if="s.collapsed"> · starts collapsed</template></span>
                <span class="le-section-tools" role="toolbar" :aria-label="`Section ${s.label}: layout`">
                  <button type="button" class="btn btn-sm" :aria-label="`Move section ${s.label} up`" title="Move up" :disabled="sectionIndex(s) <= 0" @click="onMoveSection(s, -1)">↑</button>
                  <button
                    type="button"
                    class="btn btn-sm"
                    :aria-label="`Move section ${s.label} down`"
                    title="Move down"
                    :disabled="sectionIndex(s) >= (activeTab?.sections?.length ?? 0) - 1"
                    @click="onMoveSection(s, 1)"
                  >
                    ↓
                  </button>
                  <button type="button" class="btn btn-sm" :aria-pressed="!!s.collapsed" :aria-label="`Section ${s.label} starts collapsed on the detail page`" @click="onSection(s.key, (_, own) => (own.collapsed = !own.collapsed))">
                    Collapsed
                  </button>
                  <button v-if="kindOf(s) === 'note'" type="button" class="btn btn-sm" :aria-label="`Edit the text of ${s.label}`" @click="startNote(s)">Edit text</button>
                  <select v-if="kindOf(s) === 'fields'" :aria-label="`Columns of ${s.label}`" :value="s.columns ?? 3" @change="onSection(s.key, (_, own) => setColumns(own, Number(($event.target as HTMLSelectElement).value)))">
                    <option v-for="n in MAX_COLUMNS" :key="n" :value="n">{{ n }} column{{ n === 1 ? "" : "s" }}</option>
                  </select>
                  <select v-if="tabs.length > 1" :aria-label="`Tab of ${s.label}`" :value="activeTab?.key" @change="onSectionTab(s, ($event.target as HTMLSelectElement).value)">
                    <option v-for="t in tabs" :key="t.key" :value="t.key">{{ t.label }}</option>
                  </select>
                  <button type="button" class="btn btn-sm" :aria-label="`Remove section ${s.label}`" :disabled="!layout || !canRemoveSection(layout, s)" @click="confirmRemove = { section: s }">Remove</button>
                </span>
              </div>
              <ul v-if="errorsOf(s).length > 0" class="le-errors" role="alert" :aria-label="`Errors in ${s.label}`">
                <li v-for="(e, k) in errorsOf(s)" :key="k"><code>{{ e.path }}</code> {{ e.message }}</li>
              </ul>
              <div v-if="kindOf(s) === 'note'" class="panel-body">
                <div v-if="noteKey === s.key" class="le-note-edit">
                  <label :for="`le-note-${s.key}`" class="sr-only">Text of {{ s.label }}</label>
                  <textarea
                    :id="`le-note-${s.key}`"
                    v-model="noteDraft"
                    rows="5"
                    :maxlength="NOTE_MAX_CHARS"
                    :aria-invalid="noteBlank"
                    :aria-describedby="`le-note-help-${s.key}`"
                    @keydown="onNoteKey"
                  />
                  <div :id="`le-note-help-${s.key}`" class="hint">
                    <strong v-if="noteBlank" class="le-note-error">A note needs text.</strong>
                    Plain text or limited Markdown: **bold**, *italic*, `code`, [link](https://…), lists with - or 1. HTML is shown as text.
                    {{ noteDraft.length }} / {{ NOTE_MAX_CHARS }} characters. Ctrl+Enter applies, Escape cancels.
                  </div>
                  <span class="row-actions">
                    <button type="button" class="btn btn-sm btn-primary" :disabled="noteBlank" @click="commitNote">Apply</button>
                    <button type="button" class="btn btn-sm" @click="cancelNote">Cancel</button>
                  </span>
                </div>
                <button v-else type="button" class="le-note" :aria-label="`Edit the text of ${s.label}`" title="Click to edit the text" @click="startNote(s)">
                  <NoteText :text="s.text ?? ''" />
                </button>
              </div>
              <div v-else-if="isPanelKind(kindOf(s))" class="panel-body flush" inert>
                <slot name="panel" :kind="kindOf(s) as PanelKind">
                  <p class="hint le-panel-hint">{{ PANELS.find((p) => p.kind === kindOf(s))?.hint }}. Shown on the detail page, not on the form.</p>
                </slot>
              </div>
              <div v-else class="panel-body">
                <div
                  :class="[gridClass(s.columns ?? 3), 'le-grid', { 'drop-end': dropAt?.section === s.key && dropAt.index === shownFields(s).length }]"
                  :data-section="s.key"
                  @dragover="onGridOver(s, $event)"
                  @drop="onGridDrop(s, $event)"
                >
                  <EditableField
                    v-for="(f, i) in shownFields(s)"
                    :key="f.field"
                    :field="f.field"
                    :label="labelOf(f.field)"
                    :width="f.width ?? 1"
                    :columns="s.columns ?? 3"
                    :core="isCore(f.field)"
                    :required="defFor(f.field)?.isRequired"
                    :read-only="isReadOnly(f.field)"
                    :can-read-only="form"
                    :show-label="!form"
                    :section="s.key"
                    :sections="sectionOptions"
                    :drop-before="dropAt?.section === s.key && dropAt.index === i && dragField !== f.field"
                    :dragging="dragField === f.field"
                    @move="(d) => moveField(f.field, d)"
                    @resize="(w) => resizeField(f.field, w)"
                    @hide="hide(f.field)"
                    @place="(k) => place(f.field, k)"
                    @read-only="(on) => setReadOnly(f.field, on)"
                    @dragstart="(e) => onDragStart(f.field, e)"
                    @dragend="onDragEnd"
                  >
                    <slot name="field" :field="f.field" />
                  </EditableField>
                  <p v-if="shownFields(s).length === 0" :class="['le-empty', 'lg-cell', `lg-w-${s.columns ?? 3}`]">Drop fields here</p>
                </div>
              </div>
            </section>
          </template>
          <div class="le-insert">
            <button type="button" class="btn btn-sm le-add" :aria-label="`Add a section to ${activeTab?.label}`" @click="insertSection(activeTab?.sections?.length ?? 0)">+ Section</button>
            <button type="button" class="btn btn-sm le-add" :aria-label="`Add a note to ${activeTab?.label}`" @click="insertNote(activeTab?.sections?.length ?? 0)">+ Note</button>
            <select
              class="btn btn-sm le-add"
              :aria-label="`Add a panel to ${activeTab?.label}`"
              :disabled="freePanels.length === 0"
              :title="freePanels.length === 0 ? 'Every panel is placed' : undefined"
              @change="insertPanel(activeTab?.sections?.length ?? 0, $event)"
            >
              <option value="">+ Panel</option>
              <option v-for="p in freePanels" :key="p.kind" :value="p.kind">{{ p.label }}</option>
            </select>
          </div>

          <template v-if="onFirstTab">
            <section v-for="a in autoSections" :key="a.key" class="panel layout-panel le-section auto" :aria-label="`Not placed: ${a.label}`">
              <div class="panel-header">
                <h2>{{ a.label }}</h2>
                <span class="muted">Not placed by this layout: shown here automatically</span>
                <button type="button" class="btn btn-sm" @click="makeSection(a.label, a.fields.map((f) => f.field))">Make this a section</button>
              </div>
              <div class="panel-body">
                <div :class="[gridClass(a.columns), 'le-grid']">
                  <EditableField
                    v-for="f in a.fields"
                    :key="f.field"
                    :field="f.field"
                    :label="labelOf(f.field)"
                    :width="1"
                    :columns="a.columns"
                    :core="isCore(f.field)"
                    :required="defFor(f.field)?.isRequired"
                    :read-only="isReadOnly(f.field)"
                    :can-read-only="form"
                    :show-label="!form"
                    auto
                    :sections="sectionOptions"
                    :dragging="dragField === f.field"
                    @hide="hide(f.field)"
                    @place="(k) => place(f.field, k)"
                    @read-only="(on) => setReadOnly(f.field, on)"
                    @dragstart="(e) => onDragStart(f.field, e)"
                    @dragend="onDragEnd"
                  >
                    <slot name="field" :field="f.field" />
                  </EditableField>
                </div>
              </div>
            </section>
            <slot name="first-tab-end" />
          </template>
        </div>
      </div>
    </div>
    <p id="le-keys" class="hint le-keys">
      Drag a field by its grip to move it, onto a tab to move it there, or drag its right edge to resize it. Keyboard: on a
      field's grip, Alt+↑ / Alt+↓ move it, Alt+← / Alt+→ make it narrower or wider, Delete hides it; its toolbar moves it to
      another section.
    </p>
    <div class="sr-only" aria-live="assertive">{{ announcement }}</div>
  </div>

  <ConfirmDialog :open="!!confirmRemove" :title="removeTitle" confirm-label="Remove" @confirm="doRemove" @cancel="confirmRemove = null">
    {{ removeText }} Nothing is saved until you press Save.
  </ConfirmDialog>
</template>

<style scoped>
.le-canvas {
  display: flex;
  flex-direction: column;
  gap: var(--sp-3);
}
.le-frame {
  max-width: 100%;
  margin: 0 auto;
  width: 100%;
  padding: var(--sp-3);
  border: 2px dashed var(--c-primary);
  border-radius: var(--radius);
}
.le-hidden {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--sp-2) var(--sp-3);
  padding: var(--sp-2) var(--sp-3);
  border: 1px dashed var(--c-border-strong);
  border-radius: var(--radius);
  background: var(--c-surface-alt);
}
.le-hidden.drop-target {
  outline: 2px dashed var(--c-primary);
}
.le-hidden ul {
  display: contents;
  list-style: none;
}
.le-hidden li {
  display: inline-flex;
  align-items: center;
  gap: var(--sp-1);
  padding: 2px 2px 2px var(--sp-2);
  border: 1px solid var(--c-border);
  border-radius: var(--radius);
  background: var(--c-surface);
  cursor: grab;
}
.le-tabs {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--sp-1);
  border-bottom: 1px solid var(--c-border);
  margin-bottom: var(--sp-4);
}
.le-tab {
  display: flex;
  align-items: center;
  gap: 2px;
  border-bottom: 2px solid transparent;
  margin-bottom: -1px;
}
.le-tab.current {
  border-bottom-color: var(--c-primary);
}
.le-tab.drop-target {
  background: var(--c-row-hover);
  outline: 2px dashed var(--c-primary);
}
.le-tab-label,
.le-section-label {
  border: 0;
  background: none;
  font: inherit;
  color: inherit;
  cursor: pointer;
  padding: 0;
}
.le-tab-label {
  padding: var(--sp-3) var(--sp-3);
  font-weight: 600;
  color: var(--c-text-muted);
}
.le-tab.current .le-tab-label {
  color: var(--c-text);
  cursor: text;
}
.le-section-label {
  cursor: text;
  border-bottom: 1px dashed transparent;
}
.le-section-label:hover {
  border-bottom-color: var(--c-text-muted);
}
.le-tab-label:focus-visible,
.le-section-label:focus-visible {
  outline: 2px solid var(--c-focus);
}
.le-rename {
  font: inherit;
  font-weight: 600;
  min-width: 160px;
  margin: var(--sp-1) 0;
}
.le-add {
  border-style: dashed;
  color: var(--c-primary);
}
.le-tabs > .le-add {
  margin-left: var(--sp-2);
}
.le-tab-tools,
.le-section-tools {
  display: inline-flex;
  align-items: center;
  gap: 2px;
}
.le-section .panel-header {
  gap: var(--sp-3);
  flex-wrap: wrap;
}
.le-section-tools {
  margin-left: auto;
  opacity: 0;
  pointer-events: none;
}
.le-section:hover .le-section-tools,
.le-section-tools:focus-within {
  opacity: 1;
  pointer-events: auto;
}
.le-section-tools select {
  font-size: var(--fs-sm);
  padding: 1px 4px;
}
.le-section-tools [aria-pressed="true"] {
  border-color: var(--c-primary);
  color: var(--c-primary);
}
.le-section.auto {
  border-style: dashed;
}
.le-insert {
  display: flex;
  justify-content: center;
  margin: calc(-1 * var(--sp-2)) 0;
}
.le-insert .le-add {
  opacity: 0.55;
}
.le-insert .le-add:hover,
.le-insert .le-add:focus-visible {
  opacity: 1;
}
.le-grid {
  min-height: 56px;
  padding: 2px;
  row-gap: var(--sp-5);
  padding-top: var(--sp-4);
}
.le-grid.drop-end {
  outline: 2px dashed var(--c-primary);
  outline-offset: 2px;
}
.le-empty {
  margin: 0;
  padding: var(--sp-3);
  border: 1px dashed var(--c-border-strong);
  border-radius: var(--radius);
  color: var(--c-text-muted);
  text-align: center;
}
.le-keys {
  margin: 0;
}
.le-insert {
  gap: var(--sp-2);
}
.le-insert select.le-add {
  width: auto;
}
.le-block {
  border-style: dashed;
}
.le-section.invalid {
  border-color: var(--c-danger);
}
.le-errors {
  margin: 0;
  padding: var(--sp-2) var(--sp-3) var(--sp-2) var(--sp-6);
  color: var(--c-danger);
  background: var(--c-danger-bg);
  font-size: var(--fs-sm);
}
.le-note {
  display: block;
  width: 100%;
  border: 1px dashed transparent;
  border-radius: var(--radius);
  background: none;
  font: inherit;
  color: inherit;
  text-align: left;
  padding: var(--sp-1);
  cursor: text;
}
.le-note:hover {
  border-color: var(--c-border-strong);
}
.le-note:focus-visible {
  outline: 2px solid var(--c-focus);
}
.le-note-edit {
  display: flex;
  flex-direction: column;
  gap: var(--sp-2);
}
.le-note-edit textarea {
  width: 100%;
  font: inherit;
  resize: vertical;
}
.le-note-error {
  color: var(--c-danger);
}
.le-panel-hint {
  margin: 0;
  padding: var(--sp-3);
}
</style>
