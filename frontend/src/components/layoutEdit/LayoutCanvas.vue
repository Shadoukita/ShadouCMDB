<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch } from "vue";
import { clampFrame, GUIDE_PX, LAYER_ICONS, layerOf, LAYER_MOVES, readingOrder, STACK_BELOW_PX, tabHeight, toBox, type Frame, type LayerMove, type SnapLine } from "../../lib/freeLayout";
import type { EffectiveAttribute } from "../../api/queries";
import {
  addNote,
  addPanel,
  addSection,
  addSeparator,
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
  moveSeparatorBy,
  moveSectionToTab,
  moveTab,
  placeField,
  placeSeparator,
  removalSummary,
  removeSection,
  removeSeparator,
  removeTab,
  SEPARATOR_MAX_CHARS,
  setColumns,
  setSeparatorLabel,
  setWidth,
  type LayoutField,
  type LayoutSection,
  type LayoutTab,
  type SeparatorPlace,
} from "../../lib/layoutDesign";
import { presentationOnly, type LayoutEditor } from "../../lib/layoutEditor";
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
import FreeWindow from "./FreeWindow.vue";
import Icon from "../Icon.vue";
import { t } from "../../i18n";

/**
 * The CI page's fields in layout edit mode: the class layout's tabs, sections
 * and fields, drawn with the page's own rendering of each field (the `field`
 * slot, with the CI's real values) and edited in place. Add a tab at the end of
 * the tab bar, a section below or next to another; click a tab's or a section's
 * name to rename it; drag fields between sections and onto tabs, and a field's
 * right edge to resize it. Every section is a window (FreeWindow): moved and
 * resized anywhere, overlapping, stacked in layers. Every drag has a keyboard
 * or toolbar equivalent, and a drag is one undo step. Fields the layout does
 * not place show below the windows of the first tab, as on the real page. A
 * field section's + Separator adds a line across it (with an optional label),
 * moved like a field and removed with its toolbar. Below the windows, + Note
 * adds static text (limited Markdown, edited in place) and + Panel places one
 * of the detail page's built-in panels (record details, relationships, history,
 * audit trail), each once per layout; the page draws a placed panel through the
 * `panel` slot. Notes and panels are windows like any section, and removing
 * one takes it off the detail page until + Panel adds it again.
 * When the API refuses a save, its messages about a section are listed in that
 * section. Every change goes through the editor (undo, save).
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
  /** A built-in panel placed by the layout, as the page shows it (none on the form). */
  panel?(p: { kind: PanelKind }): unknown;
}>();

/** The mouse and keyboard help under the canvas, collapsed; the grips point at its list (aria-describedby). */
const KEY_HELP = ["layoutEditor.keys.fieldMouse", "layoutEditor.keys.fieldKeys", "layoutEditor.keys.separator", "layoutEditor.keys.windowMouse", "layoutEditor.keys.snap", "layoutEditor.keys.windowKeys"] as const;
const layout = computed(() => props.editor.layout);
const defFor = (f: string) => props.attrs.find((d) => d.key === attributeKey(f));
const labelOf = (f: string) => fieldLabel(f, props.attrs);
const known = (f: string) => !f.startsWith(ATTRIBUTE_PREFIX) || !!defFor(f);
/** A section's entries the editor shows: its fields of known attributes and its separators, with their index among `fields`. */
type Entry = { f: LayoutField & { field: string }; at: number; sep: false } | { f: LayoutField; at: number; sep: true };
const entriesOf = (s: LayoutSection): Entry[] =>
  (s.fields ?? []).flatMap((f, at): Entry[] => (f.separator ? [{ f, at, sep: true }] : f.field && known(f.field) ? [{ f: f as LayoutField & { field: string }, at, sep: false }] : []));
const hasFields = (s: LayoutSection) => entriesOf(s).some((e) => !e.sep);
const isReadOnly = (f: string) => !!layout.value?.readOnlyFields?.includes(f);

const tabs = computed<LayoutTab[]>(() => layout.value?.tabs ?? []);
/** The tab in view is the editor's, so the edit bar's layer controls apply to it. */
const activeTabKey = computed({
  get: () => props.editor.tabKey,
  set: (key: string) => (props.editor.tabKey = key),
});
const activeTab = computed(() => props.editor.tab);
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

const say = (text: string) => props.editor.say(text);
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
  if (!into || !layout.value) return say(t(delta < 0 ? "layoutEditor.say.alreadyFirst" : "layoutEditor.say.alreadyLast", { field: labelOf(field) }));
  showTabOf(field);
  const at = locate(layout.value, field)!;
  say(t("layoutEditor.say.fieldMoved", { field: labelOf(field), pos: at.index + 1, count: at.section.fields?.length, section: at.section.label }));
  focus(`le-field-${field}`);
}
function resizeField(field: string, width: number, drag = false) {
  let w = 1;
  props.editor.apply((l) => (w = setWidth(l, field, width)), drag ? `field:${field}` : undefined);
  const at = layout.value && locate(layout.value, field);
  if (at) say(t("layoutEditor.say.fieldWidth", { field: labelOf(field), width: w, columns: at.section.columns }));
}
function hide(field: string) {
  if (isCore(field)) return say(t("layoutEditor.say.coreNotHidden", { field: labelOf(field) }));
  props.editor.apply((l) => hideField(l, field));
  say(t("layoutEditor.say.hidden", { field: labelOf(field) }));
}
function place(field: string, key: string, index?: number) {
  if (!key) return;
  props.editor.apply((l) => placeField(l, field, key, index));
  showTabOf(field);
  const at = layout.value && locate(layout.value, field);
  say(t("layoutEditor.say.placed", { field: labelOf(field), tab: at?.tab.label, section: at?.section.label }));
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
  say(t(on ? "layoutEditor.say.readOnly" : "layoutEditor.say.editable", { field: labelOf(field) }));
}
function makeSection(label: string, fields: string[]) {
  let key = "";
  props.editor.apply((l) => (key = adoptFields(l, label, fields).key));
  say(t("layoutEditor.say.sectionAdopted", { section: label, n: fields.length }));
  focus(`le-section-${key}`);
}

// ---------- Separators ----------

/** The separator whose label is being edited, and the label so far. */
const sepEdit = ref<SeparatorPlace | null>(null);
const sepLabel = ref("");
const sepId = (at: SeparatorPlace) => `le-sep-${at.section}-${at.index}`;
const sepName = (f: LayoutField) => (f.label ? t("layoutEditor.separatorNamed", { label: f.label }) : t("layoutEditor.separator"));
function insertSeparator(s: LayoutSection) {
  let at: SeparatorPlace | undefined;
  props.editor.apply((l) => (at = addSeparator(l, s.key)));
  if (!at) return;
  say(t("layoutEditor.say.separatorAdded", { section: s.label }));
  startSeparatorLabel(at, "");
}
function startSeparatorLabel(at: SeparatorPlace, label: string) {
  sepEdit.value = at;
  sepLabel.value = label;
  void nextTick(() => {
    const input = document.getElementById("le-sep-label") as HTMLInputElement | null;
    input?.focus();
    input?.select();
  });
}
function commitSeparatorLabel() {
  const at = sepEdit.value;
  if (!at) return;
  sepEdit.value = null;
  const label = sepLabel.value;
  props.editor.apply((l) => setSeparatorLabel(l, at, label));
  say(label.trim() ? t("layoutEditor.say.separatorLabelled", { label: label.trim() }) : t("layoutEditor.say.separatorUnlabelled"));
  focus(sepId(at));
}
function onSeparatorLabelKey(e: KeyboardEvent) {
  if (e.key === "Enter") {
    e.preventDefault();
    commitSeparatorLabel();
  } else if (e.key === "Escape") {
    e.preventDefault();
    const at = sepEdit.value;
    sepEdit.value = null;
    if (at) focus(sepId(at));
  }
}
function moveSeparator(from: SeparatorPlace, delta: -1 | 1) {
  let to: SeparatorPlace | undefined;
  props.editor.apply((l) => (to = moveSeparatorBy(l, from, delta)));
  if (!to || !layout.value) return say(t(delta < 0 ? "layoutEditor.say.separatorFirst" : "layoutEditor.say.separatorLast"));
  const at = findSection(layout.value, to.section);
  if (at) activeTabKey.value = at.tab.key;
  say(t("layoutEditor.say.separatorMoved", { pos: to.index + 1, count: at?.section.fields?.length, section: at?.section.label }));
  focus(sepId(to));
}
function dropSeparator(from: SeparatorPlace) {
  props.editor.apply((l) => removeSeparator(l, from));
  say(t("layoutEditor.say.separatorRemoved"));
}
function onSeparatorKey(at: SeparatorPlace, f: LayoutField, e: KeyboardEvent) {
  if (e.altKey && (e.key === "ArrowUp" || e.key === "ArrowDown")) {
    e.preventDefault();
    moveSeparator(at, e.key === "ArrowUp" ? -1 : 1);
  } else if (e.key === "Delete") {
    e.preventDefault();
    dropSeparator(at);
  } else if (e.key === "Enter" || e.key === "F2") {
    e.preventDefault();
    startSeparatorLabel(at, f.label ?? "");
  }
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
  const target = r.kind === "tab" ? tabs.value.find((tb) => tb.key === r.key) : layout.value && findSection(layout.value, r.key)?.section;
  if (target && label && label !== target.label) {
    props.editor.apply((l) => {
      const own = r.kind === "tab" ? l.tabs?.find((x) => x.key === r.key) : findSection(l, r.key)?.section;
      if (!own) return;
      // A tab's only section still named after it (a new tab's) follows the new name.
      const follower = r.kind === "tab" && "sections" in own && own.sections?.length === 1 && own.sections[0].label === own.label ? own.sections[0] : null;
      own.label = label.slice(0, 100);
      if (follower) follower.label = own.label;
    });
    say(t(r.kind === "tab" ? "layoutEditor.say.tabRenamed" : "layoutEditor.say.sectionRenamed", { label }));
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

function onTabClick(tb: LayoutTab) {
  if (tb === activeTab.value) startRename("tab", tb.key, tb.label);
  else activeTabKey.value = tb.key;
}
function onAddTab() {
  let tb: LayoutTab | undefined;
  props.editor.apply((l) => (tb = addTab(l, t("layoutEditor.newTab", { n: (l.tabs?.length ?? 0) + 1 }))));
  if (!tb) return;
  activeTabKey.value = tb.key;
  say(t("layoutEditor.say.tabAdded", { tab: tb.label }));
  startRename("tab", tb.key, tb.label);
}
function onMoveTab(tb: LayoutTab, delta: -1 | 1) {
  props.editor.apply((l) => {
    const own = l.tabs?.find((x) => x.key === tb.key);
    if (own) moveTab(l, own, delta);
  });
  say(t(delta < 0 ? "layoutEditor.say.tabMovedLeft" : "layoutEditor.say.tabMovedRight", { tab: tb.label }));
  focus(`le-tab-${tb.key}`);
}
/** Adds a section at `index` of the tab in view: a window below the others. */
function insertSection(index: number) {
  const tab = activeTab.value;
  if (!tab) return;
  let s: LayoutSection | undefined;
  props.editor.apply((l) => {
    const own = l.tabs?.find((x) => x.key === tab.key);
    if (own) s = addSection(l, own, t("layoutEditor.newSection"), index);
  });
  if (!s) return;
  say(t("layoutEditor.say.sectionAdded", { tab: tab.label }));
  startRename("section", s.key, s.label);
}
function insertNote(index: number) {
  const tab = activeTab.value;
  if (!tab) return;
  let s: LayoutSection | undefined;
  props.editor.apply((l) => {
    const own = l.tabs?.find((x) => x.key === tab.key);
    if (own) s = addNote(l, own, t("layoutEditor.addNote"), t("layoutEditor.newNoteText"), index);
  });
  if (!s) return;
  say(t("layoutEditor.say.noteAdded", { tab: tab.label }));
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
  if (!s) return say(t("layoutEditor.say.panelAlreadyPlaced", { panel: panelLabel(kind) }));
  say(t("layoutEditor.say.panelPlaced", { panel: panelLabel(kind), tab: tab.label }));
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
  say(t("layoutEditor.say.noteChanged"));
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

/** Runs `change` on the section `key` of the layout being edited (`group`: one undo step for a drag). */
function onSection(key: string, change: (l: NonNullable<typeof layout.value>, s: LayoutSection) => void, group?: string) {
  props.editor.apply((l) => {
    const s = findSection(l, key)?.section;
    if (s) change(l, s);
  }, group);
}
function onSectionTab(s: LayoutSection, tabKey: string) {
  onSection(s.key, (l, own) => {
    const to = l.tabs?.find((x) => x.key === tabKey);
    if (to) moveSectionToTab(l, own, to);
  });
  activeTabKey.value = tabKey;
  say(t("layoutEditor.say.sectionMovedToTab", { section: s.label, tab: tabs.value.find((tb) => tb.key === tabKey)?.label }));
  focus(`le-section-${s.key}`);
}
const confirmRemove = ref<{ tab: LayoutTab } | { section: LayoutSection } | null>(null);
const removeTitle = computed(() => {
  const c = confirmRemove.value;
  return !c ? "" : "tab" in c ? t("layoutEditor.removeTabTitle", { tab: c.tab.label }) : t("layoutEditor.removeSectionTitle", { section: c.section.label });
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
    say(t("layoutEditor.say.tabRemoved", { tab: c.tab.label }));
  } else {
    onSection(c.section.key, (l, own) => removeSection(l, own));
    say(t("layoutEditor.say.sectionRemoved", { section: c.section.label }));
  }
}

// A class change (another CI, another class) starts on the first tab; a removed tab falls back to it.
watch(
  () => props.editor.layout?.classKey,
  () => (activeTabKey.value = ""),
);

// ---------- Windows ----------

const areaEl = ref<HTMLElement>();
const areaWidth = ref(0);
const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(([e]) => (areaWidth.value = e.contentRect.width));
watch(areaEl, (el, old) => {
  if (old) observer?.unobserve(old);
  if (el) {
    observer?.observe(el);
    areaWidth.value = el.getBoundingClientRect().width;
  }
});
onBeforeUnmount(() => observer?.disconnect());
/** Below the tablet breakpoint a tab stacks its windows, as the page does: nothing to drag there. */
const stacked = computed(() => areaWidth.value > 0 && areaWidth.value < STACK_BELOW_PX);
/** The sections of the tab in view, in reading order (kept while a window moves, so it stays under the pointer). */
const frozen = ref<string[] | null>(null);
const shownSections = computed(() => {
  const list = activeTab.value?.sections ?? [];
  if (frozen.value) return frozen.value.map((k) => list.find((s) => s.key === k)).filter((s): s is LayoutSection => !!s);
  return readingOrder(list);
});
/** Room below the lowest window to drag windows into. */
const freeHeight = computed(() => (activeTab.value ? tabHeight(activeTab.value) : 0) + 160);
const guides = ref<SnapLine[]>([]);
const moving = ref(false);
const othersOf = (s: LayoutSection) => (activeTab.value?.sections ?? []).filter((o) => o !== s && o.frame).map((o) => toBox(o.frame!, areaWidth.value));
const layerAt = (s: LayoutSection) => (activeTab.value ? layerOf(activeTab.value, s) : { index: 0, count: 0 });

/** A window moved or resized: dragged (one undo step per drag, with the bring-to-front that started it) or by a key. */
function onFrame(s: LayoutSection, frame: Frame, drag: boolean) {
  onSection(s.key, (_, own) => (own.frame = clampFrame(frame)), drag ? `win:${s.key}` : undefined);
}
function onWindowStart() {
  frozen.value = shownSections.value.map((s) => s.key);
  moving.value = true;
}
function onWindowEnd(s: LayoutSection, text: string | null) {
  frozen.value = null;
  moving.value = false;
  props.editor.endGesture();
  if (text) say(text);
  focusGripIfFocused(s);
}
/** A key on a window's grip re-renders the tab in reading order: the grip keeps the focus. */
function focusGripIfFocused(s: LayoutSection) {
  const id = `le-wgrip-${s.key}`;
  if (document.activeElement?.id === id) focus(id);
}
/** Pressing on a window selects it and brings it to the front (the same undo step as the drag that may follow). */
function onSelect(s: LayoutSection) {
  props.editor.selected = s.key;
  const tab = activeTab.value;
  if (!tab) return;
  const at = layerOf(tab, s);
  if (at.index < at.count) props.editor.layer("front", s.key);
  props.editor.endGesture();
}
function onLayer(s: LayoutSection, move: LayerMove) {
  props.editor.selected = s.key;
  props.editor.layer(move, s.key);
  focusGripIfFocused(s);
}

// ---------- Drag and drop ----------

const dragField = ref<string | null>(null);
/** The separator being dragged (it moves within and between field sections only). */
const dragSep = ref<SeparatorPlace | null>(null);
/** Where a drop lands: before the entry shown at `index` of the section (its entries' count: at the end). */
const dropAt = ref<{ section: string; index: number } | null>(null);
const dropTab = ref<string | null>(null);
function onDragStart(field: string, e: DragEvent) {
  dragField.value = field;
  if (e.dataTransfer) {
    e.dataTransfer.effectAllowed = "move";
    e.dataTransfer.setData("text/plain", field);
  }
}
function onSeparatorDragStart(at: SeparatorPlace, e: DragEvent) {
  dragSep.value = at;
  if (e.dataTransfer) {
    e.dataTransfer.effectAllowed = "move";
    e.dataTransfer.setData("text/plain", "separator");
  }
}
function onDragEnd() {
  dragField.value = null;
  dragSep.value = null;
  dropAt.value = null;
  dropTab.value = null;
}
/** Where in a section's grid a drop lands: before the first entry the pointer is above or left of, else at the end. */
function onGridOver(section: LayoutSection, e: DragEvent) {
  if (!dragField.value && !dragSep.value) return;
  e.preventDefault();
  if (e.dataTransfer) e.dataTransfer.dropEffect = "move";
  const cells = [...(e.currentTarget as HTMLElement).querySelectorAll<HTMLElement>(":scope > .le-field, :scope > .le-sep")];
  let index = cells.length;
  for (let i = 0; i < cells.length; i++) {
    const r = cells[i].getBoundingClientRect();
    // A separator spans the row: the drop goes before it in its upper half.
    const before = cells[i].classList.contains("le-sep") ? e.clientY < r.top + r.height / 2 : e.clientY < r.top || (e.clientY <= r.bottom && e.clientX < r.left + r.width / 2);
    if (before) {
      index = i;
      break;
    }
  }
  dropAt.value = { section: section.key, index };
}
function onGridDrop(section: LayoutSection, e: DragEvent) {
  e.preventDefault();
  const field = dragField.value;
  const sep = dragSep.value;
  const at = dropAt.value;
  onDragEnd();
  // The shown index, as an index among the section's stored entries (some may not be shown).
  const entries = entriesOf(section);
  const index = at?.section === section.key ? (entries[at.index]?.at ?? section.fields?.length ?? 0) : undefined;
  if (field) place(field, section.key, index);
  else if (sep) {
    let to: SeparatorPlace | undefined;
    props.editor.apply((l) => (to = placeSeparator(l, sep, section.key, index)));
    if (!to) return;
    say(t("layoutEditor.say.separatorDropped", { pos: to.index + 1, section: section.label }));
    focus(sepId(to));
  }
}
function onTabOver(tb: LayoutTab, e: DragEvent) {
  if (!dragField.value) return;
  e.preventDefault();
  dropTab.value = tb.key;
}
/** Dropping on a tab puts the field at the end of the tab's last section (a tab without one gets one). */
function onTabDrop(tb: LayoutTab, e: DragEvent) {
  e.preventDefault();
  const field = dragField.value;
  onDragEnd();
  if (!field) return;
  props.editor.apply((l) => {
    const own = l.tabs?.find((x) => x.key === tb.key);
    if (!own) return;
    placeField(l, field, lastFieldSection(l, own).key);
  });
  activeTabKey.value = tb.key;
  const at = layout.value && locate(layout.value, field);
  say(t("layoutEditor.say.placed", { field: labelOf(field), tab: tb.label, section: at?.section.label }));
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
      :aria-label="t('layoutEditor.hiddenFields')"
      @dragover="onHiddenOver"
      @drop="onHiddenDrop"
    >
      <strong>{{ t("layoutEditor.hiddenFields") }}</strong>
      <ul v-if="hidden.length > 0" :aria-label="t('layoutEditor.hiddenFields')">
        <li v-for="f in hidden" :key="f" draggable="true" @dragstart="onDragStart(f, $event)" @dragend="onDragEnd">
          {{ labelOf(f) }}<span v-if="defFor(f)?.isRequired" class="badge">{{ t("layoutEditor.required") }}</span>
          <button type="button" class="btn btn-sm" :aria-label="t('layoutEditor.showField', { field: labelOf(f) })" @click="show(f)">{{ t("layoutEditor.show") }}</button>
        </li>
      </ul>
      <span v-else class="muted">{{ t("layoutEditor.hiddenNone") }}</span>
      <span class="hint">{{ t("layoutEditor.hiddenHint") }}</span>
      <span class="hint" data-testid="le-presentation-only">{{ presentationOnly() }}</span>
    </div>

    <div class="le-frame" data-testid="le-frame">
      <div class="layout-container">
        <div class="le-tabs" role="group" :aria-label="t('layoutEditor.tabs')">
          <div v-for="tb in tabs" :key="tb.key" :class="['le-tab', { current: tb === activeTab, 'drop-target': dropTab === tb.key }]" @dragover="onTabOver(tb, $event)" @dragleave="dropTab = null" @drop="onTabDrop(tb, $event)">
            <input
              v-if="renaming?.kind === 'tab' && renaming.key === tb.key"
              id="le-rename"
              v-model="renameValue"
              class="le-rename"
              type="text"
              maxlength="100"
              :aria-label="t('layoutEditor.tabName')"
              @keydown="onRenameKey"
              @blur="commitRename"
            />
            <button v-else :id="`le-tab-${tb.key}`" type="button" class="le-tab-label" :aria-pressed="tb === activeTab" :title="tb === activeTab ? t('layoutEditor.clickToRename') : undefined" @click="onTabClick(tb)">
              {{ tb.label }}
            </button>
            <span v-if="tb === activeTab && !(renaming?.kind === 'tab' && renaming.key === tb.key)" class="le-tab-tools" role="toolbar" :aria-label="t('layoutEditor.tabTools', { tab: tb.label })">
              <button type="button" class="btn btn-sm btn-icon" :aria-label="t('layoutEditor.renameTab', { tab: tb.label })" :title="t('layoutEditor.rename')" @click="startRename('tab', tb.key, tb.label)"><Icon name="pencil" /></button>
              <button type="button" class="btn btn-sm btn-icon" :aria-label="t('layoutEditor.moveTabLeft', { tab: tb.label })" :title="t('layoutEditor.moveLeft')" :disabled="tabs.indexOf(tb) === 0" @click="onMoveTab(tb, -1)"><Icon name="arrow-left" /></button>
              <button type="button" class="btn btn-sm btn-icon" :aria-label="t('layoutEditor.moveTabRight', { tab: tb.label })" :title="t('layoutEditor.moveRight')" :disabled="tabs.indexOf(tb) === tabs.length - 1" @click="onMoveTab(tb, 1)"><Icon name="arrow-right" /></button>
              <button type="button" class="btn btn-sm btn-icon" :aria-label="t('layoutEditor.removeTab', { tab: tb.label })" :title="t('layoutEditor.remove')" :disabled="!layout || !canRemoveTab(layout, tb)" @click="confirmRemove = { tab: tb }"><Icon name="x" /></button>
            </span>
          </div>
          <button type="button" class="btn btn-sm le-add" :aria-label="t('layoutEditor.addTab')" @click="onAddTab"><Icon name="plus" />{{ t("layoutEditor.addTabShort") }}</button>
        </div>

        <div
          ref="areaEl"
          :class="['le-free', { stacked, 'le-guides': moving && editor.snap }]"
          :style="stacked ? undefined : { '--free-h': `${freeHeight}px`, '--guide': `${GUIDE_PX}px` }"
          role="region"
          :aria-label="t('layoutEditor.tab', { tab: activeTab?.label ?? '' })"
          data-le-area
        >
          <FreeWindow
            v-for="s in shownSections"
            :key="s.key"
            :section="s"
            :frame="s.frame!"
            :area-width="areaWidth"
            :others="othersOf(s)"
            :layer="layerAt(s)"
            :selected="editor.selected === s.key"
            :snap="editor.snap"
            :stacked="stacked"
            id-prefix="le"
            keys-id="le-keys"
            @frame="(f: Frame, drag: boolean) => onFrame(s, f, drag)"
            @gesture-start="onWindowStart"
            @gesture-end="(text: string | null) => onWindowEnd(s, text)"
            @select="onSelect(s)"
            @layer="(m: LayerMove) => onLayer(s, m)"
            @guides="(lines: SnapLine[]) => (guides = lines)"
          >
            <section :class="['panel', 'layout-panel', 'le-section', { 'le-block': kindOf(s) !== 'fields', invalid: errorsOf(s).length > 0 }]" :aria-label="t('layoutEditor.section', { section: s.label })">
              <div class="panel-header">
                <h2>
                  <input
                    v-if="renaming?.kind === 'section' && renaming.key === s.key"
                    id="le-rename"
                    v-model="renameValue"
                    class="le-rename"
                    type="text"
                    maxlength="100"
                    :aria-label="t('layoutEditor.sectionName')"
                    @keydown="onRenameKey"
                    @blur="commitRename"
                  />
                  <button v-else :id="`le-section-${s.key}`" type="button" class="le-section-label" :title="t('layoutEditor.clickToRename')" @click="startRename('section', s.key, s.label)">{{ s.label }}</button>
                </h2>
                <span v-if="kindOf(s) === 'note'" class="muted"><span class="badge">{{ t("layoutEditor.addNote") }}</span><template v-if="s.collapsed"> · {{ t("layoutEditor.startsCollapsed") }}</template></span>
                <span v-else-if="isPanelKind(kindOf(s))" class="muted"><span class="badge">{{ t("layoutEditor.panelBadge", { panel: panelLabel(kindOf(s) as PanelKind) }) }}</span><template v-if="s.collapsed"> · {{ t("layoutEditor.startsCollapsed") }}</template></span>
                <span v-else class="muted" data-testid="le-section-size">{{ t("layoutEditor.columns", { n: s.columns ?? 3 }) }}<template v-if="s.collapsed"> · {{ t("layoutEditor.startsCollapsed") }}</template></span>
                <span class="le-section-tools" role="toolbar" :aria-label="t('layoutEditor.sectionTools', { section: s.label })">
                  <button
                    v-for="m in LAYER_MOVES"
                    :key="m.move"
                    type="button"
                    class="btn btn-sm"
                    :aria-label="`${t(`layoutEditor.layer.${m.move}`)}: ${s.label}`"
                    :title="t('layoutEditor.layerTitle', { label: t(`layoutEditor.layer.${m.move}`), keys: m.keys })"
                    :disabled="m.move === 'front' || m.move === 'forward' ? layerAt(s).index >= layerAt(s).count : layerAt(s).index <= 1"
                    @click="onLayer(s, m.move)"
                  >
                    <Icon :name="LAYER_ICONS[m.move]" />
                  </button>
                  <button type="button" class="btn btn-sm" :aria-pressed="!!s.collapsed" :aria-label="t('layoutEditor.collapsedLabel', { section: s.label })" @click="onSection(s.key, (_, own) => (own.collapsed = !own.collapsed))">
                    {{ t("layoutEditor.collapsed") }}
                  </button>
                  <button v-if="kindOf(s) === 'note'" type="button" class="btn btn-sm" :aria-label="t('layoutEditor.editTextOf', { section: s.label })" @click="startNote(s)">{{ t("layoutEditor.editText") }}</button>
                  <button v-if="kindOf(s) === 'fields'" type="button" class="btn btn-sm" :aria-label="t('layoutEditor.addSeparatorTo', { section: s.label })" :title="t('layoutEditor.separatorTitle')" @click="insertSeparator(s)">
                    <Icon name="plus" />{{ t("layoutEditor.separator") }}
                  </button>
                  <select v-if="kindOf(s) === 'fields'" :aria-label="t('layoutEditor.columnsOf', { section: s.label })" :title="t('layoutEditor.columnsTitle')" :value="s.columns ?? 3" @change="onSection(s.key, (_, own) => setColumns(own, Number(($event.target as HTMLSelectElement).value)))">
                    <option v-for="n in MAX_COLUMNS" :key="n" :value="n">{{ t("layoutEditor.columns", { n }) }}</option>
                  </select>
                  <select v-if="tabs.length > 1" :aria-label="t('layoutEditor.tabOf', { section: s.label })" :value="activeTab?.key" @change="onSectionTab(s, ($event.target as HTMLSelectElement).value)">
                    <option v-for="tb in tabs" :key="tb.key" :value="tb.key">{{ tb.label }}</option>
                  </select>
                  <button type="button" class="btn btn-sm" :aria-label="t('layoutEditor.removeSection', { section: s.label })" :disabled="!layout || !canRemoveSection(layout, s)" @click="confirmRemove = { section: s }">{{ t("layoutEditor.remove") }}</button>
                </span>
              </div>
              <ul v-if="errorsOf(s).length > 0" class="le-errors" role="alert" :aria-label="t('layoutEditor.errorsIn', { section: s.label })">
                <li v-for="(e, k) in errorsOf(s)" :key="k"><code>{{ e.path }}</code> {{ e.message }}</li>
              </ul>
              <div v-if="kindOf(s) === 'note'" class="panel-body">
                <div v-if="noteKey === s.key" class="le-note-edit">
                  <label :for="`le-note-${s.key}`" class="sr-only">{{ t("layoutEditor.textOf", { section: s.label }) }}</label>
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
                    <strong v-if="noteBlank" class="le-note-error">{{ t("layoutEditor.noteBlank") }}</strong>
                    {{ t("layoutEditor.noteHelp") }}
                    {{ t("layoutEditor.noteCount", { n: noteDraft.length, max: NOTE_MAX_CHARS }) }}
                  </div>
                  <span class="row-actions">
                    <button type="button" class="btn btn-sm btn-primary" :disabled="noteBlank" @click="commitNote">{{ t("layoutEditor.apply") }}</button>
                    <button type="button" class="btn btn-sm" @click="cancelNote">{{ t("common.cancel") }}</button>
                  </span>
                </div>
                <button v-else type="button" class="le-note" :aria-label="t('layoutEditor.editTextOf', { section: s.label })" :title="t('layoutEditor.clickToEditText')" @click="startNote(s)">
                  <NoteText :text="s.text ?? ''" />
                </button>
              </div>
              <div v-else-if="isPanelKind(kindOf(s))" class="panel-body flush" inert>
                <slot name="panel" :kind="kindOf(s) as PanelKind">
                  <p class="hint le-panel-hint">{{ t("layoutEditor.panelHint", { hint: PANELS.find((p) => p.kind === kindOf(s))?.hint }) }}</p>
                </slot>
              </div>
              <div v-else class="panel-body">
                <div
                  :class="[gridClass(s.columns ?? 3), 'le-grid', { 'drop-end': dropAt?.section === s.key && dropAt.index === entriesOf(s).length }]"
                  :data-section="s.key"
                  @dragover="onGridOver(s, $event)"
                  @drop="onGridDrop(s, $event)"
                >
                  <template v-for="(e, i) in entriesOf(s)" :key="e.sep ? `sep-${e.at}` : e.f.field">
                    <div
                      v-if="e.sep"
                      :class="['le-sep', { 'drop-before': dropAt?.section === s.key && dropAt.index === i && !(dragSep?.section === s.key && dragSep.index === e.at), dragging: dragSep?.section === s.key && dragSep.index === e.at }]"
                      data-separator
                      draggable="true"
                      @dragstart="onSeparatorDragStart({ section: s.key, index: e.at }, $event)"
                      @dragend="onDragEnd"
                    >
                      <button
                        :id="sepId({ section: s.key, index: e.at })"
                        type="button"
                        class="le-grip"
                        :aria-label="t('layoutEditor.separatorGrip', { separator: sepName(e.f), section: s.label })"
                        aria-describedby="le-keys"
                        :title="t('layoutEditor.dragToMove')"
                        @keydown="onSeparatorKey({ section: s.key, index: e.at }, e.f, $event)"
                      >
                        <Icon name="grip-vertical" />
                      </button>
                      <input
                        v-if="sepEdit?.section === s.key && sepEdit.index === e.at"
                        id="le-sep-label"
                        v-model="sepLabel"
                        class="le-rename"
                        type="text"
                        :maxlength="SEPARATOR_MAX_CHARS"
                        :aria-label="t('layoutEditor.separatorLabel')"
                        :placeholder="t('layoutEditor.noLabel')"
                        @keydown="onSeparatorLabelKey"
                        @blur="commitSeparatorLabel"
                      />
                      <button v-else type="button" class="le-sep-label" :title="t('layoutEditor.clickToEditLabel')" @click="startSeparatorLabel({ section: s.key, index: e.at }, e.f.label ?? '')">
                        <span v-if="e.f.label" dir="auto">{{ e.f.label }}</span><span v-else class="muted">{{ t("layoutEditor.separator") }}</span>
                      </button>
                      <span class="le-sep-line" aria-hidden="true" />
                      <span class="le-toolbar" role="toolbar" :aria-label="t('layoutEditor.separatorTools', { separator: sepName(e.f) })">
                        <button type="button" class="btn btn-sm btn-icon" :aria-label="t('layoutEditor.moveSeparatorEarlier', { separator: sepName(e.f) })" :title="t('layoutEditor.moveEarlier')" @click="moveSeparator({ section: s.key, index: e.at }, -1)"><Icon name="arrow-up" /></button>
                        <button type="button" class="btn btn-sm btn-icon" :aria-label="t('layoutEditor.moveSeparatorLater', { separator: sepName(e.f) })" :title="t('layoutEditor.moveLater')" @click="moveSeparator({ section: s.key, index: e.at }, 1)"><Icon name="arrow-down" /></button>
                        <button type="button" class="btn btn-sm btn-icon" :aria-label="t('layoutEditor.editLabelOf', { separator: sepName(e.f) })" :title="t('layoutEditor.editLabel')" @click="startSeparatorLabel({ section: s.key, index: e.at }, e.f.label ?? '')"><Icon name="pencil" /></button>
                        <button type="button" class="btn btn-sm" :aria-label="t('layoutEditor.removeSeparator', { separator: sepName(e.f) })" @click="dropSeparator({ section: s.key, index: e.at })">{{ t("layoutEditor.remove") }}</button>
                      </span>
                    </div>
                    <EditableField
                      v-else
                      :field="e.f.field"
                      :label="labelOf(e.f.field)"
                      :width="e.f.width ?? 1"
                      :columns="s.columns ?? 3"
                      :core="isCore(e.f.field)"
                      :required="defFor(e.f.field)?.isRequired"
                      :read-only="isReadOnly(e.f.field)"
                      :can-read-only="form"
                      :show-label="!form"
                      :section="s.key"
                      :sections="sectionOptions"
                      :drop-before="dropAt?.section === s.key && dropAt.index === i && dragField !== e.f.field"
                      :dragging="dragField === e.f.field"
                      @move="(d) => moveField(e.f.field, d)"
                      @resize="(w, drag) => resizeField(e.f.field, w, drag)"
                      @resize-end="editor.endGesture()"
                      @hide="hide(e.f.field)"
                      @place="(k) => place(e.f.field, k)"
                      @read-only="(on) => setReadOnly(e.f.field, on)"
                      @dragstart="(ev) => onDragStart(e.f.field, ev)"
                      @dragend="onDragEnd"
                    >
                      <slot name="field" :field="e.f.field" />
                    </EditableField>
                  </template>
                  <p v-if="!hasFields(s)" :class="['le-empty', 'lg-cell', `lg-w-${s.columns ?? 3}`]">{{ t("layoutEditor.dropFields") }}</p>
                </div>
              </div>
            </section>
          </FreeWindow>
          <span v-for="(g, k) in stacked ? [] : guides" :key="k" :class="['le-snap', g.axis]" :style="g.axis === 'x' ? { left: `${g.at}px` } : { top: `${g.at}px` }" aria-hidden="true" />
          <!-- Below the lowest window. -->
          <div class="layout-panels le-tail">
            <div class="le-insert">
              <button type="button" class="btn btn-sm le-add" :aria-label="t('layoutEditor.addSectionTo', { tab: activeTab?.label })" @click="insertSection(activeTab?.sections?.length ?? 0)"><Icon name="plus" />{{ t("layoutEditor.addSection") }}</button>
              <button type="button" class="btn btn-sm le-add" :aria-label="t('layoutEditor.addNoteTo', { tab: activeTab?.label })" @click="insertNote(activeTab?.sections?.length ?? 0)"><Icon name="plus" />{{ t("layoutEditor.addNote") }}</button>
              <select
                class="le-add-panel"
                :aria-label="t('layoutEditor.addPanelTo', { tab: activeTab?.label })"
                :disabled="freePanels.length === 0"
                :title="freePanels.length === 0 ? t('layoutEditor.allPanelsPlaced') : undefined"
                @change="insertPanel(activeTab?.sections?.length ?? 0, $event)"
              >
                <option value="">{{ t("layoutEditor.addPanel") }}</option>
                <option v-for="p in freePanels" :key="p.kind" :value="p.kind">{{ p.label }}</option>
              </select>
            </div>

            <template v-if="onFirstTab">
              <section v-for="a in autoSections" :key="a.key" class="panel layout-panel le-section auto" :aria-label="t('layoutEditor.notPlaced', { section: a.label })">
                <div class="panel-header">
                  <h2>{{ a.label }}</h2>
                  <span class="muted">{{ t("layoutEditor.notPlacedHint") }}</span>
                  <button type="button" class="btn btn-sm" @click="makeSection(a.label, a.fields.map((f) => f.field))">{{ t("layoutEditor.makeSection") }}</button>
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
            </template>
          </div>
        </div>
      </div>
    </div>
    <details class="le-keys">
      <summary><Icon name="info" />{{ t("layoutEditor.keys.title") }}</summary>
      <ul id="le-keys">
        <li v-for="k in KEY_HELP" :key="k">{{ t(k) }}</li>
      </ul>
    </details>
    <div class="sr-only" aria-live="assertive">{{ editor.announcement }}</div>
  </div>

  <ConfirmDialog :open="!!confirmRemove" :title="removeTitle" :confirm-label="t('layoutEditor.remove')" @confirm="doRemove" @cancel="confirmRemove = null">
    {{ removeText }} {{ t("layoutEditor.removeNotSaved") }}
  </ConfirmDialog>
</template>

<style scoped>
/* A separator in the editor: grip, label, the line, and a toolbar as on a field. */
.le-sep {
  position: relative;
  grid-column: 1 / -1;
  display: flex;
  align-items: center;
  gap: var(--space-1);
  padding: var(--space-0_5) var(--space-1);
  border: 1px solid var(--c-border);
  border-radius: var(--radius-sm);
  background: var(--c-surface);
  cursor: grab;
}
.le-sep:hover,
.le-sep:focus-within {
  border-color: var(--c-primary);
}
.le-sep.dragging {
  opacity: 0.45;
}
.le-sep.drop-before::before {
  content: "";
  position: absolute;
  left: 0;
  right: 0;
  top: -9px;
  height: 3px;
  border-radius: var(--radius-xs);
  background: var(--c-primary);
}
.le-sep-label {
  border: 0;
  background: none;
  font: inherit;
  font-size: var(--fs-sm);
  font-weight: var(--fw-semibold);
  color: var(--c-text-secondary);
  cursor: text;
  padding: 0;
  white-space: nowrap;
}
.le-sep-label:focus-visible {
  outline: 2px solid var(--c-focus);
}
.le-sep-line {
  flex: 1;
  border-top: 1px solid var(--c-border-strong);
}
.le-sep .le-grip {
  border: 0;
  background: none;
  padding: 0 var(--space-0_5);
  color: var(--c-text-secondary);
  cursor: grab;
}
.le-sep .le-grip:focus-visible {
  outline: 2px solid var(--c-focus);
  outline-offset: 1px;
}
.le-sep > .le-toolbar {
  position: absolute;
  right: 4px;
  bottom: 100%;
  z-index: var(--z-handle);
  display: flex;
  align-items: center;
  gap: var(--space-0_5);
  padding: var(--space-0_5);
  border: 1px solid var(--c-primary);
  border-radius: var(--radius-sm);
  background: var(--c-surface);
  box-shadow: var(--shadow-md);
  opacity: 0;
  pointer-events: none;
  white-space: nowrap;
}
.le-sep:hover > .le-toolbar,
.le-sep:focus-within > .le-toolbar {
  opacity: 1;
  pointer-events: auto;
}
/* A free tab: the windows sit in the room above its tail (+ Section, the unplaced fields). */
.le-free {
  position: relative;
  padding-top: var(--free-h);
}
.le-free.stacked {
  padding-top: 0;
  display: flex;
  flex-direction: column;
  gap: var(--space-3);
}
.le-free:not(.stacked) {
  background-image: linear-gradient(to bottom, transparent calc(var(--free-h) - 1px), var(--c-border) calc(var(--free-h) - 1px), var(--c-border) var(--free-h), transparent var(--free-h));
}
/* The fine guide grid, while a window moves with snapping on. */
.le-free.le-guides {
  background-image:
    linear-gradient(to right, var(--c-row-hover) 1px, transparent 1px),
    linear-gradient(to bottom, var(--c-row-hover) 1px, transparent 1px);
  background-size: var(--guide) var(--guide);
}
.le-free > .le-tail {
  margin-top: var(--space-3);
}
/* A line a moving window's edge snapped to. */
.le-snap {
  position: absolute;
  z-index: var(--z-snap);
  background: var(--c-primary);
  pointer-events: none;
}
.le-snap.x {
  top: 0;
  width: 1px;
  height: var(--free-h);
}
.le-snap.y {
  left: 0;
  right: 0;
  height: 1px;
}
.le-canvas {
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
}
/* One border language (audit A10): solid for what the layout places, dashed only for what it does not
   (unplaced fields and sections, empty drop areas), a solid primary outline for a drop target. */
.le-frame {
  padding: var(--space-2);
  border: 1px solid var(--c-border);
  border-radius: var(--radius-lg);
  background: var(--c-bg);
}
.le-hidden {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-1) var(--space-2);
  padding: var(--space-1) var(--space-2);
  border: 1px dashed var(--c-border-strong);
  border-radius: var(--radius-sm);
  background: var(--c-surface-alt);
}
.le-hidden.drop-target {
  outline: 2px solid var(--c-primary);
}
.le-hidden ul {
  display: contents;
  list-style: none;
}
.le-hidden li {
  display: inline-flex;
  align-items: center;
  gap: var(--space-0_5);
  padding: var(--space-0_5) var(--space-0_5) var(--space-0_5) var(--space-1);
  border: 1px solid var(--c-border);
  border-radius: var(--radius-sm);
  background: var(--c-surface);
  cursor: grab;
}
.le-tabs {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-0_5);
  border-bottom: 1px solid var(--c-border);
  margin-bottom: var(--space-3);
}
.le-tab {
  display: flex;
  align-items: center;
  gap: var(--space-0_5);
  border-bottom: 2px solid transparent;
  margin-bottom: -1px;
}
.le-tab.current {
  border-bottom-color: var(--c-primary);
}
.le-tab.drop-target {
  background: var(--c-row-hover);
  outline: 2px solid var(--c-primary);
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
  padding: var(--space-2) var(--space-2);
  font-weight: var(--fw-semibold);
  color: var(--c-text-secondary);
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
  border-bottom-color: var(--c-text-secondary);
}
.le-tab-label:focus-visible,
.le-section-label:focus-visible {
  outline: 2px solid var(--c-focus);
}
.le-rename {
  font: inherit;
  font-weight: var(--fw-semibold);
  min-width: 160px;
  margin: var(--space-0_5) 0;
}
.le-add {
  color: var(--c-primary);
}
.le-add-panel {
  width: auto;
  height: var(--control-h-sm);
  font-size: var(--fs-sm);
}
.le-tabs > .le-add {
  margin-left: var(--space-1);
}
.le-tab-tools,
.le-section-tools {
  display: inline-flex;
  align-items: center;
  gap: var(--space-0_5);
}
.le-section .panel-header {
  gap: var(--space-2);
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
  padding: var(--space-px) var(--space-1);
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
  margin: calc(-1 * var(--space-1)) 0;
}
.le-section-tools .btn[aria-pressed="true"] {
  border-color: var(--c-primary);
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
  padding: var(--space-0_5);
  row-gap: var(--space-4);
  padding-top: var(--space-3);
}
.le-grid.drop-end {
  outline: 2px solid var(--c-primary);
  outline-offset: 2px;
}
.le-empty {
  margin: 0;
  padding: var(--space-2);
  border: 1px dashed var(--c-border-strong);
  border-radius: var(--radius-sm);
  color: var(--c-text-secondary);
  text-align: center;
}
.le-keys {
  font-size: var(--fs-sm);
  color: var(--c-text-secondary);
}
.le-keys summary {
  display: inline-flex;
  align-items: center;
  gap: var(--space-1);
  cursor: pointer;
}
.le-keys ul {
  margin: var(--space-1) 0 0;
  padding-left: var(--space-4);
  max-width: 90ch;
}
.le-insert {
  gap: var(--space-1);
}
.le-insert select.le-add-panel {
  opacity: 0.55;
}
.le-insert select.le-add-panel:hover,
.le-insert select.le-add-panel:focus-visible {
  opacity: 1;
}
.le-section.invalid {
  border-color: var(--c-danger-text);
}
.le-errors {
  margin: 0;
  padding: var(--space-1) var(--space-2) var(--space-1) var(--space-6);
  color: var(--c-danger-text);
  background: var(--c-danger-subtle);
  font-size: var(--fs-sm);
}
.le-note {
  display: block;
  width: 100%;
  border: 1px dashed transparent;
  border-radius: var(--radius-sm);
  background: none;
  font: inherit;
  color: inherit;
  text-align: left;
  padding: var(--space-0_5);
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
  gap: var(--space-1);
}
.le-note-edit textarea {
  width: 100%;
  font: inherit;
  resize: vertical;
}
.le-note-error {
  color: var(--c-danger-text);
}
.le-panel-hint {
  margin: 0;
  padding: var(--space-2);
}
</style>
