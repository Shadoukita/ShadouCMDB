// The texts of a CI's state badge (CiStateBadge) and of an unset criticality, from the message catalog so
// the business service tables render them in the active locale (GH#478).
import { t } from "../i18n/index";
import { formatDateTime } from "./format";

export interface CiStateInput {
  active: boolean;
  deletedAt?: string | null;
  validFrom?: string;
  validUntil?: string | null;
}

/** One rendered piece: a badge (`tone` is its class) or muted text after it. */
export interface CiStatePart {
  text: string;
  tone: "danger" | "off" | "ok" | "warn" | "muted";
  title?: string;
}

/**
 * "Deleted" or "Inactive" (outside its validity period) for a CI; nothing for an active one unless
 * `showActive` is set. A validity period that starts or ends in the future says when.
 */
export function ciStateParts(ci: CiStateInput, showActive = false, now = Date.now()): CiStatePart[] {
  const future = (iso: string | null | undefined) => !!iso && new Date(iso).getTime() > now;
  if (ci.deletedAt) return [{ text: t("ciState.deleted"), tone: "danger" }];
  if (!ci.active) {
    const parts: CiStatePart[] = [{ text: t("ciState.inactive"), tone: "off", title: t("ciState.inactive.title") }];
    if (future(ci.validFrom)) parts.push({ text: t("ciState.activatesOn", { date: formatDateTime(ci.validFrom) }), tone: "muted" });
    return parts;
  }
  const parts: CiStatePart[] = [];
  if (showActive) parts.push({ text: t("ciState.active"), tone: "ok" });
  if (future(ci.validUntil)) {
    const date = formatDateTime(ci.validUntil);
    parts.push({
      text: t(showActive ? "ciState.alsoDeactivatesOn" : "ciState.deactivatesOn", { date }),
      tone: showActive ? "muted" : "warn",
      title: t("ciState.validUntil", { date }),
    });
  }
  return parts;
}

/** The muted text for a CI without a criticality, where a table shows it. */
export function criticalityNotSet(): string {
  return t("common.notSet");
}
