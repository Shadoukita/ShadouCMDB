// TanStack Query hooks over the typed client. Query keys live here so that
// mutations invalidate exactly what they change.
import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, unwrap, type Schemas } from "./client";
import type { paths } from "./schema";

export type CiSummary = Schemas["ConfigurationItemSummary"];
export type Ci = Schemas["ConfigurationItem"];
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
  search: (q: string, limit: number, offset: number) => ["cis", "search", q, limit, offset] as const,
  relationships: (ciId: string) => ["relationships", ciId] as const,
  audit: (entityId: string) => ["audit", entityId] as const,
  classes: ["ci-classes"] as const,
  classAttributes: (classId: string) => ["ci-classes", classId, "attributes"] as const,
  relTypes: (sourceClassId: string, targetClassId: string) => ["relationship-types", sourceClassId, targetClassId] as const,
  lookup: (kind: LookupKind) => ["lookup", kind] as const,
};

// ---------- Configuration items ----------

export function useCiList(query: CiListQuery) {
  return useQuery({
    queryKey: keys.ciList(query),
    queryFn: ({ signal }) => unwrap(api.GET("/api/v1/configuration-items", { params: { query }, signal })),
    placeholderData: keepPreviousData,
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

export function useCi(id: string | undefined) {
  return useQuery({
    queryKey: keys.ci(id ?? ""),
    enabled: !!id,
    queryFn: ({ signal }) => unwrap(api.GET("/api/v1/configuration-items/{id}", { params: { path: { id: id! } }, signal })),
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

export function useUpdateCi(id: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: CiUpdateBody) => unwrap(api.PATCH("/api/v1/configuration-items/{id}", { params: { path: { id } }, body })),
    onSuccess: (ci) => {
      qc.invalidateQueries({ queryKey: keys.cis });
      qc.invalidateQueries({ queryKey: keys.audit(id) });
      qc.setQueryData(keys.ci(ci.id), ci);
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

export function useSearch(q: string, limit: number, offset = 0) {
  return useQuery({
    queryKey: keys.search(q, limit, offset),
    enabled: q.trim().length > 0,
    queryFn: ({ signal }) => unwrap(api.GET("/api/v1/search", { params: { query: { q, limit, offset } }, signal })),
    placeholderData: keepPreviousData,
  });
}

export function useGraph(id: string, depth: number, direction: "both" | "outgoing" | "incoming") {
  return useQuery({
    queryKey: keys.graph(id, depth, direction),
    queryFn: ({ signal }) =>
      unwrap(api.GET("/api/v1/configuration-items/{id}/graph", { params: { path: { id }, query: { depth, direction } }, signal })),
  });
}

export function useAuditLog(entityId: string) {
  return useQuery({
    queryKey: keys.audit(entityId),
    queryFn: ({ signal }) =>
      unwrap(
        api.GET("/api/v1/audit-log", {
          params: { query: { entityId, sort: "-occurredAt", limit: 50 } },
          signal,
        }),
      ),
  });
}

// ---------- Relationships ----------

export function useRelationships(ciId: string) {
  return useQuery({
    queryKey: keys.relationships(ciId),
    queryFn: ({ signal }) =>
      unwrap(api.GET("/api/v1/relationships", { params: { query: { ciId, limit: MAX_PAGE, sort: "typeName" } }, signal })),
  });
}

export function useRelationshipTypes(sourceClassId: string | undefined, targetClassId: string | undefined) {
  return useQuery({
    queryKey: keys.relTypes(sourceClassId ?? "", targetClassId ?? ""),
    enabled: !!sourceClassId && !!targetClassId,
    queryFn: ({ signal }) =>
      unwrap(
        api.GET("/api/v1/relationship-types", {
          params: { query: { sourceClassId, targetClassId, isActive: "true", limit: MAX_PAGE } },
          signal,
        }),
      ),
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

export function useCiClasses() {
  return useQuery({
    queryKey: keys.classes,
    staleTime: 5 * 60_000,
    queryFn: ({ signal }) =>
      unwrap(api.GET("/api/v1/ci-classes", { params: { query: { limit: MAX_PAGE, sort: "name" } }, signal })).then((r) => r.data),
  });
}

/** Every attribute a CI of this class can carry, inherited ones included. The CI form is built from this. */
export function useClassAttributes(classId: string | undefined) {
  return useQuery({
    queryKey: keys.classAttributes(classId ?? ""),
    enabled: !!classId,
    staleTime: 60_000,
    queryFn: ({ signal }) =>
      unwrap(api.GET("/api/v1/ci-classes/{id}/attributes", { params: { path: { id: classId! } }, signal })).then((r) => r.data),
  });
}

// ---------- Lookups (pickers and filters) ----------

export type LookupKind = "statuses" | "environments" | "locations" | "owners";

export interface LookupOption {
  id: string;
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
          return r.data.map((s) => ({ id: s.id, name: s.name, isActive: s.isActive }));
        }
        case "environments": {
          const r = await unwrap(api.GET("/api/v1/environments", { params: { query: { ...common, sort: "sortOrder" } }, signal }));
          return r.data.map((s) => ({ id: s.id, name: s.name, isActive: s.isActive }));
        }
        case "owners": {
          const r = await unwrap(api.GET("/api/v1/owners", { params: { query: { limit: MAX_PAGE, sort: "name" } }, signal }));
          return r.data.map((o) => ({ id: o.id, name: o.name, isActive: o.isActive, hint: o.kind }));
        }
        case "locations": {
          const r = await unwrap(api.GET("/api/v1/locations", { params: { query: { limit: MAX_PAGE, sort: "name" } }, signal }));
          return flattenTree(r.data).map(({ item, depth }) => ({
            id: item.id,
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

/** Orders a parentId tree depth-first so pickers can indent children under their parent. */
function flattenTree<T extends { id: string; parentId: string | null }>(items: T[]): { item: T; depth: number }[] {
  const byParent = new Map<string | null, T[]>();
  const ids = new Set(items.map((i) => i.id));
  for (const i of items) {
    const parent = i.parentId && ids.has(i.parentId) ? i.parentId : null;
    byParent.set(parent, [...(byParent.get(parent) ?? []), i]);
  }
  const out: { item: T; depth: number }[] = [];
  const walk = (parent: string | null, depth: number) => {
    for (const i of byParent.get(parent) ?? []) {
      out.push({ item: i, depth });
      walk(i.id, depth + 1);
    }
  };
  walk(null, 0);
  return out;
}
