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
import { useCiClasses, type CiClass } from "../../../api/queries";
import AttributeInput from "../../../components/AttributeInput.vue";
import FormDialog from "../../../components/FormDialog.vue";
import { toApiValue, toFormValue, type AttributeShape } from "../../../lib/attributeValues";
import { DATA_TYPES, validationKind } from "../../../lib/dataTypes";
import { keyError, suggestKey } from "../../../lib/keys";
import { flattenTree } from "../../../lib/tree";
import FormErrorBanner from "../../form/FormErrorBanner.vue";
import FormField from "../../form/FormField.vue";

/**
 * Add or edit one attribute definition. Key, data type, referenced class and
 * lookup list are fixed after creation (stored values depend on them); every
 * other setting can change. The default value is entered with the same input the
 * CI form uses for that type.
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
const create = useCreateAttribute();
const update = usePatch<AttributeDefinition>("attribute-definitions");
const busy = computed(() => create.isPending.value || update.isPending.value);
const isNew = computed(() => !props.def);

const label = ref("");
const key = ref("");
const keyTouched = ref(false);
const dataType = ref<DataType>("text");
const referenceClassId = ref("");
const lookupListId = ref("");
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
const vUnit = ref("");
const error = ref<unknown>(null);
const local = ref<Record<string, string>>({});

interface Validation {
  min?: number;
  max?: number;
  maxLength?: number;
  pattern?: string;
  unit?: string;
}

function seed() {
  const d = props.def;
  const v = (d?.validation ?? {}) as Validation;
  label.value = d?.label ?? "";
  key.value = d?.key ?? "";
  keyTouched.value = !!d;
  dataType.value = (d?.dataType as DataType) ?? "text";
  referenceClassId.value = d?.referenceClassId ?? "";
  lookupListId.value = d?.lookupListId ?? "";
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
watch(label, (l) => {
  if (isNew.value && !keyTouched.value) key.value = suggestKey(l);
});
// A default belongs to one type; changing the type (only possible before creation) clears it.
watch(dataType, () => {
  if (isNew.value) defaultValue.value = "";
});

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
  validation: null,
  referenceClassId: referenceClassId.value || null,
  lookupListId: lookupListId.value || null,
}));
const vKind = computed(() => validationKind(dataType.value));
const concreteClasses = computed(() => flattenTree(classes.data.value ?? []));
const typeHint = computed(() => DATA_TYPES.find((t) => t.key === dataType.value)?.hint || undefined);
const refClassName = computed(() => classes.data.value?.find((c) => c.id === referenceClassId.value)?.name);
const listName = computed(() => lists.data.value?.find((l) => l.id === lookupListId.value)?.name);

const apiErrors = computed(() => (error.value instanceof ApiError ? error.value.fieldErrors() : {}));
/** Errors for a field and its sub-paths (enumValues.2, validation.min). */
function errorFor(field: string): string | undefined {
  const messages = Object.entries({ ...apiErrors.value, ...local.value })
    .filter(([k]) => k === field || k.startsWith(`${field}.`))
    .map(([, m]) => m);
  return messages.length ? messages.join("; ") : undefined;
}
const PLACED = ["label", "key", "dataType", "referenceClassId", "lookupListId", "enumValues", "validation", "groupName", "helpText", "description", "defaultValue", "isRequired"];
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
  try {
    if (isNew.value) {
      const body: AttributeCreateBody = {
        ...common,
        classId: props.cls.id,
        key: key.value,
        dataType: dataType.value,
        sortOrder: props.nextSortOrder,
        ...(dataType.value === "reference" ? { referenceClassId: referenceClassId.value } : {}),
        ...(dataType.value === "lookup" ? { lookupListId: lookupListId.value } : {}),
        ...(dv !== null ? { defaultValue: dv } : {}),
      };
      const created = await create.mutateAsync(body);
      emit("saved", `Added attribute ${created.label}. It shows on ${props.cls.name} forms now.`);
    } else {
      const body: AttributeUpdateBody = { ...common, ...(dataType.value === "reference" ? {} : { defaultValue: dv }) };
      const saved = await update.mutateAsync({ id: props.def!.id, body });
      emit("saved", `Saved attribute ${saved.label}.`);
    }
    emit("close");
  } catch (e) {
    error.value = e;
  }
}
</script>

<template>
  <FormDialog
    :open="open"
    :title="isNew ? `New attribute on ${cls.name}` : `Edit attribute “${def?.label}”`"
    :submit-label="isNew ? 'Add attribute' : 'Save attribute'"
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
      <FormField id="ad-key" v-slot="p" label="Key" :required="isNew" :error="errorFor('key')" :hint="isNew ? 'Cannot change later' : 'Fixed after creation'">
        <input
          :id="p.id"
          v-model="key"
          type="text"
          class="mono"
          spellcheck="false"
          :readonly="!isNew"
          :aria-invalid="p.invalid || undefined"
          :aria-describedby="p.describedBy"
          @input="keyTouched = true"
        />
      </FormField>
      <FormField id="ad-type" v-slot="p" label="Data type" :required="isNew" :error="errorFor('dataType')" :hint="isNew ? typeHint : 'Fixed after creation'">
        <select :id="p.id" v-model="dataType" :disabled="!isNew" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy">
          <option v-for="t in DATA_TYPES" :key="t.key" :value="t.key">{{ t.label }}</option>
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
        <span v-if="isNew" class="hint"><RouterLink to="/admin/lookups/lists">Manage lookup lists</RouterLink></span>
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
</template>
