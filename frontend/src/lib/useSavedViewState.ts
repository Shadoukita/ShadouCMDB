import { computed, ref, toValue, watch, type MaybeRefOrGetter } from "vue";
import { useRoute, useRouter, type RouteLocationNormalizedLoaded, type Router } from "vue-router";
import type { ApiError } from "../api/client";
import type { SavedView, SavedViewContext } from "../api/savedViews";
import { param } from "./inventoryQuery";
import { decideOpen, homeOf, isModified, viewQuery, type OpenInput, type StateDefaults, type ViewNotice } from "./savedViews";

/** Where the views come from: the API (api/savedViews `useSavedViewSources`), or a fake in tests. */
export interface SavedViewSources {
  /** The context's views; undefined while they load. */
  views: () => readonly SavedView[] | undefined;
  /** The error the view list failed with, if it did. */
  error: () => ApiError | null;
  /** The `view=` view fetched by id (the factory's `linkedId`): undefined while it loads or when none is asked for. */
  linked: () => OpenInput["linked"];
  refetch: () => void;
}

export interface SavedViewStateOptions {
  context: SavedViewContext;
  /** The CI classes; undefined until they have loaded. */
  classes: MaybeRefOrGetter<readonly { id: string; key: string }[] | undefined>;
  /** Creates the sources; `linkedId` is the view a `view=`-only link asks for. */
  sources: (linkedId: () => string | undefined) => SavedViewSources;
  route?: Pick<RouteLocationNormalizedLoaded, "query">;
  router?: Pick<Router, "push" | "replace">;
}

/**
 * The saved-view side of a list page: the views, the one the URL names, and the
 * §1.3 precedence (lib/savedViews `decideOpen`) applied with `router.replace`.
 * `holding` is true while the list must not be queried yet (a `view=` link or the
 * user's default is about to fill the URL), so the list is queried once (GH#167).
 */
export function useSavedViewState(options: SavedViewStateOptions) {
  const route = options.route ?? useRoute();
  const router = options.router ?? useRouter();
  const path = options.context === "inventory" ? "/cis" : "/search";
  const viewId = computed(() => param(route.query, "view") || undefined);
  /** Only a `view=` link fetches the one view; otherwise it is in the list. */
  const linkOnly = computed(() => !!viewId.value && Object.keys(route.query).every((k) => k === "view" || !param(route.query, k)));

  const src = options.sources(() => (linkOnly.value ? viewId.value : undefined));
  const views = computed(() => src.views());
  const listError = computed(() => src.error());
  const viewsFailed = computed(() => !views.value && !!listError.value);
  const linked = computed(() => (linkOnly.value ? src.linked() : undefined));

  /** The view the URL names, when the caller can read it. */
  const current = computed(() => (viewId.value ? (views.value?.find((v) => v.id === viewId.value) ?? linked.value?.view) : undefined));

  const notice = ref<ViewNotice | null>(null);
  /** The query a replace is on its way to (the decision is not taken again until it lands). */
  const replacing = ref<string | null>(null);
  const decision = computed(() =>
    decideOpen({
      query: route.query,
      context: options.context,
      classes: toValue(options.classes),
      views: views.value,
      viewsFailed: viewsFailed.value,
      linked: linked.value,
    }),
  );
  watch(
    decision,
    (d) => {
      if (d.kind !== "replace") return;
      const key = JSON.stringify(d.query);
      if (replacing.value === key) return;
      replacing.value = key;
      if (d.notice) notice.value = d.notice;
      void router.replace({ path, query: d.query }).finally(() => {
        if (replacing.value === key) replacing.value = null;
      });
    },
    { immediate: true },
  );
  const holding = computed(() => decision.value.kind !== "none" || replacing.value !== null);

  /** The default slot of the list shown. */
  const home = computed(() => homeOf(route.query, toValue(options.classes) ?? []));

  /** Whether the URL's state differs from the view it names (the Modified marker). */
  const modified = (defaults: StateDefaults) => !!current.value && !holding.value && isModified(route.query, current.value, defaults);

  /** Applies a view (a menu choice, Revert): its state into the URL. */
  function apply(view: SavedView) {
    const q = viewQuery(view);
    if (!q) return Promise.resolve();
    notice.value = null;
    return router.push({ path, query: q });
  }
  /** Leaves the view: the same list, no longer tied to it (after deleting it). */
  function detach() {
    const { view: _v, ...rest } = route.query;
    return router.replace({ path, query: rest });
  }

  return {
    views,
    viewsFailed,
    viewsLoading: computed(() => !views.value && !viewsFailed.value),
    listError,
    viewId,
    current,
    notice,
    dismissNotice: () => (notice.value = null),
    holding,
    home,
    modified,
    apply,
    detach,
    refetch: () => src.refetch(),
  };
}

export type SavedViewState = ReturnType<typeof useSavedViewState>;
