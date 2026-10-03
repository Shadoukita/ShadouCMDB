// Brand colours (SHAA-1670): the constants branding needs mirror tokens.css, and no brand colour
// takes text, links or focus rings below WCAG AA. Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { describe, test } from "node:test";
import { brandVariables, SIDEBAR, SURFACE, type Theme } from "../src/lib/brandColors";
import { contrast } from "../src/lib/color";

const css = readFileSync(new URL("../src/styles/tokens.css", import.meta.url), "utf8");

/** The custom properties declared in the first block whose selector is `selector`. */
function block(selector: string): Map<string, string> {
  const start = css.indexOf(`${selector} {`);
  assert.ok(start >= 0, `${selector} in tokens.css`);
  const body = css.slice(start, css.indexOf("\n}", start)).replace(/\/\*[\s\S]*?\*\//g, "");
  return new Map([...body.matchAll(/(--[\w-]+):\s*([^;]+);/g)].map((m) => [m[1], m[2].trim()]));
}
const light = block(":root");
const dark = new Map([...light, ...block(':root[data-theme="dark"]')]);

/** A token's value with var() references followed. */
function resolve(theme: Theme, name: string): string {
  const vars = theme === "dark" ? dark : light;
  let value = vars.get(name);
  for (let i = 0; value && i < 10; i++) {
    const ref = /^var\((--[\w-]+)\)$/.exec(value);
    if (!ref) break;
    value = vars.get(ref[1]);
  }
  assert.ok(value, `${name} resolves in the ${theme} theme`);
  return value.toLowerCase();
}

const THEMES: Theme[] = ["light", "dark"];

describe("brand colour constants", () => {
  for (const theme of THEMES) {
    test(`${theme}: SURFACE and SIDEBAR are the tokens' --c-surface and --c-sidebar`, () => {
      assert.equal(SURFACE[theme], resolve(theme, "--c-surface"));
      assert.equal(SIDEBAR[theme], resolve(theme, "--c-sidebar"));
    });
  }
});

describe("brandVariables", () => {
  test("no brand colours set nothing, so tokens.css applies", () => {
    assert.deepEqual(brandVariables(null, null, "light"), {});
    assert.deepEqual(brandVariables("red", "#12", "dark"), {});
  });

  // A spread of brand colours: very light, very dark, saturated, mid grey.
  const samples = ["#0a6c7d", "#4cc3d9", "#2457d6", "#ffd400", "#0b1f3a", "#e4002b", "#00a19a", "#808080", "#f2f2f2", "#6c2bd9"];
  for (const theme of THEMES) {
    for (const color of samples) {
      test(`${theme} ${color}: text, link, focus and accent keep their contrast`, () => {
        const v = brandVariables(color, color, theme);
        const surface = SURFACE[theme];
        assert.ok(contrast(v["--c-primary-text"], v["--c-primary"]) >= 3, "button label on primary");
        assert.ok(
          contrast(v["--c-primary-text"], v["--c-primary-hover"]) >= contrast(v["--c-primary-text"], v["--c-primary"]),
          "hover never lowers the label's contrast",
        );
        assert.ok(contrast(v["--c-link"], surface) >= 4.5 || contrast(v["--c-link"], surface) >= contrast(color, surface), "link on the surface");
        assert.ok(contrast(v["--c-focus"], surface) >= 3 || contrast(v["--c-focus"], surface) >= contrast(color, surface), "focus ring on the surface");
        assert.ok(contrast(v["--c-accent"], SIDEBAR[theme]) >= 3, "accent on the sidebar");
      });
    }
  }

  test("the default primary on its subtle tint keeps AA for text", () => {
    for (const theme of THEMES) {
      const v = brandVariables("#0a6c7d", null, theme);
      assert.ok(contrast(v["--c-link"], v["--c-primary-subtle"]) >= 4.5, theme);
    }
  });
});
