// Tokens all the way down (SHAA-1670 design document §1, rule 4): no colour, font, radius, shadow or
// z-index literal outside src/styles/tokens.css. Checks every stylesheet and every <style> block in a
// .vue file, and exits 1 with the file and line of each offending declaration.
//
//   npm run lint -w frontend          (CI runs it as `npm run lint --workspaces --if-present`)
//
// Node built-ins only, on purpose: stylelint would add a dependency tree with an unpatched advisory
// (GHSA-vfj7-8cjw-p6xm in braces), and this is all of it that the rule needs.
//
// A literal that must stay (a printed label is black on white in either theme) is allowed by a comment on
// the line above that gives the reason:  /* token-lint-allow: a printed label is black on white paper */
import { readdirSync, readFileSync } from "node:fs";
import { join, relative } from "node:path";

const ROOT = new URL("..", import.meta.url).pathname;
const SRC = join(ROOT, "src");
/** Where the raw values live: the tokens, the @font-face rules, and the generated icon subset. */
const EXEMPT = new Set(["src/styles/tokens.css", "src/styles/fonts.css", "src/icons/lucide-css.css"]);

const tokenVar = (prefix) => new RegExp(`^var\\(--${prefix}[\\w-]*\\)$`);
const RULES = [
  { prop: /^font-size$/, allow: [tokenVar("fs-"), /^inherit$/], what: "a --fs-* token" },
  { prop: /^font-weight$/, allow: [tokenVar("fw-"), /^inherit$/], what: "a --fw-* token" },
  { prop: /^font-family$/, allow: [tokenVar("font-"), /^inherit$/], what: "a --font-* token" },
  { prop: /^font$/, allow: [/^inherit$/], what: "font: inherit, or the longhand properties with tokens" },
  {
    prop: /^z-index$/,
    allow: [tokenVar("z-"), /^calc\(var\(--z-[\w-]+\) [+-] \d\)$/, /^var\(--win-z\)$/, /^(0|auto)$/],
    what: "a --z-* token",
  },
  {
    prop: /^border(-[a-z]+)*-radius$/,
    allow: [/^((0|50%|inherit|var\(--radius-(xs|sm|md|lg|full)\)|calc\(var\(--radius-[a-z]+\) - \d+px\))\s*)+$/],
    what: "a --radius-* token",
  },
  {
    // Elevation comes from the shadow tokens; an inset bar or rule drawn in a colour token is not elevation.
    prop: /^box-shadow$/,
    allow: [/^none$/, tokenVar("shadow-"), /^inset( -?\d+px| 0){3,4} var\(--c-[\w-]+\)$/],
    what: "a --shadow-* token, or an inset bar in a colour token",
  },
];
const COLOUR_PROP = /^(color|background(-color)?|border(-[a-z]+)*(-color)?|outline(-color)?|fill|stroke|caret-color|accent-color|text-decoration(-color)?|column-rule(-color)?|box-shadow|--[\w-]+)$/;
const HEX = /#[0-9a-f]{3,8}\b/i;
const COLOUR_FN = /\b(rgba?|hsla?|hwb|lab|lch|oklab|oklch|color)\(/i;
const NAMED = /(^|[\s,(])(white|black|red|green|blue|gr[ae]y|silver|maroon|purple|fuchsia|lime|olive|yellow|navy|teal|aqua|orange|pink|brown|gold|cyan|magenta)(?=$|[\s,)])/i;

function files(dir) {
  return readdirSync(dir, { withFileTypes: true }).flatMap((e) =>
    e.isDirectory() ? files(join(dir, e.name)) : /\.(css|vue)$/.test(e.name) ? [join(dir, e.name)] : [],
  );
}

/** The CSS of a file with its line numbers kept: a .vue file's <style> blocks, everything else blanked. */
function css(path, text) {
  if (!path.endsWith(".vue")) return text;
  let out = text.replace(/[^\n]/g, " ");
  for (const m of text.matchAll(/(<style\b[^>]*>)([\s\S]*?)<\/style>/g)) {
    const start = m.index + m[1].length;
    out = out.slice(0, start) + m[2] + out.slice(start + m[2].length);
  }
  return out;
}

const problems = [];
for (const path of files(SRC)) {
  const rel = relative(ROOT, path);
  if (EXEMPT.has(rel)) continue;
  const source = css(path, readFileSync(path, "utf8"));
  // Allowances first, then comments blanked so a commented-out rule is not reported (newlines kept).
  const allowed = new Set();
  for (const m of source.matchAll(/\/\*\s*token-lint-allow:(.*?)\*\//g)) {
    const line = source.slice(0, m.index).split("\n").length;
    if (!m[1].trim()) problems.push(`${rel}:${line}  token-lint-allow needs a reason`);
    allowed.add(line + 1);
  }
  const code = source.replace(/\/\*[\s\S]*?\*\//g, (c) => c.replace(/[^\n]/g, " "));
  for (const m of code.matchAll(/(?<=[{;\s])(--[\w-]+|[a-z-]+)\s*:\s*([^;{}]+?)\s*(?:;|(?=}))/g)) {
    const [, prop, rawValue] = m;
    const value = rawValue.replace(/\s*!important$/, "").replace(/\s+/g, " ");
    const line = code.slice(0, m.index).split("\n").length;
    if (allowed.has(line)) continue;
    const report = (why) => problems.push(`${rel}:${line}  ${prop}: ${value}  (${why})`);
    if (COLOUR_PROP.test(prop) && !value.startsWith("url(")) {
      if (HEX.test(value)) report("hex colour: use a --c-* token");
      else if (COLOUR_FN.test(value)) report("colour function: use a --c-* token");
      else if (NAMED.test(value)) report("named colour: use a --c-* token");
    }
    const rule = RULES.find((r) => r.prop.test(prop));
    if (rule && !rule.allow.some((a) => a.test(value))) report(`use ${rule.what}`);
  }
}

if (problems.length) {
  console.error(`Design token check: ${problems.length} problem(s)\n${problems.join("\n")}`);
  process.exit(1);
}
console.log("Design token check: no literals outside tokens.css.");
