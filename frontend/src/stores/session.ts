import { defineStore } from "pinia";
import { computed, ref } from "vue";
import { authApi, type GlobalPermission, type LoginBody, type Session, type SetupBody } from "../api/admin";
import { ApiError, setCsrfToken } from "../api/client";
import { queryClient } from "../api/queryClient";
import { canAnyClass, canClass, hasGlobal, type ClassRight } from "../lib/permissions";

/**
 * Who is signed in and what they may do. `status` drives the router guard:
 *  - "setup":     no user exists yet → first-run screen
 *  - "anonymous": sign-in screen
 *  - "signedIn":  the application
 * Permissions come from GET /auth/me and only hide UI; the API enforces them.
 */
export type SessionStatus = "unknown" | "setup" | "anonymous" | "signedIn";

export const useSessionStore = defineStore("session", () => {
  const status = ref<SessionStatus>("unknown");
  const session = ref<Session | null>(null);
  /** Set when the API ended the session under us, so the sign-in screen can say why. */
  const expired = ref(false);
  /** The API could not be asked who is signed in (unreachable, database down). */
  const bootError = ref<unknown>(null);
  let loading: Promise<void> | null = null;

  const user = computed(() => session.value?.user ?? null);
  const permissions = computed(() => session.value?.permissions);
  /** A profile the user holds requires two-factor authentication and they have not set it up: only the set-up screen works. */
  const enrolmentRequired = computed(() => !!session.value?.mfa.enrolmentRequired);

  /** A different user may sign in next: never show them the previous user's cached data. */
  function signIn(s: Session) {
    queryClient.clear();
    apply(s);
  }

  function apply(s: Session) {
    session.value = s;
    setCsrfToken(s.csrfToken);
    status.value = "signedIn";
    expired.value = false;
  }

  function clear(next: SessionStatus) {
    session.value = null;
    setCsrfToken(undefined);
    status.value = next;
  }

  /** Resolves the status once per page load. If the API cannot answer, `bootError` is set and the status stays "unknown". */
  function ensureLoaded(): Promise<void> {
    if (status.value !== "unknown") return Promise.resolve();
    loading ??= (async () => {
      bootError.value = null;
      try {
        try {
          apply(await authApi.me());
        } catch (e) {
          if (!(e instanceof ApiError) || e.status !== 401) throw e;
          const setup = await authApi.setupStatus();
          clear(setup.setupRequired ? "setup" : "anonymous");
        }
      } catch (e) {
        bootError.value = e;
      }
    })().finally(() => (loading = null));
    return loading;
  }

  /**
   * Signs in with the password. "mfa" means the password was right and the API
   * now wants a code from the user's authenticator (it set the challenge cookie).
   */
  async function login(body: LoginBody): Promise<"signedIn" | "mfa"> {
    try {
      signIn(await authApi.login(body));
      return "signedIn";
    } catch (e) {
      if (e instanceof ApiError && e.code === "MFA_REQUIRED") return "mfa";
      throw e;
    }
  }

  /** The second sign-in step: an authenticator code or a recovery code. */
  async function loginMfa(code: string) {
    signIn(await authApi.loginMfa(code));
  }

  async function setup(body: SetupBody) {
    signIn(await authApi.setup(body));
  }

  async function logout() {
    try {
      await authApi.logout();
    } catch (e) {
      // Already signed out on the server is fine; anything else is worth showing.
      if (!(e instanceof ApiError) || e.status !== 401) throw e;
    }
    expired.value = false;
    clear("anonymous");
    queryClient.clear();
  }

  /** The API answered 401 to a signed-in request: the session expired or was ended elsewhere. */
  function markExpired() {
    if (status.value !== "signedIn") return;
    expired.value = true;
    clear("anonymous");
  }

  /** Re-reads permissions (e.g. after editing a profile the current user holds). */
  async function refresh() {
    try {
      apply(await authApi.me());
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) markExpired();
      else throw e;
    }
  }

  const can = (perm: GlobalPermission) => hasGlobal(permissions.value, perm);
  const canOnClass = (classId: string | undefined, right: ClassRight) => canClass(permissions.value, classId, right);
  const canOnAnyClass = (right: ClassRight) => canAnyClass(permissions.value, right);
  /** Holds the built-in Administrator profile (some screens need exactly that, not a permission). */
  const isAdministrator = computed(() => !!user.value?.isAdministrator);
  /** For the Administration sections: what they need is a permission, or the Administrator profile. */
  const adminAccess = computed(() => ({ can, isAdministrator: isAdministrator.value }));

  return {
    status,
    session,
    user,
    permissions,
    enrolmentRequired,
    expired,
    bootError,
    ensureLoaded,
    login,
    loginMfa,
    setup,
    logout,
    markExpired,
    refresh,
    can,
    isAdministrator,
    adminAccess,
    canOnClass,
    canOnAnyClass,
  };
});
