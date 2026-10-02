import assert from "node:assert/strict";
import { describe, test } from "node:test";
import type { ApiErrorDetail } from "../src/api/client";
import type { SavedView } from "../src/api/savedViews";
import {
  decideOpen,
  definitionFromUrl,
  defaultView,
  deleteConsequence,
  degradedLines,
  groupViews,
  isModified,
  saveFailure,
  typeaheadIndex,
  viewActions,
  viewQuery,
  type OpenInput,
} from "../src/lib/savedViews";

const CLASSES = [
  { id: "c-server", key: "server" },
  { id: "c-vm", key: "vm" },
];

function view(over: Partial<SavedView> & { query?: NonNullable<SavedView["resolved"]["query"]>; columns?: string[] } = {}): SavedView {
  const { query, columns, ...rest } = over;
  return {
    id: "v1",
    context: "inventory",
    visibility: "personal",
    name: "Production servers",
    description: null,
    definition: {},
    resolved: {
      state: "ok",
      query: query ?? { classId: "c-server", includeSubclasses: "true", lookupValueId: "lv-prod", sort: "-attributes.os", limit: 25 },
      columns: columns ?? ["label", "attributes.os"],
      issues: [],
    },
    home: "server",
    isDefault: false,
    canEdit: true,
    version: 3,
    createdAt: "2026-10-01T10:00:00Z",
    createdBy: { id: "u1", name: "Ada" },
    updatedAt: "2026-10-01T10:00:00Z",
    updatedBy: { id: "u1", name: "Ada" },
    ...rest,
  };
}

/** An ApiError's shape (the client module needs the browser's config). */
class ApiError {
  status: number;
  code: string;
  message: string;
  details: ApiErrorDetail[];
  constructor(status: number, code: string, message: string, details: ApiErrorDetail[] = []) {
    this.status = status;
    this.code = code;
    this.message = message;
    this.details = details;
  }
}

describe("viewQuery", () => {
  test("writes every parameter of the resolved view with view=<id>", () => {
    assert.deepEqual(viewQuery(view()), {
      view: "v1",
      classId: "c-server",
      lookupValueId: "lv-prod",
      sort: "-attributes.os",
      limit: "25",
      columns: "label,attributes.os",
    });
  });
  test("leaves out the list's defaults and writes the label sort when the view has none", () => {
    const q = viewQuery(view({ query: { active: "true", deleted: "exclude" }, columns: [] }));
    assert.deepEqual(q, { view: "v1", sort: "label" });
  });
  test("search views carry no sort and no columns", () => {
    const q = viewQuery(view({ context: "search", query: { q: "db", classId: "c-vm", active: "all" }, columns: [] }));
    assert.deepEqual(q, { view: "v1", q: "db", classId: "c-vm", active: "all" });
  });
  test("an unavailable view is never applied", () => {
    assert.equal(viewQuery(view({ resolved: { state: "unavailable", query: null, columns: [], issues: [] } })), null);
  });
});

describe("isModified", () => {
  const v = view();
  test("the URL the view wrote is not modified, whatever the order of ids", () => {
    assert.equal(isModified(viewQuery(v)!, v), false);
    assert.equal(isModified({ ...viewQuery(v)!, lookupValueId: "lv-prod", offset: "50" }, v), false);
    const many = view({ query: { classId: "a,b", sort: "label" }, columns: [] });
    assert.equal(isModified({ view: "v1", classId: "b,a", sort: "label" }, many), false);
  });
  test("a changed sort, filter or column is", () => {
    assert.equal(isModified({ ...viewQuery(v)!, sort: "label" }, v), true);
    assert.equal(isModified({ ...viewQuery(v)!, q: "web" }, v), true);
    assert.equal(isModified({ ...viewQuery(v)!, columns: "attributes.os,label" }, v), true);
  });
  test("a missing page size compares as the list's effective one", () => {
    const { limit: _l, ...noLimit } = viewQuery(v)!;
    assert.equal(isModified(noLimit, v, { limit: 25 }), false);
    assert.equal(isModified(noLimit, v, { limit: 50 }), true);
  });
});

describe("definitionFromUrl", () => {
  const catalog = {
    classes: CLASSES,
    lists: [
      { id: "l-env", key: "environment" },
      { id: "l-crit", key: "criticality" },
    ],
    values: [
      { id: "lv-prod", listId: "l-env", key: "production" },
      { id: "lv-test", listId: "l-env", key: "test" },
      { id: "cv-high", listId: "l-crit", key: "high" },
    ],
  };
  test("turns ids into keys and keeps sort, columns and page size", () => {
    const r = definitionFromUrl(
      {
        view: "v1",
        classId: "c-server",
        lookupValueId: "lv-prod,lv-test",
        criticalityValueId: "cv-high",
        q: " web ",
        active: "all",
        deleted: "only",
        ipWithin: "10.0.0.0/8",
        sort: "-attributes.os",
        columns: "label,attributes.os,bogus",
        limit: "5",
      },
      "inventory",
      catalog,
    );
    assert.deepEqual(r.unknown, []);
    assert.deepEqual(r.definition, {
      classKeys: ["server"],
      includeSubclasses: true,
      filters: { q: "web", lookups: { environment: ["production", "test"], criticality: ["high"] }, active: "all", deleted: "only", ipWithin: "10.0.0.0/8" },
      sort: { field: "attributes.os", direction: "desc" },
      columns: ["label", "attributes.os"],
      pageSize: 10,
    });
  });
  test("columns the operator did not choose are not saved (the list view's apply)", () => {
    const r = definitionFromUrl({ classId: "c-vm" }, "inventory", catalog);
    assert.deepEqual(r.definition, { classKeys: ["vm"], includeSubclasses: true, filters: {} });
  });
  test("search definitions have no sort and no columns", () => {
    const r = definitionFromUrl({ q: "db", sort: "label", columns: "label" }, "search", catalog);
    assert.deepEqual(r.definition, { classKeys: [], includeSubclasses: true, filters: { q: "db" } });
  });
  test("reports ids it does not know", () => {
    assert.deepEqual(definitionFromUrl({ classId: "gone", lookupValueId: "lv-gone" }, "inventory", catalog).unknown, ["gone", "lv-gone"]);
  });
});

describe("decideOpen (§1.3)", () => {
  const base: OpenInput = { query: {}, context: "inventory", classes: CLASSES, views: [], viewsFailed: false, linked: undefined };
  test("1. a URL with state is shown as it is, also with view=", () => {
    assert.deepEqual(decideOpen({ ...base, query: { q: "web" }, views: undefined }), { kind: "none" });
    assert.deepEqual(decideOpen({ ...base, query: { view: "v1", sort: "label" }, views: undefined }), { kind: "none" });
  });
  test("2. a view=-only link applies the view once it is loaded", () => {
    const q = { view: "v1" };
    assert.deepEqual(decideOpen({ ...base, query: q, views: undefined }), { kind: "wait" });
    assert.deepEqual(decideOpen({ ...base, query: q, views: undefined, linked: { view: view() } }), { kind: "replace", query: viewQuery(view()) });
    assert.deepEqual(decideOpen({ ...base, query: q, views: [view()] }), { kind: "replace", query: viewQuery(view()) });
  });
  test("2. a view that is not available drops view= with the same banner for deleted and forbidden", () => {
    assert.deepEqual(decideOpen({ ...base, query: { view: "v9" }, linked: { failed: true } }), {
      kind: "replace",
      query: {},
      notice: { kind: "notAvailable" },
    });
    const search = view({ context: "search" });
    assert.deepEqual(decideOpen({ ...base, query: { view: "v1" }, linked: { view: search } }).kind, "replace");
    const gone = view({ resolved: { state: "unavailable", query: null, columns: [], issues: [] } });
    assert.deepEqual(decideOpen({ ...base, query: { view: "v1" }, linked: { view: gone } }), {
      kind: "replace",
      query: {},
      notice: { kind: "unavailable", name: "Production servers" },
    });
  });
  test("3. the user's default for the class list applies, after the views have loaded", () => {
    const def = view({ isDefault: true });
    const q = { classId: "c-server" };
    assert.deepEqual(decideOpen({ ...base, query: q, views: undefined }), { kind: "wait" });
    assert.deepEqual(decideOpen({ ...base, query: q, classes: undefined, views: [def] }), { kind: "wait" });
    assert.deepEqual(decideOpen({ ...base, query: q, views: [def] }), { kind: "replace", query: viewQuery(def) });
    // Not for another class's list, not when the views failed to load, never on search.
    assert.deepEqual(decideOpen({ ...base, query: { classId: "c-vm" }, views: [def] }), { kind: "none" });
    assert.deepEqual(decideOpen({ ...base, query: q, views: undefined, viewsFailed: true }), { kind: "none" });
    assert.deepEqual(decideOpen({ ...base, context: "search", query: {}, views: [def] }), { kind: "none" });
  });
  test("3. the unscoped inventory has its own default", () => {
    const unscoped = view({ id: "v2", isDefault: true, home: null, query: { sort: "-updatedAt" }, columns: [] });
    assert.deepEqual(decideOpen({ ...base, views: [view({ isDefault: true }), unscoped] }), { kind: "replace", query: { view: "v2", sort: "-updatedAt" } });
  });
  test("4. no default: the list view and built-in defaults apply", () => {
    assert.deepEqual(decideOpen({ ...base, query: { classId: "c-server" }, views: [view()] }), { kind: "none" });
  });
});

describe("menu", () => {
  test("groups views alphabetically, personal first", () => {
    const g = groupViews([
      view({ id: "a", name: "zeta" }),
      view({ id: "b", name: "Alpha", visibility: "shared" }),
      view({ id: "c", name: "beta" }),
    ]);
    assert.deepEqual(
      g.mine.map((v) => v.name),
      ["beta", "zeta"],
    );
    assert.deepEqual(
      g.shared.map((v) => v.name),
      ["Alpha"],
    );
  });
  test("typeahead moves to the next item starting with the letter, wrapping", () => {
    const labels = ["Alpha", "Beta", "apps", "Save view"];
    assert.equal(typeaheadIndex(labels, 0, "a"), 2);
    assert.equal(typeaheadIndex(labels, 2, "A"), 0);
    assert.equal(typeaheadIndex(labels, -1, "s"), 3);
    assert.equal(typeaheadIndex(labels, 0, "x"), -1);
  });
  test("defaultView skips unavailable views", () => {
    const off = view({ isDefault: true, resolved: { state: "unavailable", query: null, columns: [], issues: [] } });
    assert.equal(defaultView([off], "server"), undefined);
  });
  test("actions follow the view's rights and context (§1.2)", () => {
    const opts = { canShare: false, context: "inventory" as const, home: "server" };
    assert.deepEqual(viewActions(undefined, opts), {
      save: false,
      saveAs: true,
      rename: false,
      setDefault: false,
      clearDefault: false,
      copyToMine: false,
      shareCopy: false,
      delete: false,
    });
    const own = viewActions(view(), opts);
    assert.equal(own.save && own.rename && own.delete && own.setDefault, true);
    assert.equal(own.shareCopy, false);
    assert.equal(viewActions(view(), { ...opts, canShare: true }).shareCopy, true);
    // A shared view the user cannot edit: copy, default, no edit.
    const shared = viewActions(view({ visibility: "shared", canEdit: false }), opts);
    assert.deepEqual([shared.save, shared.rename, shared.delete, shared.copyToMine, shared.setDefault], [false, false, false, true, true]);
    // Search views are never a default (D7); a view is the default of its own home only.
    assert.equal(viewActions(view({ context: "search", home: null }), { ...opts, context: "search", home: null }).setDefault, false);
    assert.equal(viewActions(view(), { ...opts, home: "vm" }).setDefault, false);
    assert.equal(viewActions(view({ isDefault: true }), opts).clearDefault, true);
  });
});

describe("states (§1.5)", () => {
  test("delete says exactly what goes", () => {
    assert.equal(deleteConsequence(view(), "Server"), "This cannot be undone.");
    assert.match(deleteConsequence(view({ isDefault: true }), "Server"), /your default for Server\. The Server list will open with the standard columns/);
    assert.match(deleteConsequence(view({ visibility: "shared", defaultCount: 12 }), "Server"), /shared with everyone and is the default for 12 users\. .*CIs are not affected/);
    assert.match(deleteConsequence(view({ visibility: "shared", defaultCount: 1 }), "Server"), /default for 1 user\./);
  });
  test("API errors: name errors go to the field, conflicts and the limit get their own presentation", () => {
    const dup = new ApiError(409, "CONFLICT", "You already have a view of this context with that name.", [
      { field: "name", message: "You already have a view of this context with that name.", code: "duplicate_name" },
    ]);
    assert.deepEqual(saveFailure(dup), { kind: "fields", name: "You already have a view of this context with that name.", definition: [] });
    assert.equal(saveFailure(new ApiError(409, "VERSION_CONFLICT", "changed")).kind, "conflict");
    const limit = new ApiError(409, "CONFLICT", "You have reached the limit of 200 saved views.", [{ field: "(root)", message: "x", code: "limit_reached" }]);
    assert.deepEqual(saveFailure(limit), { kind: "limit", message: "You have reached the limit of 200 saved views." });
    const def = new ApiError(400, "VALIDATION_ERROR", "Invalid", [
      { field: "definition.sort.field", message: "Attribute \"rack\" is a reference and cannot be sorted on", code: "not_sortable" },
      { field: "definition.columns.2", message: "Attribute \"x\" is not an active attribute", code: "unknown_attribute" },
    ]);
    assert.deepEqual(saveFailure(def), {
      kind: "fields",
      definition: ['Sort: Attribute "rack" is a reference and cannot be sorted on', 'Column 3: Attribute "x" is not an active attribute'],
    });
  });
  test("degraded views list what was dropped; readers of a shared view only see a count", () => {
    const issues = [
      { path: "definition.columns.2", code: "unknown_attribute" as const, severity: "warning" as const, message: "Column OS version no longer exists and was removed." },
      { path: "definition.classKeys.0", code: "class_archived" as const, severity: "info" as const, message: "archived" },
    ];
    const v = view({ resolved: { state: "degraded", query: {}, columns: [], issues } });
    assert.deepEqual(degradedLines(v), ["Column OS version no longer exists and was removed."]);
    assert.deepEqual(degradedLines({ ...v, visibility: "shared", canEdit: false }), ["1 part of this shared view is not available and was left out."]);
  });
});
