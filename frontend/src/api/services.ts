// Business services (spec SHAA-927 §4): a service is a CI of the built-in business service class, so its own
// fields go through the CI endpoints (queries.ts); this module covers the service views, owners and lookups.
import { keepPreviousData, useMutation, useQueries, useQuery, useQueryClient } from "@tanstack/vue-query";
import { computed, toValue, type MaybeRefOrGetter } from "vue";
import { api, unwrap, type JsonBody, type ListQuery, type Schemas } from "./client";
import { keys as ciKeys } from "./queries";

export type ServiceSettings = Schemas["BusinessServiceSettings"];
export type ServiceSummary = Schemas["BusinessServiceSummary"];
export type Service = Schemas["BusinessService"];
export type ServiceOwners = Schemas["ServiceOwners"];
export type PrincipalRef = Schemas["PrincipalRef"];
export type Principal = Schemas["Principal"];
export type ServiceMember = Schemas["Member"];
export type ServiceListQuery = ListQuery<"/api/v1/business-services">;
export type ServiceMemberQuery = ListQuery<"/api/v1/business-services/{id}/members">;
export type OwnersBody = JsonBody<"/api/v1/business-services/{id}/owners", "put">;
export type OwnerRole = "technical" | "business";

export const serviceKeys = {
  all: ["business-services"] as const,
  settings: ["business-services", "settings"] as const,
  list: (q: ServiceListQuery) => ["business-services", "list", q] as const,
  detail: (id: string) => ["business-services", "detail", id] as const,
  members: (id: string, q: ServiceMemberQuery) => ["business-services", "members", id, q] as const,
  ofCi: (ciId: string) => ["business-services", "of-ci", ciId] as const,
};

/** The service class, the caller's rights on it and the limits (any signed-in user; drives the nav entry). */
export function useServiceSettings() {
  return useQuery({
    queryKey: serviceKeys.settings,
    staleTime: 5 * 60_000,
    queryFn: ({ signal }) => unwrap(api.GET("/api/v1/settings/business-services", { signal })),
  });
}

export function useServiceList(query: MaybeRefOrGetter<ServiceListQuery>, enabled: MaybeRefOrGetter<boolean> = true) {
  return useQuery(() => {
    const q = toValue(query);
    return {
      queryKey: serviceKeys.list(q),
      enabled: toValue(enabled),
      queryFn: ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/business-services", { params: { query: q }, signal })),
      placeholderData: keepPreviousData,
    };
  });
}

export function useService(id: MaybeRefOrGetter<string>) {
  return useQuery(() => {
    const serviceId = toValue(id);
    return {
      queryKey: serviceKeys.detail(serviceId),
      enabled: !!serviceId,
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/business-services/{id}", { params: { path: { id: serviceId } }, signal })),
    };
  });
}

export function useServiceMembers(id: MaybeRefOrGetter<string>, query: MaybeRefOrGetter<ServiceMemberQuery>) {
  return useQuery(() => {
    const serviceId = toValue(id);
    const q = toValue(query);
    return {
      queryKey: serviceKeys.members(serviceId, q),
      enabled: !!serviceId,
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/business-services/{id}/members", { params: { path: { id: serviceId }, query: q }, signal })),
      placeholderData: keepPreviousData,
    };
  });
}

/** The services a CI is part of, directly or through nesting (bounded, not paged). */
export function useServicesOfCi(ciId: MaybeRefOrGetter<string>, enabled: MaybeRefOrGetter<boolean> = true) {
  return useQuery(() => {
    const id = toValue(ciId);
    return {
      queryKey: serviceKeys.ofCi(id),
      enabled: !!id && toValue(enabled),
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/configuration-items/{id}/business-services", { params: { path: { id } }, signal })),
    };
  });
}

/** The owner picker's lookup: at most 20 users and groups for a term of 2 or more characters. */
export async function searchPrincipals(q: string, signal?: AbortSignal): Promise<Principal[]> {
  const res = await unwrap(api.GET("/api/v1/principals", { params: { query: { q, includeInactive: "true" } }, signal }));
  return res.data;
}

export function useReplaceOwners(id: MaybeRefOrGetter<string>) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: OwnersBody) =>
      unwrap(api.PUT("/api/v1/business-services/{id}/owners", { params: { path: { id: toValue(id) } }, body })),
    onSuccess: () => {
      // The owners bump the service's version: the detail, the lists and the CI record (its version and history).
      const serviceId = toValue(id);
      qc.invalidateQueries({ queryKey: serviceKeys.all });
      qc.invalidateQueries({ queryKey: ciKeys.ci(serviceId) });
      qc.invalidateQueries({ queryKey: ciKeys.audit(serviceId) });
    },
  });
}

/** Which of `ciIds` are already members (the picker's "Already a member"): one call per result page. */
export function useMembershipOf(serviceId: MaybeRefOrGetter<string>, ciIds: MaybeRefOrGetter<string[]>) {
  return useQuery(() => {
    const id = toValue(serviceId);
    const ids = toValue(ciIds);
    return {
      queryKey: [...serviceKeys.members(id, { ciId: ids.join(",") }), "membership"] as const,
      enabled: !!id && ids.length > 0,
      queryFn: async ({ signal }: { signal: AbortSignal }) => {
        const res = await unwrap(
          api.GET("/api/v1/business-services/{id}/members", {
            params: { path: { id }, query: { ciId: ids.join(","), limit: Math.max(1, ids.length) } },
            signal,
          }),
        );
        return new Set(res.data.map((m) => m.ci.id));
      },
    };
  });
}

/** After a member change: the member lists, the counts on the service and the CI's history. */
function invalidateMembers(qc: ReturnType<typeof useQueryClient>, serviceId: string) {
  qc.invalidateQueries({ queryKey: serviceKeys.all });
  qc.invalidateQueries({ queryKey: ciKeys.ci(serviceId) });
  qc.invalidateQueries({ queryKey: ciKeys.audit(serviceId) });
  // A membership is a relationship of the member too, and an impact path.
  qc.invalidateQueries({ queryKey: ["relationships"] });
  qc.invalidateQueries({ queryKey: [...ciKeys.cis, "impact"] });
}

/** Adds members (all or nothing): `alreadyMembers` are not an error. */
export function useAddMembers(serviceId: MaybeRefOrGetter<string>) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (memberIds: string[]) =>
      unwrap(api.POST("/api/v1/business-services/{id}/members", { params: { path: { id: toValue(serviceId) } }, body: { memberIds } })),
    onSuccess: () => invalidateMembers(qc, toValue(serviceId)),
  });
}

/** Removes members: one id through the single-member route, several through the batch (all or nothing). */
export function useRemoveMembers(serviceId: MaybeRefOrGetter<string>) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async (memberIds: string[]) => {
      const id = toValue(serviceId);
      if (memberIds.length === 1) {
        await unwrap(api.DELETE("/api/v1/business-services/{id}/members/{ciId}", { params: { path: { id, ciId: memberIds[0] } } }));
      } else {
        await unwrap(api.POST("/api/v1/business-services/{id}/members/remove", { params: { path: { id } }, body: { memberIds } }));
      }
    },
    onSettled: () => invalidateMembers(qc, toValue(serviceId)),
  });
}

/** Downloads the member CSV (the list's filters, no paging; recorded in the audit log). */
export async function downloadMembersCsv(serviceId: string, query: Omit<ServiceMemberQuery, "limit" | "offset">, fallbackName: string): Promise<void> {
  const { data, error, response } = await api.GET("/api/v1/business-services/{id}/members/export", {
    params: { path: { id: serviceId }, query },
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

/** Several services by id (the owners of the Impact tab's pinned services); a failed one is just left out. */
export function useServicesById(ids: MaybeRefOrGetter<string[]>) {
  return useQueries({
    queries: computed(() =>
      toValue(ids).map((id) => ({
        queryKey: serviceKeys.detail(id),
        queryFn: ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/business-services/{id}", { params: { path: { id } }, signal })),
        staleTime: 60_000,
        retry: false,
      })),
    ),
    combine: (results) => new Map(results.flatMap((r) => (r.data ? [[r.data.id, r.data] as const] : []))),
  });
}
