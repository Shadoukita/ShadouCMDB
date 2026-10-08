import { computed, reactive, ref, shallowRef, toValue, watch, type MaybeRefOrGetter } from "vue";
import { useIsPersonClass, useSignInAccount } from "../../api/admin";
import { ApiError } from "../../api/client";
import { useCriticalityValues, useUpdateCi, type Ci, type CiUpdateBody, type EffectiveAttribute } from "../../api/queries";
import { useCiWorkflows } from "../../api/workflowRuntime";
import { t } from "../../i18n";
import { clonedValue, type CloneSource } from "../../lib/ciClone";
import { hintFor, nowFormValue, toFormValue, type FormValue } from "../../lib/attributeValues";
import { ciEdits, type CoreValues } from "../../lib/ciEdits";
import { HIDDEN_CI } from "../../lib/format";
import { ATTRIBUTE_PREFIX, attributeKey, BUILTIN, CORE_FIELDS } from "../../lib/uiSettings";
import { useSessionStore } from "../../stores/session";

/** The built-in fields that have an input; the others (class, active, timestamps) are only shown. */
export const EDITABLE_CORE: ReadonlySet<string> = new Set(["ident", "criticality", "validFrom", "validUntil"]);
/** Input ids of the core fields. */
export const FIELD_IDS: Record<string, string> = { ident: "f-ident", criticality: "f-criticality", validFrom: "f-valid-from", validUntil: "f-valid-until" };
export const fieldIdFor = (key: string) => (key.startsWith(ATTRIBUTE_PREFIX) ? `attr-${key.slice(ATTRIBUTE_PREFIX.length)}` : (FIELD_IDS[key] ?? key));

export interface CiDraftOptions {
  mode: "create" | "edit";
  classId: MaybeRefOrGetter<string | undefined>;
  /** The CI edited (edit mode; undefined while it loads). */
  ci?: MaybeRefOrGetter<Ci | undefined>;
  /** The class's attribute definitions as loaded (retired ones included when the caller asked for them). */
  attrs: MaybeRefOrGetter<EffectiveAttribute[] | undefined>;
  /** Fields the layout makes read-only. */
  readOnlyFields: MaybeRefOrGetter<readonly string[] | undefined>;
  /** Nothing can be changed: no edit right on the class, or a deleted CI. */
  locked?: MaybeRefOrGetter<boolean>;
  /** A new CI cloned from `ci` (create mode, lib/ciClone): its values start from the source's. Read when the draft starts. */
  clone?: MaybeRefOrGetter<(CloneSource & { ci: Ci }) | undefined>;
}

/**
 * The values of the CI form being edited: the core fields, the criticality and the class attributes, as
 * typed (strings, see lib/attributeValues), with the values they started from. Used by the create and edit
 * form (CiForm) and by the detail page, which shows a CI's fields as inputs directly (SHAA-1644).
 *
 * In edit mode the draft starts from the CI and follows it while nothing was changed: a reload or a
 * save starts it over. Once something was changed it stays on the version it started from, so the save
 * sends that version and the API refuses it (VERSION_CONFLICT) if someone else saved in between.
 */
export function useCiDraft(opts: CiDraftOptions) {
  const session = useSessionStore();
  /** Only administrators choose or change an ident; everyone else gets a generated one. */
  const isAdmin = computed(() => !!session.permissions?.administrator);
  const create = opts.mode === "create";

  /** The CI the draft started from (edit mode). */
  const base = shallowRef<Ci>();
  // The validity period is edited in local time (datetime-local inputs) and sent as ISO timestamps.
  // A new CI is valid from the moment the form opens.
  const initialCore = shallowRef<CoreValues>({ ident: "", validFrom: "", validUntil: "" });
  const core = ref<CoreValues>({ ident: "", validFrom: "", validUntil: "" });
  // Criticality: a core field of every CI (a value of the system lookup list), placed, hidden or made read-only
  // by the layout like the others; empty means not set.
  const initialCriticality = ref("");
  const criticalityId = ref("");
  const initialValues = shallowRef<Record<string, FormValue>>({});
  const values = ref<Record<string, FormValue>>({});
  const refNames = ref<Record<string, string>>({});
  const error = ref<unknown>(null);
  const missing = ref<Record<string, string>>({});

  const criticality = useCriticalityValues();
  const criticalityOptions = computed(() => (criticality.data.value ?? []).filter((v) => v.isActive || v.id === criticalityId.value));
  const criticalityStray = computed(
    () => !!criticalityId.value && !!criticality.data.value && !criticalityOptions.value.some((v) => v.id === criticalityId.value),
  );

  /** The source of a clone (create mode). */
  const cloneOf = shallowRef<CloneSource & { ci: Ci }>();
  /** Starts the draft over from `ci` (a new CI: empty, valid from now, attributes at their defaults or a clone's values). */
  function reset(ci?: Ci) {
    cloneOf.value = create ? toValue(opts.clone) : undefined;
    base.value = ci;
    initialCore.value = ci
      ? { ident: ci.ident, validFrom: toFormValue(DATETIME, ci.validFrom), validUntil: toFormValue(DATETIME, ci.validUntil) }
      : { ident: "", validFrom: nowFormValue("datetime"), validUntil: "" };
    core.value = { ...initialCore.value };
    initialCriticality.value = (ci ?? cloneOf.value?.ci)?.criticality?.id ?? "";
    criticalityId.value = initialCriticality.value;
    refNames.value = referenceNames(ci ?? cloneOf.value?.ci);
    initialValues.value = {};
    values.value = {};
    error.value = null;
    missing.value = {};
    seedValues();
  }
  /** Fills in the attributes not seeded yet (the definitions arrive after the CI). */
  function seedValues() {
    const data = toValue(opts.attrs);
    if (!data) return;
    const ci = base.value;
    const src = cloneOf.value;
    const add: Record<string, FormValue> = {};
    for (const d of data) {
      if (d.key in initialValues.value) continue;
      add[d.key] = toFormValue(d, ci ? ci.attributes[d.key] : !d.isActive ? undefined : src ? clonedValue(src, d) : d.defaultValue);
    }
    if (Object.keys(add).length === 0) return;
    initialValues.value = { ...initialValues.value, ...add };
    values.value = { ...values.value, ...add };
  }
  watch(() => toValue(opts.attrs), seedValues);

  const defs = computed(() => (toValue(opts.attrs) ?? []).filter((d) => d.isActive || (base.value && base.value.attributes[d.key] != null)));

  /** The changes to send (PATCH, without the version), or null when nothing was changed. */
  const patch = computed(() =>
    base.value
      ? (ciEdits({
          defs: defs.value,
          core: core.value,
          initialCore: initialCore.value,
          criticalityId: criticalityId.value,
          initialCriticality: initialCriticality.value,
          values: values.value,
          initialValues: initialValues.value,
        }) as Omit<CiUpdateBody, "version"> | null)
      : null,
  );
  /** Something was changed (edit mode). */
  const dirty = computed(() => patch.value !== null);
  /** How many fields were changed, for the save bar. */
  const changeCount = computed(() => {
    const p = patch.value as Record<string, unknown> | null;
    if (!p) return 0;
    const { attributes, ...core } = p;
    return Object.keys(core).length + Object.keys((attributes as Record<string, unknown> | undefined) ?? {}).length;
  });

  if (create) reset();
  else {
    // A reload or a save starts the draft over while it holds no changes; another CI always does.
    watch(
      () => toValue(opts.ci),
      (ci) => {
        if (ci && (ci.id !== base.value?.id || (ci.version !== base.value.version && !dirty.value))) reset(ci);
      },
      { immediate: true },
    );
  }

  const update = useUpdateCi(() => base.value?.id ?? "");
  /** Sends the changes with the version the draft started from; null when there is nothing to save. */
  async function save(): Promise<Ci | null> {
    const body = patch.value;
    if (!body || !base.value) return null;
    return update.mutateAsync({ ...body, version: base.value.version } as CiUpdateBody);
  }

  const requiredField = (f: string) => !!defs.value.find((d) => `${ATTRIBUTE_PREFIX}${d.key}` === f && d.isRequired && d.isActive);
  /** Required fields stay editable on a new CI whatever the layout says, or it could never be saved. */
  const keepEditable = (f: string) => create && requiredField(f);
  /**
   * A Person linked to a sign-in account (SHAA-1505 decision 5): its Email follows the account's and the API refuses a
   * change (409 managed_by_user), so the field is read-only and names the account.
   */
  const isPerson = useIsPersonClass(opts.classId);
  const signInAccount = useSignInAccount(
    () => base.value?.id,
    () => !create && isPerson.value,
  );
  const managedBy = computed(() => signInAccount.data.value?.account?.username ?? null);
  const managedEmail = computed(() => {
    const d = managedBy.value ? defs.value.find((x) => x.systemRole === "person_email") : undefined;
    return d ? `${ATTRIBUTE_PREFIX}${d.key}` : null;
  });
  /**
   * Fields an active workflow drives on this CI (its state field, Q3): they change only through a transition and the
   * API refuses a direct write (409 WORKFLOW_CONTROLLED_FIELD), so they are read-only and say why.
   */
  const workflows = useCiWorkflows(() => (create ? undefined : toValue(opts.ci)?.id));
  const controlled = computed(() => new Set((workflows.data.value?.controlledFields ?? []).map((k) => `${ATTRIBUTE_PREFIX}${k}`)));
  const readOnly = computed(() => {
    const ro = new Set((toValue(opts.readOnlyFields) ?? []).filter((f) => !keepEditable(f)));
    if (managedEmail.value) ro.add(managedEmail.value);
    for (const f of controlled.value) ro.add(f);
    return ro;
  });
  const locked = computed(() => !!toValue(opts.locked));
  /** The field can be changed: not locked, not read-only, and an input exists for it (the ident only for administrators). */
  function editable(f: string): boolean {
    if (locked.value || readOnly.value.has(f)) return false;
    if (BUILTIN.has(f)) return EDITABLE_CORE.has(f) && (f !== "ident" || isAdmin.value);
    return !!defFor(f);
  }
  const defFor = (f: string) => defs.value.find((d) => d.key === attributeKey(f));

  /** A dependent lookup's parent field (Manufacturer for Model): its label and the value chosen in it. */
  function lookupParent(d: EffectiveAttribute) {
    const p = d.parentAttributeId ? toValue(opts.attrs)?.find((a) => a.id === d.parentAttributeId) : undefined;
    return p ? { label: p.label, value: values.value[p.key] ?? "" } : null;
  }
  function coreHint(f: string): string | undefined {
    const hint =
      f === "ident"
        ? create && isAdmin.value
          ? "Generated when left empty"
          : create
            ? "Generated"
            : ""
        : f === "criticality"
          ? "How critical this CI is to the business; impact analysis groups by it"
          : f === "validFrom"
            ? "Local time"
            : f === "validUntil"
              ? "Local time; empty: open-ended"
              : "";
    return [hint, EDITABLE_CORE.has(f) && !locked.value && !editable(f) ? "read-only" : ""].filter(Boolean).join(" · ") || undefined;
  }
  /** What a field shown read-only says about it: who manages it. */
  const readOnlyHint = (f: string) =>
    f === managedEmail.value
      ? t("people.form.managedBy", { username: managedBy.value ?? "" })
      : controlled.value.has(f)
        ? t("workflows.controlledField")
        : undefined;
  function attrHint(d: EffectiveAttribute): string | undefined {
    const f = `${ATTRIBUTE_PREFIX}${d.key}`;
    const managed = readOnlyHint(f);
    if (managed) return managed;
    const parent = lookupParent(d);
    const ro = !locked.value && readOnly.value.has(f) ? "read-only" : "";
    return [hintFor(d), parent ? `depends on ${parent.label}` : "", d.inherited ? `from ${d.definedOn.name}` : "", d.isActive ? "" : "retired attribute", ro].filter(Boolean).join(" · ") || undefined;
  }

  const fieldErrors = computed<Record<string, string>>(() => ({
    ...(error.value instanceof ApiError ? error.value.fieldErrors() : {}),
    ...missing.value,
  }));
  const coreError = (f: string) => fieldErrors.value[BUILTIN.get(f)?.form ?? f];
  /** API errors no field on the form shows. */
  const unplaced = computed(() => {
    if (!(error.value instanceof ApiError)) return [];
    const known = new Set<string>([...CORE_FIELDS, "criticalityValueId", "classId", "version", ...defs.value.map((d) => `attributes.${d.key}`)]);
    return error.value.details.filter((d) => !known.has(d.field));
  });
  /** Marks the empty required fields among `shown` (empty required fields are caught before the round trip); returns them. */
  function checkRequired(shown: ReadonlySet<string>): string[] {
    const req: Record<string, string> = {};
    if (!core.value.validFrom && shown.has("validFrom")) req.validFrom = "Required";
    for (const d of defs.value) {
      const f = `${ATTRIBUTE_PREFIX}${d.key}`;
      if (d.isRequired && d.isActive && shown.has(f) && editable(f) && (values.value[d.key] ?? "") === "") req[f] = "Required";
    }
    missing.value = req;
    return Object.keys(req);
  }

  return reactive({
    base,
    isAdmin,
    core,
    initialCore,
    criticalityId,
    criticalityOptions,
    criticalityStray,
    criticalityLoading: criticality.isLoading,
    criticalityError: criticality.isError,
    values,
    refNames,
    defs,
    error,
    missing,
    fieldErrors,
    unplaced,
    patch,
    dirty,
    changeCount,
    pending: update.isPending,
    locked,
    readOnly,
    reset,
    save,
    editable,
    defFor,
    lookupParent,
    coreHint,
    attrHint,
    readOnlyHint,
    coreError,
    checkRequired,
  });
}
export type CiDraft = ReturnType<typeof useCiDraft>;

const DATETIME = { dataType: "datetime" } as const;

function referenceNames(ci: Ci | undefined): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(ci?.attributeReferences ?? {})) out[k] = v.hidden ? HIDDEN_CI : (v.name ?? v.id);
  return out;
}
