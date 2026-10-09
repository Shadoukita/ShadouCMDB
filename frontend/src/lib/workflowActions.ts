// Workflow actions in the designer (SHAA-2736, design SHAA-2725 §2.1, §3, §11.1). Pure functions, no Vue.
//
// Two kinds of action, stored in two places:
// - attribute actions (`setAttributes`) are part of a transition in the draft graph: saved with the
//   draft and taking effect when it is published;
// - notification actions (inbox, e-mail, webhook) belong to the workflow itself: the whole set is
//   sent to PUT …/actions, guarded by the workflow's version, and applies at once to every version.
import type { AttributeDefinition } from "../api/datamodel";
import type { Schemas } from "../api/client";
import { t } from "../i18n";
import type { AttributeChoice, Ref } from "./workflowApprovals";

// ---------- Attribute actions (`setAttributes`, in the draft) ----------

export type SetAttribute = Schemas["WorkflowSetAttribute"];
export type ValueFrom = Schemas["WorkflowValueFrom"];
/** A literal value, or where the value comes from. */
export type SetMode = "literal" | ValueFrom;

/** One attribute action as the transition inspector edits it. */
export interface DraftSetAttribute {
  attribute: string;
  mode: SetMode;
  /** The literal, in the field's type; ignored unless `mode` is literal. */
  value: unknown;
}

/** The API's limit per transition (workflow_transition_set_attributes.position). */
export const MAX_SET_ATTRIBUTES = 20;

export function setAttributeFromApi(s: SetAttribute): DraftSetAttribute {
  if (s.valueFrom) return { attribute: s.attribute, mode: s.valueFrom, value: "" };
  return { attribute: s.attribute, mode: "literal", value: s.value ?? "" };
}

export function setAttributeToApi(s: DraftSetAttribute): SetAttribute {
  return s.mode === "literal" ? { attribute: s.attribute, value: s.value } : { attribute: s.attribute, valueFrom: s.mode };
}

/**
 * The ways a field can be set, by its type (§3.4): `now` for dates and datetimes, `today` for dates,
 * `actor` for a Person reference (the only reference an action may set; never a literal CI id), a literal
 * otherwise, and `clear` unless the field is required.
 */
export function modesFor(field: Pick<AttributeDefinition, "dataType" | "isRequired">): SetMode[] {
  const out: SetMode[] = [];
  if (field.dataType === "reference") out.push("actor");
  else out.push("literal");
  if (field.dataType === "date") out.push("today", "now");
  if (field.dataType === "datetime") out.push("now");
  if (!field.isRequired) out.push("clear");
  return out;
}

/** The first way to set a newly chosen field. */
export function defaultMode(field: Pick<AttributeDefinition, "dataType" | "isRequired">): SetMode {
  return modesFor(field)[0];
}

/**
 * Why a field cannot be the target of an attribute action, or undefined when it can. The publish lint
 * (§3.4 a–f) decides; this only keeps the obvious refusals out of the picker.
 */
export function setTargetRefusal(
  field: Pick<AttributeDefinition, "key" | "isActive" | "isIdentifying" | "systemRole" | "dataType" | "referenceClassId">,
  ctx: { stateFieldKey?: string; transitionFields: string[]; personClassIds: Set<string> },
): string | undefined {
  if (!field.isActive) return t("wfActions.set.refuse.inactive");
  if (field.key === ctx.stateFieldKey) return t("wfActions.set.refuse.state");
  if (field.isIdentifying) return t("wfActions.set.refuse.identifying");
  if (field.systemRole) return t("wfActions.set.refuse.readOnly");
  if (ctx.transitionFields.includes(field.key)) return t("wfActions.set.refuse.transitionField");
  if (field.dataType === "reference" && !(field.referenceClassId && ctx.personClassIds.has(field.referenceClassId))) return t("wfActions.set.refuse.reference");
  return undefined;
}

/** What the draft PUT would refuse for its shape, by path under the transition (`setAttributes[j]…`). */
export function checkSetAttributes(list: DraftSetAttribute[]): { path: string; code: string; message: string }[] {
  const out: { path: string; code: string; message: string }[] = [];
  if (list.length > MAX_SET_ATTRIBUTES) out.push({ path: "setAttributes", code: "too_many", message: t("wfActions.set.tooMany", { n: MAX_SET_ATTRIBUTES }) });
  const seen = new Set<string>();
  list.forEach((s, j) => {
    const path = `setAttributes[${j}]`;
    if (!s.attribute) return out.push({ path: `${path}.attribute`, code: "required", message: t("wfActions.set.chooseField") });
    if (seen.has(s.attribute)) out.push({ path: `${path}.attribute`, code: "duplicate", message: t("wfActions.set.duplicate", { field: s.attribute }) });
    seen.add(s.attribute);
    if (s.mode === "literal") {
      const v = s.value;
      if (v === undefined || v === null || v === "" || (typeof v === "number" && !Number.isFinite(v))) {
        out.push({ path: `${path}.value`, code: "required", message: t("wfActions.set.needsValue", { field: s.attribute }) });
      }
    }
  });
  return out;
}

/** One line per attribute action, for the transitions table and the inspector's tab label. */
export function describeSetAttribute(s: DraftSetAttribute, label: (key: string) => string = (k) => k): string {
  const field = label(s.attribute);
  if (s.mode === "literal") return t("wfActions.set.describe.literal", { field, value: String(s.value ?? "") });
  return t(`wfActions.set.describe.${s.mode}`, { field });
}

// ---------- Notification actions (on the workflow) ----------

export type WorkflowAction = Schemas["WorkflowAction"];
export type WorkflowActionInput = Schemas["WorkflowActionInput"];
export type ActionKind = Schemas["WorkflowActionKind"];
export type ActionTrigger = Schemas["WorkflowActionTrigger"];
export type RecipientSource = Schemas["WorkflowActionRecipientSource"];
export type Participant = Schemas["WorkflowActionParticipant"];
export type ContentLevel = Schemas["WorkflowActionContent"];
export type ClosedStatus = Schemas["WorkflowApprovalClosedStatus"];
export type ServiceOwnerRole = Schemas["WorkflowServiceOwnerRole"];

export const KINDS: ActionKind[] = ["inbox", "email", "webhook"];
export const TRIGGERS: ActionTrigger[] = [
  "transition",
  "approval_requested",
  "approval_step",
  "approval_closed",
  "approval_overdue",
  "instance_cancelled",
  "instance_forced",
];
export const CONTENT_LEVELS: ContentLevel[] = ["minimal", "standard", "detailed"];
export const CLOSED_STATUSES: ClosedStatus[] = ["approved", "rejected", "withdrawn", "cancelled"];
export const PARTICIPANTS: Participant[] = ["actor", "starter", "requester", "approvers"];
/** The placeholders an e-mail's subject and intro may use (§6.2 point 5). */
export const PLACEHOLDERS = [
  "{{ci.label}}",
  "{{ci.ident}}",
  "{{ci.class}}",
  "{{workflow.name}}",
  "{{transition.name}}",
  "{{state.from}}",
  "{{state.to}}",
  "{{actor.name}}",
  "{{approval.step}}",
  "{{approval.dueAt}}",
];
/** The API's limits: recipients per action, actions per trigger and transition, webhook payload fields. */
export const MAX_RECIPIENTS = 20;
export const MAX_PER_TRIGGER = 10;
export const MAX_INCLUDE_ATTRIBUTES = 50;

/** Whether the trigger names a transition: every one but the instance triggers. */
export const triggerHasTransition = (tr: ActionTrigger) => tr !== "instance_cancelled" && tr !== "instance_forced";
export const isApprovalTrigger = (tr: ActionTrigger) => tr.startsWith("approval_");
/** Inbox and e-mail tell people; a webhook goes to its endpoint. */
export const notifiesPeople = (k: ActionKind) => k !== "webhook";
/** The stored actions "who would be notified" can preview: webhooks reach an endpoint, never people (GH#852). */
export const previewableActions = (list: { key: string; name: string; kind: ActionKind }[]) =>
  list.filter((a) => notifiesPeople(a.kind)).map((a) => ({ key: a.key, name: a.name }));

/** The recipient sources an action of this kind takes; a fixed address has no inbox. */
export function sourcesFor(kind: ActionKind): RecipientSource[] {
  if (!notifiesPeople(kind)) return [];
  const all: RecipientSource[] = ["profile", "group", "user", "ci_owner", "ci_attribute", "service_owner", "participant"];
  return kind === "email" ? [...all, "address"] : all;
}

/** The participants a trigger has: the requester and the approvers exist only for approval triggers. */
export function participantsFor(trigger: ActionTrigger): Participant[] {
  return isApprovalTrigger(trigger) ? PARTICIPANTS : ["actor", "starter"];
}

export interface DraftRecipient {
  source: RecipientSource;
  /** Profile, group or user. */
  ref: Ref | null;
  attribute: AttributeChoice | null;
  serviceOwnerRole: ServiceOwnerRole | null;
  participant: Participant | null;
  address: string | null;
}

export interface Localized {
  en: string;
  de: string;
}

/** One notification action as the Actions tab edits it: every setting present, whichever kind it is. */
export interface DraftAction {
  key: string;
  name: string;
  kind: ActionKind;
  trigger: ActionTrigger;
  transition: string | null;
  enabled: boolean;
  recipients: DraftRecipient[];
  /** Webhook: the endpoint's key. */
  endpoint: string;
  excludeActor: boolean;
  /** approval_closed: the outcomes it fires on; empty means all. */
  statuses: ClosedStatus[];
  content: ContentLevel;
  subject: Localized;
  intro: Localized;
  includeAttributes: string[];
}

export function recipientFromApi(r: WorkflowAction["recipients"][number]): DraftRecipient {
  return {
    source: r.source,
    ref: r.source === "profile" ? (r.profile ?? null) : r.source === "group" ? (r.group ?? null) : r.source === "user" ? (r.user ?? null) : null,
    attribute: r.attribute ? { id: r.attribute.id, key: r.attribute.key, label: r.attribute.key } : null,
    serviceOwnerRole: r.serviceOwnerRole ?? null,
    participant: r.participant ?? null,
    address: r.address ?? null,
  };
}

export function actionFromApi(a: WorkflowAction): DraftAction {
  const s = a.settings;
  return {
    key: a.key,
    name: a.name,
    kind: a.kind,
    trigger: a.trigger,
    transition: a.transition ?? null,
    enabled: a.enabled,
    recipients: a.recipients.map(recipientFromApi),
    endpoint: a.endpoint?.key ?? "",
    excludeActor: s.excludeActor ?? true,
    statuses: [...(s.statuses ?? [])],
    content: s.content ?? "standard",
    subject: { en: s.subject?.en ?? "", de: s.subject?.de ?? "" },
    intro: { en: s.intro?.en ?? "", de: s.intro?.de ?? "" },
    includeAttributes: [...(s.includeAttributes ?? [])],
  };
}

/** A new action: an inbox entry on `transition` (or the instance being cancelled when there is none). */
export function newAction(existing: Pick<DraftAction, "key">[], transition: string | null): DraftAction {
  const taken = new Set(existing.map((a) => a.key));
  let key = "notify";
  for (let i = 2; taken.has(key); i++) key = `notify_${i}`;
  return {
    key,
    name: t("wfActions.newName"),
    kind: "inbox",
    trigger: transition ? "transition" : "instance_cancelled",
    transition,
    enabled: true,
    recipients: [],
    endpoint: "",
    excludeActor: true,
    statuses: [],
    content: "standard",
    subject: { en: "", de: "" },
    intro: { en: "", de: "" },
    includeAttributes: [],
  };
}

export function recipientToApi(r: DraftRecipient): Schemas["WorkflowActionRecipientInput"] {
  switch (r.source) {
    case "profile":
      return { source: r.source, profile: r.ref?.id ?? "" };
    case "group":
      return { source: r.source, group: r.ref?.id ?? "" };
    case "user":
      return { source: r.source, user: r.ref?.id ?? "" };
    case "ci_attribute":
      return { source: r.source, attribute: r.attribute?.id ?? "" };
    case "service_owner":
      return { source: r.source, serviceOwnerRole: r.serviceOwnerRole ?? "business" };
    case "participant":
      return { source: r.source, participant: r.participant ?? "actor" };
    case "address":
      return { source: r.source, address: (r.address ?? "").trim() };
    default:
      return { source: r.source };
  }
}

const localized = (l: Localized): { en?: string; de?: string } | null => {
  const out: { en?: string; de?: string } = {};
  if (l.en.trim()) out.en = l.en.trim();
  if (l.de.trim()) out.de = l.de.trim();
  return out.en || out.de ? out : null;
};

/**
 * One action of the PUT body: only what its kind and trigger take, since the API refuses a setting that
 * does not apply (`not_applicable`). Settings the editor keeps for another kind are left out, not lost.
 */
export function actionToApi(a: DraftAction): WorkflowActionInput {
  const out: WorkflowActionInput = {
    key: a.key,
    name: a.name.trim(),
    kind: a.kind,
    trigger: a.trigger,
    transition: triggerHasTransition(a.trigger) ? a.transition : null,
    enabled: a.enabled,
  };
  const settings: WorkflowActionInput["settings"] = {};
  if (notifiesPeople(a.kind)) {
    out.recipients = a.recipients.filter((r) => r.source !== "address" || a.kind === "email").map(recipientToApi);
    settings.excludeActor = a.excludeActor;
  } else {
    out.endpoint = a.endpoint.trim();
    if (a.includeAttributes.length) settings.includeAttributes = [...a.includeAttributes];
  }
  if (a.trigger === "approval_closed" && a.statuses.length && a.statuses.length < CLOSED_STATUSES.length) {
    settings.statuses = CLOSED_STATUSES.filter((s) => a.statuses.includes(s));
  }
  if (a.kind === "email") {
    settings.content = a.content;
    const subject = localized(a.subject);
    const intro = localized(a.intro);
    if (subject) settings.subject = subject;
    if (intro) settings.intro = intro;
  }
  out.settings = settings;
  return out;
}

export const actionsBody = (list: DraftAction[]) => list.map(actionToApi);

/** Whether a recipient names everything its source needs. */
export function recipientComplete(r: DraftRecipient): boolean {
  switch (r.source) {
    case "profile":
    case "group":
    case "user":
      return !!r.ref;
    case "ci_attribute":
      return !!r.attribute;
    case "participant":
      return !!r.participant;
    case "address":
      return /^[^@\s]+@[^@\s]+$/.test((r.address ?? "").trim());
    default:
      return true;
  }
}

/** Two recipients that would reach the same people. */
export function recipientIdentity(r: DraftRecipient): string {
  const what = r.ref?.id ?? r.attribute?.id ?? r.serviceOwnerRole ?? r.participant ?? r.address?.trim().toLowerCase() ?? "";
  return `${r.source}:${what}`;
}

/** A recipient in words: "Group CAB", "The CI's owner", "Whoever started the instance". */
export function recipientLabel(r: DraftRecipient): string {
  switch (r.source) {
    case "profile":
    case "group":
    case "user":
      return t(`wfApproval.source.label.${r.source}`, { name: r.ref?.name ?? "?" });
    case "ci_attribute":
      return t("wfApproval.source.label.ci_attribute", { name: r.attribute?.label ?? r.attribute?.key ?? "?" });
    case "service_owner":
      return t(`wfApproval.source.label.service_owner.${r.serviceOwnerRole ?? "business"}`);
    case "participant":
      return t(`wfActions.participant.${r.participant ?? "actor"}`);
    case "address":
      return t("wfActions.recipient.label.address", { address: r.address ?? "" });
    default:
      return t("wfActions.recipient.label.ci_owner");
  }
}

export interface ActionProblem {
  path: string;
  code: string;
  message: string;
  severity: "error" | "warning";
}

/** What the PUT would refuse for the body's shape, found before sending (paths as the API gives them). */
export function checkActions(list: DraftAction[]): ActionProblem[] {
  const out: ActionProblem[] = [];
  const add = (path: string, code: string, message: string) => out.push({ path, code, message, severity: "error" });
  const keys = new Set<string>();
  const perTrigger = new Map<string, number>();
  list.forEach((a, i) => {
    const p = `actions[${i}]`;
    if (!/^[a-z][a-z0-9_]{0,62}$/.test(a.key)) add(`${p}.key`, "invalid", t("dm.key.format"));
    else if (keys.has(a.key)) add(`${p}.key`, "duplicate", t("wfActions.check.duplicateKey", { key: a.key }));
    keys.add(a.key);
    if (!a.name.trim()) add(`${p}.name`, "required", t("wfActions.check.name"));
    if (triggerHasTransition(a.trigger) && !a.transition) add(`${p}.transition`, "required", t("wfActions.check.transition"));
    const slot = `${a.trigger}|${triggerHasTransition(a.trigger) ? (a.transition ?? "") : ""}`;
    const n = (perTrigger.get(slot) ?? 0) + 1;
    perTrigger.set(slot, n);
    if (n === MAX_PER_TRIGGER + 1) add(p, "too_many_actions", t("wfActions.check.tooMany", { n: MAX_PER_TRIGGER }));
    if (notifiesPeople(a.kind)) {
      if (a.recipients.length === 0) add(`${p}.recipients`, "required", t("wfActions.check.recipients"));
      if (a.recipients.length > MAX_RECIPIENTS) add(`${p}.recipients`, "too_many", t("wfActions.check.tooManyRecipients", { n: MAX_RECIPIENTS }));
      const allowed = participantsFor(a.trigger);
      a.recipients.forEach((r, j) => {
        if (r.source === "participant" && r.participant && !allowed.includes(r.participant)) {
          add(`${p}.recipients[${j}].participant`, "not_applicable", t("wfActions.check.participant"));
        }
        if (r.source === "address" && a.kind !== "email") add(`${p}.recipients[${j}].source`, "not_applicable", t("wfActions.check.address"));
      });
    } else {
      if (!a.endpoint.trim()) add(`${p}.endpoint`, "required", t("wfActions.check.endpoint"));
      else if (!/^[a-z][a-z0-9_-]{0,62}$/.test(a.endpoint.trim())) add(`${p}.endpoint`, "invalid", t("wfActions.check.endpointKey"));
      if (a.includeAttributes.length > MAX_INCLUDE_ATTRIBUTES) add(`${p}.settings.includeAttributes`, "too_many", t("wfActions.check.tooManyAttributes", { n: MAX_INCLUDE_ATTRIBUTES }));
    }
    if (a.kind === "email") {
      for (const [field, text] of [
        ["subject.en", a.subject.en],
        ["subject.de", a.subject.de],
        ["intro.en", a.intro.en],
        ["intro.de", a.intro.de],
      ] as const) {
        for (const m of text.matchAll(/\{\{\s*([^}]*?)\s*\}\}/g)) {
          if (!PLACEHOLDERS.includes(`{{${m[1]}}}`)) add(`${p}.settings.${field}`, "unknown_placeholder", t("wfActions.check.placeholder", { name: m[0] }));
        }
      }
    }
  });
  return out;
}

/** Problems about action `i` of the body they were found in; `under` narrows to one field (`recipients`, `settings.subject.en`). */
export function problemsOfAction<P extends { path: string }>(problems: P[], i: number, under?: string): P[] {
  const base = `actions[${i}]`;
  const prefix = under ? `${base}.${under}` : base;
  return problems.filter((p) => p.path === prefix || p.path.startsWith(`${prefix}.`) || p.path.startsWith(`${prefix}[`));
}

/**
 * Paths, after `actions[i]`, the action editor shows next to a control. The e-mail lint names a
 * subject or intro as a whole (`settings.subject`: one language only, CI placeholders with minimal
 * content); the client-side checks name one language of it (`settings.subject.en`).
 */
const ON_A_FIELD = /^\.(key|name|kind|trigger|transition|endpoint|recipients(\[\d+\](\..*)?)?|settings\.(content|statuses|excludeActor|includeAttributes(\[\d+\])?|(subject|intro)(\.(en|de))?))$/;

/** Problems of action `i` no control of its editor shows: about the action as a whole, or a field without a control. */
export function unplacedProblems<P extends { path: string }>(problems: P[], i: number): P[] {
  return problemsOfAction(problems, i).filter((p) => !ON_A_FIELD.test(p.path.slice(`actions[${i}]`.length)));
}

/** Problems about no action in particular. */
export function generalProblems<P extends { path: string }>(problems: P[]): P[] {
  return problems.filter((p) => !/^actions\[\d+\]/.test(p.path));
}

/** The trigger in words, with its transition: "After Approve", "Approval requested on Submit". */
export function describeTrigger(a: Pick<DraftAction, "trigger" | "transition">, transitionName: (key: string) => string = (k) => k): string {
  const name = a.transition ? transitionName(a.transition) : "";
  return t(`wfActions.trigger.describe.${a.trigger}`, { transition: name });
}

/**
 * The transitions an action can name: the draft's and the current version's (by key, draft names
 * first), then keys only stored actions still name, marked as older.
 */
export function transitionChoices(
  sources: { key: string; name: string }[][],
  actions: Pick<DraftAction, "transition">[],
): { key: string; name: string; orphan: boolean }[] {
  const out = new Map<string, { key: string; name: string; orphan: boolean }>();
  for (const list of sources) for (const tr of list) if (!out.has(tr.key)) out.set(tr.key, { key: tr.key, name: tr.name, orphan: false });
  for (const a of actions) if (a.transition && !out.has(a.transition)) out.set(a.transition, { key: a.transition, name: a.transition, orphan: true });
  return [...out.values()];
}
