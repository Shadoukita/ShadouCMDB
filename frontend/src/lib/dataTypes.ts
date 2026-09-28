import type { DataType } from "../api/datamodel";

/** Attribute data types with operator-facing names, in the order the attribute editor offers them. */
export const DATA_TYPES: { key: DataType; label: string; hint: string }[] = [
  { key: "text", label: "Text", hint: "Free text; optional pattern and maximum length" },
  { key: "integer", label: "Whole number", hint: "Optional minimum and maximum" },
  { key: "number", label: "Decimal number", hint: "Optional minimum and maximum" },
  { key: "boolean", label: "Yes / no", hint: "" },
  { key: "enum", label: "Choice list", hint: "One of a fixed set of values defined on the attribute" },
  { key: "lookup", label: "Lookup list", hint: "One value of an admin-defined list (Administration › Dropdowns)" },
  { key: "date", label: "Date", hint: "" },
  { key: "datetime", label: "Date and time", hint: "" },
  { key: "ip", label: "IP address", hint: "IPv4 or IPv6" },
  { key: "cidr", label: "Network (CIDR)", hint: "e.g. 10.0.0.0/24" },
  { key: "reference", label: "Reference to a CI", hint: "Links to a CI of a chosen class; shown as a link" },
];

const LABELS = new Map(DATA_TYPES.map((t) => [t.key as string, t.label]));

export function dataTypeLabel(key: string): string {
  return LABELS.get(key) ?? key;
}

/** Which validation settings a type supports. */
export function validationKind(dataType: string): "text" | "number" | null {
  if (dataType === "text") return "text";
  if (dataType === "integer" || dataType === "number") return "number";
  return null;
}
