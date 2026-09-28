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
import { GENERAL_SECTION, groupAttributes } from "./attributes";

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
    layouts: (d.layouts ?? []).map((l) => ({ ...l, panels: l.panels ?? [], hiddenFields: l.hiddenFields ?? [], readOnlyFields: l.readOnlyFields ?? [] })),
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
}

/**
 * CI fields every class has, in their built-in order. Attributes are `attributes.<key>`;
 * name, status, hostname and the like are attributes of the classes that define them.
 */
export const BUILTIN_FIELDS: BuiltinField[] = [
  { key: "label", label: "Label", sort: "label" },
  { key: "ident", label: "Ident", sort: "ident", form: "ident" },
  { key: "class", label: "Class", sort: "className" },
  { key: "validFrom", label: "Valid from", sort: "validFrom", form: "validFrom" },
  { key: "validUntil", label: "Valid until", sort: "validUntil", form: "validUntil" },
  { key: "active", label: "Active" },
  { key: "createdAt", label: "Created", sort: "createdAt" },
  { key: "updatedAt", label: "Updated", sort: "updatedAt" },
];
export const BUILTIN = new Map(BUILTIN_FIELDS.map((f) => [f.key, f]));

/** Sort fields the list API accepts, with labels. */
export const SORT_FIELDS: { field: UiListSort["field"]; label: string }[] = BUILTIN_FIELDS.filter((f) => f.sort).map((f) => ({
  field: f.sort!,
  label: f.label,
}));

/** The inventory's columns when no list view says otherwise. */
export const DEFAULT_COLUMNS = ["label", "ident", "class", "active", "updatedAt"];

export const ATTRIBUTE_PREFIX = "attributes.";
export const attributeKey = (field: string) => (field.startsWith(ATTRIBUTE_PREFIX) ? field.slice(ATTRIBUTE_PREFIX.length) : null);

export interface AttributeLike {
  key: string;
  label: string;
  groupName: string | null;
  sortOrder: number;
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

export const PAGES: { page: UiPage; label: string; to: string; hiddenByDefault?: boolean }[] = [
  { page: "dashboard", label: "Dashboard", to: "/" },
  { page: "inventory", label: "All configuration items", to: "/cis" },
  { page: "search", label: "Search", to: "/search", hiddenByDefault: true },
  { page: "audit_log", label: "Audit log", to: "/admin/audit", hiddenByDefault: true },
  { page: "administration", label: "Administration", to: "/admin" },
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
      push(SYSTEM_PAGES.has(e.page) ? "System" : null, { id: `page:${e.page}`, label: e.label || p.label, to: p.to, page: e.page });
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
      } else if (item) push("Browse by class", item);
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
  return PAGE.get(page)?.label ?? page;
}

// ---------- Dashboard ----------

export const WIDGET_TYPES: { type: UiWidgetType; label: string; hint: string }[] = [
  { type: "count_by_class", label: "CIs by class", hint: "Counts per class, optionally only some classes" },
  { type: "count_by_lookup", label: "CIs by lookup value", hint: "Counts per value of one lookup list, e.g. status" },
  { type: "recent_changes", label: "Recently changed", hint: "The latest changed CIs" },
  { type: "saved_search", label: "Saved search", hint: "CIs matching classes, filters and a sort" },
];
export const widgetLabel = (t: UiWidgetType) => WIDGET_TYPES.find((w) => w.type === t)?.label ?? t;

// ---------- Detail and form layouts ----------

export function layoutFor(doc: UiSettingsDocument | undefined, classKey: string | undefined): UiClassLayout | undefined {
  return classKey ? doc?.layouts.find((l) => l.classKey === classKey) : undefined;
}

export interface ResolvedPanel {
  key: string;
  label: string;
  collapsed: boolean;
  fields: string[];
}

/** An empty layout: what a class without one in Customization gets. */
export const builtInLayout = (classKey: string): UiClassLayout => ({ classKey, panels: [], hiddenFields: [], readOnlyFields: [] });

/**
 * A class layout as panels of fields: the administrator's panels in order, then
 * everything they do not place — a "General" panel with the unplaced `core`
 * fields and the attributes without a group, the other attribute groups, and
 * finally a "Record" panel with the unplaced `record` fields (class,
 * timestamps). Hidden fields and built-in fields in neither list are left out.
 * Without a layout of its own a class gets `builtInLayout`: General, then its groups.
 */
export function resolveLayout(
  layout: UiClassLayout,
  attrs: readonly AttributeLike[],
  core: readonly string[],
  record: readonly string[] = [],
): ResolvedPanel[] {
  const hidden = new Set(layout.hiddenFields ?? []);
  const attrKeys = new Set(attrs.map((a) => a.key));
  const usable = (f: string) => {
    if (hidden.has(f)) return false;
    const a = attributeKey(f);
    return a === null ? core.includes(f) || record.includes(f) : attrKeys.has(a);
  };
  const placed = new Set<string>();
  const panels: ResolvedPanel[] = [];
  for (const p of layout.panels ?? []) {
    const fields = (p.fields ?? []).filter((f) => usable(f) && !placed.has(f));
    fields.forEach((f) => placed.add(f));
    panels.push({ key: p.key, label: p.label, collapsed: !!p.collapsed, fields });
  }
  const general = core.filter((f) => usable(f) && !placed.has(f));
  const rest = attrs.filter((a) => usable(`${ATTRIBUTE_PREFIX}${a.key}`) && !placed.has(`${ATTRIBUTE_PREFIX}${a.key}`));
  const groups = groupAttributes(rest).map(([group, items]) => ({ group, fields: items.map((a) => `${ATTRIBUTE_PREFIX}${a.key}`) }));
  const ungrouped = groups[0]?.group === GENERAL_SECTION ? groups.shift()!.fields : [];
  panels.push({ key: "_general", label: GENERAL_SECTION, collapsed: false, fields: [...general, ...ungrouped] });
  for (const g of groups) panels.push({ key: `_group:${g.group}`, label: g.group, collapsed: false, fields: g.fields });
  const rec = record.filter((f) => usable(f) && !placed.has(f));
  panels.push({ key: "_record", label: "Record", collapsed: false, fields: rec });
  return panels.filter((p) => p.fields.length > 0);
}

/** The core fields of every CI, which the form edits: the General section starts with them. */
export const CORE_FIELDS = BUILTIN_FIELDS.filter((f) => f.form).map((f) => f.key);
/** Core fields the detail page's General panel shows: the edited ones and whether the CI is active. */
export const DETAIL_CORE = [...CORE_FIELDS, "active"];
/** Bookkeeping fields the detail page shows last, in a "Record" panel (the label is the page title). */
export const DETAIL_RECORD = BUILTIN_FIELDS.filter((f) => f.key !== "label" && !DETAIL_CORE.includes(f.key)).map((f) => f.key);
