// Two-factor authentication: the signed-in user's own authenticator (TOTP) and
// recovery codes, and a user manager's reset. Secrets and recovery codes are
// handed to the caller and never put in the query cache.
import { useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { useSessionStore } from "../stores/session";
import { adminKeys, type User } from "./admin";
import { api, unwrap, type Schemas } from "./client";

export type MfaStatus = Schemas["MfaStatus"];
export type TotpEnrolment = Schemas["TotpEnrolment"];

export const mfaKeys = {
  status: ["account", "mfa"] as const,
};

export function useMfaStatus() {
  return useQuery({
    queryKey: mfaKeys.status,
    queryFn: ({ signal }) => unwrap(api.GET("/api/v1/auth/mfa", { signal })),
  });
}

/** After a change: the status here, and the session's copy that drives forced enrolment. */
function useAfterChange() {
  const qc = useQueryClient();
  const session = useSessionStore();
  return async () => {
    await qc.invalidateQueries({ queryKey: mfaKeys.status });
    await session.refresh();
  };
}

/** Starts (or restarts) setting up an authenticator: a new secret, not active until confirmed. */
export function useStartTotp() {
  return useMutation({
    mutationFn: (currentPassword: string) => unwrap(api.POST("/api/v1/auth/mfa/totp", { body: { currentPassword } })),
  });
}

/** Confirms the authenticator with a code from it; answers the 10 recovery codes, shown once. */
export function useConfirmTotp() {
  const after = useAfterChange();
  return useMutation({
    mutationFn: (code: string) => unwrap(api.POST("/api/v1/auth/mfa/totp/confirm", { body: { code } })),
    onSuccess: after,
  });
}

export function useDisableTotp() {
  const after = useAfterChange();
  return useMutation({
    mutationFn: (body: { currentPassword: string; code: string }) => unwrap(api.DELETE("/api/v1/auth/mfa/totp", { body })),
    onSuccess: after,
  });
}

/** Replaces the recovery codes; the old ones stop working. */
export function useRegenerateRecoveryCodes() {
  const after = useAfterChange();
  return useMutation({
    mutationFn: (body: { currentPassword: string; code: string }) => unwrap(api.POST("/api/v1/auth/mfa/recovery-codes", { body })),
    onSuccess: after,
  });
}

/** A user manager turns off another user's two-factor authentication (lost device and recovery codes). */
export function useResetUserMfa() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async (user: User) => {
      await unwrap(api.DELETE("/api/v1/admin/users/{id}/mfa", { params: { path: { id: user.id } } }));
      return user;
    },
    onSuccess: (user) => {
      qc.invalidateQueries({ queryKey: adminKeys.users });
      qc.setQueryData(adminKeys.user(user.id), { ...user, mfaEnabled: false });
    },
  });
}

/**
 * Confirms the password (and a code once MFA is set up) again: for 10 minutes the session may change accounts,
 * profiles, API tokens and identity providers (GH#498).
 */
export function useReauthenticate() {
  return useMutation({
    mutationFn: (body: { currentPassword: string; code?: string }) => unwrap(api.POST("/api/v1/auth/reauthenticate", { body })),
  });
}

/** Authenticator codes are 6 digits; recovery codes ignore case, dashes and spaces. Strip what users paste around them. */
export const normaliseCode = (code: string) => code.trim().replace(/\s+/g, "");
