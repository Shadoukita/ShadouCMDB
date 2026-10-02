import assert from "node:assert/strict";
import { describe, test } from "node:test";
import { effectScope, nextTick, reactive, ref, watch } from "vue";
import type { LocationQueryRaw } from "vue-router";
import type { SavedView } from "../src/api/savedViews";
import type { UiListView } from "../src/api/uiSettings";
import { useInventoryQueryState, type QueryStateOptions } from "../src/lib/useInventoryQueryState";
import { useSavedViewState } from "../src/lib/useSavedViewState";

const SERVER = { id: "c-server", key: "server" };
const LIST_VIEW: UiListView = {
  classKey: "server",
  columns: ["label", "ident"],
  defaultSort: { field: "ident", direction: "asc" },
  pageSize: 25,
  defaultFilters: { q: null, lookups: { status: ["in_service"] } },
};

function savedView(over: Partial<SavedView> = {}): SavedView {
  return {
    id: "v1",
    context: "inventory",
    visibility: "personal",
    name: "Linux servers",
    description: null,
    definition: {},
    resolved: { state: "ok", query: { classId: SERVER.id, q: "linux", sort: "-attributes.os" }, columns: ["label", "attributes.os"], issues: [] },
    home: "server",
    isDefault: true,
    canEdit: true,
    version: 1,
    createdAt: "2026-10-01T10:00:00Z",
    createdBy: { id: "u1", name: "Ada" },
    updatedAt: "2026-10-01T10:00:00Z",
    updatedBy: { id: "u1", name: "Ada" },
    ...over,
  };
}

/** Both composables of /cis over a fake route and router, counting the list requests the page would send. */
function setup(query: LocationQueryRaw) {
  const route = reactive({ query: { ...query } as Record<string, string> });
  const history: LocationQueryRaw[] = [];
  const go = async (to: unknown) => {
    const q = (to as { query: LocationQueryRaw }).query;
    history.push(q);
    await Promise.resolve();
    route.query = { ...(q as Record<string, string>) };
    return undefined;
  };
  const router = { push: go, replace: go } as unknown as QueryStateOptions["router"];
  const classes = ref<{ id: string; key: string }[] | undefined>([SERVER]);
  const views = ref<SavedView[] | undefined>(undefined);
  const linked = ref<{ view?: SavedView; failed?: boolean } | undefined>(undefined);
  const asked: (string | undefined)[] = [];
  const scope = effectScope();
  const [sv, state] = scope.run(() => {
    const sv = useSavedViewState({
      context: "inventory",
      classes: () => classes.value,
      route,
      router,
      sources: (linkedId) => ({
        views: () => views.value,
        error: () => null,
        linked: () => (asked.push(linkedId()), linked.value),
        refetch: () => undefined,
      }),
    });
    const state = useInventoryQueryState({
      context: "inventory",
      classes: () => classes.value,
      settingsLoaded: true,
      listViewFor: (key) => (key === "server" ? LIST_VIEW : undefined),
      lookupValueIds: () => "v-in_service",
      route,
      router,
      inAppNavigation: () => true,
      hold: () => sv.holding.value,
    });
    return [sv, state] as const;
  })!;
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
  return { route, history, views, linked, sv, state, requests, asked, stop: () => scope.stop() };
}

const tick = async () => {
  for (let i = 0; i < 6; i++) await nextTick();
};

describe("useSavedViewState with the query state (§1.3)", () => {
  test("opening a class list with a default sends the list query once, with the view's state (GH#167)", async () => {
    const t = setup({ classId: SERVER.id });
    await tick();
    assert.deepEqual(t.requests, [], "waits for the views");
    t.views.value = [savedView()];
    await tick();
    assert.deepEqual(t.history, [{ view: "v1", classId: SERVER.id, q: "linux", sort: "-attributes.os", columns: "label,attributes.os" }]);
    assert.equal(t.requests.length, 1, t.requests.join("\n"));
    assert.deepEqual(JSON.parse(t.requests[0]), { q: "linux", classId: SERVER.id, sort: "-attributes.os", limit: 25, offset: 0 });
    assert.equal(t.sv.current.value?.id, "v1");
    assert.equal(t.sv.modified(t.state.stateDefaults.value), false);
    t.stop();
  });

  test("without a default the class's list view filters apply, also queried once", async () => {
    const t = setup({ classId: SERVER.id });
    t.views.value = [savedView({ isDefault: false })];
    await tick();
    assert.deepEqual(t.history, [{ classId: SERVER.id, lookupValueId: "v-in_service" }]);
    assert.equal(t.requests.length, 1, t.requests.join("\n"));
    t.stop();
  });

  test("a view=-only link fetches the view and applies it; a change shows as modified", async () => {
    const t = setup({ view: "v1" });
    await tick();
    assert.equal(t.asked.at(-1), "v1");
    assert.deepEqual(t.requests, []);
    t.linked.value = { view: savedView({ isDefault: false }) };
    await tick();
    assert.equal(t.route.query.q, "linux");
    assert.equal(t.requests.length, 1);
    await t.state.update({ q: "debian" });
    await tick();
    t.views.value = [savedView({ isDefault: false })];
    await tick();
    assert.equal(t.sv.modified(t.state.stateDefaults.value), true);
    t.stop();
  });

  test("a link to a view that is not available drops it with a notice", async () => {
    const t = setup({ view: "v9" });
    t.linked.value = { failed: true };
    t.views.value = [];
    await tick();
    assert.deepEqual(t.sv.notice.value, { kind: "notAvailable" });
    assert.equal(t.route.query.view, undefined);
    assert.equal(t.requests.length, 1);
    t.stop();
  });
});
