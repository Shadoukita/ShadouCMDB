<script setup lang="ts">
import { computed, ref, watch, watchEffect } from "vue";
import { useLookupListValues } from "../api/datamodel";
import { t } from "../i18n";

/**
 * <select> over the values of an admin-defined lookup list (`lookup` attributes). Retired values are hidden unless selected.
 *
 * `parent` makes it a dependent dropdown (Model under Manufacturer): it offers only the values
 * that belong to the parent field's value (filtered by the API), stays disabled until that is
 * set, and clears its value when a new parent value no longer contains it.
 */
export interface LookupParent {
  /** Label of the parent field, for "Choose Manufacturer first". */
  label: string;
  /** The parent field's current value id ("" while not set). */
  value: string;
}
const props = defineProps<{ listId: string | null; id: string; invalid?: boolean; describedBy?: string; parent?: LookupParent | null }>();
const model = defineModel<string>({ required: true });

const dependent = computed(() => !!props.parent);
const parentValue = computed(() => props.parent?.value ?? "");
const waiting = computed(() => dependent.value && parentValue.value === "");
const values = useLookupListValues(
  () => (waiting.value ? null : props.listId),
  () => (dependent.value ? parentValue.value : undefined),
);
const options = computed(() => (values.data.value ?? []).filter((v) => v.isActive || v.id === model.value));

// A value that is not among the options (stored before the rule existed, or of another parent)
// is still shown, by its name from the whole list, so nothing changes unnoticed.
const stray = computed(() => model.value !== "" && (waiting.value || (!!values.data.value && !options.value.some((o) => o.id === model.value))));
const whole = useLookupListValues(() => (dependent.value && stray.value ? props.listId : null));
const strayLabel = computed(() => {
  if (!dependent.value) return "Unknown value";
  const v = whole.data.value?.find((o) => o.id === model.value);
  return v ? `${v.name} (not a value of the chosen ${props.parent!.label})` : "Unknown value";
});

// Changing the parent clears a child value that the new parent does not offer.
const recheck = ref<string | null>(null);
watch(parentValue, (now, before) => {
  if (!dependent.value || now === before) return;
  if (now === "") {
    recheck.value = null;
    model.value = "";
  } else recheck.value = now;
});
watchEffect(() => {
  const parent = recheck.value;
  const data = values.data.value;
  // Only judge on the values of the new parent (the query may still hold the previous ones).
  if (!parent || !data || !data.every((v) => v.parentValueId === parent)) return;
  recheck.value = null;
  if (model.value !== "" && !data.some((v) => v.id === model.value && v.isActive)) model.value = "";
});

const placeholder = computed(() => {
  if (waiting.value) return `Choose ${props.parent!.label} first`;
  if (values.isLoading.value) return "Loading…";
  if (values.isError.value) return "Could not load the list";
  if (dependent.value && options.value.length === 0) return `No values for this ${props.parent!.label}`;
  return t("common.notSet");
});
</script>

<template>
  <select
    :id="id"
    v-model="model"
    :aria-invalid="invalid || undefined"
    :aria-describedby="describedBy"
    :disabled="waiting || values.isLoading.value"
    :title="waiting ? placeholder : undefined"
  >
    <option value="">{{ placeholder }}</option>
    <template v-if="!waiting">
      <option v-for="v in options" :key="v.id" :value="v.id">{{ v.name }}{{ v.isActive ? "" : " (retired)" }}</option>
    </template>
    <option v-if="stray" :value="model">{{ strayLabel }}</option>
  </select>
</template>
