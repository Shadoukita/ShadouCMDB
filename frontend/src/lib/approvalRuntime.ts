// Run-time approvals (SHAA-2916, design SHAA-1869 A6): the words for request statuses and close reasons, the
// progress of a pending request, and plain-language explanations of why a decision is refused, both before it
// is tried (`myEligibility.reason`) and after the API refused it (403 WORKFLOW_APPROVAL_SELF / FORBIDDEN,
// 409 WORKFLOW_APPROVAL_STALE / CONFLICT / VERSION_CONFLICT). Pure functions, so the unit tests cover them.
import { ApiError } from "../api/client";
import type { WorkflowApprovalCloseReason, WorkflowApprovalStatus, WorkflowPendingApproval } from "../api/workflowRuntime";
import { hasMessage, t } from "../i18n";

export function approvalStatusLabel(s: WorkflowApprovalStatus): string {
  return t(`approvalRun.status.${s}`);
}

/** Why a closed request ended; null when the status says it all (approved, rejected, withdrawn). */
export function closeReasonLabel(r: WorkflowApprovalCloseReason | null | undefined): string | null {
  if (!r || r === "approved" || r === "rejected" || r === "withdrawn") return null;
  return t(`approvalRun.closeReason.${r}`);
}

/** "Step 1 of 2: 1 of 2 approvals". */
export function pendingProgress(p: Pick<WorkflowPendingApproval, "stepNo" | "stepCount" | "approvals" | "required">): string {
  return t("approvalRun.progress", { step: p.stepNo, steps: p.stepCount, n: Number(p.approvals), required: p.required });
}

/**
 * Why the caller may not decide, for IT admins: the `reason` code of `myEligibility` or the `details[0].code` of
 * a refused decision. `actor_of:<key>` names the transition the caller took part in (`transitionName` resolves
 * its key). Unknown codes fall back to the API's own message.
 */
export function refusalMessage(code: string | null | undefined, apiMessage?: string | null, transitionName?: (key: string) => string): string {
  if (code?.startsWith("actor_of:")) {
    const key = code.slice("actor_of:".length);
    return t("approvalRun.refusal.actorOf", { transition: transitionName?.(key) ?? key });
  }
  const key = `approvalRun.refusal.${code ?? ""}`;
  if (code && hasMessage(key)) return t(key);
  return apiMessage || t("approvalRun.refusal.unknown");
}

export type DecisionProblem =
  /** The CI changed since the request: the change cannot be applied; nothing was recorded. */
  | { kind: "stale"; details: string[] }
  /** Four-eyes or another eligibility rule. */
  | { kind: "refused"; message: string }
  /** The request moved on (another decision, a withdrawal): reload it. */
  | { kind: "moved"; message: string }
  /** A comment problem, shown next to the comment. */
  | { kind: "comment"; message: string }
  | { kind: "other" };

/** Sorts a refused decision into what the dialog shows. */
export function decisionProblem(e: unknown, transitionName?: (key: string) => string): DecisionProblem | null {
  if (!(e instanceof ApiError)) return e ? { kind: "other" } : null;
  const first = e.details[0];
  if (e.code === "WORKFLOW_APPROVAL_STALE") return { kind: "stale", details: e.details.map((d) => d.message).filter(Boolean) };
  if (e.code === "WORKFLOW_APPROVAL_SELF" || e.code === "FORBIDDEN") return { kind: "refused", message: refusalMessage(first?.code, e.message, transitionName) };
  if (e.code === "VERSION_CONFLICT") return { kind: "moved", message: t("approvalRun.moved.version") };
  if (e.code === "CONFLICT") {
    const key = `approvalRun.moved.${first?.code ?? ""}`;
    return { kind: "moved", message: first?.code && hasMessage(key) ? t(key) : e.message };
  }
  if (e.code === "VALIDATION_ERROR") {
    const comment = e.fieldErrors().comment;
    if (comment) return { kind: "comment", message: comment };
  }
  return { kind: "other" };
}
