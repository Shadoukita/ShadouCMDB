<script setup lang="ts">
import { computed, ref } from "vue";
import { ApiError } from "../../api/client";
import { useCreateRelationship, useRelationshipTypes, type Ci, type CiSummary } from "../../api/queries";
import CiPicker from "../../components/CiPicker.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";

/** Relate this CI to another. Only types the API allows between the two classes are offered, in both directions. */
const props = defineProps<{ ci: Ci }>();
const target = ref<CiSummary | null>(null);
const choice = ref("");
const notes = ref("");
const done = ref<string | null>(null);
const outTypes = useRelationshipTypes(() => props.ci.classId, () => target.value?.classId);
const inTypes = useRelationshipTypes(() => target.value?.classId, () => props.ci.classId);
const create = useCreateRelationship();

const options = computed(() => {
  const out: { value: string; label: string }[] = [];
  const name = target.value?.name;
  for (const t of outTypes.data.value?.data ?? []) out.push({ value: `${t.id}:out`, label: `${props.ci.name} ${t.forwardLabel} ${name}` });
  for (const t of inTypes.data.value?.data ?? []) {
    if (!t.isDirectional && out.some((o) => o.value === `${t.id}:out`)) continue;
    out.push({ value: `${t.id}:in`, label: `${props.ci.name} ${t.isDirectional ? t.reverseLabel : t.forwardLabel} ${name}` });
  }
  return out;
});
const typesLoading = computed(() => !!target.value && (outTypes.isLoading.value || inTypes.isLoading.value));
const typesError = computed(() => outTypes.error.value ?? inTypes.error.value);
const apiError = computed(() => (create.error.value instanceof ApiError ? create.error.value : null));
const fe = computed(() => apiError.value?.fieldErrors() ?? {});
const targetError = computed(() => fe.value.targetCiId ?? fe.value.sourceCiId);
const typeError = computed(() => fe.value.relationshipTypeId);
const showGeneralError = computed(() => {
  const err = create.error.value;
  if (err == null) return false;
  if (!apiError.value) return true;
  const other = apiError.value.details.filter((d) => !["targetCiId", "sourceCiId", "relationshipTypeId", "notes"].includes(d.field));
  return other.length > 0 || apiError.value.details.length === 0;
});
const typePlaceholder = computed(() =>
  !target.value ? "Pick a CI first" : typesLoading.value ? "Loading…" : options.value.length === 0 ? "No relationship allowed" : "Choose…",
);

function onTarget(t: CiSummary | null) {
  target.value = t;
  choice.value = "";
  done.value = null;
  create.reset();
}

function submit() {
  const t = target.value;
  if (!t || !choice.value) return;
  const [typeId, dir] = choice.value.split(":");
  const sourceCiId = dir === "out" ? props.ci.id : t.id;
  const targetCiId = dir === "out" ? t.id : props.ci.id;
  const label = options.value.find((o) => o.value === choice.value)?.label ?? "";
  create.mutate(
    { relationshipTypeId: typeId, sourceCiId, targetCiId, notes: notes.value.trim() || null },
    {
      onSuccess: () => {
        done.value = `Added: ${label}`;
        target.value = null;
        choice.value = "";
        notes.value = "";
      },
    },
  );
}
</script>

<template>
  <form class="rel-add" aria-label="Add relationship" @submit.prevent="submit">
    <div class="field">
      <label for="rel-target">Relate to</label>
      <CiPicker
        id="rel-target"
        :exclude-id="ci.id"
        :selected="target"
        :invalid="!!targetError"
        :described-by="targetError ? 'rel-target-err' : undefined"
        @select="onTarget"
      />
      <span v-if="targetError" id="rel-target-err" class="error">{{ targetError }}</span>
    </div>
    <div class="field">
      <label for="rel-type">Relationship</label>
      <select
        id="rel-type"
        v-model="choice"
        :disabled="!target || typesLoading || options.length === 0"
        :aria-invalid="typeError ? true : undefined"
        :aria-describedby="typeError ? 'rel-type-err' : !target ? undefined : 'rel-type-hint'"
      >
        <option value="">{{ typePlaceholder }}</option>
        <option v-for="o in options" :key="o.value" :value="o.value">{{ o.label }}</option>
      </select>
      <span v-if="typeError" id="rel-type-err" class="error">{{ typeError }}</span>
      <span v-if="target && !typesLoading && !typeError" id="rel-type-hint" class="hint">
        {{
          options.length === 0
            ? `No relationship rule allows ${ci.class.name} ↔ ${target.class.name}.`
            : `Only types allowed between ${ci.class.name} and ${target.class.name}`
        }}
      </span>
    </div>
    <div class="field">
      <label for="rel-notes">Notes</label>
      <input id="rel-notes" v-model="notes" type="text" placeholder="Optional" style="width: 200px" :aria-invalid="fe.notes ? true : undefined" />
      <span v-if="fe.notes" class="error">{{ fe.notes }}</span>
    </div>
    <button type="submit" class="btn btn-primary" :disabled="!target || !choice || create.isPending.value">
      {{ create.isPending.value ? "Adding…" : "Add relationship" }}
    </button>
    <span v-if="done && !create.error.value" role="status" class="muted">{{ done }}</span>
    <ErrorAlert v-if="typesError != null" :error="typesError" title="Could not load relationship types" />
    <div v-if="showGeneralError" style="flex-basis: 100%">
      <ErrorAlert :error="create.error.value" title="Relationship not added" />
    </div>
  </form>
</template>
