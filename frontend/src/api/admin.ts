// TanStack Query composables for sign-in and the Administration area (users,
// permission profiles, audit log). Same rules as queries.ts: every request goes
// through the typed client, and mutations invalidate exactly what they change.
import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { computed, toValue, type MaybeRefOrGetter } from "vue";
import { api, unwrap, type JsonBody as Body, type Schemas } from "./client";
import { groupKeys } from "./groups";
import { keys, MAX_PAGE, useCiClasses } from "./queries";
import type { paths } from "./schema";

export type Session = Schemas["Session"];
export type User = Schemas["User"];
export type PermissionProfile = Schemas["PermissionProfile"];
export type ClassPermission = Schemas["ClassPermission"];
export type EffectivePermissions = Schemas["EffectivePermissions"];
export type GlobalPermission = EffectivePermissions["global"][number];
export type ApiToken = Schemas["ApiToken"];
export type CreatedApiToken = Schemas["CreatedApiToken"];
export type SignInStatus = User["signInStatus"];
export type SignInAccount = Schemas["SignInAccount"];

export type SetupBody = Body<"/api/v1/setup", "post">;
export type LoginBody = Body<"/api/v1/auth/login", "post">;
export type UserCreateBody = Body<"/api/v1/admin/users", "post">;
export type UserUpdateBody = Body<"/api/v1/admin/users/{id}", "patch">;
export type ProfileCreateBody = Body<"/api/v1/admin/profiles", "post">;
export type ProfileUpdateBody = Body<"/api/v1/admin/profiles/{id}", "patch">;
export type UserListQuery = NonNullable<paths["/api/v1/admin/users"]["get"]["parameters"]["query"]>;
export type ProfileListQuery = NonNullable<paths["/api/v1/admin/profiles"]["get"]["parameters"]["query"]>;
export type ApiTokenCreateBody = Body<"/api/v1/admin/api-tokens", "post">;
export type ApiTokenListQuery = NonNullable<paths["/api/v1/admin/api-tokens"]["get"]["parameters"]["query"]>;
export type AuditListQuery = NonNullable<paths["/api/v1/audit-log"]["get"]["parameters"]["query"]>;

export const adminKeys = {
  users: ["admin", "users"] as const,
  userList: (q: UserListQuery) => ["admin", "users", "list", q] as const,
  user: (id: string) => ["admin", "users", "detail", id] as const,
  profiles: ["admin", "profiles"] as const,
  profileList: (q: ProfileListQuery) => ["admin", "profiles", "list", q] as const,
  profile: (id: string) => ["admin", "profiles", "detail", id] as const,
  signInAccount: (ciId: string) => ["admin", "users", "sign-in-account", ciId] as const,
  apiTokens: ["admin", "api-tokens"] as const,
  apiTokenList: (q: ApiTokenListQuery) => ["admin", "api-tokens", "list", q] as const,
  auditList: (q: AuditListQuery) => ["audit", "list", q] as const,
};

// ---------- Sign-in (called by the session store, not by components) ----------

export const authApi = {
  setupStatus: () => unwrap(api.GET("/api/v1/setup")),
  setup: (body: SetupBody) => unwrap(api.POST("/api/v1/setup", { body })),
  login: (body: LoginBody) => unwrap(api.POST("/api/v1/auth/login", { body })),
  loginMfa: (code: string) => unwrap(api.POST("/api/v1/auth/login/mfa", { body: { code } })),
  logout: () => unwrap(api.POST("/api/v1/auth/logout")),
  me: () => unwrap(api.GET("/api/v1/auth/me")),
  /** For an account created before e-mails were required: answers the session, now linked to its Person CI. */
  enterEmail: (email: string) => unwrap(api.PUT("/api/v1/auth/email", { body: { email } })),
};

/** Changes the signed-in user's own password; the API ends their other sessions and renews this one under new cookies. */
export function useChangeOwnPassword() {
  return useMutation({
    mutationFn: (body: { currentPassword: string; newPassword: string }) => unwrap(api.PUT("/api/v1/auth/password", { body })),
  });
}

// ---------- Users ----------

export function useUserList(query: MaybeRefOrGetter<UserListQuery>, enabled: MaybeRefOrGetter<boolean> = true) {
  return useQuery(() => {
    const q = toValue(query);
    return {
      queryKey: adminKeys.userList(q),
      enabled: toValue(enabled),
      queryFn: ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/admin/users", { params: { query: q }, signal })),
      placeholderData: keepPreviousData,
    };
  });
}

export function useUser(id: MaybeRefOrGetter<string | undefined>) {
  return useQuery(() => {
    const userId = toValue(id) ?? "";
    return {
      queryKey: adminKeys.user(userId),
      enabled: !!userId,
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/admin/users/{id}", { params: { path: { id: userId } }, signal })),
    };
  });
}

export function useCreateUser() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: UserCreateBody) => unwrap(api.POST("/api/v1/admin/users", { body })),
    onSuccess: (user) => {
      qc.invalidateQueries({ queryKey: adminKeys.users });
      qc.invalidateQueries({ queryKey: adminKeys.profiles }); // userCount
      qc.invalidateQueries({ queryKey: keys.cis }); // the Person CI it was linked to, created when there was none
      qc.setQueryData(adminKeys.user(user.id), user);
    },
  });
}

export function useUpdateUser() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, body }: { id: string; body: UserUpdateBody }) =>
      unwrap(api.PATCH("/api/v1/admin/users/{id}", { params: { path: { id } }, body })),
    onSuccess: (user) => {
      qc.invalidateQueries({ queryKey: adminKeys.users });
      qc.invalidateQueries({ queryKey: adminKeys.profiles });
      qc.invalidateQueries({ queryKey: groupKeys.all }); // members show the user's name and status
      qc.invalidateQueries({ queryKey: keys.cis }); // the linked Person's Email follows the account's
      qc.setQueryData(adminKeys.user(user.id), user);
    },
  });
}

export function useSetUserPassword() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, password }: { id: string; password: string }) =>
      unwrap(api.PUT("/api/v1/admin/users/{id}/password", { params: { path: { id } }, body: { password } })),
    onSuccess: (user) => qc.setQueryData(adminKeys.user(user.id), user),
  });
}

export function useDeleteUser() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => unwrap(api.DELETE("/api/v1/admin/users/{id}", { params: { path: { id } } })),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: adminKeys.users });
      qc.invalidateQueries({ queryKey: adminKeys.profiles });
      qc.invalidateQueries({ queryKey: groupKeys.all }); // memberCount
      qc.invalidateQueries({ queryKey: keys.cis }); // its Person stays, unlinked
    },
  });
}

/** Whether CIs of this class are Persons (the built-in class with `systemRole` person), which sign-in accounts link to. */
export function useIsPersonClass(classId: MaybeRefOrGetter<string | undefined>) {
  const classes = useCiClasses();
  return computed(() => {
    const id = toValue(classId);
    return !!id && classes.data.value?.find((c) => c.id === id)?.systemRole === "person";
  });
}

/**
 * The sign-in account linked to a Person CI (null when none is, or for a CI of another class). Kept under the
 * users key, so a change to an account (its e-mail, its status, a delete) refreshes the panel.
 */
export function useSignInAccount(ciId: MaybeRefOrGetter<string | undefined>, enabled: MaybeRefOrGetter<boolean> = true) {
  return useQuery(() => {
    const id = toValue(ciId) ?? "";
    return {
      queryKey: adminKeys.signInAccount(id),
      enabled: !!id && toValue(enabled),
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/configuration-items/{id}/sign-in-account", { params: { path: { id } }, signal })),
    };
  });
}

// ---------- Permission profiles ----------

export function useProfileList(query: MaybeRefOrGetter<ProfileListQuery>) {
  return useQuery(() => {
    const q = toValue(query);
    return {
      queryKey: adminKeys.profileList(q),
      queryFn: ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/admin/profiles", { params: { query: q }, signal })),
      placeholderData: keepPreviousData,
    };
  });
}

/** Every profile, for pickers (the API serves at most MAX_PAGE; beyond that the picker says so). */
export function useAllProfiles() {
  return useProfileList({ limit: MAX_PAGE, sort: "name" });
}

export function useProfile(id: MaybeRefOrGetter<string | undefined>) {
  return useQuery(() => {
    const profileId = toValue(id) ?? "";
    return {
      queryKey: adminKeys.profile(profileId),
      enabled: !!profileId,
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        unwrap(api.GET("/api/v1/admin/profiles/{id}", { params: { path: { id: profileId } }, signal })),
    };
  });
}

function useProfileMutation<V>(fn: (vars: V) => Promise<PermissionProfile | undefined>) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: fn,
    onSuccess: (profile) => {
      qc.invalidateQueries({ queryKey: adminKeys.profiles });
      // Users list their profiles by name, and holders' effective permissions change.
      qc.invalidateQueries({ queryKey: adminKeys.users });
      if (profile) qc.setQueryData(adminKeys.profile(profile.id), profile);
    },
  });
}

export const useCreateProfile = () =>
  useProfileMutation((body: ProfileCreateBody) => unwrap(api.POST("/api/v1/admin/profiles", { body })));

export const useUpdateProfile = () =>
  useProfileMutation(({ id, body }: { id: string; body: ProfileUpdateBody }) =>
    unwrap(api.PATCH("/api/v1/admin/profiles/{id}", { params: { path: { id } }, body })),
  );

export const useCloneProfile = () =>
  useProfileMutation(({ id, name }: { id: string; name: string }) =>
    unwrap(api.POST("/api/v1/admin/profiles/{id}/clone", { params: { path: { id } }, body: { name } })),
  );

export const useDeleteProfile = () =>
  useProfileMutation(async (id: string) => {
    await unwrap(api.DELETE("/api/v1/admin/profiles/{id}", { params: { path: { id } } }));
    return undefined;
  });

// ---------- API tokens ----------

export function useApiTokenList(query: MaybeRefOrGetter<ApiTokenListQuery>) {
  return useQuery(() => {
    const q = toValue(query);
    return {
      queryKey: adminKeys.apiTokenList(q),
      queryFn: ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/admin/api-tokens", { params: { query: q }, signal })),
      placeholderData: keepPreviousData,
    };
  });
}

/** Creates a token. The answer carries the secret once; it is handed to the caller and never put in the query cache. */
export function useCreateApiToken() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: ApiTokenCreateBody) => unwrap(api.POST("/api/v1/admin/api-tokens", { body })),
    onSuccess: () => qc.invalidateQueries({ queryKey: adminKeys.apiTokens }),
  });
}

/** Revokes a token. It stays listed, with status "revoked" and who revoked it when. */
export function useRevokeApiToken() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => unwrap(api.DELETE("/api/v1/admin/api-tokens/{id}", { params: { path: { id } } })),
    onSuccess: () => qc.invalidateQueries({ queryKey: adminKeys.apiTokens }),
  });
}

// ---------- Audit log ----------

export function useAuditList(query: MaybeRefOrGetter<AuditListQuery>) {
  return useQuery(() => {
    const q = toValue(query);
    return {
      queryKey: adminKeys.auditList(q),
      queryFn: ({ signal }: { signal: AbortSignal }) => unwrap(api.GET("/api/v1/audit-log", { params: { query: q }, signal })),
      placeholderData: keepPreviousData,
    };
  });
}
