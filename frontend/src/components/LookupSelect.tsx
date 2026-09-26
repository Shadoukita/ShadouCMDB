import { useLookup, type LookupKind } from "../api/queries";

/** <select> over a lookup list (status, environment, owner, location). Retired rows are hidden unless selected. */
export function LookupSelect({
  kind,
  id,
  value,
  onChange,
  emptyLabel,
  invalid,
  describedBy,
  required,
}: {
  kind: LookupKind;
  id: string;
  value: string;
  onChange: (value: string) => void;
  emptyLabel: string;
  invalid?: boolean;
  describedBy?: string;
  required?: boolean;
}) {
  const { data, isLoading, isError } = useLookup(kind);
  const options = (data ?? []).filter((o) => o.isActive || o.id === value);
  return (
    <select
      id={id}
      value={value}
      onChange={(e) => onChange(e.target.value)}
      aria-invalid={invalid || undefined}
      aria-describedby={describedBy}
      required={required}
      disabled={isLoading}
    >
      <option value="">{isLoading ? "Loading…" : isError ? "Could not load options" : emptyLabel}</option>
      {options.map((o) => (
        <option key={o.id} value={o.id}>
          {"  ".repeat(o.depth ?? 0)}
          {o.name}
          {o.isActive ? "" : " (retired)"}
        </option>
      ))}
    </select>
  );
}
