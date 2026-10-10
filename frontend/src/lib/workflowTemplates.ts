// Ready-made workflow graphs for Administration › Workflows › New (SHAA-3040). Pure functions, no Vue: a
// template is an ordinary `Draft` that the new-workflow form saves through the same whole-graph PUT the
// designer uses, so the result is edited, linted, published and exported like any other workflow.
import { t, type MessageKey } from "../i18n/index";
import { autoLayout, type Draft, type DraftState, type DraftTransition } from "./workflowDraft";

export type WorkflowTemplateKey = "blank" | "lifecycle";

export const WORKFLOW_TEMPLATES: { key: WorkflowTemplateKey; label: MessageKey; hint: MessageKey }[] = [
  { key: "blank", label: "wfTemplate.blank", hint: "wfTemplate.blankHint" },
  { key: "lifecycle", label: "wfTemplate.lifecycle", hint: "wfTemplate.lifecycleHint" },
];

/** A template key from the URL (`?template=`); anything else is the blank workflow. */
export const templateFromQuery = (v: unknown): WorkflowTemplateKey => (v === "lifecycle" ? "lifecycle" : "blank");

/** A value of the state field's list, as far as matching needs it. */
export interface StateValueOption {
  key: string;
  name: string;
  isActive: boolean;
}

/**
 * The lifecycle's states, each with the list value keys it maps to, best first. The keys of the
 * IT infrastructure starter's "status" list come first (`in_service` for Active), then common names.
 */
const LIFECYCLE_STATES: { key: string; name: MessageKey; category: DraftState["category"]; terminal: boolean; values: string[] }[] = [
  { key: "planned", name: "wfTemplate.state.planned", category: "open", terminal: false, values: ["planned", "planning", "ordered"] },
  { key: "active", name: "wfTemplate.state.active", category: "active", terminal: false, values: ["active", "in_service", "operational", "in_production", "production", "live"] },
  { key: "retired", name: "wfTemplate.state.retired", category: "done", terminal: true, values: ["retired", "decommissioned", "disposed", "inactive"] },
];

const LIFECYCLE_TRANSITIONS: { key: string; name: MessageKey; from: string; to: string }[] = [
  { key: "activate", name: "wfTemplate.transition.activate", from: "planned", to: "active" },
  { key: "retire", name: "wfTemplate.transition.retire", from: "active", to: "retired" },
  { key: "cancel_plan", name: "wfTemplate.transition.cancelPlan", from: "planned", to: "retired" },
];

/**
 * The active list value a lifecycle state maps to: the first candidate key that is a value, else a value
 * whose name is the state's (case-insensitive). Retired values are never chosen: the lint refuses them.
 */
export function matchStateValue(candidates: string[], name: string, values: StateValueOption[]): string | null {
  const active = values.filter((v) => v.isActive);
  for (const key of candidates) if (active.some((v) => v.key === key)) return key;
  const lower = name.trim().toLocaleLowerCase();
  return active.find((v) => v.name.trim().toLocaleLowerCase() === lower)?.key ?? null;
}

/**
 * Planned (initial) → Active → Retired (terminal), plus Planned → Retired for a plan that is dropped. With
 * a state field, `stateValues` is its list and each state takes the matching value (none when the list has
 * no match: the admin picks one in the designer); without one, `stateValues` is null and no state has a value.
 */
export function lifecycleDraft(stateValues: StateValueOption[] | null): Draft {
  const states: DraftState[] = LIFECYCLE_STATES.map((s) => ({
    key: s.key,
    name: t(s.name),
    category: s.category,
    terminal: s.terminal,
    stateValue: stateValues ? matchStateValue(s.values, t(s.name), stateValues) : null,
  }));
  const transitions: DraftTransition[] = LIFECYCLE_TRANSITIONS.map((x) => ({
    key: x.key,
    name: t(x.name),
    from: x.from,
    to: x.to,
    requiresComment: false,
    fields: [],
    conditions: { kind: "group", mode: "all", children: [] },
    approval: [],
    setAttributes: [],
  }));
  const draft: Draft = { initialState: "planned", states, transitions, positions: {} };
  autoLayout(draft);
  return draft;
}

/** The draft a template starts from; `null` for the blank workflow (designed from scratch). */
export function templateDraft(key: WorkflowTemplateKey, stateValues: StateValueOption[] | null): Draft | null {
  return key === "lifecycle" ? lifecycleDraft(stateValues) : null;
}
