import { computed, ref, toValue, watch, type MaybeRefOrGetter } from "vue";
import { useRoute, useRouter, type LocationQueryRaw, type RouteLocationNormalizedLoaded, type Router } from "vue-router";
import type { SavedView } from "../api/savedViews";
import { hasUrlState, param, type QueryContext } from "./inventoryQuery";
import { isHistoryNavigation } from "./navigation";
import { viewUrlQuery } from "./savedViews";

export interface SavedViewSelectionOptions {
  context: QueryContext;
  /** The views of this context; undefined while they load. */
  views: MaybeRefOrGetter<readonly SavedView[] | undefined>;
  /** Whether loading the views failed: the list then shows its baseline instead of waiting. */
  failed: MaybeRefOrGetter<boolean>;
  /** The CI classes (to find the default of a class list); undefined while they load. */
  classes: MaybeRefOrGetter<readonly { id: string; key: string }[] | undefined>;
  /** For tests; the app uses the current route and router. */
  route?: Pick<RouteLocationNormalizedLoaded, "query" | "fullPath">;
  router?: Pick<Router, "push" | "replace">;
  historyNavigation?: () => boolean;
}

/** Why the `view` of a link was not applied (§1.5): the same text for "deleted" and "not yours", so nothing leaks. */
export type ViewNotice = { kind: "notAvailable" } | { kind: "unavailable"; name: string };

type Decision =
  | { kind: "none" }
  | { kind: "wait" }
  | { kind: "apply"; to: LocationQueryRaw; name: string }
  | { kind: "drop"; notice: ViewNotice };

/**
 * Which saved view a list shows (saved-views spec §1.3):
 *
 *   1. the URL has state: it is shown as it is (`view` only names the view it came from);
 *   2. `view=<id>` alone: the view's state is written into the URL (a bookmark of a view
 *      follows its later edits);
 *   3. nothing but the class (inventory only): the user's default view for that list,
 *      written into the URL with `view=<id>`;
 *   4. and 5. the class's list view and the built-in defaults (useInventoryQueryState).
 *
 * `pending` is true until that is settled, so the list is queried once (GH#167).
 */
export function useSavedViewSelection(options: SavedViewSelectionOptions) {
  const route = options.route ?? useRoute();
  const router = options.router ?? useRouter();
  const historyNav = options.historyNavigation ?? isHistoryNavigation;
  const path = options.context === "inventory" ? "/cis" : "/search";

  /** Set before navigating somewhere that must not get the default view (Clear filters, a dropped link). */
  let skipNextDefault = false;
  /** Whether the URL now shown may get the default view: not after Back/Forward, nor when told to skip. */
  const defaultAllowed = ref(true);
  watch(
    () => route.fullPath,
    () => {
      defaultAllowed.value = !skipNextDefault && !historyNav();
      skipNextDefault = false;
    },
    // Sync: settled before `decision` is read for the new URL.
    { immediate: true, flush: "sync" },
  );

  const viewId = computed(() => param(route.query, "view"));
  const views = computed(() => toValue(options.views));
  /** The view the URL names, if the user can see it. */
  const current = computed(() => (viewId.value ? views.value?.find((v) => v.id === viewId.value) : undefined));

  const decision = computed<Decision>(() => {
    const q = route.query;
    if (viewId.value) {
      if (hasUrlState(q, options.context)) return { kind: "none" };
      if (toValue(options.failed)) return { kind: "none" };
      if (!views.value) return { kind: "wait" };
      const v = current.value;
      if (!v) return { kind: "drop", notice: { kind: "notAvailable" } };
      const to = viewUrlQuery(v, options.context);
      return to ? { kind: "apply", to, name: v.name } : { kind: "drop", notice: { kind: "unavailable", name: v.name } };
    }
    if (options.context !== "inventory" || !defaultAllowed.value) return { kind: "none" };
    if (!Object.keys(q).every((k) => k === "classId" || k === "offset")) return { kind: "none" };
    const classId = param(q, "classId");
    if (classId.includes(",") || toValue(options.failed)) return { kind: "none" };
    if (!views.value) return { kind: "wait" };
    let home: string | null = null;
    if (classId) {
      const classes = toValue(options.classes);
      if (!classes) return { kind: "wait" };
      const c = classes.find((x) => x.id === classId);
      if (!c) return { kind: "none" };
      home = c.key;
    }
    const v = views.value.find((x) => x.context === "inventory" && x.isDefault && (x.home ?? null) === home);
    // A default that became unavailable is ignored, not deleted (§3.2): it comes back with the access.
    const to = v && viewUrlQuery(v, options.context);
    return v && to ? { kind: "apply", to, name: v.name } : { kind: "none" };
  });

  const notice = ref<(ViewNotice & { path: string }) | null>(null);
  /** The name of the view just applied, for the page's announcement; the page clears it. */
  const applied = ref<string | null>(null);

  watch(
    decision,
    (d) => {
      if (d.kind === "apply") {
        applied.value = d.name;
        void router.replace({ path, query: d.to });
      } else if (d.kind === "drop") {
        const { view: _drop, ...rest } = route.query;
        skipNextDefault = true;
        void router.replace({ path, query: rest }).then(() => {
          notice.value = { ...d.notice, path: route.fullPath };
        });
      }
    },
    { immediate: true },
  );
  // The notice is about the link that was opened: it goes with the next change of the list.
  watch(
    () => route.fullPath,
    (p) => {
      if (notice.value && notice.value.path !== p) notice.value = null;
    },
  );

  return {
    viewId,
    current,
    /** True until steps 2 and 3 are settled: the list waits, and the list view's defaults stay out. */
    pending: computed(() => decision.value.kind !== "none"),
    notice,
    applied,
    dismissNotice: () => (notice.value = null),
    /** Call before a navigation that must show the plain list (Clear filters, after deleting the view shown). */
    skipNextDefault: () => {
      skipNextDefault = true;
    },
  };
}
