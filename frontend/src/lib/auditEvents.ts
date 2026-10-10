import type { AuditEntry } from "../api/queries";
import { t, type MessageKey } from "../i18n/index";

/**
 * How an audit entry reads in an event stream (design §2.7, audit R8): one tone per action wherever it
 * appears (History, Audit trail), the source the change came through, and the time in UTC.
 */

export type EventSource = "ui" | "api" | "import" | "system";

/** The channel a change came through: a signed-in user works in the UI, an API token is the API. */
export function eventSource(entry: Pick<AuditEntry, "actorType">): EventSource {
  switch (entry.actorType) {
    case "user":
      return "ui";
    case "api_client":
      return "api";
    case "import":
      return "import";
    default:
      return "system";
  }
}

/** The sources in the order the filter chips show them, each with the actor type the API filters on. */
export const EVENT_SOURCES: readonly { source: EventSource; actorType: AuditEntry["actorType"] }[] = [
  { source: "ui", actorType: "user" },
  { source: "api", actorType: "api_client" },
  { source: "import", actorType: "import" },
  { source: "system", actorType: "system" },
];

export const sourceLabel = (s: EventSource) => t(`event.source.${s}` satisfies MessageKey);

/**
 * Creating and restoring add, deleting removes, workflow steps are information; failed and refused
 * sign-ins and second factors are dangers, a lock-out or a refused schema change a warning (audit A7).
 */
export function actionTone(action: string): "ok" | "danger" | "warn" | "info" | "" {
  if (action === "create" || action === "restore" || action === "login.success") return "ok";
  if (action === "delete" || action === "login.failure" || action === "mfa.failure") return "danger";
  if (action === "login.locked" || action === "schema_change.refused" || action === "session.reauthentication_required") return "warn";
  if (action.startsWith("workflow.")) return "info";
  return "";
}

/** Every action the audit log records, in the order the Audit log's filter offers them. */
export const AUDIT_ACTIONS = [
  "create",
  "update",
  "delete",
  "restore",
  "login.success",
  "login.failure",
  "login.locked",
  "logout",
  "session.revoke",
  "session.reauthenticate",
  "session.reauthentication_required",
  "audit.purge",
  "backup.restore",
  "token.use",
  "mfa.enrol",
  "mfa.disable",
  "mfa.failure",
  "mfa.recovery_code_used",
  "mfa.recovery_codes",
  "schema_change.refused",
  "export",
  "import.commit",
  "import.report_read",
  "workflow.publish",
  "workflow.start",
  "workflow.cancel",
  "workflow.transition",
  "workflow.migrate",
  "workflow.force",
  "workflow.approval_request",
  "workflow.approval_decide",
  "workflow.approval_close",
  "workflow.approval_overdue",
  "workflow.approval_refresh",
  "webhook_endpoint.rotate_secret",
  "webhook_endpoint.suspend",
  "webhook_endpoint.resume",
  "workflow.action_dead",
  "workflow.action_retry",
  "workflow.action_discard",
  "workflow.action_suppressed",
  "mail.test",
  "workflow.action_test",
] as const;
export type AuditAction = (typeof AUDIT_ACTIONS)[number];
const ACTION_KEYS = new Set<string>(AUDIT_ACTIONS);

/** A readable name for an action; an action newer than this client shows raw (the raw action stays in the title). */
export function actionLabel(action: string): string {
  return ACTION_KEYS.has(action) ? t(`event.action.${action}` as MessageKey) : action;
}

/** `2026-10-06 14:48:04` in UTC: a stream is compared across time zones and with server logs. */
export function formatUtc(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toISOString().slice(0, 19).replace("T", " ");
}
