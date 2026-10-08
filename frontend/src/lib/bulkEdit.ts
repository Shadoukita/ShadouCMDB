import type { Schemas } from "../api/client";
import type { paths } from "../api/schema";
import { toApiValue, type AttributeShape } from "./attributeValues";

/**
 * The inventory's bulk edit (gap G10): the same values set on every selected CI with
 * POST /configuration-items/bulk-update, which answers one result per CI.
 */
export type BulkUpdateBody = paths["/api/v1/configuration-items/bulk-update"]["post"]["requestBody"]["content"]["application/json"];
export type BulkUpdateReport = Schemas["BulkUpdateReport"];

/** Most CIs one request takes (the API refuses more with 400). */
export const BULK_EDIT_LIMIT = 500;

/** Why the selection cannot be bulk edited before anything is sent, or null. */
export function bulkEditBlocked(selected: number, canEdit: boolean): "tooMany" | "noPermission" | null {
  if (selected > BULK_EDIT_LIMIT) return "tooMany";
  if (!canEdit) return "noPermission";
  return null;
}

/** The pseudo-field for the CI's criticality, next to the class's attribute keys (which cannot start with "@"). */
export const CRITICALITY_FIELD = "@criticality";

/** One field the operator chose to change; an empty value clears it on every CI. */
export interface BulkChange {
  field: string;
  value: string;
}

/** The request body; `null` when nothing would change. */
export function bulkUpdateBody(
  ids: string[],
  changes: BulkChange[],
  defs: (AttributeShape & { key: string })[],
  allOrNothing: boolean,
): BulkUpdateBody | null {
  if (ids.length === 0 || changes.length === 0) return null;
  const body: BulkUpdateBody = { ids };
  const attributes: Record<string, string | number | boolean | null> = {};
  for (const c of changes) {
    if (c.field === CRITICALITY_FIELD) {
      body.criticalityValueId = c.value || null;
      continue;
    }
    const def = defs.find((d) => d.key === c.field);
    if (!def) continue;
    attributes[c.field] = toApiValue(def, c.value) as string | number | boolean | null;
  }
  if (Object.keys(attributes).length > 0) body.attributes = attributes;
  if (body.attributes === undefined && body.criticalityValueId === undefined) return null;
  if (allOrNothing) body.allOrNothing = true;
  return body;
}

/** A CI the API refused, with what it said (the message, then each field problem). */
export interface BulkRefusal {
  id: string;
  code: string;
  message: string;
  problems: { field: string; message: string }[];
}

export interface BulkOutcome {
  /** false: a CI was refused under `allOrNothing`, so nothing was written. */
  committed: boolean;
  /** CIs written (0 when `allOrNothing` stopped the request). */
  updated: number;
  /** CIs that passed every check but were not written because another one was refused (allOrNothing). */
  heldBack: number;
  refused: BulkRefusal[];
}

export function bulkOutcome(report: BulkUpdateReport): BulkOutcome {
  const refused = report.results
    .filter((r) => !r.ok)
    .map((r) => ({
      id: r.id,
      code: r.error?.code ?? "INTERNAL_ERROR",
      message: r.error?.message ?? "",
      problems: (r.error?.details ?? []).filter((d) => d.message && d.message !== r.error?.message).map((d) => ({ field: d.field, message: d.message })),
    }));
  return {
    committed: report.committed,
    updated: report.committed ? report.succeeded : 0,
    heldBack: report.committed ? 0 : report.succeeded,
    refused,
  };
}

/** A refused CI that still exists for the user (not NOT_FOUND or GONE): it stays selected to try again. */
export const retryable = (code: string) => code !== "NOT_FOUND" && code !== "GONE";

/** A detail's body path ("attributes.owner", "criticalityValueId") as the field's label. */
export function problemField(field: string, labelOf: (key: string) => string | undefined, criticalityLabel: string): string {
  if (field === "criticalityValueId") return criticalityLabel;
  const key = field.startsWith("attributes.") ? field.slice("attributes.".length) : field;
  return labelOf(key) ?? field;
}
