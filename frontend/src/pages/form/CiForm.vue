<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { RouterLink, useRouter } from "vue-router";
import { ApiError } from "../../api/client";
import {
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
import { groupAttributes } from "../../lib/attributes";
import { vAutofocus } from "../../lib/directives";
import { hintFor, toApiValue, toFormValue, type FormValue } from "../../lib/attributeValues";
import { useFlashStore } from "../../stores/flash";
import { useSessionStore } from "../../stores/session";
import FormErrorBanner from "./FormErrorBanner.vue";
import FormField from "./FormField.vue";

/**
 * The CI form. Core fields are the same for every CI; the class attributes are
 * rendered from GET /ci-classes/{id}/attributes, so a new class needs no frontend change.
 * The parent keys this component by class (create) or id+version (edit).
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
const groups = computed(() => groupAttributes(defs.value));

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
  for (const d of defs.value) if (d.isRequired && d.isActive && (values.value[d.key] ?? "") === "") req[`attributes.${d.key}`] = "Required";
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
  return [hintFor(d), d.inherited ? `from ${d.definedOn.name}` : "", d.isActive ? "" : "retired attribute"].filter(Boolean).join(" · ") || undefined;
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
  const refs = (ci?.attributeReferences ?? {}) as Record<string, unknown>;
  for (const [k, v] of Object.entries(refs)) {
    if (typeof v === "string") out[k] = v;
    else if (v && typeof v === "object" && "name" in v) out[k] = String((v as { name: unknown }).name);
  }
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
    <section class="panel">
      <div class="panel-header">
        <h2>General</h2>
        <span class="muted">Fields every CI has</span>
      </div>
      <div class="panel-body">
        <div class="form-grid">
          <FormField id="f-name" v-slot="p" label="Name" required :error="fieldErrors.name">
            <input :id="p.id" v-model="core.name" v-autofocus type="text" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
          </FormField>
          <FormField id="f-status" v-slot="p" label="Status" required :error="fieldErrors.statusId">
            <LookupSelect v-model="core.statusId" kind="statuses" :id="p.id" :invalid="p.invalid" :described-by="p.describedBy" empty-label="Choose a status…" required />
          </FormField>
          <FormField id="f-environment" v-slot="p" label="Environment" :error="fieldErrors.environmentId">
            <LookupSelect v-model="core.environmentId" kind="environments" :id="p.id" :invalid="p.invalid" :described-by="p.describedBy" empty-label="— none —" />
          </FormField>
          <FormField id="f-owner" v-slot="p" label="Owner" :error="fieldErrors.ownerId">
            <LookupSelect v-model="core.ownerId" kind="owners" :id="p.id" :invalid="p.invalid" :described-by="p.describedBy" empty-label="— none —" />
          </FormField>
          <FormField id="f-location" v-slot="p" label="Location" :error="fieldErrors.locationId">
            <LookupSelect v-model="core.locationId" kind="locations" :id="p.id" :invalid="p.invalid" :described-by="p.describedBy" empty-label="— none —" />
          </FormField>
          <FormField id="f-hostname" v-slot="p" label="Hostname" :error="fieldErrors.hostname">
            <input :id="p.id" v-model="core.hostname" type="text" class="mono" spellcheck="false" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
          </FormField>
          <FormField id="f-ip" v-slot="p" label="IP address" :error="fieldErrors.ipAddress" hint="IPv4 or IPv6">
            <input :id="p.id" v-model="core.ipAddress" type="text" class="mono" spellcheck="false" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
          </FormField>
          <FormField id="f-serial" v-slot="p" label="Serial number" :error="fieldErrors.serialNumber">
            <input :id="p.id" v-model="core.serialNumber" type="text" class="mono" spellcheck="false" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
          </FormField>
          <FormField id="f-notes" v-slot="p" label="Notes" :error="fieldErrors.notes" wide>
            <textarea :id="p.id" v-model="core.notes" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
          </FormField>
        </div>
      </div>
    </section>

    <section class="panel">
      <div class="panel-header">
        <h2>{{ className }} attributes</h2>
        <span class="muted">Defined by the class and its parents</span>
      </div>
      <div class="panel-body">
        <LoadingState v-if="attrs.isLoading.value" label="Loading attribute definitions…" />
        <ErrorAlert
          v-if="attrs.isError.value"
          :error="attrs.error.value"
          title="Could not load this class's attributes"
          :on-retry="() => attrs.refetch()"
        />
        <p v-if="attrs.data.value && defs.length === 0" class="muted">This class defines no extra attributes.</p>
        <fieldset v-for="[group, items] in groups" :key="group" class="group">
          <legend>{{ group }}</legend>
          <div class="form-grid">
            <FormField
              v-for="d in items"
              :id="`attr-${d.key}`"
              :key="d.id"
              v-slot="p"
              :label="d.label"
              :required="d.isRequired"
              :error="fieldErrors[`attributes.${d.key}`]"
              :hint="attrHint(d)"
            >
              <AttributeInput
                v-model="values[d.key]"
                :def="d"
                :id="p.id"
                :invalid="p.invalid"
                :described-by="p.describedBy"
                :reference-name="refNames[d.key]"
                @reference-name="(name) => (refNames[d.key] = name)"
              />
            </FormField>
          </div>
        </fieldset>
      </div>
      <div class="form-footer">
        <button type="submit" class="btn btn-primary" :disabled="pending || attrs.isLoading.value || attrs.isError.value">
          {{ pending ? "Saving…" : mode === "create" ? `Create ${className}` : "Save changes" }}
        </button>
        <RouterLink class="btn" :to="ci ? `/cis/${ci.id}` : '/cis'">Cancel</RouterLink>
      </div>
    </section>
  </form>
</template>
