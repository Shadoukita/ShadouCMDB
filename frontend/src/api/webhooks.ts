// TanStack Query composables for Administration › Webhooks and Outbound e-mail (SHAA-2725 §5, §6.1, §11.1).
// Endpoints and the allowlist need `webhooks.manage`; a `workflows.manage` holder lists endpoints by key,
// name and status only. A signing secret is in the create and rotate answers only: callers keep it in
// component state, never in the query cache.
import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { toValue, type MaybeRefOrGetter } from "vue";
import { api, unwrap, type JsonBody as Body, type ListQuery, type Schemas } from "./client";

export type WebhookEndpoint = Schemas["WebhookEndpoint"];
export type WebhookEndpointStatus = Schemas["WebhookEndpointStatus"];
export type WebhookPingResult = Schemas["WebhookPingResult"];
export type WebhookAllowedHost = Schemas["WebhookAllowedHost"];
export type MailStatus = Schemas["MailStatus"];
export type MailTestResult = Schemas["MailTestResult"];
export type WebhookEndpointCreateBody = Body<"/api/v1/admin/webhook-endpoints", "post">;
export type WebhookEndpointUpdateBody = Body<"/api/v1/admin/webhook-endpoints/{id}", "patch">;
export type WebhookAllowedHostBody = Body<"/api/v1/admin/webhook-allowed-hosts", "post">;
export type WebhookEndpointListQuery = ListQuery<"/api/v1/admin/webhook-endpoints">;

export const webhookKeys = {
  endpoints: ["admin", "webhook-endpoints"] as const,
  endpointList: (q: WebhookEndpointListQuery) => ["admin", "webhook-endpoints", "list", q] as const,
  allowedHosts: ["admin", "webhook-allowed-hosts"] as const,
  mailStatus: ["admin", "mail", "status"] as const,
};

const path = (id: string) => ({ params: { path: { id } } });

export function useWebhookEndpoints(query: MaybeRefOrGetter<WebhookEndpointListQuery>, enabled: MaybeRefOrGetter<boolean> = true) {
  return useQuery(() => {
    const q = toValue(query);
    return {
      queryKey: webhookKeys.endpointList(q),
      enabled: toValue(enabled),
      queryFn: ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/admin/webhook-endpoints", { params: { query: q }, signal })),
      placeholderData: keepPreviousData,
    };
  });
}

/** Invalidates endpoints, and the audit log every change writes to. */
function useInvalidateEndpoints() {
  const qc = useQueryClient();
  return () => {
    qc.invalidateQueries({ queryKey: webhookKeys.endpoints });
    qc.invalidateQueries({ queryKey: ["audit"] });
  };
}

/** Answers `{endpoint, secret}`: the secret is shown once and never readable again. */
export function useCreateWebhookEndpoint() {
  const done = useInvalidateEndpoints();
  return useMutation({
    mutationFn: (body: WebhookEndpointCreateBody) => unwrap(api.POST("/api/v1/admin/webhook-endpoints", { body })),
    onSuccess: done,
  });
}

export function useUpdateWebhookEndpoint() {
  const done = useInvalidateEndpoints();
  return useMutation({
    mutationFn: ({ id, body }: { id: string; body: WebhookEndpointUpdateBody }) =>
      unwrap(api.PATCH("/api/v1/admin/webhook-endpoints/{id}", { ...path(id), body })),
    onSuccess: done,
  });
}

export function useDeleteWebhookEndpoint() {
  const done = useInvalidateEndpoints();
  return useMutation({
    mutationFn: (id: string) => unwrap(api.DELETE("/api/v1/admin/webhook-endpoints/{id}", path(id))),
    onSuccess: done,
  });
}

/** Answers `{endpoint, secret}` with the new secret, shown once. */
export function useRotateWebhookSecret() {
  const done = useInvalidateEndpoints();
  return useMutation({
    mutationFn: ({ id, graceHours }: { id: string; graceHours: number }) =>
      unwrap(api.POST("/api/v1/admin/webhook-endpoints/{id}/rotate-secret", { ...path(id), body: { graceHours } })),
    onSuccess: done,
  });
}

/** Runs the URL and address checks and sends one signed `ping`. Nothing is stored. */
export function usePingWebhookEndpoint() {
  return useMutation({
    mutationFn: (id: string) => unwrap(api.POST("/api/v1/admin/webhook-endpoints/{id}/ping", path(id))),
  });
}

export function usePauseResumeWebhookEndpoint() {
  const done = useInvalidateEndpoints();
  return useMutation({
    mutationFn: ({ id, to }: { id: string; to: "pause" | "resume" }) =>
      to === "pause"
        ? unwrap(api.POST("/api/v1/admin/webhook-endpoints/{id}/pause", path(id)))
        : unwrap(api.POST("/api/v1/admin/webhook-endpoints/{id}/resume", path(id))),
    onSuccess: done,
  });
}

// ---------- Allowlist ----------

export function useWebhookAllowedHosts(enabled: MaybeRefOrGetter<boolean> = true) {
  return useQuery(() => ({
    queryKey: webhookKeys.allowedHosts,
    enabled: toValue(enabled),
    queryFn: ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/admin/webhook-allowed-hosts", { signal })),
  }));
}

export function useCreateWebhookAllowedHost() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: WebhookAllowedHostBody) => unwrap(api.POST("/api/v1/admin/webhook-allowed-hosts", { body })),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: webhookKeys.allowedHosts });
      qc.invalidateQueries({ queryKey: ["audit"] });
    },
  });
}

/** Answers the keys of the endpoints suspended because no remaining entry allows their URL. */
export function useDeleteWebhookAllowedHost() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => unwrap(api.DELETE("/api/v1/admin/webhook-allowed-hosts/{id}", path(id))),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: webhookKeys.allowedHosts });
      qc.invalidateQueries({ queryKey: webhookKeys.endpoints });
      qc.invalidateQueries({ queryKey: ["audit"] });
    },
  });
}

// ---------- Outbound e-mail ----------

export function useMailStatus() {
  return useQuery(() => ({
    queryKey: webhookKeys.mailStatus,
    queryFn: ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/admin/mail/status", { signal })),
  }));
}

/** Sends one message to the caller's own address; a refusal by the relay is in the answer, not an error. */
export function useSendMailTest() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: () => unwrap(api.POST("/api/v1/admin/mail/test")),
    onSettled: () => {
      qc.invalidateQueries({ queryKey: webhookKeys.mailStatus });
      qc.invalidateQueries({ queryKey: ["audit"] });
    },
  });
}
