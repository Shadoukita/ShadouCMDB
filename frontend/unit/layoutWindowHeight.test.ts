// A window placed from the grid is tall enough for the detail page's inline inputs (GH#621): the heights of
// the API's grid → free conversion against what the stylesheets give a field window in the comfortable
// density, the taller one.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { describe, test } from "node:test";
import type { LayoutSection } from "../src/lib/layoutDesign";
import { estimatedHeight, HEADER_PX, RECORD_PX, ROW_PX, SEPARATOR_PX } from "../src/lib/freeLayout";

const read = (path: string) => readFileSync(new URL(`../src/styles/${path}`, import.meta.url), "utf8");
const tokens = read("tokens.css");
const forms = read("components/forms.css");
const panels = read("components/panels.css");
const app = read("app.css");

/** The body of the first rule for `selector` in `css`. */
function rule(css: string, selector: string): string {
  const at = css.indexOf(`${selector} {`);
  assert.ok(at >= 0, `no rule for ${selector}`);
  return css.slice(at, css.indexOf("}", at));
}

/** A declaration of a rule, with var() resolved against the tokens (`scope`: the density's rule first). */
function px(body: string, prop: string, scope = ""): number {
  const m = new RegExp(`(?:^|[;{\\s])${prop}:\\s*([^;]+);`).exec(body);
  assert.ok(m, `no ${prop}`);
  return resolve(m[1].trim().split(/\s+/)[0], scope);
}

function resolve(value: string, scope: string): number {
  const v = /^var\((--[\w-]+)/.exec(value);
  if (!v) {
    assert.match(value, /^\d+(\.\d+)?px$/, value);
    return parseFloat(value);
  }
  for (const css of [scope, tokens]) {
    const d = new RegExp(`${v[1]}:\\s*([^;]+);`).exec(css);
    if (d) return resolve(d[1].trim(), scope);
  }
  assert.fail(`no token ${v[1]}`);
}

const comfortable = rule(tokens, ':root[data-density="comfortable"]');

describe("default window heights (GH#621)", () => {
  // A row of fields: the label, the input and the hint, each with the field's gap, then the grid's row gap.
  const field = rule(forms, ".field");
  const gap = px(field, "gap", comfortable);
  const row =
    px(rule(forms, ".field label,\n.field .label"), "line-height") +
    gap +
    resolve("var(--control-h)", comfortable) +
    gap +
    px(rule(forms, ".field .hint,\n.hint"), "line-height") +
    px(rule(app, ".lg-grid"), "gap", comfortable);
  // The title bar (with its bottom border) and the padding around the field grid.
  const header = px(rule(panels, ".panel-header"), "min-height") + 1 + 2 * resolve("var(--panel-p)", comfortable);

  test("a row of fields fits its inline inputs, with the label and the hint", () => {
    assert.equal(row, 82);
    assert.ok(ROW_PX >= row, `${ROW_PX} < ${row}`);
    assert.ok(HEADER_PX >= header, `${HEADER_PX} < ${header}`);
    // The old estimate, 48 px per row, cut them in half.
    assert.ok(48 < row);
  });

  test("a fields window of n rows is not clipped", () => {
    const fields = (n: number, columns = 3): LayoutSection => ({
      key: "s",
      label: "S",
      columns,
      width: 12,
      collapsed: false,
      fields: Array.from({ length: n * columns }, (_, i) => ({ field: `attributes.a${i}`, width: 1 })),
    });
    for (const n of [1, 2, 3, 5, 10]) {
      assert.ok(estimatedHeight(fields(n)) >= header + n * row, `${n} rows`);
    }
    // A separator is a line of its own (the hairline and its label) and starts a new row.
    const s = fields(1, 2);
    s.fields = [{ field: "label", width: 1 }, { separator: true, label: "More", width: 2 }, { field: "ident", width: 1 }];
    assert.equal(estimatedHeight(s), HEADER_PX + 2 * ROW_PX + SEPARATOR_PX);
  });

  test("the record details show both rows", () => {
    assert.ok(RECORD_PX >= header + 2 * row, `${RECORD_PX}`);
    assert.equal(estimatedHeight({ key: "r", label: "Record", kind: "record", columns: 3, width: 12, collapsed: false, fields: [] }), RECORD_PX);
  });
});
