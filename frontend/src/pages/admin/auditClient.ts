// Hover text for the audit log's sign-in, MFA and API token rows.
import type { AuditEntry } from "../../api/queries";
import { t } from "../../i18n";

export const str = (v: unknown) => (typeof v === "string" && v ? v : undefined);

/** Rows that record the requesting client: sign-in events, MFA events and API token use. */
export const isClientEvent = (e: Pick<AuditEntry, "action" | "entityType">) =>
  e.entityType === "sessions" || e.action === "token.use" || e.action.startsWith("mfa.");

/**
 * The client addresses, the browser and, for revocations and lockouts, why. `ipAddress` is the
 * address the server can vouch for; `claimedIpAddress` is the leftmost X-Forwarded-For hop, which
 * the client may have forged, so it is always labelled as unverified.
 */
export function clientTitle(e: Pick<AuditEntry, "action" | "entityType" | "newValue">): string | undefined {
  if (!isClientEvent(e)) return undefined;
  const snap = (e.newValue ?? {}) as Record<string, unknown>;
  const session = (snap.session ?? {}) as Record<string, unknown>;
  const parts = [
    str(snap.reason) && t("audit.client.reason", { reason: String(snap.reason) }),
    typeof snap.lockedForSeconds === "number" && t("audit.client.lockedFor", { n: snap.lockedForSeconds }),
    // Sign-in rows already name the address in the Record column.
    e.entityType !== "sessions" && str(snap.ipAddress) && t("audit.client.address", { address: String(snap.ipAddress) }),
    str(session.ipAddress) && str(snap.ipAddress) !== str(session.ipAddress) && t("audit.client.sessionFrom", { address: String(session.ipAddress) }),
    str(snap.peerIpAddress) && t("audit.client.peerAddress", { address: String(snap.peerIpAddress) }),
    str(snap.claimedIpAddress) && t("audit.client.claimedAddress", { address: String(snap.claimedIpAddress) }),
    (str(snap.userAgent) || str(session.userAgent)) && t("audit.client.browser", { agent: str(snap.userAgent) ?? str(session.userAgent) }),
  ].filter((p): p is string => typeof p === "string");
  return parts.length ? parts.join("\n") : undefined;
}
