const SIZES = [25, 50, 100, 200];

export function Pagination({
  total,
  limit,
  offset,
  onChange,
}: {
  total: number;
  limit: number;
  offset: number;
  onChange: (next: { limit: number; offset: number }) => void;
}) {
  const from = total === 0 ? 0 : offset + 1;
  const to = Math.min(offset + limit, total);
  const page = Math.floor(offset / limit) + 1;
  const pages = Math.max(1, Math.ceil(total / limit));
  return (
    <div className="pagination">
      <span aria-live="polite">
        {from.toLocaleString()}–{to.toLocaleString()} of {total.toLocaleString()}
      </span>
      <div className="actions">
        <label>
          Rows{" "}
          <select value={limit} onChange={(e) => onChange({ limit: Number(e.target.value), offset: 0 })}>
            {SIZES.map((s) => (
              <option key={s} value={s}>
                {s}
              </option>
            ))}
          </select>
        </label>
        <button type="button" className="btn btn-sm" disabled={offset === 0} onClick={() => onChange({ limit, offset: 0 })}>
          « First
        </button>
        <button
          type="button"
          className="btn btn-sm"
          disabled={offset === 0}
          onClick={() => onChange({ limit, offset: Math.max(0, offset - limit) })}
        >
          ‹ Prev
        </button>
        <span>
          Page {page} / {pages}
        </span>
        <button
          type="button"
          className="btn btn-sm"
          disabled={offset + limit >= total}
          onClick={() => onChange({ limit, offset: offset + limit })}
        >
          Next ›
        </button>
        <button
          type="button"
          className="btn btn-sm"
          disabled={offset + limit >= total}
          onClick={() => onChange({ limit, offset: (pages - 1) * limit })}
        >
          Last »
        </button>
      </div>
    </div>
  );
}
