// What a CI form changed (SHAA-1644): the CI page and the edit form send only the changed fields, with the
// version they started from, and offer Save only once something was changed.
import { toApiValue, type AttributeShape, type FormValue } from "./attributeValues";

/**
 * Core CI fields (CORE_FIELDS, without criticality, which is a lookup value of its own). These belong to every CI
 * regardless of class; class-specific fields come from the API.
 */
export type CoreField = "ident" | "validFrom" | "validUntil";
export type CoreValues = Record<CoreField, string>;

const DATETIME = { dataType: "datetime" } as const;

/** Core values for the API; null for an empty one (a new CI leaves those out, so the API fills them in). */
export function coreToApi(c: CoreValues): Record<CoreField, string | null> {
  return {
    ident: c.ident.trim() || null,
    validFrom: c.validFrom ? (toApiValue(DATETIME, c.validFrom) as string) : null,
    validUntil: c.validUntil ? (toApiValue(DATETIME, c.validUntil) as string) : null,
  };
}

/** A CI form's values as typed, next to the values it started from. */
export interface CiFormState {
  defs: readonly (Pick<AttributeShape, "dataType" | "validation"> & { key: string })[];
  core: CoreValues;
  initialCore: CoreValues;
  criticalityId: string;
  initialCriticality: string;
  values: Record<string, FormValue>;
  initialValues: Record<string, FormValue>;
}

/**
 * The PATCH body (without the version) for what was changed, or null when nothing was: a value typed back
 * to what it was is no change. An emptied valid until clears it (open-ended); an emptied ident keeps the current one.
 */
export function ciEdits(s: CiFormState): Record<string, unknown> | null {
  const attributes: Record<string, unknown> = {};
  for (const d of s.defs) {
    const cur = s.values[d.key] ?? "";
    if (cur !== (s.initialValues[d.key] ?? "")) attributes[d.key] = toApiValue(d, cur);
  }
  const now = coreToApi(s.core);
  const initial = coreToApi(s.initialCore);
  const changed: Record<string, unknown> = {};
  for (const k of Object.keys(now) as CoreField[]) if (now[k] !== initial[k] && !(k === "ident" && now[k] === null)) changed[k] = now[k];
  if (s.criticalityId !== s.initialCriticality) changed.criticalityValueId = s.criticalityId || null;
  if (Object.keys(changed).length === 0 && Object.keys(attributes).length === 0) return null;
  return { ...changed, ...(Object.keys(attributes).length ? { attributes } : {}) };
}
