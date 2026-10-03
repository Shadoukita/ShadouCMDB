import type {
  UiClassLayout,
  UiListFilters,
  UiListSort,
  UiListView,
  UiNavEntry,
  UiPage,
  UiSettingsDocument,
  UiWidgetType,
} from "../api/uiSettings";
import type { components } from "../api/schema";
import { hasMessage, t } from "../i18n/index";
import { GENERAL_SECTION, groupAttributes } from "./attributes";
import { freeCopy, makeFree, readingOrder } from "./freeLayout";

type UiSectionFrame = components["schemas"]["UiSectionFrame"];

/**
 * The UI settings document (Administration › Customization) as the screens
 * apply it. Every section is optional; whatever is left out keeps the built-in
 * behaviour, so `{}` is the stock UI. Classes, attributes and lookups are named
 * by key, never by id.
 */

export const EMPTY_FILTERS: UiListFilters = { q: null, lookups: {} };

export function emptyDocument(): UiSettingsDocument {
  return {
    branding: { appName: null, primaryColor: null, accentColor: null, defaultTheme: "system" },
    navigation: { entries: [] },
    dashboard: { widgets: null },
    listViews: [],
    layouts: [],
    layoutTemplates: [],
  };
}

/** Fills every section the API may have left out, so editors can bind to it. */
export function normalizeDocument(doc: Partial<UiSettingsDocument> | undefined): UiSettingsDocument {
  const e = emptyDocument();
  const d = doc ?? {};
  return {
    branding: { ...e.branding, ...d.branding },
    navigation: { entries: d.navigation?.entries ?? [] },
    dashboard: {
      widgets:
        d.dashboard?.widgets?.map((w) =>
          w.type === "saved_search"
            ? { ...w, search: { classKeys: [], includeSubclasses: false, sort: null, ...w.search, filters: { ...EMPTY_FILTERS, lookups: {}, ...w.search?.filters } } }
            : w,
        ) ?? null,
    },
    listViews: (d.listViews ?? []).map((v) => ({ columns: [], defaultSort: null, pageSize: null, ...v, defaultFilters: { ...EMPTY_FILTERS, lookups: {}, ...v.defaultFilters } })),
    layouts: (d.layouts ?? []).map(normalizeLayout),
    // Kept as the API sent them (classes and CIs refer to them, and the API refuses to drop one in use), as a
    // copy: the editors change the draft, and the query cache's data is read-only.
    layoutTemplates: JSON.parse(JSON.stringify(d.layoutTemplates ?? [])) as UiSettingsDocument["layoutTemplates"],
  };
}

// ---------- Fields ----------

export interface BuiltinField {
  key: string;
  label: string;
  /** API sort field for list columns. */
  sort?: UiListSort["field"];
  /** The CI form's field for it; absent for fields the form does not edit (label, class, timestamps). */
  form?: string;
  /** A form field a layout may hide (criticality, which a CI need not have); the other form fields cannot be hidden. */
  hideable?: true;
}

/**
 * CI fields every class has, in their built-in order. Attributes are `attributes.<key>`;
 * name, status, hostname and the like are attributes of the classes that define them.
 */
export const BUILTIN_FIELDS: BuiltinField[] = [
  { key: "label", label: "Label", sort: "label" },
  { key: "ident", label: "Ident", sort: "ident", form: "ident" },
  { key: "class", label: "Class", sort: "className" },
  { key: "criticality", label: "Criticality", sort: "criticality", form: "criticalityValueId", hideable: true },
  { key: "validFrom", label: "Valid from", sort: "validFrom", form: "validFrom" },
  { key: "validUntil", label: "Valid until", sort: "validUntil", form: "validUntil" },
  { key: "active", label: "Active" },
  { key: "createdAt", label: "Created", sort: "createdAt" },
  { key: "updatedAt", label: "Updated", sort: "updatedAt" },
];
export const BUILTIN = new Map(BUILTIN_FIELDS.map((f) => [f.key, f]));

/** Built-in sort fields the list API accepts, with labels (attributes.<key> needs a class). */
export const SORT_FIELDS: { field: UiListSort["field"]; label: string }[] = BUILTIN_FIELDS.filter((f) => f.sort).map((f) => ({
  field: f.sort!,
  label: f.label,
}));

/** The inventory's columns when no list view says otherwise. */
export const DEFAULT_COLUMNS = ["label", "ident", "class", "active", "updatedAt"];

/**
 * The columns a list view shows. The label cell is the row's link to the CI and
 * the one value every CI has, so a view without it gets it as its first column:
 * otherwise its rows could not be opened and could all read "—".
 */
export function listColumns(columns: readonly string[] | null | undefined): string[] {
  if (!columns?.length) return [...DEFAULT_COLUMNS];
  return columns.includes("label") ? [...columns] : ["label", ...columns];
}

export const ATTRIBUTE_PREFIX = "attributes.";
export const attributeKey = (field: string) => (field.startsWith(ATTRIBUTE_PREFIX) ? field.slice(ATTRIBUTE_PREFIX.length) : null);

export interface AttributeLike {
  key: string;
  label: string;
  groupName: string | null;
  sortOrder: number;
}

/** An attribute as far as sorting is concerned: the list API sorts by any but a reference. */
export interface SortableAttribute {
  key: string;
  label: string;
  dataType: string;
  isActive: boolean;
}
export const isSortableAttribute = (a: SortableAttribute) => a.dataType !== "reference";

/** Sort choices for a class's active, sortable attributes (`attributes.<key>`). */
export function attributeSortFields(attrs: readonly SortableAttribute[]): { field: string; label: string }[] {
  return attrs.filter((a) => a.isActive && isSortableAttribute(a)).map((a) => ({ field: `${ATTRIBUTE_PREFIX}${a.key}`, label: `${a.label} (attribute)` }));
}

/**
 * The attributes every one of several classes can be sorted by: the same key and
 * data type on each (the first class's order and labels). Lookups sort by their
 * list's order, which only the API knows, so rows of several classes could not be
 * merged by them: they are left out when there is more than one class.
 */
export function sharedSortAttributes<T extends SortableAttribute>(perClass: readonly (readonly T[])[]): T[] {
  if (perClass.length === 0) return [];
  const [first, ...rest] = perClass;
  return first.filter(
    (a) =>
      a.isActive &&
      isSortableAttribute(a) &&
      (rest.length === 0 || a.dataType !== "lookup") &&
      rest.every((attrs) => attrs.some((b) => b.key === a.key && b.dataType === a.dataType && b.isActive)),
  );
}

const IPV4 = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})(?:\/(\d{1,2}))?$/;
const ipv4Number = (s: string) => {
  const m = IPV4.exec(s);
  // The address, then the prefix length (a bare address counts as /32).
  return m ? (((+m[1] * 256 + +m[2]) * 256 + +m[3]) * 256 + +m[4]) * 64 + (m[5] ? +m[5] : 32) : null;
};

/**
 * Orders two field values of rows merged from several list requests the way the
 * list API does: numbers by value, IPv4 addresses by address, text
 * case-insensitively (digits by number), and missing values last in either direction.
 */
export function compareValues(a: unknown, b: unknown, dir = 1): number {
  const none = (v: unknown) => v === null || v === undefined || v === "";
  if (none(a) || none(b)) return none(a) === none(b) ? 0 : none(a) ? 1 : -1;
  if (typeof a === "number" && typeof b === "number") return dir * (a - b);
  const [x, y] = [String(a), String(b)];
  const [ix, iy] = [ipv4Number(x), ipv4Number(y)];
  if (ix !== null && iy !== null) return dir * (ix - iy);
  return dir * x.localeCompare(y, undefined, { numeric: true, sensitivity: "base" });
}

/** A sort's field, whether or not it is an attribute, among the choices offered; a label for one that is not. */
export function unavailableSortLabel(field: string | undefined, offered: readonly { field: string }[]): string | null {
  if (!field || offered.some((s) => s.field === field)) return null;
  const a = attributeKey(field);
  return a === null ? `${field} (not a sort field)` : `${a} (not available: pick another sort)`;
}

export function fieldLabel(field: string, attrs: readonly AttributeLike[]): string {
  const a = attributeKey(field);
  if (a === null) return BUILTIN.get(field)?.label ?? field;
  return attrs.find((d) => d.key === a)?.label ?? a;
}

// ---------- List views ----------

export function listViewFor(doc: UiSettingsDocument | undefined, classKey: string | undefined): UiListView | undefined {
  return classKey ? doc?.listViews.find((v) => v.classKey === classKey) : undefined;
}

export function sortParam(s: UiListSort | null | undefined): string | undefined {
  return s ? `${s.direction === "desc" ? "-" : ""}${s.field}` : undefined;
}

export function hasFilters(f: UiListFilters | undefined): boolean {
  return !!f && (!!f.q || Object.values(f.lookups ?? {}).some((keys) => keys.length > 0));
}

/**
 * A filter's lookups (list key -> value keys) as the API's `lookupValueId`: the
 * value ids, comma-separated. Undefined without any; unknown keys are skipped
 * (the settings API reports them as issues).
 */
export function lookupValueIds(
  lookups: UiListFilters["lookups"],
  lists: readonly { id: string; key: string }[],
  values: readonly { id: string; listId: string; key: string }[],
): string | undefined {
  const ids: string[] = [];
  for (const [listKey, keys] of Object.entries(lookups ?? {})) {
    const list = lists.find((l) => l.key === listKey);
    if (list) for (const v of values) if (v.listId === list.id && keys.includes(v.key)) ids.push(v.id);
  }
  return ids.join(",") || undefined;
}

// ---------- Navigation ----------

/** The built-in pages; their default names come from the message catalog (`nav.page.<page>`, see `pageLabel`). */
export const PAGES: { page: UiPage; to: string; hiddenByDefault?: boolean }[] = [
  { page: "dashboard", to: "/" },
  { page: "inventory", to: "/cis" },
  { page: "search", to: "/search", hiddenByDefault: true },
  { page: "audit_log", to: "/admin/audit", hiddenByDefault: true },
  { page: "administration", to: "/admin" },
];
const PAGE = new Map(PAGES.map((p) => [p.page, p]));
/** Pages that sit under the "System" heading. */
const SYSTEM_PAGES = new Set<UiPage>(["audit_log", "administration"]);

export interface NavClass {
  id: string;
  key: string;
  name: string;
  icon: string | null;
  color: string | null;
  isActive: boolean;
  isAbstract: boolean;
  /** The area (menu tab) the class belongs to. */
  areaId?: string;
}

/** An area: a tab of the main menu that holds its classes. */
export interface NavArea {
  id: string;
  key: string;
  name: string;
  icon: string | null;
  color: string | null;
  isActive: boolean;
  sortOrder: number;
}

/**
 * Classes the menu offers: active ones (abstract ones list their subclasses' CIs)
 * whose area is not archived. Without areas (not loaded yet) no class is left out for its area.
 */
export function menuClasses<T extends NavClass>(classes: readonly T[], areas: readonly NavArea[] = []): T[] {
  const archived = new Set(areas.filter((a) => !a.isActive).map((a) => a.id));
  return classes.filter((c) => c.isActive && !(c.areaId && archived.has(c.areaId)));
}

/** Classes ordered by their area's tab position (stable: the class order is kept within an area). */
function byArea<T extends NavClass>(classes: readonly T[], areas: readonly NavArea[]): T[] {
  const rank = new Map([...areas].sort((a, b) => a.sortOrder - b.sortOrder).map((a, i) => [a.id, i]));
  const at = (c: T) => (c.areaId && rank.has(c.areaId) ? rank.get(c.areaId)! : rank.size);
  return classes.map((c, i) => ({ c, i })).sort((x, y) => at(x.c) - at(y.c) || x.i - y.i).map((x) => x.c);
}

/**
 * The stored entries followed by every page and class they leave out, in the
 * built-in order (pages first, the rest of the classes, by area, before the
 * System pages). The editor starts from this, so it always saves the complete menu.
 */
export function completeNavEntries(entries: readonly UiNavEntry[], classes: readonly NavClass[], areas: readonly NavArea[] = []): UiNavEntry[] {
  const pages = new Set<UiPage>();
  const listed = new Set<string>();
  for (const e of entries) {
    if (e.type === "page" && e.page) pages.add(e.page);
    if (e.type === "class" && e.classKey) listed.add(e.classKey);
    for (const i of e.items ?? []) listed.add(i.classKey);
  }
  const out: UiNavEntry[] = entries.map((e) => (e.items ? { ...e, items: e.items.map((i) => ({ ...i })) } : { ...e }));
  const addPage = (p: (typeof PAGES)[number]) => {
    if (!pages.has(p.page)) out.push({ type: "page", page: p.page, label: null, hidden: !!p.hiddenByDefault });
  };
  for (const p of PAGES) if (!SYSTEM_PAGES.has(p.page)) addPage(p);
  for (const c of byArea(menuClasses(classes, areas), areas)) {
    // New classes show up in the menu; abstract ones only when an administrator adds them.
    if (!listed.has(c.key)) out.push({ type: "class", classKey: c.key, label: null, hidden: c.isAbstract });
  }
  for (const p of PAGES) if (SYSTEM_PAGES.has(p.page)) addPage(p);
  return out;
}

export interface NavLinkItem {
  id: string;
  label: string;
  to: string;
  page?: UiPage;
  cls?: NavClass;
}
export interface NavGroup {
  id: string;
  heading: string | null;
  /** Set for an area's tab: the classes of one area that no section claims. */
  area?: NavArea;
  items: NavLinkItem[];
}

/**
 * The sidebar: visible entries grouped under headings. `showPage` applies
 * permissions. Classes that are not in an administrator's section are grouped
 * under their area's tab, one tab per area, placed where the area's first class
 * is in the menu order; classes without a known area fall back to "Browse by class".
 */
export function buildNav(
  entries: readonly UiNavEntry[],
  classes: readonly NavClass[],
  showPage: (p: UiPage) => boolean,
  areas: readonly NavArea[] = [],
): NavGroup[] {
  const byKey = new Map(menuClasses(classes, areas).map((c) => [c.key, c]));
  const areaById = new Map(areas.map((a) => [a.id, a]));
  const areaGroups = new Map<string, NavGroup>();
  const groups: NavGroup[] = [];
  /** Consecutive pages and loose classes share a group; a section always starts its own. */
  let open: NavGroup | null = null;
  const push = (heading: string | null, item: NavLinkItem) => {
    if (open && open.heading === heading) open.items.push(item);
    else {
      open = { id: `${heading ?? "pages"}-${groups.length}`, heading, items: [item] };
      groups.push(open);
    }
  };
  const classItem = (key: string, label: string | null | undefined): NavLinkItem | null => {
    const c = byKey.get(key);
    return c ? { id: `class:${c.key}`, label: label || c.name, to: `/cis?classId=${c.id}`, cls: c } : null;
  };
  for (const e of completeNavEntries(entries, classes, areas)) {
    if (e.hidden) continue;
    if (e.type === "page" && e.page) {
      const p = PAGE.get(e.page);
      if (!p || !showPage(e.page)) continue;
      push(SYSTEM_PAGES.has(e.page) ? t("nav.heading.system") : null, { id: `page:${e.page}`, label: e.label || pageLabel(e.page), to: p.to, page: e.page });
    } else if (e.type === "class" && e.classKey) {
      const item = classItem(e.classKey, e.label);
      const area = item?.cls?.areaId ? areaById.get(item.cls.areaId) : undefined;
      if (item && area) {
        let g = areaGroups.get(area.id);
        if (!g) {
          g = { id: `area:${area.key}`, heading: area.name, area, items: [] };
          areaGroups.set(area.id, g);
          groups.push(g);
        }
        g.items.push(item);
        open = null;
      } else if (item) push(t("nav.heading.byClass"), item);
    } else if (e.type === "section") {
      const items = (e.items ?? []).filter((i) => !i.hidden).map((i) => classItem(i.classKey, i.label)).filter((i): i is NavLinkItem => !!i);
      if (items.length === 0) continue;
      groups.push({ id: `section:${e.key}`, heading: e.label || e.key || "", items });
      open = null;
    }
  }
  // The tabs keep their places in the menu, but follow the areas' order among themselves (Administration › Areas).
  const slots = groups.flatMap((g, i) => (g.area ? [i] : []));
  const tabs = slots.map((i) => groups[i]).sort((a, b) => a.area!.sortOrder - b.area!.sortOrder);
  slots.forEach((slot, i) => (groups[slot] = tabs[i]));
  return groups;
}

export function pageLabel(page: UiPage): string {
  const key = `nav.page.${page}`;
  return hasMessage(key) ? t(key) : page;
}

// ---------- Dashboard ----------

export const WIDGET_TYPES: { type: UiWidgetType; label: string; hint: string }[] = [
  { type: "count_by_class", label: "CIs by class", hint: "Counts per class, optionally only some classes" },
  { type: "count_by_lookup", label: "CIs by lookup value", hint: "Counts per value of one lookup list, e.g. status" },
  { type: "recent_changes", label: "Recently changed", hint: "The latest changed CIs" },
  { type: "saved_search", label: "Saved search", hint: "CIs matching classes, filters and a sort" },
];
export function widgetLabel(type: UiWidgetType): string {
  const key = `dashboard.widget.${type}`;
  return hasMessage(key) ? t(key) : (WIDGET_TYPES.find((w) => w.type === type)?.label ?? type);
}

// ---------- Detail and form layouts ----------

/**
 * A class's layout: its default template's. The settings the API returns carry it on the class's entry; a
 * class without an entry uses the Standard template (SHAA-1472), which can have a layout of its own too.
 * Undefined when there is nothing to show but the built-in arrangement.
 */
export function layoutFor(doc: UiSettingsDocument | undefined, classKey: string | undefined): UiClassLayout | undefined {
  if (!classKey || !doc) return undefined;
  const filled = (l: { tabs?: unknown[]; hiddenFields?: unknown[]; readOnlyFields?: unknown[] } | undefined) =>
    !!l && ((l.tabs?.length ?? 0) > 0 || (l.hiddenFields?.length ?? 0) > 0 || (l.readOnlyFields?.length ?? 0) > 0);
  const entry = doc.layouts.find((l) => l.classKey === classKey);
  if (filled(entry)) return entry;
  const template = doc.layoutTemplates.find((x) => x.key === (entry?.templateKey ?? "standard"));
  if (!template?.layout || !filled(template.layout)) return entry;
  return normalizeLayout({ classKey, ...(entry?.templateKey ? { templateKey: entry.templateKey } : {}), ...template.layout });
}

/** Grid columns of a section that does not say (the API's default). */
export const GRID_COLUMNS = 3;
/** Columns of a tab's grid: a section spans 1 to 12 of them (the API's default is 12, the full width). */
export const SECTION_GRID = 12;
/** The most columns a section's grid has, and the widest a field can be. */
export const MAX_COLUMNS = 12;

/** What a layout section shows: a grid of fields, a note, or one of the detail page's built-in panels. */
export type SectionKind = NonNullable<NonNullable<NonNullable<UiClassLayout["tabs"]>[number]["sections"]>[number]["kind"]>;
export type PanelKind = Exclude<SectionKind, "fields" | "note">;
/** The detail page's built-in panels, each placeable once per layout. */
export const PANELS: { kind: PanelKind; label: string; hint: string }[] = [
  { kind: "relations", label: "Relationships", hint: "The CI's relationships, with adding and removing them" },
  { kind: "history", label: "History", hint: "Changes to the CI, field by field" },
  { kind: "audit", label: "Audit trail", hint: "Who changed the CI when, with the request id" },
];
export const panelLabel = (kind: PanelKind) => PANELS.find((p) => p.kind === kind)?.label ?? kind;
export const isPanelKind = (kind: SectionKind): kind is PanelKind => kind !== "fields" && kind !== "note";
/** Longest note text, in characters (the API's limit). */
export const NOTE_MAX_CHARS = 4000;
export const sectionKind = (s: { kind?: SectionKind }): SectionKind => s.kind ?? "fields";

/** The built-in panels a layout places somewhere. */
export function placedPanels(l: UiClassLayout | undefined): Set<PanelKind> {
  const out = new Set<PanelKind>();
  for (const t of l?.tabs ?? []) for (const s of t.sections ?? []) if (isPanelKind(sectionKind(s))) out.add(s.kind as PanelKind);
  return out;
}

/** The layout without the sections of `kinds` (e.g. panels on the form, or ones the user may not see). */
export function withoutKinds(l: UiClassLayout, kinds: readonly SectionKind[]): UiClassLayout {
  return { ...l, tabs: (l.tabs ?? []).map((t) => ({ ...t, sections: (t.sections ?? []).filter((s) => !kinds.includes(sectionKind(s))) })) };
}

/**
 * Every optional part of a layout filled in (tabs, sections, columns, widths),
 * without the old `panels`. Every tab is free: one stored on the earlier grid
 * gets each section as a window where it was on the grid (lib/freeLayout makeFree).
 */
export function normalizeLayout(l: UiClassLayout): UiClassLayout {
  const tabs = (l.tabs ?? []).map((t) => ({
    key: t.key,
    label: t.label,
    placement: "free" as const,
    // Content blocks (a `kind` other than fields: notes, panels) are kept as stored (a copy: the draft is edited), without fields.
    sections: (t.sections ?? []).map((s) =>
      sectionKind(s) !== "fields"
        ? { ...s, width: s.width ?? SECTION_GRID }
        : {
            ...s,
            key: s.key,
            label: s.label,
            columns: s.columns ?? GRID_COLUMNS,
            width: s.width ?? SECTION_GRID,
            collapsed: !!s.collapsed,
            fields: (s.fields ?? []).map((f) => ({ field: f.field, width: f.width ?? 1 })),
          },
    ),
  }));
  tabs.forEach(makeFree);
  return {
    classKey: l.classKey,
    // The class's default template: the tabs below are its layout, and saving them edits it.
    ...(l.templateKey ? { templateKey: l.templateKey } : {}),
    tabs,
    hiddenFields: l.hiddenFields ?? [],
    readOnlyFields: l.readOnlyFields ?? [],
  };
}

export interface ResolvedField {
  field: string;
  /** Columns spanned, at most the section's columns. */
  width: number;
}
export interface ResolvedSection {
  key: string;
  label: string;
  collapsed: boolean;
  columns: number;
  /** Columns of the tab's 12-column grid the section spans (SECTION_GRID: the full width). */
  width: number;
  /** Starts a new row of the tab's grid. */
  newRow: boolean;
  /** At least this many field rows tall, when set. */
  minHeight?: number;
  fields: ResolvedField[];
  /** Placed by the built-in rules, not by the administrator: fields no section of the layout holds. */
  auto: boolean;
  /** A grid of `fields`, or a content block (a note's `text`, a built-in panel) without fields. */
  kind: SectionKind;
  text?: string;
  /** Where the section sits as a window of its tab (lib/freeLayout); absent for the automatic sections. */
  frame?: UiSectionFrame;
}
/** A tab: the layout's sections are windows (in reading order), the automatic ones follow below them at the full width. */
export interface ResolvedTab {
  key: string;
  label: string;
  sections: ResolvedSection[];
}

/** An empty layout: what a class without one in Customization gets. */
export const builtInLayout = (classKey: string): UiClassLayout => ({ classKey, tabs: [], hiddenFields: [], readOnlyFields: [] });

/** Fields shown right after another one wherever that is placed, unless placed themselves (the detail page's "Active"). */
const COMPANIONS: Record<string, string> = { validUntil: "active" };

/**
 * A class layout as tabs of sections: the administrator's tabs, sections and
 * field widths in order; then, at the end of the first tab, everything they do
 * not place — a "General" section with the unplaced `core` fields and the
 * attributes without a group, the other attribute groups, and finally a
 * "Record" section with the unplaced `record` fields (class, timestamps).
 * Hidden fields, built-in fields in neither list, and empty sections and tabs
 * are left out. Without a layout of its own a class gets `builtInLayout`: one
 * General tab with General, then its attribute groups.
 */
export function resolveLayout(
  layout: UiClassLayout,
  attrs: readonly AttributeLike[],
  core: readonly string[],
  record: readonly string[] = [],
  keepEmpty = false,
): ResolvedTab[] {
  const hidden = new Set(layout.hiddenFields ?? []);
  const attrKeys = new Set(attrs.map((a) => a.key));
  const usable = (f: string) => {
    if (hidden.has(f)) return false;
    const a = attributeKey(f);
    return a === null ? core.includes(f) || record.includes(f) : attrKeys.has(a);
  };
  const placed = new Set<string>();
  for (const t of layout.tabs ?? []) for (const s of t.sections ?? []) for (const f of s.fields ?? []) if (usable(f.field)) placed.add(f.field);
  const companion = (f: string) => {
    const c = COMPANIONS[f];
    return c && usable(c) && !placed.has(c) ? c : null;
  };
  const taken = new Set<string>();
  // Every tab free: one stored on the earlier grid shows its sections as windows where they were on the grid.
  const tabs: ResolvedTab[] = (layout.tabs ?? []).map(freeCopy).map((t) => ({
    key: t.key,
    label: t.label,
    // The windows in reading order (y, then x): the order of the page, the keyboard and screen readers.
    sections: readingOrder(t.sections ?? []).map((s): ResolvedSection => {
      const kind = sectionKind(s);
      const place = { width: sectionWidth(s), newRow: !!s.newRow, minHeight: s.minHeight ?? undefined, ...(s.frame ? { frame: s.frame } : {}) };
      if (kind !== "fields") return { key: s.key, label: s.label, collapsed: !!s.collapsed, columns: GRID_COLUMNS, ...place, fields: [], auto: false, kind, text: s.text };
      const columns = Math.min(Math.max(s.columns ?? GRID_COLUMNS, 1), MAX_COLUMNS);
      const fields: ResolvedField[] = [];
      for (const f of s.fields ?? []) {
        if (!usable(f.field) || taken.has(f.field)) continue;
        taken.add(f.field);
        fields.push({ field: f.field, width: Math.min(Math.max(f.width ?? 1, 1), columns) });
        const c = companion(f.field);
        if (c && !taken.has(c)) {
          taken.add(c);
          fields.push({ field: c, width: 1 });
        }
      }
      return {
        key: s.key,
        label: s.label,
        collapsed: !!s.collapsed,
        columns,
        ...place,
        fields,
        auto: false,
        kind,
      };
    }),
  }));
  const one = (field: string): ResolvedField => ({ field, width: 1 });
  const auto = (key: string, label: string, fields: string[]): ResolvedSection => ({
    key,
    label,
    collapsed: false,
    columns: GRID_COLUMNS,
    width: SECTION_GRID,
    newRow: false,
    fields: fields.map(one),
    auto: true,
    kind: "fields",
  });
  const general = core.filter((f) => usable(f) && !taken.has(f));
  const rest = attrs.filter((a) => usable(`${ATTRIBUTE_PREFIX}${a.key}`) && !taken.has(`${ATTRIBUTE_PREFIX}${a.key}`));
  const groups = groupAttributes(rest).map(([group, items]) => ({ group, fields: items.map((a) => `${ATTRIBUTE_PREFIX}${a.key}`) }));
  const ungrouped = groups[0]?.group === GENERAL_SECTION ? groups.shift()!.fields : [];
  const trailing = [
    auto("_general", GENERAL_SECTION, [...general, ...ungrouped]),
    ...groups.map((g) => auto(`_group:${g.group}`, g.group, g.fields)),
    auto("_record", "Record", record.filter((f) => usable(f) && !taken.has(f))),
  ];
  if (tabs.length === 0) tabs.push({ key: "general", label: GENERAL_SECTION, sections: [] });
  tabs[0].sections.push(...trailing);
  if (keepEmpty) return tabs.map((t) => ({ ...t, sections: t.sections.filter((s) => !s.auto || s.fields.length > 0) }));
  const shown = tabs.map((t) => ({ ...t, sections: t.sections.filter((s) => s.kind !== "fields" || s.fields.length > 0) })).filter((t) => t.sections.length > 0);
  return shown.length > 0 ? shown : [{ ...tabs[0], sections: [] }];
}

/** A section's width on the tab's 12-column grid (full width when it does not say). */
export const sectionWidth = (s: { width?: number | null }) => Math.min(Math.max(Math.round(s.width ?? SECTION_GRID), 1), SECTION_GRID);

/**
 * The CSS classes that put a section's grid and its fields in place: `lg-cols-N`
 * on the grid, `lg-w-N` on a field (the stylesheet narrows both on small screens).
 * Below the tablet breakpoint a grid of 3 or more columns has 2, and a field
 * spans `lg-n-1` or `lg-n-2` of them: the grids of up to 4 columns keep their
 * earlier rule (at most 2), finer grids give a field both columns when it takes
 * more than half of the section.
 */
export const gridClass = (columns: number) => `lg-grid lg-cols-${columns}`;
export function cellClass(width: number, columns: number = GRID_COLUMNS): string {
  const narrow = columns <= 4 ? Math.min(width, 2) : width * 2 > columns ? 2 : 1;
  return `lg-cell lg-w-${width} lg-n-${narrow}`;
}
/**
 * The CSS classes and style that put a section on its tab's 12-column grid:
 * `lg-s-N` spans N columns, `lg-new-row` starts a row, `--lg-min-h` is the
 * minimum height in field rows. Below the tablet breakpoint every section takes
 * the full width (the stylesheet).
 */
export function sectionClass(s: { width?: number | null; newRow?: boolean | null }): string[] {
  return ["lg-sec", `lg-s-${sectionWidth(s)}`, ...(s.newRow ? ["lg-new-row"] : [])];
}
export const sectionStyle = (s: { minHeight?: number | null }) => (s.minHeight ? { "--lg-min-h": String(s.minHeight) } : undefined);

/**
 * A window of a free tab (lg-win) inside its tab's area (lg-free, as tall as
 * `freeAreaStyle` says): position, size and stacking order as CSS variables,
 * which the stylesheet ignores below the tablet breakpoint and in print (the
 * windows stack in reading order there, at least `minH` tall).
 */
export const windowClass = "lg-win";
export function windowStyle(f: UiSectionFrame): Record<string, string> {
  return {
    "--win-x": `${f.x * 100}%`,
    "--win-y": `${f.y}px`,
    "--win-w": `${f.w * 100}%`,
    "--win-h": `${f.h}px`,
    "--win-z": String(f.z),
    ...(f.minH ? { "--win-min-h": `${f.minH}px` } : {}),
  };
}
/** The area of a free tab's windows: as tall as the lowest one reaches. */
export function freeAreaStyle(sections: readonly { frame?: UiSectionFrame }[]): Record<string, string> {
  const bottom = Math.max(0, ...sections.flatMap((s) => (s.frame ? [s.frame.y + s.frame.h] : [])));
  return { "--free-h": `${bottom}px` };
}

/** Core fields a layout cannot hide (the API's CORE_FIELDS): every CI has them. */
export const LOCKED_FIELDS = BUILTIN_FIELDS.filter((f) => f.form && !f.hideable).map((f) => f.key);
/** The core fields of every CI, which the form edits: the General section starts with them, the hideable ones last. */
export const CORE_FIELDS = [...LOCKED_FIELDS, ...BUILTIN_FIELDS.filter((f) => f.form && f.hideable).map((f) => f.key)];
/** Core fields the detail page's General panel shows: the edited ones and whether the CI is active. */
export const DETAIL_CORE = [...LOCKED_FIELDS, "active", ...CORE_FIELDS.filter((f) => !LOCKED_FIELDS.includes(f))];
/** Bookkeeping fields the detail page shows last, in a "Record" panel (the label is the page title). */
export const DETAIL_RECORD = BUILTIN_FIELDS.filter((f) => f.key !== "label" && !DETAIL_CORE.includes(f.key)).map((f) => f.key);
