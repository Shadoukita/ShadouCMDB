import { t, type MessageKey } from "../i18n/index";

/** Why the last OIDC sign-in did not go through; the server never says more than the code. */
export const SSO_ERRORS: Record<string, MessageKey> = {
  expired: "auth.sso.expired",
  cancelled: "auth.sso.cancelled",
  failed: "auth.sso.failed",
  unavailable: "auth.sso.unavailable",
  not_configured: "auth.sso.not_configured",
  not_authorised: "auth.sso.not_authorised",
  account_conflict: "auth.sso.account_conflict",
  account_disabled: "auth.sso.account_disabled",
  invalid_username: "auth.sso.invalid_username",
  last_administrator: "auth.sso.last_administrator",
  mfa_not_enforced: "auth.sso.mfa_not_enforced",
};

/** The shape of a server error code. Anything else in ?ssoError= came from a crafted link, not from us. */
const SSO_CODE = /^[a-z_]{1,32}$/;

/**
 * The message for ?ssoError=. A code this build does not know yet is still named, so an administrator can
 * look it up, but only when it looks like a code: free text in the link is never shown (GH#442).
 */
export function ssoErrorMessage(code: unknown): string | null {
  if (typeof code !== "string" || code === "") return null;
  if (Object.hasOwn(SSO_ERRORS, code)) return t(SSO_ERRORS[code]);
  return SSO_CODE.test(code) ? t("auth.sso.unknownCode", { code }) : t("auth.sso.unknown");
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
