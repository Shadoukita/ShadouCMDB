import type { AttributeReference, AuditEntry, Ci } from "../api/queries";

/**
 * A CI as it was at a past moment, and the net difference between two moments, rebuilt from its audit
 * history (SHAA-3131): start from the current CI and walk its entries newer than the moment backwards,
 * newest first, each one putting back the values it replaced. Most changes record the whole API
 * representation before and after; a workflow's attribute action records only the attributes it set, and a
 * few old or special entries record something else. Nothing is guessed: a value no entry vouches for reads
 * "not recorded", a withheld entry or a reference into a class the reader may not view reads "hidden", a
 * moment before the CI existed or before the history retention kept says so instead of showing values.
 */

/** At most this many audit entries are walked for one view (10 pages of 200); beyond it the view stops and says so. */
export const HISTORY_WALK_LIMIT = 2000;
/** Page size of the walk: the audit log's maximum. */
export const HISTORY_WALK_PAGE = 200;

/** The history walked for a view: every audit entry of the CI from `since` on, newest first. */
export type HistoryWalk =
  | { complete: true; since: string; entries: AuditEntry[] }
  /** The CI has more than {@link HISTORY_WALK_LIMIT} entries from `since` on: they were not loaded. */
  | { complete: false; since: string; total: number };

/** One field at one moment. Lookups, references, the class and the criticality are ids; `name` is the recorded name. */
export type FieldValue =
  | { kind: "value"; value: string | number | boolean; name?: string }
  | { kind: "unset" }
  /** A reference into a class the reader may not view, or a withheld entry: the value is never kept. */
  | { kind: "hidden" }
  /** No entry tells the value at that moment. */
  | { kind: "notRecorded" };

/** The built-in fields a snapshot shows and compares, before the class's attributes. */
export const CORE_FIELDS = ["ident", "class", "criticality", "validFrom", "validUntil"] as const;
export type CoreField = (typeof CORE_FIELDS)[number];
/** A field's key: a built-in field, or `attributes.<key>`. */
export type FieldKey = CoreField | `attributes.${string}`;

/** The CI's fields at one moment. */
export type Snapshot = {
  fields: Partial<Record<FieldKey, FieldValue>>;
  /** Any field not in `fields`: unset when a whole representation was recorded, otherwise not recorded or hidden. */
  rest: FieldValue;
  /** The recorded reference targets of reference attributes (name as it was, hidden flag). */
  refs: Record<string, AttributeReference>;
  classId: string | null;
  label: string | null;
  /** Whether the CI was deleted at that moment; null when not recorded. */
  deleted: boolean | null;
};

/** The CI at one moment. */
export type PointState =
  | { kind: "snapshot"; snapshot: Snapshot }
  /** The moment is before the CI was created. */
  | { kind: "notYet"; createdAt: string }
  /** Audit retention removed the CI history before `before`, and the moment is earlier. */
  | { kind: "purged"; before: string }
  /** More entries than {@link HISTORY_WALK_LIMIT} would have to be walked. */
  | { kind: "tooMany"; total: number };

const ms = (iso: string) => Date.parse(iso);
const isObject = (v: unknown): v is Record<string, unknown> => !!v && typeof v === "object" && !Array.isArray(v);
const isScalar = (v: unknown): v is string | number | boolean => typeof v === "string" || typeof v === "number" || typeof v === "boolean";
const scalar = (v: unknown): FieldValue => (isScalar(v) && v !== "" ? { kind: "value", value: v } : { kind: "unset" });

export const fieldValue = (s: Snapshot, key: FieldKey): FieldValue => s.fields[key] ?? s.rest;

/** The fields of a whole CI representation (the current CI, or one an entry recorded). */
export function snapshotOf(ci: Ci): Snapshot {
  const fields: Snapshot["fields"] = {
    ident: scalar(ci.ident),
    class: ci.classId ? { kind: "value", value: ci.classId, ...(ci.class?.name ? { name: ci.class.name } : {}) } : { kind: "unset" },
    criticality: ci.criticality?.id ? { kind: "value", value: ci.criticality.id, name: ci.criticality.name } : { kind: "unset" },
    validFrom: scalar(ci.validFrom),
    validUntil: scalar(ci.validUntil),
  };
  const refs: Snapshot["refs"] = {};
  for (const [k, v] of Object.entries(ci.attributes ?? {})) fields[`attributes.${k}`] = scalar(v);
  // The audit log keeps the id of a reference into a class the reader may not view: never pass it on.
  for (const [k, r] of Object.entries(ci.attributeReferences ?? {})) {
    if (r?.hidden) fields[`attributes.${k}`] = { kind: "hidden" };
    else if (r) refs[k] = r;
  }
  return { fields, rest: { kind: "unset" }, refs, classId: ci.classId ?? null, label: ci.label ?? null, deleted: !!ci.deletedAt };
}

/** A recorded value that is a whole CI representation (as GET /configuration-items/{id} answers it). */
function wholeCi(v: unknown): v is Ci {
  return isObject(v) && typeof v.id === "string" && typeof v.ident === "string" && typeof v.classId === "string" && isObject(v.attributes) && isObject(v.attributeReferences);
}

const unknownState = (rest: FieldValue): Snapshot => ({ fields: {}, rest, refs: {}, classId: null, label: null, deleted: null });

/** Only these entries change the CI's own fields; the others on its id (workflow steps, exports, layouts) are events. */
const isChange = (e: AuditEntry) => e.entityType === "configuration_items" && (e.action === "create" || e.action === "update" || e.action === "delete" || e.action === "restore");

/**
 * The state before `e`, given the state after it. A whole representation replaces everything; a workflow
 * attribute action puts back the attributes it set; a criticality move by the 0036 upgrade leaves the
 * criticality before it unknown; a business service's member or owner change touches no field. Anything else
 * (an entry in an older shape) leaves every earlier value not recorded.
 */
function before(e: AuditEntry, after: Snapshot): Snapshot {
  if (e.redacted) return unknownState({ kind: "hidden" });
  const old = e.oldValue;
  if (wholeCi(old)) return snapshotOf(old);
  if (!isObject(old)) return unknownState({ kind: "notRecorded" });
  const keys = Object.keys(old).filter((k) => k !== "classId");
  if (keys.length > 0 && keys.every((k) => k === "members" || k === "owners")) return after;
  if (keys.length === 1 && keys[0] === "attributes" && isObject(old.attributes) && (!old.classId || old.classId === after.classId)) {
    const fields = { ...after.fields };
    // A partial entry carries no reference targets: the value is the id, named only through the API.
    for (const [k, v] of Object.entries(old.attributes)) fields[`attributes.${k}`] = after.fields[`attributes.${k}`]?.kind === "hidden" ? { kind: "hidden" } : scalar(v);
    const refs = { ...after.refs };
    for (const k of Object.keys(old.attributes)) delete refs[k];
    return { ...after, fields, refs };
  }
  if (keys.length === 1 && keys[0] === "criticality") return { ...after, fields: { ...after.fields, criticality: { kind: "notRecorded" } } };
  return unknownState({ kind: "notRecorded" });
}

/**
 * The CI at `at` (ISO 8601). `walk` must hold every entry from `at` on (or from a later `since` when the
 * moment is before the CI's creation or a purge); `purgedBefore` is the latest cutoff of an audit purge of
 * changes, if any. Changes made at exactly `at` count as made.
 */
export function stateAt(current: Ci, walk: HistoryWalk, at: string, purgedBefore: string | null): PointState {
  const t = ms(at);
  if (t < ms(current.createdAt)) return { kind: "notYet", createdAt: current.createdAt };
  // Entries after the cutoff are all kept, so a moment after it is complete; one before it may have lost changes.
  if (purgedBefore && t < ms(purgedBefore)) return { kind: "purged", before: purgedBefore };
  if (!walk.complete) return { kind: "tooMany", total: walk.total };
  const newer = walk.entries.filter((e) => isChange(e) && ms(e.occurredAt) > t);
  // Newest first; the list order breaks ties between entries of one request.
  newer.sort((a, b) => ms(b.occurredAt) - ms(a.occurredAt));
  let state = snapshotOf(current);
  for (const e of newer) {
    if (e.action === "create") return { kind: "notYet", createdAt: e.occurredAt };
    state = before(e, state);
  }
  return { kind: "snapshot", snapshot: state };
}

/**
 * The fields to show, in order: the built-in ones, then the attributes in the order of `attributeOrder` (the
 * class's definitions, retired ones too), then any other recorded attribute by key. `show`: attributes to list
 * even without a value (the class's current fields).
 */
export function fieldKeys(snapshots: readonly Snapshot[], attributeOrder: readonly string[], show: readonly string[] = []): FieldKey[] {
  const keys = new Set<string>(show);
  for (const s of snapshots) for (const k of Object.keys(s.fields)) if (k.startsWith("attributes.")) keys.add(k.slice("attributes.".length));
  const ordered = attributeOrder.filter((k) => keys.has(k));
  const rest = [...keys].filter((k) => !attributeOrder.includes(k)).sort();
  return [...CORE_FIELDS, ...[...ordered, ...rest].map((k) => `attributes.${k}` as const)];
}

const same = (a: FieldValue, b: FieldValue) => a.kind === b.kind && (a.kind !== "value" || (b.kind === "value" && a.value === b.value));

/** One field that differs between two moments. */
export type FieldChange = { key: FieldKey; from: FieldValue; to: FieldValue };

/**
 * The net per-field difference between two moments. Two hidden values are never compared: whether a value
 * the reader may not view changed is not theirs to learn either. Neither are two unrecorded ones.
 */
export function diffSnapshots(from: Snapshot, to: Snapshot, attributeOrder: readonly string[]): FieldChange[] {
  const out: FieldChange[] = [];
  for (const key of fieldKeys([from, to], attributeOrder)) {
    const a = fieldValue(from, key);
    const b = fieldValue(to, key);
    if (a.kind === b.kind && (a.kind === "hidden" || a.kind === "notRecorded")) continue;
    if (!same(a, b)) out.push({ key, from: a, to: b });
  }
  return out;
}

/** The workflow steps on the CI after `from` up to and including `to`, oldest first. */
export function workflowEventsBetween(walk: HistoryWalk, from: string, to: string): AuditEntry[] {
  if (!walk.complete) return [];
  const [a, b] = [ms(from), ms(to)];
  return walk.entries
    .filter((e) => e.action.startsWith("workflow.") && ms(e.occurredAt) > a && ms(e.occurredAt) <= b)
    .sort((x, y) => ms(x.occurredAt) - ms(y.occurredAt));
}

/** How many changes to the CI itself (create, update, delete, restore) were made after `from` up to `to`. */
export function changesBetween(walk: HistoryWalk, from: string, to: string): number {
  if (!walk.complete) return 0;
  const [a, b] = [ms(from), ms(to)];
  return walk.entries.filter((e) => isChange(e) && ms(e.occurredAt) > a && ms(e.occurredAt) <= b).length;
}

/**
 * The latest cutoff of an audit purge of changes (`audit.purge` rows with scope `changes`, which removes
 * create, update, delete, restore and workflow entries older than the cutoff), or null when there was none.
 */
export function purgeCutoff(purges: readonly AuditEntry[]): string | null {
  let latest: string | null = null;
  for (const p of purges) {
    if (p.action !== "audit.purge" || !isObject(p.newValue) || p.newValue.scope !== "changes") continue;
    const cutoff = p.newValue.cutoff;
    if (typeof cutoff !== "string" || Number.isNaN(ms(cutoff))) continue;
    if (!latest || ms(cutoff) > ms(latest)) latest = cutoff;
  }
  return latest;
}

/** Where a walk for the moment `at` must start: no entry before the CI's creation or a purge's cutoff is needed. */
export function walkStart(at: string, createdAt: string, purgedBefore: string | null): string {
  return new Date(Math.max(ms(at), ms(createdAt), purgedBefore ? ms(purgedBefore) : -Infinity)).toISOString();
}

/** A `datetime-local` value (`YYYY-MM-DDTHH:mm`) read as UTC, as ISO 8601; null when it is not one. */
export function utcInputToIso(value: string): string | null {
  if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(:\d{2})?$/.test(value)) return null;
  const t = Date.parse(`${value.length === 16 ? `${value}:00` : value}Z`);
  return Number.isNaN(t) ? null : new Date(t).toISOString();
}

/** An ISO 8601 time as a `datetime-local` value in UTC, to the minute. */
export function isoToUtcInput(iso: string): string {
  return new Date(iso).toISOString().slice(0, 16);
}
