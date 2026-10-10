// Webhook endpoints (SHAA-2725 §5). Same rules as admin.ts: every request goes through the typed client.
import { useQuery } from "@tanstack/vue-query";
import { toValue, type MaybeRefOrGetter } from "vue";
import { api, unwrap, type Schemas } from "./client";
import { MAX_PAGE } from "./queries";

export type WebhookEndpoint = Schemas["WebhookEndpoint"];
export type WebhookEndpointStatus = Schemas["WebhookEndpointStatus"];

export const webhookKeys = {
  endpoints: ["webhook-endpoints"] as const,
};

/**
 * Every endpoint, for the workflow action's endpoint picker. A caller with `workflows.manage` only gets
 * key, name and status (every other field null); without either permission the API answers 403.
 * The API serves at most MAX_PAGE; beyond that the picker still shows the action's own key.
 */
export function useWebhookEndpoints(enabled: MaybeRefOrGetter<boolean> = true) {
  return useQuery(() => ({
    queryKey: webhookKeys.endpoints,
    enabled: toValue(enabled),
    queryFn: ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/admin/webhook-endpoints", { params: { query: { limit: MAX_PAGE } }, signal })),
  }));
}
