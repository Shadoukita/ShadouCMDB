import assert from "node:assert/strict";
import { describe, test } from "node:test";
import type { Schemas } from "../src/api/client";
import {
  blankForm,
  checkMapping,
  formFromMapping,
  matchedText,
  normaliseHeader,
  optionIndex,
  placeApiErrors,
  remapByHeaders,
  targetGroups,
  toDefinition,
  toMapping,
} from "../src/lib/importMapping";

const T = "2026-09-30T00:00:00Z";
const cls = (id: string, parentId: string | null = null, isAbstract = false) =>
  ({ id, key: id, name: id.toUpperCase(), parentId, isAbstract, isActive: true }) as Schemas["CiClass"];
const attr = (key: string, classId: string, extra: Partial<Schemas["AttributeDefinition"]> = {}) =>
  ({
    id: `a-${key}`,
    classId,
    key,
    label: key[0]!.toUpperCase() + key.slice(1),
    dataType: "text",
    isRequired: false,
    isActive: true,
    referenceClassId: null,
    defaultValue: null,
    ...extra,
  }) as Schemas["AttributeDefinition"];
const type = (id: string, isDirectional = true) =>
  ({ id, key: id, name: id, forwardLabel: `${id} fwd`, reverseLabel: `${id} rev`, isDirectional, isActive: true, sortOrder: 0 }) as Schemas["RelationshipType"];
const rule = (relationshipTypeId: string, sourceClassId: string, targetClassId: string) =>
  ({ id: `${relationshipTypeId}-${sourceClassId}-${targetClassId}`, relationshipTypeId, sourceClassId, targetClassId, createdAt: T, updatedAt: T }) as Schemas["RelationshipRule"];

// hardware (abstract) > server; application; location
const classes = [cls("hardware", null, true), cls("server", "hardware"), cls("application"), cls("location")];
const attributes = [
  attr("hostname", "server", { isRequired: true }),
  attr("serial", "hardware"),
  attr("site", "server", { dataType: "reference", referenceClassId: "location" }),
  attr("retired", "server", { isActive: false }),
];
const types = [type("runs_on"), type("located_in"), type("connected_to", false), type("unused")];
const rules = [
  rule("runs_on", "application", "hardware"),
  rule("located_in", "hardware", "location"),
  rule("connected_to", "hardware", "hardware"),
  rule("unused", "application", "location"),
];
const groups = targetGroups({ classId: "server", classes, attributes, types, rules });
const options = optionIndex(groups);

describe("mapping targets come from the class's metadata, not a per-class list", () => {
  test("active attributes, inherited ones named by their class", () => {
    assert.deepEqual(
      groups.attributes.map((o) => [o.value, o.definedBy]),
      [
        ["attr:hostname", undefined],
        ["attr:serial", "HARDWARE"],
        ["attr:site", undefined],
      ],
    );
  });
  test("relationship types whose rules allow the class (or an ancestor) on either end", () => {
    const rel = groups.relationships.map((o) => [o.value, o.label, o.otherClassIds]);
    assert.deepEqual(rel, [
      ["rel:runs_on:incoming", "runs_on rev", ["application"]],
      ["rel:located_in:outgoing", "located_in fwd", ["location"]],
      // Symmetric: offered once.
      ["rel:connected_to:outgoing", "connected_to fwd", ["hardware", "server"]],
    ]);
  });
});

describe("the mapping sent to the API", () => {
  test("every file column in order, so columns[i] is file column i", () => {
    const form = blankForm(4, ";", "server");
    form.keyField = "attributes.hostname";
    form.columns[0]!.target = "attr:hostname";
    form.columns[1]!.target = "attr:site";
    form.columns[1]!.matchBy = "attribute";
    form.columns[1]!.matchAttribute = "code";
    form.columns[3]!.target = "rel:runs_on:incoming";
    form.columns[3]!.emptyCells = "clear";
    const m = toMapping(form, options);
    assert.equal(m.options?.decimalSeparator, ",", "; files default to the decimal comma");
    assert.deepEqual(
      m.columns.map((c) => [c.index, c.target]),
      [
        [0, { kind: "attribute", key: "hostname", match: null }],
        [1, { kind: "attribute", key: "site", match: { by: "attribute", attributeKey: "code" } }],
        [2, { kind: "ignore" }],
        [3, { kind: "relationship", typeKey: "runs_on", direction: "incoming", match: { by: "label", attributeKey: null } }],
      ],
    );
    assert.equal(m.columns[3]!.emptyCells, "clear");
    // And back: a reload shows the same form.
    const again = formFromMapping(m, 4, ";");
    assert.deepEqual(again.columns.map((c) => c.target), form.columns.map((c) => c.target));
    assert.equal(again.columns[1]!.matchAttribute, "code");
  });
});

describe("checks before saving", () => {
  test("mapped twice, key not mapped, required not mapped, ident for non-administrators", () => {
    const form = blankForm(3, ",", "server");
    form.keyField = "attributes.serial";
    form.columns[0]!.target = "ident";
    form.columns[1]!.target = "attr:site";
    form.columns[2]!.target = "attr:site";
    const problems = checkMapping(form, options, attributes, false);
    assert.deepEqual(
      problems.map((p) => [p.column ?? p.control, p.message.split(":")[0]]),
      [
        [0, "Only administrators can set the ident of new CIs. Map it only to match existing CIs."],
        [2, "Mapped twice"],
        ["import-key", "The match key Serial is not mapped to a column."],
        ["import-mapping-table", "Hostname is required; map a column to it."],
      ],
    );
    form.mode = "update_only";
    form.columns[0]!.target = "attr:hostname";
    form.columns[2]!.target = "attr:serial";
    assert.deepEqual(checkMapping(form, options, attributes, false), []);
  });
});

test("API field errors land on their column's row, the rest above the table", () => {
  const placed = placeApiErrors([
    { field: "columns[3].target.key", message: "This attribute is retired", code: "attribute_inactive" },
    { field: "key.field", message: "The key's column is not mapped", code: "key_unmapped" },
    { field: "attributes.os", message: "OS is required; map a column to it", code: "required_unmapped" },
  ]);
  assert.deepEqual(
    placed.map((p) => p.column ?? p.control),
    [3, "import-key", "import-mapping-table"],
  );
});

describe("saved mappings and corrected files", () => {
  const mapping: Schemas["ImportMapping"] = {
    classKey: "server",
    mode: "create_or_update",
    key: { field: "attributes.hostname" },
    emptyCells: "ignore",
    columns: [
      { index: 0, target: { kind: "attribute", key: "hostname" } },
      { index: 1, target: { kind: "attribute", key: "os" }, emptyCells: "clear" },
      { index: 2, target: { kind: "ignore" } },
    ],
  };

  test("headers compare like the server's auto-match", () => {
    assert.equal(normaliseHeader("  Host_Name.-x "), "host name x");
    assert.equal(normaliseHeader("ＨＯＳＴ"), "host");
  });

  test("a definition names columns by header and keeps ignore targets", () => {
    const d = toDefinition(mapping, ["Hostname", "OS", "Notes"]);
    assert.equal("classKey" in d, false);
    assert.deepEqual(
      d.columns.map((c) => [c.header, c.target.kind, c.emptyCells]),
      [
        ["Hostname", "attribute", null],
        ["OS", "attribute", "clear"],
        ["Notes", "ignore", null],
      ],
    );
  });

  test("a corrected file keeps targets by header, whatever the column order", () => {
    const m = remapByHeaders(mapping, ["Hostname", "OS", "Notes"], ["os", "Extra", " HOSTNAME "]);
    assert.deepEqual(
      m.columns.map((c) => [c.index, c.target.kind === "attribute" ? c.target.key : c.target.kind]),
      [
        [2, "hostname"],
        [0, "os"],
      ],
    );
    assert.equal(m.classKey, "server");
  });

  test("status wording for suggested columns", () => {
    assert.equal(matchedText("key", null), "Matched by key");
    assert.equal(matchedText("label", null), "Matched by name");
    assert.equal(matchedText("saved_mapping", null), "From saved mapping");
    assert.match(matchedText(null, "ambiguous_label"), /several fields/);
    assert.equal(matchedText(null, null), "Not mapped");
  });
});
