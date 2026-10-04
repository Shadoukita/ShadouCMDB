import type { LocationQuery, LocationQueryRaw } from "vue-router";
import { param } from "./inventoryQuery";

/**
 * The inventory's query bar (design document §2.7): `key:value` tokens and plain words, as a
 * front end to the list's existing URL filters. It is not a query language of its own: every
 * token maps onto one of the parameters the API filters on (GET /configuration-items), and the
 * URL maps back onto tokens. Nothing is filtered in the browser.
 *
 *   class:server,vm          classId (the classes' keys; any of them)
 *   <lookup list>:prod,test  lookupValueId (a lookup list's key, then its values' keys; any value of
 *                            a list, and every list, as the API combines them)
 *   criticality:high         criticalityValueId
 *   validity:all|inactive    active ("active", the default, removes it)
 *   deleted:include|only     deleted ("hide", the default, removes it)
 *   ip:10.0.0.0/8            ipWithin
 *   layout:own|default       ownLayout
 *   template:<key>           layoutTemplate
 *   anything else            q, the full-text search; "quoted text" is searched as it is
 *
 * Keys are the same in every language: they are what a bookmark or a colleague's message holds.
 * Values match by key, or by name when quoted. `-key:value` (excluding) has no API filter, so it is
 * an error, as is an unknown key or value: the bar says so under itself and the list stays as it was.
 */

/** The URL parameters the bar reads and writes. Any other (sort, columns, includeSubclasses) it leaves alone. */
export const BAR_PARAMS = ["classId", "lookupValueId", "criticalityValueId", "active", "deleted", "ipWithin", "ownLayout", "layoutTemplate", "q"] as const;
export type BarParam = (typeof BAR_PARAMS)[number];
export type BarPatch = Partial<Record<BarParam, string | undefined>>;

/** Keys with a fixed meaning. A lookup list with one of these keys is reached through the filters instead. */
export const FIXED_KEYS = ["class", "criticality", "validity", "deleted", "ip", "layout", "template"] as const;
type FixedKey = (typeof FIXED_KEYS)[number];

/** Keys with a fixed set of values: the value as typed, and the URL value it stands for (undefined: the default). */
const ENUMS = {
  validity: { param: "active", values: { active: undefined, all: "all", inactive: "false" } },
  deleted: { param: "deleted", values: { hide: undefined, include: "include", only: "only" } },
  layout: { param: "ownLayout", values: { own: "true", default: "false" } },
} as const satisfies Record<string, { param: BarParam; values: Record<string, string | undefined> }>;
type EnumKey = keyof typeof ENUMS;

interface Keyed {
  id: string;
  key: string;
  name: string;
}

/** What the bar resolves keys against. A part is undefined while it loads: its tokens wait (pending) rather than fail. */
export interface BarCatalogue {
  classes?: readonly Keyed[];
  /** The criticality list's values. */
  criticality?: readonly Keyed[];
  /** Lookup lists other than criticality (that one is `criticality:`), in display order. */
  lists?: readonly (Keyed & { systemRole?: string | null })[];
  values?: readonly (Keyed & { listId: string })[];
}

export type BarErrorCode = "negation" | "unknownKey" | "unknownValue" | "badValue" | "twice";
export interface BarError {
  code: BarErrorCode;
  /** The token as typed, and where it starts in the text. */
  token: string;
  at: number;
  key: string;
  value?: string;
  /** For badValue: the values the key takes. */
  allowed?: string[];
}

export interface Parsed {
  /** The bar's parameters as the text says (undefined: not set). */
  patch: Required<Record<BarParam, string | undefined>>;
  /** Which parameters the text names with a token (q: whether it has any words). */
  named: Set<BarParam>;
  errors: BarError[];
  /** A token is unfinished (`class:`) or names something still loading: not applied yet. */
  pending: boolean;
}

export interface Token {
  start: number;
  end: number;
  raw: string;
}

const KEY_TOKEN = /^(-?)([A-Za-z][A-Za-z0-9_]*):(.*)$/s;

/** The text split at whitespace outside double quotes, with each token's position. An unclosed quote runs to the end. */
export function tokenize(text: string): Token[] {
  const out: Token[] = [];
  let i = 0;
  while (i < text.length) {
    while (i < text.length && /\s/.test(text[i]!)) i++;
    if (i >= text.length) break;
    const start = i;
    let quoted = false;
    while (i < text.length && (quoted || !/\s/.test(text[i]!))) {
      if (text[i] === '"') quoted = !quoted;
      i++;
    }
    out.push({ start, end: i, raw: text.slice(start, i) });
  }
  return out;
}

const unquote = (s: string) => (s.startsWith('"') ? s.slice(1, s.endsWith('"') && s.length > 1 ? -1 : undefined) : s);
const splitValues = (value: string) => {
  const v = value.trim();
  return v.startsWith('"') ? [unquote(v)] : v.split(",").map((s) => s.trim());
};

/** A key token's parts, or null for a word. `raw` keys are lower-cased. */
export function keyToken(raw: string): { negated: boolean; key: string; value: string } | null {
  if (raw.startsWith('"')) return null;
  const m = KEY_TOKEN.exec(raw);
  return m ? { negated: m[1] === "-", key: m[2]!.toLowerCase(), value: m[3]! } : null;
}

const isFixed = (key: string): key is FixedKey => (FIXED_KEYS as readonly string[]).includes(key);
const listsOf = (c: BarCatalogue) => c.lists?.filter((l) => !l.systemRole && !isFixed(l.key.toLowerCase()));

/** A value by key, or by name (any case). */
function find<T extends Keyed>(items: readonly T[], value: string): T | undefined {
  const v = value.toLowerCase();
  return items.find((x) => x.key.toLowerCase() === v) ?? items.find((x) => x.name.toLowerCase() === v);
}

const emptyPatch = (): Parsed["patch"] => Object.fromEntries(BAR_PARAMS.map((p) => [p, undefined])) as Parsed["patch"];

/** The bar's text as URL parameters, with what is wrong with it. */
export function parseBar(text: string, catalogue: BarCatalogue): Parsed {
  const patch = emptyPatch();
  const named = new Set<BarParam>();
  const errors: BarError[] = [];
  let pending = false;
  const words: string[] = [];
  const ids: Record<"classId" | "criticalityValueId", string[]> = { classId: [], criticalityValueId: [] };
  const lookupIds = new Map<string, string[]>(); // by list, in the order typed
  const lists = listsOf(catalogue);

  for (const tok of tokenize(text)) {
    const k = keyToken(tok.raw);
    if (!k) {
      const w = unquote(tok.raw);
      if (w) words.push(w);
      continue;
    }
    const { key, value } = k;
    const list = isFixed(key) ? undefined : lists?.find((l) => l.key.toLowerCase() === key);
    if (!isFixed(key) && !list) {
      // An unknown key may be a lookup list that is still loading.
      if (!lists) pending = true;
      else errors.push({ code: "unknownKey", token: tok.raw, at: tok.start, key });
      continue;
    }
    if (k.negated) {
      errors.push({ code: "negation", token: tok.raw, at: tok.start, key });
      continue;
    }
    const values = splitValues(value).filter(Boolean);
    if (values.length === 0) {
      pending = true; // still being typed
      continue;
    }

    if (key === "class" || key === "criticality") {
      const p = key === "class" ? "classId" : "criticalityValueId";
      const items = key === "class" ? catalogue.classes : catalogue.criticality;
      named.add(p);
      if (!items) {
        pending = true;
        continue;
      }
      for (const v of values) {
        const hit = find(items, v);
        if (hit) ids[p].includes(hit.id) || ids[p].push(hit.id);
        else errors.push({ code: "unknownValue", token: tok.raw, at: tok.start, key, value: v });
      }
    } else if (list) {
      named.add("lookupValueId");
      if (!catalogue.values) {
        pending = true;
        continue;
      }
      const mine = catalogue.values.filter((x) => x.listId === list.id);
      const acc = lookupIds.get(list.id) ?? [];
      lookupIds.set(list.id, acc);
      for (const v of values) {
        const hit = find(mine, v);
        if (hit) acc.includes(hit.id) || acc.push(hit.id);
        else errors.push({ code: "unknownValue", token: tok.raw, at: tok.start, key, value: v });
      }
    } else if (key === "ip" || key === "template") {
      const p = key === "ip" ? "ipWithin" : "layoutTemplate";
      if (named.has(p)) errors.push({ code: "twice", token: tok.raw, at: tok.start, key });
      named.add(p);
      patch[p] = unquote(value.trim());
    } else {
      const e = ENUMS[key as EnumKey];
      if (named.has(e.param)) errors.push({ code: "twice", token: tok.raw, at: tok.start, key });
      named.add(e.param);
      const v = values[0]!.toLowerCase();
      if (values.length > 1 || !(v in e.values)) errors.push({ code: "badValue", token: tok.raw, at: tok.start, key, value: value.trim(), allowed: Object.keys(e.values) });
      else patch[e.param] = e.values[v as keyof typeof e.values];
    }
  }

  if (ids.classId.length) patch.classId = ids.classId.join(",");
  if (ids.criticalityValueId.length) patch.criticalityValueId = ids.criticalityValueId.join(",");
  const lookup = [...lookupIds.values()].flat();
  if (lookup.length) patch.lookupValueId = lookup.join(",");
  if (words.length) {
    patch.q = words.join(" ");
    named.add("q");
  }
  return { patch, named, errors, pending };
}

const quoteWord = (w: string) => (/\s/.test(w) || /^-?[A-Za-z][A-Za-z0-9_]*:/.test(w) ? `"${w}"` : w);
const quoteValue = (v: string) => (/[\s,"]/.test(v) ? `"${v}"` : v);
const ids = (raw: string) => raw.split(",").filter(Boolean);

/**
 * The URL's filters as bar text, and which parameters it could not write as tokens (an id the
 * catalogue does not know, or a part that is still loading). Those stay out of the text, and
 * applying the text leaves them as they are unless it names them.
 */
export function serializeBar(query: LocationQuery | LocationQueryRaw, catalogue: BarCatalogue): { text: string; unrepresented: Set<BarParam> } {
  const parts: string[] = [];
  const unrepresented = new Set<BarParam>();
  const keysOf = (raw: string, items: readonly Keyed[] | undefined) => {
    const hits = ids(raw).map((id) => items?.find((x) => x.id === id));
    return hits.every(Boolean) ? (hits as Keyed[]).map((x) => quoteValue(x.key)) : null;
  };

  const classId = param(query, "classId");
  if (classId) {
    const keys = keysOf(classId, catalogue.classes);
    if (keys) parts.push(`class:${keys.join(",")}`);
    else unrepresented.add("classId");
  }
  const lookup = param(query, "lookupValueId");
  if (lookup) {
    const lists = listsOf(catalogue);
    const values = ids(lookup).map((id) => catalogue.values?.find((x) => x.id === id));
    if (lists && values.every((v) => v && lists.some((l) => l.id === v.listId))) {
      for (const l of lists) {
        const mine = values.filter((v) => v!.listId === l.id);
        if (mine.length) parts.push(`${l.key}:${mine.map((v) => quoteValue(v!.key)).join(",")}`);
      }
    } else unrepresented.add("lookupValueId");
  }
  const crit = param(query, "criticalityValueId");
  if (crit) {
    const keys = keysOf(crit, catalogue.criticality);
    if (keys) parts.push(`criticality:${keys.join(",")}`);
    else unrepresented.add("criticalityValueId");
  }
  for (const [key, e] of Object.entries(ENUMS)) {
    const raw = param(query, e.param);
    if (!raw) continue;
    const name = Object.entries(e.values).find(([, v]) => v === raw)?.[0];
    if (name) parts.push(`${key}:${name}`);
    else unrepresented.add(e.param);
  }
  const ip = param(query, "ipWithin");
  if (ip) parts.push(`ip:${quoteValue(ip)}`);
  const template = param(query, "layoutTemplate");
  if (template) parts.push(`template:${quoteValue(template)}`);
  const q = param(query, "q").trim();
  if (q) parts.push(...q.split(/\s+/).map(quoteWord));
  return { text: parts.join(" "), unrepresented };
}

/**
 * The URL patch that applying the bar's text makes: every parameter it can show, set as the text
 * says (so deleting a token removes its filter), and the ones it cannot show only when the text names them.
 */
export function barPatch(parsed: Parsed, unrepresented: ReadonlySet<BarParam>): BarPatch {
  const out: BarPatch = {};
  for (const p of BAR_PARAMS) if (!unrepresented.has(p) || parsed.named.has(p)) out[p] = parsed.patch[p];
  return out;
}

const norm = (p: BarParam, v: string | undefined) => {
  const s = (v ?? "").trim();
  return p === "q" ? s.replace(/\s+/g, " ") : ids(s).sort().join(",");
};

/** Whether the URL already holds what the text says, so the text can stay as the operator typed it. */
export function sameAsUrl(query: LocationQuery | LocationQueryRaw, patch: BarPatch): boolean {
  return Object.entries(patch).every(([p, v]) => norm(p as BarParam, param(query, p)) === norm(p as BarParam, v));
}

// ---------- Syntax colouring ----------

export type SegmentKind = "space" | "word" | "negation" | "key" | "punct" | "value";
export interface Segment {
  text: string;
  kind: SegmentKind;
  /** Part of a token the bar reports an error on. */
  error?: true;
}

/**
 * The text cut into coloured runs for the bar's overlay: the negation sign, the key, the colon and
 * commas, the values, and plain words. The runs join back into the text exactly (the overlay must
 * line up with the input glyph by glyph). `errorAt` holds the starts of the tokens with an error.
 */
export function segmentBar(text: string, errorAt: ReadonlySet<number> = new Set()): Segment[] {
  const out: Segment[] = [];
  let pos = 0;
  for (const tk of tokenize(text)) {
    if (tk.start > pos) out.push({ text: text.slice(pos, tk.start), kind: "space" });
    pos = tk.end;
    const mark = errorAt.has(tk.start) ? ({ error: true } as const) : {};
    const k = keyToken(tk.raw);
    if (!k) {
      out.push({ text: tk.raw, kind: "word", ...mark });
      continue;
    }
    const colon = tk.raw.indexOf(":");
    if (k.negated) out.push({ text: "-", kind: "negation", ...mark });
    out.push({ text: tk.raw.slice(k.negated ? 1 : 0, colon), kind: "key", ...mark });
    out.push({ text: ":", kind: "punct", ...mark });
    // A quoted value is one value, commas and all; otherwise commas separate the values.
    const parts = k.value.trimStart().startsWith('"') ? [k.value] : k.value.split(/(,)/);
    for (const part of parts) if (part) out.push({ text: part, kind: part === "," ? "punct" : "value", ...mark });
  }
  if (pos < text.length) out.push({ text: text.slice(pos), kind: "space" });
  return out;
}

// ---------- Autocomplete ----------

export interface Suggestion {
  /** What goes into the text (a key with its colon, or a value's key). */
  insert: string;
  /** For the list: the key or value key (mono), and a name. */
  label: string;
  detail: string;
}
export interface Suggestions {
  kind: "key" | "value";
  /** The span of the text a suggestion replaces. */
  from: number;
  to: number;
  items: Suggestion[];
}

/** Names for the fixed keys and their values, from the screen's language (`validity.all` and so on). */
export type BarLabels = Record<string, string>;

const MAX_SUGGESTIONS = 20;

/** The token around the caret: an empty one when the caret sits in whitespace. */
function tokenAt(text: string, caret: number): Token {
  return tokenize(text).find((tk) => tk.start <= caret && caret <= tk.end) ?? { start: caret, end: caret, raw: "" };
}

/**
 * What to offer at the caret: the keys when a word is begun (all of them in an empty spot, as the
 * bar's help), or a key's values after its colon (from the last comma on). Null when nothing fits.
 */
export function suggest(text: string, caret: number, catalogue: BarCatalogue, labels: BarLabels): Suggestions | null {
  const tk = tokenAt(text, caret);
  if (tk.raw.startsWith('"')) return null;
  const k = keyToken(tk.raw);
  const lists = listsOf(catalogue) ?? [];
  if (!k) {
    const typed = tk.raw.toLowerCase();
    const all: Suggestion[] = [
      ...FIXED_KEYS.map((key) => ({ insert: `${key}:`, label: `${key}:`, detail: labels[key] ?? key })),
      ...lists.map((l) => ({ insert: `${l.key}:`, label: `${l.key}:`, detail: l.name })),
    ];
    const items = all.filter((s) => s.insert.toLowerCase().startsWith(typed) && s.insert.toLowerCase() !== typed);
    return items.length ? { kind: "key", from: tk.start, to: tk.end, items: items.slice(0, MAX_SUGGESTIONS) } : null;
  }
  if (k.negated || k.value.startsWith('"')) return null;
  const valueStart = tk.start + tk.raw.indexOf(":") + 1;
  const lastComma = tk.raw.lastIndexOf(",");
  const from = lastComma >= 0 && tk.start + lastComma >= valueStart ? tk.start + lastComma + 1 : valueStart;
  const typed = text.slice(from, tk.end).toLowerCase();
  const already = new Set(k.value.toLowerCase().split(","));

  let candidates: Suggestion[] = [];
  if (k.key === "class") candidates = (catalogue.classes ?? []).map((c) => ({ insert: c.key, label: c.key, detail: c.name }));
  else if (k.key === "criticality") candidates = (catalogue.criticality ?? []).map((c) => ({ insert: c.key, label: c.key, detail: c.name }));
  else if (k.key in ENUMS)
    candidates = Object.keys(ENUMS[k.key as EnumKey].values).map((v) => ({ insert: v, label: v, detail: labels[`${k.key}.${v}`] ?? v }));
  else {
    const list = lists.find((l) => l.key.toLowerCase() === k.key);
    if (list) candidates = (catalogue.values ?? []).filter((v) => v.listId === list.id).map((v) => ({ insert: v.key, label: v.key, detail: v.name }));
  }
  const items = candidates.filter(
    (s) => (s.label.toLowerCase().includes(typed) || s.detail.toLowerCase().includes(typed)) && (s.insert.toLowerCase() === typed || !already.has(s.insert.toLowerCase())),
  );
  // An exact match alone has nothing left to offer.
  if (items.length === 1 && items[0]!.insert.toLowerCase() === typed) return null;
  return items.length ? { kind: "value", from, to: tk.end, items: items.slice(0, MAX_SUGGESTIONS) } : null;
}

/** The text and caret after taking a suggestion. A value ends its token with a space, unless one follows. */
export function accept(text: string, s: Suggestions, item: Suggestion): { text: string; caret: number } {
  const after = text.slice(s.to);
  const sep = s.kind === "value" && !/^\s/.test(after) ? " " : "";
  const next = text.slice(0, s.from) + item.insert + sep + after;
  return { text: next, caret: s.from + item.insert.length + sep.length };
}
