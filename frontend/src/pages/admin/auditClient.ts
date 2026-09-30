// Hover text for the audit log's sign-in, MFA and API token rows.
import type { AuditEntry } from "../../api/queries";

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
    str(snap.reason) && `Reason: ${snap.reason}`,
    typeof snap.lockedForSeconds === "number" && `Locked for ${snap.lockedForSeconds}s`,
    // Sign-in rows already name the address in the Record column.
    e.entityType !== "sessions" && str(snap.ipAddress) && `Address: ${snap.ipAddress}`,
    str(session.ipAddress) && str(snap.ipAddress) !== str(session.ipAddress) && `Session opened from ${session.ipAddress}`,
    str(snap.peerIpAddress) && `Peer address: ${snap.peerIpAddress}`,
    str(snap.claimedIpAddress) && `Claimed address (unverified): ${snap.claimedIpAddress}`,
    (str(snap.userAgent) || str(session.userAgent)) && `Browser: ${str(snap.userAgent) ?? str(session.userAgent)}`,
  ].filter((p): p is string => typeof p === "string");
  return parts.length ? parts.join("\n") : undefined;
}
