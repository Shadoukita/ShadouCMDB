// The workflow designer's editable model (Administration › Workflows). Pure functions, no Vue: the
// designer edits a `Draft`, sends `toDraftBody(draft)` as the whole-graph PUT, and places the
// lint's problems (paths into that body) on the state or transition they are about.
import type {
  StateCategory,
  WorkflowDraftBody,
  WorkflowGrants,
  WorkflowProblem,
  WorkflowState,
  WorkflowTransition,
  WorkflowVersion,
} from "../api/workflows";
import { checkSetAttributes, setAttributeFromApi, setAttributeToApi, type DraftSetAttribute } from "./workflowActions";
import { checkSteps, stepFromApi, stepToApi, type DraftApprovalStep } from "./workflowApprovals";
import { t, type MessageKey } from "../i18n/index";

export type ConditionOp = "eq" | "ne" | "in" | "notIn" | "isSet" | "isNotSet" | "gt" | "gte" | "lt" | "lte" | "contains";
export type FieldDataType = "text" | "number" | "integer" | "boolean" | "enum" | "date" | "datetime" | "ip" | "cidr" | "reference" | "lookup";

export interface ConditionLeaf {
  kind: "leaf";
  field: string;
  op: ConditionOp;
  /** Absent for isSet/isNotSet, an array for in/notIn, else one value of the field's type. */
  value?: unknown;
}
export interface ConditionGroup {
  kind: "group";
  mode: "all" | "any";
  children: ConditionNode[];
}
export type ConditionNode = ConditionLeaf | ConditionGroup;

export interface DraftTransition extends Omit<WorkflowTransition, "conditions" | "fields" | "requiresComment" | "approval" | "setAttributes"> {
  requiresComment: boolean;
  fields: { attribute: string; required: boolean }[];
  /** The root group; no children means no condition. */
  conditions: ConditionGroup;
  /** The approval policy's steps, in order; none means the transition runs without approval. */
  approval: DraftApprovalStep[];
  /** Attribute actions: fields the transition sets on the CI when it runs; none means it sets nothing. */
  setAttributes: DraftSetAttribute[];
}
export interface DraftState extends Omit<WorkflowState, "terminal" | "stateValue"> {
  terminal: boolean;
  stateValue: string | null;
}
export interface Position {
  x: number;
  y: number;
}
export interface Draft {
  initialState: string | null;
  states: DraftState[];
  transitions: DraftTransition[];
  /** Designer node positions by state key; saved as the draft's `layout` (`{ key: { x, y } }`, design §4), outside its checksum. */
  positions: Record<string, Position>;
}

/** The API's limits (WorkflowTransition.conditions): nesting depth and leaves. */
export const MAX_CONDITION_DEPTH = 4;
export const MAX_CONDITION_LEAVES = 32;

export const CATEGORIES: { value: StateCategory; label: MessageKey }[] = [
  { value: "open", label: "wfRun.category.open" },
  { value: "active", label: "wfRun.category.active" },
  { value: "done", label: "wfRun.category.done" },
  { value: "cancelled", label: "wfRun.category.cancelled" },
];

/** A state category in the active locale; an unknown one as its key. */
export const categoryLabel = (c: string) => {
  const k = CATEGORIES.find((x) => x.value === c)?.label;
  return k ? t(k) : c;
};

export const OP_LABELS: Record<ConditionOp, string> = {
  eq: "is",
  ne: "is not",
  in: "is one of",
  notIn: "is none of",
  isSet: "is set",
  isNotSet: "is not set",
  gt: "is greater than",
  gte: "is at least",
  lt: "is less than",
  lte: "is at most",
  contains: "contains",
};

const ORDERED: FieldDataType[] = ["number", "integer", "date", "datetime"];

/** The operators the API accepts for a field of this type (§3.3: ordering only on numbers and dates, contains only on text). */
export function opsFor(dataType: FieldDataType): ConditionOp[] {
  const ops: ConditionOp[] = ["eq", "ne", "in", "notIn", "isSet", "isNotSet"];
  if (ORDERED.includes(dataType)) ops.push("gt", "gte", "lt", "lte");
  if (dataType === "text") ops.push("contains");
  return ops;
}

export function opTakesValue(op: ConditionOp): boolean {
  return op !== "isSet" && op !== "isNotSet";
}

export function opTakesList(op: ConditionOp): boolean {
  return op === "in" || op === "notIn";
}

/** A fresh value for a leaf after its field or operator changed: the old one rarely fits. */
export function defaultValue(dataType: FieldDataType, op: ConditionOp): unknown {
  if (!opTakesValue(op)) return undefined;
  if (opTakesList(op)) return [];
  if (dataType === "boolean") return true;
  return "";
}

// ---------- Conditions: API JSON <-> editor tree ----------

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function parseNode(v: unknown): ConditionNode {
  if (isRecord(v) && Array.isArray(v.all)) return { kind: "group", mode: "all", children: v.all.map(parseNode) };
  if (isRecord(v) && Array.isArray(v.any)) return { kind: "group", mode: "any", children: v.any.map(parseNode) };
  const o = isRecord(v) ? v : {};
  const leaf: ConditionLeaf = { kind: "leaf", field: String(o.field ?? ""), op: (o.op as ConditionOp) ?? "eq" };
  if ("value" in o) leaf.value = o.value;
  return leaf;
}

/** The stored condition as the editor's root group: a lone leaf becomes the only child of an "all" group. */
export function parseConditions(v: unknown): ConditionGroup {
  if (v === undefined || v === null) return { kind: "group", mode: "all", children: [] };
  const node = parseNode(v);
  return node.kind === "group" ? node : { kind: "group", mode: "all", children: [node] };
}

function nodeToJson(n: ConditionNode): Record<string, unknown> {
  if (n.kind === "group") return { [n.mode]: n.children.map(nodeToJson) };
  const out: Record<string, unknown> = { field: n.field, op: n.op };
  if (opTakesValue(n.op)) out.value = n.value;
  return out;
}

/** The API body of a root group; `undefined` (no condition) when it is empty. Empty inner groups are dropped. */
export function conditionsToJson(root: ConditionGroup): Record<string, unknown> | undefined {
  const prune = (g: ConditionGroup): ConditionGroup => ({
    ...g,
    children: g.children.map((c) => (c.kind === "group" ? prune(c) : c)).filter((c) => c.kind === "leaf" || c.children.length > 0),
  });
  const pruned = prune(root);
  return pruned.children.length ? nodeToJson(pruned) : undefined;
}

export function countLeaves(n: ConditionNode): number {
  return n.kind === "leaf" ? 1 : n.children.reduce((sum, c) => sum + countLeaves(c), 0);
}

/** One plain-language line per condition, for the transition table: `Environment is Production and …`. */
export function describeConditions(n: ConditionNode, label: (field: string) => string = (f) => f): string {
  if (n.kind === "group") {
    const parts = n.children.map((c) => (c.kind === "group" && c.children.length > 1 ? `(${describeConditions(c, label)})` : describeConditions(c, label)));
    return parts.join(n.mode === "all" ? " and " : " or ");
  }
  const v = !opTakesValue(n.op) ? "" : Array.isArray(n.value) ? ` ${n.value.join(", ")}` : ` ${String(n.value ?? "")}`;
  return `${label(n.field)} ${OP_LABELS[n.op] ?? n.op}${v}`;
}

// ---------- Draft <-> API ----------

function layoutPositions(layout: unknown): Record<string, Position> {
  const out: Record<string, Position> = {};
  for (const [key, p] of Object.entries(isRecord(layout) ? layout : {})) {
    if (isRecord(p) && typeof p.x === "number" && typeof p.y === "number" && Number.isFinite(p.x) && Number.isFinite(p.y)) {
      out[key] = { x: p.x, y: p.y };
    }
  }
  return out;
}

export function draftFromVersion(v: Pick<WorkflowVersion, "initialState" | "states" | "transitions" | "layout">): Draft {
  return {
    initialState: v.initialState,
    states: v.states.map((s) => ({ key: s.key, name: s.name, category: s.category, terminal: s.terminal ?? false, stateValue: s.stateValue ?? null })),
    transitions: v.transitions.map((t) => ({
      key: t.key,
      name: t.name,
      from: t.from,
      to: t.to,
      requiresComment: t.requiresComment ?? false,
      fields: (t.fields ?? []).map((f) => ({ attribute: f.attribute, required: f.required ?? true })),
      conditions: parseConditions(t.conditions),
      approval: (t.approval?.steps ?? []).map(stepFromApi),
      setAttributes: (t.setAttributes ?? []).map(setAttributeFromApi),
    })),
    positions: layoutPositions(v.layout),
  };
}

export function emptyDraft(): Draft {
  return { initialState: null, states: [], transitions: [], positions: {} };
}

/** The whole-graph PUT body. Positions of states that no longer exist are not kept. */
export function toDraftBody(d: Draft, expectedChecksum?: string | null): WorkflowDraftBody {
  const keys = new Set(d.states.map((s) => s.key));
  const positions: Record<string, Position> = Object.fromEntries(Object.entries(d.positions).filter(([k]) => keys.has(k)));
  const body: WorkflowDraftBody = {
    initialState: d.initialState && keys.has(d.initialState) ? d.initialState : null,
    states: d.states.map((s) => ({ key: s.key, name: s.name, category: s.category, terminal: s.terminal, stateValue: s.stateValue || null })),
    transitions: d.transitions.map((t) => {
      const out: WorkflowTransition = { key: t.key, name: t.name, from: t.from, to: t.to, requiresComment: t.requiresComment };
      if (t.fields.length) out.fields = t.fields.map((f) => ({ attribute: f.attribute, required: f.required }));
      const c = conditionsToJson(t.conditions);
      // Free-form objects in the spec generate as Record<string, never>.
      if (c) out.conditions = c as WorkflowTransition["conditions"];
      if (t.approval.length) out.approval = { steps: t.approval.map(stepToApi) };
      if (t.setAttributes.length) out.setAttributes = t.setAttributes.map(setAttributeToApi);
      return out;
    }),
    layout: positions as unknown as WorkflowDraftBody["layout"],
  };
  if (expectedChecksum) body.expectedChecksum = expectedChecksum;
  return body;
}

/** Fingerprint of what a save would send (layout included), to tell unsaved edits apart. */
export function draftFingerprint(d: Draft): string {
  return JSON.stringify(toDraftBody(d));
}

// ---------- Edits ----------

/** `base`, or `base_2`, `base_3`… the first one not taken. */
export function uniqueKey(base: string, taken: Iterable<string>): string {
  const set = new Set(taken);
  const b = base || "item";
  if (!set.has(b)) return b;
  for (let i = 2; ; i++) {
    const k = `${b.slice(0, 60)}_${i}`;
    if (!set.has(k)) return k;
  }
}

/** Renames a state's key everywhere it is referenced: transitions, the initial state and its position. */
export function renameState(d: Draft, from: string, to: string): void {
  if (from === to) return;
  for (const s of d.states) if (s.key === from) s.key = to;
  for (const t of d.transitions) {
    if (t.from === from) t.from = to;
    if (t.to === from) t.to = to;
  }
  if (d.initialState === from) d.initialState = to;
  if (d.positions[from]) {
    d.positions[to] = d.positions[from];
    delete d.positions[from];
  }
}

/** Removes a state and every transition into or out of it. Returns the keys of the removed transitions. */
export function removeState(d: Draft, key: string): string[] {
  const gone = d.transitions.filter((t) => t.from === key || t.to === key).map((t) => t.key);
  d.states = d.states.filter((s) => s.key !== key);
  d.transitions = d.transitions.filter((t) => t.from !== key && t.to !== key);
  if (d.initialState === key) d.initialState = null;
  delete d.positions[key];
  return gone;
}

export const NODE_W = 168;
export const NODE_H = 56;
/** Room between columns for a transition's label (EDGE_LABEL_MAX characters) clear of both boxes. */
const GAP_X = 144;
/** From one column of the arrangement to the next. */
export const COLUMN_STEP = NODE_W + GAP_X;
const GAP_Y = 64;

/**
 * Gives every state without a position one: columns by distance from the initial state (breadth
 * first), states not reached from it in a last column. Placed states keep theirs.
 */
export function autoLayout(d: Draft, all = false): void {
  const depth = new Map<string, number>();
  const start = d.initialState && d.states.some((s) => s.key === d.initialState) ? d.initialState : d.states[0]?.key;
  if (start) {
    depth.set(start, 0);
    const queue = [start];
    while (queue.length) {
      const k = queue.shift()!;
      for (const t of d.transitions) {
        if (t.from === k && !depth.has(t.to) && d.states.some((s) => s.key === t.to)) {
          depth.set(t.to, depth.get(k)! + 1);
          queue.push(t.to);
        }
      }
    }
  }
  const maxDepth = Math.max(-1, ...depth.values());
  const rows = new Map<number, number>();
  for (const s of d.states) {
    if (!all && d.positions[s.key]) continue;
    const col = depth.get(s.key) ?? maxDepth + 1;
    const row = rows.get(col) ?? 0;
    rows.set(col, row + 1);
    d.positions[s.key] = { x: 40 + col * COLUMN_STEP, y: 24 + row * (NODE_H + GAP_Y) };
  }
}

// ---------- Lint problems ----------

export type ProblemTarget = { kind: "state"; key: string } | { kind: "transition"; key: string } | { kind: "graph" };

/**
 * What a problem's path (`states[2].stateValue`, `transitions[0].fields[1].attribute`) is about,
 * resolved against the body that was saved: indices are of that body, keys survive later edits.
 */
export function problemTarget(path: string, saved: Pick<WorkflowDraftBody, "states" | "transitions">): ProblemTarget {
  const m = /^(states|transitions)\[(\d+)\]/.exec(path);
  if (m) {
    const i = Number(m[2]);
    const key = m[1] === "states" ? saved.states[i]?.key : saved.transitions[i]?.key;
    if (key) return { kind: m[1] === "states" ? "state" : "transition", key };
  }
  return { kind: "graph" };
}

export interface PlacedProblem extends Pick<WorkflowProblem, "code" | "message" | "severity" | "path"> {
  target: ProblemTarget;
}

export function placeProblems(problems: Pick<WorkflowProblem, "code" | "message" | "severity" | "path">[], saved: Pick<WorkflowDraftBody, "states" | "transitions">): PlacedProblem[] {
  return problems.map((p) => ({ ...p, target: problemTarget(p.path, saved) }));
}

/** Problems about one state or transition, errors first. */
export function problemsFor(problems: PlacedProblem[], kind: "state" | "transition", key: string): PlacedProblem[] {
  return problems
    .filter((p) => p.target.kind === kind && p.target.key === key)
    .sort((a, b) => (a.severity === b.severity ? 0 : a.severity === "error" ? -1 : 1));
}

// ---------- Grants ----------

export const CANCEL_GRANT = "_cancel";
/** Starting the workflow again on a CI where an instance of it ended (GH#666). */
export const START_GRANT = "_start";
/** Grant keys that are not transitions: rows of their own, never orphans. */
export const PSEUDO_GRANTS: readonly string[] = [START_GRANT, CANCEL_GRANT];

/**
 * The rows of the grants matrix: the transitions of the draft and of the current version (by key,
 * draft names first), then keys only older versions or stored grants still name, then starting again
 * and cancelling.
 */
export function grantRows(
  sources: (Pick<WorkflowTransition, "key" | "name"> & { from?: string; to?: string })[][],
  grants: WorkflowGrants["grants"],
): { key: string; name: string; orphan: boolean }[] {
  const out = new Map<string, { key: string; name: string; orphan: boolean }>();
  for (const list of sources) for (const t of list) if (!out.has(t.key)) out.set(t.key, { key: t.key, name: t.name, orphan: false });
  for (const g of grants) {
    if (!PSEUDO_GRANTS.includes(g.transitionKey) && !out.has(g.transitionKey)) out.set(g.transitionKey, { key: g.transitionKey, name: g.transitionKey, orphan: true });
  }
  out.set(START_GRANT, { key: START_GRANT, name: "Start again after an instance ended", orphan: false });
  out.set(CANCEL_GRANT, { key: CANCEL_GRANT, name: "Cancel an instance", orphan: false });
  return [...out.values()];
}

/** transition key -> granted profile ids. */
export function grantSets(grants: WorkflowGrants["grants"]): Map<string, Set<string>> {
  return new Map(grants.map((g) => [g.transitionKey, new Set(g.profiles.map((p) => p.id))]));
}

/** The PUT body's grants from the matrix: rows without a profile are left out. */
export function grantsBody(sets: Map<string, Set<string>>): { transitionKey: string; profiles: string[] }[] {
  return [...sets.entries()].filter(([, ids]) => ids.size > 0).map(([transitionKey, ids]) => ({ transitionKey, profiles: [...ids].sort() }));
}

// ---------- Client-side checks before a save ----------

const KEY = /^[a-z][a-z0-9_]{0,62}$/;

/**
 * What the PUT would refuse for its shape (keys, names, ends, incomplete conditions), found before
 * sending: the designer saves only a draft that passes these, and shows them where the lint's
 * problems go. Whether the graph can be published stays the lint's question.
 */
export function checkDraft(d: Draft): PlacedProblem[] {
  const out: PlacedProblem[] = [];
  const add = (target: ProblemTarget, path: string, message: string, code = "invalid") => out.push({ target, path, message, code, severity: "error" });
  const stateKeys = new Set<string>();
  d.states.forEach((s, i) => {
    const target: ProblemTarget = { kind: "state", key: s.key };
    if (!KEY.test(s.key)) add(target, `states[${i}].key`, "The key must be lower-case letters, digits and _, starting with a letter (max 63).");
    else if (stateKeys.has(s.key)) add(target, `states[${i}].key`, `Another state already has the key ${s.key}.`, "duplicate");
    stateKeys.add(s.key);
    if (!s.name.trim()) add(target, `states[${i}].name`, "The state needs a name.", "required");
  });
  const transitionKeys = new Set<string>();
  d.transitions.forEach((t, i) => {
    const target: ProblemTarget = { kind: "transition", key: t.key };
    if (!KEY.test(t.key)) add(target, `transitions[${i}].key`, "The key must be lower-case letters, digits and _, starting with a letter (max 63).");
    else if (transitionKeys.has(t.key)) add(target, `transitions[${i}].key`, `Another transition already has the key ${t.key}.`, "duplicate");
    transitionKeys.add(t.key);
    if (!t.name.trim()) add(target, `transitions[${i}].name`, "The transition needs a name.", "required");
    if (!stateKeys.has(t.from)) add(target, `transitions[${i}].from`, "Choose the state it starts from.", "required");
    if (!stateKeys.has(t.to)) add(target, `transitions[${i}].to`, "Choose the state it leads to.", "required");
    else if (t.from === t.to) add(target, `transitions[${i}].to`, "A transition must lead to another state.", "self_loop");
    const fields = new Set<string>();
    t.fields.forEach((f, j) => {
      if (!f.attribute) add(target, `transitions[${i}].fields[${j}].attribute`, "Choose a field or remove the row.", "required");
      else if (fields.has(f.attribute)) add(target, `transitions[${i}].fields[${j}].attribute`, `The field ${f.attribute} is listed twice.`, "duplicate");
      fields.add(f.attribute);
    });
    const leaves = countLeaves(t.conditions);
    if (leaves > MAX_CONDITION_LEAVES) add(target, `transitions[${i}].conditions`, `At most ${MAX_CONDITION_LEAVES} conditions.`, "too_many_leaves");
    const walk = (n: ConditionNode, path: string) => {
      if (n.kind === "group") return n.children.forEach((c, j) => walk(c, `${path}.${n.mode}[${j}]`));
      if (!n.field) return add(target, `${path}.field`, "A condition has no field: choose one or remove it.", "required");
      if (!opTakesValue(n.op)) return;
      const v = n.value;
      const empty = v === undefined || v === null || v === "" || (Array.isArray(v) && v.length === 0) || (typeof v === "number" && !Number.isFinite(v));
      if (empty) add(target, `${path}.value`, `The condition on ${n.field} needs a value.`, "required");
    };
    walk(t.conditions, `transitions[${i}].conditions`);
    for (const p of checkSteps(t.approval)) add(target, `transitions[${i}].${p.path}`, p.message, p.code);
    for (const p of checkSetAttributes(t.setAttributes)) add(target, `transitions[${i}].${p.path}`, p.message, p.code);
  });
  return out;
}
