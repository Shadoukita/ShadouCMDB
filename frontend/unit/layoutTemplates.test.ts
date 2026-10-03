// Unit tests for layout templates (SHAA-1473): class defaults, new keys and names, who uses a template,
// the class table and where the API's refusals land. Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { describe, test } from "node:test";
import type { UiClassLayout, UiSettingsDocument } from "../src/api/uiSettings";
import {
  addTemplate,
  classesUsing,
  classRows,
  classTemplateKey,
  compactLayouts,
  deletable,
  freeTemplateName,
  ownLayoutLink,
  sectionErrors,
  setClassTemplate,
  STANDARD_TEMPLATE,
  templateKeyFor,
  templateNameProblem,
} from "../src/lib/layoutTemplates";
import { emptyDocument, layoutFor } from "../src/lib/uiSettings";

const TABS: NonNullable<UiClassLayout["tabs"]> = [
  {
    key: "main",
    label: "Main",
    placement: "free",
    sections: [
      { key: "a", label: "A", columns: 3, width: 12, collapsed: false, fields: [{ field: "ident", width: 1 }] },
      { key: "b", label: "B", columns: 3, width: 12, collapsed: false, fields: [{ field: "attributes.cpu", width: 1 }] },
    ],
  },
];

/** Settings as the API returns them: two templates, Server on "hosts" (with the template's tabs copied on the class). */
function doc(): UiSettingsDocument {
  const d = emptyDocument();
  d.layoutTemplates = [
    { key: STANDARD_TEMPLATE, name: "Standard", layout: { tabs: [] } },
    { key: "hosts", name: "Hosts", description: "Servers", layout: { tabs: TABS, hiddenFields: ["attributes.asset_tag"], readOnlyFields: [] } },
  ];
  d.layouts = [{ classKey: "server", templateKey: "hosts", tabs: TABS, hiddenFields: ["attributes.asset_tag"] }];
  return d;
}
const CLASSES = [
  { id: "1", key: "server", name: "Server", isActive: true },
  { id: "2", key: "application", name: "Application", isActive: true },
  { id: "3", key: "switch", name: "switch", isActive: false },
];

describe("class defaults", () => {
  test("a class without an entry uses Standard", () => {
    assert.equal(classTemplateKey(doc(), "server"), "hosts");
    assert.equal(classTemplateKey(doc(), "application"), STANDARD_TEMPLATE);
    assert.deepEqual(classesUsing(doc(), STANDARD_TEMPLATE, ["server", "application", "switch"]), ["application", "switch"]);
    assert.deepEqual(classesUsing(doc(), "hosts", ["server", "application"]), ["server"]);
  });
  test("a new default drops the old template's tabs from the entry, so the API does not take them as an edit of the new one", () => {
    const d = doc();
    setClassTemplate(d, "server", STANDARD_TEMPLATE);
    assert.deepEqual(d.layouts, [{ classKey: "server", templateKey: STANDARD_TEMPLATE }]);
    setClassTemplate(d, "application", "hosts");
    assert.deepEqual(d.layouts[1], { classKey: "application", templateKey: "hosts" });
  });
  test("the same default again leaves the entry as it is", () => {
    const d = doc();
    setClassTemplate(d, "server", "hosts");
    assert.equal(d.layouts[0].tabs, TABS);
  });
  test("the editors send class entries with their template's key only", () => {
    const d = doc();
    d.layouts.push({ classKey: "legacy", tabs: TABS });
    assert.deepEqual(compactLayouts(d).layouts, [{ classKey: "server", templateKey: "hosts" }, { classKey: "legacy", tabs: TABS }]);
    assert.equal(d.layouts[0].tabs, TABS, "the document itself is not changed");
  });
});

describe("a class's layout", () => {
  test("its entry's (the API copies the template's layout onto it)", () => {
    assert.equal(layoutFor(doc(), "server")?.tabs?.[0].key, "main");
  });
  test("a class without an entry shows the Standard template, also when Standard has a layout", () => {
    assert.equal(layoutFor(doc(), "application"), undefined, "an empty Standard: the built-in arrangement");
    const d = doc();
    d.layoutTemplates[0].layout = { tabs: TABS, hiddenFields: ["criticality"] };
    const l = layoutFor(d, "application");
    assert.equal(l?.classKey, "application");
    assert.deepEqual(l?.hiddenFields, ["criticality"]);
    assert.deepEqual(l?.tabs?.map((t) => t.key), ["main"]);
  });
});

describe("new templates", () => {
  test("keys are lower_snake_case, start with a letter, fit 63 characters and are unique", () => {
    assert.equal(templateKeyFor("Linux Hosts (EU)", []), "linux_hosts_eu");
    assert.equal(templateKeyFor("Süße Größe", []), "susse_grosse");
    assert.equal(templateKeyFor("2024 servers", []), "t_2024_servers");
    assert.equal(templateKeyFor("!!!", []), "template");
    assert.equal(templateKeyFor("Hosts", ["hosts", "hosts_2"]), "hosts_3");
    const long = templateKeyFor("x".repeat(80), ["x".repeat(63)]);
    assert.equal(long, `${"x".repeat(61)}_2`);
    for (const k of [long, templateKeyFor("Linux Hosts (EU)", [])]) assert.match(k, /^[a-z][a-z0-9_]{0,62}$/);
  });
  test("names: required, at most 100 characters, unique ignoring case (a rename may keep its own)", () => {
    const ts = doc().layoutTemplates;
    assert.equal(templateNameProblem("  ", ts), "required");
    assert.equal(templateNameProblem("x".repeat(101), ts), "tooLong");
    assert.equal(templateNameProblem(" hosts ", ts), "taken");
    assert.equal(templateNameProblem("HOSTS", ts, "hosts"), null);
    assert.equal(templateNameProblem("Network", ts), null);
  });
  test("a free name for a copy", () => {
    const ts = [...doc().layoutTemplates, { key: "c", name: "Hosts (copy)" }];
    assert.equal(freeTemplateName("Hosts (copy)", ts), "Hosts (copy) (2)");
    assert.equal(freeTemplateName("Network", ts), "Network");
  });
  test("added with a copy of the layout it starts from", () => {
    const d = doc();
    const t = addTemplate(d, " Hosts EU ", d.layoutTemplates[1].layout, " ");
    assert.deepEqual(t, { key: "hosts_eu", name: "Hosts EU", layout: { tabs: TABS, hiddenFields: ["attributes.asset_tag"], readOnlyFields: [] } });
    assert.notEqual(t.layout!.tabs, TABS);
    t.layout!.tabs[0].label = "Changed";
    assert.equal(TABS[0].label, "Main");
    assert.deepEqual(addTemplate(d, "Blank", undefined, "Nothing yet").layout, { tabs: [], hiddenFields: [], readOnlyFields: [] });
    assert.equal(d.layoutTemplates.at(-1)!.description, "Nothing yet");
  });
});

describe("delete", () => {
  test("only a template nobody uses, never Standard; not before the CIs are counted", () => {
    assert.equal(deletable("hosts", { classKeys: [], ciCount: 0 }), true);
    assert.equal(deletable("hosts", { classKeys: ["server"], ciCount: 0 }), false);
    assert.equal(deletable("hosts", { classKeys: [], ciCount: 2 }), false);
    assert.equal(deletable("hosts", { classKeys: [], ciCount: null }), false);
    assert.equal(deletable("hosts", { classKeys: [], ciCount: undefined }), false);
    assert.equal(deletable(STANDARD_TEMPLATE, { classKeys: [], ciCount: 0 }), false);
  });
});

describe("class table", () => {
  const names = (rows: { name: string }[]) => rows.map((r) => r.name);
  test("every class with its default template, by class name", () => {
    const rows = classRows(CLASSES, doc(), {});
    assert.deepEqual(names(rows), ["Application", "Server", "switch"]);
    assert.deepEqual(
      rows.map((r) => [r.key, r.templateKey, r.templateName]),
      [
        ["application", "standard", "Standard"],
        ["server", "hosts", "Hosts"],
        ["switch", "standard", "Standard"],
      ],
    );
  });
  test("searched by class name, class key or template name; filtered by template", () => {
    assert.deepEqual(names(classRows(CLASSES, doc(), { q: "SERV" })), ["Server"]);
    assert.deepEqual(names(classRows(CLASSES, doc(), { q: "applic" })), ["Application"]);
    assert.deepEqual(names(classRows(CLASSES, doc(), { q: "standard" })), ["Application", "switch"]);
    assert.deepEqual(names(classRows(CLASSES, doc(), { uses: "hosts" })), ["Server"]);
    assert.deepEqual(names(classRows(CLASSES, doc(), { uses: "hosts", q: "app" })), []);
  });
  test("sorted by class or template, either way", () => {
    assert.deepEqual(names(classRows(CLASSES, doc(), { sort: "-class" })), ["switch", "Server", "Application"]);
    assert.deepEqual(names(classRows(CLASSES, doc(), { sort: "template" })), ["Server", "Application", "switch"]);
    assert.deepEqual(names(classRows(CLASSES, doc(), { sort: "-template" })), ["Application", "switch", "Server"]);
  });
  test("CIs with their own layout: counted per class, unknown shown apart, sortable, linked to the inventory", () => {
    assert.deepEqual(
      classRows(CLASSES, doc(), {}).map((r) => r.ownLayoutCount),
      [undefined, undefined, undefined],
      "not counted yet",
    );
    const owned = new Map<string, number | null>([
      ["application", 0],
      ["server", 3],
      ["switch", null],
    ]);
    assert.deepEqual(
      classRows(CLASSES, doc(), { owned }).map((r) => [r.key, r.ownLayoutCount]),
      [
        ["application", 0],
        ["server", 3],
        ["switch", null],
      ],
    );
    assert.equal(classRows(CLASSES, doc(), { owned: new Map() })[0].ownLayoutCount, null, "a class the counts leave out is unknown");
    assert.deepEqual(names(classRows(CLASSES, doc(), { owned, sort: "owned" })), ["Application", "Server", "switch"]);
    assert.deepEqual(names(classRows(CLASSES, doc(), { owned, sort: "-owned" })), ["Server", "Application", "switch"], "unknown last either way");
    assert.deepEqual(ownLayoutLink("c-1"), { path: "/cis", query: { classId: "c-1", includeSubclasses: "false", ownLayout: "true" } });
  });
  test("a few hundred classes", () => {
    const many = Array.from({ length: 500 }, (_, i) => ({ id: String(i), key: `c_${i}`, name: `Class ${String(i).padStart(3, "0")}`, isActive: true }));
    const rows = classRows(many, doc(), { q: "class 49" });
    assert.deepEqual(names(rows).slice(0, 2), ["Class 490", "Class 491"]);
    assert.equal(rows.length, 10);
  });
});

describe("API refusals next to their section", () => {
  const layout: UiClassLayout = { classKey: "server", tabs: TABS };
  const error = (field: string) => ({ code: "VALIDATION_ERROR", details: [{ field, message: "Bad" }] });
  test("a template's paths and a CI's own layout's paths", () => {
    assert.deepEqual(sectionErrors(error("settings.layoutTemplates.1.layout.tabs.0.sections.1.kind"), { prefix: "settings.layoutTemplates.1.layout", layout }), {
      b: [{ path: "settings.layoutTemplates.1.layout.tabs.0.sections.1.kind", message: "Bad" }],
    });
    assert.deepEqual(sectionErrors(error("layout.tabs.0.sections.0"), { prefix: "layout", layout }), { a: [{ path: "layout.tabs.0.sections.0", message: "Bad" }] });
  });
  test("another template's paths, unknown sections and other errors are left out", () => {
    const sent = { prefix: "settings.layoutTemplates.1.layout", layout };
    assert.deepEqual(sectionErrors(error("settings.layoutTemplates.10.layout.tabs.0.sections.0.kind"), sent), {});
    assert.deepEqual(sectionErrors(error("settings.layoutTemplates.1.layout.tabs.0.sections.9.kind"), sent), {});
    assert.deepEqual(sectionErrors(error("settings.layoutTemplates.1.layout.hiddenFields.0"), sent), {});
    assert.deepEqual(sectionErrors(new Error("boom"), sent), {});
    assert.deepEqual(sectionErrors(error("layout.tabs.0.sections.0"), null), {});
  });
});
