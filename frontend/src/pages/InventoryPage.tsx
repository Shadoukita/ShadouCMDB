import { useEffect, useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { useCiClasses, useCiList, type CiListQuery } from "../api/queries";
import { StatusBadge } from "../components/Badge";
import { Breadcrumbs } from "../components/Breadcrumbs";
import { LookupSelect } from "../components/LookupSelect";
import { Pagination } from "../components/Pagination";
import { EmptyState, ErrorAlert, Loading } from "../components/States";
import { formatRelative } from "../lib/format";
import { useDebounced, useDocumentTitle } from "../lib/hooks";

type SortField = NonNullable<CiListQuery["sort"]>;

const FILTER_KEYS = ["q", "classId", "statusId", "environmentId", "ownerId", "locationId", "deleted"] as const;
const DEFAULT_LIMIT = 50;

const COLUMNS: { key: string; label: string; sort?: string }[] = [
  { key: "name", label: "Name", sort: "name" },
  { key: "class", label: "Class", sort: "className" },
  { key: "status", label: "Status", sort: "statusName" },
  { key: "environment", label: "Environment" },
  { key: "owner", label: "Owner" },
  { key: "location", label: "Location" },
  { key: "hostname", label: "Hostname", sort: "hostname" },
  { key: "ip", label: "IP address", sort: "ipAddress" },
  { key: "serial", label: "Serial", sort: "serialNumber" },
  { key: "updated", label: "Updated", sort: "updatedAt" },
];

/**
 * CI inventory. Every filter, the sort and the page live in the URL
 * (/cis?classId=…&statusId=…&q=…&sort=-updatedAt&offset=50), so a view survives
 * reload and can be bookmarked or shared. Filtering and paging happen in the API.
 */
export function InventoryPage() {
  const [params, setParams] = useSearchParams();
  const get = (k: string) => params.get(k) ?? "";
  const limit = clampInt(params.get("limit"), DEFAULT_LIMIT, 1, 200);
  const offset = clampInt(params.get("offset"), 0, 0, Number.MAX_SAFE_INTEGER);
  const sort = get("sort") || "name";
  const deleted = get("deleted") === "include" ? "include" : get("deleted") === "only" ? "only" : undefined;

  const query: CiListQuery = {
    q: get("q") || undefined,
    classId: get("classId") || undefined,
    statusId: get("statusId") || undefined,
    environmentId: get("environmentId") || undefined,
    ownerId: get("ownerId") || undefined,
    locationId: get("locationId") || undefined,
    deleted,
    sort: sort as SortField,
    limit,
    offset,
  };
  const list = useCiList(query);
  const classes = useCiClasses();
  const currentClass = classes.data?.find((c) => c.id === query.classId);
  useDocumentTitle(currentClass ? currentClass.name : "Inventory");

  const update = (patch: Record<string, string | undefined>, resetPage = true) => {
    const next = new URLSearchParams(params);
    for (const [k, v] of Object.entries(patch)) {
      if (v) next.set(k, v);
      else next.delete(k);
    }
    if (resetPage) next.delete("offset");
    setParams(next, { replace: "q" in patch });
  };

  // Search box: local state for typing, debounced into the URL.
  const [qText, setQText] = useState(get("q"));
  const debouncedQ = useDebounced(qText, 300);
  useEffect(() => {
    if (debouncedQ !== get("q")) update({ q: debouncedQ || undefined });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [debouncedQ]);
  useEffect(() => setQText(get("q")), [params.get("q")]); // back/forward

  const activeFilters = FILTER_KEYS.filter((k) => params.get(k));
  const total = list.data?.page.total ?? 0;
  const rows = list.data?.data ?? [];

  const toggleSort = (field: string) => update({ sort: sort === field ? `-${field}` : field }, true);

  return (
    <>
      <Breadcrumbs items={currentClass ? [{ label: "Inventory", to: "/cis" }, { label: currentClass.name }] : [{ label: "Inventory" }]} />
      <div className="page-header">
        <div className="title">
          <h1>{currentClass ? currentClass.name : "Configuration items"}</h1>
          {list.data && <span className="muted">{total.toLocaleString()} total</span>}
          {list.isFetching && !list.isLoading && <span className="spinner" aria-label="Refreshing" />}
        </div>
        <div className="actions">
          <Link className="btn btn-primary" to={query.classId && !currentClass?.isAbstract ? `/cis/new?classId=${query.classId}` : "/cis/new"}>
            + New {currentClass && !currentClass.isAbstract ? currentClass.name : "CI"}
          </Link>
        </div>
      </div>

      <section className="panel" aria-label="Inventory">
        <form className="toolbar" role="search" onSubmit={(e) => e.preventDefault()}>
          <div className="field search">
            <label htmlFor="f-q">Search</label>
            <input id="f-q" type="search" placeholder="Name, hostname, IP, serial, notes…" value={qText} onChange={(e) => setQText(e.target.value)} />
          </div>
          <div className="field">
            <label htmlFor="f-class">Class</label>
            <select id="f-class" value={get("classId")} onChange={(e) => update({ classId: e.target.value || undefined })}>
              <option value="">All classes</option>
              {(classes.data ?? []).map((c) => (
                <option key={c.id} value={c.id}>
                  {c.name}
                  {c.isAbstract ? " (incl. subclasses)" : ""}
                </option>
              ))}
            </select>
          </div>
          <div className="field">
            <label htmlFor="f-status">Status</label>
            <LookupSelect kind="statuses" id="f-status" value={get("statusId")} onChange={(v) => update({ statusId: v || undefined })} emptyLabel="Any status" />
          </div>
          <div className="field">
            <label htmlFor="f-env">Environment</label>
            <LookupSelect kind="environments" id="f-env" value={get("environmentId")} onChange={(v) => update({ environmentId: v || undefined })} emptyLabel="Any environment" />
          </div>
          <div className="field">
            <label htmlFor="f-owner">Owner</label>
            <LookupSelect kind="owners" id="f-owner" value={get("ownerId")} onChange={(v) => update({ ownerId: v || undefined })} emptyLabel="Any owner" />
          </div>
          <div className="field">
            <label htmlFor="f-location">Location</label>
            <LookupSelect kind="locations" id="f-location" value={get("locationId")} onChange={(v) => update({ locationId: v || undefined })} emptyLabel="Any location" />
          </div>
          <div className="field">
            <label htmlFor="f-deleted">Deleted CIs</label>
            <select id="f-deleted" value={deleted ?? ""} onChange={(e) => update({ deleted: e.target.value || undefined })}>
              <option value="">Hide</option>
              <option value="include">Include</option>
              <option value="only">Only deleted</option>
            </select>
          </div>
          {activeFilters.length > 0 && (
            <button
              type="button"
              className="btn"
              onClick={() => {
                setQText("");
                const next = new URLSearchParams();
                if (params.get("sort")) next.set("sort", params.get("sort")!);
                if (params.get("limit")) next.set("limit", params.get("limit")!);
                setParams(next);
              }}
            >
              Clear filters
            </button>
          )}
        </form>

        {list.isError && <div className="panel-body"><ErrorAlert error={list.error} onRetry={() => list.refetch()} /></div>}
        {list.isLoading && <Loading label="Loading inventory…" />}

        {list.data && total === 0 && activeFilters.length === 0 && (
          <EmptyState
            title="The inventory is empty"
            actions={
              <Link className="btn btn-primary" to="/cis/new">
                + Create your first configuration item
              </Link>
            }
          >
            Configuration items are the servers, VMs, applications, databases, network devices and locations you track.
            Create one, then relate it to others from its detail page.
          </EmptyState>
        )}
        {list.data && total === 0 && activeFilters.length > 0 && (
          <EmptyState title="No configuration items match these filters">Adjust or clear the filters above.</EmptyState>
        )}
        {list.data && total > 0 && rows.length === 0 && (
          <EmptyState title="This page is past the end of the results" actions={<button className="btn" onClick={() => update({}, true)}>Go to first page</button>} />
        )}

        {rows.length > 0 && (
          <>
            <div className="table-wrap">
              <table className={`data ${list.isPlaceholderData ? "loading" : ""}`}>
                <thead>
                  <tr>
                    {COLUMNS.map((c) => (
                      <th key={c.key} scope="col" aria-sort={c.sort ? ariaSort(sort, c.sort) : undefined}>
                        {c.sort ? (
                          <button type="button" className="sort" onClick={() => toggleSort(c.sort!)}>
                            {c.label} {sortIndicator(sort, c.sort)}
                          </button>
                        ) : (
                          c.label
                        )}
                      </th>
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {rows.map((ci) => (
                    <tr key={ci.id} className={ci.deletedAt ? "deleted" : undefined}>
                      <td>
                        <Link to={`/cis/${ci.id}`}>{ci.name}</Link>
                      </td>
                      <td>{ci.class.name}</td>
                      <td>
                        {ci.deletedAt ? <span className="badge danger">Deleted</span> : <StatusBadge status={ci.status} />}
                      </td>
                      <td>{ci.environment?.name ?? <span className="muted">—</span>}</td>
                      <td>{ci.owner?.name ?? <span className="muted">—</span>}</td>
                      <td>{ci.location?.name ?? <span className="muted">—</span>}</td>
                      <td className="mono">{ci.hostname ?? ""}</td>
                      <td className="mono">{ci.ipAddress ?? ""}</td>
                      <td className="mono">{ci.serialNumber ?? ""}</td>
                      <td title={ci.updatedAt}>{formatRelative(ci.updatedAt)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            <Pagination
              total={total}
              limit={limit}
              offset={offset}
              onChange={(p) => update({ limit: p.limit === DEFAULT_LIMIT ? undefined : String(p.limit), offset: p.offset ? String(p.offset) : undefined }, false)}
            />
          </>
        )}
      </section>
    </>
  );
}

function clampInt(raw: string | null, fallback: number, min: number, max: number): number {
  const n = raw ? Number.parseInt(raw, 10) : NaN;
  return Number.isFinite(n) ? Math.min(max, Math.max(min, n)) : fallback;
}

function sortIndicator(sort: string, field: string): string {
  if (sort === field) return "▲";
  if (sort === `-${field}`) return "▼";
  return "";
}

function ariaSort(sort: string, field: string): "ascending" | "descending" | "none" {
  if (sort === field) return "ascending";
  if (sort === `-${field}`) return "descending";
  return "none";
}
