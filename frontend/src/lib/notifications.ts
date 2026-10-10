// What a notification says and where it leads (SHAA-2356, SHAA-2649). The texts come from the display values the
// server froze at the event (`data`), so they read the same after the CI or workflow is renamed; any value may be
// null, and each sentence falls back to a neutral word rather than showing a blank.
import type { Notification } from "../api/notifications";
import { t } from "../i18n";
import type { MessageKey } from "../i18n/en";
import { formatDateTime } from "./format";

type Data = Record<string, unknown>;

const str = (d: Data, key: string): string | null => {
  const v = d[key];
  return typeof v === "string" && v.trim() ? v : null;
};

const join = (...parts: (string | null)[]) => parts.filter(Boolean).join(" · ");

/** The bell's badge: the count, capped so it stays a small pill. */
export function badgeText(unread: number): string {
  return unread > 99 ? "99+" : String(unread);
}

/**
 * The route the notification opens: approvals and workflow events the workflow instance page (which lists the
 * approval), an import its job page. Null when the event did not record what to open.
 */
export function notificationTarget(n: Pick<Notification, "kind" | "entityType" | "entityId" | "data">): string | null {
  const d = n.data as Data;
  if (n.entityType === "import_jobs") return `/imports/${encodeURIComponent(n.entityId)}`;
  if (n.entityType === "workflow_instances") return `/workflows/${encodeURIComponent(n.entityId)}`;
  const instance = str(d, "instanceId");
  return instance ? `/workflows/${encodeURIComponent(instance)}` : null;
}

const APPROVAL_CLOSED: Record<string, MessageKey> = {
  approved: "notifications.text.approvalApproved",
  rejected: "notifications.text.approvalRejected",
  cancelled: "notifications.text.approvalCancelled",
};

const CLOSE_REASONS: Record<string, MessageKey> = {
  overdue: "notifications.reason.overdue",
  instance_cancelled: "notifications.reason.instanceCancelled",
  instance_forced: "notifications.reason.instanceForced",
  instance_migrated: "notifications.reason.instanceMigrated",
  ci_deleted: "notifications.reason.ciDeleted",
};

/** Approval events a configured workflow notification can report (`data.event`). */
const APPROVAL_EVENTS: Record<string, MessageKey> = {
  approval_request: "notifications.event.approval_request",
  approval_decision: "notifications.event.approval_decision",
  approval_close: "notifications.event.approval_close",
  approval_withdraw: "notifications.event.approval_withdraw",
  approval_overdue: "notifications.event.approval_overdue",
};

const IMPORT_FINISHED: Record<string, MessageKey> = {
  completed: "notifications.text.importCompleted",
  completed_with_errors: "notifications.text.importCompletedWithErrors",
  failed: "notifications.text.importFailed",
};

/** The notification as one sentence and a line of context (workflow, who, when due). */
export function describeNotification(n: Pick<Notification, "kind" | "data">): { title: string; detail: string } {
  const d = n.data as Data;
  const ci = str(d, "ciLabel") ?? str(d, "ciIdent") ?? t("notifications.fallback.ci");
  const workflow = str(d, "definitionName");
  const transition = str(d, "transitionName") ?? str(d, "transitionKey") ?? t("notifications.fallback.transition");
  switch (n.kind) {
    case "approval_requested": {
      const due = str(d, "dueAt");
      const by = str(d, "requestedByName");
      return {
        title: t("notifications.text.approvalRequested", { transition, ci }),
        detail: join(
          workflow,
          str(d, "stepName"),
          by && t("notifications.detail.requestedBy", { name: by }),
          due && t("notifications.detail.due", { date: formatDateTime(due) }),
        ),
      };
    }
    case "approval_closed": {
      const key = APPROVAL_CLOSED[str(d, "status") ?? ""] ?? "notifications.text.approvalClosed";
      const reason = CLOSE_REASONS[str(d, "closeReason") ?? ""];
      const by = str(d, "closedByName");
      return {
        title: t(key, { transition, ci }),
        detail: join(workflow, reason ? t(reason) : null, by && t("notifications.detail.by", { name: by })),
      };
    }
    case "workflow_transition":
    case "workflow_action": {
      const event = str(d, "event");
      const approvalEvent = event ? APPROVAL_EVENTS[event] : undefined;
      if (n.kind === "workflow_action" && approvalEvent) {
        const by = str(d, "actorName");
        return {
          title: t("notifications.text.workflowApprovalEvent", { transition, ci, event: t(approvalEvent) }),
          detail: join(workflow, str(d, "actionName"), by && t("notifications.detail.by", { name: by })),
        };
      }
      const from = str(d, "fromStateName") ?? str(d, "fromStateKey") ?? t("notifications.fallback.state");
      const to = str(d, "toStateName") ?? str(d, "toStateKey") ?? t("notifications.fallback.state");
      const title =
        event === "cancel"
          ? t("notifications.text.workflowCancelled", { ci })
          : event === "force"
            ? t("notifications.text.workflowForced", { ci, to })
            : t("notifications.text.workflowMoved", { ci, from, to });
      const actor = str(d, "actorName");
      return {
        title,
        detail: join(
          workflow,
          event === "transition" ? transition : null,
          n.kind === "workflow_action" ? str(d, "actionName") : null,
          actor && t("notifications.detail.by", { name: actor }),
        ),
      };
    }
    case "import_finished": {
      const key = IMPORT_FINISHED[str(d, "status") ?? ""] ?? "notifications.text.importFinished";
      return {
        title: t(key, { file: str(d, "fileName") ?? t("notifications.fallback.file") }),
        detail: join(str(d, "classKey"), str(d, "errorCode")),
      };
    }
    case "webhook_suspended": {
      const failures = d.consecutiveFailures;
      return {
        title: t("notifications.text.webhookSuspended", {
          endpoint: str(d, "endpointName") ?? str(d, "endpointKey") ?? "",
        }),
        detail: join(
          str(d, "endpointKey"),
          typeof failures === "number" ? t("notifications.detail.failures", { count: String(failures) }) : null,
        ),
      };
    }
    default:
      // A kind added on the server before this client knows it: say something rather than nothing.
      return { title: t("notifications.text.unknown"), detail: "" };
  }
}
