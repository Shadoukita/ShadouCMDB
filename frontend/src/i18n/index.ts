// In-house message catalog (spec SHAA-927 §5.9, D5). No dependency: a small subset of ICU
// MessageFormat, `{name}` and `{n, plural, one {…} other {…}}`, with Intl.PluralRules.
//
// The UI is English (Q1 → a): there is deliberately no user or instance locale setting. The German
// catalog is only reachable through the test-only overrides below, until the whole app is translated.
import { de } from "./de";
import { en, type MessageKey } from "./en";

export type { MessageKey } from "./en";
export type Locale = "en" | "de";
export type MessageParams = Record<string, string | number | null | undefined>;

const catalogs: Record<Locale, Record<MessageKey, string>> = { en, de };

let overrideLocale: Locale | null = null;

/** Test-only: force a locale in unit tests. `null` restores the default. Not wired to any UI. */
export function setLocaleForTests(locale: Locale | null): void {
  overrideLocale = locale;
}

/**
 * The active locale: English, unless a test forced another one, either with `setLocaleForTests`
 * or, for Playwright against a built app, with `window.__shadoucmdbTestLocale = "de"` set in
 * `page.addInitScript` before the app loads.
 */
export function currentLocale(): Locale {
  if (overrideLocale) return overrideLocale;
  const forced = (globalThis as { __shadoucmdbTestLocale?: unknown }).__shadoucmdbTestLocale;
  return forced === "de" || forced === "en" ? forced : "en";
}

type Node =
  | string
  | { kind: "arg"; name: string }
  | { kind: "plural"; name: string; branches: Map<string, Node[]> }
  | { kind: "hash" };

class Parser {
  pos = 0;
  readonly src: string;
  constructor(src: string) {
    this.src = src;
  }

  /** Reads nodes up to an unmatched `}` (left unconsumed) or the end. */
  message(inPlural: boolean): Node[] {
    const nodes: Node[] = [];
    let text = "";
    const flush = () => {
      if (text) nodes.push(text);
      text = "";
    };
    while (this.pos < this.src.length) {
      const c = this.src[this.pos];
      if (c === "}") break;
      if (c === "{") {
        flush();
        nodes.push(this.argument());
      } else if (c === "#" && inPlural) {
        flush();
        nodes.push({ kind: "hash" });
        this.pos++;
      } else {
        text += c;
        this.pos++;
      }
    }
    flush();
    return nodes;
  }

  private argument(): Node {
    this.pos++; // {
    const name = this.until(",}").trim();
    if (!name) this.fail("empty argument name");
    if (this.src[this.pos] === "}") {
      this.pos++;
      return { kind: "arg", name };
    }
    this.pos++; // ,
    const type = this.until(",}").trim();
    if (type !== "plural" || this.src[this.pos] !== ",") this.fail(`unsupported argument type "${type}"`);
    this.pos++; // ,
    const branches = new Map<string, Node[]>();
    for (;;) {
      this.skipSpace();
      if (this.src[this.pos] === "}") break;
      const selector = this.until("{} \t\n").trim();
      this.skipSpace();
      if (!selector || this.src[this.pos] !== "{") this.fail("expected a plural branch");
      this.pos++; // {
      branches.set(selector, this.message(true));
      if (this.src[this.pos] !== "}") this.fail("unclosed plural branch");
      this.pos++; // }
    }
    this.pos++; // }
    if (!branches.has("other")) this.fail("plural without an `other` branch");
    return { kind: "plural", name, branches };
  }

  private until(stops: string): string {
    const start = this.pos;
    while (this.pos < this.src.length && !stops.includes(this.src[this.pos])) this.pos++;
    if (this.pos >= this.src.length) this.fail("unexpected end");
    return this.src.slice(start, this.pos);
  }

  private skipSpace(): void {
    while (/\s/.test(this.src[this.pos] ?? "")) this.pos++;
  }

  private fail(reason: string): never {
    throw new Error(`i18n: ${reason} at ${this.pos} in "${this.src}"`);
  }
}

/** Parses one message; throws on a malformed one. Exported for the catalog test. */
export function parseMessage(src: string): Node[] {
  const parser = new Parser(src);
  const nodes = parser.message(false);
  if (parser.pos !== src.length) throw new Error(`i18n: unmatched "}" at ${parser.pos} in "${src}"`);
  return nodes;
}

const parsed = new Map<string, Node[]>();
const pluralRules = new Map<Locale, Intl.PluralRules>();
const numberFormats = new Map<Locale, Intl.NumberFormat>();

function cached<V>(map: Map<Locale, V>, locale: Locale, make: () => V): V {
  let value = map.get(locale);
  if (value === undefined) map.set(locale, (value = make()));
  return value;
}

function render(nodes: Node[], params: MessageParams, locale: Locale, count: number | null, countName: string): string {
  let out = "";
  for (const node of nodes) {
    if (typeof node === "string") {
      out += node;
    } else if (node.kind === "hash") {
      out += count === null ? `{${countName}}` : cached(numberFormats, locale, () => new Intl.NumberFormat(locale)).format(count);
    } else if (node.kind === "arg") {
      // A missing parameter stays visible as `{name}` instead of rendering "undefined" or nothing.
      const value = params[node.name];
      out += value === null || value === undefined ? `{${node.name}}` : String(value);
    } else {
      const value = params[node.name];
      const n = typeof value === "number" && Number.isFinite(value) ? value : null;
      let branch = n === null ? undefined : node.branches.get(`=${n}`);
      if (!branch && n !== null) branch = node.branches.get(cached(pluralRules, locale, () => new Intl.PluralRules(locale)).select(n));
      out += render(branch ?? node.branches.get("other")!, params, locale, n, node.name);
    }
  }
  return out;
}

/** The text for `key` in the active locale, with `{name}` parameters and plurals filled in. */
export function t(key: MessageKey, params: MessageParams = {}): string {
  const locale = currentLocale();
  const cacheKey = `${locale}\u0000${key}`;
  let nodes = parsed.get(cacheKey);
  if (!nodes) parsed.set(cacheKey, (nodes = parseMessage(catalogs[locale][key])));
  return render(nodes, params, locale, null, "");
}
