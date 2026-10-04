import assert from "node:assert/strict";
import { describe, test } from "node:test";
import { accept, barPatch, parseBar, sameAsUrl, serializeBar, suggest, tokenize, type BarCatalogue } from "../src/lib/queryBar";

const catalogue: BarCatalogue = {
  classes: [
    { id: "c-srv", key: "server", name: "Server" },
    { id: "c-vm", key: "vm", name: "Virtual machine" },
  ],
  criticality: [
    { id: "k-hi", key: "high", name: "High" },
    { id: "k-lo", key: "low", name: "Low" },
  ],
  lists: [
    { id: "l-env", key: "environment", name: "Environment" },
    { id: "l-st", key: "ci_status", name: "Status" },
    { id: "l-crit", key: "criticality_list", name: "Criticality", systemRole: "criticality" },
  ],
  values: [
    { id: "v-prod", listId: "l-env", key: "prod", name: "Production" },
    { id: "v-test", listId: "l-env", key: "test", name: "Test" },
    { id: "v-in", listId: "l-st", key: "in_service", name: "In service" },
  ],
};

describe("tokenize", () => {
  test("splits at whitespace outside quotes", () => {
    assert.deepEqual(
      tokenize(' class:server  "web 01" x').map((t) => [t.start, t.end, t.raw]),
      [
        [1, 13, "class:server"],
        [15, 23, '"web 01"'],
        [24, 25, "x"],
      ],
    );
  });
});

describe("parseBar", () => {
  test("maps tokens onto the URL filters and words onto q", () => {
    const p = parseBar("class:server,vm environment:prod,Test ci_status:in_service criticality:high validity:all deleted:only ip:10.0.0.0/8 layout:own template:std web 01", catalogue);
    assert.deepEqual(p.errors, []);
    assert.equal(p.pending, false);
    assert.deepEqual(p.patch, {
      classId: "c-srv,c-vm",
      lookupValueId: "v-prod,v-test,v-in",
      criticalityValueId: "k-hi",
      active: "all",
      deleted: "only",
      ipWithin: "10.0.0.0/8",
      ownLayout: "true",
      layoutTemplate: "std",
      q: "web 01",
    });
  });

  test("defaults remove their parameter, and quoted text is searched as typed", () => {
    const p = parseBar('validity:active deleted:hide "note:x y" class:"Virtual machine"', catalogue);
    assert.deepEqual(p.errors, []);
    assert.equal(p.patch.active, undefined);
    assert.equal(p.patch.deleted, undefined);
    assert.equal(p.patch.q, "note:x y");
    assert.equal(p.patch.classId, "c-vm");
    assert.ok(p.named.has("active"));
  });

  test("an IPv6 network keeps its colons", () => {
    assert.equal(parseBar("ip:2001:db8::/32", catalogue).patch.ipWithin, "2001:db8::/32");
  });

  test("reports what the API cannot filter on", () => {
    const codes = parseBar("-class:server owner:me class:nope validity:sometimes validity:all criticality_list:high", catalogue).errors.map((e) => [e.code, e.key]);
    assert.deepEqual(codes, [
      ["negation", "class"],
      ["unknownKey", "owner"],
      ["unknownValue", "class"],
      ["badValue", "validity"],
      ["twice", "validity"],
      // The criticality list is reached as criticality:, not by its own key.
      ["unknownKey", "criticality_list"],
    ]);
  });

  test("an unfinished token or a loading catalogue waits instead of failing", () => {
    assert.equal(parseBar("class:", catalogue).pending, true);
    const loading = parseBar("class:server environment:prod", {});
    assert.equal(loading.pending, true);
    assert.deepEqual(loading.errors, []);
  });
});

describe("serializeBar", () => {
  test("writes the URL's filters as tokens, in a fixed order", () => {
    const query = { q: "web note:x", classId: "c-vm", lookupValueId: "v-in,v-prod", criticalityValueId: "k-lo", active: "false", deleted: "include", ownLayout: "false", sort: "-label" };
    const s = serializeBar(query, catalogue);
    assert.equal(s.text, 'class:vm environment:prod ci_status:in_service criticality:low validity:inactive deleted:include layout:default web "note:x"');
    assert.equal(s.unrepresented.size, 0);
    // And back: the same filters.
    const p = parseBar(s.text, catalogue);
    assert.ok(sameAsUrl(query, barPatch(p, s.unrepresented)));
  });

  test("an id it cannot name stays out of the text, and applying the text keeps it", () => {
    const query = { classId: "c-gone", lookupValueId: "v-prod", q: "x" };
    const s = serializeBar(query, catalogue);
    assert.equal(s.text, "environment:prod x");
    assert.deepEqual([...s.unrepresented], ["classId"]);
    const patch = barPatch(parseBar("environment:test", catalogue), s.unrepresented);
    assert.equal("classId" in patch, false);
    assert.deepEqual(patch, { lookupValueId: "v-test", criticalityValueId: undefined, active: undefined, deleted: undefined, ipWithin: undefined, ownLayout: undefined, layoutTemplate: undefined, q: undefined });
    // Naming it replaces it.
    assert.equal(barPatch(parseBar("class:vm", catalogue), s.unrepresented).classId, "c-vm");
  });

  test("a bare search term is the text as it is", () => {
    assert.equal(serializeBar({ q: "crm" }, catalogue).text, "crm");
  });
});

describe("sameAsUrl", () => {
  test("ignores id order and extra whitespace", () => {
    assert.ok(sameAsUrl({ classId: "c-vm,c-srv", q: "a  b" }, { classId: "c-srv,c-vm", q: "a b" }));
    assert.ok(!sameAsUrl({ classId: "c-vm" }, { classId: undefined }));
  });
});

describe("suggest and accept", () => {
  const labels = { class: "Class", "validity.all": "Show inactive" };

  test("offers every key in an empty spot, and those that start with a begun word", () => {
    const all = suggest("", 0, catalogue, labels)!;
    assert.equal(all.kind, "key");
    assert.deepEqual(
      all.items.map((i) => i.insert),
      ["class:", "criticality:", "validity:", "deleted:", "ip:", "layout:", "template:", "environment:", "ci_status:"],
    );
    assert.deepEqual(
      suggest("web cl", 6, catalogue, labels)!.items.map((i) => i.insert),
      ["class:"],
    );
    assert.equal(suggest("webserver", 9, catalogue, labels), null);
  });

  test("offers a key's values by key or name, after the last comma, without those already listed", () => {
    const s = suggest("class:server,v", 14, catalogue, labels)!;
    assert.equal(s.kind, "value");
    assert.deepEqual(s.items.map((i) => i.insert), ["vm"]);
    assert.deepEqual(suggest("environment:produ", 17, catalogue, labels)!.items.map((i) => i.insert), ["prod"]);
    assert.deepEqual(suggest("validity:", 9, catalogue, labels)!.items.map((i) => i.detail), ["active", "Show inactive", "inactive"]);
  });

  test("accepting a key keeps the caret after the colon; a value ends its token", () => {
    const k = suggest("x cl", 4, catalogue, labels)!;
    assert.deepEqual(accept("x cl", k, k.items[0]!), { text: "x class:", caret: 8 });
    const v = suggest("class:se web", 8, catalogue, labels)!;
    assert.deepEqual(accept("class:se web", v, v.items[0]!), { text: "class:server web", caret: 12 });
    const end = suggest("class:se", 8, catalogue, labels)!;
    assert.deepEqual(accept("class:se", end, end.items[0]!), { text: "class:server ", caret: 13 });
  });
});
