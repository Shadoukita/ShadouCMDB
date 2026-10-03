// Stylesheets (SHAA-1670 design document §2.4, §2.7): the primitives draw only from tokens, every
// variable they use exists, and both densities keep targets of at least 24 px (WCAG 2.5.8).
// Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { describe, test } from "node:test";

const src = new URL("../src/", import.meta.url);
const read = (path: string) => readFileSync(new URL(path, src), "utf8");
const stripComments = (css: string) => css.replace(/\/\*[\s\S]*?\*\//g, "");

const PRIMITIVES = ["styles/base.css", ...readdirSync(new URL("styles/components/", src)).map((f) => `styles/components/${f}`)];
const STYLESHEETS = ["styles/tokens.css", "styles/fonts.css", "styles/app.css", "icons/lucide-css.css", ...PRIMITIVES];

/** Every file under src/ with the given extensions, as paths relative to src/. */
function files(dir: string, exts: string[]): string[] {
  return readdirSync(new URL(dir, src), { withFileTypes: true }).flatMap((e) =>
    e.isDirectory() ? files(`${dir}${e.name}/`, exts) : exts.some((x) => e.name.endsWith(x)) ? [`${dir}${e.name}`] : [],
  );
}

describe("stylesheets", () => {
  test("every custom property a stylesheet reads is defined", () => {
    const defined = new Set<string>();
    for (const f of STYLESHEETS) for (const m of stripComments(read(f)).matchAll(/(--[\w-]+)\s*:/g)) defined.add(m[1]);
    // Set at runtime from components (inline style bindings, branding): named in a .vue or .ts file.
    for (const f of files("", [".vue", ".ts"])) for (const m of read(f).matchAll(/["'`](--[\w-]+)["'`]/g)) defined.add(m[1]);
    const missing: string[] = [];
    for (const f of STYLESHEETS)
      for (const m of stripComments(read(f)).matchAll(/var\((--[\w-]+)/g)) if (!defined.has(m[1])) missing.push(`${f}: ${m[1]}`);
    assert.deepEqual([...new Set(missing)], []);
  });

  test("the primitives use no colour literal: colours come from tokens.css", () => {
    const literals: string[] = [];
    for (const f of PRIMITIVES)
      for (const m of stripComments(read(f)).matchAll(/#[0-9a-f]{3,8}\b|\b(?:rgba?|hsla?)\(/gi)) literals.push(`${f}: ${m[0]}`);
    assert.deepEqual(literals, []);
  });

  test("app.css imports every primitive stylesheet", () => {
    const app = read("styles/app.css");
    for (const f of PRIMITIVES) assert.ok(app.includes(`@import "./${f.replace("styles/", "")}";`), `${f} imported`);
  });
});

describe("density tokens", () => {
  const tokens = stripComments(read("styles/tokens.css"));
  const block = (selector: string) => {
    const start = tokens.indexOf(`${selector} {`);
    assert.ok(start >= 0, `${selector} in tokens.css`);
    const body = tokens.slice(start, tokens.indexOf("\n}", start));
    return new Map([...body.matchAll(/(--[\w-]+):\s*([^;]+);/g)].map((m) => [m[1], m[2].trim()]));
  };
  const standard = block(":root");
  const comfortable = block(':root[data-density="comfortable"]');
  const px = (v: string | undefined) => {
    const m = /^(\d+(?:\.\d+)?)px$/.exec(v ?? "");
    assert.ok(m, `${v} is a px length`);
    return Number(m[1]);
  };

  test("comfortable only changes size tokens, and never makes anything smaller", () => {
    assert.ok(comfortable.size > 0);
    for (const [name, value] of comfortable) {
      assert.ok(standard.has(name), `${name} has a standard value`);
      assert.ok(px(value) >= px(standard.get(name)), `${name}: comfortable ${value} ≥ standard ${standard.get(name)}`);
    }
  });

  test("small controls and rows stay at 24 px or more in both densities (WCAG 2.5.8)", () => {
    for (const d of [standard, new Map([...standard, ...comfortable])])
      for (const name of ["--control-h-sm", "--control-h", "--row-h", "--menu-item-h"]) assert.ok(px(d.get(name)) >= 24, `${name} = ${d.get(name)}`);
  });
});
