// Every user-visible word comes from the message catalog (SHAA-927 §5.9, SHAA-3035): a German instance must not
// show English that bypassed src/i18n. Checks the <template> of every .vue file under src/ and exits 1 with the
// file and line of each hard-coded string:
//
//   - text between tags, outside {{ }};
//   - a literal title, aria-label, placeholder, alt or label attribute (static, or a quoted string in a binding);
//   - a quoted prose string inside {{ }} or inside a bound title/aria-label/placeholder/alt/label.
//
//   npm run lint -w frontend          (CI runs it as `npm run lint --workspaces --if-present`)
//
// Symbols, units, punctuation and numbers pass (ALLOWED below). A literal that must stay (a product name, a
// protocol keyword shown verbatim) is allowed by a comment on the line above that gives the reason:
//   <!-- i18n-lint-allow: "PostgreSQL" is a product name -->
import { readdirSync, readFileSync } from "node:fs";
import { join, relative } from "node:path";
import { parse } from "vue/compiler-sfc";

const ROOT = new URL("..", import.meta.url).pathname;
const SRC = join(ROOT, "src");

/** Attributes a user reads (or a screen reader announces). */
const TEXT_ATTRS = new Set(["title", "aria-label", "aria-description", "aria-placeholder", "aria-roledescription", "aria-valuetext", "placeholder", "alt", "label"]);
/** No letters at all, or a lone unit/symbol: "…", "·", "×", "%", "px", "→", "/", "#", "MB". */
const ALLOWED = /^[\s\d\p{P}\p{S}]*$|^(px|ms|s|MB|GB|KB|kB|B|ID|IP|CI|CIs|URL|UUID|CSV|JSON|SQL|API|TLS|PDF|QR|OK|x)$/u;
// Identifiers, message keys, enum values and codes ("error", "wfDesign.op.eq", "CONFLICT") pass; a word next to a
// space or bracket (" (deleted)") or a capitalised word ("Yes") is prose.
const prose = (s) => /[A-Za-z]{2,}/.test(s) && !ALLOWED.test(s.trim()) && (/[^\w.:/-]/.test(s) || /^[A-Z][a-z]+$/.test(s));

function files(dir) {
  return readdirSync(dir, { withFileTypes: true }).flatMap((e) =>
    e.isDirectory() ? files(join(dir, e.name)) : e.name.endsWith(".vue") ? [join(dir, e.name)] : [],
  );
}

/** The quoted string literals of a JS expression, template-literal parts included, without their ${…}. */
function literals(exp) {
  const out = [];
  const re = /'((?:[^'\\]|\\.)*)'|"((?:[^"\\]|\\.)*)"|`((?:[^`\\]|\\.)*)`/g;
  for (const m of exp.matchAll(re)) {
    const s = m[1] ?? m[2] ?? m[3].replace(/\$\{[^}]*\}/g, "x");
    out.push(s);
  }
  return out;
}

/** The expression parts of an expression that are not arguments of t()/tAround(): a message key is not prose. */
function stripKeys(exp) {
  return exp.replace(/\b(t|tAround|hasMessage)\(\s*(['"`])[^'"`]*\2/g, "$1(");
}

const ELEMENT = 1, TEXT = 2, INTERPOLATION = 5, ATTRIBUTE = 6, DIRECTIVE = 7;
const problems = [];

for (const path of files(SRC)) {
  const rel = relative(ROOT, path);
  const source = readFileSync(path, "utf8");
  const ast = parse(source, { filename: rel }).descriptor.template?.ast;
  if (!ast) continue;
  const lines = source.split("\n");
  const report = (loc, what, text) => {
    const line = loc.start.line;
    if (/i18n-lint-allow:/.test(lines[line - 2] ?? "") || /i18n-lint-allow:/.test(lines[line - 1] ?? "")) return;
    problems.push(`${rel}:${line}: ${what} "${text.trim().replace(/\s+/g, " ").slice(0, 80)}" — use t() and add the key to src/i18n/en.ts and de.ts`);
  };
  const expression = (exp, loc, what) => {
    for (const lit of literals(stripKeys(exp))) if (prose(lit)) report(loc, what, lit);
  };
  const walk = (node, inPre) => {
    if (node.type === TEXT) {
      if (!inPre && !ALLOWED.test(node.content.trim())) report(node.loc, "text", node.content);
      return;
    }
    if (node.type === INTERPOLATION) {
      expression(node.content.content ?? "", node.loc, "string in {{ }}");
      return;
    }
    if (node.type !== ELEMENT) return;
    for (const p of node.props ?? []) {
      if (p.type === ATTRIBUTE && TEXT_ATTRS.has(p.name) && p.value && !ALLOWED.test(p.value.content.trim())) {
        report(p.loc, `${p.name}=`, p.value.content);
      } else if (p.type === DIRECTIVE && p.name === "bind" && p.arg?.content && TEXT_ATTRS.has(p.arg.content) && p.exp) {
        expression(p.exp.content, p.loc, `:${p.arg.content}=`);
      }
    }
    const pre = inPre || node.tag === "pre" || node.tag === "code" || node.tag === "kbd";
    for (const child of node.children ?? []) walk(child, pre);
  };
  walk(ast, false);
}

if (problems.length) {
  console.error(problems.join("\n"));
  console.error(`\n${problems.length} hard-coded user-visible string(s). See scripts/lint-i18n.mjs.`);
  process.exit(1);
}
console.log("i18n lint: no hard-coded user-visible strings in templates.");
