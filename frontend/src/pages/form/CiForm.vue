<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { RouterLink, useRouter } from "vue-router";
import { ApiError } from "../../api/client";
import {
  useCiClasses,
  useClassAttributes,
  useCreateCi,
  useUpdateCi,
  type Ci,
  type CiCreateBody,
  type CiUpdateBody,
} from "../../api/queries";
import AttributeInput from "../../components/AttributeInput.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import type { LookupParent } from "../../components/LookupValueSelect.vue";
import { useAppSettings } from "../../lib/appSettings";
import { HIDDEN_CI } from "../../lib/format";
import { hintFor, nowFormValue, NOW_HINT, toApiValue, toFormValue, type FormValue } from "../../lib/attributeValues";
import { ATTRIBUTE_PREFIX, attributeKey, BUILTIN, builtInLayout, CORE_FIELDS, layoutFor, resolveLayout } from "../../lib/uiSettings";
import { useFlashStore } from "../../stores/flash";
import { useSessionStore } from "../../stores/session";
import FormErrorBanner from "./FormErrorBanner.vue";
import FormField from "./FormField.vue";

/**
 * The CI form. Core fields (ident, validity period) are the same for every CI; everything
 * else, name and status included, is a class attribute rendered from
 * GET /ci-classes/{id}/attributes, so a new class needs no frontend change.
 * The parent keys this component by class (create) or id+version (edit).
 *
 * Every class starts with a General section: ident, valid from, valid until and
 * the attributes without a group; the other attribute groups follow as sections.
 * A class layout (Administration › Customization › Detail and form layout) adds
 * panels before that, hides fields and makes fields read-only. Required fields
 * stay editable on a new CI whatever the layout says, or it could never be saved.
 */
const props = defineProps<{ mode: "create" | "edit"; classId: string; className: string; ci?: Ci }>();

/** Core CI fields (CORE_FIELDS). These belong to every CI regardless of class; class-specific fields come from the API. */
type CoreField = "ident" | "validFrom" | "validUntil";
type CoreValues = Record<CoreField, string>;

const router = useRouter();
const flash = useFlashStore();
const session = useSessionStore();
/** Only administrators choose or change an ident; everyone else gets a generated one. */
const isAdmin = computed(() => !!session.permissions?.administrator);
const attrs = useClassAttributes(() => props.classId);
const create = useCreateCi();
const update = useUpdateCi(() => props.ci?.id ?? "");
const pending = computed(() => (props.mode === "create" ? create.isPending.value : update.isPending.value));

// The validity period is edited in local time (datetime-local inputs) and sent as ISO timestamps.
// A new CI is valid from the moment the form opens.
const DATETIME = { dataType: "datetime" } as const;
const initialCore: CoreValues = props.ci
  ? { ident: props.ci.ident, validFrom: toFormValue(DATETIME, props.ci.validFrom), validUntil: toFormValue(DATETIME, props.ci.validUntil) }
  : { ident: "", validFrom: nowFormValue("datetime"), validUntil: "" };
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
const requiredField = (f: string) => !!defs.value.find((d) => `${ATTRIBUTE_PREFIX}${d.key}` === f && d.isRequired && d.isActive);
const keepEditable = (f: string) => props.mode === "create" && requiredField(f);
const readOnly = computed(() => new Set((layout.value?.readOnlyFields ?? []).filter((f) => !keepEditable(f))));

/** The class's layout, or the built-in one: General (core fields and ungrouped attributes), then the attribute groups. */
const sections = computed(() => {
  const l = layout.value ?? builtInLayout("");
  return resolveLayout({ ...l, hiddenFields: (l.hiddenFields ?? []).filter((f) => !keepEditable(f)) }, defs.value, CORE_FIELDS);
});
const FIELD_IDS: Record<string, string> = { ident: "f-ident", validFrom: "f-valid-from", validUntil: "f-valid-until" };
const defFor = (f: string) => defs.value.find((d) => d.key === attributeKey(f));
/** A dependent lookup's parent field (Manufacturer for Model): its label and the value chosen in it. */
function lookupParent(d: (typeof defs.value)[number]): LookupParent | null {
  const p = d.parentAttributeId ? attrs.data.value?.find((a) => a.id === d.parentAttributeId) : undefined;
  return p ? { label: p.label, value: values.value[p.key] ?? "" } : null;
}
const coreError = (f: string) => fieldErrors.value[BUILTIN.get(f)?.form ?? f];
/** The ident is editable for administrators only (the API refuses anyone else). */
const coreDisabled = (f: string) => readOnly.value.has(f) || (f === "ident" && !isAdmin.value);
function coreHint(f: string): string | undefined {
  const create = props.mode === "create";
  const hint = f === "ident" ? (create && isAdmin.value ? "Generated when left empty" : create ? "Generated" : "") : f === "validFrom" ? "Local time" : "Local time; empty: open-ended";
  return [hint, coreDisabled(f) ? "read-only" : ""].filter(Boolean).join(" · ") || undefined;
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
  if (!core.value.validFrom) req.validFrom = "Required";
  const shown = new Set(sections.value.flatMap((sec) => sec.fields));
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
      // Empty core fields are left out: the API generates the ident and starts the validity period now.
      const given = Object.fromEntries(Object.entries(coreBody).filter(([k, v]) => v !== null && (k !== "ident" || isAdmin.value)));
      const body = { classId: props.classId, ...given, attributes } as CiCreateBody;
      const created = await create.mutateAsync(body);
      flash.show(created.id, `Created ${created.label}.`);
      await router.push(`/cis/${created.id}`);
    } else if (props.ci) {
      const initial = coreToApi(initialCore);
      const changed: Record<string, unknown> = {};
      // An emptied valid until clears it (open-ended); an emptied ident keeps the current one.
      for (const k of CORE_FIELDS as CoreField[]) if (coreBody[k] !== initial[k] && !(k === "ident" && coreBody[k] === null)) changed[k] = coreBody[k];
      if (Object.keys(changed).length === 0 && Object.keys(attributes).length === 0) {
        await router.push(`/cis/${props.ci.id}`);
        return;
      }
      const body = { ...changed, ...(Object.keys(attributes).length ? { attributes } : {}), version: props.ci.version } as CiUpdateBody;
      const saved = await update.mutateAsync(body);
      flash.show(saved.id, `Saved ${saved.label}.`);
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

function fieldIdFor(key: string): string {
  return key.startsWith("attributes.") ? `attr-${key.slice(11)}` : (FIELD_IDS[key] ?? key);
}

/** Core values for the API; null for an empty one (a new CI leaves those out, so the API fills them in). */
function coreToApi(c: CoreValues): Record<CoreField, string | null> {
  return {
    ident: c.ident.trim() || null,
    validFrom: c.validFrom ? (toApiValue(DATETIME, c.validFrom) as string) : null,
    validUntil: c.validUntil ? (toApiValue(DATETIME, c.validUntil) as string) : null,
  };
}

function referenceNames(ci: Ci | undefined): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(ci?.attributeReferences ?? {})) out[k] = v.hidden ? HIDDEN_CI : (v.name ?? v.id);
  return out;
}
</script>

<template>
  <form novalidate :aria-label="mode === 'create' ? `New ${className}` : `Edit ${ci?.label}`" @submit.prevent="onSubmit">
    <FormErrorBanner v-if="error != null" :error="error" :unplaced="unplaced" :version-conflict-href="ci ? `/cis/${ci.id}` : undefined" />
    <details v-for="(sec, i) in sections" :key="sec.key" class="panel layout-panel" :open="!sec.collapsed">
      <summary class="panel-header">
        <h2>{{ sec.label }}</h2>
      </summary>
      <div class="panel-body">
        <template v-if="i === 0">
          <LoadingState v-if="attrs.isLoading.value" label="Loading attribute definitions…" />
          <ErrorAlert
            v-if="attrs.isError.value"
            :error="attrs.error.value"
            title="Could not load this class's attributes"
            :on-retry="() => attrs.refetch()"
          />
        </template>
        <div class="form-grid">
          <template v-for="f in sec.fields" :key="f">
            <FormField v-if="BUILTIN.has(f)" :id="FIELD_IDS[f]" v-slot="p" :label="BUILTIN.get(f)!.label" :error="coreError(f)" :hint="coreHint(f)" :required="f === 'validFrom'">
              <fieldset class="ro-wrap" :disabled="coreDisabled(f)">
                <input
                  v-if="f === 'ident'"
                  :id="p.id"
                  v-model="core.ident"
                  type="text"
                  class="mono"
                  spellcheck="false"
                  :placeholder="mode === 'create' ? 'Generated' : undefined"
                  :aria-invalid="p.invalid || undefined"
                  :aria-describedby="p.describedBy"
                />
                <input
                  v-else-if="f === 'validFrom' || f === 'validUntil'"
                  :id="p.id"
                  v-model="core[f]"
                  type="datetime-local"
                  :title="NOW_HINT"
                  :aria-invalid="p.invalid || undefined"
                  :aria-describedby="p.describedBy"
                  @dblclick="core[f] = nowFormValue('datetime')"
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
