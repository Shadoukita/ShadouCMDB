// Every custom property a stylesheet reads is declared somewhere: in tokens.css, in a rule, or set from script
// (an inline style or the branding store). A removed or misspelt token otherwise fails silently: the declaration
// falls back to its initial value. Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";

const SRC = new URL("../src/", import.meta.url).pathname;

function files(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((e) =>
    e.isDirectory() ? files(join(dir, e.name)) : /\.(css|vue|ts)$/.test(e.name) ? [join(dir, e.name)] : [],
  );
}

const sources = files(SRC).map((path) => ({ path: path.slice(SRC.length), text: readFileSync(path, "utf8").replace(/\/\*[\s\S]*?\*\//g, "") }));

const declared = new Set<string>();
for (const { text } of sources) {
  for (const m of text.matchAll(/(--[\w-]+)\s*:/g)) declared.add(m[1]); // a rule, or an inline style object key
  for (const m of text.matchAll(/["'`](--[\w-]+)["'`]/g)) declared.add(m[1]); // setProperty("--x", …), { "--x": … }
}

test("every var(--…) in the stylesheets and components is declared", () => {
  const missing: string[] = [];
  for (const { path, text } of sources) {
    for (const m of text.matchAll(/var\((--[\w-]+)/g)) if (!declared.has(m[1])) missing.push(`${path}: ${m[1]}`);
  }
  assert.deepEqual([...new Set(missing)], []);
});

test("the older token names stay removed", () => {
  const retired = ["--sp-1", "--sp-2", "--sp-3", "--sp-4", "--sp-5", "--sp-6", "--radius", "--shadow-pop", "--fs-lg", "--fs-xl",
    "--c-text-muted", "--c-ok", "--c-danger-bg", "--c-danger-hover", "--c-warn-bg", "--c-warn-border"];
  for (const name of retired) assert.ok(!declared.has(name), `${name} is declared again`);
});
