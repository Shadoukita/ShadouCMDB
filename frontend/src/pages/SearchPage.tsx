import { Link, useSearchParams } from "react-router-dom";
import { useSearch } from "../api/queries";
import { StatusBadge } from "../components/Badge";
import { Breadcrumbs } from "../components/Breadcrumbs";
import { Pagination } from "../components/Pagination";
import { EmptyState, ErrorAlert, Loading } from "../components/States";
import { useDocumentTitle } from "../lib/hooks";

/** Full global-search results (ranked by the API), with the field that matched. State lives in the URL. */
export function SearchPage() {
  const [params, setParams] = useSearchParams();
  const q = params.get("q") ?? "";
  const limit = Number(params.get("limit")) || 50;
  const offset = Number(params.get("offset")) || 0;
  const search = useSearch(q, limit, offset);
  useDocumentTitle(q ? `Search: ${q}` : "Search");
  const rows = search.data?.data ?? [];

  return (
    <>
      <Breadcrumbs items={[{ label: "Search" }]} />
      <div className="page-header">
        <div className="title">
          <h1>{q ? <>Results for “{q}”</> : "Search"}</h1>
          {search.data && <span className="muted">{search.data.page.total.toLocaleString()} matches</span>}
        </div>
        {q && (
          <Link className="btn" to={`/cis?q=${encodeURIComponent(q)}`}>
            Open as filterable inventory
          </Link>
        )}
      </div>
      <section className="panel">
        {!q && <EmptyState title="Type in the search box above">Search covers names, hostnames, IPs and networks, serial numbers, notes and attribute values.</EmptyState>}
        {search.isLoading && <Loading label="Searching…" />}
        {search.isError && (
          <div className="panel-body">
            <ErrorAlert error={search.error} onRetry={() => search.refetch()} />
          </div>
        )}
        {search.data && rows.length === 0 && <EmptyState title={`No configuration item matches “${q}”`}>Try a shorter term, an IP address, or a CIDR like 10.0.0.0/24.</EmptyState>}
        {rows.length > 0 && (
          <>
            <div className="table-wrap">
              <table className={`data ${search.isPlaceholderData ? "loading" : ""}`}>
                <thead>
                  <tr>
                    <th scope="col">Name</th>
                    <th scope="col">Class</th>
                    <th scope="col">Status</th>
                    <th scope="col">Matched on</th>
                    <th scope="col">Hostname</th>
                    <th scope="col">IP address</th>
                  </tr>
                </thead>
                <tbody>
                  {rows.map(({ item, matches }) => (
                    <tr key={item.id}>
                      <td>
                        <Link to={`/cis/${item.id}`}>{item.name}</Link>
                      </td>
                      <td>{item.class.name}</td>
                      <td>
                        <StatusBadge status={item.status} />
                      </td>
                      <td title={matches.map((m) => `${m.label}: ${m.value}`).join("\n")}>
                        {matches.slice(0, 2).map((m, i) => (
                          <span key={i}>
                            {i > 0 && ", "}
                            <span className="muted">{m.label}:</span> <span className="mono">{m.value}</span>
                          </span>
                        ))}
                      </td>
                      <td className="mono">{item.hostname ?? ""}</td>
                      <td className="mono">{item.ipAddress ?? ""}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            <Pagination
              total={search.data!.page.total}
              limit={limit}
              offset={offset}
              onChange={(p) => setParams({ q, ...(p.limit !== 50 ? { limit: String(p.limit) } : {}), ...(p.offset ? { offset: String(p.offset) } : {}) })}
            />
          </>
        )}
      </section>
    </>
  );
}
