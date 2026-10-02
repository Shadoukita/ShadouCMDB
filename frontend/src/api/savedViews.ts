// TanStack Query composables for saved views (inventory and search). Same rules
// as queries.ts: every request goes through the typed client, and mutations
// invalidate exactly what they change (the view lists of both contexts).
import { useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { toValue, type MaybeRefOrGetter } from "vue";
import { api, unwrap, type Schemas } from "./client";

export type SavedView = Schemas["SavedView"];
export type SavedViewContext = Schemas["SavedViewContext"];
export type SavedViewVisibility = Schemas["SavedViewVisibility"];
export type SavedViewDefinition = Schemas["SavedViewDefinition"];
export type SavedViewIssue = Schemas["SavedViewIssue"];
export type SavedViewList = Schemas["SavedViewList"];

export const savedViewKeys = {
  all: ["saved-views"] as const,
  list: (context: SavedViewContext | "all") => ["saved-views", "list", context] as const,
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
