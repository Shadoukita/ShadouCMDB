// TanStack Query composables for the approvals inbox (GET /workflow-approval-requests) and approval delegations,
// your own (/me/approval-delegations) and every user's (/admin/approval-delegations, users.manage). The inbox
// keys sit under `runtimeKeys.all`, so a decision, a withdrawal or a workflow step refreshes them with the
// navigation count (`awaitingMyDecision`, the actionable inbox's total).
import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { toValue, type MaybeRefOrGetter } from "vue";
import { api, unwrap, type JsonBody as Body, type ListQuery, type Schemas } from "./client";
import { runtimeKeys } from "./workflowRuntime";

export type ApprovalInboxQuery = ListQuery<"/api/v1/workflow-approval-requests">;
export type ApprovalInboxView = NonNullable<ApprovalInboxQuery["view"]>;
export type ApprovalDelegation = Schemas["WorkflowApprovalDelegation"];
export type ApprovalDelegationStatus = ApprovalDelegation["status"];
export type ApprovalDelegationUser = Schemas["WorkflowApprovalDelegationUser"];
export type MyDelegationQuery = ListQuery<"/api/v1/me/approval-delegations">;
export type AdminDelegationQuery = ListQuery<"/api/v1/admin/approval-delegations">;
export type MyDelegationBody = Body<"/api/v1/me/approval-delegations", "post">;
export type AdminDelegationBody = Body<"/api/v1/admin/approval-delegations", "post">;

export const approvalKeys = {
  inbox: (q: ApprovalInboxQuery) => [...runtimeKeys.all, "inbox", q] as const,
  delegations: ["approval-delegations"] as const,
  mine: (q: MyDelegationQuery) => ["approval-delegations", "me", q] as const,
  admin: (q: AdminDelegationQuery) => ["approval-delegations", "admin", q] as const,
};

/** One view of the inbox: actionable (decide now), requested (yours), decided (you decided a step of), all. */
export function useApprovalInbox(query: MaybeRefOrGetter<ApprovalInboxQuery>) {
  return useQuery(() => {
    const q = toValue(query);
    return {
      queryKey: approvalKeys.inbox(q),
      queryFn: ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/workflow-approval-requests", { params: { query: q }, signal })),
      placeholderData: keepPreviousData,
    };
  });
}

export function useMyDelegations(query: MaybeRefOrGetter<MyDelegationQuery>, enabled: MaybeRefOrGetter<boolean> = true) {
  return useQuery(() => {
    const q = toValue(query);
    return {
      queryKey: approvalKeys.mine(q),
      enabled: toValue(enabled),
      queryFn: ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/me/approval-delegations", { params: { query: q }, signal })),
      placeholderData: keepPreviousData,
    };
  });
}

export function useAdminDelegations(query: MaybeRefOrGetter<AdminDelegationQuery>, enabled: MaybeRefOrGetter<boolean> = true) {
  return useQuery(() => {
    const q = toValue(query);
    return {
      queryKey: approvalKeys.admin(q),
      enabled: toValue(enabled),
      queryFn: ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/admin/approval-delegations", { params: { query: q }, signal })),
      placeholderData: keepPreviousData,
    };
  });
}

/** A new or revoked delegation changes who may decide what: the delegation lists and the inbox (its count too). */
function useInvalidateDelegations() {
  const qc = useQueryClient();
  return () => {
    void qc.invalidateQueries({ queryKey: approvalKeys.delegations });
    void qc.invalidateQueries({ queryKey: runtimeKeys.all });
  };
}

/** Creates a delegation: your own (`admin` false) or, with users.manage, someone else's. */
export function useCreateDelegation(admin: boolean) {
  const done = useInvalidateDelegations();
  return useMutation({
    mutationFn: (body: MyDelegationBody | AdminDelegationBody) =>
      admin
        ? unwrap(api.POST("/api/v1/admin/approval-delegations", { body: body as AdminDelegationBody }))
        : unwrap(api.POST("/api/v1/me/approval-delegations", { body: body as MyDelegationBody })),
    onSuccess: done,
  });
}

/** Revokes a delegation; it stays listed as revoked, and decisions already made through it stand. */
export function useRevokeDelegation(admin: boolean) {
  const done = useInvalidateDelegations();
  return useMutation({
    mutationFn: (id: string) =>
      admin
        ? unwrap(api.POST("/api/v1/admin/approval-delegations/{id}/revoke", { params: { path: { id } } }))
        : unwrap(api.POST("/api/v1/me/approval-delegations/{id}/revoke", { params: { path: { id } } })),
    onSuccess: done,
  });
}

export const DELEGATION_STATUS_TONES: Record<ApprovalDelegationStatus, string> = { scheduled: "info", active: "ok", ended: "off", revoked: "off" };
