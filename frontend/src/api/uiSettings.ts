// TanStack Query composables for UI settings (Administration › Customization)
// and configuration export/import. Same rules as queries.ts: every request goes
// through the typed client, and mutations invalidate exactly what they change.
import { keepPreviousData, useMutation, useQuery, useQueryClient, type QueryClient } from "@tanstack/vue-query";
import { toValue, type MaybeRefOrGetter } from "vue";
import { api, unwrap, type JsonBody, type Schemas } from "./client";

export type UiSettings = Schemas["UiSettings"];
export type UiBranding = Schemas["UiBranding"];
export type UiNavEntry = Schemas["UiNavEntry"];
export type UiNavClassItem = Schemas["UiNavClassItem"];
export type UiListView = Schemas["UiListView"];
export type UiClassLayout = Schemas["UiClassLayout"];
export type UiLayoutTab = Schemas["UiLayoutTab"];
export type UiLayoutSection = Schemas["UiLayoutSection"];
export type UiLayoutField = Schemas["UiLayoutField"];
export type UiLayout = Schemas["UiLayout"];
export type UiLayoutTemplate = Schemas["UiLayoutTemplate"];
export type CiLayout = Schemas["CiLayout"];
export type LayoutTemplateUsages = Schemas["LayoutTemplateUsages"];
export type UiListSort = NonNullable<UiListView["defaultSort"]>;
export type UiListFilters = NonNullable<UiListView["defaultFilters"]>;
export type UiPage = NonNullable<UiNavEntry["page"]>;
// The generator types the saved search's inline `filters` as `{…} & Record<string, never>`, which no
// object literal satisfies; these restate the widget and document with the plain UiListFilters shape.
type RawWidget = Schemas["UiWidget"];
export type UiSavedSearch = Omit<NonNullable<RawWidget["search"]>, "filters"> & { filters?: UiListFilters };
export type UiWidget = Omit<RawWidget, "search"> & { search?: UiSavedSearch };
export type UiSettingsDocument = Omit<Schemas["UiSettingsDocument"], "dashboard"> & { dashboard: { widgets?: UiWidget[] | null } };
export type UiWidgetType = UiWidget["type"];
export type UiWidgetSize = NonNullable<UiWidget["size"]>;
export type UiTheme = NonNullable<UiBranding["defaultTheme"]>;
export type PublicBranding = Schemas["PublicBranding"];
export type UiAsset = Schemas["UiAsset"];
export type UiSettingsIssue = Schemas["Issue"];
export type UiSettingsVersionSummary = Schemas["UiSettingsVersionSummary"];
export type AssetKind = UiAsset["kind"];
export type ImageType = Schemas["AssetData"]["contentType"];
export type ConfigFile = Schemas["ConfigFile"];
export type ImportResult = Schemas["ImportResult"];
export type ImportMode = ImportResult["mode"];

export const uiKeys = {
  all: ["ui-settings"] as const,
  settings: ["ui-settings", "current"] as const,
  branding: ["ui-settings", "branding"] as const,
  versions: (limit: number, offset: number) => ["ui-settings", "versions", limit, offset] as const,
  version: (n: number) => ["ui-settings", "version", n] as const,
  // Under "ui-settings": every settings save can change them (a template's layout, a class's default).
  templateUsage: ["ui-settings", "layout-template-usage"] as const,
  ciLayout: (id: string) => ["ui-settings", "ci-layout", id] as const,
};

const fetchSettings = ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/ui-settings", { signal }));

/** The effective settings every signed-in screen applies. */
export function useUiSettings(enabled: MaybeRefOrGetter<boolean> = true) {
  return useQuery(() => ({
    queryKey: uiKeys.settings,
    enabled: toValue(enabled),
    queryFn: fetchSettings,
    staleTime: 60_000,
  }));
}

/** Branding without a session (login page, favicon, title). */
export const fetchPublicBranding = () => unwrap(api.GET("/api/v1/ui-settings/branding"));

const versionQuery = (n: number) => ({
  queryKey: uiKeys.version(n),
  queryFn: ({ signal }: { signal: AbortSignal }) =>
    unwrap(api.GET("/api/v1/ui-settings/versions/{version}", { params: { path: { version: n } }, signal })),
  staleTime: Infinity, // a saved version never changes
});

/** A saved version as stored (with references the effective settings leave out). */
export function useUiSettingsVersion(version: MaybeRefOrGetter<number | undefined>) {
  return useQuery(() => {
    const n = toValue(version) ?? 0;
    return { ...versionQuery(n), enabled: n > 0 };
  });
}

/**
 * The current settings and the stored document of their version, fresh from the
 * API: what an editor starts from (the in-page layout editor).
 */
export async function fetchCurrentStoredSettings(qc: QueryClient) {
  const current = await qc.fetchQuery({ queryKey: uiKeys.settings, queryFn: fetchSettings, staleTime: 0 });
  return qc.fetchQuery(versionQuery(current.version));
}

export function useUiSettingsVersions(page: MaybeRefOrGetter<{ limit: number; offset: number }>) {
  return useQuery(() => {
    const { limit, offset } = toValue(page);
    return {
      queryKey: uiKeys.versions(limit, offset),
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/ui-settings/versions", { params: { query: { limit, offset } }, signal })),
      placeholderData: keepPreviousData,
    };
  });
}

function useUiMutation<V, R>(fn: (vars: V) => Promise<R>) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: fn,
    onSuccess: () => qc.invalidateQueries({ queryKey: uiKeys.all }),
  });
}

export const useSaveUiSettings = () =>
  useUiMutation((body: { version: number; settings: UiSettingsDocument; comment?: string | null }) =>
    unwrap(api.PUT("/api/v1/ui-settings", { body: body as JsonBody<"/api/v1/ui-settings", "put"> })),
  );

export const useRestoreUiSettings = () =>
  useUiMutation(({ restore, version, comment }: { restore: number; version: number; comment?: string | null }) =>
    unwrap(api.POST("/api/v1/ui-settings/versions/{version}/restore", { params: { path: { version: restore } }, body: { version, comment } })),
  );

export const useUploadAsset = () =>
  useUiMutation(({ kind, contentType, data }: { kind: AssetKind; contentType: ImageType; data: string }) =>
    unwrap(api.PUT("/api/v1/ui-settings/assets/{kind}", { params: { path: { kind } }, body: { contentType, data } })),
  );

export const useDeleteAsset = () =>
  useUiMutation((kind: AssetKind) => unwrap(api.DELETE("/api/v1/ui-settings/assets/{kind}", { params: { path: { kind } } })));

// ---------- Layout templates ----------

/** Which template each class uses, and who uses each template (customization.manage). */
export function useLayoutTemplateUsage(enabled: MaybeRefOrGetter<boolean> = true) {
  return useQuery(() => ({
    queryKey: uiKeys.templateUsage,
    enabled: toValue(enabled),
    queryFn: ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/ui-settings/layout-templates/usage", { signal })),
  }));
}

const ciLayoutQuery = (id: string) => ({
  queryKey: uiKeys.ciLayout(id),
  queryFn: ({ signal }: { signal: AbortSignal }) =>
    unwrap(api.GET("/api/v1/configuration-items/{id}/layout", { params: { path: { id } }, signal })),
  staleTime: 60_000,
});

/** The layout a CI's detail page and form show, and where it comes from (its own, another template, the class's default). */
export function useCiLayout(id: MaybeRefOrGetter<string | undefined>) {
  return useQuery(() => {
    const ci = toValue(id) ?? "";
    return { ...ciLayoutQuery(ci), enabled: !!ci };
  });
}

/** The CI's layout fresh from the API: what the layout editor starts from. */
export const fetchCiLayout = (qc: QueryClient, id: string) => qc.fetchQuery({ ...ciLayoutQuery(id), staleTime: 0 });

export type CiLayoutUpdate = { templateKey: string; layout?: never; version?: number } | { layout: UiLayout; templateKey?: never; version?: number };

export const layoutApi = {
  /** Another template (`templateKey`) or a layout of its own (`layout`) for the CI; `version` is the one loaded, if it had its own. */
  setCiLayout: (id: string, body: CiLayoutUpdate) =>
    unwrap(api.PUT("/api/v1/configuration-items/{id}/layout", { params: { path: { id } }, body: body as JsonBody<"/api/v1/configuration-items/{id}/layout", "put"> })),
  /** Back to the class's default template. */
  resetCiLayout: (id: string) => unwrap(api.DELETE("/api/v1/configuration-items/{id}/layout", { params: { path: { id } } })),
};

// ---------- Configuration export/import ----------

export const configApi = {
  export: () => unwrap(api.GET("/api/v1/admin/config/export")),
  /** `file` is whatever the operator uploaded; the API validates it and reports every problem with a path. */
  import: (file: unknown, mode: ImportMode) =>
    unwrap(api.POST("/api/v1/admin/config/import", { params: { query: { mode } }, body: file as ConfigFile })),
};

export function useImportConfig() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ file, mode }: { file: unknown; mode: ImportMode }) => configApi.import(file, mode),
    // An applied import can touch anything the UI caches (classes, lookups, profiles, settings).
    onSuccess: (result) => {
      if (result.applied) qc.invalidateQueries();
    },
  });
}
