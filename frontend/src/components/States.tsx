import type { ReactNode } from "react";
import { ApiError } from "../api/client";

export function Loading({ label = "Loading…" }: { label?: string }) {
  return (
    <div className="state" role="status" aria-live="polite">
      <span className="spinner" aria-hidden="true" /> {label}
    </div>
  );
}

export function EmptyState({ title, children, actions }: { title: string; children?: ReactNode; actions?: ReactNode }) {
  return (
    <div className="state">
      <h2>{title}</h2>
      {children && <div>{children}</div>}
      {actions && <div className="actions">{actions}</div>}
    </div>
  );
}

/** Human-readable explanation for any thrown error, with the API's details when present. */
export function ErrorAlert({ error, title, onRetry }: { error: unknown; title?: string; onRetry?: () => void }) {
  const e = error instanceof ApiError ? error : null;
  const heading = title ?? headingFor(e);
  return (
    <div className="alert alert-error" role="alert">
      <strong>{heading}</strong>
      <div>{e ? e.message : error instanceof Error ? error.message : String(error)}</div>
      {e && e.details.length > 0 && (
        <ul>
          {e.details.map((d, i) => (
            <li key={i}>
              {d.field && d.field !== "(root)" && <code>{d.field}</code>} {d.message}
            </li>
          ))}
        </ul>
      )}
      {(e?.requestId || onRetry) && (
        <div className="meta">
          {e?.requestId && (
            <>
              Request id <code>{e.requestId}</code>{" "}
            </>
          )}
          {onRetry && (
            <button type="button" className="btn btn-sm" onClick={onRetry}>
              Retry
            </button>
          )}
        </div>
      )}
    </div>
  );
}

function headingFor(e: ApiError | null): string {
  if (!e) return "Something went wrong";
  switch (e.code) {
    case "NETWORK_ERROR":
      return "API unreachable";
    case "DATABASE_UNAVAILABLE":
      return "The CMDB database is unavailable";
    case "NOT_FOUND":
      return "Not found";
    case "VALIDATION_ERROR":
      return "The API rejected the request";
    case "VERSION_CONFLICT":
      return "Someone else changed this record";
    case "CONFLICT":
    case "IN_USE":
      return "Conflict";
    case "FORBIDDEN":
    case "UNAUTHORIZED":
      return "Permission denied";
    default:
      return `Request failed (${e.status || e.code})`;
  }
}
