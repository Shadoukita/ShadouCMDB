import assert from "node:assert/strict";
import { describe, test } from "node:test";
import { effectScope, nextTick, reactive, ref } from "vue";
import type { LocationQueryRaw } from "vue-router";
import type { SavedView } from "../src/api/savedViews";
import { definitionFromUrl, droppedSummary, groupViews, sameState, urlState, viewState, viewUrlQuery } from "../src/lib/savedViews";
import { useSavedViewSelection } from "../src/lib/useSavedViewSelection";

function view(over: Partial<SavedView> & { query?: SavedView["resolved"]["query"]; columns?: string[] } = {}): SavedView {
  const { query, columns, ...rest } = over;
  return {
    id: "v1",
    context: "inventory",
    visibility: "personal",
    name: "Prod servers",
    definition: {},
    resolved: {
      state: "ok",
      query: query === undefined ? { classId: "c-server", includeSubclasses: "true", lookupValueId: "lv1", sort: "-attributes.os", limit: 25 } : query,
      columns: columns ?? ["label", "attributes.os"],
      issues: [],
    },
    home: "server",
    isDefault: false,
    canEdit: true,
    version: 1,
    createdAt: "2026-10-01T00:00:00Z",
    createdBy: { name: "a" },
    updatedAt: "2026-10-01T00:00:00Z",
    updatedBy: { name: "a" },
    ...rest,
  };
}

const CAT = {
  classes: [
    { id: "c-server", key: "server" },
    { id: "c-vm", key: "vm" },
  ],
  lists: [
    { id: "l-env", key: "environment" },
    { id: "l-crit", key: "criticality" },
  ],
  values: [
    { id: "lv1", listId: "l-env", key: "production" },
    { id: "lv2", listId: "l-env", key: "staging" },
    { id: "cv1", listId: "l-crit", key: "high" },
  ],
};

describe("a view in the URL", () => {
  test("every parameter is written, with an explicit sort and page size", () => {
    assert.deepEqual(viewUrlQuery(view(), "inventory"), {
      view: "v1",
      classId: "c-server",
      lookupValueId: "lv1",
      sort: "-attributes.os",
      columns: "label,attributes.os",
      limit: "25",
    });
    const plain = viewUrlQuery(view({ query: { active: "true", deleted: "exclude" }, columns: [] }), "inventory");
    assert.deepEqual(plain, { view: "v1", sort: "label", limit: "50" }, "defaults stay out, sort and page size do not");
  });

  test("a search view has no sort and no columns", () => {
    const q = viewUrlQuery(view({ context: "search", query: { q: "web", classId: "c-vm", includeSubclasses: "false" }, columns: [] }), "search");
    assert.deepEqual(q, { view: "v1", q: "web", classId: "c-vm", includeSubclasses: "false", limit: "50" });
  });

  test("an unavailable view is never applied", () => {
    assert.equal(viewUrlQuery(view({ resolved: { state: "unavailable", query: null, columns: [], issues: [] } }), "inventory"), null);
  });

  test("the list matches the view until something changes, whatever the order of ids", () => {
    const v = view({ query: { classId: "c-server", lookupValueId: "lv1,lv2", sort: "label", limit: 50 }, columns: [] });
    const saved = viewState(v, "inventory")!;
    const eff = { sort: "label", limit: 50 };
    assert.ok(sameState(urlState({ view: "v1", classId: "c-server", lookupValueId: "lv2,lv1" }, "inventory", eff), saved));
    assert.ok(!sameState(urlState({ view: "v1", classId: "c-server", lookupValueId: "lv1" }, "inventory", eff), saved));
    assert.ok(!sameState(urlState({ view: "v1", classId: "c-server", lookupValueId: "lv1,lv2" }, "inventory", { sort: "-label", limit: 50 }), saved));
    assert.ok(!sameState(urlState({ classId: "c-server", lookupValueId: "lv1,lv2", columns: "label,ident" }, "inventory", eff), saved));
  });
});

describe("saving what the URL shows", () => {
  test("ids become keys; lookup and criticality values group by list", () => {
    const r = definitionFromUrl(
      { classId: "c-server", lookupValueId: "lv1,lv2", criticalityValueId: "cv1", active: "all", deleted: "include", q: " web ", columns: "label,attributes.os" },
      "inventory",
      CAT,
      { sort: "-attributes.os", limit: 25 },
    );
    assert.ok(r.ok);
    assert.deepEqual(r.definition, {
      classKeys: ["server"],
      includeSubclasses: true,
      filters: { q: "web", lookups: { environment: ["production", "staging"], criticality: ["high"] }, active: "all", deleted: "include" },
      pageSize: 25,
      sort: { field: "attributes.os", direction: "desc" },
      columns: ["label", "attributes.os"],
    });
  });

  test("a search view saves no sort and no columns", () => {
    const r = definitionFromUrl({ q: "db", classId: "c-vm", includeSubclasses: "false" }, "search", CAT, { sort: "label", limit: 50 });
    assert.ok(r.ok);
    assert.deepEqual(r.definition, { classKeys: ["vm"], includeSubclasses: false, filters: { q: "db" }, pageSize: 50 });
  });

  test("an unknown id is refused, never dropped (that would save a wider list)", () => {
    assert.equal(definitionFromUrl({ lookupValueId: "gone" }, "inventory", CAT, { sort: "label", limit: 50 }).ok, false);
    assert.equal(definitionFromUrl({ classId: "gone" }, "inventory", CAT, { sort: "label", limit: 50 }).ok, false);
  });
});

describe("the menu", () => {
  test("readers of a shared view see what was dropped only as a count", () => {
    const issues: SavedView["resolved"]["issues"] = [
      { path: "definition.columns.1", code: "unknown_attribute", severity: "warning", message: "Column gone" },
      { path: "definition.classKeys.0", code: "class_archived", severity: "info", message: "Server is archived" },
    ];
    const resolved = { state: "degraded" as const, query: {}, columns: [], issues };
    assert.deepEqual(droppedSummary(view({ resolved })), { messages: ["Column gone"], count: 1, notes: ["Server is archived"] });
    assert.deepEqual(droppedSummary(view({ resolved, visibility: "shared", canEdit: false })).messages, []);
  });

  test("views group into mine and shared, filtered by name", () => {
    const vs = [view({ id: "a", name: "Alpha" }), view({ id: "b", name: "Beta", visibility: "shared" })];
    assert.deepEqual(
      groupViews(vs, "bet").shared.map((v) => v.id),
      ["b"],
    );
    assert.equal(groupViews(vs, "bet").personal.length, 0);
  });
});

/** The selection over a fake route and router: the URL changes a tick after replace, as in the app. */
function selection(query: LocationQueryRaw, context: "inventory" | "search" = "inventory", historyNav = false) {
  const route = reactive({ query: { ...query } as Record<string, string>, fullPath: "" });
  route.fullPath = JSON.stringify(route.query);
  const calls: LocationQueryRaw[] = [];
  const replace = async (to: unknown) => {
    const q = (to as { query: LocationQueryRaw }).query;
    calls.push(q);
    await Promise.resolve();
    route.query = { ...(q as Record<string, string>) };
    route.fullPath = JSON.stringify(route.query);
    return undefined;
  };
  const views = ref<SavedView[] | undefined>(undefined);
  const failed = ref(false);
  const scope = effectScope();
  const s = scope.run(() =>
    useSavedViewSelection({
      context,
      views: () => views.value,
      failed: () => failed.value,
      classes: () => CAT.classes,
      route,
      router: { push: replace, replace } as never,
      historyNavigation: () => historyNav,
    }),
  )!;
  return { s, route, calls, views, failed, stop: () => scope.stop() };
}
const tick = async () => {
  for (let i = 0; i < 5; i++) await nextTick();
};

describe("useSavedViewSelection (§1.3)", () => {
  test("view=<id> alone: waits for the views, then writes the view into the URL", async () => {
    const t = selection({ view: "v1" });
    assert.equal(t.s.pending.value, true);
    t.views.value = [view()];
    await tick();
    assert.equal(t.calls.length, 1);
    assert.equal(t.route.query.sort, "-attributes.os");
    assert.equal(t.s.pending.value, false);
    assert.equal(t.s.current.value?.id, "v1");
    t.stop();
  });

  test("a URL with state wins over its view", async () => {
    const t = selection({ view: "v1", sort: "ident" });
    assert.equal(t.s.pending.value, false);
    t.views.value = [view()];
    await tick();
    assert.equal(t.calls.length, 0);
    t.stop();
  });

  test("a view the user cannot see: dropped, with the not-available notice and no default", async () => {
    const t = selection({ view: "nope" });
    t.views.value = [view({ id: "d", isDefault: true, home: null })];
    await tick();
    assert.deepEqual(t.calls, [{}]);
    assert.equal(t.s.notice.value?.kind, "notAvailable");
    assert.equal(t.s.pending.value, false, "the list view and built-in defaults apply, not the default view");
    t.stop();
  });

  test("an unavailable view is never applied", async () => {
    const t = selection({ view: "v1" });
    t.views.value = [view({ resolved: { state: "unavailable", query: null, columns: [], issues: [] } })];
    await tick();
    assert.deepEqual(t.calls, [{}]);
    assert.equal(t.s.notice.value?.kind, "unavailable");
    t.stop();
  });

  test("the default of the class list is applied once", async () => {
    const t = selection({ classId: "c-server" });
    t.views.value = [view({ id: "other", home: null, isDefault: true }), view({ isDefault: true })];
    await tick();
    assert.equal(t.calls.length, 1);
    assert.equal(t.route.query.view, "v1");
    t.stop();
  });

  test("no default after Back, after Clear filters, or on the search page", async () => {
    const back = selection({}, "inventory", true);
    back.views.value = [view({ home: null, isDefault: true })];
    await tick();
    assert.equal(back.calls.length, 0);
    back.stop();

    const search = selection({}, "search");
    search.views.value = [view({ home: null, isDefault: true })];
    await tick();
    assert.equal(search.s.pending.value, false);
    assert.equal(search.calls.length, 0);
    search.stop();
  });

  test("when the views fail to load, the list does not wait for them", async () => {
    const t = selection({ classId: "c-server" });
    t.failed.value = true;
    await tick();
    assert.equal(t.s.pending.value, false);
    t.stop();
  });
});
