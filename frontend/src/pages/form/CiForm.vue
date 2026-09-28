<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { RouterLink, useRouter } from "vue-router";
import { ApiError } from "../../api/client";
import {
  useCiClasses,
  useClassAttributes,
  useCreateCi,
  useLookup,
  useUpdateCi,
  type Ci,
  type CiCreateBody,
  type CiUpdateBody,
} from "../../api/queries";
import AttributeInput from "../../components/AttributeInput.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import LookupSelect from "../../components/LookupSelect.vue";
import type { LookupParent } from "../../components/LookupValueSelect.vue";
import { useAppSettings } from "../../lib/appSettings";
import { groupAttributes } from "../../lib/attributes";
import { vAutofocus } from "../../lib/directives";
import { HIDDEN_CI } from "../../lib/format";
import { hintFor, toApiValue, toFormValue, type FormValue } from "../../lib/attributeValues";
import { ATTRIBUTE_PREFIX, attributeKey, BUILTIN, FORM_BUILTINS, layoutFor, resolveLayout } from "../../lib/uiSettings";
import { useFlashStore } from "../../stores/flash";
import { useSessionStore } from "../../stores/session";
import FormErrorBanner from "./FormErrorBanner.vue";
import FormField from "./FormField.vue";

/**
 * The CI form. Core fields are the same for every CI; the class attributes are
 * rendered from GET /ci-classes/{id}/attributes, so a new class needs no frontend change.
 * The parent keys this component by class (create) or id+version (edit).
 *
 * A class layout (Administration › Customization › Detail and form layout) arranges
 * the fields in panels, hides fields and makes fields read-only. Required fields
 * stay editable on a new CI whatever the layout says, or it could never be saved.
 */
const props = defineProps<{ mode: "create" | "edit"; classId: string; className: string; ci?: Ci }>();

/** Core CI fields. These belong to every CI regardless of class; class-specific fields come from the API. */
const CORE_FIELDS = ["name", "statusId", "environmentId", "ownerId", "locationId", "hostname", "ipAddress", "serialNumber", "notes"] as const;
type CoreField = (typeof CORE_FIELDS)[number];
type CoreValues = Record<CoreField, string>;

const router = useRouter();
const flash = useFlashStore();
const session = useSessionStore();
/** Every CI needs a status; on a fresh install there may be none yet. */
const statuses = useLookup("statuses");
const noStatuses = computed(() => !!statuses.data.value && !statuses.data.value.some((s) => s.isActive));
const attrs = useClassAttributes(() => props.classId);
const create = useCreateCi();
const update = useUpdateCi(() => props.ci?.id ?? "");
const pending = computed(() => (props.mode === "create" ? create.isPending.value : update.isPending.value));

const initialCore: CoreValues = props.ci
  ? {
      name: props.ci.name,
      statusId: props.ci.statusId,
      environmentId: props.ci.environmentId ?? "",
      ownerId: props.ci.ownerId ?? "",
      locationId: props.ci.locationId ?? "",
      hostname: props.ci.hostname ?? "",
      ipAddress: props.ci.ipAddress ?? "",
      serialNumber: props.ci.serialNumber ?? "",
      notes: props.ci.notes ?? "",
    }
  : { name: "", statusId: "", environmentId: "", ownerId: "", locationId: "", hostname: "", ipAddress: "", serialNumber: "", notes: "" };
const core = ref<CoreValues>({ ...initialCore });
const values = ref<Record<string, FormValue>>({});
let initialValues: Record<string, FormValue> = {};
const refNames = ref<Record<string, string>>(referenceNames(props.ci));
const error = ref<unknown>(null);
const missing = ref<Record<string, string>>({});

const defs = computed(() => (attrs.data.value ?? []).filter((d) => d.isActive || (props.ci && props.ci.attributes[d.key] != null)));

const settings = useAppSettings();
const classes = useCiClasses();
const layout = computed(() => layoutFor(settings.doc.value, classes.data.value?.find((c) => c.id === props.classId)?.key));
const requiredField = (f: string) =>
  f === "name" || f === "status" || !!defs.value.find((d) => `${ATTRIBUTE_PREFIX}${d.key}` === f && d.isRequired && d.isActive);
const keepEditable = (f: string) => props.mode === "create" && requiredField(f);
const readOnly = computed(() => new Set((layout.value?.readOnlyFields ?? []).filter((f) => !keepEditable(f))));

interface FormSection {
  key: string;
  label: string;
  subtitle?: string;
  collapsed?: boolean;
  /** The section that reports loading/errors of the class's attribute definitions. */
  attributes?: boolean;
  groups: { legend: string | null; fields: string[] }[];
}
const sections = computed<FormSection[]>(() => {
  const l = layout.value;
  const panels = l ? resolveLayout({ ...l, hiddenFields: (l.hiddenFields ?? []).filter((f) => !keepEditable(f)) }, defs.value, FORM_BUILTINS) : null;
  if (panels) {
    return panels.map((p, i) => ({ key: p.key, label: p.label, collapsed: p.collapsed, attributes: i === 0, groups: [{ legend: null, fields: p.fields }] }));
  }
  return [
    { key: "general", label: "General", subtitle: "Fields every CI has", groups: [{ legend: null, fields: FORM_BUILTINS }] },
    {
      key: "attributes",
      label: `${props.className} attributes`,
      subtitle: "Defined by the class and its parents",
      attributes: true,
      groups: groupAttributes(defs.value).map(([g, items]) => ({ legend: g, fields: items.map((d) => `${ATTRIBUTE_PREFIX}${d.key}`) })),
    },
  ];
});
const FIELD_IDS: Record<string, string> = {
  name: "f-name",
  status: "f-status",
  environment: "f-environment",
  owner: "f-owner",
  location: "f-location",
  hostname: "f-hostname",
  ipAddress: "f-ip",
  serialNumber: "f-serial",
  notes: "f-notes",
};
const defFor = (f: string) => defs.value.find((d) => d.key === attributeKey(f));
/** A dependent lookup's parent field (Manufacturer for Model): its label and the value chosen in it. */
function lookupParent(d: (typeof defs.value)[number]): LookupParent | null {
  const p = d.parentAttributeId ? attrs.data.value?.find((a) => a.id === d.parentAttributeId) : undefined;
  return p ? { label: p.label, value: values.value[p.key] ?? "" } : null;
}
const coreError = (f: string) => fieldErrors.value[BUILTIN.get(f)?.form ?? f];
function coreHint(f: string): string | undefined {
  return [f === "ipAddress" ? "IPv4 or IPv6" : "", readOnly.value.has(f) ? "read-only" : ""].filter(Boolean).join(" · ") || undefined;
}

// Seed attribute values once the definitions arrive. A new CI starts from each attribute's default value.
watch(
  () => attrs.data.value,
  (data) => {
    if (!data) return;
    const v: Record<string, FormValue> = {};
    for (const d of data) v[d.key] = toFormValue(d, props.ci ? props.ci.attributes[d.key] : d.isActive ? d.defaultValue : undefined);
    values.value = v;
    initialValues = { ...v };
  },
  { immediate: true },
);

const fieldErrors = computed<Record<string, string>>(() => ({
  ...(error.value instanceof ApiError ? error.value.fieldErrors() : {}),
  ...missing.value,
}));
const unplaced = computed(() => {
  if (!(error.value instanceof ApiError)) return [];
  const known = new Set<string>([...CORE_FIELDS, "classId", "version", ...defs.value.map((d) => `attributes.${d.key}`)]);
  return error.value.details.filter((d) => !known.has(d.field));
});

async function onSubmit() {
  error.value = null;
  // Catch empty required fields before the round trip; everything else is validated by the API.
  const req: Record<string, string> = {};
  if (!core.value.name.trim()) req.name = "Required";
  if (!core.value.statusId) req.statusId = "Required";
  const shown = new Set(sections.value.flatMap((sec) => sec.groups.flatMap((g) => g.fields)));
  for (const d of defs.value) {
    const f = `${ATTRIBUTE_PREFIX}${d.key}`;
    if (d.isRequired && d.isActive && shown.has(f) && (values.value[d.key] ?? "") === "") req[f] = "Required";
  }
  missing.value = req;
  if (Object.keys(req).length > 0) {
    document.getElementById(fieldIdFor(Object.keys(req)[0]))?.focus();
    return;
  }
  const attributes: Record<string, unknown> = {};
  for (const d of defs.value) {
    const cur = values.value[d.key] ?? "";
    if (props.mode === "create") {
      if (cur !== "") attributes[d.key] = toApiValue(d, cur);
    } else if (cur !== (initialValues[d.key] ?? "")) {
      attributes[d.key] = toApiValue(d, cur);
    }
  }
  const coreBody = coreToApi(core.value);
  try {
    if (props.mode === "create") {
      const body = { classId: props.classId, ...coreBody, attributes } as CiCreateBody;
      const created = await create.mutateAsync(body);
      flash.show(created.id, `Created ${created.name}.`);
      await router.push(`/cis/${created.id}`);
    } else if (props.ci) {
      const initial = coreToApi(initialCore);
      const changed: Record<string, unknown> = {};
      for (const k of CORE_FIELDS) if (coreBody[k] !== initial[k]) changed[k] = coreBody[k];
      if (Object.keys(changed).length === 0 && Object.keys(attributes).length === 0) {
        await router.push(`/cis/${props.ci.id}`);
        return;
      }
      const body = { ...changed, ...(Object.keys(attributes).length ? { attributes } : {}), version: props.ci.version } as CiUpdateBody;
      const saved = await update.mutateAsync(body);
      flash.show(saved.id, `Saved ${saved.name}.`);
      await router.push(`/cis/${saved.id}`);
    }
  } catch (err) {
    error.value = err;
    window.scrollTo({ top: 0 });
  }
}

function attrHint(d: (typeof defs.value)[number]): string | undefined {
  const ro = readOnly.value.has(`${ATTRIBUTE_PREFIX}${d.key}`) ? "read-only" : "";
  const parent = lookupParent(d);
  return [hintFor(d), parent ? `depends on ${parent.label}` : "", d.inherited ? `from ${d.definedOn.name}` : "", d.isActive ? "" : "retired attribute", ro].filter(Boolean).join(" · ") || undefined;
}

const CORE_IDS: Record<string, string> = { name: "f-name", statusId: "f-status" };
function fieldIdFor(key: string): string {
  return key.startsWith("attributes.") ? `attr-${key.slice(11)}` : (CORE_IDS[key] ?? key);
}

function coreToApi(c: CoreValues): Record<CoreField, string | null> {
  const out = {} as Record<CoreField, string | null>;
  for (const k of CORE_FIELDS) {
    const v = c[k].trim();
    out[k] = v === "" ? null : k === "notes" ? c[k] : v;
  }
  // name and statusId are required; send "" rather than null so the API reports them as fields.
  if (out.name === null) out.name = "";
  if (out.statusId === null) out.statusId = "";
  return out;
}

function referenceNames(ci: Ci | undefined): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(ci?.attributeReferences ?? {})) out[k] = v.hidden ? HIDDEN_CI : (v.name ?? v.id);
  return out;
}
</script>

<template>
  <form novalidate :aria-label="mode === 'create' ? `New ${className}` : `Edit ${ci?.name}`" @submit.prevent="onSubmit">
    <FormErrorBanner v-if="error != null" :error="error" :unplaced="unplaced" :version-conflict-href="ci ? `/cis/${ci.id}` : undefined" />
    <div v-if="noStatuses" class="alert alert-warn" role="alert">
      <strong>No statuses are defined yet.</strong> Every configuration item needs one.
      <template v-if="session.can('datamodel.manage')">
        Add them under <RouterLink to="/admin/lookups/statuses">Administration › Lookups</RouterLink>, or install the
        <RouterLink to="/admin/templates">IT infrastructure starter</RouterLink>.
      </template>
      <template v-else>Ask an administrator to add statuses under Administration › Lookups.</template>
    </div>
    <details v-for="sec in sections" :key="sec.key" class="panel layout-panel" :open="!sec.collapsed">
      <summary class="panel-header">
        <h2>{{ sec.label }}</h2>
        <span v-if="sec.subtitle" class="muted">{{ sec.subtitle }}</span>
      </summary>
      <div class="panel-body">
        <template v-if="sec.attributes">
          <LoadingState v-if="attrs.isLoading.value" label="Loading attribute definitions…" />
          <ErrorAlert
            v-if="attrs.isError.value"
            :error="attrs.error.value"
            title="Could not load this class's attributes"
            :on-retry="() => attrs.refetch()"
          />
          <p v-if="sec.key === 'attributes' && attrs.data.value && defs.length === 0" class="muted">This class defines no extra attributes.</p>
        </template>
        <component :is="g.legend === null ? 'div' : 'fieldset'" v-for="g in sec.groups" :key="g.legend ?? ''" :class="{ group: g.legend !== null }">
          <legend v-if="g.legend !== null">{{ g.legend }}</legend>
          <div class="form-grid">
            <template v-for="f in g.fields" :key="f">
              <FormField v-if="BUILTIN.has(f)" :id="FIELD_IDS[f]" v-slot="p" :label="BUILTIN.get(f)!.label" :required="f === 'name' || f === 'status'" :error="coreError(f)" :hint="coreHint(f)" :wide="f === 'notes'">
                <fieldset class="ro-wrap" :disabled="readOnly.has(f)">
                  <input v-if="f === 'name'" :id="p.id" v-model="core.name" v-autofocus type="text" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
                  <LookupSelect v-else-if="f === 'status'" v-model="core.statusId" kind="statuses" :id="p.id" :invalid="p.invalid" :described-by="p.describedBy" empty-label="Choose a status…" required />
                  <LookupSelect v-else-if="f === 'environment'" v-model="core.environmentId" kind="environments" :id="p.id" :invalid="p.invalid" :described-by="p.describedBy" empty-label="— none —" />
                  <LookupSelect v-else-if="f === 'owner'" v-model="core.ownerId" kind="owners" :id="p.id" :invalid="p.invalid" :described-by="p.describedBy" empty-label="— none —" />
                  <LookupSelect v-else-if="f === 'location'" v-model="core.locationId" kind="locations" :id="p.id" :invalid="p.invalid" :described-by="p.describedBy" empty-label="— none —" />
                  <textarea v-else-if="f === 'notes'" :id="p.id" v-model="core.notes" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
                  <input
                    v-else-if="f === 'hostname' || f === 'ipAddress' || f === 'serialNumber'"
                    :id="p.id"
                    v-model="core[f]"
                    type="text"
                    class="mono"
                    spellcheck="false"
                    :aria-invalid="p.invalid || undefined"
                    :aria-describedby="p.describedBy"
                  />
                </fieldset>
              </FormField>
              <FormField
                v-else-if="defFor(f)"
                :id="`attr-${defFor(f)!.key}`"
                v-slot="p"
                :label="defFor(f)!.label"
                :required="defFor(f)!.isRequired"
                :error="fieldErrors[f]"
                :hint="attrHint(defFor(f)!)"
              >
                <fieldset class="ro-wrap" :disabled="readOnly.has(f)">
                  <AttributeInput
                    v-model="values[defFor(f)!.key]"
                    :def="defFor(f)!"
                    :id="p.id"
                    :invalid="p.invalid"
                    :described-by="p.describedBy"
                    :reference-name="refNames[defFor(f)!.key]"
                    :lookup-parent="lookupParent(defFor(f)!)"
                    @reference-name="(name) => (refNames[defFor(f)!.key] = name)"
                  />
                </fieldset>
              </FormField>
            </template>
          </div>
        </component>
      </div>
    </details>
    <div class="panel form-footer">
      <button type="submit" class="btn btn-primary" :disabled="pending || attrs.isLoading.value || attrs.isError.value">
        {{ pending ? "Saving…" : mode === "create" ? `Create ${className}` : "Save changes" }}
      </button>
      <RouterLink class="btn" :to="ci ? `/cis/${ci.id}` : '/cis'">Cancel</RouterLink>
    </div>
  </form>
</template>
