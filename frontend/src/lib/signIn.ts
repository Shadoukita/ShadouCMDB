/** Why the last OIDC sign-in did not go through; the server never says more than the code. */
export const SSO_ERRORS: Record<string, string> = {
  expired: "The sign-in took too long or was started in another browser tab. Start again.",
  cancelled: "The sign-in was cancelled at the identity provider.",
  failed: "The identity provider's answer could not be verified. Start again; if it keeps failing, ask an administrator to check the provider settings.",
  unavailable: "The identity provider could not be reached or is disabled. Try again later, or sign in with a local account.",
  not_configured: "Single sign-on is not fully set up on this server (PUBLIC_URL is missing). Ask an administrator.",
  not_authorised: "None of your groups gives access to ShadouCMDB. Ask an administrator for access.",
  account_conflict: "A ShadouCMDB account with your username already exists and does not belong to this identity provider. Ask an administrator to resolve the conflict.",
  account_disabled: "Your ShadouCMDB account is disabled. Ask an administrator.",
  invalid_username: "Your identity provider did not send a usable username. Ask an administrator to check the provider settings.",
  last_administrator: "Signing in would leave ShadouCMDB without an active administrator, because your groups no longer map to the Administrator profile. Ask another administrator to check the group mappings.",
  mfa_not_enforced: "Your identity provider did not confirm a second factor, which your access to ShadouCMDB requires. Sign in again using multi-factor authentication, or ask an administrator to check the provider's MFA settings.",
};

/** The shape of a server error code. Anything else in ?ssoError= came from a crafted link, not from us. */
const SSO_CODE = /^[a-z_]{1,32}$/;

/**
 * The message for ?ssoError=. A code this build does not know yet is still named, so an administrator can
 * look it up, but only when it looks like a code: free text in the link is never shown (GH#442).
 */
export function ssoErrorMessage(code: unknown): string | null {
  if (typeof code !== "string" || code === "") return null;
  if (Object.hasOwn(SSO_ERRORS, code)) return SSO_ERRORS[code];
  return SSO_CODE.test(code)
    ? `Single sign-on failed (${code}). Try again, or ask an administrator.`
    : "Single sign-on failed. Try again, or ask an administrator.";
}

/**
 * Only same-app paths are followed after sign-in (never another origin). Mirrors the server's
 * `safe_return_to`: no `//` or `/\` prefix, no backslash or control character anywhere (browsers read `\`
 * as `/` and drop tabs and newlines, so `/\evil.com` or `/<TAB>/evil.com` would leave the origin), and
 * at most 2048 characters. The value arrives decoded, so spaces and non-ASCII letters stay allowed.
 */
export function safeRedirect(value: unknown): string {
  if (typeof value !== "string" || !value.startsWith("/") || value.startsWith("//")) return "/";
  return value.length > 2048 || /[\\\u0000-\u001f\u007f]/.test(value) ? "/" : value;
}
