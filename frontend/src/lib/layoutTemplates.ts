import type { UiClassLayout, UiLayout, UiLayoutTemplate, UiSettingsDocument } from "../api/uiSettings";

/**
 * Layout templates (SHAA-1472): named detail page and form layouts in the
 * settings document (`layoutTemplates`). Each class names its default
 * (`layouts[].templateKey`; Standard when it has no entry), and a CI can show
 * another template or a layout of its own (/configuration-items/{id}/layout).
 *
 * The settings the API returns carry each class's template layout on the class
 * as well ("expanded"). Sent back unchanged that is ignored, but a class entry
 * sent with another template's key and the old tabs would overwrite the new
 * template, so whatever changes a class's template drops the copy first.
 */

/** Key of the built-in template every class without an entry uses (backend STANDARD_TEMPLATE). */
export const STANDARD_TEMPLATE = "standard";
export const TEMPLATE_NAME_MAX = 100;
export const TEMPLATE_DESCRIPTION_MAX = 500;
const KEY_MAX = 63;

/** The class's default template: its entry's, else Standard. */
export function classTemplateKey(doc: UiSettingsDocument, classKey: string): string {
  return doc.layouts.find((l) => l.classKey === classKey)?.templateKey ?? STANDARD_TEMPLATE;
}

/** Makes `templateKey` the default of the class; the entry keeps only its key and template (see above). */
export function setClassTemplate(doc: UiSettingsDocument, classKey: string, templateKey: string): void {
  const entry: UiClassLayout = { classKey, templateKey };
  const at = doc.layouts.findIndex((l) => l.classKey === classKey);
  if (at < 0) doc.layouts.push(entry);
  else if (doc.layouts[at].templateKey !== templateKey) doc.layouts[at] = entry;
}

/** The document as the editors send it: class entries that name a template carry only the key. */
export function compactLayouts(doc: UiSettingsDocument): UiSettingsDocument {
  return { ...doc, layouts: doc.layouts.map((l) => (l.templateKey ? { classKey: l.classKey, templateKey: l.templateKey } : l)) };
}

/** The keys of the classes that use the template as their default (in `classKeys`: classes without an entry use Standard). */
export function classesUsing(doc: UiSettingsDocument, templateKey: string, classKeys: readonly string[]): string[] {
  return classKeys.filter((k) => classTemplateKey(doc, k) === templateKey);
}

/** A layout's content (tabs, hidden and read-only fields), as a template or a CI's own layout stores it. */
export function layoutContent(l: { tabs?: UiLayout["tabs"]; hiddenFields?: string[]; readOnlyFields?: string[] }): UiLayout {
  return {
    tabs: JSON.parse(JSON.stringify(l.tabs ?? [])) as UiLayout["tabs"],
    hiddenFields: [...(l.hiddenFields ?? [])],
    readOnlyFields: [...(l.readOnlyFields ?? [])],
  };
}

/** A template's content as a class layout of `classKey`, the shape the layout editor and the pages work on. */
export function asClassLayout(classKey: string, l: UiLayout | undefined): UiClassLayout {
  return { classKey, ...layoutContent(l ?? { tabs: [] }) };
}

/**
 * A key for a new template named `name`: lower_snake_case of its letters and
 * digits (accents dropped), starting with a letter, unique among `taken` by
 * `_2`, `_3`…
 */
export function templateKeyFor(name: string, taken: Iterable<string>): string {
  const used = new Set(taken);
  let stem = name
    .normalize("NFKD")
    .replace(/\p{M}/gu, "")
    .toLowerCase()
    .replace(/ß/g, "ss")
    .replace(/[^a-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "");
  if (!/^[a-z]/.test(stem)) stem = stem ? `t_${stem}` : "template";
  stem = stem.slice(0, KEY_MAX).replace(/_+$/, "");
  if (!used.has(stem)) return stem;
  for (let n = 2; ; n++) {
    const suffix = `_${n}`;
    const key = `${stem.slice(0, KEY_MAX - suffix.length)}${suffix}`;
    if (!used.has(key)) return key;
  }
}

/** `base`, else `base (2)`, `base (3)`…: unique among the templates' names, ignoring case, at most TEMPLATE_NAME_MAX characters. */
export function freeTemplateName(base: string, templates: readonly UiLayoutTemplate[]): string {
  const taken = new Set(templates.map((t) => t.name.trim().toLowerCase()));
  for (let n = 1; ; n++) {
    const suffix = n === 1 ? "" : ` (${n})`;
    const name = `${base.trim().slice(0, TEMPLATE_NAME_MAX - suffix.length).trimEnd()}${suffix}`;
    if (!taken.has(name.toLowerCase())) return name;
  }
}

export type NameProblem = "required" | "tooLong" | "taken";

/** What is wrong with `name` for a template (`except`: the template being renamed), as the API checks it. */
export function templateNameProblem(name: string, templates: readonly UiLayoutTemplate[], except?: string): NameProblem | null {
  const n = name.trim();
  if (!n) return "required";
  if (n.length > TEMPLATE_NAME_MAX) return "tooLong";
  if (templates.some((t) => t.key !== except && t.name.trim().toLowerCase() === n.toLowerCase())) return "taken";
  return null;
}

/** Adds a template named `name` with `layout` (copied) and returns it. */
export function addTemplate(doc: UiSettingsDocument, name: string, layout: UiLayout | undefined, description?: string): UiLayoutTemplate {
  const t: UiLayoutTemplate = {
    key: templateKeyFor(name, doc.layoutTemplates.map((x) => x.key)),
    name: name.trim(),
    ...(description?.trim() ? { description: description.trim() } : {}),
    layout: layoutContent(layout ?? { tabs: [] }),
  };
  doc.layoutTemplates.push(t);
  return t;
}

/** Who uses a template: the classes that have it as their default, and live CIs that show it instead of theirs (null: unknown, some are in classes you may not view; undefined: not counted yet). */
export interface TemplateUsers {
  classKeys: string[];
  ciCount: number | null | undefined;
}

/** Whether a template can be deleted: not Standard, and nobody uses it. */
export function deletable(key: string, users: TemplateUsers): boolean {
  return key !== STANDARD_TEMPLATE && users.classKeys.length === 0 && users.ciCount === 0;
}

/** A row of Customization › Layouts' class table. */
export interface ClassRow {
  key: string;
  id: string;
  name: string;
  isActive: boolean;
  templateKey: string;
  templateName: string;
  /** Live CIs of the class itself with a layout of their own: null when unknown (a class you may not view), undefined while counting. */
  ownLayoutCount: number | null | undefined;
}

export type ClassSort = "class" | "-class" | "template" | "-template" | "owned" | "-owned";

/** The inventory of a class's CIs with a layout of their own (the count is per class, without subclasses). */
export function ownLayoutLink(classId: string) {
  return { path: "/cis", query: { classId, includeSubclasses: "false", ownLayout: "true" } };
}

/**
 * The class table's rows: every class with its default template (from `doc`, the
 * page's draft), narrowed by `q` (class name or key, or template name; any case)
 * and `uses` (a template key), sorted by class or template name or by the number
 * of CIs with a layout of their own (`owned`, by class key; unknown counts last), then class.
 */
export function classRows(
  classes: readonly { id: string; key: string; name: string; isActive: boolean }[],
  doc: UiSettingsDocument,
  opts: { q?: string; uses?: string; sort?: string; owned?: ReadonlyMap<string, number | null> },
): ClassRow[] {
  const names = new Map(doc.layoutTemplates.map((t) => [t.key, t.name]));
  const q = (opts.q ?? "").trim().toLowerCase();
  const rows = classes
    .map((c): ClassRow => {
      const templateKey = classTemplateKey(doc, c.key);
      const ownLayoutCount = opts.owned ? (opts.owned.get(c.key) ?? null) : undefined;
      return { key: c.key, id: c.id, name: c.name, isActive: c.isActive, templateKey, templateName: names.get(templateKey) ?? templateKey, ownLayoutCount };
    })
    .filter((r) => !opts.uses || r.templateKey === opts.uses)
    .filter((r) => !q || r.name.toLowerCase().includes(q) || r.key.includes(q) || r.templateName.toLowerCase().includes(q));
  const byName = (a: ClassRow, b: ClassRow) => a.name.localeCompare(b.name, undefined, { sensitivity: "base" }) || a.key.localeCompare(b.key);
  const sort = (opts.sort ?? "class") as ClassSort;
  const dir = sort.startsWith("-") ? -1 : 1;
  const field = sort.replace(/^-/, "");
  const known = (r: ClassRow) => (typeof r.ownLayoutCount === "number" ? 1 : 0);
  return rows.sort((a, b) =>
    field === "template"
      ? dir * a.templateName.localeCompare(b.templateName, undefined, { sensitivity: "base" }) || byName(a, b)
      : field === "owned"
        ? known(b) - known(a) || dir * ((a.ownLayoutCount ?? 0) - (b.ownLayoutCount ?? 0)) || byName(a, b)
        : dir * byName(a, b),
  );
}

export interface SectionError {
  /** The API's path, e.g. `settings.layoutTemplates.0.layout.tabs.1.sections.0.kind`. */
  path: string;
  message: string;
}
const SECTION_PATH = /^\.tabs\.(\d+)\.sections\.(\d+)(?:\.|$)/;

/** A refused save: the layout as sent, and where the API's paths for it start. */
export interface SentLayout {
  /** e.g. `settings.layoutTemplates.2.layout` (a template) or `layout` (a CI's own layout). */
  prefix: string;
  layout: UiClassLayout;
}

/**
 * The API's refusals of a saved layout that point into one of its sections
 * (`<prefix>.tabs.N.sections.N…`), by the key of that section in the layout as
 * it was sent: the editor shows them next to the section even after it moved.
 */
export function sectionErrors(error: unknown, sent: SentLayout | null | undefined): Record<string, SectionError[]> {
  const out: Record<string, SectionError[]> = {};
  // An ApiError's details (duck-typed: this module stays free of the API client, for the unit tests).
  const details = (error as { details?: unknown } | null)?.details;
  if (!sent || !Array.isArray(details)) return out;
  for (const d of details as { field?: string | null; message: string }[]) {
    if (!d.field?.startsWith(`${sent.prefix}.`)) continue;
    const m = SECTION_PATH.exec(d.field.slice(sent.prefix.length));
    const section = m ? sent.layout.tabs?.[Number(m[1])]?.sections?.[Number(m[2])] : undefined;
    if (section) (out[section.key] ??= []).push({ path: d.field, message: d.message });
  }
  return out;
}
