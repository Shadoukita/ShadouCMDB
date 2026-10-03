// Design tokens (SHAA-1670, direction 04 "Control Room"): every text and control-boundary pair the
// components draw keeps WCAG 2.1 AA in both themes. Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { describe, test } from "node:test";
import { contrast } from "../src/lib/color";

const css = readFileSync(new URL("../src/styles/tokens.css", import.meta.url), "utf8");

function block(selector: string): Map<string, string> {
  const start = css.indexOf(`${selector} {`);
  assert.ok(start >= 0, `${selector} in tokens.css`);
  const body = css.slice(start, css.indexOf("\n}", start)).replace(/\/\*[\s\S]*?\*\//g, "");
  return new Map([...body.matchAll(/(--[\w-]+):\s*([^;]+);/g)].map((m) => [m[1], m[2].trim()]));
}
const light = block(":root");
const THEMES = { light, dark: new Map([...light, ...block(':root[data-theme="dark"]')]) };

function resolve(vars: Map<string, string>, name: string): string {
  let value = name.startsWith("--") ? vars.get(name) : name;
  for (let i = 0; value && i < 10; i++) {
    const ref = /^var\((--[\w-]+)\)$/.exec(value);
    if (!ref) break;
    value = vars.get(ref[1]);
  }
  assert.ok(value && /^#[0-9a-f]{6}$/i.test(value), `${name} resolves to a hex colour`);
  return value;
}

const SURFACES = ["--c-surface", "--c-bg", "--c-surface-alt", "--c-row-hover"];
/** [foreground, backgrounds, minimum ratio] */
const PAIRS: [string, string[], number][] = [
  ["--c-text", [...SURFACES, "--c-row-selected"], 4.5],
  ["--c-text-secondary", [...SURFACES, "--c-row-selected", "--c-surface-sunken"], 4.5],
  ["--c-text-tertiary", [...SURFACES, "--c-surface-sunken"], 4.5],
  ["--c-link", [...SURFACES, "--c-primary-subtle", "--c-row-selected"], 4.5],
  ["--c-danger-text", ["--c-surface", "--c-bg", "--c-row-hover", "--c-danger-subtle"], 4.5],
  ["--c-success-text", ["--c-success-subtle"], 4.5],
  ["--c-warning-text", ["--c-warning-subtle"], 4.5],
  ["--c-info-text", ["--c-info-subtle"], 4.5],
  ["--c-primary-text", ["--c-primary", "--c-primary-hover"], 4.5],
  ["--c-on-solid", ["--c-danger-solid", "--c-danger-solid-hover"], 4.5],
  ["--badge-neutral-fg", ["--badge-neutral-bg"], 4.5],
  ["--badge-ok-fg", ["--badge-ok-bg"], 4.5],
  ["--badge-warn-fg", ["--badge-warn-bg"], 4.5],
  ["--badge-off-fg", ["--badge-off-bg"], 4.5],
  ["--badge-danger-fg", ["--badge-danger-bg"], 4.5],
  ["--badge-info-fg", ["--badge-info-bg"], 4.5],
  ["--c-sidebar-text", ["--c-sidebar", "--c-sidebar-hover", "--c-sidebar-active"], 4.5],
  ["--c-sidebar-text-muted", ["--c-sidebar", "--c-sidebar-hover", "--c-sidebar-active"], 4.5],
  ["--c-sidebar-link", ["--c-sidebar"], 4.5],
  // Control boundaries and focus rings (1.4.11).
  ["--c-border-control", ["--c-surface", "--c-bg", "--c-surface-alt"], 3],
  ["--c-focus", ["--c-surface", "--c-bg", "--c-surface-alt", "--c-row-selected"], 3],
  ["--c-primary", ["--c-surface", "--c-bg"], 3],
  ["--c-accent", ["--c-sidebar", "--c-sidebar-active"], 3],
  ["--c-success", ["--c-surface"], 3],
  ["--c-warning", ["--c-surface"], 3],
  ["--c-danger-solid", ["--c-surface"], 3],
];

for (const [theme, vars] of Object.entries(THEMES)) {
  describe(`${theme} theme contrast`, () => {
    for (const [fg, bgs, min] of PAIRS) {
      test(`${fg} ≥ ${min}:1`, () => {
        for (const bg of bgs) {
          const ratio = contrast(resolve(vars, fg), resolve(vars, bg));
          assert.ok(ratio >= min, `${fg} on ${bg}: ${ratio.toFixed(2)}:1`);
        }
      });
    }
  });
}
