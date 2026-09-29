import assert from "node:assert/strict";
import { describe, test } from "node:test";
import { effectScope, nextTick, reactive, ref, watch } from "vue";
import type { LocationQueryRaw } from "vue-router";
import type { UiListView } from "../src/api/uiSettings";
import { useInventoryQueryState, type QueryStateOptions } from "../src/lib/useInventoryQueryState";

const SERVER = { id: "c-server", key: "server" };
const VIEW: UiListView = {
  classKey: "server",
  columns: ["label", "attributes.ip_address"],
  defaultSort: { field: "attributes.ip_address", direction: "desc" },
  pageSize: 25,
  defaultFilters: { q: null, lookups: {} },
};

/** The composable over a fake route and router: the URL changes a tick after push/replace, as in the app. */
function setup(query: LocationQueryRaw, opts: Partial<QueryStateOptions> & { inApp?: boolean } = {}) {
  const route = reactive({ query: { ...query } as Record<string, string> });
  const history: { how: "push" | "replace"; query: LocationQueryRaw }[] = [];
  const go = (how: "push" | "replace") => async (to: unknown) => {
    const q = (to as { query: LocationQueryRaw }).query;
    history.push({ how, query: q });
    await Promise.resolve();
    route.query = { ...(q as Record<string, string>) };
    return undefined;
  };
  const router = { push: go("push"), replace: go("replace") } as unknown as QueryStateOptions["router"];
  const classes = ref<{ id: string; key: string }[] | undefined>(undefined);
  const settingsLoaded = ref(false);
  const views = ref<UiListView[]>([]);
  const scope = effectScope();
  const state = scope.run(() =>
    useInventoryQueryState({
      context: "inventory",
      classes: () => classes.value,
      settingsLoaded: () => settingsLoaded.value,
      listViewFor: (key) => views.value.find((v) => v.classKey === key),
      route,
      router,
      inAppNavigation: () => opts.inApp ?? false,
      ...opts,
    }),
  )!;
  // Every list request the page would send: the query whenever it is enabled.
  const requests: string[] = [];
  scope.run(() =>
    watch(
      () => (state.settled.value ? JSON.stringify(state.listQuery.value) : null),
      (q) => {
        if (q && requests.at(-1) !== q) requests.push(q);
      },
      { immediate: true, flush: "sync" },
    ),
  );
  return { route, history, classes, settingsLoaded, views, state, requests, stop: () => scope.stop() };
}

const tick = async () => {
  for (let i = 0; i < 5; i++) await nextTick();
};

describe("useInventoryQueryState", () => {
  test("a class with a list view sort is queried once, with that sort (GH#167)", async () => {
    const t = setup({ classId: SERVER.id });
    assert.equal(t.state.settled.value, false, "waits for the classes and the list views");
    t.classes.value = [SERVER];
    await tick();
    assert.equal(t.state.settled.value, false, "still waits for the UI settings");
    t.views.value = [VIEW];
    t.settingsLoaded.value = true;
    await tick();
    assert.equal(t.requests.length, 1, t.requests.join("\n"));
    assert.deepEqual(JSON.parse(t.requests[0]), { classId: SERVER.id, sort: "-attributes.ip_address", limit: 25, offset: 0 });
    t.stop();
  });

  test("a URL naming the sort and the page size needs no list view", () => {
    const t = setup({ classId: SERVER.id, sort: "ident", limit: "100" });
    assert.equal(t.state.settled.value, true);
    assert.equal(t.requests.length, 1);
    assert.equal(t.state.listQuery.value.sort, "ident");
    t.stop();
  });

  test("the URL wins over the list view, which wins over the defaults", async () => {
    const t = setup({ classId: SERVER.id, columns: "ident,attributes.cpu" });
    t.classes.value = [SERVER];
    t.views.value = [VIEW];
    t.settingsLoaded.value = true;
    await tick();
    assert.deepEqual(t.state.columns.value, ["label", "ident", "attributes.cpu"]);
    assert.equal(t.state.sort.value, "-attributes.ip_address");
    assert.equal(t.state.limit.value, 25);
    t.route.query = { classId: SERVER.id };
    await tick();
    assert.deepEqual(t.state.columns.value, ["label", "attributes.ip_address"]);
    t.route.query = {};
    await tick();
    assert.deepEqual(t.state.columns.value, ["label", "ident", "class", "active", "updatedAt"]);
    assert.equal(t.state.sort.value, "label");
    assert.equal(t.state.limit.value, 50);
    t.stop();
  });

  test("adding a column to the list view's columns keeps them, in the URL (GH#168)", async () => {
    const t = setup({ classId: SERVER.id });
    t.classes.value = [SERVER];
    t.views.value = [VIEW];
    t.settingsLoaded.value = true;
    await tick();
    await t.state.toggleColumn("attributes.hostname");
    await tick();
    assert.equal(t.route.query.columns, "label,attributes.ip_address,attributes.hostname");
    assert.equal(t.history.at(-1)!.how, "push", "Back undoes it");
    await t.state.resetColumns();
    await tick();
    assert.equal(t.route.query.columns, undefined);
    t.stop();
  });

  test("a list view's default filters are written into the URL before the list is queried", async () => {
    const lookupsLoaded = ref(false);
    const t = setup(
      { classId: SERVER.id },
      { inApp: true, lookupValueIds: (l) => (lookupsLoaded.value ? Object.values(l ?? {}).flat().map((k) => `v-${k}`).join(",") : null) },
    );
    t.classes.value = [SERVER];
    t.views.value = [{ ...VIEW, defaultFilters: { q: null, lookups: { status: ["in_service"] } } }];
    t.settingsLoaded.value = true;
    await tick();
    assert.equal(t.state.settled.value, false, "waits for the lookup values");
    assert.deepEqual(t.requests, []);
    lookupsLoaded.value = true;
    await tick();
    assert.deepEqual(t.history, [{ how: "replace", query: { classId: SERVER.id, lookupValueId: "v-in_service" } }]);
    assert.equal(t.requests.length, 1, t.requests.join("\n"));
    assert.equal(JSON.parse(t.requests[0]).lookupValueId, "v-in_service");
    t.stop();
  });

  test("a reload or Back does not write the default filters", async () => {
    const t = setup({ classId: SERVER.id }, { inApp: false });
    t.classes.value = [SERVER];
    t.views.value = [{ ...VIEW, defaultFilters: { q: "prod", lookups: {} } }];
    t.settingsLoaded.value = true;
    await tick();
    assert.deepEqual(t.history, []);
    assert.equal(t.requests.length, 1);
    t.stop();
  });

  test("the search context keeps the term when the filters are cleared, and has no sort", async () => {
    const t = setup({ q: "web", classId: SERVER.id, deleted: "include" }, { context: "search", listViewFor: () => undefined, settingsLoaded: true });
    assert.deepEqual(t.state.activeFilters.value, ["classId", "deleted"]);
    await t.state.clearFilters();
    await tick();
    assert.deepEqual(t.route.query, { q: "web" });
    t.stop();
  });
});
