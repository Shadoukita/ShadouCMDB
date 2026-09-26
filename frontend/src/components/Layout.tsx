import { useQueries } from "@tanstack/react-query";
import { Link, NavLink, Outlet, useLocation } from "react-router-dom";
import { ciCountQuery, useCiClasses } from "../api/queries";
import { GlobalSearch } from "./GlobalSearch";

export function Layout() {
  return (
    <div className="shell">
      <div className="shell-brand">
        <Link to="/">ShadouCMDB</Link>
      </div>
      <header className="shell-header">
        <GlobalSearch />
        <div className="actions" style={{ marginLeft: "auto" }}>
          <Link className="btn btn-primary" to="/cis/new">
            + New CI
          </Link>
        </div>
      </header>
      <nav className="shell-nav" aria-label="Main">
        <NavLink to="/" end>
          Dashboard
        </NavLink>
        <InventoryNavLink />
        <ClassNav />
      </nav>
      <main className="shell-main" id="main">
        <Outlet />
      </main>
    </div>
  );
}

function InventoryNavLink() {
  const { pathname, search } = useLocation();
  const params = new URLSearchParams(search);
  const active = pathname === "/cis" && !params.get("classId");
  return (
    <Link to="/cis" className={active ? "active" : undefined} aria-current={active ? "page" : undefined}>
      All configuration items
    </Link>
  );
}

/** "Browse by class" is built from the API's class list, so new classes appear automatically. */
function ClassNav() {
  const { pathname, search } = useLocation();
  const currentClass = pathname === "/cis" ? new URLSearchParams(search).get("classId") : null;
  const classes = useCiClasses();
  const concrete = (classes.data ?? []).filter((c) => c.isActive && !c.isAbstract);
  const counts = useQueries({ queries: concrete.map((c) => ciCountQuery({ classId: c.id })) });
  if (classes.isError) return <h2>Classes unavailable</h2>;
  return (
    <>
      <h2>Browse by class</h2>
      {concrete.map((c, i) => (
        <Link
          key={c.id}
          to={`/cis?classId=${c.id}`}
          className={currentClass === c.id ? "active" : undefined}
          aria-current={currentClass === c.id ? "page" : undefined}
        >
          <span>{c.name}</span>
          <span className="muted">{counts[i]?.data ?? ""}</span>
        </Link>
      ))}
    </>
  );
}
