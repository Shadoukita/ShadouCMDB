import { useAuditLog, type AuditEntry, type Ci } from "../../api/queries";
import { EmptyState, ErrorAlert, Loading } from "../../components/States";
import { formatDateTime } from "../../lib/format";

const HIDDEN = new Set(["updatedAt", "createdAt", "version", "classId", "statusId", "environmentId", "ownerId", "locationId", "attributeReferences"]);
/** Embedded references are shown by name instead of by id. */
const REFS = new Set(["class", "status", "environment", "owner", "location"]);

/** Change history from GET /audit-log?entityId=… with a field-level diff of each update. */
export function HistoryPanel({ ci }: { ci: Ci }) {
  const log = useAuditLog(ci.id);
  if (log.isLoading) return <Loading label="Loading history…" />;
  if (log.isError) return <ErrorAlert error={log.error} onRetry={() => log.refetch()} />;
  const entries = log.data?.data ?? [];
  if (entries.length === 0) return <EmptyState title="No recorded changes">This CI has no audit entries yet.</EmptyState>;
  return (
    <section className="panel">
      <div className="table-wrap">
        <table className="data">
          <thead>
            <tr>
              <th scope="col">When</th>
              <th scope="col">Action</th>
              <th scope="col">Actor</th>
              <th scope="col" style={{ width: "100%" }}>
                Changes
              </th>
            </tr>
          </thead>
          <tbody>
            {entries.map((e) => (
              <tr key={e.id} style={{ verticalAlign: "top" }}>
                <td style={{ paddingTop: 5 }}>{formatDateTime(e.occurredAt)}</td>
                <td style={{ paddingTop: 5 }}>
                  <span className={`badge ${e.action === "delete" ? "danger" : e.action === "create" ? "ok" : ""}`}>{e.action}</span>
                </td>
                <td style={{ paddingTop: 5 }}>
                  {e.actorName ?? <span className="muted">unknown</span>} <span className="muted">({e.actorType})</span>
                </td>
                <td style={{ whiteSpace: "normal", paddingTop: 5, paddingBottom: 5 }}>
                  <Changes entry={e} />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {log.data && log.data.page.total > entries.length && (
        <div className="pagination">Showing the latest {entries.length} of {log.data.page.total} entries.</div>
      )}
    </section>
  );
}

function Changes({ entry }: { entry: AuditEntry }) {
  const oldV = flatten(entry.oldValue);
  const newV = flatten(entry.newValue);
  if (entry.action === "create") return <span className="muted">Created</span>;
  if (entry.action === "delete") return <span className="muted">Deleted (relationships removed with it)</span>;
  const keys = [...new Set([...Object.keys(oldV), ...Object.keys(newV)])].filter((k) => !HIDDEN.has(k.split(".")[0]) && oldV[k] !== newV[k]);
  if (keys.length === 0) return <span className="muted">No visible field changes</span>;
  return (
    <ul className="diff">
      {keys.map((k) => (
        <li key={k}>
          <code>{k}</code>: {oldV[k] !== undefined && <del>{oldV[k]}</del>} → {newV[k] !== undefined ? <ins>{newV[k]}</ins> : <span className="muted">cleared</span>}
        </li>
      ))}
    </ul>
  );
}

function flatten(value: unknown, prefix = ""): Record<string, string> {
  const out: Record<string, string> = {};
  if (!value || typeof value !== "object") return out;
  for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
    const key = prefix ? `${prefix}.${k}` : k;
    if (v === null || v === undefined) continue;
    if (REFS.has(k) && typeof v === "object" && "name" in v) {
      out[key] = String((v as { name: unknown }).name);
      continue;
    }
    if (typeof v === "object" && !Array.isArray(v) && k === "attributes") Object.assign(out, flatten(v, key));
    else out[key] = typeof v === "object" ? JSON.stringify(v) : String(v);
  }
  return out;
}
