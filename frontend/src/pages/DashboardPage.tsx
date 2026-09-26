import { useQueries } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import { ciCountQuery, useCiClasses, useCiList, useLookup } from "../api/queries";
import { StatusBadge } from "../components/Badge";
import { Breadcrumbs } from "../components/Breadcrumbs";
import { EmptyState, ErrorAlert, Loading } from "../components/States";
import { formatRelative } from "../lib/format";
import { useDocumentTitle } from "../lib/hooks";

/**
 * Operational overview. Counts are server-side (each is a limit=1 list request
 * reading page.total), so they stay correct at any inventory size.
 */
export function DashboardPage() {
  useDocumentTitle("Dashboard");
  const total = useQueries({ queries: [ciCountQuery({})] })[0];
  const recent = useCiList({ sort: "-updatedAt", limit: 12 });

  if (total.isError) {
    return (
      <>
        <Breadcrumbs items={[]} />
        <div className="page-header">
          <h1>Dashboard</h1>
        </div>
        <ErrorAlert error={total.error} onRetry={() => total.refetch()} />
      </>
    );
  }

  return (
    <>
      <Breadcrumbs items={[]} />
      <div className="page-header">
        <div className="title">
          <h1>Dashboard</h1>
        </div>
        <div className="actions">
          <Link className="btn" to="/cis">
            Open inventory
          </Link>
          <Link className="btn btn-primary" to="/cis/new">
            + New CI
          </Link>
        </div>
      </div>

      {total.isLoading && <Loading />}
      {total.data === 0 && (
        <section className="panel">
          <EmptyState
            title="Welcome to ShadouCMDB — the inventory is empty"
            actions={
              <Link className="btn btn-primary" to="/cis/new">
                + Create your first configuration item
              </Link>
            }
          >
            Start with the things everything else depends on: a location, then the servers in it, then the applications
            and databases that run on them. Relate them from each CI's detail page.
          </EmptyState>
        </section>
      )}
      {total.data !== undefined && total.data > 0 && (
        <>
          <div className="kpis">
            <div className="kpi">
              <div className="value">{total.data.toLocaleString()}</div>
              <div className="label">Configuration items</div>
            </div>
          </div>
          <div className="grid-2">
            <CountsByClass total={total.data} />
            <CountsByStatus total={total.data} />
          </div>
          <div style={{ height: "var(--sp-4)" }} />
          <section className="panel">
            <div className="panel-header">
              <h2>Recently changed</h2>
              <Link to="/cis?sort=-updatedAt">View all</Link>
            </div>
            <div className="panel-body flush">
              {recent.isLoading && <Loading />}
              {recent.isError && (
                <div className="panel-body">
                  <ErrorAlert error={recent.error} onRetry={() => recent.refetch()} />
                </div>
              )}
              {recent.data && (
                <table className="data">
                  <thead>
                    <tr>
                      <th scope="col">Name</th>
                      <th scope="col">Class</th>
                      <th scope="col">Status</th>
                      <th scope="col">Environment</th>
                      <th scope="col">Owner</th>
                      <th scope="col">Changed</th>
                    </tr>
                  </thead>
                  <tbody>
                    {recent.data.data.map((ci) => (
                      <tr key={ci.id}>
                        <td>
                          <Link to={`/cis/${ci.id}`}>{ci.name}</Link>
                        </td>
                        <td>{ci.class.name}</td>
                        <td>
                          <StatusBadge status={ci.status} />
                        </td>
                        <td>{ci.environment?.name ?? ""}</td>
                        <td>{ci.owner?.name ?? ""}</td>
                        <td title={ci.updatedAt}>{formatRelative(ci.updatedAt)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </div>
          </section>
        </>
      )}
    </>
  );
}

function CountsByClass({ total }: { total: number }) {
  const classes = useCiClasses();
  const concrete = (classes.data ?? []).filter((c) => !c.isAbstract);
  const counts = useQueries({ queries: concrete.map((c) => ciCountQuery({ classId: c.id })) });
  return (
    <CountTable
      title="By class"
      loading={classes.isLoading}
      error={classes.error ?? counts.find((c) => c.error)?.error}
      rows={concrete.map((c, i) => ({
        id: c.id,
        label: c.name,
        count: counts[i]?.data,
        to: `/cis?classId=${c.id}`,
        extra: (
          <Link to={`/cis/new?classId=${c.id}`} aria-label={`New ${c.name}`}>
            + New
          </Link>
        ),
      }))}
      total={total}
    />
  );
}

function CountsByStatus({ total }: { total: number }) {
  const statuses = useLookup("statuses");
  const list = statuses.data ?? [];
  const counts = useQueries({ queries: list.map((s) => ciCountQuery({ statusId: s.id })) });
  return (
    <CountTable
      title="By status"
      loading={statuses.isLoading}
      error={statuses.error ?? counts.find((c) => c.error)?.error}
      rows={list.map((s, i) => ({
        id: s.id,
        label: s.name,
        count: counts[i]?.data,
        to: `/cis?statusId=${s.id}`,
      }))}
      total={total}
    />
  );
}

function CountTable({
  title,
  rows,
  total,
  loading,
  error,
}: {
  title: string;
  rows: { id: string; label: string; count: number | undefined; to: string; extra?: React.ReactNode }[];
  total: number;
  loading: boolean;
  error: unknown;
}) {
  const sorted = [...rows].sort((a, b) => (b.count ?? -1) - (a.count ?? -1) || a.label.localeCompare(b.label));
  return (
    <section className="panel">
      <div className="panel-header">
        <h2>{title}</h2>
      </div>
      <div className="panel-body flush">
        {loading && <Loading />}
        {error != null && (
          <div className="panel-body">
            <ErrorAlert error={error} title="Some counts could not be loaded" />
          </div>
        )}
        {!loading && (
          <table className="data">
            <tbody>
              {sorted.map((r) => (
                <tr key={r.id}>
                  <td style={{ width: "35%" }}>
                    <Link to={r.to}>{r.label}</Link>
                  </td>
                  <td className="num" style={{ width: 70 }}>
                    {r.count === undefined ? <span className="spinner" aria-label="Loading" /> : r.count.toLocaleString()}
                  </td>
                  <td>
                    <div className="bar" style={{ width: `${total && r.count ? Math.max(1, (r.count / total) * 100) : 0}%` }} aria-hidden="true" />
                  </td>
                  {rows.some((x) => x.extra) && <td className="num">{r.extra}</td>}
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </section>
  );
}
