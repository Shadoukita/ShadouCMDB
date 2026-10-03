// Icon system (SHAA-1670, design document §2.6): class icon keys stay stable, every one draws a
// vendored icon, and no unicode glyph comes back as an icon. Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { describe, test } from "node:test";
import { ICONS } from "../src/icons/lucide";
import { CLASS_ICONS, classIcon } from "../src/lib/classIcons";

describe("class icons", () => {
  test("the stored keys are unchanged, so existing classes keep their icon", () => {
    assert.deepEqual(
      CLASS_ICONS.map((i) => i.key),
      ["server", "vm", "network", "storage", "database", "application", "service", "container", "cloud", "location", "device", "document", "person", "generic"],
    );
  });

  test("each key draws an icon of the vendored set", () => {
    for (const i of CLASS_ICONS) assert.ok(ICONS[i.icon], `${i.key} → ${i.icon}`);
  });

  test("an unknown or empty key draws nothing", () => {
    assert.equal(classIcon("rack-unit-from-the-api"), undefined);
    assert.equal(classIcon(""), undefined);
    assert.equal(classIcon(null), undefined);
  });
});

describe("vendored Lucide subset", () => {
  test("every icon is a non-empty list of SVG shape elements", () => {
    for (const [name, nodes] of Object.entries(ICONS)) {
      assert.ok(nodes.length > 0, name);
      for (const [tag, attrs] of nodes) {
        assert.match(tag, /^(path|circle|rect|line|polyline|polygon|ellipse)$/, name);
        for (const k of Object.keys(attrs)) assert.doesNotMatch(k, /^on/i, `${name}: no event attributes`);
      }
    }
  });
});

describe("no unicode glyphs as icons", () => {
  // Arrows and × also appear in running text ("Alt+↑", "640 × 480 px"), so only glyphs that are never text are listed.
  const GLYPHS = /[☰▾▸▴▲▼⋯✎✓✕⠿⇤⇥⤒⤓○⚠]/;
  const src = new URL("../src/", import.meta.url).pathname;
  const files = (dir: string): string[] =>
    readdirSync(dir).flatMap((f) => {
      const p = join(dir, f);
      return statSync(p).isDirectory() ? files(p) : /\.(vue|ts|css)$/.test(f) ? [p] : [];
    });

  test("components, scripts and stylesheets draw icons with <Icon>, not text", () => {
    const hits = files(src)
      .filter((p) => !p.includes("/i18n/"))
      .flatMap((p) =>
        readFileSync(p, "utf8")
          .split("\n")
          .flatMap((line, i) => (GLYPHS.test(line) && !/^\s*(\/\/|\*|\/\*)/.test(line) ? [`${p.slice(src.length)}:${i + 1}`] : [])),
      );
    assert.deepEqual(hits, []);
  });
});
