<script setup lang="ts">
import { computed, reactive, ref, useId, watch } from "vue";
import { RouterLink } from "vue-router";
import { ApiError } from "../api/client";
import { useBulkUpdateCis, useCriticalityValues, type EffectiveAttribute } from "../api/queries";
import { bulkOutcome, bulkUpdateBody, CRITICALITY_FIELD, problemField, type BulkChange, type BulkOutcome } from "../lib/bulkEdit";
import { formatNumber, t } from "../i18n";
import FormField from "../pages/form/FormField.vue";
import AttributeInput from "./AttributeInput.vue";
import ErrorAlert from "./ErrorAlert.vue";
import Icon from "./Icon.vue";

/**
 * The inventory's bulk edit (gap G10): the operator adds the fields to change, the values are set on every
 * selected CI in one request (POST /configuration-items/bulk-update), and the dialog then reports how many
 * CIs were updated and lists each refused one with the API's reason. Fields are the class's attributes when
 * the list shows one class (every selected CI then has them), and the criticality, which every CI has.
 */
const props = defineProps<{
  open: boolean;
  ids: string[];
  /** Labels of the selected CIs, to name the refused ones (falls back to the id). */
  labels: ReadonlyMap<string, string>;
  /** The attributes the selection shares (the shown class's); empty without a class filter. */
  defs: EffectiveAttribute[];
  className?: string;
}>();
const emit = defineEmits<{ close: [outcome: BulkOutcome | null] }>();

const dialog = ref<HTMLDialogElement>();
const titleId = `bulk-edit-title-${useId()}`;
const mutation = useBulkUpdateCis();
const criticality = useCriticalityValues();

const changes = ref<BulkChange[]>([]);
const refNames = reactive<Record<string, string>>({});
const allOrNothing = ref(false);
const outcome = ref<BulkOutcome | null>(null);
/** How many CIs the last request sent (the selection may change behind the dialog). */
const sent = ref(0);
const choice = ref("");

watch(
  () => props.open,
  (open) => {
    const d = dialog.value;
    if (open) {
      changes.value = [];
      allOrNothing.value = false;
      outcome.value = null;
      choice.value = "";
      mutation.reset();
      if (d && !d.open) d.showModal();
    } else if (d?.open) d.close();
  },
  { flush: "post" },
);

const editable = computed(() => props.defs.filter((d) => d.isActive));
const defFor = (key: string) => editable.value.find((d) => d.key === key);
const fieldLabel = (key: string) => (key === CRITICALITY_FIELD ? t("bulkEdit.criticality") : (defFor(key)?.label ?? key));
const available = computed(() => {
  const taken = new Set(changes.value.map((c) => c.field));
  return [{ key: CRITICALITY_FIELD, label: t("bulkEdit.criticality") }, ...editable.value.map((d) => ({ key: d.key, label: d.label }))].filter(
    (f) => !taken.has(f.key),
  );
});
function addField() {
  if (!choice.value) return;
  changes.value.push({ field: choice.value, value: "" });
  choice.value = "";
}
const removeField = (field: string) => (changes.value = changes.value.filter((c) => c.field !== field));

const body = computed(() => bulkUpdateBody(props.ids, changes.value, editable.value, allOrNothing.value));
const busy = computed(() => mutation.isPending.value);

// A malformed body (400) or a failed request: its field errors go next to the fields, the rest above them.
const requestError = computed(() => mutation.error.value);
const fieldErrors = computed(() => (requestError.value instanceof ApiError ? requestError.value.fieldErrors() : {}));
const errorFor = (field: string) => fieldErrors.value[field === CRITICALITY_FIELD ? "criticalityValueId" : `attributes.${field}`];

async function submit() {
  if (!body.value || busy.value) return;
  sent.value = body.value.ids.length;
  try {
    outcome.value = bulkOutcome(await mutation.mutateAsync(body.value));
  } catch {
    // Shown from mutation.error.
  }
}

function close() {
  if (!busy.value) emit("close", outcome.value);
}
function onCancel(e: Event) {
  e.preventDefault();
  close();
}

const labelOf = (id: string) => props.labels.get(id);
const problemLabel = (field: string) => problemField(field, (key) => defFor(key)?.label, t("bulkEdit.criticality"));
</script>

<template>
  <Teleport to="body">
    <dialog ref="dialog" class="confirm form-dialog wide bulk-edit" :aria-labelledby="titleId" @cancel="onCancel">
      <form v-if="!outcome" novalidate @submit.prevent="submit">
        <h2 :id="titleId">{{ t("bulkEdit.title", { n: ids.length }) }}</h2>
        <div v-if="open" class="body">
          <ErrorAlert v-if="requestError" :error="requestError" :title="t('bulkEdit.failed')" />
          <p class="hint">{{ t("bulkEdit.intro") }}</p>
          <p v-if="editable.length === 0" class="hint">{{ className ? t("bulkEdit.noAttributes", { name: className }) : t("bulkEdit.oneClass") }}</p>

          <div v-for="c in changes" :key="c.field" class="bulk-change">
            <FormField
              :id="`bulk-${c.field === CRITICALITY_FIELD ? 'criticality' : c.field}`"
              v-slot="p"
              :label="fieldLabel(c.field)"
              :error="errorFor(c.field)"
              :hint="c.value === '' ? t('bulkEdit.clears') : undefined"
            >
              <select
                v-if="c.field === CRITICALITY_FIELD"
                :id="p.id"
                v-model="c.value"
                :aria-invalid="p.invalid || undefined"
                :aria-describedby="p.describedBy"
              >
                <option value="">{{ t("common.notSet") }}</option>
                <option v-for="v in (criticality.data.value ?? []).filter((v) => v.isActive)" :key="v.id" :value="v.id">{{ v.name }}</option>
              </select>
              <AttributeInput
                v-else
                :id="p.id"
                v-model="c.value"
                :def="defFor(c.field)!"
                :invalid="p.invalid"
                :described-by="p.describedBy"
                :reference-name="refNames[c.field]"
                @reference-name="(name) => (refNames[c.field] = name)"
              />
            </FormField>
            <button type="button" class="btn btn-sm btn-ghost" :aria-label="t('bulkEdit.remove', { name: fieldLabel(c.field) })" @click="removeField(c.field)">
              <Icon name="x" />
            </button>
          </div>

          <div v-if="available.length > 0" class="field bulk-add">
            <label :for="`${titleId}-add`">{{ t("bulkEdit.addField") }}</label>
            <div class="bulk-add-row">
              <select :id="`${titleId}-add`" v-model="choice">
                <option value="">{{ t("bulkEdit.chooseField") }}</option>
                <option v-for="f in available" :key="f.key" :value="f.key">{{ f.label }}</option>
              </select>
              <button type="button" class="btn" :disabled="!choice" @click="addField"><Icon name="plus" />{{ t("bulkEdit.add") }}</button>
            </div>
          </div>

          <label class="checkbox-row">
            <input v-model="allOrNothing" type="checkbox" />
            {{ t("bulkEdit.allOrNothing") }}
          </label>
        </div>
        <div class="footer">
          <button type="button" class="btn" :disabled="busy" @click="close">{{ t("common.cancel") }}</button>
          <button type="submit" class="btn btn-primary" :disabled="busy || !body">
            {{ busy ? t("common.saving") : t("bulkEdit.apply", { n: ids.length }) }}
          </button>
        </div>
      </form>

      <template v-else>
        <h2 :id="titleId">{{ t("bulkEdit.resultTitle") }}</h2>
        <div class="body">
          <p role="status">
            <template v-if="!outcome.committed">
              {{ t("bulkEdit.nothingSaved", { n: outcome.refused.length }) }}
            </template>
            <template v-else>{{ t("bulkEdit.updated", { n: outcome.updated, all: formatNumber(sent) }) }}</template>
          </p>
          <template v-if="outcome.refused.length > 0">
            <h3>{{ t("bulkEdit.refused", { n: outcome.refused.length }) }}</h3>
            <ul class="bulk-refused">
              <li v-for="r in outcome.refused" :key="r.id">
                <RouterLink :to="`/cis/${r.id}`" :class="{ mono: !labelOf(r.id) }">{{ labelOf(r.id) ?? r.id }}</RouterLink>
                <span>{{ r.message }}</span>
                <ul v-if="r.problems.length > 0">
                  <li v-for="(pr, i) in r.problems" :key="i">{{ problemLabel(pr.field) }}: {{ pr.message }}</li>
                </ul>
              </li>
            </ul>
            <p class="hint">{{ t("bulkEdit.refusedKept") }}</p>
          </template>
        </div>
        <div class="footer">
          <button type="button" class="btn btn-primary" autofocus @click="close">{{ t("bulkEdit.close") }}</button>
        </div>
      </template>
    </dialog>
  </Teleport>
</template>
