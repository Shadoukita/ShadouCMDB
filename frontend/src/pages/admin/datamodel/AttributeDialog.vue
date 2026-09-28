<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { RouterLink } from "vue-router";
import { ApiError } from "../../../api/client";
import {
  useCreateAttribute,
  useLookupLists,
  usePatch,
  type AttributeCreateBody,
  type AttributeDefinition,
  type AttributeUpdateBody,
  type DataType,
} from "../../../api/datamodel";
import { useCiClasses, useClassAttributes, type CiClass } from "../../../api/queries";
import AttributeInput from "../../../components/AttributeInput.vue";
import FormDialog from "../../../components/FormDialog.vue";
import SchemaChangeDialog from "../../../components/SchemaChangeDialog.vue";
import TechnicalNameField from "../../../components/TechnicalNameField.vue";
import { toApiValue, toFormValue, type AttributeShape } from "../../../lib/attributeValues";
import { DATA_TYPES, validationKind } from "../../../lib/dataTypes";
import { keyError } from "../../../lib/keys";
import { useSchemaChangeFlow } from "../../../lib/schemaChange";
import { flattenTree } from "../../../lib/tree";
import FormErrorBanner from "../../form/FormErrorBanner.vue";
import FormField from "../../form/FormField.vue";

/**
 * Add or edit one attribute definition: a typed column of the class's table.
 * The technical name (the column), referenced class and lookup list are fixed
 * after creation. The data type can change between the plain types; the server
 * dry-runs the conversion of every stored value and refuses the change if one
 * would not convert. Every save is previewed as DDL first. The default value is
 * entered with the same input the CI form uses for that type. A lookup attribute
 * on a list with a parent list names its parent field: the attribute (on this
 * class or an ancestor) bound to the parent list, whose value narrows its choices.
 */
const props = defineProps<{
  open: boolean;
  cls: CiClass;
  def: AttributeDefinition | null;
  sections: string[];
  defaultSection?: string;
  nextSortOrder: number;
}>();
const emit = defineEmits<{ close: []; saved: [message: string] }>();

const classes = useCiClasses();
const lists = useLookupLists();
const classAttrs = useClassAttributes(() => props.cls.id);
const create = useCreateAttribute();
const update = usePatch<AttributeDefinition>("attribute-definitions");
const flow = useSchemaChangeFlow();
const busy = computed(() => create.isPending.value || update.isPending.value || flow.state.loading);
const isNew = computed(() => !props.def);

const label = ref("");
const key = ref("");
const dataType = ref<DataType>("text");
const referenceClassId = ref("");
const lookupListId = ref("");
const parentAttributeId = ref("");
const isRequired = ref(false);
const groupName = ref("");
const helpText = ref("");
const description = ref("");
const enumText = ref("");
const defaultValue = ref("");
// v-model on type="number" inputs yields numbers once typed in; "" while empty.
const vMin = ref<string | number>("");
const vMax = ref<string | number>("");
const vMaxLength = ref<string | number>("");
const vPattern = ref("");
const vMultiline = ref(false);
const vUnit = ref("");
const error = ref<unknown>(null);
const local = ref<Record<string, string>>({});

interface Validation {
  min?: number;
  max?: number;
  maxLength?: number;
  pattern?: string;
  unit?: string;
  multiline?: boolean;
}

function seed() {
  const d = props.def;
  const v = (d?.validation ?? {}) as Validation;
  label.value = d?.label ?? "";
  key.value = d?.key ?? "";
  dataType.value = (d?.dataType as DataType) ?? "text";
  referenceClassId.value = d?.referenceClassId ?? "";
  lookupListId.value = d?.lookupListId ?? "";
  parentAttributeId.value = d?.parentAttributeId ?? "";
  isRequired.value = d?.isRequired ?? false;
  groupName.value = d ? (d.groupName ?? "") : (props.defaultSection ?? "");
  helpText.value = d?.helpText ?? "";
  description.value = d?.description ?? "";
  enumText.value = (d?.enumValues ?? []).join("\n");
  defaultValue.value = d ? toFormValue(d, d.defaultValue) : "";
  vMin.value = v.min?.toString() ?? "";
  vMax.value = v.max?.toString() ?? "";
  vMaxLength.value = v.maxLength?.toString() ?? "";
  vPattern.value = v.pattern ?? "";
  vMultiline.value = v.multiline === true;
  vUnit.value = v.unit ?? "";
  error.value = null;
  local.value = {};
  create.reset();
  update.reset();
}
watch(
  () => [props.open, props.def] as const,
  ([open]) => open && seed(),
  { immediate: true },
);
// A default belongs to one type; changing the type clears it.
watch(dataType, (t) => {
  const d = props.def;
  defaultValue.value = d && t === d.dataType ? toFormValue(d, d.defaultValue) : "";
});

/** Types a stored column can be converted between; reference and lookup columns are foreign keys and stay what they are. */
const CONVERTIBLE = new Set<DataType>(["text", "number", "integer", "boolean", "enum", "date", "datetime", "ip", "cidr"]);
const typeLocked = computed(() => !isNew.value && !CONVERTIBLE.has(props.def!.dataType as DataType));
const typeOptions = computed(() => (isNew.value ? DATA_TYPES : DATA_TYPES.filter((t) => (typeLocked.value ? t.key === dataType.value : CONVERTIBLE.has(t.key as DataType)))));
const typeChanged = computed(() => !isNew.value && dataType.value !== props.def?.dataType);

const enumValues = computed(() =>
  enumText.value
    .split("\n")
    .map((s) => s.trim())
    .filter(Boolean),
);
/** The attribute as the CI form would see it, for the default-value input. */
const draft = computed<AttributeShape>(() => ({
  dataType: dataType.value,
  enumValues: dataType.value === "enum" ? enumValues.value : null,
  // Only the flag that changes the input: a multi-line default value gets a text area too.
  validation: dataType.value === "text" && vMultiline.value ? { multiline: true } : null,
  referenceClassId: referenceClassId.value || null,
  lookupListId: lookupListId.value || null,
}));
const vKind = computed(() => validationKind(dataType.value));
const concreteClasses = computed(() => flattenTree(classes.data.value ?? []));
const typeHint = computed(() => DATA_TYPES.find((t) => t.key === dataType.value)?.hint || undefined);
const refClassName = computed(() => classes.data.value?.find((c) => c.id === referenceClassId.value)?.name);
const listName = computed(() => lists.data.value?.find((l) => l.id === lookupListId.value)?.name);
/** The parent list of the chosen list, if it depends on one. */
const parentList = computed(() => {
  const pid = lists.data.value?.find((l) => l.id === lookupListId.value)?.parentListId;
  return pid ? lists.data.value?.find((l) => l.id === pid) : undefined;
});
/** Lookup attributes of this class and its ancestors bound to the parent list. */
const parentCandidates = computed(() =>
  parentList.value
    ? (classAttrs.data.value ?? []).filter((a) => a.dataType === "lookup" && a.lookupListId === parentList.value!.id && a.id !== props.def?.id)
    : [],
);
// A new attribute on a dependent list starts with its parent field when there is only one candidate.
watch([lookupListId, parentCandidates], () => {
  if (!isNew.value) return;
  if (!parentCandidates.value.some((a) => a.id === parentAttributeId.value)) {
    parentAttributeId.value = parentCandidates.value.length === 1 ? parentCandidates.value[0].id : "";
  }
});

const apiErrors = computed(() => (error.value instanceof ApiError ? error.value.fieldErrors() : {}));
/** Errors for a field and its sub-paths (enumValues.2, validation.min). */
function errorFor(field: string): string | undefined {
  const messages = Object.entries({ ...apiErrors.value, ...local.value })
    .filter(([k]) => k === field || k.startsWith(`${field}.`))
    .map(([, m]) => m);
  return messages.length ? messages.join("; ") : undefined;
}
/** An error on validation as a whole (not one of its rules) is shown next to the Multiline option. */
const validationError = computed(() => apiErrors.value.validation);
const PLACED = ["label", "key", "dataType", "referenceClassId", "lookupListId", "parentAttributeId", "enumValues", "validation", "groupName", "helpText", "description", "defaultValue", "isRequired"];
const unplaced = computed(() =>
  error.value instanceof ApiError ? error.value.details.filter((d) => !PLACED.some((f) => d.field === f || d.field.startsWith(`${f}.`))) : [],
);

function numberOrUndefined(raw: string | number): number | undefined {
  const s = String(raw).trim();
  if (s === "") return undefined;
  const n = Number(s);
  return Number.isFinite(n) ? n : undefined;
}

function validation(): Validation | null {
  const v: Validation = {};
  if (vKind.value === "number") {
    if (numberOrUndefined(vMin.value) !== undefined) v.min = numberOrUndefined(vMin.value);
    if (numberOrUndefined(vMax.value) !== undefined) v.max = numberOrUndefined(vMax.value);
    if (vUnit.value.trim()) v.unit = vUnit.value.trim();
  }
  if (vKind.value === "text") {
    if (numberOrUndefined(vMaxLength.value) !== undefined) v.maxLength = numberOrUndefined(vMaxLength.value);
    if (vPattern.value.trim()) v.pattern = vPattern.value.trim();
    // Cleared by leaving the key out: validation is replaced as a whole.
    if (vMultiline.value) v.multiline = true;
  }
  return Object.keys(v).length ? v : null;
}

function checkLocal(): Record<string, string> {
  const errs: Record<string, string> = {};
  if (!label.value.trim()) errs.label = "Required";
  if (isNew.value) {
    const k = keyError(key.value);
    if (k) errs.key = k;
    if (dataType.value === "reference" && !referenceClassId.value) errs.referenceClassId = "Choose the class it refers to";
    if (dataType.value === "lookup" && !lookupListId.value) errs.lookupListId = "Choose a lookup list";
  }
  if (dataType.value === "enum" && enumValues.value.length === 0) errs.enumValues = "Enter at least one value";
  for (const [field, raw] of [["validation.min", vMin.value], ["validation.max", vMax.value], ["validation.maxLength", vMaxLength.value]] as const) {
    if (String(raw).trim() !== "" && numberOrUndefined(raw) === undefined) errs[field] = "Must be a number";
  }
  return errs;
}

async function submit() {
  error.value = null;
  local.value = checkLocal();
  if (Object.keys(local.value).length > 0) return;
  const common = {
    label: label.value.trim(),
    isRequired: isRequired.value,
    groupName: groupName.value.trim() || null,
    helpText: helpText.value.trim() || null,
    description: description.value.trim() || null,
    validation: validation(),
    ...(dataType.value === "enum" ? { enumValues: enumValues.value } : {}),
  };
  const dv = dataType.value === "reference" ? null : (toApiValue(draft.value, defaultValue.value) as string | number | boolean | null);
  if (isNew.value) {
      const body: AttributeCreateBody = {
        ...common,
        classId: props.cls.id,
        key: key.value,
        dataType: dataType.value,
        sortOrder: props.nextSortOrder,
        ...(dataType.value === "reference" ? { referenceClassId: referenceClassId.value } : {}),
        ...(dataType.value === "lookup" ? { lookupListId: lookupListId.value } : {}),
        ...(dataType.value === "lookup" && parentList.value && parentAttributeId.value ? { parentAttributeId: parentAttributeId.value } : {}),
        ...(dv !== null ? { defaultValue: dv } : {}),
      };
    const outcome = await flow.run({
      title: `Add attribute “${body.label}” to ${props.cls.name}`,
      intro: `Adds the column “${body.key}” to the table ${props.cls.tableName}.`,
      preview: { operation: "createField", body },
      apply: () => create.mutateAsync(body),
      applyLabel: "Add attribute",
      alwaysShow: true,
    });
    if (outcome.status === "applied") {
      emit("saved", `Added attribute ${(outcome.result as AttributeDefinition).label}. It shows on ${props.cls.name} forms now.`);
      emit("close");
    } else if (outcome.status === "refused") error.value = outcome.error;
    return;
  }
  const d = props.def!;
  const body: AttributeUpdateBody = {
    ...common,
    ...(typeChanged.value ? { dataType: dataType.value } : {}),
    ...(dataType.value === "reference" ? {} : { defaultValue: dv }),
    ...(dataType.value === "lookup" && (parentAttributeId.value || null) !== d.parentAttributeId ? { parentAttributeId: parentAttributeId.value || null } : {}),
  };
  const outcome = await flow.run({
    title: `Save attribute “${body.label}”`,
    intro: typeChanged.value
      ? `Converts the column “${d.key}” from ${d.dataType} to ${dataType.value}. Every stored value was converted in a dry run; the change is refused if any would not convert.`
      : undefined,
    preview: { operation: "updateField", id: d.id, body },
    apply: () => update.mutateAsync({ id: d.id, body }),
    applyLabel: "Save attribute",
    alwaysShow: typeChanged.value || (isRequired.value && !d.isRequired),
  });
  if (outcome.status === "applied") {
    emit("saved", `Saved attribute ${(outcome.result as AttributeDefinition).label}.`);
    emit("close");
  } else if (outcome.status === "refused") error.value = outcome.error;
}
</script>

<template>
  <FormDialog
    :open="open"
    :title="isNew ? `New attribute on ${cls.name}` : `Edit attribute “${def?.label}”`"
    :submit-label="isNew ? 'Preview and add…' : 'Save attribute'"
    :busy="busy"
    wide
    @submit="submit"
    @cancel="emit('close')"
  >
    <FormErrorBanner v-if="error" :error="error" :unplaced="unplaced" />
    <div class="form-grid">
      <FormField id="ad-label" v-slot="p" label="Label" required :error="errorFor('label')">
        <input :id="p.id" v-model="label" type="text" maxlength="200" autofocus :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
      </FormField>
      <TechnicalNameField
        id="ad-key"
        v-model="key"
        kind="field"
        :name="label"
        :editable="isNew"
        :class-id="cls.id"
        :location="def ? `${cls.tableName}.${def.key}` : undefined"
        :error="errorFor('key')"
      />
      <FormField
        id="ad-type"
        v-slot="p"
        label="Data type"
        required
        :error="errorFor('dataType')"
        :hint="
          typeLocked
            ? 'Reference and lookup columns cannot change type'
            : typeChanged
              ? 'Stored values are converted; refused if any would not convert'
              : typeHint
        "
      >
        <select :id="p.id" v-model="dataType" :disabled="typeLocked" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy">
          <option v-for="t in typeOptions" :key="t.key" :value="t.key">{{ t.label }}</option>
        </select>
      </FormField>
      <FormField v-if="dataType === 'reference'" id="ad-ref-class" v-slot="p" label="Refers to class" :required="isNew" :error="errorFor('referenceClassId')" hint="Its subclasses are allowed too">
        <select v-if="isNew" :id="p.id" v-model="referenceClassId" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy">
          <option value="">Choose a class…</option>
          <option v-for="n in concreteClasses" :key="n.item.id" :value="n.item.id">{{ "  ".repeat(n.depth) }}{{ n.item.name }}</option>
        </select>
        <input v-else :id="p.id" type="text" readonly :value="refClassName ?? referenceClassId" :aria-describedby="p.describedBy" />
      </FormField>
      <FormField v-if="dataType === 'lookup'" id="ad-list" v-slot="p" label="Lookup list" :required="isNew" :error="errorFor('lookupListId')">
        <select v-if="isNew" :id="p.id" v-model="lookupListId" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy">
          <option value="">{{ lists.isLoading.value ? "Loading…" : lists.data.value?.length ? "Choose a list…" : "No lists yet" }}</option>
          <option v-for="l in lists.data.value ?? []" :key="l.id" :value="l.id">{{ l.name }}{{ l.isActive ? "" : " (archived)" }}</option>
        </select>
        <input v-else :id="p.id" type="text" readonly :value="listName ?? lookupListId" :aria-describedby="p.describedBy" />
        <span v-if="isNew" class="hint"><RouterLink to="/admin/dropdowns">Manage dropdowns</RouterLink></span>
      </FormField>
      <FormField
        v-if="dataType === 'lookup' && parentList"
        id="ad-parent-attr"
        v-slot="p"
        :label="`Parent field (${parentList.name})`"
        :error="errorFor('parentAttributeId')"
        :hint="
          parentCandidates.length
            ? `CI forms offer only the ${listName ?? 'values'} of the ${parentList.name} chosen in this field`
            : `No attribute of ${cls.name} or its parents uses the list ${parentList.name}: add one first. Without a parent field every value can be chosen.`
        "
      >
        <select :id="p.id" v-model="parentAttributeId" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy">
          <option value="">— none: every value can be chosen —</option>
          <option v-for="a in parentCandidates" :key="a.id" :value="a.id">
            {{ a.label }}{{ a.inherited ? ` (from ${a.definedOn.name})` : "" }}{{ a.isActive ? "" : " (retired)" }}
          </option>
        </select>
      </FormField>
      <FormField id="ad-section" v-slot="p" label="Form section" :error="errorFor('groupName')" hint="Attributes with the same section are shown together">
        <input :id="p.id" v-model="groupName" type="text" list="ad-sections" maxlength="100" placeholder="e.g. Hardware" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
        <datalist id="ad-sections">
          <option v-for="s in sections" :key="s" :value="s" />
        </datalist>
      </FormField>
      <div class="field">
        <span class="label">Required</span>
        <label class="checkbox-row">
          <input id="ad-required" v-model="isRequired" type="checkbox" />
          Every CI of this class must have a value
        </label>
        <span v-if="!isNew && isRequired && !def?.isRequired" class="hint">Refused while a CI of this class has no value</span>
        <span v-if="errorFor('isRequired')" class="error">{{ errorFor("isRequired") }}</span>
      </div>
      <FormField
        v-if="dataType === 'enum'"
        id="ad-enum"
        v-slot="p"
        label="Allowed values"
        required
        wide
        :error="errorFor('enumValues')"
        hint="One per line. A value that CIs still hold cannot be removed."
      >
        <textarea :id="p.id" v-model="enumText" rows="4" class="mono" spellcheck="false" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
      </FormField>
      <template v-if="vKind === 'number'">
        <FormField id="ad-min" v-slot="p" label="Minimum" :error="errorFor('validation.min')">
          <input :id="p.id" v-model="vMin" type="number" step="any" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
        </FormField>
        <FormField id="ad-max" v-slot="p" label="Maximum" :error="errorFor('validation.max')">
          <input :id="p.id" v-model="vMax" type="number" step="any" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
        </FormField>
        <FormField id="ad-unit" v-slot="p" label="Unit" :error="errorFor('validation.unit')" hint="Shown next to the value, e.g. GB">
          <input :id="p.id" v-model="vUnit" type="text" maxlength="20" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
        </FormField>
      </template>
      <template v-if="vKind === 'text'">
        <FormField id="ad-maxlength" v-slot="p" label="Maximum length" :error="errorFor('validation.maxLength')">
          <input :id="p.id" v-model="vMaxLength" type="number" min="1" step="1" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
        </FormField>
        <FormField id="ad-pattern" v-slot="p" label="Pattern" :error="errorFor('validation.pattern')" hint="Regular expression the value must match">
          <input :id="p.id" v-model="vPattern" type="text" class="mono" spellcheck="false" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
        </FormField>
        <div class="field">
          <span class="label">Multiline</span>
          <label class="checkbox-row">
            <input id="ad-multiline" v-model="vMultiline" type="checkbox" :aria-describedby="validationError ? 'ad-multiline-error' : undefined" />
            Text area that keeps line breaks, e.g. notes or runbook steps
          </label>
          <span v-if="validationError" id="ad-multiline-error" class="error">{{ validationError }}</span>
        </div>
      </template>
      <FormField
        id="ad-default"
        v-slot="p"
        label="Default value"
        :error="errorFor('defaultValue')"
        :hint="dataType === 'reference' ? 'References have no default' : 'Pre-filled on new CIs, and applied when a CI is created without a value'"
      >
        <input v-if="dataType === 'reference'" :id="p.id" type="text" disabled value="" :aria-describedby="p.describedBy" />
        <AttributeInput v-else :id="p.id" v-model="defaultValue" :def="draft" :invalid="p.invalid" :described-by="p.describedBy" />
      </FormField>
      <FormField id="ad-help" v-slot="p" label="Help text" wide :error="errorFor('helpText')" hint="Shown under the field on CI forms">
        <input :id="p.id" v-model="helpText" type="text" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
      </FormField>
      <FormField id="ad-description" v-slot="p" label="Description" wide :error="errorFor('description')" hint="For administrators: what the attribute is for">
        <textarea :id="p.id" v-model="description" rows="2" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
      </FormField>
    </div>
  </FormDialog>
  <SchemaChangeDialog :flow="flow" />
</template>
