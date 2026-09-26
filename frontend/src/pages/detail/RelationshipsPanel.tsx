import { useState, type FormEvent } from "react";
import { ApiError } from "../../api/client";
import {
  useCreateRelationship,
  useDeleteRelationship,
  useRelationships,
  useRelationshipTypes,
  type Ci,
  type CiSummary,
  type Relationship,
} from "../../api/queries";
import { CiLink } from "../../components/CiLink";
import { CiPicker } from "../../components/CiPicker";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { EmptyState, ErrorAlert, Loading } from "../../components/States";
import type { TrailStep } from "../../lib/trail";

/** An edge seen from one CI: "runs on X" when it is the source, "hosts X" when it is the target. */
export function describeEdge(r: Relationship, ciId: string) {
  const outgoing = r.sourceCiId === ciId;
  return {
    outgoing,
    label: outgoing || !r.type.isDirectional ? r.type.forwardLabel : r.type.reverseLabel,
    other: outgoing ? r.target : r.source,
  };
}

export function RelationshipsPanel({ ci, self, trail }: { ci: Ci; self: TrailStep; trail: TrailStep[] }) {
  const rels = useRelationships(ci.id);
  const [removing, setRemoving] = useState<Relationship | null>(null);
  const del = useDeleteRelationship();
  const rows = [...(rels.data?.data ?? [])].sort((a, b) => {
    const da = describeEdge(a, ci.id);
    const db = describeEdge(b, ci.id);
    return da.label.localeCompare(db.label) || da.other.name.localeCompare(db.other.name);
  });

  return (
    <section className="panel" aria-labelledby="rel-title">
      <div className="panel-header">
        <h2 id="rel-title">Relationships</h2>
        {rels.data && <span className="muted">{rels.data.page.total} direct</span>}
      </div>
      <div className="panel-body flush">
        {rels.isLoading && <Loading label="Loading relationships…" />}
        {rels.isError && (
          <div className="panel-body">
            <ErrorAlert error={rels.error} onRetry={() => rels.refetch()} />
          </div>
        )}
        {rels.data && rows.length === 0 && (
          <EmptyState title="No relationships yet">
            {ci.deletedAt ? "Deleted CIs keep no live relationships." : "Relate this CI to the things it runs on, depends on, or is located in using the form below."}
          </EmptyState>
        )}
        {rows.length > 0 && (
          <div className="table-wrap">
            <table className="data">
              <thead>
                <tr>
                  <th scope="col">This CI…</th>
                  <th scope="col">Related CI</th>
                  <th scope="col">Class</th>
                  <th scope="col">Direction</th>
                  <th scope="col">Notes</th>
                  {!ci.deletedAt && (
                    <th scope="col">
                      <span className="sr-only">Actions</span>
                    </th>
                  )}
                </tr>
              </thead>
              <tbody>
                {rows.map((r) => {
                  const d = describeEdge(r, ci.id);
                  return (
                    <tr key={r.id}>
                      <td>{d.label}</td>
                      <td>
                        <CiLink id={d.other.id} from={self} trail={trail}>
                          {d.other.name}
                        </CiLink>
                        {d.other.deleted && <span className="badge danger"> deleted</span>}
                      </td>
                      <td>{d.other.className}</td>
                      <td className="muted">{r.type.isDirectional ? (d.outgoing ? "outgoing →" : "← incoming") : "↔"}</td>
                      <td title={r.notes ?? undefined}>{r.notes ?? ""}</td>
                      {!ci.deletedAt && (
                        <td className="num">
                          <button type="button" className="btn-link danger" onClick={() => setRemoving(r)} aria-label={`Remove relationship: ${ci.name} ${d.label} ${d.other.name}`}>
                            Remove
                          </button>
                        </td>
                      )}
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
        {!ci.deletedAt && <AddRelationshipForm ci={ci} />}
      </div>
      <ConfirmDialog
        open={!!removing}
        title="Remove relationship?"
        confirmLabel="Remove relationship"
        busy={del.isPending}
        onCancel={() => {
          del.reset();
          setRemoving(null);
        }}
        onConfirm={() => removing && del.mutate(removing.id, { onSuccess: () => setRemoving(null) })}
      >
        {del.isError && <ErrorAlert error={del.error} title="Remove failed" />}
        {removing && (
          <p>
            <strong>{removing.source.name}</strong> <em>{removing.type.forwardLabel}</em> <strong>{removing.target.name}</strong> will be removed.
            Neither CI is deleted.
          </p>
        )}
      </ConfirmDialog>
    </section>
  );
}

function AddRelationshipForm({ ci }: { ci: Ci }) {
  const [target, setTarget] = useState<CiSummary | null>(null);
  const [choice, setChoice] = useState("");
  const [notes, setNotes] = useState("");
  const [done, setDone] = useState<string | null>(null);
  const outTypes = useRelationshipTypes(ci.classId, target?.classId);
  const inTypes = useRelationshipTypes(target?.classId, ci.classId);
  const create = useCreateRelationship();

  const options: { value: string; label: string }[] = [];
  for (const t of outTypes.data?.data ?? []) options.push({ value: `${t.id}:out`, label: `${ci.name} ${t.forwardLabel} ${target?.name}` });
  for (const t of inTypes.data?.data ?? []) {
    if (!t.isDirectional && options.some((o) => o.value === `${t.id}:out`)) continue;
    options.push({ value: `${t.id}:in`, label: `${ci.name} ${t.isDirectional ? t.reverseLabel : t.forwardLabel} ${target?.name}` });
  }
  const typesLoading = !!target && (outTypes.isLoading || inTypes.isLoading);
  const typesError = outTypes.error ?? inTypes.error;
  const fe = create.error instanceof ApiError ? create.error.fieldErrors() : {};
  const targetError = fe.targetCiId ?? fe.sourceCiId;
  const typeError = fe.relationshipTypeId;
  const otherErrors = create.error instanceof ApiError ? create.error.details.filter((d) => !["targetCiId", "sourceCiId", "relationshipTypeId", "notes"].includes(d.field)) : [];

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (!target || !choice) return;
    const [typeId, dir] = choice.split(":");
    const sourceCiId = dir === "out" ? ci.id : target.id;
    const targetCiId = dir === "out" ? target.id : ci.id;
    const label = options.find((o) => o.value === choice)?.label ?? "";
    create.mutate(
      { relationshipTypeId: typeId, sourceCiId, targetCiId, notes: notes.trim() || null },
      {
        onSuccess: () => {
          setDone(`Added: ${label}`);
          setTarget(null);
          setChoice("");
          setNotes("");
        },
      },
    );
  };

  return (
    <form className="rel-add" onSubmit={submit} aria-label="Add relationship">
      <div className="field">
        <label htmlFor="rel-target">Relate to</label>
        <CiPicker
          id="rel-target"
          excludeId={ci.id}
          selected={target}
          onSelect={(t) => {
            setTarget(t);
            setChoice("");
            setDone(null);
            create.reset();
          }}
          invalid={!!targetError}
          describedBy={targetError ? "rel-target-err" : undefined}
        />
        {targetError && <span className="error" id="rel-target-err">{targetError}</span>}
      </div>
      <div className="field">
        <label htmlFor="rel-type">Relationship</label>
        <select
          id="rel-type"
          value={choice}
          onChange={(e) => setChoice(e.target.value)}
          disabled={!target || typesLoading || options.length === 0}
          aria-invalid={typeError ? true : undefined}
          aria-describedby={typeError ? "rel-type-err" : !target ? undefined : "rel-type-hint"}
        >
          <option value="">
            {!target ? "Pick a CI first" : typesLoading ? "Loading…" : options.length === 0 ? "No relationship allowed" : "Choose…"}
          </option>
          {options.map((o) => (
            <option key={o.value} value={o.value}>
              {o.label}
            </option>
          ))}
        </select>
        {typeError && <span className="error" id="rel-type-err">{typeError}</span>}
        {target && !typesLoading && !typeError && (
          <span className="hint" id="rel-type-hint">
            {options.length === 0
              ? `No relationship rule allows ${ci.class.name} ↔ ${target.class.name}.`
              : `Only types allowed between ${ci.class.name} and ${target.class.name}`}
          </span>
        )}
      </div>
      <div className="field">
        <label htmlFor="rel-notes">Notes</label>
        <input id="rel-notes" type="text" value={notes} onChange={(e) => setNotes(e.target.value)} placeholder="Optional" style={{ width: 200 }} aria-invalid={fe.notes ? true : undefined} />
        {fe.notes && <span className="error">{fe.notes}</span>}
      </div>
      <button type="submit" className="btn btn-primary" disabled={!target || !choice || create.isPending}>
        {create.isPending ? "Adding…" : "Add relationship"}
      </button>
      {done && !create.error && <span role="status" className="muted">{done}</span>}
      {typesError != null && <ErrorAlert error={typesError} title="Could not load relationship types" />}
      {create.error != null && (!(create.error instanceof ApiError) || otherErrors.length > 0 || create.error.details.length === 0) && (
        <div style={{ flexBasis: "100%" }}>
          <ErrorAlert error={create.error} title="Relationship not added" />
        </div>
      )}
    </form>
  );
}
