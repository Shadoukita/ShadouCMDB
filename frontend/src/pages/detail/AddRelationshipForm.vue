<script setup lang="ts">
import { computed, ref } from "vue";
import { ApiError } from "../../api/client";
import { useCreateRelationship, useRelationshipTypes, type Ci, type CiSummary } from "../../api/queries";
import CiPicker from "../../components/CiPicker.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import Icon from "../../components/Icon.vue";
import { t } from "../../i18n";
import { useSessionStore } from "../../stores/session";

/**
 * Relate this CI to another. Only types the API allows between the two classes are offered, in both directions,
 * and only in a direction the user may create (edit right on the source CI's class).
 * Every control has the form's one width (`.rel-add`), the notes field takes what is left (audit R5).
 */
const props = defineProps<{ ci: Ci }>();
const target = ref<CiSummary | null>(null);
const choice = ref("");
const notes = ref("");
const done = ref<string | null>(null);
const outTypes = useRelationshipTypes(() => props.ci.classId, () => target.value?.classId);
const inTypes = useRelationshipTypes(() => target.value?.classId, () => props.ci.classId);
const create = useCreateRelationship();
const session = useSessionStore();
const mayOut = computed(() => session.canOnClass(props.ci.classId, "edit"));
const mayIn = computed(() => !!target.value && session.canOnClass(target.value.classId, "edit"));

const options = computed(() => {
  const out: { value: string; label: string }[] = [];
  const name = target.value?.label;
  if (mayOut.value) for (const t of outTypes.data.value?.data ?? []) out.push({ value: `${t.id}:out`, label: `${props.ci.label} ${t.forwardLabel} ${name}` });
  for (const t of mayIn.value ? (inTypes.data.value?.data ?? []) : []) {
    if (!t.isDirectional && out.some((o) => o.value === `${t.id}:out`)) continue;
    out.push({ value: `${t.id}:in`, label: `${props.ci.label} ${t.isDirectional ? t.reverseLabel : t.forwardLabel} ${name}` });
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
  !target.value
    ? t("rel.add.pickFirst")
    : typesLoading.value
      ? t("common.loading")
      : options.value.length === 0
        ? t("rel.add.noneAllowed")
        : t("rel.add.choose"),
);

function onTarget(t: CiSummary | null) {
  target.value = t;
  choice.value = "";
  done.value = null;
  create.reset();
}

function submit() {
  const other = target.value;
  if (!other || !choice.value) return;
  const [typeId, dir] = choice.value.split(":");
  const sourceCiId = dir === "out" ? props.ci.id : other.id;
  const targetCiId = dir === "out" ? other.id : props.ci.id;
  const label = options.value.find((o) => o.value === choice.value)?.label ?? "";
  create.mutate(
    { relationshipTypeId: typeId, sourceCiId, targetCiId, notes: notes.value.trim() || null },
    {
      onSuccess: () => {
        done.value = t("rel.add.done", { label });
        target.value = null;
        choice.value = "";
        notes.value = "";
      },
    },
  );
}
</script>

<template>
  <form class="rel-add" :aria-label="t('rel.add.title')" @submit.prevent="submit">
    <div class="field">
      <label for="rel-target">{{ t("rel.add.target") }}</label>
      <CiPicker
        id="rel-target"
        :exclude-id="ci.id"
        :selected="target && { id: target.id, name: target.label }"
        :invalid="!!targetError"
        :described-by="targetError ? 'rel-target-err' : undefined"
        @select="onTarget"
      />
      <span v-if="targetError" id="rel-target-err" class="error">{{ targetError }}</span>
    </div>
    <div class="field">
      <label for="rel-type">{{ t("rel.add.type") }}</label>
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
            ? t("rel.add.hintNone", { a: ci.class.name, b: target.class.name })
            : t("rel.add.hintSome", { a: ci.class.name, b: target.class.name })
        }}
      </span>
    </div>
    <div class="field rel-add-notes">
      <label for="rel-notes">{{ t("rel.add.notes") }}</label>
      <input
        id="rel-notes"
        v-model="notes"
        type="text"
        :placeholder="t('rel.add.optional')"
        :aria-invalid="fe.notes ? true : undefined"
        :aria-describedby="fe.notes ? 'rel-notes-err' : undefined"
      />
      <span v-if="fe.notes" id="rel-notes-err" class="error">{{ fe.notes }}</span>
    </div>
    <button type="submit" class="btn btn-primary" :disabled="!target || !choice || create.isPending.value">
      <Icon name="plus" :size="16" />{{ create.isPending.value ? t("rel.add.adding") : t("rel.add.submit") }}
    </button>
    <span v-if="done && !create.error.value" role="status" class="muted">{{ done }}</span>
    <div v-if="typesError != null" class="rel-add-alert">
      <ErrorAlert :error="typesError" :title="t('rel.add.typesFailed')" />
    </div>
    <div v-if="showGeneralError" class="rel-add-alert">
      <ErrorAlert :error="create.error.value" :title="t('rel.add.failed')" />
    </div>
  </form>
</template>
