// Search results mark the matched text (audit Q2): the runs a template wraps in <mark>, as plain text.
import assert from "node:assert/strict";
import { describe, test } from "node:test";
import { highlight } from "../src/lib/highlight";
import { ciRowMenu } from "../src/lib/ciRowMenu";

const marked = (text: string, term: string) =>
  highlight(text, term)
    .map((p) => (p.match ? `[${p.text}]` : p.text))
    .join("");

describe("highlight", () => {
  test("marks every case-insensitive occurrence and keeps the text unchanged", () => {
    assert.equal(marked("crm-app-01.CRM.example", "crm"), "[crm]-app-01.[CRM].example");
    assert.equal(highlight("crm-app-01", "CRM").map((p) => p.text).join(""), "crm-app-01");
  });
  test("each word of the term matches on its own; overlapping matches merge", () => {
    assert.equal(marked("10.20.5.21 fra1", "10.20 fra"), "[10.20].5.21 [fra]1");
    assert.equal(marked("aaaa", "aa aaa"), "[aaaa]");
  });
  test("no term, one-character words or no match leave the text as one plain run", () => {
    assert.deepEqual(highlight("crm-app-01", ""), [{ text: "crm-app-01", match: false }]);
    assert.deepEqual(highlight("crm-app-01", "c"), [{ text: "crm-app-01", match: false }]);
    assert.deepEqual(highlight("crm-app-01", "xyz"), [{ text: "crm-app-01", match: false }]);
  });
  test("markup in a value stays text", () => {
    assert.equal(marked("<img src=x onerror=alert(1)>", "img"), "<[img] src=x onerror=alert(1)>");
  });
});

describe("CI row menu", () => {
  test("opens the CI and its impact analysis", () => {
    assert.deepEqual(
      ciRowMenu({ id: "c1", deletedAt: null }).map((i) => [i.label, i.to]),
      [
        ["Open", "/cis/c1"],
        ["Impact analysis", "/cis/c1/impact"],
      ],
    );
  });
  test("a deleted CI has no impact analysis", () => {
    assert.deepEqual(
      ciRowMenu({ id: "c1", deletedAt: "2026-10-01T00:00:00Z" }).map((i) => i.label),
      ["Open"],
    );
  });
});
