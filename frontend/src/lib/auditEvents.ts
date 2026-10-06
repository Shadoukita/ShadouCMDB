import type { AuditEntry } from "../api/queries";
import { t, type MessageKey } from "../i18n";

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

/** Creating and restoring add, deleting removes, workflow steps are information; everything else is neutral. */
export function actionTone(action: string): "ok" | "danger" | "info" | "" {
  if (action === "create" || action === "restore") return "ok";
  if (action === "delete") return "danger";
  if (action.startsWith("workflow.")) return "info";
  return "";
}

const ACTION_KEYS = new Set([
  "create",
  "update",
  "delete",
  "restore",
  "export",
  "workflow.start",
  "workflow.transition",
  "workflow.cancel",
  "workflow.migrate",
  "workflow.force",
]);

/** A readable name for the actions a record's history shows; the raw action otherwise (it stays in the title). */
export function actionLabel(action: string): string {
  return ACTION_KEYS.has(action) ? t(`event.action.${action}` as MessageKey) : action;
}

/** `2026-10-06 14:48:04` in UTC: a stream is compared across time zones and with server logs. */
export function formatUtc(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toISOString().slice(0, 19).replace("T", " ");
}
