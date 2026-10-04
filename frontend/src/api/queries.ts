// TanStack Query composables over the typed client. Query keys live here so that
// mutations invalidate exactly what they change. Arguments are refs or getters,
// so a query refetches when the URL or form state it depends on changes.
import { keepPreviousData, useMutation, useQueries, useQuery, useQueryClient, type QueryClient } from "@tanstack/vue-query";
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
export type ImpactSettings = Schemas["ImpactSettings"];
export type ImpactParams = NonNullable<paths["/api/v1/configuration-items/{id}/impact"]["get"]["parameters"]["query"]>;

export type ChangeHistogramQuery = NonNullable<paths["/api/v1/configuration-items/change-histogram"]["get"]["parameters"]["query"]>;
export type CiListQuery = NonNullable<paths["/api/v1/configuration-items"]["get"]["parameters"]["query"]>;
export type SearchQuery = paths["/api/v1/search"]["get"]["parameters"]["query"];
export type CiCreateBody = NonNullable<paths["/api/v1/configuration-items"]["post"]["requestBody"]>["content"]["application/json"];
export type CiUpdateBody = NonNullable<paths["/api/v1/configuration-items/{id}"]["patch"]["requestBody"]>["content"]["application/json"];
export type RelationshipCreateBody = NonNullable<paths["/api/v1/relationships"]["post"]["requestBody"]>["content"]["application/json"];

/** Largest page the API serves; used for small reference lists (classes, lookup lists, ...). */
export const MAX_PAGE = 200;

export const keys = {
  cis: ["cis"] as const,
  ciList: (q: CiListQuery) => ["cis", "list", q] as const,
  ciCount: (q: CiListQuery) => ["cis", "count", q] as const,
  changeHistogram: (q: ChangeHistogramQuery) => ["cis", "change-histogram", q] as const,
  ci: (id: string) => ["cis", "detail", id] as const,
  graph: (id: string, depth: number, direction: string) => ["cis", "graph", id, depth, direction] as const,
  impact: (id: string, params: ImpactParams) => ["cis", "impact", id, params] as const,
  impactSettings: ["impact-settings"] as const,
  criticality: ["lookup-list-values", "criticality"] as const,
  search: (q: string, limit: number, offset: number, filters: SearchFilters = {}) => ["cis", "search", q, limit, offset, filters] as const,
  relationships: (ciId: string) => ["relationships", ciId] as const,
  audit: (entityId: string) => ["audit", entityId] as const,
  classes: ["ci-classes"] as const,
  classAttributes: (classId: string) => ["ci-classes", classId, "attributes"] as const,
  relTypes: (sourceClassId: string, targetClassId: string) => ["relationship-types", sourceClassId, targetClassId] as const,
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

/**
 * Changes per hour or day to the CIs of a list query (needs audit.view). The previous histogram
 * stays on screen while a changed filter or range loads; a refusal is shown at once, not retried.
 */
export function useChangeHistogram(query: MaybeRefOrGetter<ChangeHistogramQuery>, enabled: MaybeRefOrGetter<boolean> = true) {
  return useQuery(() => {
    const q = toValue(query);
    return {
      queryKey: keys.changeHistogram(q),
      enabled: toValue(enabled),
      retry: false,
      staleTime: 60_000,
      queryFn: ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/configuration-items/change-histogram", { params: { query: q }, signal })),
      placeholderData: keepPreviousData,
    };
  });
}

/** A page of the CI list, fetched once (cached like useCiList): for actions that need a CI, e.g. a sample to open. */
export function fetchCiList(qc: QueryClient, query: CiListQuery) {
  return qc.fetchQuery({
    queryKey: keys.ciList(query),
    queryFn: ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/configuration-items", { params: { query }, signal })),
    staleTime: 10_000,
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

const ciQuery = (ciId: string) => ({
  queryKey: keys.ci(ciId),
  queryFn: ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/configuration-items/{id}", { params: { path: { id: ciId } }, signal })),
});

export function useCi(id: MaybeRefOrGetter<string | undefined>) {
  return useQuery(() => {
    const ciId = toValue(id) ?? "";
    return { ...ciQuery(ciId), enabled: !!ciId };
  });
}

/** One CI, outside a component (from the cache when fresh). */
export const fetchCi = (qc: QueryClient, id: string) => qc.fetchQuery({ ...ciQuery(id), staleTime: 10_000 });

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
      qc.invalidateQueries({ queryKey: ["cis", "impact"] });
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
      qc.invalidateQueries({ queryKey: ["cis", "impact"] });
      qc.invalidateQueries({ queryKey: ["audit"] });
    },
  });
}

// ---------- Impact analysis ----------

/** The bounds and defaults of an analysis, and whether any relationship type propagates impact. */
export function useImpactSettings() {
  return useQuery({
    queryKey: keys.impactSettings,
    staleTime: 60_000,
    queryFn: ({ signal }) => unwrap(api.GET("/api/v1/settings/impact", { signal })),
  });
}

/**
 * GET /configuration-items/{id}/impact. Never retried: a refusal (busy server, bad parameter) is
 * shown at once, and the operator's Retry is the retry. The previous result stays on screen while
 * a changed control re-runs the analysis.
 */
export function useImpact(id: MaybeRefOrGetter<string>, params: MaybeRefOrGetter<ImpactParams>, enabled: MaybeRefOrGetter<boolean> = true) {
  return useQuery(() => {
    const ciId = toValue(id);
    const p = toValue(params);
    return {
      queryKey: keys.impact(ciId, p),
      enabled: !!ciId && toValue(enabled),
      retry: false,
      placeholderData: keepPreviousData,
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/configuration-items/{id}/impact", { params: { path: { id: ciId }, query: p }, signal })),
    };
  });
}

/**
 * The analysis as CSV from GET …/impact/export (built, neutralised against spreadsheet formulas and
 * audited by the server), saved under the file name the server gives (or `fallbackName`).
 */
export async function downloadImpactCsv(id: string, params: ImpactParams, fallbackName: string): Promise<void> {
  const { data, error, response } = await api.GET("/api/v1/configuration-items/{id}/impact/export", {
    params: { path: { id }, query: params },
    parseAs: "blob",
  });
  if (!response.ok) await unwrap(Promise.resolve({ data: undefined, error, response }));
  // The header is unreadable when the API is on another origin and does not expose it.
  const name = /filename="?([^";]+)"?/.exec(response.headers.get("Content-Disposition") ?? "")?.[1] ?? fallbackName;
  const a = document.createElement("a");
  a.href = URL.createObjectURL(data as Blob);
  a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(a.href), 1000);
}

/** The values of the system lookup list `criticality`, most critical first (the CI form, filters and badges). */
export function useCriticalityValues() {
  return useQuery({
    queryKey: keys.criticality,
    staleTime: 60_000,
    queryFn: async ({ signal }) => {
      const lists = await unwrap(api.GET("/api/v1/lookup-lists", { params: { query: { systemRole: "criticality", limit: 1 } }, signal }));
      const list = lists.data[0];
      if (!list) return [];
      const values = await unwrap(
        api.GET("/api/v1/lookup-list-values", { params: { query: { listId: list.id, limit: MAX_PAGE, sort: "sortOrder" } }, signal }),
      );
      return values.data;
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

/**
 * Every attribute a CI of this class can carry, inherited ones included. The CI form is built from this.
 * `includeInactive` adds the archived ones (the detail page shows their stored values); it is cached
 * under the class's attributes key, so invalidating that refreshes both.
 */
export function useClassAttributes(classId: MaybeRefOrGetter<string | undefined>, opts: { includeInactive?: boolean } = {}) {
  return useQuery(() => {
    const id = toValue(classId) ?? "";
    const query = opts.includeInactive ? { includeInactive: "true" as const } : {};
    return {
      queryKey: opts.includeInactive ? ([...keys.classAttributes(id), "all"] as const) : keys.classAttributes(id),
      enabled: !!id,
      staleTime: 60_000,
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/ci-classes/{id}/attributes", { params: { path: { id }, query }, signal })).then((r) => r.data),
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
