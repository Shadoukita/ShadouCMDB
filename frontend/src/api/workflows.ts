// TanStack Query composables for Administration › Workflows (`workflows.manage`): definitions, their
// draft graph, the lint, publishing, versions and transition grants. Same rules as admin.ts: every
// request goes through the typed client, and mutations invalidate exactly what they change.
import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { toValue, type MaybeRefOrGetter } from "vue";
import { ApiError, api, unwrap, type JsonBody as Body, type ListQuery, type Schemas } from "./client";
import { keys } from "./queries";
import { runtimeKeys } from "./workflowRuntime";

export type WorkflowDefinition = Schemas["WorkflowDefinition"];
/**
 * The spec gives `WorkflowWarning.code` as `allOf: [enum, object]` (a doc comment on the enum became an
 * object schema), which generates the impossible `"UNINSTANCED_CIS" & Record<string, never>`. The API sends
 * a plain string; the client types it as one until the spec is fixed.
 */
export type WorkflowWarning = Omit<Schemas["WorkflowWarning"], "code"> & { code: "UNINSTANCED_CIS" | (string & {}) };
export type WorkflowDefinitionDetail = Omit<Schemas["WorkflowDefinitionDetail"], "warnings"> & { warnings: WorkflowWarning[] };
export type WorkflowVersion = Schemas["WorkflowVersion"];
export type WorkflowVersionSummary = Schemas["WorkflowVersionSummary"];
export type WorkflowState = Schemas["WorkflowState"];
export type WorkflowTransition = Schemas["WorkflowTransition"];
export type WorkflowTransitionField = Schemas["WorkflowTransitionField"];
export type WorkflowValidation = Schemas["WorkflowValidation"];
export type WorkflowProblem = Schemas["WorkflowProblem"];
export type WorkflowGrants = Schemas["WorkflowGrants"];
export type WorkflowBootstrapResult = Schemas["WorkflowBootstrapResult"];
export type StateCategory = WorkflowState["category"];

export type WorkflowListQuery = ListQuery<"/api/v1/admin/workflow-definitions">;
export type WorkflowCreateBody = Body<"/api/v1/admin/workflow-definitions", "post">;
export type WorkflowUpdateBody = Body<"/api/v1/admin/workflow-definitions/{id}", "patch">;
export type WorkflowDraftBody = Body<"/api/v1/admin/workflow-definitions/{id}/draft", "put">;
export type WorkflowGrantsBody = Body<"/api/v1/admin/workflow-definitions/{id}/grants", "put">;

export const workflowKeys = {
  all: ["admin", "workflows"] as const,
  list: (q: WorkflowListQuery) => ["admin", "workflows", "list", q] as const,
  detail: (id: string) => ["admin", "workflows", "detail", id] as const,
  draft: (id: string) => ["admin", "workflows", "draft", id] as const,
  versions: (id: string) => ["admin", "workflows", "versions", id] as const,
  version: (id: string, no: number) => ["admin", "workflows", "version", id, no] as const,
  grants: (id: string) => ["admin", "workflows", "grants", id] as const,
};

const path = (id: string) => ({ params: { path: { id } } });

export function useWorkflowList(query: MaybeRefOrGetter<WorkflowListQuery>) {
  return useQuery(() => {
    const q = toValue(query);
    return {
      queryKey: workflowKeys.list(q),
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/admin/workflow-definitions", { params: { query: q }, signal })),
      placeholderData: keepPreviousData,
    };
  });
}

export function useWorkflow(id: MaybeRefOrGetter<string | undefined>) {
  return useQuery(() => {
    const wid = toValue(id) ?? "";
    return {
      queryKey: workflowKeys.detail(wid),
      enabled: !!wid,
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/admin/workflow-definitions/{id}", { ...path(wid), signal })) as Promise<WorkflowDefinitionDetail>,
    };
  });
}

/** The draft graph; `null` when the workflow has none (everything published). */
export function useWorkflowDraft(id: MaybeRefOrGetter<string | undefined>, enabled: MaybeRefOrGetter<boolean> = true) {
  return useQuery(() => {
    const wid = toValue(id) ?? "";
    return {
      queryKey: workflowKeys.draft(wid),
      enabled: !!wid && toValue(enabled),
      queryFn: async ({ signal }: { signal: AbortSignal }) => {
        try {
          return await unwrap(api.GET("/api/v1/admin/workflow-definitions/{id}/draft", { ...path(wid), signal }));
        } catch (e) {
          if (e instanceof ApiError && e.status === 404 && e.code === "NOT_FOUND") return null;
          throw e;
        }
      },
    };
  });
}

/** Versions, newest first; a workflow has few, so one page of 200 holds them. */
export function useWorkflowVersions(id: MaybeRefOrGetter<string | undefined>) {
  return useQuery(() => {
    const wid = toValue(id) ?? "";
    return {
      queryKey: workflowKeys.versions(wid),
      enabled: !!wid,
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/admin/workflow-definitions/{id}/versions", { params: { path: { id: wid }, query: { limit: 200 } }, signal })),
    };
  });
}

export function useWorkflowVersion(id: MaybeRefOrGetter<string | undefined>, no: MaybeRefOrGetter<number | null | undefined>) {
  return useQuery(() => {
    const wid = toValue(id) ?? "";
    const n = toValue(no) ?? 0;
    return {
      queryKey: workflowKeys.version(wid, n),
      enabled: !!wid && n > 0,
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/admin/workflow-definitions/{id}/versions/{no}", { params: { path: { id: wid, no: n } }, signal })),
    };
  });
}

export function useWorkflowGrants(id: MaybeRefOrGetter<string | undefined>) {
  return useQuery(() => {
    const wid = toValue(id) ?? "";
    return {
      queryKey: workflowKeys.grants(wid),
      enabled: !!wid,
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/admin/workflow-definitions/{id}/grants", { ...path(wid), signal })),
    };
  });
}

/** Changes to a definition move its `version` on: refresh the lists, and the detail from the answer. */
function useDefinitionMutation<V>(fn: (vars: V) => Promise<WorkflowDefinitionDetail>) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: fn,
    onSuccess: (wf) => {
      qc.invalidateQueries({ queryKey: [...workflowKeys.all, "list"] });
      qc.setQueryData(workflowKeys.detail(wf.id), wf);
    },
  });
}

export const useCreateWorkflow = () =>
  useDefinitionMutation((body: WorkflowCreateBody) => unwrap(api.POST("/api/v1/admin/workflow-definitions", { body })) as Promise<WorkflowDefinitionDetail>);

export const useUpdateWorkflow = () =>
  useDefinitionMutation(({ id, body }: { id: string; body: WorkflowUpdateBody }) =>
    unwrap(api.PATCH("/api/v1/admin/workflow-definitions/{id}", { ...path(id), body })) as Promise<WorkflowDefinitionDetail>,
  );

/**
 * Starts the workflow on the covered CIs that have no running instance, each in the state of its state field value.
 * A dry run only counts; a real run changes instances (and their CIs' audit), not the definition.
 */
export function useBootstrapWorkflow() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, dryRun }: { id: string; dryRun: boolean }) =>
      unwrap(api.POST("/api/v1/admin/workflow-definitions/{id}/bootstrap", { ...path(id), body: { stateFromAttribute: true, dryRun } })),
    onSuccess: (r) => {
      if (r.dryRun) return;
      void qc.invalidateQueries({ queryKey: runtimeKeys.all });
      void qc.invalidateQueries({ queryKey: keys.cis });
      void qc.invalidateQueries({ queryKey: ["audit"] });
    },
  });
}

export function useDeleteWorkflow() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => unwrap(api.DELETE("/api/v1/admin/workflow-definitions/{id}", path(id))),
    onSuccess: (_r, id) => {
      for (const key of [workflowKeys.detail(id), workflowKeys.draft(id), workflowKeys.versions(id), workflowKeys.grants(id)]) {
        qc.removeQueries({ queryKey: key });
      }
      qc.invalidateQueries({ queryKey: [...workflowKeys.all, "list"] });
    },
  });
}

/**
 * Draft writes. Saving does not change the definition's `version`, but it does change its
 * `draftVersionNo` and `draftChecksum` (and the versions list when it creates the draft), so the
 * detail and the versions are refetched; the draft cache takes the answer.
 */
function useDraftMutation<V extends { id: string }, R>(fn: (vars: V) => Promise<R>, onDone?: (qc: ReturnType<typeof useQueryClient>, vars: V, res: R) => void) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: fn,
    onSuccess: (res, vars) => {
      qc.invalidateQueries({ queryKey: workflowKeys.detail(vars.id) });
      qc.invalidateQueries({ queryKey: workflowKeys.versions(vars.id) });
      qc.invalidateQueries({ queryKey: [...workflowKeys.all, "list"] });
      onDone?.(qc, vars, res);
    },
  });
}

export const useSaveDraft = () =>
  useDraftMutation(
    ({ id, body }: { id: string; body: WorkflowDraftBody }) => unwrap(api.PUT("/api/v1/admin/workflow-definitions/{id}/draft", { ...path(id), body })),
    (qc, vars, draft) => qc.setQueryData(workflowKeys.draft(vars.id), draft),
  );

export const useDiscardDraft = () =>
  useDraftMutation(
    ({ id }: { id: string }) => unwrap(api.DELETE("/api/v1/admin/workflow-definitions/{id}/draft", path(id))),
    (qc, vars) => qc.setQueryData(workflowKeys.draft(vars.id), null),
  );

export const usePublishDraft = () =>
  useDraftMutation(
    ({ id, expectedDraftChecksum, changeNote }: { id: string; expectedDraftChecksum: string; changeNote: string | null }) =>
      unwrap(api.POST("/api/v1/admin/workflow-definitions/{id}/draft/publish", { ...path(id), body: { expectedDraftChecksum, changeNote } })),
    (qc, vars) => qc.setQueryData(workflowKeys.draft(vars.id), null),
  );

export const useRetireVersion = () =>
  useDraftMutation(({ id, no }: { id: string; no: number }) =>
    unwrap(api.POST("/api/v1/admin/workflow-definitions/{id}/versions/{no}/retire", { params: { path: { id, no } } })),
  );

/** The lint of the stored draft (not of unsaved edits). Always 200; `valid` says whether publishing would go ahead. */
export function validateDraft(id: string): Promise<WorkflowValidation> {
  return unwrap(api.POST("/api/v1/admin/workflow-definitions/{id}/draft/validate", path(id)));
}

export function useSaveGrants() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, body }: { id: string; body: WorkflowGrantsBody }) =>
      unwrap(api.PUT("/api/v1/admin/workflow-definitions/{id}/grants", { ...path(id), body })),
    onSuccess: (grants, vars) => {
      qc.setQueryData(workflowKeys.grants(vars.id), grants);
      // The grants moved the definition's version on.
      qc.invalidateQueries({ queryKey: workflowKeys.detail(vars.id) });
    },
  });
}
