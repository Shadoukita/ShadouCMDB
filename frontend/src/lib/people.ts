// Users ↔ Person CIs (SHAA-1505): every sign-in account is linked 1 : 1 to a CI of the built-in Person class
// through its e-mail address. The API keeps the link; these helpers word its answers for the UI.
import type { ApiErrorDetail } from "../api/client";
import { t, type MessageKey } from "../i18n/index";

export type SignInStatus = "ready" | "email_required" | "person_missing";

/** The sign-in states an account can be filtered by on the Users page, in the order the filter lists them. */
export const SIGN_IN_STATUSES: readonly SignInStatus[] = ["ready", "email_required", "person_missing"];

const STATUS_LABELS: Record<SignInStatus, MessageKey> = {
  ready: "people.status.ready",
  email_required: "people.status.email_required",
  person_missing: "people.status.person_missing",
};

export const signInStatusLabel = (s: SignInStatus) => t(STATUS_LABELS[s]);

/** A value of ?signInStatus= the API accepts, else undefined (a hand-edited URL is ignored, not sent). */
export function parseSignInStatus(value: unknown): SignInStatus | undefined {
  return typeof value === "string" && (SIGN_IN_STATUSES as readonly string[]).includes(value) ? (value as SignInStatus) : undefined;
}

const EMAIL_CODES: Record<string, MessageKey> = {
  unique: "people.email.taken",
  person_email_taken: "people.email.personTaken",
};

/**
 * The message for an e-mail the API refused (409 with `details[].field = "email"`): another account uses it
 * (`unique`), or a Person CI that is not this account's has it (`person_email_taken`). Any other detail keeps
 * the API's own message.
 */
export function emailErrorMessage(details: readonly ApiErrorDetail[]): string | undefined {
  const d = details.find((x) => x.field === "email");
  if (!d) return undefined;
  return d.code && Object.hasOwn(EMAIL_CODES, d.code) ? t(EMAIL_CODES[d.code]) : d.message;
}

/** A light check before the round trip; the API decides (it also enforces uniqueness and the 254 limit). */
export function looksLikeEmail(value: string): boolean {
  return value.length <= 254 && /^[^\s@]+@[^\s@]+$/.test(value);
}
