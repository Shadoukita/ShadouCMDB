import type { UiClassLayout } from "../api/uiSettings";
import { suggestKey } from "./keys";
import { CORE_FIELDS, GRID_COLUMNS, MAX_COLUMNS, SECTION_GRID, panelLabel, placedPanels, resolveLayout, sectionKind, type AttributeLike, type PanelKind } from "./uiSettings";

/**
 * The form designer's edits (Customization › Detail and form layout) on a class
 * layout in format v2: tabs → sections → fields with a width. Every edit
 * changes the layout in place (it is part of the reactive settings draft) and
 * keeps the rules the API checks: tab keys unique, section keys unique across
 * the layout, a field placed once, widths within the section's columns, and the
 * core fields (ident, valid from, valid until) never hidden or dropped.
 * Content blocks (notes and the built-in panels) are sections without fields:
 * fields never go into one, and each panel is placed at most once.
 */

export type LayoutTab = NonNullable<UiClassLayout["tabs"]>[number];
export type LayoutSection = NonNullable<LayoutTab["sections"]>[number];
export type LayoutField = NonNullable<LayoutSection["fields"]>[number];

export const isCore = (field: string) => CORE_FIELDS.includes(field);

export interface FieldPlace {
  tab: LayoutTab;
  section: LayoutSection;
  index: number;
}

const tabsOf = (l: UiClassLayout) => (l.tabs ??= []);
const sectionsOf = (t: LayoutTab) => (t.sections ??= []);
/** Whether a section is a grid of fields (not a note or a panel). */
export const isFieldSection = (s: LayoutSection) => sectionKind(s) === "fields";
/** A section's fields; a content block has none (and gets no `fields` written). */
const fieldsOf = (s: LayoutSection) => (isFieldSection(s) ? (s.fields ??= []) : []);
const columnsOf = (s: LayoutSection) => s.columns ?? GRID_COLUMNS;

export function allSections(l: UiClassLayout): { tab: LayoutTab; section: LayoutSection }[] {
  return tabsOf(l).flatMap((tab) => sectionsOf(tab).map((section) => ({ tab, section })));
}

/** The sections that hold fields, in reading order. */
export const fieldSections = (l: UiClassLayout) => allSections(l).filter((x) => isFieldSection(x.section));

export function findSection(l: UiClassLayout, key: string): { tab: LayoutTab; section: LayoutSection } | undefined {
  return allSections(l).find((x) => x.section.key === key);
}

export function locate(l: UiClassLayout, field: string): FieldPlace | undefined {
  for (const { tab, section } of allSections(l)) {
    const index = fieldsOf(section).findIndex((f) => f.field === field);
    if (index >= 0) return { tab, section, index };
  }
  return undefined;
}

/** A key from `label` that none of `taken` has. */
export function uniqueKey(label: string, taken: Iterable<string>, fallback: string): string {
  const used = new Set(taken);
  const base = (suggestKey(label) || fallback).slice(0, 56);
  let key = base;
  for (let n = 2; used.has(key); n++) key = `${base}_${n}`;
  return key;
}

/**
 * Puts `field` into section `sectionKey` at `index` (the end when omitted),
 * taking it from wherever it was and out of the hidden fields. Its width is kept
 * where the new section's grid allows it.
 */
export function placeField(l: UiClassLayout, field: string, sectionKey: string, index?: number): void {
  const target = findSection(l, sectionKey)?.section;
  if (!target || !isFieldSection(target)) return;
  const from = locate(l, field);
  let width = 1;
  let at = index ?? fieldsOf(target).length;
  if (from) {
    width = fieldsOf(from.section)[from.index].width ?? 1;
    fieldsOf(from.section).splice(from.index, 1);
    if (from.section === target && from.index < at) at -= 1;
  }
  l.hiddenFields = (l.hiddenFields ?? []).filter((f) => f !== field);
  const list = fieldsOf(target);
  list.splice(Math.max(0, Math.min(at, list.length)), 0, { field, width: Math.min(width, columnsOf(target)) });
}

/** Hides `field` from the detail page and the form. Core fields cannot be hidden. */
export function hideField(l: UiClassLayout, field: string): boolean {
  if (isCore(field)) return false;
  const from = locate(l, field);
  if (from) fieldsOf(from.section).splice(from.index, 1);
  if (!(l.hiddenFields ?? []).includes(field)) l.hiddenFields = [...(l.hiddenFields ?? []), field];
  return true;
}

/**
 * Moves `field` one place earlier (-1) or later (+1) in reading order: within
 * its section, or across into the neighbouring section (and tab) at the edges.
 * Returns the section it ends up in, or undefined if it could not move.
 */
export function moveFieldBy(l: UiClassLayout, field: string, delta: -1 | 1): LayoutSection | undefined {
  const from = locate(l, field);
  if (!from) return undefined;
  const list = fieldsOf(from.section);
  const to = from.index + delta;
  if (to >= 0 && to < list.length) {
    const [f] = list.splice(from.index, 1);
    list.splice(to, 0, f);
    return from.section;
  }
  const sections = fieldSections(l).map((x) => x.section);
  const next = sections[sections.indexOf(from.section) + delta];
  if (!next) return undefined;
  placeField(l, field, next.key, delta < 0 ? fieldsOf(next).length : 0);
  return next;
}

export function setWidth(l: UiClassLayout, field: string, width: number): number {
  const at = locate(l, field);
  if (!at) return 1;
  const f = fieldsOf(at.section)[at.index];
  f.width = Math.max(1, Math.min(Math.round(width), columnsOf(at.section)));
  return f.width;
}

/** Changes a section's grid; fields wider than the new grid get narrower. */
export function setColumns(section: LayoutSection, columns: number): void {
  section.columns = Math.max(1, Math.min(columns, MAX_COLUMNS));
  for (const f of fieldsOf(section)) f.width = Math.min(f.width ?? 1, section.columns);
}

export function addTab(l: UiClassLayout, label: string): LayoutTab {
  const key = uniqueKey(label, tabsOf(l).map((t) => t.key), "tab");
  const tab: LayoutTab = { key, label, sections: [] };
  tabsOf(l).push(tab);
  addSection(l, tab, label);
  return tab;
}

/** Adds an empty section to `tab`, at `index` among its sections (the end when omitted). */
export function addSection(l: UiClassLayout, tab: LayoutTab, label: string, index?: number): LayoutSection {
  const key = uniqueKey(label, allSections(l).map((x) => x.section.key), "section");
  const section: LayoutSection = {
    key,
    label,
    columns: GRID_COLUMNS,
    width: SECTION_GRID,
    collapsed: false,
    fields: [],
  };
  const list = sectionsOf(tab);
  list.splice(Math.max(0, Math.min(index ?? list.length, list.length)), 0, section);
  return section;
}

/** Adds a note (static text) to `tab`, at `index` among its sections (the end when omitted). */
export function addNote(l: UiClassLayout, tab: LayoutTab, label: string, text: string, index?: number): LayoutSection {
  const key = uniqueKey(label, allSections(l).map((x) => x.section.key), "note");
  const section: LayoutSection = { key, label, kind: "note", text, columns: GRID_COLUMNS, width: SECTION_GRID, collapsed: false };
  const list = sectionsOf(tab);
  list.splice(Math.max(0, Math.min(index ?? list.length, list.length)), 0, section);
  return section;
}

/** Places a built-in panel in `tab`; undefined when the layout already places it. */
export function addPanel(l: UiClassLayout, tab: LayoutTab, kind: PanelKind, index?: number): LayoutSection | undefined {
  if (placedPanels(l).has(kind)) return undefined;
  const label = panelLabel(kind);
  const key = uniqueKey(label, allSections(l).map((x) => x.section.key), kind);
  const section: LayoutSection = { key, label, kind, columns: GRID_COLUMNS, width: SECTION_GRID, collapsed: false };
  const list = sectionsOf(tab);
  list.splice(Math.max(0, Math.min(index ?? list.length, list.length)), 0, section);
  return section;
}

/** The last section of `tab` that holds fields; a tab without one gets one, named after the tab. */
export function lastFieldSection(l: UiClassLayout, tab: LayoutTab): LayoutSection {
  const own = sectionsOf(tab).filter(isFieldSection);
  return own[own.length - 1] ?? addSection(l, tab, tab.label);
}

/** Where removing `tab` puts its fields: the last field section of the tab before it (or after it). */
export function tabFallback(l: UiClassLayout, tab: LayoutTab): LayoutSection | undefined {
  const tabs = tabsOf(l);
  const i = tabs.indexOf(tab);
  const others = [...tabs.slice(0, i).reverse(), ...tabs.slice(i + 1)];
  for (const t of others) {
    const s = sectionsOf(t).filter(isFieldSection);
    if (s.length > 0) return s[s.length - 1];
  }
  return undefined;
}

/** Where removing `section` puts its fields: the field section before it in the layout, else the one after it. */
export function sectionFallback(l: UiClassLayout, section: LayoutSection): LayoutSection | undefined {
  const sections = fieldSections(l).map((x) => x.section);
  const i = sections.indexOf(section);
  return i < 0 ? undefined : (sections[i - 1] ?? sections[i + 1]);
}

/**
 * Removes a tab; its fields move to the end of `tabFallback`. The last tab
 * holding sections cannot go (there would be nowhere for the core fields).
 */
export function removeTab(l: UiClassLayout, tab: LayoutTab): boolean {
  const into = tabFallback(l, tab);
  const fields = sectionsOf(tab).flatMap((s) => fieldsOf(s));
  if (!into && fields.length > 0) return false;
  if (into) fieldsOf(into).push(...fields.map((f) => ({ ...f, width: Math.min(f.width ?? 1, columnsOf(into)) })));
  l.tabs = tabsOf(l).filter((t) => t !== tab);
  return true;
}

/** Removes a section; its fields move to the end of `sectionFallback`. The only field section cannot go; a block always can. */
export function removeSection(l: UiClassLayout, section: LayoutSection): boolean {
  const into = sectionFallback(l, section);
  if (!into && isFieldSection(section)) return false;
  if (into) fieldsOf(into).push(...fieldsOf(section).map((f) => ({ ...f, width: Math.min(f.width ?? 1, columnsOf(into)) })));
  for (const t of tabsOf(l)) t.sections = sectionsOf(t).filter((s) => s !== section);
  return true;
}

/** Whether `tab` can be removed: not the only tab, and its fields have somewhere to go. */
export const canRemoveTab = (l: UiClassLayout, tab: LayoutTab) =>
  tabsOf(l).length > 1 && (!!tabFallback(l, tab) || sectionsOf(tab).every((s) => fieldsOf(s).length === 0));
/** Whether `section` can be removed: a note or a panel, or not the only field section of the layout. */
export const canRemoveSection = (l: UiClassLayout, section: LayoutSection) => !isFieldSection(section) || !!sectionFallback(l, section);

/** What removing a tab or a section does with its fields, for the confirmation. */
export function removalSummary(l: UiClassLayout, target: { tab: LayoutTab } | { section: LayoutSection }): string {
  if ("section" in target && !isFieldSection(target.section)) {
    const kind = sectionKind(target.section);
    return kind === "note"
      ? "The note and its text are removed."
      : `The ${panelLabel(kind as PanelKind)} panel is no longer placed by this layout: the detail page shows it at its usual position.`;
  }
  const noun = "tab" in target ? "tab" : "section";
  const fields = "tab" in target ? sectionsOf(target.tab).flatMap((s) => fieldsOf(s)) : fieldsOf(target.section);
  const into = "tab" in target ? tabFallback(l, target.tab) : sectionFallback(l, target.section);
  const n = fields.length;
  const moved = n === 0 ? `The ${noun} has no fields.` : `Its ${n} field${n === 1 ? "" : "s"} move to the end of the section ${into?.label}.`;
  const blocks = "tab" in target ? sectionsOf(target.tab).filter((s) => !isFieldSection(s)) : [];
  if (blocks.length === 0) return moved;
  return `${moved} ${blocks.length === 1 ? "Its note or panel" : `Its ${blocks.length} notes and panels`} (${blocks.map((b) => b.label).join(", ")}) ${blocks.length === 1 ? "is" : "are"} removed with it.`;
}

export function moveTab(l: UiClassLayout, tab: LayoutTab, delta: -1 | 1): void {
  const tabs = tabsOf(l);
  const i = tabs.indexOf(tab);
  const j = i + delta;
  if (i < 0 || j < 0 || j >= tabs.length) return;
  [tabs[i], tabs[j]] = [tabs[j], tabs[i]];
}

/** Moves a section up or down within its tab. */
export function moveSection(l: UiClassLayout, section: LayoutSection, delta: -1 | 1): void {
  const owner = allSections(l).find((x) => x.section === section)?.tab;
  if (!owner) return;
  const list = sectionsOf(owner);
  const i = list.indexOf(section);
  const j = i + delta;
  if (j < 0 || j >= list.length) return;
  [list[i], list[j]] = [list[j], list[i]];
}

/** Moves a section to the end of another tab. */
export function moveSectionToTab(l: UiClassLayout, section: LayoutSection, tab: LayoutTab): void {
  for (const t of tabsOf(l)) t.sections = sectionsOf(t).filter((s) => s !== section);
  sectionsOf(tab).push(section);
}

/**
 * The built-in layout of a class with everything placed explicitly: what
 * "Customize" starts from. The General tab with its General section and the
 * attribute groups becomes real tabs and sections the administrator can change.
 */
export function materialize(classKey: string, attrs: readonly AttributeLike[]): UiClassLayout {
  const l: UiClassLayout = { classKey, tabs: [], hiddenFields: [], readOnlyFields: [] };
  for (const t of resolveLayout(l, attrs, CORE_FIELDS)) {
    const tab: LayoutTab = { key: uniqueKey(t.label, tabsOf(l).map((x) => x.key), "tab"), label: t.label, sections: [] };
    tabsOf(l).push(tab);
    for (const s of t.sections) {
      const section = addSection(l, tab, s.label);
      section.fields = s.fields.map((f) => ({ field: f.field, width: f.width }));
    }
  }
  return l;
}

/**
 * Turns fields the layout leaves to the built-in placement (an automatic section
 * at the end of the first tab) into a real section there, named `label`.
 */
export function adoptFields(l: UiClassLayout, label: string, fields: readonly string[]): LayoutSection {
  if (tabsOf(l).length === 0) tabsOf(l).push({ key: "general", label: "General", sections: [] });
  const section = addSection(l, tabsOf(l)[0], label);
  for (const f of fields) placeField(l, f, section.key);
  return section;
}
