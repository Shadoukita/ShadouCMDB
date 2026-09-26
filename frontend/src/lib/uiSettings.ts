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
import { groupAttributes } from "./attributes";

/**
 * The UI settings document (Administration › Customization) as the screens
 * apply it. Every section is optional; whatever is left out keeps the built-in
 * behaviour, so `{}` is the stock UI. Classes, attributes and lookups are named
 * by key, never by id.
 */

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
    dashboard: { widgets: d.dashboard?.widgets ?? null },
    listViews: d.listViews ?? [],
    layouts: d.layouts ?? [],
  };
}

// ---------- Fields ----------

export interface BuiltinField {
  key: string;
  label: string;
  /** API sort field for list columns. */
  sort?: UiListSort["field"];
  /** The CI form's field for it; absent for fields the form does not edit (class, timestamps). */
  form?: string;
}

/** CI fields every class has, in their built-in order. Attributes are `attributes.<key>`. */
export const BUILTIN_FIELDS: BuiltinField[] = [
  { key: "name", label: "Name", sort: "name", form: "name" },
  { key: "class", label: "Class", sort: "className" },
  { key: "status", label: "Status", sort: "statusName", form: "statusId" },
  { key: "environment", label: "Environment", form: "environmentId" },
  { key: "owner", label: "Owner", form: "ownerId" },
  { key: "location", label: "Location", form: "locationId" },
  { key: "hostname", label: "Hostname", sort: "hostname", form: "hostname" },
  { key: "ipAddress", label: "IP address", sort: "ipAddress", form: "ipAddress" },
  { key: "serialNumber", label: "Serial number", sort: "serialNumber", form: "serialNumber" },
  { key: "notes", label: "Notes", form: "notes" },
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
export const DEFAULT_COLUMNS = ["name", "class", "status", "environment", "owner", "location", "hostname", "ipAddress", "serialNumber", "updatedAt"];

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
  return !!f && (!!f.q || !!f.statusKeys?.length || !!f.environmentKeys?.length || !!f.locationKeys?.length);
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
}

/** Classes the menu offers: active ones (abstract ones list their subclasses' CIs). */
export const menuClasses = <T extends NavClass>(classes: readonly T[]) => classes.filter((c) => c.isActive);

/**
 * The stored entries followed by every page and class they leave out, in the
 * built-in order (pages first, the rest of the classes before the System pages).
 * The editor starts from this, so it always saves the complete menu.
 */
export function completeNavEntries(entries: readonly UiNavEntry[], classes: readonly NavClass[]): UiNavEntry[] {
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
  for (const c of menuClasses(classes)) {
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
  items: NavLinkItem[];
}

/** The sidebar: visible entries grouped under headings. `showPage` applies permissions. */
export function buildNav(entries: readonly UiNavEntry[], classes: readonly NavClass[], showPage: (p: UiPage) => boolean): NavGroup[] {
  const byKey = new Map(menuClasses(classes).map((c) => [c.key, c]));
  const groups: NavGroup[] = [];
  const push = (heading: string | null, item: NavLinkItem, id = heading ?? "pages") => {
    const last = groups[groups.length - 1];
    if (last && last.heading === heading && last.id === id) last.items.push(item);
    else groups.push({ id: `${id}-${groups.length}`, heading, items: [item] });
  };
  const classItem = (key: string, label: string | null | undefined): NavLinkItem | null => {
    const c = byKey.get(key);
    return c ? { id: `class:${c.key}`, label: label || c.name, to: `/cis?classId=${c.id}`, cls: c } : null;
  };
  for (const e of completeNavEntries(entries, classes)) {
    if (e.hidden) continue;
    if (e.type === "page" && e.page) {
      const p = PAGE.get(e.page);
      if (!p || !showPage(e.page)) continue;
      push(SYSTEM_PAGES.has(e.page) ? "System" : null, { id: `page:${e.page}`, label: e.label || p.label, to: p.to, page: e.page });
    } else if (e.type === "class" && e.classKey) {
      const item = classItem(e.classKey, e.label);
      if (item) push("Browse by class", item);
    } else if (e.type === "section") {
      const items = (e.items ?? []).filter((i) => !i.hidden).map((i) => classItem(i.classKey, i.label)).filter((i): i is NavLinkItem => !!i);
      if (items.length === 0) continue;
      groups.push({ id: `section:${e.key}`, heading: e.label || e.key || "", items });
    }
  }
  return groups;
}

export function pageLabel(page: UiPage): string {
  return PAGE.get(page)?.label ?? page;
}

// ---------- Dashboard ----------

export const WIDGET_TYPES: { type: UiWidgetType; label: string; hint: string }[] = [
  { type: "count_by_class", label: "CIs by class", hint: "Counts per class, optionally only some classes" },
  { type: "count_by_status", label: "CIs by status", hint: "Counts per status" },
  { type: "count_by_environment", label: "CIs by environment", hint: "Counts per environment" },
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

/**
 * A class layout as panels of fields: the administrator's panels in order,
 * then everything they do not place — built-in fields in a "General" panel and
 * attributes in their attribute groups. Hidden fields and fields that are not
 * in `builtins` (e.g. timestamps on the form) are left out. Null without a
 * layout, so screens keep their built-in arrangement.
 */
export function resolveLayout(
  layout: UiClassLayout | undefined,
  attrs: readonly AttributeLike[],
  builtins: readonly string[],
): ResolvedPanel[] | null {
  if (!layout) return null;
  const hidden = new Set(layout.hiddenFields ?? []);
  const attrKeys = new Set(attrs.map((a) => a.key));
  const usable = (f: string) => {
    if (hidden.has(f) && f !== "name") return false;
    const a = attributeKey(f);
    return a === null ? builtins.includes(f) : attrKeys.has(a);
  };
  const placed = new Set<string>();
  const panels: ResolvedPanel[] = [];
  for (const p of layout.panels ?? []) {
    const fields = (p.fields ?? []).filter((f) => usable(f) && !placed.has(f));
    fields.forEach((f) => placed.add(f));
    panels.push({ key: p.key, label: p.label, collapsed: !!p.collapsed, fields });
  }
  const general = builtins.filter((f) => usable(f) && !placed.has(f));
  if (general.length > 0) panels.push({ key: "_general", label: "General", collapsed: false, fields: general });
  const rest = attrs.filter((a) => usable(`${ATTRIBUTE_PREFIX}${a.key}`) && !placed.has(`${ATTRIBUTE_PREFIX}${a.key}`));
  for (const [group, items] of groupAttributes(rest)) {
    panels.push({ key: `_group:${group}`, label: group, collapsed: false, fields: items.map((a) => `${ATTRIBUTE_PREFIX}${a.key}`) });
  }
  return panels.filter((p) => p.fields.length > 0);
}

/** Built-in fields the detail page shows (the name is the page title). */
export const DETAIL_BUILTINS = BUILTIN_FIELDS.filter((f) => f.key !== "name").map((f) => f.key);
/** Built-in fields the CI form edits. */
export const FORM_BUILTINS = BUILTIN_FIELDS.filter((f) => f.form).map((f) => f.key);
