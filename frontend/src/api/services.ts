// Business services (spec SHAA-927 §4): a service is a CI of the built-in business service class, so its own
// fields go through the CI endpoints (queries.ts); this module covers the service views, owners and lookups.
import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { toValue, type MaybeRefOrGetter } from "vue";
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
