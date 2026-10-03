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
const header = (i: number) => props.job.columns[i]?.header ?? `Column ${i + 1}`;

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
  if (rowProblems.value.has(i)) return "Needs attention";
  const m = matched.value.get(i);
  if (m && m.target === c.target) return m.text;
  if (c.target === "ignore") return "Not mapped";
  return props.job.mapping && props.job.mappingId ? "From saved mapping" : "Mapped";
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
  if (e instanceof ApiError && refusalCode(e) === "import_busy") return "Another import of yours is running";
  return setMapping.isError.value ? "The mapping was not saved" : "The check did not start";
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
    saveErrors.value = { name: "Enter a name." };
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
    savedMessage.value = dialog.value === "update" ? `Mapping “${m.name}” updated.` : `Mapping saved as “${m.name}”.`;
    dialog.value = null;
  } catch (e) {
    if (e instanceof ApiError) {
      const code = refusalCode(e);
      if (code === "duplicate_name") return void (saveErrors.value = { name: "A mapping with this name already exists for this class. Choose another name." });
      if (code === "VERSION_CONFLICT") {
        return void (saveErrors.value = {
          other: new Error("Someone else changed this mapping after you loaded it. Choose it again under Saved mapping, then save your changes."),
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
</script>

<template>
  <section class="panel" aria-labelledby="step-heading">
    <div class="panel-header"><h2 id="step-heading" tabindex="-1">Map columns</h2></div>
    <div class="panel-body">
      <div v-if="job.status === 'ready' && job.error?.code === 'mapping_invalid'" class="alert alert-error" role="alert">
        <strong>The mapping no longer fits the data model.</strong> {{ job.error.message }} Check the mapping and start the check again.
      </div>
      <p v-if="job.status === 'validated'" class="alert" role="status">
        This file was already checked with the mapping below. Saving the mapping again discards that result and checks the file again.
      </p>

      <LoadingState v-if="loading" label="Loading classes…" />
      <ErrorAlert v-else-if="loadError" :error="loadError" :on-retry="() => { classesQ.refetch(); typesQ.refetch(); rulesQ.refetch(); }" />
      <p v-else-if="importable.length === 0" class="alert" role="status">
        You cannot create or edit CIs in any class, so there is nothing to import into.
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
            {{ problems.length === 1 ? "1 problem in the mapping" : `${problems.length} problems in the mapping` }}
          </strong>
          <ul class="error-summary">
            <li v-for="(p, n) in problems" :key="n">
              <a href="#" @click.prevent="focusProblem(p)">
                <template v-if="p.column !== undefined">Column {{ p.column + 1 }} ({{ header(p.column) }}): </template>{{ p.message }}
              </a>
            </li>
          </ul>
        </div>

        <div class="import-mapping-settings">
          <div class="field">
            <label for="import-class">Target class</label>
            <select id="import-class" v-model="form.classKey" required :disabled="!editable || busy" @change="onClassChange">
              <option value="" disabled>Choose a class…</option>
              <option v-for="c in importable" :key="c.id" :value="c.key">{{ c.name }}</option>
            </select>
            <span v-if="cls" class="hint"><ClassBadge :icon="cls.icon" :color="cls.color" :name="cls.name" /></span>
          </div>

          <div v-if="cls" class="field">
            <label for="import-saved">Saved mapping</label>
            <select
              id="import-saved"
              :value="savedId"
              :disabled="!editable || busy || saved.isPending.value"
              @change="onSavedChange(($event.target as HTMLSelectElement).value)"
            >
              <option value="">None</option>
              <option v-for="m in saved.data.value?.data ?? []" :key="m.id" :value="m.id">{{ m.name }}</option>
            </select>
            <span v-if="savedMapping?.description" class="hint">{{ savedMapping.description }}</span>
            <span v-else-if="saved.isError.value" class="hint error">The saved mappings could not be loaded.</span>
          </div>

          <template v-if="cls">
            <fieldset id="import-mode" class="field" tabindex="-1">
              <legend>Mode</legend>
              <label v-for="m in MODES" :key="m.value" class="checkbox-row">
                <input v-model="form.mode" type="radio" name="import-mode" :value="m.value" :disabled="!editable || busy" />
                {{ m.label }}
              </label>
            </fieldset>

            <div class="field">
              <label for="import-key">Match existing CIs by<span v-if="form.mode !== 'create_only'" aria-hidden="true"> *</span></label>
              <select
                id="import-key"
                v-model="form.keyField"
                :required="form.mode !== 'create_only'"
                :disabled="!editable || busy"
                aria-describedby="import-key-help"
              >
                <option value="">{{ form.mode === "create_only" ? "Not needed" : "Choose…" }}</option>
                <option value="ident">Ident</option>
                <option v-for="a in keyChoices" :key="a.id" :value="`attributes.${a.key}`">{{ a.label }}</option>
              </select>
              <span id="import-key-help" class="hint">
                A row updates the CI whose value is equal (ignoring case and surrounding spaces). Rows with no match create a CI.
                A value that matches more than one CI is an error.
              </span>
            </div>

            <fieldset class="field">
              <legend>Empty cells</legend>
              <label class="checkbox-row">
                <input v-model="form.emptyCells" type="radio" name="import-empty" value="ignore" :disabled="!editable || busy" />
                Leave the existing value unchanged
              </label>
              <label class="checkbox-row">
                <input v-model="form.emptyCells" type="radio" name="import-empty" value="clear" :disabled="!editable || busy" />
                Clear the existing value
              </label>
            </fieldset>

            <details class="import-mapping-options">
              <summary>Value options</summary>
              <div class="import-file-options">
                <div class="field">
                  <label for="import-decimalSeparator">Decimal separator</label>
                  <select id="import-decimalSeparator" v-model="form.decimalSeparator" :disabled="!editable || busy">
                    <option value=".">Point ( . )</option>
                    <option value=",">Comma ( , )</option>
                  </select>
                </div>
                <div class="field">
                  <label for="import-dateFormat">Date format</label>
                  <select id="import-dateFormat" v-model="form.dateFormat" :disabled="!editable || busy">
                    <option v-for="f in DATE_FORMATS" :key="f" :value="f">{{ f }}</option>
                  </select>
                </div>
                <div class="field">
                  <label for="import-timeZone">Time zone for times without an offset</label>
                  <input id="import-timeZone" v-model.trim="form.timeZone" list="import-time-zones" :disabled="!editable || busy" />
                </div>
                <div class="field">
                  <label for="import-listSeparator">Several values separated by</label>
                  <input id="import-listSeparator" v-model="form.listSeparator" maxlength="1" size="2" :disabled="!editable || busy" />
                </div>
                <div class="field">
                  <label class="checkbox-row">
                    <input v-model="form.trim" type="checkbox" :disabled="!editable || busy" />
                    Trim spaces around text values
                  </label>
                </div>
              </div>
              <p class="muted">XLSX date cells are read as dates and need no format.</p>
            </details>
            <datalist id="import-time-zones">
              <option v-for="z in timeZones" :key="z" :value="z" />
            </datalist>
          </template>
        </div>

        <p v-if="appliedNotice" class="alert" role="status">
          Mapping <em>{{ appliedNotice }}</em> applied because the column names match.
        </p>
        <p v-else-if="fromJobNotice" class="alert" role="status">
          The mapping of the previous upload was applied: columns with the same names keep their targets.
        </p>
        <p v-if="savedMessage" class="alert" role="status">{{ savedMessage }}</p>
        <ErrorAlert
          v-if="suggestion.isError.value && suggestArgs"
          :error="suggestion.error.value"
          title="The columns could not be matched automatically"
          :on-retry="() => suggestion.refetch()"
        />

        <LoadingState v-if="suggesting" label="Matching the file's columns…" />
        <LoadingState v-else-if="cls && !groups" label="Loading the class's attributes…" />
        <div v-else-if="cls && groups" class="table-wrap import-mapping">
          <table id="import-mapping-table" class="data" tabindex="-1">
            <caption>{{ job.columns.length }} columns in the file</caption>
            <thead>
              <tr>
                <th scope="col">File column</th>
                <th scope="col">Maps to</th>
                <th scope="col">Options</th>
                <th scope="col">Status</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="(col, i) in job.columns" :key="col.index" :class="{ 'has-error': rowProblems.has(i) }">
                <td data-label="File column">
                  <strong :id="`import-col-${i}`">{{ col.header }}</strong>
                  <div class="muted import-samples">{{ col.samples.join(" · ") || "(no values)" }}</div>
                </td>
                <td data-label="Maps to">
                  <select
                    :id="`import-map-${i}`"
                    v-model="form.columns[i]!.target"
                    :aria-labelledby="`import-col-${i}`"
                    :aria-invalid="rowProblems.has(i) || undefined"
                    :aria-describedby="rowProblems.has(i) ? `import-col-${i}-error` : undefined"
                    :disabled="!editable || busy"
                    @change="clearColumnProblems(i)"
                  >
                    <option value="ignore">Do not import</option>
                    <optgroup label="Core fields">
                      <option v-for="o in groups.core" :key="o.value" :value="o.value">{{ o.label }}</option>
                    </optgroup>
                    <optgroup v-if="groups.attributes.length" label="Attributes">
                      <option v-for="o in groups.attributes" :key="o.value" :value="o.value">
                        {{ o.label }}{{ o.attribute?.isRequired ? " *" : "" }}{{ o.definedBy ? ` (from ${o.definedBy})` : "" }}
                      </option>
                    </optgroup>
                    <optgroup v-if="groups.relationships.length" label="Relationships">
                      <option v-for="o in groups.relationships" :key="o.value" :value="o.value">
                        {{ o.label }} ({{ o.direction === "outgoing" ? "this CI →" : "→ this CI" }})
                      </option>
                    </optgroup>
                  </select>
                  <div v-if="rowProblems.has(i)" :id="`import-col-${i}-error`" class="error">
                    {{ rowProblems.get(i)!.join(" ") }}
                  </div>
                </td>
                <td data-label="Options" class="import-col-options">
                  <template v-if="needsMatch(option(i))">
                    <label :for="`import-match-${i}`">Find the other CI by</label>
                    <select :id="`import-match-${i}`" v-model="form.columns[i]!.matchBy" :disabled="!editable || busy">
                      <option value="ident">Ident</option>
                      <option value="label">Label</option>
                      <option value="attribute">An attribute</option>
                    </select>
                    <template v-if="form.columns[i]!.matchBy === 'attribute'">
                      <label :for="`import-match-attr-${i}`">Attribute</label>
                      <select :id="`import-match-attr-${i}`" v-model="form.columns[i]!.matchAttribute" :disabled="!editable || busy">
                        <option value="">Choose…</option>
                        <option v-for="a in matchAttributes(option(i))" :key="a.key" :value="a.key">{{ a.label }}</option>
                      </select>
                    </template>
                    <span v-if="option(i)?.type" class="muted">Several values separated by “{{ form.listSeparator }}”</span>
                  </template>
                  <span v-else-if="option(i)?.attribute?.dataType === 'lookup'" class="muted">Values are the name or key of a list value</span>
                  <template v-else-if="option(i)?.attribute?.dataType === 'number'">
                    <label :for="`import-dec-${i}`">Decimal separator</label>
                    <select :id="`import-dec-${i}`" v-model="form.columns[i]!.decimalSeparator" :disabled="!editable || busy">
                      <option value="">As set above ({{ form.decimalSeparator }})</option>
                      <option value=".">Point ( . )</option>
                      <option value=",">Comma ( , )</option>
                    </select>
                  </template>
                  <template v-else-if="option(i)?.attribute?.dataType === 'date'">
                    <label :for="`import-date-${i}`">Date format</label>
                    <select :id="`import-date-${i}`" v-model="form.columns[i]!.dateFormat" :disabled="!editable || busy">
                      <option value="">As set above ({{ form.dateFormat }})</option>
                      <option v-for="f in DATE_FORMATS" :key="f" :value="f">{{ f }}</option>
                    </select>
                  </template>
                  <template v-else-if="option(i)?.attribute?.dataType === 'datetime'">
                    <label :for="`import-tz-${i}`">Time zone</label>
                    <input
                      :id="`import-tz-${i}`"
                      v-model.trim="form.columns[i]!.timeZone"
                      list="import-time-zones"
                      :placeholder="form.timeZone"
                      :disabled="!editable || busy"
                    />
                  </template>
                  <template v-if="form.columns[i]!.target !== 'ignore'">
                    <label :for="`import-empty-${i}`">Empty cells</label>
                    <select :id="`import-empty-${i}`" v-model="form.columns[i]!.emptyCells" :disabled="!editable || busy">
                      <option value="">As set above</option>
                      <option value="ignore">Leave unchanged</option>
                      <option value="clear">Clear the value</option>
                    </select>
                  </template>
                  <span v-if="form.columns[i]!.target === 'ignore'" class="muted">–</span>
                </td>
                <td data-label="Status">
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
            {{ setMapping.isPending.value ? "Saving the mapping…" : startDryRun.isPending.value ? "Starting the check…" : "Next: Check file" }}
          </button>
          <button v-if="cls && groups" type="button" class="btn" :disabled="busy || saving" @click="openSave('new')">
            {{ savedMapping ? "Save as new…" : "Save mapping as…" }}
          </button>
          <button v-if="canUpdateSaved" type="button" class="btn" :disabled="busy || saving" @click="openSave('update')">
            Update mapping…
          </button>
          <span class="muted">Nothing is saved to the inventory until the last step.</span>
        </div>
      </form>
    </div>
  </section>

  <FormDialog
    :open="!!dialog"
    :title="dialog === 'update' ? `Update mapping “${savedMapping?.name ?? ''}”` : 'Save mapping as'"
    :submit-label="dialog === 'update' ? 'Update mapping' : 'Save mapping'"
    :busy="saving"
    @cancel="dialog = null"
    @submit="submitSave"
  >
    <p class="muted">
      Everyone who may import into {{ cls?.name ?? "this class" }} can use this mapping. Files with the same column names
      get it applied automatically.
    </p>
    <div class="field">
      <label for="import-save-name">Name</label>
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
      <label for="import-save-description">Description (optional)</label>
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
    <ErrorAlert v-if="saveErrors.other" :error="saveErrors.other" title="The mapping was not saved" />
  </FormDialog>
</template>
