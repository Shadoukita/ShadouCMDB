// The approvals part of the workflow designer (SHAA-1869 A5). Pure functions, no Vue: a transition's
// approval policy (steps, N, due period, flags) as the draft edits it, the due period as an amount and
// a unit, and the approvers matrix (who decides each step), sent as the whole set to PUT …/approvers.
import type { Schemas } from "../api/client";
import { t } from "../i18n";

export type ApprovalStep = Schemas["WorkflowApprovalStep"];
export type Approver = Schemas["WorkflowApprover"];
export type ApproverRole = Approver["role"];
export type ApproverSource = Approver["source"];
export type ServiceOwnerRole = "technical" | "business";

/** A step as the designer edits it: every optional field filled with the API's default. */
export interface DraftApprovalStep {
  key: string;
  name: string;
  requiredApprovals: number;
  /** ISO 8601 duration (`P2D`, `PT4H`), or null for no due date. */
  dueAfter: string | null;
  onOverdue: "flag" | "reject";
  distinctFromEarlier: boolean;
  excludeActorsOf: string[];
  allowApiTokens: boolean;
}

/** The API's limits (workflow_transition_approval_steps). */
export const MAX_STEPS = 5;
export const MAX_REQUIRED = 20;
export const MIN_DUE_MINUTES = 15;
export const MAX_DUE_MINUTES = 90 * 24 * 60;

export function stepFromApi(s: ApprovalStep): DraftApprovalStep {
  return {
    key: s.key,
    name: s.name,
    requiredApprovals: s.requiredApprovals ?? 1,
    dueAfter: s.dueAfter ?? null,
    onOverdue: s.onOverdue ?? "flag",
    distinctFromEarlier: s.distinctFromEarlier ?? true,
    excludeActorsOf: [...(s.excludeActorsOf ?? [])],
    allowApiTokens: s.allowApiTokens ?? false,
  };
}

export function stepToApi(s: DraftApprovalStep): ApprovalStep {
  const out: ApprovalStep = {
    key: s.key,
    name: s.name,
    requiredApprovals: s.requiredApprovals,
    onOverdue: s.onOverdue,
    distinctFromEarlier: s.distinctFromEarlier,
    excludeActorsOf: [...s.excludeActorsOf],
    allowApiTokens: s.allowApiTokens,
  };
  if (s.dueAfter) out.dueAfter = s.dueAfter;
  return out;
}

/** A new step after `existing`: the first is the technical review, later ones get a numbered key. */
export function newStep(existing: Pick<DraftApprovalStep, "key">[]): DraftApprovalStep {
  const taken = new Set(existing.map((s) => s.key));
  let n = existing.length + 1;
  while (taken.has(`step_${n}`)) n++;
  return {
    key: `step_${n}`,
    name: t("wfApproval.step.defaultName", { n }),
    requiredApprovals: 1,
    dueAfter: null,
    onOverdue: "flag",
    distinctFromEarlier: true,
    excludeActorsOf: [],
    allowApiTokens: false,
  };
}

// ---------- The due period ----------

export type DueUnit = "minutes" | "hours" | "days" | "weeks";
export const DUE_UNITS: DueUnit[] = ["minutes", "hours", "days", "weeks"];
const UNIT_MINUTES: Record<DueUnit, number> = { minutes: 1, hours: 60, days: 1440, weeks: 10080 };

/** Minutes of an ISO 8601 duration of weeks, days, hours and minutes; null when it is not one. */
export function durationMinutes(iso: string): number | null {
  const m = /^P(?:(\d+)W)?(?:(\d+)D)?(?:T(?:(\d+)H)?(?:(\d+)M)?)?$/.exec(iso.trim().toUpperCase());
  if (!m || iso.trim().length < 3 || /T$/.test(iso.trim())) return null;
  const [w, d, h, min] = m.slice(1).map((x) => (x ? Number(x) : 0));
  return w * 10080 + d * 1440 + h * 60 + min;
}

/** The due period as the editor shows it: the largest unit that divides it evenly. */
export function splitDuration(iso: string | null): { amount: number | null; unit: DueUnit } {
  const total = iso ? durationMinutes(iso) : null;
  if (total === null || total === 0) return { amount: null, unit: "days" };
  for (const unit of ["weeks", "days", "hours"] as const) {
    if (total % UNIT_MINUTES[unit] === 0) return { amount: total / UNIT_MINUTES[unit], unit };
  }
  return { amount: total, unit: "minutes" };
}

/** The ISO 8601 duration of an amount and a unit; null for no (or a non-positive) amount. */
export function joinDuration(amount: number | null, unit: DueUnit): string | null {
  if (amount === null || !Number.isInteger(amount) || amount <= 0) return null;
  if (unit === "weeks") return `P${amount}W`;
  if (unit === "days") return `P${amount}D`;
  return unit === "hours" ? `PT${amount}H` : `PT${amount}M`;
}

/** "2 days", "4 hours": for summaries. */
export function describeDuration(iso: string | null): string {
  const { amount, unit } = splitDuration(iso);
  return amount === null ? t("wfApproval.due.none") : t(`wfApproval.due.${unit}`, { n: amount });
}

/** "2 steps: Technical review (1), CAB (2 of …)": the transition table's line for a policy. */
export function describePolicy(steps: DraftApprovalStep[]): string {
  return t("wfApproval.summary", { n: steps.length, steps: steps.map((s) => `${s.name} (${s.requiredApprovals})`).join(" → ") });
}

// ---------- Client-side checks ----------

const KEY = /^[a-z][a-z0-9_]{0,62}$/;

export interface StepProblem {
  /** Path relative to the transition: `approval.steps[1].dueAfter`. */
  path: string;
  code: string;
  message: string;
}

/**
 * What the draft PUT would refuse in a policy, found before sending (the column checks of design §3.1).
 * Whether every step has approvers, and whether the transitions `excludeActorsOf` names exist, stays the lint's question.
 */
export function checkSteps(steps: DraftApprovalStep[]): StepProblem[] {
  const out: StepProblem[] = [];
  if (steps.length > MAX_STEPS) out.push({ path: "approval.steps", code: "too_many", message: t("wfApproval.check.tooMany", { n: MAX_STEPS }) });
  const keys = new Set<string>();
  steps.forEach((s, j) => {
    const p = `approval.steps[${j}]`;
    if (!KEY.test(s.key)) out.push({ path: `${p}.key`, code: "invalid", message: t("wfApproval.check.key", { n: j + 1 }) });
    else if (keys.has(s.key)) out.push({ path: `${p}.key`, code: "duplicate", message: t("wfApproval.check.duplicateKey", { key: s.key }) });
    keys.add(s.key);
    if (!s.name.trim()) out.push({ path: `${p}.name`, code: "required", message: t("wfApproval.check.name", { n: j + 1 }) });
    if (!Number.isInteger(s.requiredApprovals) || s.requiredApprovals < 1 || s.requiredApprovals > MAX_REQUIRED) {
      out.push({ path: `${p}.requiredApprovals`, code: "range", message: t("wfApproval.check.required", { n: j + 1, max: MAX_REQUIRED }) });
    }
    if (s.dueAfter) {
      const min = durationMinutes(s.dueAfter);
      if (min === null || min < MIN_DUE_MINUTES || min > MAX_DUE_MINUTES) {
        out.push({ path: `${p}.dueAfter`, code: "range", message: t("wfApproval.check.due", { n: j + 1 }) });
      }
    } else if (s.onOverdue === "reject") {
      out.push({ path: `${p}.onOverdue`, code: "needs_due", message: t("wfApproval.check.rejectNeedsDue", { n: j + 1 }) });
    }
  });
  return out;
}

// ---------- The approvers matrix ----------

export interface Ref {
  id: string;
  name: string;
}
export interface AttributeChoice {
  id: string;
  key: string;
  label: string;
}

/** One assignment as the matrix edits it. Exactly the reference `source` names is set. */
export interface DraftApprover {
  transitionKey: string;
  stepKey: string;
  role: ApproverRole;
  source: ApproverSource;
  ref: Ref | null;
  attribute: AttributeChoice | null;
  serviceOwnerRole: ServiceOwnerRole | null;
}

export const SOURCES: ApproverSource[] = ["profile", "group", "user", "ci_attribute", "service_owner"];

export function approverFromApi(a: Approver): DraftApprover {
  const ref = a.source === "profile" ? a.profile : a.source === "group" ? a.group : a.source === "user" ? a.user : null;
  return {
    transitionKey: a.transitionKey,
    stepKey: a.stepKey,
    role: a.role,
    source: a.source,
    ref: ref ? { id: ref.id, name: ref.name } : null,
    attribute: a.source === "ci_attribute" && a.attribute ? { id: a.attribute.id, key: a.attribute.key, label: a.attribute.label } : null,
    serviceOwnerRole: a.source === "service_owner" ? a.serviceOwnerRole : null,
  };
}

/** Whether an assignment names everything its source needs. */
export function isComplete(a: DraftApprover): boolean {
  if (a.source === "ci_attribute") return !!a.attribute;
  if (a.source === "service_owner") return !!a.serviceOwnerRole;
  return !!a.ref;
}

/** What tells two assignments of a step apart (the API's unique index). */
export function approverIdentity(a: DraftApprover): string {
  const target = a.source === "ci_attribute" ? a.attribute?.id : a.source === "service_owner" ? a.serviceOwnerRole : a.ref?.id;
  return [a.transitionKey, a.stepKey, a.role, a.source, target ?? ""].join("|");
}

type ApproversBody = {
  transitionKey: string;
  stepKey: string;
  role?: ApproverRole;
  source: ApproverSource;
  profile?: string;
  group?: string;
  user?: string;
  attribute?: string;
  serviceOwnerRole?: ServiceOwnerRole | null;
}[];

/** The PUT body's `approvers`, sorted so equal sets compare equal. */
export function approversBody(list: DraftApprover[]): ApproversBody {
  return list
    .filter(isComplete)
    .map((a) => {
      const out: ApproversBody[number] = { transitionKey: a.transitionKey, stepKey: a.stepKey, role: a.role, source: a.source };
      if (a.source === "profile") out.profile = a.ref!.id;
      else if (a.source === "group") out.group = a.ref!.id;
      else if (a.source === "user") out.user = a.ref!.id;
      else if (a.source === "ci_attribute") out.attribute = a.attribute!.id;
      else out.serviceOwnerRole = a.serviceOwnerRole;
      return out;
    })
    .sort((x, y) => JSON.stringify(x).localeCompare(JSON.stringify(y)));
}

/** "Group CAB", "Field Owner", "Business service owners". */
export function approverLabel(a: DraftApprover): string {
  if (a.source === "ci_attribute") return t("wfApproval.source.label.ci_attribute", { name: a.attribute?.label ?? "?" });
  if (a.source === "service_owner") return t(`wfApproval.source.label.service_owner.${a.serviceOwnerRole ?? "business"}`);
  return t(`wfApproval.source.label.${a.source}`, { name: a.ref?.name ?? "?" });
}

export interface StepRow {
  transitionKey: string;
  transitionName: string;
  stepKey: string;
  stepName: string;
  requiredApprovals: number;
  /** In neither the draft nor the current version: the assignment may serve an older version. */
  orphan: boolean;
}

type PolicySource = { key: string; name: string; approval?: { steps: Pick<ApprovalStep, "key" | "name" | "requiredApprovals">[] } | null }[];

/**
 * The rows of the matrix: every step of the draft and of the current version (the draft's names
 * first), then the (transition, step) pairs only stored assignments still name.
 */
export function stepRows(sources: PolicySource[], approvers: Pick<DraftApprover, "transitionKey" | "stepKey">[]): StepRow[] {
  const out = new Map<string, StepRow>();
  for (const list of sources) {
    for (const tr of list) {
      for (const s of tr.approval?.steps ?? []) {
        const k = `${tr.key}|${s.key}`;
        if (!out.has(k)) {
          out.set(k, { transitionKey: tr.key, transitionName: tr.name, stepKey: s.key, stepName: s.name, requiredApprovals: s.requiredApprovals ?? 1, orphan: false });
        }
      }
    }
  }
  for (const a of approvers) {
    const k = `${a.transitionKey}|${a.stepKey}`;
    if (!out.has(k)) out.set(k, { transitionKey: a.transitionKey, transitionName: a.transitionKey, stepKey: a.stepKey, stepName: a.stepKey, requiredApprovals: 1, orphan: true });
  }
  return [...out.values()];
}

/** The approvers lint's problems about one step: their path is `transitions.<key>.steps.<key>`. */
export function stepProblems<P extends { path: string }>(problems: P[], transitionKey: string, stepKey: string): P[] {
  const exact = `transitions.${transitionKey}.steps.${stepKey}`;
  return problems.filter((p) => p.path === exact || p.path.startsWith(`${exact}.`));
}

/** Problems not about one step of the matrix (`approvers[3]`, `approvers`). */
export function otherProblems<P extends { path: string }>(problems: P[], rows: Pick<StepRow, "transitionKey" | "stepKey">[]): P[] {
  return problems.filter((p) => !rows.some((r) => stepProblems([p], r.transitionKey, r.stepKey).length));
}

// ---------- Publishing ----------

/**
 * Transitions the draft gates whose current-version namesake does not, or does with other steps:
 * instances running on older versions keep the old rules until they are migrated.
 */
export function changedPolicies(
  draft: { key: string; name: string; approval: DraftApprovalStep[] }[],
  current: { key: string; approval?: { steps: ApprovalStep[] } | null }[] | undefined,
): string[] {
  const before = new Map((current ?? []).map((tr) => [tr.key, JSON.stringify((tr.approval?.steps ?? []).map((s) => stepToApi(stepFromApi(s))))]));
  return draft
    .filter((tr) => tr.approval.length > 0 && before.get(tr.key) !== JSON.stringify(tr.approval.map(stepToApi)))
    .map((tr) => tr.name);
}
