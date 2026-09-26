import { useEffect, useState } from "react";
import { Link, useLocation, useNavigate, useParams } from "react-router-dom";
import { ApiError } from "../api/client";
import { useCi, useClassAttributes, useDeleteCi, useRelationships, type Ci, type EffectiveAttribute } from "../api/queries";
import { StatusBadge } from "../components/Badge";
import { Breadcrumbs, type Crumb } from "../components/Breadcrumbs";
import { CiLink } from "../components/CiLink";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { EmptyState, ErrorAlert, Loading } from "../components/States";
import { formatDate, formatDateTime, plural } from "../lib/format";
import { groupAttributes } from "../lib/attributes";
import { useDocumentTitle } from "../lib/hooks";
import { useTrail, type TrailStep } from "../lib/trail";
import { HistoryPanel } from "./detail/HistoryPanel";
import { RelationshipGraphPanel } from "./detail/RelationshipGraphPanel";
import { RelationshipsPanel, describeEdge } from "./detail/RelationshipsPanel";

type Tab = "overview" | "graph" | "history";

export function CiDetailPage() {
  const { id = "" } = useParams();
  const location = useLocation();
  const trail = useTrail();
  const ci = useCi(id);
  const [tab, setTab] = useState<Tab>("overview");
  const navigate = useNavigate();
  const flash = (location.state as { flash?: string } | null)?.flash;
  useDocumentTitle(ci.data?.name);
  // Show a save/create confirmation once, then drop it from history state so reload/back do not repeat it.
  useEffect(() => {
    if (!flash) return;
    const t = setTimeout(() => navigate(location.pathname, { replace: true, state: { trail } }), 6000);
    return () => clearTimeout(t);
  }, [flash, location.pathname, navigate, trail]);

  if (ci.isLoading) return <Loading label="Loading configuration item…" />;
  if (ci.isError) {
    // A malformed id in the URL is rejected by the API as a params validation error; for the operator it is simply "not found".
    const notFound =
      ci.error instanceof ApiError &&
      (ci.error.code === "NOT_FOUND" || (ci.error.code === "VALIDATION_ERROR" && ci.error.details.some((d) => d.in === "params")));
    return (
      <>
        <Breadcrumbs items={[{ label: "Inventory", to: "/cis" }, { label: "Not found" }]} />
        {notFound ? (
          <EmptyState title="Configuration item not found" actions={<Link className="btn" to="/cis">Back to inventory</Link>}>
            No CI has the id <code>{id}</code>. It may have been removed, or the link is wrong.
          </EmptyState>
        ) : (
          <ErrorAlert error={ci.error} onRetry={() => ci.refetch()} />
        )}
      </>
    );
  }
  if (!ci.data) return null;
  const c = ci.data;
  const self: TrailStep = { id: c.id, name: c.name };

  const crumbs: Crumb[] = [{ label: "Inventory", to: "/cis" }];
  if (trail.length > 0) {
    trail.forEach((s, i) => crumbs.push({ label: s.name, to: `/cis/${s.id}`, state: { trail: trail.slice(0, i) } }));
  } else {
    crumbs.push({ label: c.class.name, to: `/cis?classId=${c.classId}` });
  }
  crumbs.push({ label: c.name });

  return (
    <>
      <Breadcrumbs items={crumbs} />
      <div className="page-header">
        <div className="title">
          <h1>{c.name}</h1>
          <Link to={`/cis?classId=${c.classId}`} className="badge">
            {c.class.name}
          </Link>
          {c.deletedAt ? <span className="badge danger">Deleted {formatDateTime(c.deletedAt)}</span> : <StatusBadge status={c.status} />}
          {c.environment && <span className="badge">{c.environment.name}</span>}
        </div>
        {!c.deletedAt && (
          <div className="actions">
            <Link className="btn" to={`/cis/${c.id}/edit`}>
              Edit
            </Link>
            <DeleteCiButton ci={c} />
          </div>
        )}
      </div>
      {flash && (
        <div className="alert" role="status">
          {flash}
        </div>
      )}
      {c.deletedAt && (
        <div className="alert alert-warn">
          This CI was deleted on {formatDateTime(c.deletedAt)}. It is kept read-only for history; its relationships were removed with it.
        </div>
      )}

      <div className="tabs" role="tablist" aria-label="CI sections">
        {(
          [
            ["overview", "Overview"],
            ["graph", "Relationship map"],
            ["history", "History"],
          ] as const
        ).map(([key, label]) => (
          <button key={key} type="button" role="tab" id={`tab-${key}`} aria-selected={tab === key} aria-controls={`panel-${key}`} onClick={() => setTab(key)}>
            {label}
          </button>
        ))}
      </div>

      <div role="tabpanel" id={`panel-${tab}`} aria-labelledby={`tab-${tab}`}>
        {tab === "overview" && (
          <>
            <div className="grid-2">
              <CorePanel ci={c} />
              <AttributesPanel ci={c} self={self} trail={trail} />
            </div>
            <div style={{ height: "var(--sp-4)" }} />
            <RelationshipsPanel ci={c} self={self} trail={trail} />
          </>
        )}
        {tab === "graph" && <RelationshipGraphPanel ci={c} self={self} trail={trail} />}
        {tab === "history" && <HistoryPanel ci={c} />}
      </div>
    </>
  );
}

function CorePanel({ ci }: { ci: Ci }) {
  const v = (x: string | null | undefined, mono = false) => (x ? mono ? <span className="mono">{x}</span> : x : <span className="muted">—</span>);
  return (
    <section className="panel">
      <div className="panel-header">
        <h2>General</h2>
      </div>
      <div className="panel-body">
        <dl className="props">
          <dt>Class</dt>
          <dd>
            <Link to={`/cis?classId=${ci.classId}`}>{ci.class.name}</Link>
          </dd>
          <dt>Status</dt>
          <dd>
            <Link to={`/cis?statusId=${ci.statusId}`}>{ci.status.name}</Link>
          </dd>
          <dt>Environment</dt>
          <dd>{ci.environment ? <Link to={`/cis?environmentId=${ci.environment.id}`}>{ci.environment.name}</Link> : v(null)}</dd>
          <dt>Owner</dt>
          <dd>{ci.owner ? <Link to={`/cis?ownerId=${ci.owner.id}`}>{ci.owner.name}</Link> : v(null)}</dd>
          <dt>Location</dt>
          <dd>
            {ci.location ? (
              <Link to={`/cis?locationId=${ci.location.id}`} title="All CIs at this location">
                {ci.location.name}
              </Link>
            ) : (
              v(null)
            )}
          </dd>
          <dt>Hostname</dt>
          <dd>{v(ci.hostname, true)}</dd>
          <dt>IP address</dt>
          <dd>{v(ci.ipAddress, true)}</dd>
          <dt>Serial number</dt>
          <dd>{v(ci.serialNumber, true)}</dd>
          <dt>Notes</dt>
          <dd style={{ whiteSpace: "pre-wrap" }}>{v(ci.notes)}</dd>
          <dt>Created</dt>
          <dd>{formatDateTime(ci.createdAt)}</dd>
          <dt>Updated</dt>
          <dd>
            {formatDateTime(ci.updatedAt)} <span className="muted">· version {ci.version}</span>
          </dd>
          <dt>ID</dt>
          <dd className="mono">{ci.id}</dd>
        </dl>
      </div>
    </section>
  );
}

function AttributesPanel({ ci, self, trail }: { ci: Ci; self: TrailStep; trail: TrailStep[] }) {
  const attrs = useClassAttributes(ci.classId);
  const values = ci.attributes as Record<string, unknown>;
  const refs = ci.attributeReferences as Record<string, { id: string; name: string; deleted?: boolean } | undefined>;
  const defs = (attrs.data ?? []).filter((d) => d.isActive || values[d.key] != null);
  const groups = groupAttributes(defs);
  const known = new Set(defs.map((d) => d.key));
  const orphans = Object.keys(values).filter((k) => !known.has(k));

  return (
    <section className="panel">
      <div className="panel-header">
        <h2>{ci.class.name} attributes</h2>
      </div>
      <div className="panel-body">
        {attrs.isLoading && <Loading label="Loading attribute definitions…" />}
        {attrs.isError && <ErrorAlert error={attrs.error} title="Could not load attribute definitions" onRetry={() => attrs.refetch()} />}
        {attrs.data && defs.length === 0 && orphans.length === 0 && <p className="muted">This class defines no extra attributes.</p>}
        {attrs.data && (defs.length > 0 || orphans.length > 0) && (
          <dl className="props">
            {groups.map(([g, items]) => (
              <GroupRows key={g} title={g}>
                {items.map((d) => (
                  <Row key={d.key} label={d.label}>
                    <AttributeValue def={d} value={values[d.key]} refInfo={refs[d.key]} self={self} trail={trail} />
                  </Row>
                ))}
              </GroupRows>
            ))}
            {orphans.length > 0 && (
              <GroupRows title="Not defined by this class">
                {orphans.map((k) => (
                  <Row key={k} label={k}>
                    <span className="mono">{JSON.stringify(values[k])}</span>
                  </Row>
                ))}
              </GroupRows>
            )}
          </dl>
        )}
      </div>
    </section>
  );
}

function GroupRows({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <>
      <div className="group-title">{title}</div>
      {children}
    </>
  );
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <>
      <dt>{label}</dt>
      <dd>{children}</dd>
    </>
  );
}

function AttributeValue({
  def,
  value,
  refInfo,
  self,
  trail,
}: {
  def: EffectiveAttribute;
  value: unknown;
  refInfo?: { id: string; name: string; deleted?: boolean };
  self: TrailStep;
  trail: TrailStep[];
}) {
  if (value === null || value === undefined || value === "") return <span className="muted">—</span>;
  switch (def.dataType) {
    case "boolean":
      return <>{value ? "Yes" : "No"}</>;
    case "date":
      return <>{formatDate(String(value))}</>;
    case "datetime":
      return <>{formatDateTime(String(value))}</>;
    case "ip":
    case "cidr":
      return <span className="mono">{String(value)}</span>;
    case "reference":
      return (
        <CiLink id={String(value)} from={self} trail={trail}>
          {refInfo?.name ?? String(value)}
          {refInfo?.deleted ? " (deleted)" : ""}
        </CiLink>
      );
    case "text":
      return /^https?:\/\//.test(String(value)) ? (
        <a href={String(value)} target="_blank" rel="noreferrer noopener">
          {String(value)}
        </a>
      ) : (
        <>{String(value)}</>
      );
    default:
      return <>{String(value)}</>;
  }
}

function DeleteCiButton({ ci }: { ci: Ci }) {
  const navigate = useNavigate();
  const [open, setOpen] = useState(false);
  const rels = useRelationships(ci.id);
  const del = useDeleteCi();
  const edges = rels.data?.data ?? [];
  const total = rels.data?.page.total ?? 0;

  return (
    <>
      <button type="button" className="btn btn-danger" onClick={() => setOpen(true)}>
        Delete
      </button>
      <ConfirmDialog
        open={open}
        title={`Delete ${ci.class.name.toLowerCase()} “${ci.name}”?`}
        confirmLabel={total > 0 ? `Delete CI and ${plural(total, "relationship")}` : "Delete CI"}
        busy={del.isPending}
        onCancel={() => {
          del.reset();
          setOpen(false);
        }}
        onConfirm={() =>
          del.mutate(ci.id, {
            onSuccess: () => navigate("/cis", { replace: true }),
          })
        }
      >
        {del.isError && <ErrorAlert error={del.error} title="Delete failed" />}
        <p>
          <strong>{ci.name}</strong> ({ci.class.name}
          {ci.hostname ? `, ${ci.hostname}` : ""}) will be removed from the inventory. The record and its history stay
          available as a deleted CI.
        </p>
        {rels.isLoading && <Loading label="Checking relationships…" />}
        {rels.isError && <ErrorAlert error={rels.error} title="Could not check which relationships would break" />}
        {rels.data && total === 0 && <p>It has no relationships, so no other CI is affected.</p>}
        {rels.data && total > 0 && (
          <>
            <p>
              These <strong>{plural(total, "relationship")}</strong> will break:
            </p>
            <ul>
              {edges.map((r) => {
                const d = describeEdge(r, ci.id);
                return (
                  <li key={r.id}>
                    {ci.name} <em>{d.label}</em> <strong>{d.other.name}</strong> <span className="muted">({d.other.className})</span>
                  </li>
                );
              })}
            </ul>
            {total > edges.length && <p className="muted">…and {total - edges.length} more.</p>}
          </>
        )}
      </ConfirmDialog>
    </>
  );
}
