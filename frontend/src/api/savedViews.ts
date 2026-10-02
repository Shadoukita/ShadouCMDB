// TanStack Query composables for saved views (the View menu of /cis and /search).
// Same rules as queries.ts: every request goes through the typed client, and a
// change invalidates the context's view list (defaults and badges live in it).
import { useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { toValue, type MaybeRefOrGetter } from "vue";
import type { SavedViewSources } from "../lib/useSavedViewState";
import { ApiError, api, unwrap, type Schemas } from "./client";

export type SavedView = Schemas["SavedView"];
export type SavedViewList = Schemas["SavedViewList"];
export type SavedViewContext = Schemas["SavedViewContext"];
export type SavedViewVisibility = Schemas["SavedViewVisibility"];
export type SavedViewDefinition = Schemas["SavedViewDefinition"];
export type SavedViewQuery = Schemas["SavedViewQuery"];
export type SavedViewIssue = Schemas["SavedViewIssue"];

export const savedViewKeys = {
  all: ["saved-views"] as const,
  list: (context: SavedViewContext) => ["saved-views", "list", context] as const,
  view: (id: string) => ["saved-views", "view", id] as const,
};

/** The caller's views and the shared views available to them, for one context. */
export function useSavedViews(context: SavedViewContext, enabled: MaybeRefOrGetter<boolean> = true) {
  return useQuery(() => ({
    queryKey: savedViewKeys.list(context),
    enabled: toValue(enabled),
    staleTime: 30_000,
    queryFn: ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/saved-views", { params: { query: { context } }, signal })),
  }));
}

/**
 * One view, for a `view=<id>` link. Not retried: a 404 means "not available to you"
 * (deleted or not permitted, the same answer for both) and shows the banner at once.
 */
export function useSavedView(id: MaybeRefOrGetter<string | undefined>) {
  return useQuery(() => {
    const v = toValue(id);
    return {
      queryKey: savedViewKeys.view(v ?? ""),
      enabled: !!v,
      staleTime: 30_000,
      retry: false,
      queryFn: ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/saved-views/{id}", { params: { path: { id: v! } }, signal })),
    };
  });
}

/** The view list and the `view=` view for lib/useSavedViewState. */
export function useSavedViewSources(context: SavedViewContext, linkedId: () => string | undefined): SavedViewSources {
  const list = useSavedViews(context);
  const single = useSavedView(linkedId);
  return {
    views: () => list.data.value?.data,
    error: () => {
      const e = list.error.value;
      return e instanceof ApiError ? e : e ? new ApiError(0, "UNKNOWN", String(e)) : null;
    },
    linked: () => (single.data.value ? { view: single.data.value } : single.isError.value ? { failed: true } : undefined),
    refetch: () => void list.refetch(),
  };
}

function useViewMutation<V, R>(fn: (vars: V) => Promise<R>) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: fn,
    onSuccess: () => qc.invalidateQueries({ queryKey: savedViewKeys.all }),
  });
}

export interface CreateViewBody {
  context: SavedViewContext;
  name: string;
  description?: string | null;
  visibility: SavedViewVisibility;
  definition: SavedViewDefinition;
}

export const useCreateSavedView = () => useViewMutation((body: CreateViewBody) => unwrap(api.POST("/api/v1/saved-views", { body })));

export interface UpdateViewBody {
  version: number;
  name?: string;
  description?: string | null;
  definition?: SavedViewDefinition;
}

export const useUpdateSavedView = () =>
  useViewMutation(({ id, body }: { id: string; body: UpdateViewBody }) =>
    unwrap(api.PATCH("/api/v1/saved-views/{id}", { params: { path: { id } }, body })),
  );

export const useDeleteSavedView = () =>
  useViewMutation(({ id, version }: { id: string; version: number }) =>
    unwrap(api.DELETE("/api/v1/saved-views/{id}", { params: { path: { id }, query: { version } } })),
  );

/** "Copy to my views" and "Share a copy": a new view with the source's definition. */
export const useCopySavedView = () =>
  useViewMutation(({ id, name, visibility }: { id: string; name: string; visibility: SavedViewVisibility }) =>
    unwrap(api.POST("/api/v1/saved-views/{id}/copy", { params: { path: { id } }, body: { name, visibility } })),
  );

/** Sets (viewId) or clears (null) the caller's default for a class list (classKey) or the unscoped inventory (null). */
export const useSetDefaultView = () =>
  useViewMutation(({ classKey, viewId }: { classKey: string | null; viewId: string | null }) =>
    unwrap(api.PUT("/api/v1/saved-views/defaults", { body: { context: "inventory", classKey, viewId } })),
  );
