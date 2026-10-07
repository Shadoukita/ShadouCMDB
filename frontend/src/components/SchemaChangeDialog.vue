<script setup lang="ts">
import { computed, nextTick, ref, useId, watch } from "vue";
import { ApiError } from "../api/client";
import { t, tAround } from "../i18n";
import { DESTRUCTIVE_IMPACT, impactLabel, type SchemaChangeFlow } from "../lib/schemaChange";
import ErrorAlert from "./ErrorAlert.vue";
import Icon from "./Icon.vue";
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
  if (!(e instanceof ApiError)) return t("dm.schemaChange.refusal.previewFailed");
  switch (e.code) {
    case "SCHEMA_CHANGE_REFUSED":
      return t("dm.schemaChange.refusal.dataLoss");
    case "IN_USE":
      return t("dm.schemaChange.refusal.inUse");
    case "INVALID_NAME":
      return t("dm.schemaChange.refusal.invalidName");
    case "CONFLICT":
      return t("dm.schemaChange.refusal.conflict");
    default:
      return t("dm.schemaChange.refusal.other");
  }
});
const typedOk = computed(() => !o.value?.confirmName || s.value.typed.trim() === o.value.confirmName);
const canApply = computed(() => !!s.value.preview && !s.value.refusal && !s.value.loading && typedOk.value);
const statementImpacts = (i: number) => (s.value.preview?.impact ?? []).filter((x) => x.statement === i);
const planImpacts = computed(() => (s.value.preview?.impact ?? []).filter((x) => x.statement === null || x.statement === undefined));
const allImpacts = computed(() => s.value.preview?.impact ?? []);
const rowsText = (n: number | null | undefined) => (n === null || n === undefined ? "" : ` ${t("dm.schemaChange.rows", { n })}`);
/** "Type <code>name</code> to confirm.", split around the name in the translator's word order. */
const confirmText = computed(() => tAround("dm.schemaChange.confirmType", "name"));
</script>

<template>
  <dialog ref="dialog" class="confirm form-dialog wide schema-change" :aria-labelledby="`sc-title-${uid}`" @cancel="onCancel">
    <form novalidate @submit.prevent="flow.apply()">
      <h2 :id="`sc-title-${uid}`">{{ o?.title }}</h2>
      <div v-if="s.open" class="body">
        <p v-if="o?.intro" class="sc-intro">{{ o.intro }}</p>
        <LoadingState v-if="s.loading" :label="t('dm.schemaChange.loading')" />
        <template v-else-if="s.refusal">
          <ErrorAlert :error="s.refusal" :title="refusalTitle" />
          <p class="muted">{{ t("dm.schemaChange.nothingChanged") }}</p>
        </template>
        <template v-else-if="s.preview">
          <section :aria-labelledby="`sc-what-${uid}`">
            <h3 :id="`sc-what-${uid}`">{{ t("dm.schemaChange.what") }}</h3>
            <ul v-if="s.preview.summaries.length" class="sc-summaries">
              <li v-for="(line, i) in s.preview.summaries" :key="i">{{ line }}</li>
            </ul>
            <p v-else class="muted">{{ t("dm.schemaChange.whatNone") }}</p>
          </section>
          <section :aria-labelledby="`sc-impact-${uid}`">
            <h3 :id="`sc-impact-${uid}`">{{ t("dm.schemaChange.impactTitle") }}</h3>
            <ul v-if="allImpacts.length" class="sc-impact">
              <li v-for="(x, i) in planImpacts" :key="`p${i}`" :class="{ destructive: DESTRUCTIVE_IMPACT.has(x.kind) }">
                <span class="badge" :class="DESTRUCTIVE_IMPACT.has(x.kind) ? 'danger' : ''">{{ impactLabel(x.kind) }}</span>
                {{ x.message }}{{ rowsText(x.rows) }}
              </li>
              <template v-for="(_, si) in s.preview.statements" :key="`s${si}`">
                <li v-for="(x, i) in statementImpacts(si)" :key="`s${si}-${i}`" :class="{ destructive: DESTRUCTIVE_IMPACT.has(x.kind) }">
                  <span class="badge" :class="DESTRUCTIVE_IMPACT.has(x.kind) ? 'danger' : ''">{{ impactLabel(x.kind) }}</span>
                  {{ x.message }}{{ rowsText(x.rows) }} <span class="muted">{{ t("dm.schemaChange.statementRef", { n: si + 1 }) }}</span>
                </li>
              </template>
            </ul>
            <p v-else class="muted">{{ t("dm.schemaChange.impactNone") }}</p>
          </section>
          <section :aria-labelledby="`sc-ddl-${uid}`">
            <h3 :id="`sc-ddl-${uid}`">{{ t("dm.schemaChange.sql") }} <span class="muted">{{ t("dm.schemaChange.sqlTransaction") }}</span></h3>
            <ol v-if="s.preview.statements.length" class="sc-ddl">
              <li v-for="(sql, i) in s.preview.statements" :key="i"><pre>{{ sql }}</pre></li>
            </ol>
            <p v-else class="muted">{{ t("dm.schemaChange.sqlNone") }}</p>
          </section>
          <div v-if="o?.confirmName" class="field sc-confirm">
            <label :for="`sc-confirm-input-${uid}`">
              <span class="sc-warning"><Icon name="triangle-alert" />{{ t("dm.schemaChange.cannotUndo") }}</span>
              {{ confirmText[0] }}<code>{{ o.confirmName }}</code>{{ confirmText[1] }}
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
        <ErrorAlert v-if="s.applyError" :error="s.applyError" :title="t('dm.schemaChange.applyFailed')" />
      </div>
      <div class="footer">
        <button type="button" class="btn" :disabled="s.applying" @click="flow.cancel()">{{ s.refusal ? t("dm.schemaChange.close") : t("common.cancel") }}</button>
        <button v-if="!s.refusal" type="submit" :class="['btn', o?.danger ? 'btn-danger' : 'btn-primary']" :disabled="!canApply || s.applying">
          {{ s.applying ? t("dm.schemaChange.applying") : o?.applyLabel }}
        </button>
      </div>
    </form>
  </dialog>
</template>
