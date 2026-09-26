import { useEffect, useMemo, useState, type FormEvent, type ReactNode } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router-dom";
import { ApiError } from "../api/client";
import {
  useCi,
  useCiClasses,
  useClassAttributes,
  useCreateCi,
  useUpdateCi,
  type Ci,
  type CiCreateBody,
  type CiUpdateBody,
} from "../api/queries";
import { AttributeInput, hintFor, toApiValue, toFormValue, type FormValue } from "../components/AttributeInput";
import { Breadcrumbs } from "../components/Breadcrumbs";
import { LookupSelect } from "../components/LookupSelect";
import { EmptyState, ErrorAlert, Loading } from "../components/States";
import { groupAttributes } from "../lib/attributes";
import { useDocumentTitle } from "../lib/hooks";

/** Core CI fields. These belong to every CI regardless of class; class-specific fields come from the API. */
const CORE_FIELDS = ["name", "statusId", "environmentId", "ownerId", "locationId", "hostname", "ipAddress", "serialNumber", "notes"] as const;
type CoreField = (typeof CORE_FIELDS)[number];
type CoreValues = Record<CoreField, string>;

const EMPTY_CORE: CoreValues = {
  name: "",
  statusId: "",
  environmentId: "",
  ownerId: "",
  locationId: "",
  hostname: "",
  ipAddress: "",
  serialNumber: "",
  notes: "",
};

export function CiCreatePage() {
  const [params, setParams] = useSearchParams();
  const classId = params.get("classId") ?? "";
  const classes = useCiClasses();
  const cls = classes.data?.find((c) => c.id === classId);
  useDocumentTitle(cls ? `New ${cls.name}` : "New CI");

  const concrete = (classes.data ?? []).filter((c) => c.isActive && !c.isAbstract);

  return (
    <>
      <Breadcrumbs items={[{ label: "Inventory", to: "/cis" }, ...(cls ? [{ label: cls.name, to: `/cis?classId=${cls.id}` }] : []), { label: "New" }]} />
      <div className="page-header">
        <div className="title">
          <h1>New configuration item</h1>
        </div>
      </div>
      <section className="panel">
        <div className="panel-body">
          <div className="field" style={{ maxWidth: 320 }}>
            <label htmlFor="ci-class">
              Class<span className="req" aria-hidden="true">*</span>
            </label>
            {classes.isError ? (
              <ErrorAlert error={classes.error} onRetry={() => classes.refetch()} />
            ) : (
              <select
                id="ci-class"
                value={classId}
                required
                autoFocus={!classId}
                onChange={(e) => setParams(e.target.value ? { classId: e.target.value } : {}, { replace: true })}
              >
                <option value="">{classes.isLoading ? "Loading…" : "Choose a class…"}</option>
                {concrete.map((c) => (
                  <option key={c.id} value={c.id}>
                    {c.name}
                  </option>
                ))}
              </select>
            )}
            <span className="hint">The class decides which attributes the CI carries.</span>
          </div>
        </div>
      </section>
      {classId && cls && <CiForm key={classId} mode="create" classId={classId} className={cls.name} />}
      {classId && classes.data && !cls && <ErrorAlert error={new Error(`Class ${classId} does not exist.`)} />}
    </>
  );
}

export function CiEditPage() {
  const { id } = useParams();
  const ci = useCi(id);
  useDocumentTitle(ci.data ? `Edit ${ci.data.name}` : "Edit CI");
  if (ci.isLoading) return <Loading />;
  if (ci.isError) return <ErrorAlert error={ci.error} onRetry={() => ci.refetch()} />;
  if (!ci.data) return null;
  if (ci.data.deletedAt) {
    return <EmptyState title="This configuration item is deleted" actions={<Link to={`/cis/${ci.data.id}`}>Back to the record</Link>}>Deleted CIs cannot be edited.</EmptyState>;
  }
  return (
    <>
      <Breadcrumbs
        items={[
          { label: "Inventory", to: "/cis" },
          { label: ci.data.class.name, to: `/cis?classId=${ci.data.classId}` },
          { label: ci.data.name, to: `/cis/${ci.data.id}` },
          { label: "Edit" },
        ]}
      />
      <div className="page-header">
        <div className="title">
          <h1>Edit {ci.data.name}</h1>
          <span className="muted">
            {ci.data.class.name} · version {ci.data.version}
          </span>
        </div>
      </div>
      <CiForm key={`${ci.data.id}-${ci.data.version}`} mode="edit" classId={ci.data.classId} className={ci.data.class.name} ci={ci.data} />
    </>
  );
}

function CiForm({ mode, classId, className, ci }: { mode: "create" | "edit"; classId: string; className: string; ci?: Ci }) {
  const navigate = useNavigate();
  const attrs = useClassAttributes(classId);
  const create = useCreateCi();
  const update = useUpdateCi(ci?.id ?? "");
  const mutation = mode === "create" ? create : update;

  const initialCore: CoreValues = useMemo(
    () =>
      ci
        ? {
            name: ci.name,
            statusId: ci.statusId,
            environmentId: ci.environmentId ?? "",
            ownerId: ci.ownerId ?? "",
            locationId: ci.locationId ?? "",
            hostname: ci.hostname ?? "",
            ipAddress: ci.ipAddress ?? "",
            serialNumber: ci.serialNumber ?? "",
            notes: ci.notes ?? "",
          }
        : EMPTY_CORE,
    [ci],
  );
  const [core, setCore] = useState<CoreValues>(initialCore);
  const [values, setValues] = useState<Record<string, FormValue>>({});
  const [initialValues, setInitialValues] = useState<Record<string, FormValue>>({});
  const [refNames, setRefNames] = useState<Record<string, string>>(() => referenceNames(ci));
  const [error, setError] = useState<unknown>(null);

  const defs = useMemo(() => (attrs.data ?? []).filter((d) => d.isActive || (ci && ci.attributes[d.key] != null)), [attrs.data, ci]);

  // Seed attribute values once the definitions arrive.
  useEffect(() => {
    if (!attrs.data) return;
    const v: Record<string, FormValue> = {};
    for (const d of attrs.data) v[d.key] = toFormValue(d, ci?.attributes[d.key]);
    setValues(v);
    setInitialValues(v);
  }, [attrs.data, ci]);

  const [missing, setMissing] = useState<Record<string, string>>({});
  const fieldErrors = { ...(error instanceof ApiError ? error.fieldErrors() : {}), ...missing };
  const knownFields = new Set<string>([...CORE_FIELDS, "classId", "version", ...defs.map((d) => `attributes.${d.key}`)]);
  const unplaced = error instanceof ApiError ? error.details.filter((d) => !knownFields.has(d.field)) : [];

  const onSubmit = async (e: FormEvent) => {
    e.preventDefault();
    setError(null);
    // Catch empty required fields before the round trip; everything else is validated by the API.
    const req: Record<string, string> = {};
    if (!core.name.trim()) req.name = "Required";
    if (!core.statusId) req.statusId = "Required";
    for (const d of defs) if (d.isRequired && d.isActive && (values[d.key] ?? "") === "") req[`attributes.${d.key}`] = "Required";
    setMissing(req);
    if (Object.keys(req).length > 0) {
      document.getElementById(fieldIdFor(Object.keys(req)[0]))?.focus();
      return;
    }
    const attributes: Record<string, unknown> = {};
    for (const d of defs) {
      const cur = values[d.key] ?? "";
      if (mode === "create") {
        if (cur !== "") attributes[d.key] = toApiValue(d, cur);
      } else if (cur !== (initialValues[d.key] ?? "")) {
        attributes[d.key] = toApiValue(d, cur);
      }
    }
    const coreBody = coreToApi(core);
    try {
      if (mode === "create") {
        const body = { classId, ...coreBody, attributes } as CiCreateBody;
        const created = await create.mutateAsync(body);
        navigate(`/cis/${created.id}`, { state: { flash: `Created ${created.name}.` } });
      } else if (ci) {
        const initial = coreToApi(initialCore);
        const changed: Record<string, unknown> = {};
        for (const k of CORE_FIELDS) if (coreBody[k] !== initial[k]) changed[k] = coreBody[k];
        if (Object.keys(changed).length === 0 && Object.keys(attributes).length === 0) {
          navigate(`/cis/${ci.id}`);
          return;
        }
        const body = { ...changed, ...(Object.keys(attributes).length ? { attributes } : {}), version: ci.version } as CiUpdateBody;
        const saved = await update.mutateAsync(body);
        navigate(`/cis/${saved.id}`, { state: { flash: `Saved ${saved.name}.` } });
      }
    } catch (err) {
      setError(err);
      window.scrollTo({ top: 0 });
    }
  };

  const setField = (k: CoreField) => (v: string) => setCore((c) => ({ ...c, [k]: v }));
  const cancelTo = ci ? `/cis/${ci.id}` : "/cis";

  const groups = groupAttributes(defs);

  return (
    <form onSubmit={onSubmit} noValidate aria-label={mode === "create" ? `New ${className}` : `Edit ${ci?.name}`}>
      {error != null && (
        <FormErrorBanner error={error} unplaced={unplaced} versionConflictHref={ci ? `/cis/${ci.id}` : undefined} />
      )}
      <section className="panel">
        <div className="panel-header">
          <h2>General</h2>
          <span className="muted">Fields every CI has</span>
        </div>
        <div className="panel-body">
          <div className="form-grid">
            <Field id="f-name" label="Name" required error={fieldErrors.name}>
              {(p) => <input {...p} type="text" value={core.name} onChange={(e) => setField("name")(e.target.value)} autoFocus />}
            </Field>
            <Field id="f-status" label="Status" required error={fieldErrors.statusId}>
              {(p) => <LookupSelect kind="statuses" {...lookupProps(p)} value={core.statusId} onChange={setField("statusId")} emptyLabel="Choose a status…" required />}
            </Field>
            <Field id="f-environment" label="Environment" error={fieldErrors.environmentId}>
              {(p) => <LookupSelect kind="environments" {...lookupProps(p)} value={core.environmentId} onChange={setField("environmentId")} emptyLabel="— none —" />}
            </Field>
            <Field id="f-owner" label="Owner" error={fieldErrors.ownerId}>
              {(p) => <LookupSelect kind="owners" {...lookupProps(p)} value={core.ownerId} onChange={setField("ownerId")} emptyLabel="— none —" />}
            </Field>
            <Field id="f-location" label="Location" error={fieldErrors.locationId}>
              {(p) => <LookupSelect kind="locations" {...lookupProps(p)} value={core.locationId} onChange={setField("locationId")} emptyLabel="— none —" />}
            </Field>
            <Field id="f-hostname" label="Hostname" error={fieldErrors.hostname}>
              {(p) => <input {...p} type="text" className="mono" spellCheck={false} value={core.hostname} onChange={(e) => setField("hostname")(e.target.value)} />}
            </Field>
            <Field id="f-ip" label="IP address" error={fieldErrors.ipAddress} hint="IPv4 or IPv6">
              {(p) => <input {...p} type="text" className="mono" spellCheck={false} value={core.ipAddress} onChange={(e) => setField("ipAddress")(e.target.value)} />}
            </Field>
            <Field id="f-serial" label="Serial number" error={fieldErrors.serialNumber}>
              {(p) => <input {...p} type="text" className="mono" spellCheck={false} value={core.serialNumber} onChange={(e) => setField("serialNumber")(e.target.value)} />}
            </Field>
            <Field id="f-notes" label="Notes" error={fieldErrors.notes} wide>
              {(p) => <textarea {...p} value={core.notes} onChange={(e) => setField("notes")(e.target.value)} />}
            </Field>
          </div>
        </div>
      </section>

      <section className="panel">
        <div className="panel-header">
          <h2>{className} attributes</h2>
          <span className="muted">Defined by the class and its parents</span>
        </div>
        <div className="panel-body">
          {attrs.isLoading && <Loading label="Loading attribute definitions…" />}
          {attrs.isError && <ErrorAlert error={attrs.error} title="Could not load this class's attributes" onRetry={() => attrs.refetch()} />}
          {attrs.data && defs.length === 0 && <p className="muted">This class defines no extra attributes.</p>}
          {groups.map(([group, items]) => (
            <fieldset className="group" key={group}>
              <legend>{group}</legend>
              <div className="form-grid">
                {items.map((d) => (
                  <Field
                    key={d.id}
                    id={`attr-${d.key}`}
                    label={d.label}
                    required={d.isRequired}
                    error={fieldErrors[`attributes.${d.key}`]}
                    hint={[hintFor(d), d.inherited ? `from ${d.definedOn.name}` : "", d.isActive ? "" : "retired attribute"].filter(Boolean).join(" · ") || undefined}
                  >
                    {(p) => (
                      <AttributeInput
                        def={d}
                        id={p.id}
                        invalid={p["aria-invalid"]}
                        describedBy={p["aria-describedby"]}
                        value={values[d.key] ?? ""}
                        onChange={(v) => setValues((s) => ({ ...s, [d.key]: v }))}
                        referenceName={refNames[d.key]}
                        onReferenceName={(name) => setRefNames((s) => ({ ...s, [d.key]: name }))}
                      />
                    )}
                  </Field>
                ))}
              </div>
            </fieldset>
          ))}
        </div>
        <div className="form-footer">
          <button type="submit" className="btn btn-primary" disabled={mutation.isPending || attrs.isLoading || attrs.isError}>
            {mutation.isPending ? "Saving…" : mode === "create" ? `Create ${className}` : "Save changes"}
          </button>
          <Link className="btn" to={cancelTo}>
            Cancel
          </Link>
        </div>
      </section>
    </form>
  );
}

interface FieldProps {
  id: string;
  "aria-invalid"?: true;
  "aria-describedby"?: string;
}

function Field({
  id,
  label,
  required,
  error,
  hint,
  wide,
  children,
}: {
  id: string;
  label: string;
  required?: boolean;
  error?: string;
  hint?: string;
  wide?: boolean;
  children: (p: FieldProps) => ReactNode;
}) {
  const describedBy = [error ? `${id}-err` : "", hint ? `${id}-hint` : ""].filter(Boolean).join(" ") || undefined;
  return (
    <div className={`field${wide ? " wide" : ""}`}>
      <label htmlFor={id}>
        {label}
        {required && (
          <span className="req" aria-label="required">
            *
          </span>
        )}
      </label>
      {children({ id, "aria-invalid": error ? true : undefined, "aria-describedby": describedBy })}
      {error && (
        <span className="error" id={`${id}-err`}>
          {error}
        </span>
      )}
      {hint && (
        <span className="hint" id={`${id}-hint`}>
          {hint}
        </span>
      )}
    </div>
  );
}

function lookupProps(p: FieldProps) {
  return { id: p.id, invalid: !!p["aria-invalid"], describedBy: p["aria-describedby"] };
}

function FormErrorBanner({
  error,
  unplaced,
  versionConflictHref,
}: {
  error: unknown;
  unplaced: { field: string; message: string }[];
  versionConflictHref?: string;
}) {
  if (!(error instanceof ApiError)) return <ErrorAlert error={error} />;
  if (error.code === "VERSION_CONFLICT") {
    return (
      <div className="alert alert-warn" role="alert">
        <strong>Someone else saved this CI while you were editing.</strong>
        <div>
          {error.message} Your changes were not saved.{" "}
          {versionConflictHref && (
            <>
              <Link to={versionConflictHref}>Open the current version</Link> and re-apply your edits.
            </>
          )}
        </div>
      </div>
    );
  }
  if (error.code === "VALIDATION_ERROR" || error.code === "CONFLICT") {
    return (
      <div className="alert alert-error" role="alert">
        <strong>Not saved — fix the highlighted fields.</strong>
        <div>{error.message}</div>
        {unplaced.length > 0 && (
          <ul>
            {unplaced.map((d, i) => (
              <li key={i}>
                {d.field && d.field !== "(root)" && <code>{d.field}</code>} {d.message}
              </li>
            ))}
          </ul>
        )}
      </div>
    );
  }
  return <ErrorAlert error={error} title="Not saved" />;
}

const CORE_IDS: Record<string, string> = { name: "f-name", statusId: "f-status" };
function fieldIdFor(key: string): string {
  return key.startsWith("attributes.") ? `attr-${key.slice(11)}` : (CORE_IDS[key] ?? key);
}

function coreToApi(core: CoreValues): Record<CoreField, string | null> {
  const out = {} as Record<CoreField, string | null>;
  for (const k of CORE_FIELDS) {
    const v = core[k].trim();
    out[k] = v === "" ? null : k === "notes" ? core[k] : v;
  }
  // name and statusId are required; send "" rather than null so the API reports them as fields.
  if (out.name === null) out.name = "";
  if (out.statusId === null) out.statusId = "";
  return out;
}

function referenceNames(ci: Ci | undefined): Record<string, string> {
  const out: Record<string, string> = {};
  const refs = (ci?.attributeReferences ?? {}) as Record<string, unknown>;
  for (const [k, v] of Object.entries(refs)) {
    if (typeof v === "string") out[k] = v;
    else if (v && typeof v === "object" && "name" in v) out[k] = String((v as { name: unknown }).name);
  }
  return out;
}
