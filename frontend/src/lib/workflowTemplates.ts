// Ready-made workflow graphs for Administration › Workflows › New (SHAA-3040, SHAA-3105). Pure functions, no
// Vue: a template is an ordinary `Draft` that the new-workflow form saves through the same whole-graph PUT the
// designer uses, so the result is edited, linted, published and exported like any other workflow.
import { t, type MessageKey } from "../i18n/index";
import type { DraftApprovalStep } from "./workflowApprovals";
import { autoLayout, type Draft, type DraftState, type DraftTransition } from "./workflowDraft";

export type WorkflowTemplateKey = "blank" | "lifecycle" | "change";

export const WORKFLOW_TEMPLATES: { key: WorkflowTemplateKey; label: MessageKey; hint: MessageKey }[] = [
  { key: "blank", label: "wfTemplate.blank", hint: "wfTemplate.blankHint" },
  { key: "lifecycle", label: "wfTemplate.lifecycle", hint: "wfTemplate.lifecycleHint" },
  { key: "change", label: "wfTemplate.change", hint: "wfTemplate.changeHint" },
];

/** A template key from the URL (`?template=`); anything else is the blank workflow. */
export const templateFromQuery = (v: unknown): WorkflowTemplateKey => (v === "lifecycle" || v === "change" ? v : "blank");

/** A value of the state field's list, as far as matching needs it. */
export interface StateValueOption {
  key: string;
  name: string;
  isActive: boolean;
}

/** A state of a template, with the list value keys it maps to, best first. */
interface StateSpec {
  key: string;
  name: MessageKey;
  category: DraftState["category"];
  terminal: boolean;
  values: string[];
}

interface TransitionSpec {
  key: string;
  name: MessageKey;
  from: string;
  to: string;
  requiresComment?: boolean;
  approval?: { key: string; name: MessageKey; requiredApprovals: number; excludeActorsOf: string[] }[];
}

interface TemplateSpec {
  initialState: string;
  states: StateSpec[];
  transitions: TransitionSpec[];
}

/**
 * The lifecycle's states. The keys of the IT infrastructure starter's "status" list come first (`in_service`
 * for Active), then common names.
 */
const LIFECYCLE: TemplateSpec = {
  initialState: "planned",
  states: [
    { key: "planned", name: "wfTemplate.state.planned", category: "open", terminal: false, values: ["planned", "planning", "ordered"] },
    { key: "active", name: "wfTemplate.state.active", category: "active", terminal: false, values: ["active", "in_service", "operational", "in_production", "production", "live"] },
    { key: "retired", name: "wfTemplate.state.retired", category: "done", terminal: true, values: ["retired", "decommissioned", "disposed", "inactive"] },
  ],
  transitions: [
    { key: "activate", name: "wfTemplate.transition.activate", from: "planned", to: "active" },
    { key: "retire", name: "wfTemplate.transition.retire", from: "active", to: "retired" },
    { key: "cancel_plan", name: "wfTemplate.transition.cancelPlan", from: "planned", to: "retired" },
  ],
};

/** The state a change's impact is assessed in: the CI's workflow panel links to its Impact tab there. */
export const CHANGE_ASSESSMENT_STATE = "assessment";
/** The transition that carries the CAB approval step, and the step's key (approvers are assigned by both). */
export const CHANGE_CAB_TRANSITION = "approve";
export const CHANGE_CAB_STEP = "cab";

/**
 * A change (RFC): Requested → Assessment → Approved (by the CAB) → Implementation → Review → Closed, or
 * Rejected from the assessment, or cancelled while still requested. Whoever submitted the RFC for assessment
 * may not approve it; the requester of the approval never may (four-eyes, always on). The CAB's members are
 * left to the admin: the lint reports the step as having no approvers until they are chosen.
 */
const CHANGE: TemplateSpec = {
  initialState: "requested",
  states: [
    { key: "requested", name: "wfTemplate.state.requested", category: "open", terminal: false, values: ["requested", "new", "submitted", "draft"] },
    { key: CHANGE_ASSESSMENT_STATE, name: "wfTemplate.state.assessment", category: "active", terminal: false, values: ["assessment", "in_assessment", "assess", "evaluation"] },
    { key: "approved", name: "wfTemplate.state.approved", category: "active", terminal: false, values: ["approved", "authorized", "authorised", "scheduled"] },
    { key: "implementation", name: "wfTemplate.state.implementation", category: "active", terminal: false, values: ["implementation", "implementing", "in_progress"] },
    { key: "review", name: "wfTemplate.state.review", category: "active", terminal: false, values: ["review", "in_review", "post_implementation_review"] },
    { key: "closed", name: "wfTemplate.state.closed", category: "done", terminal: true, values: ["closed", "completed", "done"] },
    { key: "rejected", name: "wfTemplate.state.rejected", category: "cancelled", terminal: true, values: ["rejected", "declined", "cancelled", "canceled"] },
  ],
  transitions: [
    { key: "submit", name: "wfTemplate.transition.submit", from: "requested", to: CHANGE_ASSESSMENT_STATE },
    {
      key: CHANGE_CAB_TRANSITION,
      name: "wfTemplate.transition.approve",
      from: CHANGE_ASSESSMENT_STATE,
      to: "approved",
      approval: [{ key: CHANGE_CAB_STEP, name: "wfTemplate.step.cab", requiredApprovals: 1, excludeActorsOf: ["submit"] }],
    },
    { key: "reject", name: "wfTemplate.transition.reject", from: CHANGE_ASSESSMENT_STATE, to: "rejected", requiresComment: true },
    { key: "implement", name: "wfTemplate.transition.implement", from: "approved", to: "implementation" },
    { key: "complete", name: "wfTemplate.transition.complete", from: "implementation", to: "review" },
    { key: "close", name: "wfTemplate.transition.close", from: "review", to: "closed" },
    { key: "cancel", name: "wfTemplate.transition.cancel", from: "requested", to: "rejected" },
  ],
};

/**
 * The active list value a template state maps to: the first candidate key that is a value, else a value
 * whose name is the state's (case-insensitive). Retired values are never chosen: the lint refuses them.
 */
export function matchStateValue(candidates: string[], name: string, values: StateValueOption[]): string | null {
  const active = values.filter((v) => v.isActive);
  for (const key of candidates) if (active.some((v) => v.key === key)) return key;
  const lower = name.trim().toLocaleLowerCase();
  return active.find((v) => v.name.trim().toLocaleLowerCase() === lower)?.key ?? null;
}

/**
 * The draft of a template spec. With a state field, `stateValues` is its list and each state takes the matching
 * value (none when the list has no match: the admin picks one in the designer); without one, `stateValues` is
 * null and no state has a value.
 */
function buildDraft(spec: TemplateSpec, stateValues: StateValueOption[] | null): Draft {
  const states: DraftState[] = spec.states.map((s) => ({
    key: s.key,
    name: t(s.name),
    category: s.category,
    terminal: s.terminal,
    stateValue: stateValues ? matchStateValue(s.values, t(s.name), stateValues) : null,
  }));
  const transitions: DraftTransition[] = spec.transitions.map((x) => ({
    key: x.key,
    name: t(x.name),
    from: x.from,
    to: x.to,
    requiresComment: x.requiresComment ?? false,
    fields: [],
    conditions: { kind: "group", mode: "all", children: [] },
    approval: (x.approval ?? []).map(
      (s): DraftApprovalStep => ({
        key: s.key,
        name: t(s.name),
        requiredApprovals: s.requiredApprovals,
        dueAfter: null,
        onOverdue: "flag",
        distinctFromEarlier: true,
        excludeActorsOf: [...s.excludeActorsOf],
        allowApiTokens: false,
      }),
    ),
    setAttributes: [],
  }));
  const draft: Draft = { initialState: spec.initialState, states, transitions, positions: {} };
  autoLayout(draft);
  return draft;
}

/** Planned (initial) → Active → Retired (terminal), plus Planned → Retired for a plan that is dropped. */
export const lifecycleDraft = (stateValues: StateValueOption[] | null): Draft => buildDraft(LIFECYCLE, stateValues);

/** Requested (initial) → Assessment → Approved (CAB) → Implementation → Review → Closed, or Rejected. */
export const changeDraft = (stateValues: StateValueOption[] | null): Draft => buildDraft(CHANGE, stateValues);

/** The draft a template starts from; `null` for the blank workflow (designed from scratch). */
export function templateDraft(key: WorkflowTemplateKey, stateValues: StateValueOption[] | null): Draft | null {
  if (key === "lifecycle") return lifecycleDraft(stateValues);
  return key === "change" ? changeDraft(stateValues) : null;
}
