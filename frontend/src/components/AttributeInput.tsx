import type { EffectiveAttribute } from "../api/queries";
import { CiPicker } from "./CiPicker";

/**
 * Form values are kept as strings/booleans while editing and converted to API
 * JSON with `toApiValue`. One input per attribute dataType, so any CI class the
 * API defines renders without frontend changes.
 */
export type FormValue = string | boolean;

export function toFormValue(def: EffectiveAttribute, value: unknown): FormValue {
  if (value === null || value === undefined) return def.dataType === "boolean" ? "" : "";
  if (def.dataType === "boolean") return value === true ? "true" : value === false ? "false" : "";
  if (def.dataType === "datetime" && typeof value === "string") return isoToLocalInput(value);
  return String(value);
}

/** Returns the JSON value for the API; null clears. Non-numeric text is sent as-is so the API reports it. */
export function toApiValue(def: EffectiveAttribute, value: FormValue): unknown {
  if (value === "" || value === undefined) return null;
  switch (def.dataType) {
    case "boolean":
      return value === "true";
    case "integer":
    case "number": {
      const n = Number(value);
      return typeof value === "string" && value.trim() !== "" && Number.isFinite(n) ? n : value;
    }
    case "datetime":
      return new Date(String(value)).toISOString();
    default:
      return String(value).trim() === "" ? null : String(value).trim();
  }
}

function isoToLocalInput(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

interface Validation {
  min?: number;
  max?: number;
  pattern?: string;
  maxLength?: number;
}

export function hintFor(def: EffectiveAttribute): string | undefined {
  const v = (def.validation ?? {}) as Validation;
  const parts: string[] = [];
  if (def.description) parts.push(def.description);
  if (def.dataType === "ip") parts.push("IPv4 or IPv6 address");
  if (def.dataType === "cidr") parts.push("Network in CIDR notation, e.g. 10.0.0.0/24");
  if (v.min !== undefined && v.max !== undefined) parts.push(`${v.min} – ${v.max}`);
  else if (v.min !== undefined) parts.push(`Min ${v.min}`);
  else if (v.max !== undefined) parts.push(`Max ${v.max}`);
  if (v.maxLength) parts.push(`Max ${v.maxLength} characters`);
  return parts.join(" · ") || undefined;
}

export function AttributeInput({
  def,
  id,
  value,
  onChange,
  invalid,
  describedBy,
  referenceName,
  onReferenceName,
}: {
  def: EffectiveAttribute;
  id: string;
  value: FormValue;
  onChange: (v: FormValue) => void;
  invalid?: boolean;
  describedBy?: string;
  referenceName?: string;
  onReferenceName?: (name: string) => void;
}) {
  const v = (def.validation ?? {}) as Validation;
  const common = {
    id,
    "aria-invalid": invalid || undefined,
    "aria-describedby": describedBy,
  };
  switch (def.dataType) {
    case "boolean":
      return (
        <select {...common} value={String(value)} onChange={(e) => onChange(e.target.value)}>
          <option value="">— not set —</option>
          <option value="true">Yes</option>
          <option value="false">No</option>
        </select>
      );
    case "enum":
      return (
        <select {...common} value={String(value)} onChange={(e) => onChange(e.target.value)}>
          <option value="">— not set —</option>
          {(def.enumValues ?? []).map((ev) => (
            <option key={ev} value={ev}>
              {ev}
            </option>
          ))}
          {value !== "" && !(def.enumValues ?? []).includes(String(value)) && (
            <option value={String(value)}>{String(value)} (no longer allowed)</option>
          )}
        </select>
      );
    case "integer":
    case "number":
      return (
        <input
          {...common}
          type="number"
          step={def.dataType === "integer" ? 1 : "any"}
          min={v.min}
          max={v.max}
          value={String(value)}
          onChange={(e) => onChange(e.target.value)}
        />
      );
    case "date":
      return <input {...common} type="date" value={String(value)} onChange={(e) => onChange(e.target.value)} />;
    case "datetime":
      return <input {...common} type="datetime-local" value={String(value)} onChange={(e) => onChange(e.target.value)} />;
    case "reference":
      return (
        <CiPicker
          id={id}
          classId={def.referenceClassId}
          selected={value ? { id: String(value), name: referenceName ?? String(value) } : null}
          onSelect={(ci) => {
            if (ci) onReferenceName?.(ci.name);
            onChange(ci ? ci.id : "");
          }}
          invalid={invalid}
          describedBy={describedBy}
        />
      );
    case "ip":
    case "cidr":
      return (
        <input
          {...common}
          type="text"
          className="mono"
          spellCheck={false}
          value={String(value)}
          onChange={(e) => onChange(e.target.value)}
        />
      );
    default:
      return (
        <input
          {...common}
          type="text"
          maxLength={v.maxLength}
          value={String(value)}
          onChange={(e) => onChange(e.target.value)}
        />
      );
  }
}
