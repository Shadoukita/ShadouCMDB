<script setup lang="ts">
import { computed, nextTick, ref, useId, watch } from "vue";
import { ApiError } from "../api/client";
import { DESTRUCTIVE_IMPACT, type SchemaChangeFlow } from "../lib/schemaChange";
import ErrorAlert from "./ErrorAlert.vue";
import LoadingState from "./LoadingState.vue";

/**
 * Shows what a data model change does to the database before it runs: one line
 * per change, its effect on stored data, and the exact DDL. A refused change
 * (a guard on the server) is explained instead, with nothing applied. Purges ask
 * for the technical name to be typed. Bound to a flow from useSchemaChangeFlow.
 */
const props = defineProps<{ flow: SchemaChangeFlow }>();
const s = computed(() => props.flow.state);
const o = computed(() => s.value.options);
const dialog = ref<HTMLDialogElement>();
const confirmInput = ref<HTMLInputElement>();
/** Several flows can be mounted on one page; ids stay unique. */
const uid = useId();

watch(
  () => s.value.open,
  async (open) => {
    await nextTick();
    const d = dialog.value;
    if (!d) return;
    if (open && !d.open) d.showModal();
    if (!open && d.open) d.close();
  },
  { flush: "post" },
);
// Focus the confirmation field once the preview is there.
watch(
  () => s.value.preview,
  async (p) => {
    if (p && o.value?.confirmName) {
      await nextTick();
      confirmInput.value?.focus();
    }
  },
);

function onCancel(e: Event) {
  e.preventDefault();
  props.flow.cancel();
}

const refusalTitle = computed(() => {
  const e = s.value.refusal;
  if (!(e instanceof ApiError)) return "The preview failed";
  switch (e.code) {
    case "SCHEMA_CHANGE_REFUSED":
      return "Refused: this change would lose or break stored data";
    case "IN_USE":
      return "Refused: it is still in use";
    case "INVALID_NAME":
      return "Refused: the technical name cannot be used";
    case "CONFLICT":
      return "Refused: it conflicts with the current data model";
    default:
      return "The change cannot be made";
  }
});
const typedOk = computed(() => !o.value?.confirmName || s.value.typed.trim() === o.value.confirmName);
const canApply = computed(() => !!s.value.preview && !s.value.refusal && !s.value.loading && typedOk.value);
const statementImpacts = (i: number) => (s.value.preview?.impact ?? []).filter((x) => x.statement === i);
const planImpacts = computed(() => (s.value.preview?.impact ?? []).filter((x) => x.statement === null || x.statement === undefined));
const allImpacts = computed(() => s.value.preview?.impact ?? []);
const rowsText = (n: number | null | undefined) => (n === null || n === undefined ? "" : ` (${n.toLocaleString()} ${n === 1 ? "row" : "rows"})`);
</script>

<template>
  <dialog ref="dialog" class="confirm form-dialog wide schema-change" :aria-labelledby="`sc-title-${uid}`" @cancel="onCancel">
    <form novalidate @submit.prevent="flow.apply()">
      <h2 :id="`sc-title-${uid}`">{{ o?.title }}</h2>
      <div v-if="s.open" class="body">
        <p v-if="o?.intro" class="sc-intro">{{ o.intro }}</p>
        <LoadingState v-if="s.loading" label="Previewing the database change…" />
        <template v-else-if="s.refusal">
          <ErrorAlert :error="s.refusal" :title="refusalTitle" />
          <p class="muted">Nothing was changed. Adjust the change, or resolve what the message names, and try again.</p>
        </template>
        <template v-else-if="s.preview">
          <section :aria-labelledby="`sc-what-${uid}`">
            <h3 :id="`sc-what-${uid}`">What it does</h3>
            <ul v-if="s.preview.summaries.length" class="sc-summaries">
              <li v-for="(line, i) in s.preview.summaries" :key="i">{{ line }}</li>
            </ul>
            <p v-else class="muted">It changes only the data model's settings; no table or column is touched.</p>
          </section>
          <section :aria-labelledby="`sc-impact-${uid}`">
            <h3 :id="`sc-impact-${uid}`">Effect on stored data</h3>
            <ul v-if="allImpacts.length" class="sc-impact">
              <li v-for="(x, i) in planImpacts" :key="`p${i}`" :class="{ destructive: DESTRUCTIVE_IMPACT.has(x.kind) }">
                <span class="badge" :class="DESTRUCTIVE_IMPACT.has(x.kind) ? 'danger' : ''">{{ x.kind.replace(/_/g, " ") }}</span>
                {{ x.message }}{{ rowsText(x.rows) }}
              </li>
              <template v-for="(_, si) in s.preview.statements" :key="`s${si}`">
                <li v-for="(x, i) in statementImpacts(si)" :key="`s${si}-${i}`" :class="{ destructive: DESTRUCTIVE_IMPACT.has(x.kind) }">
                  <span class="badge" :class="DESTRUCTIVE_IMPACT.has(x.kind) ? 'danger' : ''">{{ x.kind.replace(/_/g, " ") }}</span>
                  {{ x.message }}{{ rowsText(x.rows) }} <span class="muted">· statement {{ si + 1 }}</span>
                </li>
              </template>
            </ul>
            <p v-else class="muted">No stored values are changed.</p>
          </section>
          <section :aria-labelledby="`sc-ddl-${uid}`">
            <h3 :id="`sc-ddl-${uid}`">SQL that will run <span class="muted">(one transaction)</span></h3>
            <ol v-if="s.preview.statements.length" class="sc-ddl">
              <li v-for="(sql, i) in s.preview.statements" :key="i"><pre>{{ sql }}</pre></li>
            </ol>
            <p v-else class="muted">None.</p>
          </section>
          <div v-if="o?.confirmName" class="field sc-confirm">
            <label :for="`sc-confirm-input-${uid}`">
              This cannot be undone. Type <code>{{ o.confirmName }}</code> to confirm.
            </label>
            <input
              :id="`sc-confirm-input-${uid}`"
              ref="confirmInput"
              v-model="s.typed"
              type="text"
              class="mono"
              autocomplete="off"
              spellcheck="false"
              :aria-invalid="(s.typed !== '' && !typedOk) || undefined"
            />
          </div>
        </template>
        <ErrorAlert v-if="s.applyError" :error="s.applyError" title="The change was not applied" />
      </div>
      <div class="footer">
        <button type="button" class="btn" :disabled="s.applying" @click="flow.cancel()">{{ s.refusal ? "Close" : "Cancel" }}</button>
        <button v-if="!s.refusal" type="submit" :class="['btn', o?.danger ? 'btn-danger' : 'btn-primary']" :disabled="!canApply || s.applying">
          {{ s.applying ? "Applying…" : o?.applyLabel }}
        </button>
      </div>
    </form>
  </dialog>
</template>
