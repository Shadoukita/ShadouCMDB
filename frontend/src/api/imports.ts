// TanStack Query composables for bulk import (/imports, /imports/:id and
// Administration › Import). Same rules as queries.ts: every request goes through
// the typed client, and mutations invalidate exactly what they change. The upload
// is the one raw-body request of the app: the file itself, not JSON or multipart,
// with its name in the X-File-Name header (never in the URL, which proxies log).
import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { toValue, type MaybeRefOrGetter } from "vue";
import { api, ApiError, unwrap, type Schemas } from "./client";
import { pollInterval, RUNNING } from "../lib/imports";
import type { paths } from "./schema";

export type ImportSettings = Schemas["ImportSettings"];
export type ImportLimits = Schemas["ImportLimits"];
export type ImportJob = Schemas["ImportJob"];
export type ImportJobSummary = Schemas["ImportJobSummary"];
export type ImportStatus = Schemas["ImportStatus"];
export type ImportPhase = Schemas["ImportPhase"];
export type ImportEncoding = Schemas["ImportEncoding"];
export type ImportFile = Schemas["ImportFile"];
export type ImportJobMapping = Schemas["ImportMapping"];
export type ImportColumnTarget = Schemas["ImportColumnTarget"];
export type ImportListQuery = NonNullable<paths["/api/v1/imports"]["get"]["parameters"]["query"]>;
export type ImportFileOptions = NonNullable<paths["/api/v1/imports/{id}/file-options"]["patch"]["requestBody"]>["content"]["application/json"];

/** The two content types the upload accepts; anything else is 415. */
export const IMPORT_TYPES = {
  csv: "text/csv",
  xlsx: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
} as const;

export const importKeys = {
  all: ["imports"] as const,
  settings: ["imports", "settings"] as const,
  lists: ["imports", "list"] as const,
  list: (q: ImportListQuery) => ["imports", "list", q] as const,
  job: (id: string) => ["imports", "job", id] as const,
};

// ---------- Settings ----------

/** Whether import is on, locked by the server configuration, and the upload limits. Any signed-in user may read it. */
export function useImportSettings(enabled: MaybeRefOrGetter<boolean> = true) {
  return useQuery(() => ({
    queryKey: importKeys.settings,
    enabled: toValue(enabled),
    queryFn: ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/imports/settings", { signal })),
    staleTime: 60_000,
  }));
}

/** Administrator only; `409 import_locked` while the server configuration forbids import. */
export function useUpdateImportSettings() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (enabled: boolean) => unwrap(api.PUT("/api/v1/imports/settings", { body: { enabled } })),
    onSuccess: (settings) => qc.setQueryData(importKeys.settings, settings),
  });
}

// ---------- Jobs ----------

export function useImportList(query: MaybeRefOrGetter<ImportListQuery>, enabled: MaybeRefOrGetter<boolean> = true) {
  return useQuery(() => {
    const q = toValue(query);
    return {
      queryKey: importKeys.list(q),
      enabled: toValue(enabled),
      queryFn: ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/imports", { params: { query: q }, signal })),
      placeholderData: keepPreviousData,
      // While a job of the list runs, keep its row current (slowly: the wizard is where progress is watched).
      refetchInterval: (query: { state: { data?: Schemas["ImportJobList"] } }) =>
        query.state.data?.data.some((j) => RUNNING.has(j.status)) ? 5_000 : false,
    };
  });
}

export function useImportJob(id: MaybeRefOrGetter<string | undefined>) {
  return useQuery(() => {
    const jobId = toValue(id) ?? "";
    return {
      queryKey: importKeys.job(jobId),
      enabled: !!jobId,
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/imports/{id}", { params: { path: { id: jobId } }, signal })),
      staleTime: 0,
      // A failed poll is not retried in place: the next interval is the retry, with backoff.
      retry: false,
      refetchInterval: (query: { state: { data?: ImportJob; error: unknown; fetchFailureCount: number } }) => {
        const e = query.state.error;
        // A 404 (expired and removed, or never ours) or 403 will not fix itself.
        if (e instanceof ApiError && e.status > 0 && e.status < 500) return false;
        return pollInterval(query.state.data, e ? query.state.fetchFailureCount : 0);
      },
      refetchIntervalInBackground: false,
    };
  });
}

/** Random, unique per upload attempt: a retried request after a dropped connection returns the same job. */
export function newIdempotencyKey(): string {
  return crypto.randomUUID();
}

/** Uploads a file (raw body). Resolves once the server has stored the last byte; analysis then runs in the background. */
export function useCreateImport() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ file, format, idempotencyKey }: { file: File; format: keyof typeof IMPORT_TYPES; idempotencyKey: string }) =>
      unwrap(
        api.POST("/api/v1/imports", {
          // The typed body is the file's text; the File itself is sent as-is, streamed by the browser.
          body: file as unknown as string,
          bodySerializer: (body: unknown) => body as BodyInit,
          headers: {
            "Content-Type": IMPORT_TYPES[format],
            "X-File-Name": encodeURIComponent(file.name),
            "Idempotency-Key": idempotencyKey,
          },
        }),
      ),
    onSuccess: (job) => {
      qc.setQueryData(importKeys.job(job.id), job);
      qc.invalidateQueries({ queryKey: importKeys.lists });
    },
  });
}

function useJobMutation<V>(fn: (vars: V) => Promise<ImportJob | undefined>) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: fn,
    onSuccess: (job) => {
      if (job) qc.setQueryData(importKeys.job(job.id), job);
      qc.invalidateQueries({ queryKey: importKeys.lists });
    },
  });
}

/** Sheet, encoding, delimiter or header row changed: the analysis runs again (`202`). */
export const useUpdateImportFileOptions = () =>
  useJobMutation(({ id, options }: { id: string; options: ImportFileOptions }) =>
    unwrap(api.PATCH("/api/v1/imports/{id}/file-options", { params: { path: { id } }, body: options })),
  );

/** Stops a running job; a commit stops after its current batch. Also works while import is turned off. */
export const useCancelImport = () =>
  useJobMutation((id: string) => unwrap(api.POST("/api/v1/imports/{id}/cancel", { params: { path: { id } } })));

/** Removes the job, its file and its row problems. Never touches CIs. Also works while import is turned off. */
export function useDeleteImport() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => unwrap(api.DELETE("/api/v1/imports/{id}", { params: { path: { id } } })),
    onSuccess: (_d, id) => {
      qc.removeQueries({ queryKey: importKeys.job(id) });
      qc.invalidateQueries({ queryKey: importKeys.lists });
    },
  });
}

// ---------- Downloads ----------

/** The file name from `Content-Disposition: attachment; filename="…"`, else the fallback. */
export function attachmentName(response: Response, fallback: string): string {
  const cd = response.headers.get("Content-Disposition") ?? "";
  const star = /filename\*=UTF-8''([^;]+)/i.exec(cd);
  if (star) {
    try {
      return decodeURIComponent(star[1]!);
    } catch {
      // fall through to the plain name
    }
  }
  return /filename="([^"]+)"/i.exec(cd)?.[1] ?? fallback;
}

function save(blob: Blob, name: string) {
  const a = document.createElement("a");
  a.href = URL.createObjectURL(blob);
  a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(a.href), 1000);
}

export const importDownloads = {
  /** The empty CSV template of a class (the server writes the header row, neutralised against formula injection). */
  async template(classKey: string) {
    const request = api.GET("/api/v1/imports/template", { params: { query: { classKey } }, parseAs: "blob" });
    const blob = (await unwrap(request)) as unknown as Blob;
    save(blob, attachmentName((await request).response, `${classKey}-template.csv`));
  },
};
