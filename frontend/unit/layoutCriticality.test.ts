import assert from "node:assert/strict";
import { describe, test } from "node:test";
import { hideField, isCore, placeField } from "../src/lib/layoutDesign";
import { CORE_FIELDS, DETAIL_CORE, DETAIL_RECORD, LOCKED_FIELDS, SORT_FIELDS, builtInLayout, resolveLayout } from "../src/lib/uiSettings";

const fieldsOf = (tabs: ReturnType<typeof resolveLayout>) => tabs.flatMap((t) => t.sections.flatMap((s) => s.fields.map((f) => f.field)));

describe("criticality in layouts and list views", () => {
  test("is a form field a layout places, but may hide (the API's CORE_FIELDS leave it out)", () => {
    assert.ok(CORE_FIELDS.includes("criticality"));
    assert.deepEqual(DETAIL_CORE, ["ident", "validFrom", "validUntil", "active", "criticality"]);
    assert.ok(!DETAIL_RECORD.includes("criticality"));
    assert.deepEqual(LOCKED_FIELDS, ["ident", "validFrom", "validUntil"]);
    assert.equal(isCore("criticality"), false);
  });

  test("a class without a layout shows it in General, after the other core fields", () => {
    const [tab] = resolveLayout(builtInLayout("server"), [], CORE_FIELDS);
    assert.deepEqual(tab.sections[0].fields.map((f) => f.field), ["ident", "validFrom", "validUntil", "criticality"]);
  });

  test("a layout places it where the administrator put it, and hides it", () => {
    const l = builtInLayout("server");
    l.tabs = [{ key: "main", label: "Main", sections: [{ key: "risk", label: "Risk", columns: 3, width: 12, fields: [] }] }];
    placeField(l, "criticality", "risk");
    const tabs = resolveLayout(l, [], CORE_FIELDS);
    assert.deepEqual(tabs[0].sections[0].fields.map((f) => f.field), ["criticality"]);
    assert.ok(!tabs[0].sections.find((s) => s.key === "_general")!.fields.some((f) => f.field === "criticality"));

    assert.equal(hideField(l, "criticality"), true);
    assert.deepEqual(l.hiddenFields, ["criticality"]);
    assert.ok(!fieldsOf(resolveLayout(l, [], CORE_FIELDS)).includes("criticality"));
    assert.equal(hideField(l, "ident"), false);
  });

  test("list views can sort by it", () => {
    assert.ok(SORT_FIELDS.some((s) => s.field === "criticality"));
  });
});
