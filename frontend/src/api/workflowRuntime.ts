// TanStack Query composables for running workflows on CIs (the runtime API, /workflow-instances and
// /configuration-items/{id}/workflows). Every signed-in user may call them: the API leaves out instances on CIs
// of types the caller may not view, and offers only the transitions they are granted. A step changes the CI
// (its fields, its state field) and its history, so mutations invalidate those too.
import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { toValue, type MaybeRefOrGetter } from "vue";
import { api, unwrap, type JsonBody as Body, type ListQuery, type Schemas } from "./client";
import { keys } from "./queries";

export type WorkflowInstance = Schemas["WorkflowInstance"];
export type WorkflowInstanceView = Schemas["WorkflowInstanceView"];
export type WorkflowInstanceDetail = Schemas["WorkflowInstanceDetail"];
export type WorkflowAvailableTransition = Schemas["WorkflowAvailableTransition"];
export type WorkflowTransitionFieldView = Schemas["WorkflowTransitionFieldView"];
export type WorkflowStateRef = Schemas["WorkflowStateRef"];
export type WorkflowStartable = Schemas["WorkflowStartable"];
export type WorkflowEvent = Schemas["WorkflowEvent"];
export type WorkflowStateCount = Schemas["WorkflowStateCount"];
export type WorkflowInstanceStatus = WorkflowInstance["status"];

export type WorkflowInstanceListQuery = ListQuery<"/api/v1/workflow-instances">;
export type WorkflowTransitionBody = Body<"/api/v1/workflow-instances/{id}/transitions", "post">;

export const runtimeKeys = {
  all: ["workflow-runtime"] as const,
  ci: (ciId: string) => ["workflow-runtime", "ci", ciId] as const,
  list: (q: WorkflowInstanceListQuery) => ["workflow-runtime", "list", q] as const,
  summary: (definitionKey?: string) => ["workflow-runtime", "summary", definitionKey ?? ""] as const,
  instance: (id: string) => ["workflow-runtime", "instance", id] as const,
  events: (id: string, limit: number, offset: number) => ["workflow-runtime", "events", id, limit, offset] as const,
};

const path = (id: string) => ({ params: { path: { id } } });

/** A CI's running instances (then the 20 that ended last) and the workflows the caller may start on it. */
export function useCiWorkflows(ciId: MaybeRefOrGetter<string | undefined>) {
  return useQuery(() => {
    const id = toValue(ciId) ?? "";
    return {
      queryKey: runtimeKeys.ci(id),
      enabled: !!id,
      queryFn: ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/configuration-items/{id}/workflows", { ...path(id), signal })),
    };
  });
}

export function useWorkflowInstances(query: MaybeRefOrGetter<WorkflowInstanceListQuery>) {
  return useQuery(() => {
    const q = toValue(query);
    return {
      queryKey: runtimeKeys.list(q),
      queryFn: ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/workflow-instances", { params: { query: q }, signal })),
      placeholderData: keepPreviousData,
    };
  });
}

/** Running instances per workflow and state. */
export function useWorkflowSummary(definitionKey: MaybeRefOrGetter<string | undefined>) {
  return useQuery(() => {
    const key = toValue(definitionKey) || undefined;
    return {
      queryKey: runtimeKeys.summary(key),
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/workflow-instances/summary", { params: { query: { definitionKey: key } }, signal })),
    };
  });
}

export function useWorkflowInstance(id: MaybeRefOrGetter<string | undefined>) {
  return useQuery(() => {
    const iid = toValue(id) ?? "";
    return {
      queryKey: runtimeKeys.instance(iid),
      enabled: !!iid,
      queryFn: ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/workflow-instances/{id}", { ...path(iid), signal })),
    };
  });
}

export function useWorkflowEvents(id: MaybeRefOrGetter<string | undefined>, limit: MaybeRefOrGetter<number>, offset: MaybeRefOrGetter<number>) {
  return useQuery(() => {
    const iid = toValue(id) ?? "";
    const l = toValue(limit);
    const o = toValue(offset);
    return {
      queryKey: runtimeKeys.events(iid, l, o),
      enabled: !!iid,
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/workflow-instances/{id}/events", { params: { path: { id: iid }, query: { limit: l, offset: o } }, signal })),
      placeholderData: keepPreviousData,
    };
  });
}

/** After a step: the instance lists and counts, the CI (fields, state field, version) and its history. */
function useInvalidateStep() {
  const qc = useQueryClient();
  return (ciId: string) => {
    void qc.invalidateQueries({ queryKey: runtimeKeys.all });
    void qc.invalidateQueries({ queryKey: keys.ci(ciId) });
    void qc.invalidateQueries({ queryKey: keys.audit(ciId) });
    void qc.invalidateQueries({ queryKey: keys.cis });
  };
}

export function useStartWorkflow() {
  const done = useInvalidateStep();
  return useMutation({
    mutationFn: (b: { ciId: string; definitionId: string; comment?: string }) =>
      unwrap(api.POST("/api/v1/workflow-instances", { body: { ciId: b.ciId, definitionId: b.definitionId, comment: b.comment || undefined } })),
    onSuccess: (_d, b) => done(b.ciId),
  });
}

export function useRunTransition() {
  const done = useInvalidateStep();
  return useMutation({
    mutationFn: (v: { id: string; ciId: string; body: WorkflowTransitionBody }) =>
      unwrap(api.POST("/api/v1/workflow-instances/{id}/transitions", { ...path(v.id), body: v.body })),
    onSuccess: (_d, v) => done(v.ciId),
  });
}

export function useCancelWorkflow() {
  const done = useInvalidateStep();
  return useMutation({
    mutationFn: (v: { id: string; ciId: string; expectedVersion: number; reason: string }) =>
      unwrap(api.POST("/api/v1/workflow-instances/{id}/cancel", { ...path(v.id), body: { expectedVersion: v.expectedVersion, reason: v.reason } })),
    onSuccess: (_d, v) => done(v.ciId),
  });
}

export function useForceWorkflowState() {
  const done = useInvalidateStep();
  return useMutation({
    mutationFn: (v: { id: string; ciId: string; expectedVersion: number; stateKey: string; reason: string }) =>
      unwrap(
        api.POST("/api/v1/workflow-instances/{id}/force", {
          ...path(v.id),
          body: { expectedVersion: v.expectedVersion, stateKey: v.stateKey, reason: v.reason },
        }),
      ),
    onSuccess: (_d, v) => done(v.ciId),
  });
}

/** Labels and badge tones of the instance statuses and state categories. */
export const STATUS_LABELS: Record<WorkflowInstanceStatus, string> = { active: "Running", completed: "Completed", cancelled: "Cancelled" };
export const STATUS_TONES: Record<WorkflowInstanceStatus, string> = { active: "info", completed: "ok", cancelled: "off" };
export const CATEGORY_TONES: Record<WorkflowStateRef["category"], string> = { open: "", active: "info", done: "ok", cancelled: "off" };
export const EVENT_LABELS: Record<WorkflowEvent["kind"], string> = {
  start: "Started",
  transition: "Transition",
  cancel: "Cancelled",
  migrate: "Moved to a new version",
  force: "State forced",
};
