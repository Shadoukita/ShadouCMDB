// TanStack Query composables for Administration › Data model, Dropdowns and
// Templates. Same rules as queries.ts: every request goes through the typed
// client, and a mutation invalidates every cached view its change can reach
// (a renamed lookup value also shows in CI lists; a new attribute changes CI forms).
import { useMutation, useQuery, useQueryClient, type QueryClient } from "@tanstack/vue-query";
import { toValue, type MaybeRefOrGetter } from "vue";
import { api, unwrap, type JsonBody, type Schemas } from "./client";
import { MAX_PAGE } from "./queries";

export type Area = Schemas["Area"];
export type AttributeDefinition = Schemas["AttributeDefinition"];
export type DataType = JsonBody<"/api/v1/attribute-definitions", "post">["dataType"];
export type RelationshipRule = Schemas["RelationshipRule"];
export type LookupList = Schemas["LookupList"];
export type LookupListValue = Schemas["LookupListValue"];
export type StarterTemplate = Schemas["StarterTemplate"];
export type TemplateInstallResult = Schemas["TemplateInstallResult"];
export type UsageReport = Schemas["UsageReport"];

export type AreaCreateBody = JsonBody<"/api/v1/areas", "post">;
export type AreaUpdateBody = JsonBody<"/api/v1/areas/{id}", "patch">;
export type ClassCreateBody = JsonBody<"/api/v1/ci-classes", "post">;
export type ClassUpdateBody = JsonBody<"/api/v1/ci-classes/{id}", "patch">;
export type AttributeCreateBody = JsonBody<"/api/v1/attribute-definitions", "post">;
export type AttributeUpdateBody = JsonBody<"/api/v1/attribute-definitions/{id}", "patch">;
export type RelTypeCreateBody = JsonBody<"/api/v1/relationship-types", "post">;
export type RelTypeUpdateBody = JsonBody<"/api/v1/relationship-types/{id}", "patch">;
export type RuleCreateBody = JsonBody<"/api/v1/relationship-rules", "post">;
export type LookupListBody = JsonBody<"/api/v1/lookup-lists", "post">;
export type LookupListValueBody = JsonBody<"/api/v1/lookup-list-values", "post">;

/** Every data model and lookup resource. Each has GET/PATCH/DELETE /{resource}/{id} and GET …/{id}/usage. */
export type Resource =
  | "areas"
  | "ci-classes"
  | "attribute-definitions"
  | "relationship-types"
  | "relationship-rules"
  | "lookup-lists"
  | "lookup-list-values";

export const dmKeys = {
  classAttributesOwn: (classId: string) => ["attribute-definitions", "class", classId] as const,
  relTypeList: ["relationship-types", "admin"] as const,
  relType: (id: string) => ["relationship-types", "detail", id] as const,
  rules: (q: { relationshipTypeId?: string; classId?: string }) => ["relationship-rules", q] as const,
  lookupLists: ["lookup-lists"] as const,
  lookupListValues: (listId: string, parentValueId?: string) =>
    (parentValueId ? ["lookup-list-values", listId, parentValueId] : ["lookup-list-values", listId]) as readonly string[],
  templates: ["admin", "templates"] as const,
  usage: (resource: Resource, id: string) => ["usage", resource, id] as const,
};

/** Query-key prefixes a change to each resource can make stale. */
const AFFECTS: Record<Resource, readonly (readonly unknown[])[]> = {
  areas: [["areas"], ["technical-names"], ["ci-classes"], ["cis"], ["schema-changes"], dmKeys.templates],
  "ci-classes": [["areas"], ["technical-names"], ["ci-classes"], ["attribute-definitions"], ["relationship-rules"], ["relationship-types"], ["cis"], ["schema-changes"], dmKeys.templates],
  "attribute-definitions": [["technical-names"], ["ci-classes"], ["attribute-definitions"], ["cis", "detail"], ["schema-changes"], dmKeys.templates],
  "relationship-types": [["relationship-types"], ["relationship-rules"], ["relationships"], ["cis", "graph"], ["cis", "impact"], ["impact-settings"], dmKeys.templates],
  "relationship-rules": [["relationship-rules"], ["relationship-types"], dmKeys.templates],
  "lookup-lists": [["lookup-lists"], ["lookup-list-values"], ["ci-classes"], ["attribute-definitions"]],
  // Criticality is a lookup list: its values show in CI lists and impact results too.
  "lookup-list-values": [["lookup-list-values"], ["cis"]],
};

export function invalidateResource(qc: QueryClient, resource: Resource) {
  for (const queryKey of AFFECTS[resource]) qc.invalidateQueries({ queryKey: [...queryKey] });
  qc.removeQueries({ queryKey: ["usage", resource] });
}

// The generic operations share one shape across resources; the path cast picks one representative for typing.
type ItemPath = "/api/v1/lookup-list-values/{id}";
type UsagePath = "/api/v1/lookup-list-values/{id}/usage";
const itemPath = (r: Resource) => `/api/v1/${r}/{id}` as ItemPath;

/** What still refers to a row (fetched fresh when a delete dialog opens). */
export function useUsage(resource: Resource, id: MaybeRefOrGetter<string | undefined>, enabled: MaybeRefOrGetter<boolean> = true) {
  return useQuery(() => {
    const rowId = toValue(id) ?? "";
    return {
      queryKey: dmKeys.usage(resource, rowId),
      enabled: !!rowId && toValue(enabled),
      staleTime: 0,
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET(`/api/v1/${resource}/{id}/usage` as UsagePath, { params: { path: { id: rowId } }, signal })),
    };
  });
}

/** PATCH any data model row. Bodies are typed at the call sites that build them; this is the shared transport. */
export function usePatch<T = unknown>(resource: Resource) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, body }: { id: string; body: Record<string, unknown> }) =>
      unwrap(api.PATCH(itemPath(resource), { params: { path: { id } }, body: body as never })) as Promise<T>,
    onSuccess: () => invalidateResource(qc, resource),
  });
}

export function useRemove(resource: Resource) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => unwrap(api.DELETE(itemPath(resource), { params: { path: { id } } })),
    onSuccess: () => invalidateResource(qc, resource),
  });
}

/**
 * Persists a new order by PATCHing sortOrder (10, 20, 30, …) on each row whose
 * value changes; the API has no bulk reorder. Extra fields per row (e.g. a new
 * form section for a moved attribute) ride along in the same PATCH.
 */
export function useReorder(resource: Resource) {
  const qc = useQueryClient();
  return useMutation({
    // Rows get 10, 20, 30… in the given order, unless a row names its `next` sort order (reordering a
    // filtered part of a list keeps the positions that part had among the rest).
    mutationFn: async (rows: { id: string; sortOrder: number; next?: number; extra?: Record<string, unknown> }[]) => {
      const writes = rows
        .map((r, i) => ({ ...r, next: r.next ?? (i + 1) * 10 }))
        .filter((r) => r.sortOrder !== r.next || r.extra);
      for (const r of writes) {
        await unwrap(api.PATCH(itemPath(resource), { params: { path: { id: r.id } }, body: { sortOrder: r.next, ...r.extra } as never }));
      }
      return writes.length;
    },
    // Also after a partial failure: the rows written so far did change.
    onSettled: () => invalidateResource(qc, resource),
  });
}

// ---------- Areas ----------

/**
 * Every area, archived ones included, in tab order. Areas are few (one per
 * menu tab), so they are fetched whole; screens filter out archived ones.
 */
export function useAreas() {
  return useQuery({
    queryKey: ["areas"],
    staleTime: 5 * 60_000,
    queryFn: ({ signal }) =>
      unwrap(api.GET("/api/v1/areas", { params: { query: { limit: MAX_PAGE, sort: "sortOrder" } }, signal })).then((r) => r.data),
  });
}

export function useCreateArea() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: AreaCreateBody) => unwrap(api.POST("/api/v1/areas", { body })),
    onSuccess: () => invalidateResource(qc, "areas"),
  });
}

// ---------- CI classes ----------

export function useCiClass(id: MaybeRefOrGetter<string | undefined>) {
  return useQuery(() => {
    const classId = toValue(id) ?? "";
    return {
      queryKey: ["ci-classes", "detail", classId],
      enabled: !!classId,
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/ci-classes/{id}", { params: { path: { id: classId } }, signal })),
    };
  });
}

export function useCreateClass() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: ClassCreateBody) => unwrap(api.POST("/api/v1/ci-classes", { body })),
    onSuccess: () => invalidateResource(qc, "ci-classes"),
  });
}

// ---------- Attribute definitions ----------

/** Attributes defined directly on one class (inherited ones come from useClassAttributes). */
export function useOwnAttributes(classId: MaybeRefOrGetter<string | undefined>) {
  return useQuery(() => {
    const id = toValue(classId) ?? "";
    return {
      queryKey: dmKeys.classAttributesOwn(id),
      enabled: !!id,
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(
          api.GET("/api/v1/attribute-definitions", { params: { query: { classId: id, limit: MAX_PAGE, sort: "sortOrder" } }, signal }),
        ),
    };
  });
}

export function useCreateAttribute() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: AttributeCreateBody) => unwrap(api.POST("/api/v1/attribute-definitions", { body })),
    onSuccess: () => invalidateResource(qc, "attribute-definitions"),
  });
}

// ---------- Relationship types and rules ----------

export function useRelTypeList() {
  return useQuery({
    queryKey: dmKeys.relTypeList,
    queryFn: ({ signal }) =>
      unwrap(api.GET("/api/v1/relationship-types", { params: { query: { limit: MAX_PAGE, sort: "sortOrder" } }, signal })),
  });
}

export function useRelType(id: MaybeRefOrGetter<string | undefined>) {
  return useQuery(() => {
    const typeId = toValue(id) ?? "";
    return {
      queryKey: dmKeys.relType(typeId),
      enabled: !!typeId,
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/relationship-types/{id}", { params: { path: { id: typeId } }, signal })),
    };
  });
}

export function useCreateRelType() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: RelTypeCreateBody) => unwrap(api.POST("/api/v1/relationship-types", { body })),
    onSuccess: () => invalidateResource(qc, "relationship-types"),
  });
}

/** Every rule (small: one row per legal type × class pair); filtered in the views that show them. */
export function useRules() {
  return useQuery({
    queryKey: dmKeys.rules({}),
    queryFn: ({ signal }) => unwrap(api.GET("/api/v1/relationship-rules", { params: { query: { limit: MAX_PAGE } }, signal })),
  });
}

export function useCreateRule() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: RuleCreateBody) => unwrap(api.POST("/api/v1/relationship-rules", { body })),
    onSuccess: () => invalidateResource(qc, "relationship-rules"),
  });
}

/** POST a lookup list value. Bodies are built by the value editor from its typed field list. */
export function useCreateLookup(resource: Extract<Resource, "lookup-list-values">) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: Record<string, unknown>) =>
      unwrap(api.POST(`/api/v1/${resource}` as "/api/v1/lookup-list-values", { body: body as LookupListValueBody })),
    onSuccess: () => invalidateResource(qc, resource),
  });
}

// ---------- Admin-defined lookup lists ----------

export function useLookupLists() {
  return useQuery({
    queryKey: dmKeys.lookupLists,
    staleTime: 5 * 60_000,
    queryFn: ({ signal }) =>
      unwrap(api.GET("/api/v1/lookup-lists", { params: { query: { limit: MAX_PAGE, sort: "sortOrder" } }, signal })).then((r) => r.data),
  });
}

export function useCreateLookupList() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: LookupListBody) => unwrap(api.POST("/api/v1/lookup-lists", { body })),
    onSuccess: () => invalidateResource(qc, "lookup-lists"),
  });
}

/**
 * The values of one list, in order. Also used by CI forms and detail pages for `lookup` attributes.
 * `parentValueId` (a value id, or "none" for values not assigned to one) narrows a dependent list
 * on the server: the values of Model that belong to Cisco.
 */
export function useLookupListValues(
  listId: MaybeRefOrGetter<string | null | undefined>,
  parentValueId: MaybeRefOrGetter<string | null | undefined> = undefined,
) {
  return useQuery(() => {
    const id = toValue(listId) ?? "";
    const parent = toValue(parentValueId) || undefined;
    return {
      queryKey: dmKeys.lookupListValues(id, parent),
      enabled: !!id,
      staleTime: 60_000,
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(
          api.GET("/api/v1/lookup-list-values", {
            params: { query: { listId: id, limit: MAX_PAGE, sort: "sortOrder", ...(parent ? { parentValueId: parent } : {}) } },
            signal,
          }),
        ).then((r) => r.data),
    };
  });
}

/** Every value of every list, page by page. UI settings name lookup values by list key and value key; filters need their ids. */
export function useAllLookupListValues() {
  return useQuery({
    queryKey: ["lookup-list-values", "all"],
    staleTime: 60_000,
    queryFn: async ({ signal }) => {
      const out: LookupListValue[] = [];
      for (let offset = 0; ; offset += MAX_PAGE) {
        const r = await unwrap(api.GET("/api/v1/lookup-list-values", { params: { query: { limit: MAX_PAGE, offset, sort: "sortOrder" } }, signal }));
        out.push(...r.data);
        if (r.data.length === 0 || out.length >= r.page.total) return out;
      }
    },
  });
}

// ---------- Starter templates ----------

export function useTemplates(enabled: MaybeRefOrGetter<boolean> = true) {
  return useQuery(() => ({
    queryKey: dmKeys.templates,
    enabled: toValue(enabled),
    queryFn: ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/admin/templates", { signal })).then((r) => r.data),
  }));
}

export function useInstallTemplate() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (key: string) => unwrap(api.POST("/api/v1/admin/templates/{key}/install", { params: { path: { key } } })),
    // A template touches the whole data model and every lookup: refetch everything.
    onSuccess: () => qc.invalidateQueries(),
  });
}
