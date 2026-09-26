import { useEffect, useRef, useState } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import { useSearch } from "../api/queries";
import { useDebounced } from "../lib/hooks";

/**
 * Header search. Type-ahead shows the top ranked hits from GET /search;
 * Enter opens the full result page (/search?q=…). Press "/" anywhere to focus.
 */
export function GlobalSearch() {
  const navigate = useNavigate();
  const location = useLocation();
  const inputRef = useRef<HTMLInputElement>(null);
  const [text, setText] = useState("");
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(-1);
  const q = useDebounced(text.trim(), 200);
  const { data, isFetching, isError } = useSearch(q, 8);
  const hits = q ? (data?.data ?? []) : [];

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement;
      if (e.key === "/" && !["INPUT", "TEXTAREA", "SELECT"].includes(t.tagName) && !t.isContentEditable) {
        e.preventDefault();
        inputRef.current?.focus();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  useEffect(() => setOpen(false), [location.pathname, location.search]);

  const go = (path: string) => {
    setOpen(false);
    setText("");
    inputRef.current?.blur();
    navigate(path);
  };

  return (
    <form
      className="global-search combo"
      role="search"
      onSubmit={(e) => {
        e.preventDefault();
        if (active >= 0 && hits[active]) go(`/cis/${hits[active].item.id}`);
        else if (text.trim()) go(`/search?q=${encodeURIComponent(text.trim())}`);
      }}
    >
      <label htmlFor="global-search" className="sr-only">
        Search configuration items
      </label>
      <input
        ref={inputRef}
        id="global-search"
        type="search"
        role="combobox"
        aria-expanded={open && !!q}
        aria-controls="global-search-list"
        aria-activedescendant={active >= 0 ? `gs-${active}` : undefined}
        autoComplete="off"
        placeholder="Search CIs by name, hostname, IP, serial, attribute…  ( / )"
        value={text}
        onChange={(e) => {
          setText(e.target.value);
          setOpen(true);
          setActive(-1);
        }}
        onFocus={() => setOpen(true)}
        onBlur={() => setTimeout(() => setOpen(false), 150)}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown") {
            e.preventDefault();
            setActive((a) => Math.min(a + 1, hits.length - 1));
          } else if (e.key === "ArrowUp") {
            e.preventDefault();
            setActive((a) => Math.max(a - 1, -1));
          } else if (e.key === "Escape") {
            setOpen(false);
          }
        }}
      />
      {open && q && (
        <ul className="combo-list" id="global-search-list" role="listbox" style={{ width: "100%" }}>
          {isError && <li className="note">Search failed — press Enter to see the error</li>}
          {!isError && hits.length === 0 && <li className="note">{isFetching ? "Searching…" : `No CI matches “${q}”`}</li>}
          {hits.map((h, i) => (
            <li
              key={h.item.id}
              id={`gs-${i}`}
              role="option"
              aria-selected={i === active}
              onMouseDown={(e) => {
                e.preventDefault();
                go(`/cis/${h.item.id}`);
              }}
              onMouseEnter={() => setActive(i)}
            >
              <strong>{h.item.name}</strong>
              <span className="muted">{h.item.class.name}</span>
              {h.matches[0] && (
                <span className="muted">
                  {h.matches[0].label}: <span className="mono">{h.matches[0].value}</span>
                </span>
              )}
            </li>
          ))}
          {data && data.page.total > hits.length && (
            <li
              className="note"
              style={{ cursor: "pointer" }}
              onMouseDown={(e) => {
                e.preventDefault();
                go(`/search?q=${encodeURIComponent(q)}`);
              }}
            >
              See all {data.page.total.toLocaleString()} results ↵
            </li>
          )}
        </ul>
      )}
    </form>
  );
}
