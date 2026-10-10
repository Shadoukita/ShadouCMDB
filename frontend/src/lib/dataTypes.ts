import type { DataType } from "../api/datamodel";

/** Attribute data types, in the order the attribute editor offers them. Names and hints: `dm.attr.type.*`, `dm.attr.typeHint.*`. */
export const DATA_TYPES: { key: DataType }[] = (
  ["text", "integer", "number", "boolean", "enum", "lookup", "date", "datetime", "ip", "cidr", "reference"] as const
).map((key) => ({ key }));

/** Which validation settings a type supports. */
export function validationKind(dataType: string): "text" | "number" | null {
  if (dataType === "text") return "text";
  if (dataType === "integer" || dataType === "number") return "number";
  return null;
}
