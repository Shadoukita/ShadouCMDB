import type { EffectiveAttribute } from "../api/queries";

/**
 * Form values are kept as strings while editing and converted to API JSON with
 * `toApiValue`. One input per attribute dataType, so any CI class the API
 * defines renders without frontend changes.
 */
export type FormValue = string;

/** The parts of an attribute definition that decide its input and value format (a definition, or a draft of one). */
export type AttributeShape = Pick<EffectiveAttribute, "dataType" | "enumValues" | "validation" | "referenceClassId" | "lookupListId">;

export function toFormValue(def: Pick<AttributeShape, "dataType">, value: unknown): FormValue {
  if (value === null || value === undefined) return "";
  if (def.dataType === "boolean") return value === true ? "true" : value === false ? "false" : "";
  if (def.dataType === "datetime" && typeof value === "string") return isoToLocalInput(value);
  return String(value);
}

/** Returns the JSON value for the API; null clears. Non-numeric text is sent as-is so the API reports it. */
export function toApiValue(def: Pick<AttributeShape, "dataType"> & Partial<Pick<AttributeShape, "validation">>, value: FormValue): unknown {
  if (value === "" || value === undefined) return null;
  switch (def.dataType) {
    case "boolean":
      return value === "true";
    case "integer":
    case "number": {
      const n = Number(value);
      return value.trim() !== "" && Number.isFinite(n) ? n : value;
    }
    case "datetime":
      return new Date(value).toISOString();
    default:
      // Multi-line text is stored as typed: trimming would drop a leading indent or trailing line breaks.
      if (value.trim() === "") return null;
      return isMultiline(def) ? value : value.trim();
  }
}

/**
 * The current local date ("date") or date and time (anything else) as a form value.
 * Double-clicking a date or datetime input fills it in with this.
 */
export function nowFormValue(dataType: "date" | "datetime"): FormValue {
  const v = isoToLocalInput(new Date().toISOString());
  return dataType === "date" ? v.slice(0, 10) : v;
}

/** Tooltip of date and datetime inputs. */
export const NOW_HINT = "Double-click to set the current date and time";

function isoToLocalInput(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export interface Validation {
  min?: number;
  max?: number;
  pattern?: string;
  maxLength?: number;
  /** Text only: a multi-line text area whose line breaks and surrounding whitespace are kept. */
  multiline?: boolean;
}

export const isMultiline = (def: { dataType: string; validation?: unknown }) =>
  def.dataType === "text" && (def.validation as Validation | null | undefined)?.multiline === true;

export function hintFor(def: EffectiveAttribute): string | undefined {
  const v = (def.validation ?? {}) as Validation;
  const parts: string[] = [];
  // helpText is written for the operator filling in the form; description is the fallback.
  if (def.helpText || def.description) parts.push((def.helpText || def.description)!);
  if (def.dataType === "ip") parts.push("IPv4 or IPv6 address");
  if (def.dataType === "cidr") parts.push("Network in CIDR notation, e.g. 10.0.0.0/24");
  if (v.min !== undefined && v.max !== undefined) parts.push(`${v.min} – ${v.max}`);
  else if (v.min !== undefined) parts.push(`Min ${v.min}`);
  else if (v.max !== undefined) parts.push(`Max ${v.max}`);
  if (v.maxLength) parts.push(`Max ${v.maxLength} characters`);
  return parts.join(" · ") || undefined;
}
