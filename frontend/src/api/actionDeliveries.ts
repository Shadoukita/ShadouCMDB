// TanStack Query composables for Administration › Workflow deliveries (SHAA-2725 §4.4, §11.2): one workflow's
// action deliveries, their summary and the whole outbox's state, retry and discard. `workflows.manage`; webhook
// deliveries are listed only with `webhooks.manage` too.
import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { toValue, type MaybeRefOrGetter } from "vue";
import { api, unwrap, type JsonBody as Body, type ListQuery, type Schemas } from "./client";

export type ActionDelivery = Schemas["WorkflowActionDelivery"];
export type ActionDeliveryDetail = Schemas["WorkflowActionDeliveryDetail"];
export type ActionDeliveryStatus = Schemas["WorkflowActionDeliveryStatus"];
export type ActionDeliveryFilter = Schemas["WorkflowActionDeliveryFilter"];
export type ActionDeliveryBulkResult = Schemas["WorkflowActionDeliveryBulkResult"];
export type ActionsSummary = Schemas["WorkflowActionsSummary"];
export type ActionDeliveryListQuery = ListQuery<"/api/v1/admin/workflow-definitions/{id}/action-deliveries">;
export type ActionDeliveryBulkBody = Body<"/api/v1/admin/workflow-definitions/{id}/action-deliveries/retry", "post">;

export const deliveryKeys = {
  all: ["admin", "action-deliveries"] as const,
  list: (id: string, q: ActionDeliveryListQuery) => ["admin", "action-deliveries", id, "list", q] as const,
  detail: (id: string, deliveryId: string) => ["admin", "action-deliveries", id, "detail", deliveryId] as const,
  summary: (id: string) => ["admin", "action-deliveries", id, "summary"] as const,
};

export function useActionDeliveries(id: MaybeRefOrGetter<string | undefined>, query: MaybeRefOrGetter<ActionDeliveryListQuery>) {
  return useQuery(() => {
    const wid = toValue(id) ?? "";
    const q = toValue(query);
    return {
      queryKey: deliveryKeys.list(wid, q),
      enabled: !!wid,
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/admin/workflow-definitions/{id}/action-deliveries", { params: { path: { id: wid }, query: q }, signal })),
      placeholderData: keepPreviousData,
    };
  });
}

export function useActionDelivery(id: MaybeRefOrGetter<string | undefined>, deliveryId: MaybeRefOrGetter<string | undefined>) {
  return useQuery(() => {
    const wid = toValue(id) ?? "";
    const did = toValue(deliveryId) ?? "";
    return {
      queryKey: deliveryKeys.detail(wid, did),
      enabled: !!wid && !!did,
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/admin/workflow-definitions/{id}/action-deliveries/{deliveryId}", { params: { path: { id: wid, deliveryId: did } }, signal })),
    };
  });
}

/** Per action, the last 24 hours and 7 days by status, and the outbox as the workers last counted it. */
export function useActionsSummary(id: MaybeRefOrGetter<string | undefined>) {
  return useQuery(() => {
    const wid = toValue(id) ?? "";
    return {
      queryKey: deliveryKeys.summary(wid),
      enabled: !!wid,
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/admin/workflow-definitions/{id}/actions/summary", { params: { path: { id: wid } }, signal })),
    };
  });
}

/**
 * Retry or discard, by ids or by filter (at most 1,000 per request; `more` says others remain). Each changed
 * delivery is one audit row.
 */
export function useBulkDeliveryAction() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, op, body }: { id: string; op: "retry" | "discard"; body: ActionDeliveryBulkBody }) =>
      op === "retry"
        ? unwrap(api.POST("/api/v1/admin/workflow-definitions/{id}/action-deliveries/retry", { params: { path: { id } }, body }))
        : unwrap(api.POST("/api/v1/admin/workflow-definitions/{id}/action-deliveries/discard", { params: { path: { id } }, body })),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: deliveryKeys.all });
      qc.invalidateQueries({ queryKey: ["audit"] });
    },
  });
}
