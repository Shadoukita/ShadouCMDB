// Enterprise sign-in: the public list of sign-in options for the sign-in page, and
// Administration › Identity providers (OIDC providers and LDAP / AD directories).
// Same rules as admin.ts: every request goes through the typed client.
import { useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { toValue, type MaybeRefOrGetter } from "vue";
import { config } from "../config";
import { adminKeys } from "./admin";
import { api, unwrap, type JsonBody as Body, type Schemas } from "./client";

export type SignInOptions = Schemas["SignInOptions"];
export type SignInProvider = Schemas["SignInProvider"];
export type IdentityProvider = Schemas["IdentityProvider"];
export type ProviderKind = Schemas["ProviderKind"];
export type OidcConfig = Schemas["OidcConfig"];
export type LdapConfig = Schemas["LdapConfig"];
export type GroupMapping = Schemas["GroupMapping"];
export type ConnectionTest = Schemas["ConnectionTest"];
export type IdentityProviderCreateBody = Body<"/api/v1/admin/identity-providers", "post">;
export type IdentityProviderUpdateBody = Body<"/api/v1/admin/identity-providers/{id}", "patch">;

export const providerKeys = {
  signIn: ["auth", "providers"] as const,
  all: ["admin", "identity-providers"] as const,
  detail: (id: string) => ["admin", "identity-providers", "detail", id] as const,
};

export const KIND_LABELS: Record<ProviderKind, string> = { oidc: "OpenID Connect", ldap: "LDAP / Active Directory" };

/**
 * Where an OIDC button goes. A browser navigation, not a fetch: the API redirects to the
 * provider, and after it back to `returnTo` on this UI (or to /login?ssoError=…).
 */
export function oidcStartHref(startUrl: string, returnTo: string): string {
  const base = /^https?:\/\//.test(startUrl) ? startUrl : `${config.apiBaseUrl}${startUrl}`;
  return returnTo === "/" ? base : `${base}${base.includes("?") ? "&" : "?"}returnTo=${encodeURIComponent(returnTo)}`;
}

/** Public: OIDC buttons and whether a directory takes the username/password form too. */
export function useSignInOptions() {
  return useQuery({
    queryKey: providerKeys.signIn,
    queryFn: ({ signal }) => unwrap(api.GET("/api/v1/auth/providers", { signal })),
    staleTime: 60_000,
    retry: false,
  });
}

export function useIdentityProviders() {
  return useQuery({
    queryKey: providerKeys.all,
    queryFn: ({ signal }) => unwrap(api.GET("/api/v1/admin/identity-providers", { signal })),
  });
}

export function useIdentityProvider(id: MaybeRefOrGetter<string | undefined>) {
  return useQuery(() => {
    const providerId = toValue(id) ?? "";
    return {
      queryKey: providerKeys.detail(providerId),
      enabled: !!providerId,
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/admin/identity-providers/{id}", { params: { path: { id: providerId } }, signal })),
    };
  });
}

function useProviderMutation<V>(fn: (vars: V) => Promise<IdentityProvider | undefined>) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: fn,
    onSuccess: (provider) => {
      qc.invalidateQueries({ queryKey: providerKeys.all });
      qc.invalidateQueries({ queryKey: providerKeys.signIn });
      // Disabling a provider ends its accounts' sessions; mappings name profiles.
      qc.invalidateQueries({ queryKey: adminKeys.users });
      if (provider) qc.setQueryData(providerKeys.detail(provider.id), provider);
    },
  });
}

export const useCreateIdentityProvider = () =>
  useProviderMutation((body: IdentityProviderCreateBody) => unwrap(api.POST("/api/v1/admin/identity-providers", { body })));

export const useUpdateIdentityProvider = () =>
  useProviderMutation(({ id, body }: { id: string; body: IdentityProviderUpdateBody }) =>
    unwrap(api.PATCH("/api/v1/admin/identity-providers/{id}", { params: { path: { id } }, body })),
  );

/** 409 IN_USE while accounts still sign in through it: disable it instead. */
export const useDeleteIdentityProvider = () =>
  useProviderMutation(async (id: string) => {
    await unwrap(api.DELETE("/api/v1/admin/identity-providers/{id}", { params: { path: { id } } }));
    return undefined;
  });

/** Checks the saved settings without changing anything; for a directory, optionally looks a user up. */
export function useTestIdentityProvider() {
  return useMutation({
    mutationFn: ({ id, username }: { id: string; username?: string }) =>
      unwrap(api.POST("/api/v1/admin/identity-providers/{id}/test", { params: { path: { id } }, body: { username: username || null } })),
  });
}
