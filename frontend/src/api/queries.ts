// TanStack Query composables over the typed client. Query keys live here so that
// mutations invalidate exactly what they change. Arguments are refs or getters,
// so a query refetches when the URL or form state it depends on changes.
import { keepPreviousData, useMutation, useQueries, useQuery, useQueryClient } from "@tanstack/vue-query";
import { computed, toValue, type MaybeRefOrGetter } from "vue";
import { ApiError, api, unwrap, type Schemas } from "./client";
import type { paths } from "./schema";
import { bySortOrder, flattenTree } from "../lib/tree";

export type CiSummary = Schemas["ConfigurationItemSummary"];
export type Ci = Schemas["ConfigurationItem"];
/** A reference attribute's target. `hidden` means the caller may not view its class: `name` is null. */
export type AttributeReference = Ci["attributeReferences"][string];
export type CiClass = Schemas["CiClass"];
export type EffectiveAttribute = Schemas["EffectiveAttribute"];
export type Relationship = Schemas["Relationship"];
export type RelationshipType = Schemas["RelationshipType"];
export type RelationshipGraph = Schemas["RelationshipGraph"];
export type AuditEntry = Schemas["AuditEntry"];
export type Status = Schemas["Status"];
export type Environment = Schemas["Environment"];
export type Location = Schemas["Location"];
export type Owner = Schemas["Owner"];
export type SearchResults = Schemas["SearchResults"];

export type CiListQuery = NonNullable<paths["/api/v1/configuration-items"]["get"]["parameters"]["query"]>;
export type SearchQuery = paths["/api/v1/search"]["get"]["parameters"]["query"];
export type CiCreateBody = NonNullable<paths["/api/v1/configuration-items"]["post"]["requestBody"]>["content"]["application/json"];
export type CiUpdateBody = NonNullable<paths["/api/v1/configuration-items/{id}"]["patch"]["requestBody"]>["content"]["application/json"];
export type RelationshipCreateBody = NonNullable<paths["/api/v1/relationships"]["post"]["requestBody"]>["content"]["application/json"];

/** Largest page the API serves; used for small reference lists (classes, statuses, ...). */
export const MAX_PAGE = 200;

export const keys = {
  cis: ["cis"] as const,
  ciList: (q: CiListQuery) => ["cis", "list", q] as const,
  ciCount: (q: CiListQuery) => ["cis", "count", q] as const,
  ci: (id: string) => ["cis", "detail", id] as const,
  graph: (id: string, depth: number, direction: string) => ["cis", "graph", id, depth, direction] as const,
  search: (q: string, limit: number, offset: number, filters: SearchFilters = {}) => ["cis", "search", q, limit, offset, filters] as const,
  relationships: (ciId: string) => ["relationships", ciId] as const,
  audit: (entityId: string) => ["audit", entityId] as const,
  classes: ["ci-classes"] as const,
  classAttributes: (classId: string) => ["ci-classes", classId, "attributes"] as const,
  relTypes: (sourceClassId: string, targetClassId: string) => ["relationship-types", sourceClassId, targetClassId] as const,
  lookup: (kind: LookupKind) => ["lookup", kind] as const,
};

// ---------- Configuration items ----------

export function useCiList(query: MaybeRefOrGetter<CiListQuery>, enabled: MaybeRefOrGetter<boolean> = true) {
  return useQuery(() => {
    const q = toValue(query);
    return {
      queryKey: keys.ciList(q),
      enabled: toValue(enabled),
      queryFn: ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/configuration-items", { params: { query: q }, signal })),
      placeholderData: keepPreviousData,
    };
  });
}

/** Server-side count only (limit=1, read page.total). */
export function ciCountQuery(query: CiListQuery) {
  return {
    queryKey: keys.ciCount(query),
    queryFn: async ({ signal }: { signal: AbortSignal }) => {
      const res = await unwrap(api.GET("/api/v1/configuration-items", { params: { query: { ...query, limit: 1 } }, signal }));
      return res.page.total;
    },
  };
}

export function useCi(id: MaybeRefOrGetter<string | undefined>) {
  return useQuery(() => {
    const ciId = toValue(id) ?? "";
    return {
      queryKey: keys.ci(ciId),
      enabled: !!ciId,
      queryFn: ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/configuration-items/{id}", { params: { path: { id: ciId } }, signal })),
    };
  });
}

export function useCreateCi() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: CiCreateBody) => unwrap(api.POST("/api/v1/configuration-items", { body })),
    onSuccess: (ci) => {
      qc.invalidateQueries({ queryKey: keys.cis });
      qc.setQueryData(keys.ci(ci.id), ci);
    },
  });
}

export function useUpdateCi(id: MaybeRefOrGetter<string>) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: CiUpdateBody) =>
      unwrap(api.PATCH("/api/v1/configuration-items/{id}", { params: { path: { id: toValue(id) } }, body })),
    onSuccess: (ci) => {
      qc.invalidateQueries({ queryKey: keys.cis });
      qc.invalidateQueries({ queryKey: keys.audit(ci.id) });
      qc.setQueryData(keys.ci(ci.id), ci);
    },
    onError: (error) => {
      // Our copy is outdated. Mark it stale so "Open the current version" refetches,
      // but do not refetch now: that would re-key the open form and drop the operator's edits.
      if (error instanceof ApiError && error.code === "VERSION_CONFLICT") {
        qc.invalidateQueries({ queryKey: keys.ci(toValue(id)), refetchType: "none" });
      }
    },
  });
}

export function useDeleteCi() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => unwrap(api.DELETE("/api/v1/configuration-items/{id}", { params: { path: { id } } })),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: keys.cis });
      qc.invalidateQueries({ queryKey: ["relationships"] });
    },
  });
}

/** The search page's filters (the term, page size and offset are passed on their own). */
export type SearchFilters = Omit<SearchQuery, "q" | "limit" | "offset">;

export function useSearch(
  q: MaybeRefOrGetter<string>,
  limit: MaybeRefOrGetter<number>,
  offset: MaybeRefOrGetter<number> = 0,
  filters: MaybeRefOrGetter<SearchFilters> = {},
) {
  return useQuery(() => {
    const text = toValue(q);
    const l = toValue(limit);
    const o = toValue(offset);
    const f = toValue(filters);
    return {
      queryKey: keys.search(text, l, o, f),
      enabled: text.trim().length > 0,
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/search", { params: { query: { ...f, q: text, limit: l, offset: o } }, signal })),
      placeholderData: keepPreviousData,
    };
  });
}

export function useGraph(
  id: MaybeRefOrGetter<string>,
  depth: MaybeRefOrGetter<number>,
  direction: MaybeRefOrGetter<"both" | "outgoing" | "incoming">,
) {
  return useQuery(() => {
    const ciId = toValue(id);
    const d = toValue(depth);
    const dir = toValue(direction);
    return {
      queryKey: keys.graph(ciId, d, dir),
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/configuration-items/{id}/graph", { params: { path: { id: ciId }, query: { depth: d, direction: dir } }, signal })),
    };
  });
}

export function useAuditLog(entityId: MaybeRefOrGetter<string>) {
  return useQuery(() => {
    const id = toValue(entityId);
    return {
      queryKey: keys.audit(id),
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/audit-log", { params: { query: { entityId: id, sort: "-occurredAt", limit: 50 } }, signal })),
    };
  });
}

// ---------- Relationships ----------

export function useRelationships(ciId: MaybeRefOrGetter<string>) {
  return useQuery(() => {
    const id = toValue(ciId);
    return {
      queryKey: keys.relationships(id),
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/relationships", { params: { query: { ciId: id, limit: MAX_PAGE, sort: "typeName" } }, signal })),
    };
  });
}

export function useRelationshipTypes(sourceClassId: MaybeRefOrGetter<string | undefined>, targetClassId: MaybeRefOrGetter<string | undefined>) {
  return useQuery(() => {
    const source = toValue(sourceClassId);
    const target = toValue(targetClassId);
    return {
      queryKey: keys.relTypes(source ?? "", target ?? ""),
      enabled: !!source && !!target,
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(
          api.GET("/api/v1/relationship-types", {
            params: { query: { sourceClassId: source, targetClassId: target, isActive: "true", limit: MAX_PAGE } },
            signal,
          }),
        ),
    };
  });
}

export function useCreateRelationship() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: RelationshipCreateBody) => unwrap(api.POST("/api/v1/relationships", { body })),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["relationships"] });
      qc.invalidateQueries({ queryKey: ["cis", "graph"] });
      qc.invalidateQueries({ queryKey: ["audit"] });
    },
  });
}

export function useDeleteRelationship() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => unwrap(api.DELETE("/api/v1/relationships/{id}", { params: { path: { id } } })),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["relationships"] });
      qc.invalidateQueries({ queryKey: ["cis", "graph"] });
      qc.invalidateQueries({ queryKey: ["audit"] });
    },
  });
}

// ---------- Classes and attribute metadata ----------

/**
 * Every CI class, in the administrator's order (Administration › Data model):
 * depth-first through the class tree, siblings by sortOrder then name. Menus,
 * pickers and the dashboard all use this order.
 */
export function useCiClasses() {
  return useQuery({
    queryKey: keys.classes,
    staleTime: 5 * 60_000,
    queryFn: ({ signal }) =>
      unwrap(api.GET("/api/v1/ci-classes", { params: { query: { limit: MAX_PAGE, sort: "sortOrder" } }, signal })).then((r) =>
        flattenTree(r.data, bySortOrder).map((n) => n.item),
      ),
  });
}

/** Every attribute a CI of this class can carry, inherited ones included. The CI form is built from this. */
export function useClassAttributes(classId: MaybeRefOrGetter<string | undefined>) {
  return useQuery(() => {
    const id = toValue(classId) ?? "";
    return {
      queryKey: keys.classAttributes(id),
      enabled: !!id,
      staleTime: 60_000,
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/ci-classes/{id}/attributes", { params: { path: { id } }, signal })).then((r) => r.data),
    };
  });
}

/** The attributes of several classes (same cache as useClassAttributes): class id -> attributes, once all have loaded. */
export function useAttributesOfClasses(classIds: MaybeRefOrGetter<readonly string[]>) {
  const ids = computed(() => [...new Set(toValue(classIds))]);
  const results = useQueries({
    queries: computed(() =>
      ids.value.map((id) => ({
        queryKey: keys.classAttributes(id),
        staleTime: 60_000,
        queryFn: ({ signal }: { signal: AbortSignal }) =>
          unwrap(api.GET("/api/v1/ci-classes/{id}/attributes", { params: { path: { id } }, signal })).then((r) => r.data),
      })),
    ),
  });
  return computed(() =>
    results.value.every((r) => r.data) ? new Map(ids.value.map((id, i) => [id, results.value[i].data!])) : undefined,
  );
}

// ---------- Older lookups (Administration › Lookups; CIs use lookup list attributes instead) ----------

export type LookupKind = "statuses" | "environments" | "locations" | "owners";

export interface LookupOption {
  id: string;
  /** Stable key (statuses, environments, locations); UI settings refer to lookups by key. */
  key?: string;
  name: string;
  isActive: boolean;
  depth?: number;
  hint?: string;
}

export function useLookup(kind: LookupKind) {
  return useQuery({
    queryKey: keys.lookup(kind),
    staleTime: 5 * 60_000,
    queryFn: async ({ signal }): Promise<LookupOption[]> => {
      const common = { limit: MAX_PAGE, sort: kind === "owners" ? "name" : "sortOrder" } as const;
      switch (kind) {
        case "statuses": {
          const r = await unwrap(api.GET("/api/v1/statuses", { params: { query: { ...common, sort: "sortOrder" } }, signal }));
          return r.data.map((s) => ({ id: s.id, key: s.key, name: s.name, isActive: s.isActive }));
        }
        case "environments": {
          const r = await unwrap(api.GET("/api/v1/environments", { params: { query: { ...common, sort: "sortOrder" } }, signal }));
          return r.data.map((s) => ({ id: s.id, key: s.key, name: s.name, isActive: s.isActive }));
        }
        case "owners": {
          const r = await unwrap(api.GET("/api/v1/owners", { params: { query: { limit: MAX_PAGE, sort: "name" } }, signal }));
          return r.data.map((o) => ({ id: o.id, name: o.name, isActive: o.isActive, hint: o.kind }));
        }
        case "locations": {
          const r = await unwrap(api.GET("/api/v1/locations", { params: { query: { limit: MAX_PAGE, sort: "name" } }, signal }));
          return flattenTree(r.data, bySortOrder).map(({ item, depth }) => ({
            id: item.id,
            key: item.key,
            name: item.name,
            isActive: item.isActive,
            depth,
            hint: item.locationType,
          }));
        }
      }
    },
  });
}
