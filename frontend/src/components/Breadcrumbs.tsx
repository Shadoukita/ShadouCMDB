import { Link } from "react-router-dom";

export interface Crumb {
  label: string;
  to?: string;
  state?: unknown;
}

export function Breadcrumbs({ items }: { items: Crumb[] }) {
  return (
    <nav className="breadcrumbs" aria-label="Breadcrumb">
      <ol>
        <li>
          <Link to="/">Dashboard</Link>
        </li>
        {items.map((c, i) => (
          <li key={i}>
            {c.to && i < items.length - 1 ? (
              <Link to={c.to} state={c.state}>
                {c.label}
              </Link>
            ) : (
              <span aria-current={i === items.length - 1 ? "page" : undefined}>{c.label}</span>
            )}
          </li>
        ))}
      </ol>
    </nav>
  );
}
