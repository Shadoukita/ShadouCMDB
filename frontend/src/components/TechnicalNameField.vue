<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useTechnicalName, type TechnicalNameKind } from "../api/schemaChanges";
import { useDebounced } from "../lib/composables";
import { suggestKey } from "../lib/keys";

/**
 * The technical name of an area, type or field: its PostgreSQL schema, table or
 * column. On create it follows the display name ("Virtuelle Maschinen" →
 * virtuelle_maschinen) until the administrator types their own; either way the
 * API checks it live (format, reserved words, names taken) and shows where it
 * will live ("bestand.virtuelle_maschinen"). After creation it is read-only.
 */
const props = defineProps<{
  id: string;
  kind: TechnicalNameKind;
  /** The display name it is derived from. */
  name: string;
  modelValue: string;
  /** Create mode: editable and checked. */
  editable: boolean;
  areaId?: string;
  classId?: string;
  /** Read-only mode: where it lives, e.g. the type's table "bestand.netzwerk". */
  location?: string;
  error?: string;
}>();
const emit = defineEmits<{ "update:modelValue": [value: string] }>();

const touched = ref(false);
watch(
  () => props.editable,
  () => (touched.value = false),
);

// Follow the display name at once (local guess), then with the API's answer.
watch(
  () => props.name,
  (n) => {
    if (props.editable && !touched.value) emit("update:modelValue", suggestKey(n));
  },
);

const query = computed(() => {
  if (!props.editable) return null;
  const name = props.name.trim() || props.modelValue.trim();
  if (!name) return null;
  return {
    kind: props.kind,
    name,
    key: touched.value ? props.modelValue.trim() || undefined : undefined,
    areaId: props.areaId || undefined,
    classId: props.classId || undefined,
  };
});
const debounced = useDebounced(query, 250);
const check = useTechnicalName(debounced);
const result = computed(() => (query.value ? check.data.value : undefined));

watch(result, (r) => {
  if (r && r.derived && !touched.value && props.editable && r.technicalName !== props.modelValue) emit("update:modelValue", r.technicalName);
});

function onInput(e: Event) {
  touched.value = true;
  emit("update:modelValue", (e.target as HTMLInputElement).value);
}
function useDerived() {
  touched.value = false;
  emit("update:modelValue", suggestKey(props.name));
}

const WHAT: Record<TechnicalNameKind, string> = { area: "schema", type: "table", field: "column" };
const label = computed(() => `Technical name (${WHAT[props.kind]})`);
/** The API's verdict, once it matches what is in the box. */
const current = computed(() => (result.value && (!touched.value || result.value.technicalName === props.modelValue.trim()) ? result.value : undefined));
const apiError = computed(() => (current.value && !current.value.valid ? (current.value.message ?? "This name cannot be used") : undefined));
const shownError = computed(() => props.error || apiError.value);
const describedBy = computed(() => [shownError.value ? `${props.id}-err` : "", `${props.id}-hint`].filter(Boolean).join(" "));
</script>

<template>
  <div class="field technical-name">
    <label :for="id">{{ label }}<span v-if="editable" class="req" aria-hidden="true">*</span></label>
    <div class="inline-control">
      <input
        :id="id"
        :value="modelValue"
        type="text"
        class="mono"
        spellcheck="false"
        autocomplete="off"
        maxlength="63"
        :readonly="!editable"
        :aria-required="editable || undefined"
        :aria-invalid="!!shownError || undefined"
        :aria-describedby="describedBy"
        @input="onInput"
      />
      <span v-if="editable && check.isFetching.value" class="spinner" aria-label="Checking the name" />
      <span v-else-if="editable && current?.valid" class="tn-ok" aria-hidden="true">✓</span>
      <button v-if="editable && touched" type="button" class="btn btn-sm" @click="useDerived">From name</button>
    </div>
    <span v-if="shownError" :id="`${id}-err`" class="error">{{ shownError }}</span>
    <span :id="`${id}-hint`" class="hint">
      <template v-if="!editable">
        Fixed after creation<template v-if="location">: <code>{{ location }}</code></template>
      </template>
      <template v-else-if="current?.valid">
        Will be created as <code>{{ current.qualifiedName ?? current.technicalName }}</code>. Cannot be changed later; renaming changes only the display name.
      </template>
      <template v-else>Lower-case a–z, digits and _, starting with a letter. Cannot be changed later.</template>
    </span>
  </div>
</template>
