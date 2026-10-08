// TanStack Query composables for the notes on a CI (gap G13, /configuration-items/{id}/notes). Reading needs view on
// the CI's class, adding edit on it; each note says whether the caller may change (`canEdit`) or delete
// (`canDelete`) it, and the server checks again. A write is audited as entity `ci_notes`, so mutations refresh the
// audit queries too.
import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { toValue, type MaybeRefOrGetter } from "vue";
import { api, unwrap, type Schemas } from "./client";

export type CiNote = Schemas["CiNote"];

/** The page the Notes tab opens on; the tab's count reads the same query. */
export const NOTES_PAGE = 50;

export const noteKeys = {
  all: ["ci-notes"] as const,
  ci: (ciId: string) => ["ci-notes", ciId] as const,
  page: (ciId: string, limit: number, offset: number) => ["ci-notes", ciId, limit, offset] as const,
};

const notePath = (id: string, noteId: string) => ({ path: { id, noteId } });

/** A CI's notes, newest first, one server-side page at a time. */
export function useCiNotes(
  ciId: MaybeRefOrGetter<string>,
  paging: MaybeRefOrGetter<{ limit: number; offset: number }> = { limit: NOTES_PAGE, offset: 0 },
  enabled: MaybeRefOrGetter<boolean> = true,
) {
  return useQuery(() => {
    const id = toValue(ciId);
    const { limit, offset } = toValue(paging);
    return {
      queryKey: noteKeys.page(id, limit, offset),
      enabled: !!id && toValue(enabled),
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/configuration-items/{id}/notes", { params: { path: { id }, query: { limit, offset } }, signal })),
      placeholderData: keepPreviousData,
    };
  });
}

function useNoteMutation<V>(ciId: MaybeRefOrGetter<string>, fn: (id: string, vars: V) => Promise<unknown>) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (vars: V) => fn(toValue(ciId), vars),
    // A conflict or a refusal means the list is stale too: refresh it either way.
    onSettled: () => {
      qc.invalidateQueries({ queryKey: noteKeys.ci(toValue(ciId)) });
      qc.invalidateQueries({ queryKey: ["audit"] });
    },
  });
}

export function useCreateCiNote(ciId: MaybeRefOrGetter<string>) {
  return useNoteMutation(ciId, (id, body: string) => unwrap(api.POST("/api/v1/configuration-items/{id}/notes", { params: { path: { id } }, body: { body } })));
}

export function useUpdateCiNote(ciId: MaybeRefOrGetter<string>) {
  return useNoteMutation(ciId, (id, v: { noteId: string; version: number; body: string }) =>
    unwrap(api.PATCH("/api/v1/configuration-items/{id}/notes/{noteId}", { params: notePath(id, v.noteId), body: { version: v.version, body: v.body } })),
  );
}

export function useDeleteCiNote(ciId: MaybeRefOrGetter<string>) {
  return useNoteMutation(ciId, (id, v: { noteId: string; version: number }) =>
    unwrap(api.DELETE("/api/v1/configuration-items/{id}/notes/{noteId}", { params: { ...notePath(id, v.noteId), query: { version: v.version } } })),
  );
}
