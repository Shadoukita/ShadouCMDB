import { useId, useState } from "react";
import { useCiList, type CiSummary } from "../api/queries";
import { useDebounced } from "../lib/hooks";

/**
 * Type-ahead picker for a configuration item. Queries the API server-side
 * (optionally restricted to a class and its subclasses); never loads the inventory.
 */
export function CiPicker({
  id,
  classId,
  excludeId,
  selected,
  onSelect,
  placeholder = "Search by name, hostname, IP…",
  invalid,
  describedBy,
}: {
  id?: string;
  classId?: string | null;
  excludeId?: string;
  selected: { id: string; name: string } | null;
  onSelect: (ci: CiSummary | null) => void;
  placeholder?: string;
  invalid?: boolean;
  describedBy?: string;
}) {
  const autoId = useId();
  const inputId = id ?? autoId;
  const listId = `${inputId}-list`;
  const [text, setText] = useState("");
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const q = useDebounced(text.trim(), 200);
  const { data, isFetching, isError } = useCiList({
    q: q || undefined,
    classId: classId || undefined,
    limit: 15,
    sort: "name",
  });
  const items = (data?.data ?? []).filter((c) => c.id !== excludeId);

  if (selected) {
    return (
      <div className="checkbox-row">
        <strong>{selected.name}</strong>
        <button type="button" className="btn btn-sm" onClick={() => onSelect(null)} aria-label={`Clear ${selected.name}`}>
          Change
        </button>
      </div>
    );
  }

  const choose = (ci: CiSummary) => {
    onSelect(ci);
    setText("");
    setOpen(false);
  };

  return (
    <div className="combo">
      <input
        id={inputId}
        type="search"
        role="combobox"
        aria-expanded={open}
        aria-controls={listId}
        aria-autocomplete="list"
        aria-activedescendant={open && items[active] ? `${listId}-${active}` : undefined}
        aria-invalid={invalid || undefined}
        aria-describedby={describedBy}
        autoComplete="off"
        placeholder={placeholder}
        value={text}
        onChange={(e) => {
          setText(e.target.value);
          setOpen(true);
          setActive(0);
        }}
        onFocus={() => setOpen(true)}
        onBlur={() => setTimeout(() => setOpen(false), 150)}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown") {
            e.preventDefault();
            setOpen(true);
            setActive((a) => Math.min(a + 1, items.length - 1));
          } else if (e.key === "ArrowUp") {
            e.preventDefault();
            setActive((a) => Math.max(a - 1, 0));
          } else if (e.key === "Enter" && open && items[active]) {
            e.preventDefault();
            choose(items[active]);
          } else if (e.key === "Escape") {
            setOpen(false);
          }
        }}
        style={{ width: 280 }}
      />
      {open && (
        <ul className="combo-list" id={listId} role="listbox">
          {isError && <li className="note">Search failed</li>}
          {!isError && items.length === 0 && <li className="note">{isFetching ? "Searching…" : "No matching CIs"}</li>}
          {items.map((ci, i) => (
            <li
              key={ci.id}
              id={`${listId}-${i}`}
              role="option"
              aria-selected={i === active}
              onMouseDown={(e) => {
                e.preventDefault();
                choose(ci);
              }}
              onMouseEnter={() => setActive(i)}
            >
              <span>{ci.name}</span>
              <span className="muted">{ci.class.name}</span>
              {ci.hostname && <span className="muted mono">{ci.hostname}</span>}
            </li>
          ))}
          {data && data.page.total > items.length && (
            <li className="note">
              {data.page.total - items.length} more — keep typing to narrow down
            </li>
          )}
        </ul>
      )}
    </div>
  );
}
