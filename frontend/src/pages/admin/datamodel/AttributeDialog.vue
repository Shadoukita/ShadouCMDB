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
import { t, type MessageKey } from "../../../i18n";
import { toApiValue, toFormValue, type AttributeShape } from "../../../lib/attributeValues";
import { changedFields } from "../../../lib/changes";
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
const isExpected = ref(false);
const isIdentifying = ref(false);
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
/** The edit body as the dialog opened, to send only changed fields. */
let initial: AttributeUpdateBody = {};

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
  isExpected.value = d?.isExpected ?? false;
  isIdentifying.value = d?.isIdentifying ?? false;
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
  initial = d ? updateBody() : {};
}
// A default belongs to one type; changing the type clears it.
watch(dataType, (type) => {
  const d = props.def;
  defaultValue.value = d && type === d.dataType ? toFormValue(d, d.defaultValue) : "";
});

/** Types a stored column can be converted between; reference and lookup columns are foreign keys and stay what they are. */
const CONVERTIBLE = new Set<DataType>(["text", "number", "integer", "boolean", "enum", "date", "datetime", "ip", "cidr"]);
/** The Person's Name and Email (SHAA-1505): the API refuses to archive them, make them optional or change their type. */
const systemField = computed(() => !!props.def?.systemRole);
const typeLocked = computed(() => !isNew.value && (systemField.value || !CONVERTIBLE.has(props.def!.dataType as DataType)));
const typeOptions = computed(() =>
  (isNew.value ? DATA_TYPES : DATA_TYPES.filter((d) => (typeLocked.value ? d.key === dataType.value : CONVERTIBLE.has(d.key)))).map((d) => ({
    key: d.key,
    label: t(`dm.attr.type.${d.key}` as MessageKey),
  })),
);
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
const typeHint = computed(() => (DATA_TYPES.find((d) => d.key === dataType.value)?.hint ? t(`dm.attr.typeHint.${dataType.value}` as MessageKey) : undefined));
const refClassName = computed(() => classes.data.value?.find((c) => c.id === referenceClassId.value)?.name);
const listName = computed(() => lists.data.value?.find((l) => l.id === lookupListId.value)?.name);
/** The parent list of the chosen list, if it depends on one. */
const parentList = computed(() => {
  const pid = lists.data.value?.find((l) => l.id === lookupListId.value)?.parentListId;
  return pid ? lists.data.value?.find((l) => l.id === pid) : undefined;
});
/** A parent-field choice: "Site (from Hardware) (retired)". */
function candidateName(a: { label: string; inherited: boolean; definedOn: { name: string }; isActive: boolean }): string {
  const name = a.inherited ? t("dm.attr.parent.from", { name: a.label, cls: a.definedOn.name }) : a.label;
  return a.isActive ? name : t("dm.attr.parent.retired", { name });
}
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
const PLACED = ["label", "key", "dataType", "referenceClassId", "lookupListId", "parentAttributeId", "enumValues", "validation", "groupName", "helpText", "description", "defaultValue", "isRequired", "isExpected", "isIdentifying"];
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
  if (!label.value.trim()) errs.label = t("common.required");
  if (isNew.value) {
    const k = keyError(key.value);
    if (k) errs.key = k;
    if (dataType.value === "reference" && !referenceClassId.value) errs.referenceClassId = t("dm.attr.err.referenceClass");
    if (dataType.value === "lookup" && !lookupListId.value) errs.lookupListId = t("dm.attr.err.lookupList");
  }
  if (dataType.value === "enum" && enumValues.value.length === 0) errs.enumValues = t("dm.attr.err.enumValues");
  for (const [field, raw] of [["validation.min", vMin.value], ["validation.max", vMax.value], ["validation.maxLength", vMaxLength.value]] as const) {
    if (String(raw).trim() !== "" && numberOrUndefined(raw) === undefined) errs[field] = t("dm.attr.err.number");
  }
  return errs;
}

function commonBody() {
  return {
    label: label.value.trim(),
    isRequired: isRequired.value,
    isExpected: isExpected.value,
    isIdentifying: isIdentifying.value,
    groupName: groupName.value.trim() || null,
    helpText: helpText.value.trim() || null,
    description: description.value.trim() || null,
    validation: validation(),
    ...(dataType.value === "enum" ? { enumValues: enumValues.value } : {}),
  };
}
function defaultBody() {
  return dataType.value === "reference" ? null : (toApiValue(draft.value, defaultValue.value) as string | number | boolean | null);
}
/** An edit's body with every field that may change; submit() sends the ones that differ from `initial`. */
function updateBody(): AttributeUpdateBody {
  const d = props.def!;
  return {
    ...commonBody(),
    ...(typeChanged.value ? { dataType: dataType.value } : {}),
    ...(dataType.value === "reference" ? {} : { defaultValue: defaultBody() }),
    ...(dataType.value === "lookup" && (parentAttributeId.value || null) !== d.parentAttributeId ? { parentAttributeId: parentAttributeId.value || null } : {}),
  };
}

// Below the computeds that seed() reads through updateBody(): it runs during setup.
watch(
  () => [props.open, props.def] as const,
  ([open]) => open && seed(),
  { immediate: true },
);

async function submit() {
  error.value = null;
  local.value = checkLocal();
  if (Object.keys(local.value).length > 0) return;
  const common = commonBody();
  const dv = defaultBody();
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
      title: t("dm.attr.create.title", { name: body.label, cls: props.cls.name }),
      intro: t("dm.attr.create.intro", { key: body.key, table: props.cls.tableName }),
      preview: { operation: "createField", body },
      apply: () => create.mutateAsync(body),
      applyLabel: t("dm.attr.add"),
      alwaysShow: true,
    });
    if (outcome.status === "applied") {
      emit("saved", t("dm.attr.toast.added", { name: (outcome.result as AttributeDefinition).label, cls: props.cls.name }));
      emit("close");
    } else if (outcome.status === "refused") error.value = outcome.error;
    return;
  }
  const d = props.def!;
  const full = updateBody();
  // Only what changed; a type change takes the settings that belong to the type with it.
  const body: AttributeUpdateBody = changedFields(full, initial);
  if (typeChanged.value) {
    for (const k of ["defaultValue", "validation", "enumValues"] as const) if (k in full) (body as Record<string, unknown>)[k] = full[k];
  }
  if (Object.keys(body).length === 0) {
    emit("close");
    return;
  }
  const outcome = await flow.run({
    title: t("dm.attr.save.title", { name: full.label ?? d.label }),
    intro: typeChanged.value ? t("dm.attr.save.convertIntro", { key: d.key, from: d.dataType, to: dataType.value }) : undefined,
    preview: { operation: "updateField", id: d.id, body },
    apply: () => update.mutateAsync({ id: d.id, body }),
    applyLabel: t("dm.attr.save.apply"),
    alwaysShow: typeChanged.value || (isRequired.value && !d.isRequired),
  });
  if (outcome.status === "applied") {
    emit("saved", t("dm.attr.toast.saved", { name: (outcome.result as AttributeDefinition).label }));
    emit("close");
  } else if (outcome.status === "refused") error.value = outcome.error;
}
</script>

<template>
  <FormDialog
    :open="open"
    :title="isNew ? t('dm.attr.dialog.newTitle', { cls: cls.name }) : t('dm.attr.dialog.editTitle', { name: def?.label ?? '' })"
    :submit-label="isNew ? t('dm.attr.dialog.submitNew') : t('dm.attr.save.apply')"
    :busy="busy"
    wide
    @submit="submit"
    @cancel="emit('close')"
  >
    <FormErrorBanner v-if="error" :error="error" :unplaced="unplaced" />
    <div class="form-grid">
      <FormField id="ad-label" v-slot="p" :label="t('dm.attr.col.label')" required :error="errorFor('label')">
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
        :label="t('dm.attr.f.dataType')"
        required
        :error="errorFor('dataType')"
        :hint="
          systemField
            ? t('people.datamodel.systemTitle')
            : typeLocked
            ? t('dm.attr.f.typeLocked')
            : typeChanged
              ? t('dm.attr.f.typeChanged')
              : typeHint
        "
      >
        <select :id="p.id" v-model="dataType" :disabled="typeLocked" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy">
          <option v-for="o in typeOptions" :key="o.key" :value="o.key">{{ o.label }}</option>
        </select>
      </FormField>
      <FormField
        v-if="dataType === 'reference'"
        id="ad-ref-class"
        v-slot="p"
        :label="t('dm.attr.f.refClass')"
        :required="isNew"
        :error="errorFor('referenceClassId')"
        :hint="t('dm.attr.f.refClassHint')"
      >
        <select v-if="isNew" :id="p.id" v-model="referenceClassId" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy">
          <option value="">{{ t("dm.attr.f.chooseClass") }}</option>
          <option v-for="n in concreteClasses" :key="n.item.id" :value="n.item.id">{{ "  ".repeat(n.depth) }}{{ n.item.name }}</option>
        </select>
        <input v-else :id="p.id" type="text" readonly :value="refClassName ?? referenceClassId" :aria-describedby="p.describedBy" />
      </FormField>
      <FormField v-if="dataType === 'lookup'" id="ad-list" v-slot="p" :label="t('dm.attr.f.lookupList')" :required="isNew" :error="errorFor('lookupListId')">
        <select v-if="isNew" :id="p.id" v-model="lookupListId" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy">
          <option value="">{{ lists.isLoading.value ? t("common.loading") : lists.data.value?.length ? t("dm.attr.f.chooseList") : t("dm.attr.f.noLists") }}</option>
          <option v-for="l in lists.data.value ?? []" :key="l.id" :value="l.id">{{ l.isActive ? l.name : t("dm.attr.f.listArchived", { name: l.name }) }}</option>
        </select>
        <input v-else :id="p.id" type="text" readonly :value="listName ?? lookupListId" :aria-describedby="p.describedBy" />
        <span v-if="isNew" class="hint"><RouterLink to="/admin/dropdowns">{{ t("dm.attr.f.manageDropdowns") }}</RouterLink></span>
      </FormField>
      <FormField
        v-if="dataType === 'lookup' && parentList"
        id="ad-parent-attr"
        v-slot="p"
        :label="t('dm.attr.parent.label', { list: parentList.name })"
        :error="errorFor('parentAttributeId')"
        :hint="
          parentCandidates.length
            ? t('dm.attr.parent.hint', { values: listName ?? t('dm.attr.parent.values'), parent: parentList.name })
            : t('dm.attr.parent.none', { cls: cls.name, list: parentList.name })
        "
      >
        <select :id="p.id" v-model="parentAttributeId" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy">
          <option value="">{{ t("dm.attr.parent.noneOption") }}</option>
          <option v-for="a in parentCandidates" :key="a.id" :value="a.id">
            {{ candidateName(a) }}
          </option>
        </select>
      </FormField>
      <FormField id="ad-section" v-slot="p" :label="t('dm.attr.f.section')" :error="errorFor('groupName')" :hint="t('dm.attr.f.sectionHint')">
        <input :id="p.id" v-model="groupName" type="text" list="ad-sections" maxlength="100" :placeholder="t('dm.attr.f.sectionPlaceholder')" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
        <datalist id="ad-sections">
          <option v-for="s in sections" :key="s" :value="s" />
        </datalist>
      </FormField>
      <div class="field">
        <span class="label">{{ t("common.required") }}</span>
        <label class="checkbox-row">
          <input id="ad-required" v-model="isRequired" type="checkbox" :disabled="systemField" />
          {{ t("dm.attr.f.requiredText") }}
        </label>
        <span v-if="systemField" class="hint">{{ t("people.datamodel.systemTitle") }}</span>
        <span v-if="!isNew && isRequired && !def?.isRequired" class="hint">{{ t("dm.attr.f.requiredRefused") }}</span>
        <span v-if="errorFor('isRequired')" class="error">{{ errorFor("isRequired") }}</span>
      </div>
      <div class="field">
        <span class="label">{{ t("dm.attr.f.expected") }}</span>
        <label class="checkbox-row">
          <input id="ad-expected" v-model="isExpected" type="checkbox" />
          {{ t("dm.attr.f.expectedText") }}
        </label>
        <span v-if="errorFor('isExpected')" class="error">{{ errorFor("isExpected") }}</span>
      </div>
      <div class="field">
        <span class="label">{{ t("dm.attr.f.identifying") }}</span>
        <label class="checkbox-row">
          <input id="ad-identifying" v-model="isIdentifying" type="checkbox" />
          {{ t("dm.attr.f.identifyingText") }}
        </label>
        <span v-if="errorFor('isIdentifying')" class="error">{{ errorFor("isIdentifying") }}</span>
      </div>
      <FormField
        v-if="dataType === 'enum'"
        id="ad-enum"
        v-slot="p"
        :label="t('dm.attr.f.enum')"
        required
        wide
        :error="errorFor('enumValues')"
        :hint="t('dm.attr.f.enumHint')"
      >
        <textarea :id="p.id" v-model="enumText" rows="4" class="mono" spellcheck="false" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
      </FormField>
      <template v-if="vKind === 'number'">
        <FormField id="ad-min" v-slot="p" :label="t('dm.attr.f.min')" :error="errorFor('validation.min')">
          <input :id="p.id" v-model="vMin" type="number" step="any" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
        </FormField>
        <FormField id="ad-max" v-slot="p" :label="t('dm.attr.f.max')" :error="errorFor('validation.max')">
          <input :id="p.id" v-model="vMax" type="number" step="any" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
        </FormField>
        <FormField id="ad-unit" v-slot="p" :label="t('dm.attr.f.unit')" :error="errorFor('validation.unit')" :hint="t('dm.attr.f.unitHint')">
          <input :id="p.id" v-model="vUnit" type="text" maxlength="20" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
        </FormField>
      </template>
      <template v-if="vKind === 'text'">
        <FormField id="ad-maxlength" v-slot="p" :label="t('dm.attr.f.maxLength')" :error="errorFor('validation.maxLength')">
          <input :id="p.id" v-model="vMaxLength" type="number" min="1" step="1" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
        </FormField>
        <FormField id="ad-pattern" v-slot="p" :label="t('dm.attr.f.pattern')" :error="errorFor('validation.pattern')" :hint="t('dm.attr.f.patternHint')">
          <input :id="p.id" v-model="vPattern" type="text" class="mono" spellcheck="false" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
        </FormField>
        <div class="field">
          <span class="label">{{ t("dm.attr.f.multiline") }}</span>
          <label class="checkbox-row">
            <input id="ad-multiline" v-model="vMultiline" type="checkbox" :aria-describedby="validationError ? 'ad-multiline-error' : undefined" />
            {{ t("dm.attr.f.multilineText") }}
          </label>
          <span v-if="validationError" id="ad-multiline-error" class="error">{{ validationError }}</span>
        </div>
      </template>
      <FormField
        id="ad-default"
        v-slot="p"
        :label="t('dm.attr.f.default')"
        :error="errorFor('defaultValue')"
        :hint="dataType === 'reference' ? t('dm.attr.f.defaultNone') : t('dm.attr.f.defaultHint')"
      >
        <input v-if="dataType === 'reference'" :id="p.id" type="text" disabled value="" :aria-describedby="p.describedBy" />
        <AttributeInput v-else :id="p.id" v-model="defaultValue" :def="draft" :invalid="p.invalid" :described-by="p.describedBy" />
      </FormField>
      <FormField id="ad-help" v-slot="p" :label="t('dm.attr.f.help')" wide :error="errorFor('helpText')" :hint="t('dm.attr.f.helpHint')">
        <input :id="p.id" v-model="helpText" type="text" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
      </FormField>
      <FormField id="ad-description" v-slot="p" :label="t('dm.attr.f.description')" wide :error="errorFor('description')" :hint="t('dm.attr.f.descriptionHint')">
        <textarea :id="p.id" v-model="description" rows="2" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
      </FormField>
    </div>
  </FormDialog>
  <SchemaChangeDialog :flow="flow" />
</template>
