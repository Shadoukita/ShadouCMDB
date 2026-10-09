<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import { ApiError } from "../../api/client";
import { useRelTypeList, useRules } from "../../api/datamodel";
import {
  useCreateImportMapping,
  useImportJob,
  useImportMappings,
  useImportMappingSuggestion,
  useSetImportMapping,
  useStartImportDryRun,
  useUpdateImportMapping,
  type ImportJob,
  type ImportJobMapping,
} from "../../api/imports";
import { useAttributesOfClasses, useCiClasses, useClassAttributes } from "../../api/queries";
import ClassBadge from "../../components/ClassBadge.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import FormDialog from "../../components/FormDialog.vue";
import LoadingState from "../../components/LoadingState.vue";
import { refusalCode } from "../../lib/imports";
import {
  blankColumn,
  blankForm,
  checkMapping,
  DATE_FORMATS,
  formFromMapping,
  MATCHABLE_TYPES,
  matchedText,
  MODES,
  needsMatch,
  optionIndex,
  placeApiErrors,
  remapByHeaders,
  subtree,
  targetGroups,
  toDefinition,
  toMapping,
  type MappingForm,
  type MappingProblem,
  type TargetOption,
} from "../../lib/importMapping";
import { useSessionStore } from "../../stores/session";
import { formatNumber, t, tAround } from "../../i18n";
import Icon from "../../components/Icon.vue";

/**
 * Step 2: map the file's columns to a class. Everything a column can map to comes from the class's attribute
 * definitions and the relationship rules, so a new class is importable without a frontend change. Next saves
 * the mapping (PUT /imports/{id}/mapping) and starts the check; the API's field errors land on the table row
 * of their column, or in the summary above it.
 */
const props = defineProps<{ job: ImportJob; presetClassKey?: string; presetFromJob?: string }>();
const emit = defineEmits<{ checking: [] }>();

const session = useSessionStore();
const classesQ = useCiClasses();
const typesQ = useRelTypeList();
const rulesQ = useRules();
const setMapping = useSetImportMapping();
const startDryRun = useStartImportDryRun();

/** Concrete, active classes the user may create or edit CIs in (§1.2). */
const importable = computed(() =>
  (classesQ.data.value ?? []).filter(
    (c) => !c.isAbstract && c.isActive && (session.canOnClass(c.id, "create") || session.canOnClass(c.id, "edit")),
  ),
);

const form = ref<MappingForm>(blankForm(0, null));
const problems = ref<MappingProblem[]>([]);

// ---------- Auto-match and saved mappings ----------

/** The suggestion to fetch and apply (§3.3); null once applied, so later edits are the user's. */
const suggestArgs = ref<{ classKey: string; mappingId: string } | null>(null);
/** The saved mapping the form came from, if any ("None" otherwise). */
const savedId = ref(props.job.mappingId ?? "");
/** Set when the server applied a saved mapping because the file's headers equal its headers. */
const appliedNotice = ref<string>();
/** Per column: how the suggestion mapped it, while the user has not changed that column. */
const matched = ref(new Map<number, { text: string; target: string }>());

function reset() {
  const cols = props.job.columns.length;
  matched.value = new Map();
  appliedNotice.value = undefined;
  if (props.job.mapping) {
    form.value = formFromMapping(props.job.mapping, cols, props.job.file.delimiter);
    suggestArgs.value = null;
    return;
  }
  form.value = blankForm(cols, props.job.file.delimiter, props.presetClassKey ?? "");
  // A corrected file takes the previous upload's mapping (below); otherwise the server suggests one.
  suggestArgs.value = !props.presetFromJob && form.value.classKey ? { classKey: form.value.classKey, mappingId: "" } : null;
}
watch(() => [props.job.id, props.job.mapping, props.job.columns.length], reset, { immediate: true });

function apply(mapping: ImportJobMapping) {
  form.value = formFromMapping(mapping, props.job.columns.length, props.job.file.delimiter);
  problems.value = [];
}

/** "Upload a corrected file": the earlier job's class and mapping, moved onto this file's columns by header. */
const fromJob = useImportJob(() => (props.job.mapping ? undefined : props.presetFromJob));
const fromJobApplied = ref(false);
watch(
  [() => fromJob.data.value, () => fromJob.isError.value],
  ([old, failed]) => {
    if (props.job.mapping || fromJobApplied.value || (!old && !failed)) return;
    fromJobApplied.value = true;
    if (old?.mapping) {
      apply(remapByHeaders(old.mapping, old.columns.map((c) => c.header), props.job.columns.map((c) => c.header)));
      savedId.value = old.mappingId ?? "";
    } else if (form.value.classKey) {
      suggestArgs.value = { classKey: form.value.classKey, mappingId: "" };
    }
  },
  { immediate: true },
);
const fromJobNotice = computed(() => fromJobApplied.value && !!fromJob.data.value?.mapping && !props.job.mapping);

// A preset class the user cannot import into is dropped rather than sent.
watch(
  importable,
  (list) => {
    if (form.value.classKey && list.length && !list.some((c) => c.key === form.value.classKey) && !props.job.mapping) form.value.classKey = "";
  },
  { immediate: true },
);

const cls = computed(() => importable.value.find((c) => c.key === form.value.classKey));

const suggestion = useImportMappingSuggestion(
  () => props.job.id,
  () => suggestArgs.value?.classKey ?? "",
  () => suggestArgs.value?.mappingId ?? "",
  () => !!suggestArgs.value && !!cls.value && suggestArgs.value.classKey === form.value.classKey,
);
watch(
  () => suggestion.data.value,
  (s) => {
    const args = suggestArgs.value;
    if (!s || !args || args.classKey !== form.value.classKey) return;
    suggestArgs.value = null;
    apply(s.mapping);
    matched.value = new Map(
      s.matchedBy.map((m) => [m.column, { text: matchedText(m.via, m.hint), target: form.value.columns[m.column]?.target ?? "ignore" }]),
    );
    if (s.savedMapping) {
      savedId.value = s.savedMapping.id;
      appliedNotice.value = s.savedMapping.byHeaders ? s.savedMapping.name : undefined;
    } else {
      savedId.value = "";
      appliedNotice.value = undefined;
    }
  },
  { immediate: true },
);
const suggesting = computed(() => !!suggestArgs.value && !!cls.value && suggestion.isFetching.value);

const saved = useImportMappings(() => (cls.value ? form.value.classKey : ""));
const savedMapping = computed(() => saved.data.value?.data.find((m) => m.id === savedId.value));
/** Only the creator or an administrator may change a saved mapping. */
const canUpdateSaved = computed(() => !!savedMapping.value && (session.isAdministrator || savedMapping.value.createdBy.id === session.user?.id));

function onSavedChange(id: string) {
  appliedNotice.value = undefined;
  if (!id) {
    // "None": the form stays as it is, it is just no longer tied to a saved mapping.
    savedId.value = "";
    return;
  }
  savedId.value = id;
  suggestArgs.value = { classKey: form.value.classKey, mappingId: id };
}
const attributesQ = useClassAttributes(() => cls.value?.id);
const attributes = computed(() => attributesQ.data.value ?? []);

const groups = computed(() =>
  cls.value && attributesQ.data.value && typesQ.data.value && rulesQ.data.value
    ? targetGroups({
        classId: cls.value.id,
        classes: classesQ.data.value ?? [],
        attributes: attributesQ.data.value,
        types: typesQ.data.value.data,
        rules: rulesQ.data.value.data,
      })
    : undefined,
);
const options = computed(() => (groups.value ? optionIndex(groups.value) : new Map<string, TargetOption>()));
const option = (i: number) => options.value.get(form.value.columns[i]!.target);

/** A new class starts the mapping over: targets of the old class mean nothing in the new one. */
function onClassChange() {
  form.value.keyField = "";
  form.value.columns = form.value.columns.map(blankColumn);
  problems.value = [];
  matched.value = new Map();
  savedId.value = "";
  appliedNotice.value = undefined;
  suggestArgs.value = form.value.classKey ? { classKey: form.value.classKey, mappingId: "" } : null;
}

/** The classes whose attributes "Find the other CI by" offers: reference classes and relationship ends in use. */
const otherClassIds = computed(() => {
  const ids = new Set<string>();
  const all = classesQ.data.value ?? [];
  form.value.columns.forEach((_, i) => {
    const o = option(i);
    if (o?.attribute?.referenceClassId) for (const id of subtree(all, o.attribute.referenceClassId)) ids.add(id);
    for (const id of o?.otherClassIds ?? []) ids.add(id);
  });
  return [...ids];
});
const otherAttributes = useAttributesOfClasses(otherClassIds);

/** Text, integer, IP and CIDR attributes on the classes the other CI may be in, once per key. */
function matchAttributes(o: TargetOption | undefined): { key: string; label: string }[] {
  const ids = o?.attribute?.referenceClassId ? subtree(classesQ.data.value ?? [], o.attribute.referenceClassId) : (o?.otherClassIds ?? []);
  const byKey = new Map<string, string>();
  for (const id of ids) for (const a of otherAttributes.value?.get(id) ?? []) if (a.isActive && MATCHABLE_TYPES.has(a.dataType)) byKey.set(a.key, a.label);
  return [...byKey].map(([key, label]) => ({ key, label })).sort((a, b) => a.label.localeCompare(b.label));
}

/** Ident, or a text, integer, IP or CIDR attribute of the class (§2.2). */
const keyChoices = computed(() => attributes.value.filter((a) => a.isActive && MATCHABLE_TYPES.has(a.dataType)));

const timeZones = (() => {
  const intl = Intl as unknown as { supportedValuesOf?: (k: string) => string[] };
  try {
    return intl.supportedValuesOf?.("timeZone") ?? [];
  } catch {
    return [];
  }
})();

// ---------- Problems ----------

const rowProblems = computed(() => {
  const out = new Map<number, string[]>();
  for (const p of problems.value) if (p.column !== undefined) out.set(p.column, [...(out.get(p.column) ?? []), p.message]);
  return out;
});
const summary = ref<HTMLElement>();
const header = (i: number) => props.job.columns[i]?.header ?? t("imports.map.columnN", { n: formatNumber(i + 1) });

async function showProblems(list: MappingProblem[]) {
  problems.value = list;
  await nextTick();
  summary.value?.focus();
}
/** A changed column drops its own problems; the next check finds any that remain. */
function clearColumnProblems(i: number) {
  if (rowProblems.value.has(i)) problems.value = problems.value.filter((p) => p.column !== i);
}
function focusProblem(p: MappingProblem) {
  document.getElementById(p.column !== undefined ? `import-map-${p.column}` : (p.control ?? "import-mapping-table"))?.focus();
}

function status(i: number): string {
  const c = form.value.columns[i]!;
  if (rowProblems.value.has(i)) return t("imports.map.status.needsAttention");
  const m = matched.value.get(i);
  if (m && m.target === c.target) return m.text;
  if (c.target === "ignore") return t("imports.map.status.notMapped");
  return props.job.mapping && props.job.mappingId ? t("imports.map.status.fromSaved") : t("imports.map.status.mapped");
}

// ---------- Save and check ----------

const failure = ref<unknown>();
const busy = computed(() => setMapping.isPending.value || startDryRun.isPending.value);

async function next() {
  failure.value = undefined;
  const local = checkMapping(form.value, options.value, attributes.value, session.isAdministrator);
  if (local.length) return showProblems(local);
  try {
    await setMapping.mutateAsync({ id: props.job.id, mapping: toMapping(form.value, options.value) });
  } catch (e) {
    if (e instanceof ApiError && e.status === 400 && e.details.length) return showProblems(placeApiErrors(e.details));
    failure.value = e;
    return;
  }
  problems.value = [];
  try {
    await startDryRun.mutateAsync(props.job.id);
    emit("checking");
  } catch (e) {
    failure.value = e;
  }
}
const failureTitle = computed(() => {
  const e = failure.value;
  if (e instanceof ApiError && refusalCode(e) === "import_busy") return t("imports.map.failure.busy");
  return setMapping.isError.value ? t("imports.map.failure.notSaved") : t("imports.map.failure.checkNotStarted");
});

// ---------- Save mapping as… / Update mapping ----------

const createSaved = useCreateImportMapping();
const updateSaved = useUpdateImportMapping();
const dialog = ref<"new" | "update" | null>(null);
const saveName = ref("");
const saveDescription = ref("");
const saveErrors = ref<{ name?: string; description?: string; other?: unknown }>({});
const savedMessage = ref<string>();
const saving = computed(() => createSaved.isPending.value || updateSaved.isPending.value);

function openSave(kind: "new" | "update") {
  saveErrors.value = {};
  savedMessage.value = undefined;
  saveName.value = kind === "update" ? (savedMapping.value?.name ?? "") : "";
  saveDescription.value = kind === "update" ? (savedMapping.value?.description ?? "") : "";
  dialog.value = kind;
}

async function submitSave() {
  saveErrors.value = {};
  const name = saveName.value.trim();
  if (!name) {
    saveErrors.value = { name: t("imports.map.save.nameMissing") };
    return;
  }
  const definition = toDefinition(
    toMapping(form.value, options.value),
    props.job.columns.map((c) => c.header),
  );
  const description = saveDescription.value.trim() || null;
  try {
    const m =
      dialog.value === "update" && savedMapping.value
        ? await updateSaved.mutateAsync({ id: savedMapping.value.id, version: savedMapping.value.version, name, description, definition })
        : await createSaved.mutateAsync({ name, description, classKey: form.value.classKey, definition });
    savedId.value = m.id;
    appliedNotice.value = undefined;
    savedMessage.value = dialog.value === "update" ? t("imports.map.save.updated", { name: m.name }) : t("imports.map.save.savedAs", { name: m.name });
    dialog.value = null;
  } catch (e) {
    if (e instanceof ApiError) {
      const code = refusalCode(e);
      if (code === "duplicate_name") return void (saveErrors.value = { name: t("imports.map.save.duplicateName") });
      if (code === "VERSION_CONFLICT") {
        return void (saveErrors.value = {
          other: new Error(t("imports.map.save.versionConflict")),
        });
      }
      if (e.status === 400 && e.details.length) {
        const out: { name?: string; description?: string; other?: unknown } = {};
        for (const d of e.details) {
          if (d.field === "name") out.name = d.message;
          else if (d.field === "description") out.description = d.message;
          else out.other = e;
        }
        return void (saveErrors.value = out);
      }
    }
    saveErrors.value = { other: e };
  }
}

const loading = computed(() => classesQ.isPending.value || typesQ.isPending.value || rulesQ.isPending.value);
const loadError = computed(() => classesQ.error.value ?? typesQ.error.value ?? rulesQ.error.value ?? attributesQ.error.value);
const editable = computed(() => props.job.status === "ready" || props.job.status === "validated");
const appliedAround = computed(() => tAround("imports.map.appliedByHeaders", "name"));
</script>

<template>
  <section class="panel" aria-labelledby="step-heading">
    <div class="panel-header"><h2 id="step-heading" tabindex="-1">{{ t("imports.map.title") }}</h2></div>
    <div class="panel-body">
      <div v-if="job.status === 'ready' && job.error?.code === 'mapping_invalid'" class="alert alert-error" role="alert">
        <strong>{{ t("imports.map.invalid.title") }}</strong> {{ job.error.message }} {{ t("imports.map.invalid.body") }}
      </div>
      <p v-if="job.status === 'validated'" class="alert" role="status">
        {{ t("imports.map.alreadyChecked") }}
      </p>

      <LoadingState v-if="loading" :label="t('imports.map.loadingClasses')" />
      <ErrorAlert v-else-if="loadError" :error="loadError" :on-retry="() => { classesQ.refetch(); typesQ.refetch(); rulesQ.refetch(); }" />
      <p v-else-if="importable.length === 0" class="alert" role="status">
        {{ t("imports.map.noClass") }}
      </p>

      <form v-else id="import-mapping-form" novalidate @submit.prevent="next">
        <div
          v-if="problems.length"
          ref="summary"
          class="alert alert-error"
          role="alert"
          tabindex="-1"
          aria-labelledby="import-problems-title"
        >
          <strong id="import-problems-title">
            {{ t("imports.map.problems", { n: problems.length }) }}
          </strong>
          <ul class="error-summary">
            <li v-for="(p, n) in problems" :key="n">
              <a href="#" @click.prevent="focusProblem(p)">
                {{
                  p.column !== undefined
                    ? t("imports.map.problemAt", { column: formatNumber(p.column + 1), header: header(p.column), message: p.message })
                    : p.message
                }}
              </a>
            </li>
          </ul>
        </div>

        <div class="import-mapping-settings">
          <div class="field">
            <label for="import-class">{{ t("imports.map.targetClass") }}</label>
            <select id="import-class" v-model="form.classKey" required :disabled="!editable || busy" @change="onClassChange">
              <option value="" disabled>{{ t("imports.template.choose") }}</option>
              <option v-for="c in importable" :key="c.id" :value="c.key">{{ c.name }}</option>
            </select>
            <span v-if="cls" class="hint"><ClassBadge :icon="cls.icon" :color="cls.color" :name="cls.name" /></span>
          </div>

          <div v-if="cls" class="field">
            <label for="import-saved">{{ t("imports.map.savedMapping") }}</label>
            <select
              id="import-saved"
              :value="savedId"
              :disabled="!editable || busy || saved.isPending.value"
              @change="onSavedChange(($event.target as HTMLSelectElement).value)"
            >
              <option value="">{{ t("imports.map.savedNone") }}</option>
              <option v-for="m in saved.data.value?.data ?? []" :key="m.id" :value="m.id">{{ m.name }}</option>
            </select>
            <span v-if="savedMapping?.description" class="hint">{{ savedMapping.description }}</span>
            <span v-else-if="saved.isError.value" class="hint error">{{ t("imports.map.savedFailed") }}</span>
          </div>

          <template v-if="cls">
            <fieldset id="import-mode" class="field" tabindex="-1">
              <legend>{{ t("imports.map.mode") }}</legend>
              <label v-for="m in MODES" :key="m.value" class="checkbox-row">
                <input v-model="form.mode" type="radio" name="import-mode" :value="m.value" :disabled="!editable || busy" />
                {{ m.label }}
              </label>
            </fieldset>

            <div class="field">
              <label for="import-key">{{ t("imports.map.key") }}<span v-if="form.mode !== 'create_only'" aria-hidden="true"> *</span></label>
              <select
                id="import-key"
                v-model="form.keyField"
                :required="form.mode !== 'create_only'"
                :disabled="!editable || busy"
                aria-describedby="import-key-help"
              >
                <option value="">{{ form.mode === "create_only" ? t("imports.map.key.notNeeded") : t("imports.map.choose") }}</option>
                <option value="ident">{{ t("imports.map.ident") }}</option>
                <option v-for="a in keyChoices" :key="a.id" :value="`attributes.${a.key}`">{{ a.label }}</option>
              </select>
              <span id="import-key-help" class="hint">
                {{ t("imports.map.key.hint") }}
              </span>
            </div>

            <fieldset class="field">
              <legend>{{ t("imports.map.emptyCells") }}</legend>
              <label class="checkbox-row">
                <input v-model="form.emptyCells" type="radio" name="import-empty" value="ignore" :disabled="!editable || busy" />
                {{ t("imports.map.emptyCells.ignore") }}
              </label>
              <label class="checkbox-row">
                <input v-model="form.emptyCells" type="radio" name="import-empty" value="clear" :disabled="!editable || busy" />
                {{ t("imports.map.emptyCells.clear") }}
              </label>
            </fieldset>

            <details class="import-mapping-options">
              <summary>{{ t("imports.map.valueOptions") }}</summary>
              <div class="import-file-options">
                <div class="field">
                  <label for="import-decimalSeparator">{{ t("imports.map.decimalSeparator") }}</label>
                  <select id="import-decimalSeparator" v-model="form.decimalSeparator" :disabled="!editable || busy">
                    <option value=".">{{ t("imports.map.decimalPoint") }}</option>
                    <option value=",">{{ t("imports.map.decimalComma") }}</option>
                  </select>
                </div>
                <div class="field">
                  <label for="import-dateFormat">{{ t("imports.map.dateFormat") }}</label>
                  <select id="import-dateFormat" v-model="form.dateFormat" :disabled="!editable || busy">
                    <option v-for="f in DATE_FORMATS" :key="f" :value="f">{{ f }}</option>
                  </select>
                </div>
                <div class="field">
                  <label for="import-timeZone">{{ t("imports.map.timeZoneDefault") }}</label>
                  <input id="import-timeZone" v-model.trim="form.timeZone" list="import-time-zones" :disabled="!editable || busy" />
                </div>
                <div class="field">
                  <label for="import-listSeparator">{{ t("imports.map.listSeparator") }}</label>
                  <input id="import-listSeparator" v-model="form.listSeparator" maxlength="1" size="2" :disabled="!editable || busy" />
                </div>
                <div class="field">
                  <label class="checkbox-row">
                    <input v-model="form.trim" type="checkbox" :disabled="!editable || busy" />
                    {{ t("imports.map.trim") }}
                  </label>
                </div>
              </div>
              <p class="muted">{{ t("imports.map.xlsxDates") }}</p>
            </details>
            <datalist id="import-time-zones">
              <option v-for="z in timeZones" :key="z" :value="z" />
            </datalist>
          </template>
        </div>

        <p v-if="appliedNotice" class="alert alert-success" role="status">
          {{ appliedAround[0] }}<em>{{ appliedNotice }}</em>{{ appliedAround[1] }}
        </p>
        <p v-else-if="fromJobNotice" class="alert" role="status">
          {{ t("imports.map.fromJob") }}
        </p>
        <p v-if="savedMessage" class="alert alert-success" role="status">{{ savedMessage }}</p>
        <ErrorAlert
          v-if="suggestion.isError.value && suggestArgs"
          :error="suggestion.error.value"
          :title="t('imports.map.suggestFailed')"
          :on-retry="() => suggestion.refetch()"
        />

        <LoadingState v-if="suggesting" :label="t('imports.map.suggesting')" />
        <LoadingState v-else-if="cls && !groups" :label="t('imports.map.loadingAttributes')" />
        <div v-else-if="cls && groups" class="table-wrap import-mapping">
          <table id="import-mapping-table" class="data" tabindex="-1">
            <caption>{{ t("imports.map.caption", { n: job.columns.length }) }}</caption>
            <thead>
              <tr>
                <th scope="col">{{ t("imports.map.col.file") }}</th>
                <th scope="col">{{ t("imports.map.col.target") }}</th>
                <th scope="col">{{ t("imports.map.col.options") }}</th>
                <th scope="col">{{ t("imports.col.status") }}</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="(col, i) in job.columns" :key="col.index" :class="{ 'has-error': rowProblems.has(i) }">
                <td :data-label="t('imports.map.col.file')">
                  <strong :id="`import-col-${i}`">{{ col.header }}</strong>
                  <div class="muted import-samples">{{ col.samples.join(" · ") || t("imports.map.noValues") }}</div>
                </td>
                <td :data-label="t('imports.map.col.target')">
                  <select
                    :id="`import-map-${i}`"
                    v-model="form.columns[i]!.target"
                    :aria-labelledby="`import-col-${i}`"
                    :aria-invalid="rowProblems.has(i) || undefined"
                    :aria-describedby="rowProblems.has(i) ? `import-col-${i}-error` : undefined"
                    :disabled="!editable || busy"
                    @change="clearColumnProblems(i)"
                  >
                    <option value="ignore">{{ t("imports.map.ignore") }}</option>
                    <optgroup :label="t('imports.map.group.core')">
                      <option v-for="o in groups.core" :key="o.value" :value="o.value">{{ o.label }}</option>
                    </optgroup>
                    <optgroup v-if="groups.attributes.length" :label="t('imports.map.group.attributes')">
                      <option v-for="o in groups.attributes" :key="o.value" :value="o.value">
                        {{ o.label }}{{ o.attribute?.isRequired ? " *" : "" }}{{ o.definedBy ? ` ${t("imports.map.definedBy", { class: o.definedBy })}` : "" }}
                      </option>
                    </optgroup>
                    <optgroup v-if="groups.relationships.length" :label="t('imports.map.group.relationships')">
                      <option v-for="o in groups.relationships" :key="o.value" :value="o.value">
                        {{ o.label }} ({{ o.direction === "outgoing" ? t("imports.map.dir.outgoing") : t("imports.map.dir.incoming") }})
                      </option>
                    </optgroup>
                  </select>
                  <div v-if="rowProblems.has(i)" :id="`import-col-${i}-error`" class="error">
                    {{ rowProblems.get(i)!.join(" ") }}
                  </div>
                </td>
                <td :data-label="t('imports.map.col.options')" class="import-col-options">
                  <template v-if="needsMatch(option(i))">
                    <label :for="`import-match-${i}`">{{ t("imports.map.matchBy") }}</label>
                    <select :id="`import-match-${i}`" v-model="form.columns[i]!.matchBy" :disabled="!editable || busy">
                      <option value="ident">{{ t("imports.map.ident") }}</option>
                      <option value="label">{{ t("imports.map.matchBy.label") }}</option>
                      <option value="attribute">{{ t("imports.map.matchBy.attribute") }}</option>
                    </select>
                    <template v-if="form.columns[i]!.matchBy === 'attribute'">
                      <label :for="`import-match-attr-${i}`">{{ t("imports.map.matchAttribute") }}</label>
                      <select :id="`import-match-attr-${i}`" v-model="form.columns[i]!.matchAttribute" :disabled="!editable || busy">
                        <option value="">{{ t("imports.map.choose") }}</option>
                        <option v-for="a in matchAttributes(option(i))" :key="a.key" :value="a.key">{{ a.label }}</option>
                      </select>
                    </template>
                    <span v-if="option(i)?.type" class="muted">{{ t("imports.map.listSeparatorHint", { separator: form.listSeparator }) }}</span>
                  </template>
                  <span v-else-if="option(i)?.attribute?.dataType === 'lookup'" class="muted">{{ t("imports.map.lookupHint") }}</span>
                  <template v-else-if="option(i)?.attribute?.dataType === 'number'">
                    <label :for="`import-dec-${i}`">{{ t("imports.map.decimalSeparator") }}</label>
                    <select :id="`import-dec-${i}`" v-model="form.columns[i]!.decimalSeparator" :disabled="!editable || busy">
                      <option value="">{{ t("imports.map.asAboveValue", { value: form.decimalSeparator }) }}</option>
                      <option value=".">{{ t("imports.map.decimalPoint") }}</option>
                      <option value=",">{{ t("imports.map.decimalComma") }}</option>
                    </select>
                  </template>
                  <template v-else-if="option(i)?.attribute?.dataType === 'date'">
                    <label :for="`import-date-${i}`">{{ t("imports.map.dateFormat") }}</label>
                    <select :id="`import-date-${i}`" v-model="form.columns[i]!.dateFormat" :disabled="!editable || busy">
                      <option value="">{{ t("imports.map.asAboveValue", { value: form.dateFormat }) }}</option>
                      <option v-for="f in DATE_FORMATS" :key="f" :value="f">{{ f }}</option>
                    </select>
                  </template>
                  <template v-else-if="option(i)?.attribute?.dataType === 'datetime'">
                    <label :for="`import-tz-${i}`">{{ t("imports.map.timeZone") }}</label>
                    <input
                      :id="`import-tz-${i}`"
                      v-model.trim="form.columns[i]!.timeZone"
                      list="import-time-zones"
                      :placeholder="form.timeZone"
                      :disabled="!editable || busy"
                    />
                  </template>
                  <template v-if="form.columns[i]!.target !== 'ignore'">
                    <label :for="`import-empty-${i}`">{{ t("imports.map.emptyCells") }}</label>
                    <select :id="`import-empty-${i}`" v-model="form.columns[i]!.emptyCells" :disabled="!editable || busy">
                      <option value="">{{ t("imports.map.asAbove") }}</option>
                      <option value="ignore">{{ t("imports.map.emptyCells.leave") }}</option>
                      <option value="clear">{{ t("imports.map.emptyCells.clearValue") }}</option>
                    </select>
                  </template>
                  <span v-if="form.columns[i]!.target === 'ignore'" class="muted">–</span>
                </td>
                <td :data-label="t('imports.col.status')">
                  <span :class="rowProblems.has(i) ? 'status-error' : undefined">
                    <Icon :name="rowProblems.has(i) ? 'circle-alert' : form.columns[i]!.target === 'ignore' ? 'circle' : 'check'" /> {{ status(i) }}
                  </span>
                </td>
              </tr>
            </tbody>
          </table>
        </div>

        <ErrorAlert v-if="failure" :error="failure" :title="failureTitle" />

        <div class="form-footer">
          <button type="submit" class="btn btn-primary" :disabled="!cls || !groups || !editable || busy">
            {{ setMapping.isPending.value ? t("imports.map.savingMapping") : startDryRun.isPending.value ? t("imports.map.startingCheck") : t("imports.map.next") }}
          </button>
          <button v-if="cls && groups" type="button" class="btn" :disabled="busy || saving" @click="openSave('new')">
            {{ savedMapping ? t("imports.map.saveAsNew") : t("imports.map.saveAs") }}
          </button>
          <button v-if="canUpdateSaved" type="button" class="btn" :disabled="busy || saving" @click="openSave('update')">
            {{ t("imports.map.update") }}
          </button>
          <span class="muted">{{ t("imports.map.nothingSaved") }}</span>
        </div>
      </form>
    </div>
  </section>

  <FormDialog
    :open="!!dialog"
    :title="dialog === 'update' ? t('imports.map.dialog.updateTitle', { name: savedMapping?.name ?? '' }) : t('imports.map.dialog.saveTitle')"
    :submit-label="dialog === 'update' ? t('imports.map.dialog.updateSubmit') : t('imports.map.dialog.saveSubmit')"
    :busy="saving"
    @cancel="dialog = null"
    @submit="submitSave"
  >
    <p class="muted">
      {{ t("imports.map.dialog.body", { class: cls?.name ?? t("imports.map.dialog.thisClass") }) }}
    </p>
    <div class="field">
      <label for="import-save-name">{{ t("imports.map.dialog.name") }}</label>
      <input
        id="import-save-name"
        v-model="saveName"
        maxlength="100"
        required
        :aria-invalid="!!saveErrors.name || undefined"
        :aria-describedby="saveErrors.name ? 'import-save-name-error' : undefined"
      />
      <span v-if="saveErrors.name" id="import-save-name-error" class="error">{{ saveErrors.name }}</span>
    </div>
    <div class="field">
      <label for="import-save-description">{{ t("imports.map.dialog.description") }}</label>
      <textarea
        id="import-save-description"
        v-model="saveDescription"
        maxlength="500"
        rows="3"
        :aria-invalid="!!saveErrors.description || undefined"
        :aria-describedby="saveErrors.description ? 'import-save-description-error' : undefined"
      />
      <span v-if="saveErrors.description" id="import-save-description-error" class="error">{{ saveErrors.description }}</span>
    </div>
    <ErrorAlert v-if="saveErrors.other" :error="saveErrors.other" :title="t('imports.map.failure.notSaved')" />
  </FormDialog>
</template>
