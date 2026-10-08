// TanStack Query composables for saved views (inventory and search). Same rules
// as queries.ts: every request goes through the typed client, and mutations
// invalidate exactly what they change (the view lists of both contexts).
import { useMutation, useQueries, useQuery, useQueryClient } from "@tanstack/vue-query";
import { computed, toValue, type MaybeRefOrGetter } from "vue";
import { concurrencyLimit } from "../lib/concurrency";
import { api, unwrap, type Schemas } from "./client";

export type SavedView = Schemas["SavedView"];
export type SavedViewContext = Schemas["SavedViewContext"];
export type SavedViewVisibility = Schemas["SavedViewVisibility"];
export type SavedViewDefinition = Schemas["SavedViewDefinition"];
export type SavedViewIssue = Schemas["SavedViewIssue"];
export type SavedViewList = Schemas["SavedViewList"];
export type SavedViewCount = Schemas["SavedViewCount"];

export const savedViewKeys = {
  all: ["saved-views"] as const,
  list: (context: SavedViewContext | "all") => ["saved-views", "list", context] as const,
  counts: (ids: string) => ["saved-views", "counts", ids] as const,
};

/** The caller's views and the shared views available to them (unpaged: the API caps both). */
export function useSavedViews(context: MaybeRefOrGetter<SavedViewContext | "all">, enabled: MaybeRefOrGetter<boolean> = true) {
  return useQuery(() => {
    const c = toValue(context);
    return {
      queryKey: savedViewKeys.list(c),
      enabled: toValue(enabled),
      staleTime: 30_000,
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/saved-views", { params: { query: c === "all" ? {} : { context: c } }, signal })),
    };
  });
}

/** The API counts at most this many views per request. */
const COUNT_BATCH = 50;
/**
 * At most this many count requests in flight from this tab (GH#780): the server counts for only a few requests at
 * once (a quarter of its database pool), and a request that waits too long for its turn answers `timed_out`.
 */
const countLimit = concurrencyLimit(2);

/**
 * How many CIs each of the given views shows the caller (GET /saved-views/counts, session only), in requests of
 * at most 50 ids sent two at a time, merged into one map by view id with the cap of `at_least` counts. Read-only
 * and cheap to repeat, but it runs on every page: cached for 30 s, and `refetchStale` refreshes it on navigation
 * rather than on a timer. Changing a view invalidates `savedViewKeys.all`, so the counts follow.
 */
export function useSavedViewCounts(ids: MaybeRefOrGetter<readonly string[]>) {
  const results = useQueries({
    queries: computed(() => {
      const all = toValue(ids);
      const batches: string[] = [];
      for (let i = 0; i < all.length; i += COUNT_BATCH) batches.push(all.slice(i, i + COUNT_BATCH).join(","));
      return batches.map((batch) => ({
        queryKey: savedViewKeys.counts(batch),
        staleTime: 30_000,
        queryFn: ({ signal }: { signal: AbortSignal }) =>
          countLimit(() => {
            // Cancelled while it waited for its turn (the rail closed or its views changed): never sent.
            signal.throwIfAborted();
            return unwrap(api.GET("/api/v1/saved-views/counts", { params: { query: { ids: batch } }, signal }));
          }),
      }));
    }),
  });
  const byView = computed(() => {
    const m = new Map<string, SavedViewCount & { cap: number }>();
    for (const r of results.value) for (const c of r.data?.data ?? []) m.set(c.viewId, { ...c, cap: r.data!.cap });
    return m;
  });
  const refetchStale = () => {
    for (const r of results.value) if (r.isStale && !r.isFetching) void r.refetch();
  };
  return { byView, refetchStale };
}

function useViewMutation<V, R>(fn: (vars: V) => Promise<R>) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: fn,
    onSuccess: () => qc.invalidateQueries({ queryKey: savedViewKeys.all }),
  });
}

export const useCreateSavedView = () =>
  useViewMutation(
    (body: { context: SavedViewContext; name: string; description?: string | null; visibility: SavedViewVisibility; definition: SavedViewDefinition }) =>
      unwrap(api.POST("/api/v1/saved-views", { body })),
  );

export const useUpdateSavedView = () =>
  useViewMutation(
    ({ id, ...body }: { id: string; version: number; name?: string; description?: string | null; definition?: SavedViewDefinition }) =>
      unwrap(api.PATCH("/api/v1/saved-views/{id}", { params: { path: { id } }, body })),
  );

export const useDeleteSavedView = () =>
  useViewMutation(({ id, version }: { id: string; version: number }) =>
    unwrap(api.DELETE("/api/v1/saved-views/{id}", { params: { path: { id }, query: { version } } })),
  );

export const useCopySavedView = () =>
  useViewMutation(({ id, ...body }: { id: string; name: string; visibility: SavedViewVisibility }) =>
    unwrap(api.POST("/api/v1/saved-views/{id}/copy", { params: { path: { id } }, body })),
  );

/** Set (`viewId`) or clear (null) the caller's default for an inventory list (`classKey` null: the unscoped one). */
export const useSetSavedViewDefault = () =>
  useViewMutation(({ classKey, viewId }: { classKey: string | null; viewId: string | null }) =>
    unwrap(api.PUT("/api/v1/saved-views/defaults", { body: { context: "inventory", classKey, viewId } })),
  );
