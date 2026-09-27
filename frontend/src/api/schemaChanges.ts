// The data model's database side: technical names, DDL previews, purges and the
// schema change history. Areas, types and fields are real PostgreSQL schemas,
// tables and columns; every change to them runs as DDL on the server, so the UI
// previews it first (POST /schema-changes/preview runs the operation in a
// transaction that is always rolled back) and shows what it would do.
import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { toValue, type MaybeRefOrGetter } from "vue";
import { api, unwrap, type JsonBody, type ListQuery, type Schemas } from "./client";
import { invalidateResource, type Resource } from "./datamodel";

export type SchemaChangePreview = Schemas["SchemaChangePreview"];
export type SchemaChange = Schemas["SchemaChange"];
export type Impact = Schemas["Impact"];
export type TechnicalName = Schemas["TechnicalName"];
type PreviewBody = JsonBody<"/api/v1/schema-changes/preview", "post">;
export type SchemaOperation = PreviewBody["operation"];
/** The spec leaves `body` open (it is the body of the operation's own endpoint); typed as such here. */
export type PreviewRequest = Omit<PreviewBody, "body"> & { body?: object | null };
export type TechnicalNameKind = ListQuery<"/api/v1/technical-names">["kind"];
export type SchemaChangeListQuery = ListQuery<"/api/v1/schema-changes">;

/** The resources whose rows are database objects, and so can be purged. */
export type PurgeableResource = Extract<Resource, "areas" | "ci-classes" | "attribute-definitions">;

/** Dry-runs an operation: the DDL and data impact, or the ApiError the endpoint would answer (a refused change). */
export function previewSchemaChange(request: PreviewRequest): Promise<SchemaChangePreview> {
  return unwrap(api.POST("/api/v1/schema-changes/preview", { body: request as PreviewBody }));
}

export interface TechnicalNameQuery {
  kind: TechnicalNameKind;
  /** The display name the technical name is derived from. */
  name: string;
  /** A technical name the administrator typed instead, to check it. */
  key?: string;
  /** kind=type: the area the table goes into (for the qualified name). */
  areaId?: string;
  /** kind=field: the type the column goes into. */
  classId?: string;
}

/**
 * The technical name the API would use for a display name (or whether a typed
 * one can be used), with where it would live ("bestand.virtuelle_maschinen").
 * Pass null while there is nothing to check.
 */
export function useTechnicalName(query: MaybeRefOrGetter<TechnicalNameQuery | null>) {
  return useQuery(() => {
    const q = toValue(query);
    return {
      queryKey: ["technical-names", q],
      enabled: !!q,
      placeholderData: keepPreviousData,
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(
          api.GET("/api/v1/technical-names", {
            params: {
              query: {
                kind: q!.kind,
                name: q!.name,
                ...(q!.key ? { key: q!.key } : {}),
                ...(q!.areaId ? { areaId: q!.areaId } : {}),
                ...(q!.classId ? { classId: q!.classId } : {}),
              },
            },
            signal,
          }),
        ),
    };
  });
}

type PurgePath = "/api/v1/areas/{id}/purge";

/** Purges an archived area, type or field: drops its schema, table or column. `confirm` is its technical name, typed. */
export function usePurge(resource: PurgeableResource) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, confirm }: { id: string; confirm: string }) =>
      unwrap(api.POST(`/api/v1/${resource}/{id}/purge` as PurgePath, { params: { path: { id } }, body: { confirm } })),
    onSuccess: () => {
      invalidateResource(qc, resource);
      // A purged type takes its CIs (and their relationships) with it.
      if (resource !== "attribute-definitions") qc.invalidateQueries({ queryKey: ["relationships"] });
    },
  });
}

/** The DDL the data model administration ran, newest first. */
export function useSchemaChanges(query: MaybeRefOrGetter<SchemaChangeListQuery>) {
  return useQuery(() => {
    const q = toValue(query);
    return {
      queryKey: ["schema-changes", q],
      placeholderData: keepPreviousData,
      queryFn: ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/schema-changes", { params: { query: q }, signal })),
    };
  });
}
