// Webhook endpoints and workflow action deliveries (SHAA-2725 §4.4, §5): the words for the API's codes.
import type { ActionDeliveryStatus } from "../api/actionDeliveries";
import type { WebhookEndpointStatus } from "../api/webhooks";
import { t, type MessageKey } from "../i18n";

/** An endpoint key: lower case first, then lower case, digits, `_` and `-`, at most 63 (the API's rule). */
export const ENDPOINT_KEY_PATTERN = /^[a-z][a-z0-9_-]{0,62}$/;

/** A header name the receiver may need: letters, digits and `-`, at most 64 (the API's rule). */
export const HEADER_NAME_PATTERN = /^[A-Za-z0-9-]{1,64}$/;

/** Limits the API enforces on an endpoint; the defaults are what it uses when a field is left empty. */
export const ENDPOINT_LIMITS = {
  timeoutMs: { min: 1000, max: 30000, default: 10000 },
  maxPerMinute: { min: 1, max: 6000, default: 120 },
  maxInFlight: { min: 1, max: 16, default: 2 },
} as const;

/** Grace periods offered when rotating a secret (the API takes 0–168 hours). */
export const GRACE_HOURS = [0, 1, 24, 72, 168] as const;

export const ENDPOINT_STATUSES: WebhookEndpointStatus[] = ["active", "paused", "suspended"];

export const DELIVERY_STATUSES: ActionDeliveryStatus[] = ["pending", "sending", "held", "delivered", "skipped", "dead", "discarded"];

const STATUS_BADGE: Record<ActionDeliveryStatus | WebhookEndpointStatus, string> = {
  active: "ok",
  paused: "off",
  suspended: "danger",
  pending: "info",
  sending: "info",
  held: "warn",
  delivered: "ok",
  skipped: "off",
  dead: "danger",
  discarded: "off",
};

export const statusBadge = (s: ActionDeliveryStatus | WebhookEndpointStatus) => STATUS_BADGE[s];

export const endpointStatusLabel = (s: WebhookEndpointStatus) => t(`webhooks.status.${s}` as MessageKey);
export const deliveryStatusLabel = (s: ActionDeliveryStatus) => t(`deliveries.status.${s}` as MessageKey);

/** Codes a delivery, a ping or a suspension records, with an explanation each. `address_blocked:<ip>` carries the address. */
const REASONS = [
  "host_not_allowed",
  "http_not_allowed",
  "address_blocked",
  "redirect_not_followed",
  "http_status",
  "unreachable",
  "secret_unreadable",
  "secret_required",
  "breaker",
  "restored",
  "paused",
  "no_view",
  "inactive",
  "no_email",
  "mail_off",
  "throttled_digest",
  "max_attempts",
  "expired",
  "endpoint_deleted",
  "endpoint_suspended",
  "discarded",
  "smtp_rejected",
  "invalid_address",
  "ci_deleted",
  "queue_full",
  "instance_rate",
] as const;

/**
 * The explanation of a reason code, for an administrator who should not need the server log. An unknown
 * code (a newer server) is shown as it is.
 */
export function reasonText(code: string | null | undefined): string {
  if (!code) return "";
  const [head, ...rest] = code.split(":");
  const arg = rest.join(":");
  if ((REASONS as readonly string[]).includes(head)) return t(`outbound.reason.${head}` as MessageKey, { address: arg });
  return code;
}
