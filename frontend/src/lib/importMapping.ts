// Step 2 of the import wizard, kept free of Vue so it can be unit tested: what a
// file column can map to (built from the class's attribute definitions and the
// relationship rules, never from a per-class list), the mapping sent with
// PUT /imports/{id}/mapping, the checks worth making before sending it, and where
// the API's field errors belong in the mapping table.
import type { ApiErrorDetail, Schemas } from "../api/client";
import type { ImportColumnTarget, ImportJobMapping, ImportMappingDefinition } from "../api/imports";

type Attribute = Schemas["AttributeDefinition"];
type CiClass = Schemas["CiClass"];
type RelationshipType = Schemas["RelationshipType"];
type RelationshipRule = Schemas["RelationshipRule"];
export type ImportMode = Schemas["ImportMode"];
export type ImportEmptyCells = Schemas["ImportEmptyCells"];
export type ImportDateFormat = Schemas["ImportDateFormat"];
export type MatchBy = Schemas["ImportMatchBy"];
type Direction = Schemas["ImportRelationshipDirection"];

/** Attribute types a CI can be found by (§2.2): the same list as the server's `matchable`. */
export const MATCHABLE_TYPES: ReadonlySet<Attribute["dataType"]> = new Set(["text", "integer", "ip", "cidr"]);

export const MODES: { value: ImportMode; label: string }[] = [
  { value: "create_or_update", label: "Create new and update existing CIs" },
  { value: "create_only", label: "Only create new CIs" },
  { value: "update_only", label: "Only update existing CIs" },
];

export const DATE_FORMATS: ImportDateFormat[] = ["YYYY-MM-DD", "DD.MM.YYYY", "MM/DD/YYYY"];

/** A target as one select value: `ignore`, `ident`, `validFrom`, `validUntil`, `attr:<key>`, `rel:<typeKey>:<direction>`. */
export type TargetValue = string;

export interface TargetOption {
  value: TargetValue;
  label: string;
  /** Attribute targets: the definition. */
  attribute?: Attribute;
  /** Inherited attributes: the class that defines it. */
  definedBy?: string;
  /** Relationship targets. */
  type?: RelationshipType;
  direction?: Direction;
  /** Relationship targets: the classes the other CI may be in (rule classes and their subclasses). */
  otherClassIds?: string[];
}

export interface TargetGroups {
  core: TargetOption[];
  attributes: TargetOption[];
  relationships: TargetOption[];
}

function parents(classes: CiClass[]) {
  const byId = new Map(classes.map((c) => [c.id, c]));
  return (id: string): string[] => {
    const out: string[] = [];
    for (let c = byId.get(id); c; c = c.parentId ? byId.get(c.parentId) : undefined) out.push(c.id);
    return out;
  };
}

/** The class and every class below it. */
export function subtree(classes: CiClass[], id: string): string[] {
  const out = [id];
  for (let i = 0; i < out.length; i++) for (const c of classes) if (c.parentId === out[i]) out.push(c.id);
  return out;
}

/**
 * What a column can map to for this class (§1.2): core fields; every active attribute of the class and its
 * ancestors (the attributes endpoint already includes inherited ones); every active relationship type whose
 * rules allow the class as source (outgoing, forward label) or target (incoming, reverse label).
 */
export function targetGroups(input: {
  classId: string;
  classes: CiClass[];
  attributes: Attribute[];
  types: RelationshipType[];
  rules: RelationshipRule[];
}): TargetGroups {
  const { classId, classes, attributes, types, rules } = input;
  const names = new Map(classes.map((c) => [c.id, c.name]));
  const mine = new Set(parents(classes)(classId));

  const attributeOptions = attributes
    .filter((a) => a.isActive)
    .map<TargetOption>((a) => ({
      value: `attr:${a.key}`,
      label: a.label,
      attribute: a,
      definedBy: a.classId !== classId ? names.get(a.classId) : undefined,
    }));

  const relationships: TargetOption[] = [];
  for (const t of types) {
    if (!t.isActive) continue;
    for (const direction of ["outgoing", "incoming"] as const) {
      const other = new Set<string>();
      for (const r of rules) {
        if (r.relationshipTypeId !== t.id) continue;
        const [near, far] = direction === "outgoing" ? [r.sourceClassId, r.targetClassId] : [r.targetClassId, r.sourceClassId];
        if (mine.has(near)) for (const id of subtree(classes, far)) other.add(id);
      }
      if (other.size === 0) continue;
      // A symmetric type reads the same both ways: offer it once.
      if (direction === "incoming" && !t.isDirectional && relationships.some((o) => o.type?.id === t.id)) continue;
      relationships.push({
        value: `rel:${t.key}:${direction}`,
        label: direction === "outgoing" ? t.forwardLabel : t.reverseLabel,
        type: t,
        direction,
        otherClassIds: [...other],
      });
    }
  }

  return {
    core: [
      { value: "ident", label: "Ident" },
      { value: "validFrom", label: "Valid from" },
      { value: "validUntil", label: "Valid until" },
    ],
    attributes: attributeOptions,
    relationships,
  };
}

export function optionIndex(groups: TargetGroups): Map<TargetValue, TargetOption> {
  return new Map([...groups.core, ...groups.attributes, ...groups.relationships].map((o) => [o.value, o]));
}

/** The form state of one file column. Empty strings mean "use the job's setting". */
export interface ColumnForm {
  target: TargetValue;
  matchBy: MatchBy;
  matchAttribute: string;
  emptyCells: "" | ImportEmptyCells;
  decimalSeparator: "" | "." | ",";
  dateFormat: "" | ImportDateFormat;
  timeZone: string;
}

export interface MappingForm {
  classKey: string;
  mode: ImportMode;
  /** `ident` or `attributes.<key>`; empty for none. */
  keyField: string;
  emptyCells: ImportEmptyCells;
  trim: boolean;
  decimalSeparator: "." | ",";
  dateFormat: ImportDateFormat;
  timeZone: string;
  listSeparator: string;
  /** One per file column, in file order. */
  columns: ColumnForm[];
}

export const blankColumn = (): ColumnForm => ({
  target: "ignore",
  matchBy: "label",
  matchAttribute: "",
  emptyCells: "",
  decimalSeparator: "",
  dateFormat: "",
  timeZone: "",
});

/** The browser's time zone, sent explicitly (§1.2), else UTC. */
export function browserTimeZone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
  } catch {
    return "UTC";
  }
}

/** A fresh form: nothing mapped. `;`-delimited files default to the decimal comma (§1.2). */
export function blankForm(columnCount: number, delimiter: string | null | undefined, classKey = ""): MappingForm {
  return {
    classKey,
    mode: "create_or_update",
    keyField: "",
    emptyCells: "ignore",
    trim: true,
    decimalSeparator: delimiter === ";" ? "," : ".",
    dateFormat: "YYYY-MM-DD",
    timeZone: browserTimeZone(),
    listSeparator: ";",
    columns: Array.from({ length: columnCount }, blankColumn),
  };
}

export function targetValue(t: ImportColumnTarget): TargetValue {
  switch (t.kind) {
    case "attribute":
      return `attr:${t.key}`;
    case "relationship":
      return `rel:${t.typeKey}:${t.direction}`;
    default:
      return t.kind;
  }
}

/** The form for a mapping the job already has (a reload, or back from step 3). */
export function formFromMapping(m: ImportJobMapping, columnCount: number, delimiter: string | null | undefined): MappingForm {
  const form = blankForm(columnCount, delimiter, m.classKey);
  form.mode = m.mode;
  form.keyField = m.key?.field ?? "";
  form.emptyCells = m.emptyCells ?? "ignore";
  form.trim = m.options?.trim ?? true;
  if (m.options?.decimalSeparator === "," || m.options?.decimalSeparator === ".") form.decimalSeparator = m.options.decimalSeparator;
  form.dateFormat = m.options?.dateFormat ?? form.dateFormat;
  form.timeZone = m.options?.timeZone ?? form.timeZone;
  form.listSeparator = m.options?.listSeparator ?? form.listSeparator;
  for (const c of m.columns) {
    const col = form.columns[c.index];
    if (!col) continue;
    col.target = targetValue(c.target);
    const match = "match" in c.target ? c.target.match : undefined;
    if (match) {
      col.matchBy = match.by;
      col.matchAttribute = match.attributeKey ?? "";
    }
    col.emptyCells = c.emptyCells ?? "";
    const sep = c.options?.decimalSeparator;
    col.decimalSeparator = sep === "," || sep === "." ? sep : "";
    col.dateFormat = c.options?.dateFormat ?? "";
    col.timeZone = c.options?.timeZone ?? "";
  }
  return form;
}

/** Whether the target needs "Find the other CI by". */
export function needsMatch(o: TargetOption | undefined): boolean {
  return !!o && (!!o.type || o.attribute?.dataType === "reference");
}

/**
 * The body of PUT /imports/{id}/mapping. Every file column is listed, in file order, so the API's
 * `columns[i]` is file column i and its errors land on the right table row.
 */
export function toMapping(form: MappingForm, options: Map<TargetValue, TargetOption>): ImportJobMapping {
  return {
    classKey: form.classKey,
    mode: form.mode,
    key: form.keyField ? { field: form.keyField } : null,
    emptyCells: form.emptyCells,
    options: {
      trim: form.trim,
      decimalSeparator: form.decimalSeparator,
      dateFormat: form.dateFormat,
      timeZone: form.timeZone,
      listSeparator: form.listSeparator,
    },
    columns: form.columns.map((c, index) => {
      const o = options.get(c.target);
      const match = needsMatch(o)
        ? { by: c.matchBy, attributeKey: c.matchBy === "attribute" ? c.matchAttribute || null : null }
        : null;
      let target: ImportColumnTarget;
      if (o?.attribute) target = { kind: "attribute", key: o.attribute.key, match };
      else if (o?.type && o.direction && match) target = { kind: "relationship", typeKey: o.type.key, direction: o.direction, match };
      else if (c.target === "ident" || c.target === "validFrom" || c.target === "validUntil") target = { kind: c.target };
      else target = { kind: "ignore" };
      const dataType = o?.attribute?.dataType;
      const colOptions = {
        ...(c.decimalSeparator && dataType === "number" ? { decimalSeparator: c.decimalSeparator } : {}),
        ...(c.dateFormat && dataType === "date" ? { dateFormat: c.dateFormat } : {}),
        ...(c.timeZone && dataType === "datetime" ? { timeZone: c.timeZone } : {}),
      };
      return {
        index,
        target,
        emptyCells: target.kind === "ignore" ? null : c.emptyCells || null,
        options: Object.keys(colOptions).length ? colOptions : null,
      };
    }),
  };
}

/** A problem found in the mapping: `column` is the file column it belongs to, else it is about the whole mapping. */
export interface MappingProblem {
  column?: number;
  /** For problems about the whole mapping: the id of the control to focus. */
  control?: string;
  message: string;
}

/**
 * The checks worth making before saving (§1.2), so the common mistakes show at once. The server makes the same
 * checks and more; its answer is placed with `placeApiErrors`.
 */
export function checkMapping(
  form: MappingForm,
  options: Map<TargetValue, TargetOption>,
  attributes: Attribute[],
  isAdministrator: boolean,
): MappingProblem[] {
  const problems: MappingProblem[] = [];
  const firstColumn = new Map<TargetValue, number>();
  form.columns.forEach((c, i) => {
    if (c.target === "ignore") return;
    const first = firstColumn.get(c.target);
    if (first !== undefined) {
      problems.push({ column: i, message: `Mapped twice: column ${first + 1} already maps to ${options.get(c.target)?.label ?? c.target}.` });
    } else {
      firstColumn.set(c.target, i);
    }
    if (c.target === "ident" && form.mode !== "update_only" && !isAdministrator) {
      problems.push({ column: i, message: "Only administrators can set the ident of new CIs. Map it only to match existing CIs." });
    }
    if (needsMatch(options.get(c.target)) && c.matchBy === "attribute" && !c.matchAttribute) {
      problems.push({ column: i, message: "Choose the attribute to find the other CI by." });
    }
  });

  if (form.mode !== "create_only") {
    if (!form.keyField) {
      problems.push({ control: "import-key", message: "Choose how rows find existing CIs (Match existing CIs by)." });
    } else {
      const keyTarget = form.keyField === "ident" ? "ident" : `attr:${form.keyField.replace(/^attributes\./, "")}`;
      if (!firstColumn.has(keyTarget)) {
        const label = form.keyField === "ident" ? "Ident" : (options.get(keyTarget)?.label ?? form.keyField);
        problems.push({ control: "import-key", message: `The match key ${label} is not mapped to a column.` });
      }
    }
  }

  if (form.mode !== "update_only") {
    for (const a of attributes) {
      if (!a.isActive || !a.isRequired || a.defaultValue != null) continue;
      if (!firstColumn.has(`attr:${a.key}`)) problems.push({ control: "import-mapping-table", message: `${a.label} is required; map a column to it.` });
    }
  }
  return problems;
}

const COLUMN_FIELD = /^columns\[(\d+)\]/;

/** The control a whole-mapping field error belongs to. */
function controlOf(field: string): string | undefined {
  if (field === "classKey") return "import-class";
  if (field === "mode") return "import-mode";
  if (field === "key" || field.startsWith("key.")) return "import-key";
  if (field.startsWith("options.")) return `import-${field.slice("options.".length)}`;
  if (field.startsWith("attributes.")) return "import-mapping-table";
  return undefined;
}

/** Where the 400's problems belong: `columns[i].…` on file column i (every column is sent, in order), the rest above the table. */
export function placeApiErrors(details: ApiErrorDetail[]): MappingProblem[] {
  return details.map((d) => {
    const m = COLUMN_FIELD.exec(d.field ?? "");
    if (m) return { column: Number(m[1]), message: d.message };
    return { control: controlOf(d.field ?? ""), message: d.message };
  });
}

/** "old → new" for a planned change. */
export function changeText(v: unknown): string {
  if (v === null || v === undefined || v === "") return "(empty)";
  if (typeof v === "string") return v;
  if (typeof v === "number" || typeof v === "boolean") return String(v);
  return JSON.stringify(v);
}

/** Headers compared as the server compares them (§3.3): NFKC, lower case, trimmed, runs of space, `_`, `-`, `.` as one space. */
export function normaliseHeader(h: string): string {
  return h
    .normalize("NFKC")
    .toLowerCase()
    .replace(/[\s_.-]+/g, " ")
    .trim();
}

/**
 * A saved mapping's definition (D9): the job's mapping without its class, with every column named by its header,
 * `ignore` included, so a file with the same header set gets it suggested (§3.3).
 */
export function toDefinition(mapping: ImportJobMapping, headers: string[]): ImportMappingDefinition {
  return {
    mode: mapping.mode,
    key: mapping.key ?? null,
    emptyCells: mapping.emptyCells,
    options: mapping.options,
    columns: mapping.columns
      .filter((c) => headers[c.index] !== undefined && headers[c.index] !== "")
      .map((c) => ({ header: headers[c.index]!, target: c.target, emptyCells: c.emptyCells ?? null, options: c.options ?? null })),
  };
}

/**
 * The mapping of an earlier job, moved onto a new file's columns by header ("Upload a corrected file"). Columns
 * the new file does not have are dropped; the new file's extra columns stay unmapped.
 */
export function remapByHeaders(mapping: ImportJobMapping, oldHeaders: string[], newHeaders: string[]): ImportJobMapping {
  const at = new Map<string, number>();
  newHeaders.forEach((h, i) => {
    const k = normaliseHeader(h);
    if (!at.has(k)) at.set(k, i);
  });
  const columns = mapping.columns.flatMap((c) => {
    const h = oldHeaders[c.index];
    const index = h === undefined ? undefined : at.get(normaliseHeader(h));
    return index === undefined ? [] : [{ ...c, index }];
  });
  return { ...mapping, columns };
}

/** How a column got its target in the server's suggestion, as the Status column says it (§1.2). */
export function matchedText(via: Schemas["ImportMatchVia"] | null | undefined, hint: string | null | undefined): string {
  switch (via) {
    case "key":
      return "Matched by key";
    case "label":
      return "Matched by name";
    case "saved_mapping":
      return "From saved mapping";
  }
  switch (hint) {
    case "ambiguous_label":
      return "Not mapped: several fields have this name";
    case "duplicate_target":
      return "Not mapped: an earlier column has this field";
    case "ident_admin_only":
      return "Not mapped: only administrators can set the ident of new CIs";
  }
  return "Not mapped";
}
