// Workflow problems in the user's language (SHAA-3003, GH#870): the lint of a draft
// (POST …/draft/validate), a save or publish refusal (VALIDATION_ERROR details) and the approver
// lint come with a stable `code`, a `path` and, from the lint, `params` (the data the API's English
// `message` names). The text comes from `wfProblem.<code>`, optionally narrowed by what the path is
// about (`wfProblem.<code>.<context>`) and by the params at hand; the API's message stays the
// fallback for a code (or a combination of params) the catalog does not know.
import { en } from "../i18n/en";
import { hasMessage, messageParams, t, type MessageKey, type MessageParams } from "../i18n/index";

export type ProblemParams = Record<string, string | number>;

export interface ProblemLike {
  path: string;
  code: string;
  message: string;
  params?: ProblemParams;
  /** Already in the active locale (a check the designer made before sending): shown as is. */
  localized?: boolean;
  /** The state or transition the problem is about, when it was placed on one. */
  target?: { kind: "state" | "transition"; key: string } | { kind: "graph" };
}

/** What a path is about, most specific first: narrows `wfProblem.<code>` to `wfProblem.<code>.<context>`. */
const CONTEXTS: [RegExp, string][] = [
  [/^stateAttributeId$/, "stateField"],
  [/^states\[\d+\]\.stateValue$/, "stateValue"],
  [/\.setAttributes(\[|$)/, "action"],
  [/\.approval(\.|$)|^transitions\.[^.]+\.steps\.[^.]+/, "approval"],
  [/\.conditions(\.|\[|$)/, "condition"],
  [/\.fields\[\d+\]/, "transitionField"],
  [/^approvers\[/, "assignment"],
  [/^approvers$/, "approvers"],
  [/^layout$/, "layout"],
];
const CONTEXT_NAMES = new Set(CONTEXTS.map(([, c]) => c));

export function problemContext(path: string): string | undefined {
  return CONTEXTS.find(([re]) => re.test(path))?.[1];
}

const PROBLEM_KEYS = (Object.keys(en) as MessageKey[]).filter((k) => k.startsWith("wfProblem."));

/**
 * The message for `prefix` that the params can fill, the most specific first. Candidates: `prefix`
 * itself, `prefix.<param>` (only when that param is given, e.g. `.workflow` or `.refersTo`) and
 * `prefix.bare` (no data, the last resort). The one that uses the most params wins.
 */
function pick(prefix: string, given: Set<string>): MessageKey | undefined {
  let best: MessageKey | undefined;
  let score = -2;
  for (const key of PROBLEM_KEYS) {
    let variant: string | undefined;
    if (key === prefix) variant = undefined;
    else if (key.startsWith(`${prefix}.`) && !key.slice(prefix.length + 1).includes(".")) variant = key.slice(prefix.length + 1);
    else continue;
    if (variant && CONTEXT_NAMES.has(variant)) continue;
    if (variant && variant !== "bare" && !given.has(variant)) continue;
    const args = messageParams(key);
    if (!args.every((a) => given.has(a))) continue;
    const s = variant === "bare" ? -1 : new Set([...args, ...(variant ? [variant] : [])]).size;
    if (s > score) [best, score] = [key, s];
  }
  return best;
}

/** The message key a problem is worded with, or undefined when the API's message has to do. */
export function problemKey(p: ProblemLike): MessageKey | undefined {
  const given = new Set(Object.keys(allParams(p)));
  const ctx = problemContext(p.path);
  const prefixes = ctx ? [`wfProblem.${p.code}.${ctx}`, `wfProblem.${p.code}`] : [`wfProblem.${p.code}`];
  for (const prefix of prefixes) {
    const key = pick(prefix, given);
    if (key) return key;
  }
  return undefined;
}

/** The problem's params with the state or transition it was placed on, unless it names its own. */
function allParams(p: ProblemLike): ProblemParams {
  const out: ProblemParams = {};
  if (p.target?.kind === "state") out.state = p.target.key;
  if (p.target?.kind === "transition") out.transition = p.target.key;
  for (const [k, v] of Object.entries(p.params ?? {})) if (v !== "" && v !== null && v !== undefined) out[k] = v;
  return out;
}

const label = (key: string, fallback: string) => (hasMessage(key) ? t(key) : fallback);

/** Params as the message shows them: data types, comparisons and value sources in words. */
function shown(params: ProblemParams): MessageParams {
  const out: MessageParams = { ...params };
  if (typeof params.dataType === "string") out.dataType = label(`dm.attr.type.${params.dataType}`, params.dataType);
  if (typeof params.op === "string") out.op = label(`wfDesign.op.${params.op}`, params.op);
  if (typeof params.valueFrom === "string") out.valueFrom = label(`wfActions.set.mode.${params.valueFrom}`, params.valueFrom);
  if (typeof params.expected === "string") out.expected = label(`wfProblem.expected.${params.expected}`, params.expected);
  if (typeof params.approverKind === "string" && params.approver !== undefined) {
    const k = `wfProblem.approver.${params.approverKind}`;
    if (hasMessage(k)) out.approver = t(k, { name: params.approver });
  }
  return out;
}

/** A workflow problem in the active locale; the API's message for what the catalog cannot word. */
export function problemText(p: ProblemLike): string {
  if (p.localized) return p.message;
  const key = problemKey(p);
  return key ? t(key, shown(allParams(p))) : p.message;
}

// ---------- Params from the body that was sent ----------

/** The value at `path` (`transitions[0].fields[1].attribute`) in `body`, or undefined. */
export function valueAt(body: unknown, path: string): unknown {
  let node: unknown = body;
  for (const part of path.split(/\.|\[|\]/).filter(Boolean)) {
    if (node === null || typeof node !== "object") return undefined;
    node = (node as Record<string, unknown>)[part];
  }
  return node;
}

/**
 * What a refusal of `body` (a save's VALIDATION_ERROR details carry no params) is about, read from
 * the body at the problem's path: the field key a `….attribute` or a condition names, the state
 * value a `….stateValue` maps to, the key or state a `….key`/`….from`/`….to`/`initialState` gives,
 * and a condition's op and value.
 */
export function paramsFromBody(path: string, body: unknown): ProblemParams {
  const out: ProblemParams = {};
  const at = valueAt(body, path);
  const text = (v: unknown) => (typeof v === "string" || typeof v === "number" ? v : undefined);
  if (/\.attribute$/.test(path) && text(at) !== undefined) out.attribute = text(at)!;
  if (/\.stateValue$/.test(path) && text(at) !== undefined) out.value = text(at)!;
  if (/(\.key|\.from|\.to|^initialState|\.excludeActorsOf\[\d+\])$/.test(path) && text(at) !== undefined) out.given = text(at)!;
  const cond = /^(transitions\[\d+\]\.conditions(?:\.(?:all|any)\[\d+\])*)(\..*)?$/.exec(path);
  if (cond) {
    const leaf = valueAt(body, cond[1]) as Record<string, unknown> | undefined;
    if (leaf && typeof leaf === "object" && typeof leaf.field === "string") {
      out.attribute = leaf.field;
      if (typeof leaf.op === "string") out.op = leaf.op;
      if (cond[2]?.startsWith(".value") && at !== undefined) out.value = typeof at === "string" ? at : JSON.stringify(at);
    }
  }
  return out;
}
