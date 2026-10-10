// Approval delegations (SHAA-2916, design SHAA-1869 A6b): the form's local date-time values, the checks the API
// makes too (so the operator hears about them before sending), the request body, and how a delegation's scope
// reads. Problems are keyed by the body field, the same keys as the API's VALIDATION_ERROR details, so both
// land next to the same input. Pure functions, so the unit tests cover them.
import type { ApprovalDelegation, ApprovalDelegationStatus, AdminDelegationBody, MyDelegationBody } from "../api/approvals";
import { t } from "../i18n";

/** The longest window the API accepts. */
export const MAX_DELEGATION_DAYS = 90;
const DAY_MS = 86_400_000;
const KEY = /^[a-z][a-z0-9_]{0,62}$/;

/** A Date as the value of an `<input type="datetime-local">` (local time, to the minute). */
export function toLocalInput(d: Date): string {
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}T${p(d.getHours())}:${p(d.getMinutes())}`;
}

/** The ISO instant of a `datetime-local` value, or null when it is empty or not a date. */
export function fromLocalInput(s: string): string | null {
  if (!s) return null;
  const d = new Date(s);
  return Number.isNaN(d.getTime()) ? null : d.toISOString();
}

export interface DelegationDraft {
  /** Whose approvals (the admin form only). */
  principalId: string;
  delegateId: string;
  /** `datetime-local` values. */
  startsAt: string;
  endsAt: string;
  /** Limits it to one workflow; "" for every workflow. */
  definitionKey: string;
  reason: string;
}

/** A week from now, starting now: what the form opens with. */
export function newDelegationDraft(now = new Date()): DelegationDraft {
  return { principalId: "", delegateId: "", startsAt: toLocalInput(now), endsAt: toLocalInput(new Date(now.getTime() + 7 * DAY_MS)), definitionKey: "", reason: "" };
}

/** What is wrong before sending, by body field. `me` is the caller (no self-delegation; an admin may not be the delegate). */
export function delegationProblems(d: DelegationDraft, admin: boolean, me: string | undefined, now = Date.now()): Record<string, string> {
  const out: Record<string, string> = {};
  if (admin && !d.principalId) out.principalUserId = t("delegations.problem.principal");
  if (!d.delegateId) out.delegateUserId = t("delegations.problem.delegate");
  else if (d.delegateId === (admin ? d.principalId : me)) out.delegateUserId = t("delegations.problem.self");
  else if (admin && d.delegateId === me) out.delegateUserId = t("delegations.problem.creator");
  const start = fromLocalInput(d.startsAt);
  const end = fromLocalInput(d.endsAt);
  if (!start) out.startsAt = t("delegations.problem.date");
  if (!end) out.endsAt = t("delegations.problem.date");
  else if (Date.parse(end) <= now) out.endsAt = t("delegations.problem.past");
  else if (start && Date.parse(end) <= Date.parse(start)) out.endsAt = t("delegations.problem.order");
  else if (start && Date.parse(end) - Date.parse(start) > MAX_DELEGATION_DAYS * DAY_MS) out.endsAt = t("delegations.problem.long", { days: MAX_DELEGATION_DAYS });
  if (d.definitionKey && !KEY.test(d.definitionKey)) out.definitionKey = t("delegations.problem.key");
  return out;
}

export function delegationBody(d: DelegationDraft, admin: true): AdminDelegationBody;
export function delegationBody(d: DelegationDraft, admin: false): MyDelegationBody;
export function delegationBody(d: DelegationDraft, admin: boolean): MyDelegationBody | AdminDelegationBody {
  const body: MyDelegationBody = {
    delegateUserId: d.delegateId,
    startsAt: fromLocalInput(d.startsAt) ?? "",
    endsAt: fromLocalInput(d.endsAt) ?? "",
    definitionKey: d.definitionKey || undefined,
    reason: d.reason.trim() || null,
  };
  return admin ? { ...body, principalUserId: d.principalId } : body;
}

/** Which workflows it covers: all, one by name, or one on a type the caller may not view (not named). */
export function delegationScope(r: Pick<ApprovalDelegation, "scoped" | "definitionKey" | "definitionName">): string {
  if (!r.scoped) return t("delegations.scope.all");
  return r.definitionName ?? r.definitionKey ?? t("delegations.scope.hidden");
}

/** Scheduled and active delegations may be revoked; ended and revoked ones stay as history. */
export const canRevoke = (s: ApprovalDelegationStatus) => s === "scheduled" || s === "active";
