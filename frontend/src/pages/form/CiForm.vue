<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { useIsPersonClass, useSignInAccount } from "../../api/admin";
import { ApiError } from "../../api/client";
import {
  useCiClasses,
  useClassAttributes,
  useCreateCi,
  useCriticalityValues,
  useUpdateCi,
  type Ci,
  type CiCreateBody,
  type CiUpdateBody,
} from "../../api/queries";
import { useCiLayout } from "../../api/uiSettings";
import AttributeInput from "../../components/AttributeInput.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LayoutEditView from "../../components/layoutEdit/LayoutEditView.vue";
import LoadingState from "../../components/LoadingState.vue";
import type { LookupParent } from "../../components/LookupValueSelect.vue";
import { useAppSettings } from "../../lib/appSettings";
import { HIDDEN_CI } from "../../lib/format";
import { t } from "../../i18n";
import { hintFor, nowFormValue, NOW_HINT, toApiValue, toFormValue, type FormValue } from "../../lib/attributeValues";
import type { LayoutEditor } from "../../lib/layoutEditor";
import { asClassLayout } from "../../lib/layoutTemplates";
import { createReusableTemplate } from "../../lib/reusableTemplate";
import {
  ATTRIBUTE_PREFIX,
  attributeKey,
  BUILTIN,
  builtInLayout,
  cellClass,
  CORE_FIELDS,
  freeAreaStyle,
  gridClass,
  layoutFor,
  normalizeLayout,
  PANELS,
  resolveLayout,
  sectionClass,
  sectionStyle,
  windowClass,
  windowStyle,
  withoutKinds,
  type ResolvedSection,
} from "../../lib/uiSettings";
import { useFlashStore } from "../../stores/flash";
import { useSessionStore } from "../../stores/session";
import NoteText from "../../components/NoteText.vue";
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
 * A class layout (Administration › Customization › Detail and form layout) arranges
 * the fields in tabs and sections (windows), hides fields and makes fields
 * read-only. Required fields stay editable on a new CI whatever the layout says,
 * or it could never be saved. Every tab stays in the page (only one is shown),
 * so the whole form is submitted and a tab holding an error says so.
 *
 * In layout edit mode (`editor` active, see lib/layoutEditor) the form's fields
 * are shown on the layout canvas instead, inert, with what has been typed so far.
 */
const props = defineProps<{ mode: "create" | "edit"; classId: string; className: string; ci?: Ci; editor?: LayoutEditor }>();
/** One field of the form, as the form and the layout canvas show it. */
const [DefineField, FormCell] = createReusableTemplate<{ f: string; width?: number; columns?: number }>();

/**
 * Core CI fields (CORE_FIELDS, without criticality, which is a lookup value of its own). These belong to every CI
 * regardless of class; class-specific fields come from the API.
 */
type CoreField = "ident" | "validFrom" | "validUntil";
type CoreValues = Record<CoreField, string>;

const router = useRouter();
const route = useRoute();
/** Created from the business service list: back there on Cancel, on to the service (Owners open) on save. */
const fromServices = computed(() => props.mode === "create" && route.query.return === "services");
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
// Criticality: a core field of every CI (a value of the system lookup list), placed, hidden or made read-only
// by the layout like the others; empty means not set.
const criticality = useCriticalityValues();
const initialCriticality = props.ci?.criticality?.id ?? "";
const criticalityId = ref(initialCriticality);
const criticalityOptions = computed(() => (criticality.data.value ?? []).filter((v) => v.isActive || v.id === criticalityId.value));
const criticalityStray = computed(
  () => !!criticalityId.value && !!criticality.data.value && !criticalityOptions.value.some((v) => v.id === criticalityId.value),
);
const values = ref<Record<string, FormValue>>({});
let initialValues: Record<string, FormValue> = {};
const refNames = ref<Record<string, string>>(referenceNames(props.ci));
const error = ref<unknown>(null);
const missing = ref<Record<string, string>>({});

const defs = computed(() => (attrs.data.value ?? []).filter((d) => d.isActive || (props.ci && props.ci.attributes[d.key] != null)));

const settings = useAppSettings();
const classes = useCiClasses();
const classKey = computed(() => classes.data.value?.find((c) => c.id === props.classId)?.key);
/** An existing CI's layout: its own, a template chosen for it, or its class's default (a new CI gets the class's). */
const ciLayout = useCiLayout(() => (props.mode === "edit" ? props.ci?.id : undefined));
/** The layout; in layout edit mode the draft, so the fields show what is being set (read-only). */
const layout = computed(() => {
  if (props.editor?.active && props.editor.layout) return props.editor.layout;
  const own = ciLayout.data.value;
  if (own && own.ciId === props.ci?.id) return normalizeLayout(asClassLayout(own.classKey, own.layout));
  return layoutFor(settings.doc.value, classKey.value);
});
const activeAttrs = computed(() => attrs.data.value?.filter((d) => d.isActive));
const requiredField = (f: string) => !!defs.value.find((d) => `${ATTRIBUTE_PREFIX}${d.key}` === f && d.isRequired && d.isActive);
const keepEditable = (f: string) => props.mode === "create" && requiredField(f);
/**
 * A Person linked to a sign-in account (SHAA-1505 decision 5): its Email follows the account's and the API refuses a
 * change (409 managed_by_user), so the field is read-only and names the account.
 */
const isPerson = useIsPersonClass(() => props.classId);
const signInAccount = useSignInAccount(() => props.ci?.id, () => props.mode === "edit" && isPerson.value);
const managedBy = computed(() => signInAccount.data.value?.account?.username ?? null);
const managedEmail = computed(() => {
  const d = managedBy.value ? defs.value.find((x) => x.systemRole === "person_email") : undefined;
  return d ? `${ATTRIBUTE_PREFIX}${d.key}` : null;
});
const readOnly = computed(() => {
  const ro = new Set((layout.value?.readOnlyFields ?? []).filter((f) => !keepEditable(f)));
  if (managedEmail.value) ro.add(managedEmail.value);
  return ro;
});

/**
 * The class's layout, or the built-in one: General (core fields and ungrouped attributes), then the attribute groups.
 * Notes show on the form too; the built-in panels are the detail page's.
 */
const tabs = computed(() => {
  const l = withoutKinds(layout.value ?? builtInLayout(""), PANELS.map((p) => p.kind));
  return resolveLayout({ ...l, hiddenFields: (l.hiddenFields ?? []).filter((f) => !keepEditable(f)) }, defs.value, CORE_FIELDS);
});
/** A tab's sections: the windows (lib/freeLayout), then everything the layout does not place. */
function sectionGroups(sections: readonly ResolvedSection[]) {
  const windows = sections.filter((sec) => sec.frame);
  const flow = sections.filter((sec) => !sec.frame);
  return windows.length > 0 ? [{ free: true, items: windows }, { free: false, items: flow }] : [{ free: false, items: flow }];
}
/** The first section of fields, which also shows whether the attributes loaded. */
const firstGrid = computed(() => tabs.value[0]?.sections.find((sec) => sec.kind === "fields")?.key);
const activeTab = ref(0);
const tabIndex = computed(() => Math.min(activeTab.value, tabs.value.length - 1));
const tabFields = (i: number) => tabs.value[i]?.sections.flatMap((sec) => sec.fields.map((c) => c.field)) ?? [];
const tabErrorCount = (i: number) => tabFields(i).filter((f) => fieldErrors.value[f]).length;
/** Arrow keys, Home and End move between the tabs (the tab list is one stop in the tab order). */
function onTabKey(e: KeyboardEvent) {
  const n = tabs.value.length;
  const to = { ArrowRight: tabIndex.value + 1, ArrowLeft: tabIndex.value - 1 + n, Home: 0, End: n - 1 }[e.key];
  if (to === undefined) return;
  e.preventDefault();
  activeTab.value = to % n;
  void nextTick(() => document.getElementById(`form-tab-${tabs.value[activeTab.value].key}`)?.focus());
}
/** Shows the tab holding `field` and puts the cursor in it. */
async function focusField(field: string) {
  const i = tabs.value.findIndex((_, j) => tabFields(j).includes(field));
  if (i >= 0) activeTab.value = i;
  await nextTick();
  document.getElementById(fieldIdFor(field))?.focus();
}
const FIELD_IDS: Record<string, string> = { ident: "f-ident", criticality: "f-criticality", validFrom: "f-valid-from", validUntil: "f-valid-until" };
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
          : "Local time; empty: open-ended";
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
  const known = new Set<string>([...CORE_FIELDS, "criticalityValueId", "classId", "version", ...defs.value.map((d) => `attributes.${d.key}`)]);
  return error.value.details.filter((d) => !known.has(d.field));
});

async function onSubmit() {
  error.value = null;
  // Catch empty required fields before the round trip; everything else is validated by the API.
  const req: Record<string, string> = {};
  if (!core.value.validFrom) req.validFrom = "Required";
  const shown = new Set(tabs.value.flatMap((_, i) => tabFields(i)));
  for (const d of defs.value) {
    const f = `${ATTRIBUTE_PREFIX}${d.key}`;
    if (d.isRequired && d.isActive && shown.has(f) && (values.value[d.key] ?? "") === "") req[f] = "Required";
  }
  missing.value = req;
  if (Object.keys(req).length > 0) {
    await focusField(Object.keys(req)[0]);
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
      const body = { classId: props.classId, ...given, ...(criticalityId.value ? { criticalityValueId: criticalityId.value } : {}), attributes } as CiCreateBody;
      const created = await create.mutateAsync(body);
      flash.show(created.id, `Created ${created.label}.`);
      await router.push(fromServices.value ? { path: `/services/${created.id}`, query: { edit: "owners" } } : `/cis/${created.id}`);
    } else if (props.ci) {
      const initial = coreToApi(initialCore);
      const changed: Record<string, unknown> = {};
      // An emptied valid until clears it (open-ended); an emptied ident keeps the current one.
      for (const k of Object.keys(coreBody) as CoreField[]) if (coreBody[k] !== initial[k] && !(k === "ident" && coreBody[k] === null)) changed[k] = coreBody[k];
      if (criticalityId.value !== initialCriticality) changed.criticalityValueId = criticalityId.value || null;
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
    // Show the first tab with a rejected field, so the message next to it is in view.
    const withError = tabs.value.findIndex((_, i) => tabErrorCount(i) > 0);
    if (withError >= 0) activeTab.value = withError;
    window.scrollTo({ top: 0 });
  }
}

function attrHint(d: (typeof defs.value)[number]): string | undefined {
  const f = `${ATTRIBUTE_PREFIX}${d.key}`;
  if (f === managedEmail.value) return t("people.form.managedBy", { username: managedBy.value ?? "" });
  const ro = readOnly.value.has(f) ? "read-only" : "";
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
  <DefineField v-slot="{ f, width, columns }">
    <FormField
      v-if="BUILTIN.has(f)"
      :id="FIELD_IDS[f]"
      v-slot="p"
      :class="width ? cellClass(width, columns) : undefined"
      :label="BUILTIN.get(f)!.label"
      :error="coreError(f)"
      :hint="coreHint(f)"
      :required="f === 'validFrom'"
    >
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
        <select
          v-else-if="f === 'criticality'"
          :id="p.id"
          v-model="criticalityId"
          :disabled="criticality.isLoading.value"
          :aria-invalid="p.invalid || undefined"
          :aria-describedby="p.describedBy"
        >
          <option value="">{{ criticality.isLoading.value ? "Loading…" : criticality.isError.value ? "Could not load the list" : "— not set —" }}</option>
          <option v-for="v in criticalityOptions" :key="v.id" :value="v.id">{{ v.name }}{{ v.isActive ? "" : " (retired)" }}</option>
          <option v-if="criticalityStray" :value="criticalityId">{{ ci?.criticality?.name ?? "Unknown value" }}</option>
        </select>
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
      :class="width ? cellClass(width, columns) : undefined"
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
  </DefineField>
  <LayoutEditView v-if="editor?.active && classKey" :editor="editor" :class-name="className" :attrs="activeAttrs" :attrs-error="attrs.error.value" form>
    <template #field="{ field }"><FormCell :f="field" /></template>
  </LayoutEditView>
  <form v-else novalidate :aria-label="mode === 'create' ? `New ${className}` : `Edit ${ci?.label}`" @submit.prevent="onSubmit">
    <FormErrorBanner v-if="error != null" :error="error" :unplaced="unplaced" :version-conflict-href="ci ? `/cis/${ci.id}` : undefined" />
    <div class="layout-container">
      <div v-if="tabs.length > 1" class="tabs" role="tablist" aria-label="Form tabs">
        <button
          v-for="(t, i) in tabs"
          :id="`form-tab-${t.key}`"
          :key="t.key"
          type="button"
          role="tab"
          :aria-selected="i === tabIndex"
          :aria-controls="`form-tabpanel-${t.key}`"
          :tabindex="i === tabIndex ? 0 : -1"
          @click="activeTab = i"
          @keydown="onTabKey"
        >
          {{ t.label }}<span v-if="tabErrorCount(i) > 0" class="badge danger tab-errors">{{ tabErrorCount(i) }} error{{ tabErrorCount(i) === 1 ? "" : "s" }}</span>
        </button>
      </div>
      <div
        v-for="(t, i) in tabs"
        v-show="i === tabIndex"
        :id="`form-tabpanel-${t.key}`"
        :key="t.key"
        :role="tabs.length > 1 ? 'tabpanel' : undefined"
        :aria-labelledby="tabs.length > 1 ? `form-tab-${t.key}` : undefined"
      >
        <div v-for="g in sectionGroups(t.sections)" :key="String(g.free)" :class="g.free ? 'lg-free' : 'layout-panels'" :style="g.free ? freeAreaStyle(g.items) : undefined">
          <details
            v-for="sec in g.items"
            :key="sec.key"
            :class="['panel', 'layout-panel', ...(sec.frame ? [windowClass] : sectionClass(sec))]"
            :style="sec.frame ? windowStyle(sec.frame) : sectionStyle(sec)"
            :data-section="sec.key"
            :open="!sec.collapsed"
          >
            <summary class="panel-header">
              <h2>{{ sec.label }}</h2>
            </summary>
            <div v-if="sec.kind === 'note'" class="panel-body"><NoteText :text="sec.text ?? ''" /></div>
            <div v-else class="panel-body">
              <template v-if="i === 0 && sec.key === firstGrid">
                <LoadingState v-if="attrs.isLoading.value" label="Loading attribute definitions…" />
                <ErrorAlert
                  v-if="attrs.isError.value"
                  :error="attrs.error.value"
                  title="Could not load this class's attributes"
                  :on-retry="() => attrs.refetch()"
                />
              </template>
              <div :class="gridClass(sec.columns)">
                <FormCell v-for="{ field: f, width } in sec.fields" :key="f" :f="f" :width="width" :columns="sec.columns" />
              </div>
            </div>
          </details>
        </div>
      </div>
    </div>
    <div class="panel form-footer">
      <button type="submit" class="btn btn-primary" :disabled="pending || attrs.isLoading.value || attrs.isError.value">
        {{ pending ? "Saving…" : mode === "create" ? `Create ${className}` : "Save changes" }}
      </button>
      <RouterLink class="btn" :to="ci ? `/cis/${ci.id}` : fromServices ? '/services' : '/cis'">Cancel</RouterLink>
    </div>
  </form>
</template>
