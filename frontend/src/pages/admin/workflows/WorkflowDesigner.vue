<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { onBeforeRouteLeave, onBeforeRouteUpdate, useRouter } from "vue-router";
import { ApiError } from "../../../api/client";
import { useLookupListValues } from "../../../api/datamodel";
import { useCiClasses, useClassAttributes } from "../../../api/queries";
import {
  useDiscardDraft,
  usePublishDraft,
  useSaveDraft,
  useWorkflowDraft,
  useWorkflowGrants,
  useWorkflowVersion,
  useWorkflowVersions,
  validateDraft,
  type WorkflowDefinitionDetail,
  type WorkflowDraftBody,
  type WorkflowValidation,
} from "../../../api/workflows";
import ConfirmDialog from "../../../components/ConfirmDialog.vue";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import {
  categoryLabel,
  COLUMN_STEP,
  autoLayout,
  checkDraft,
  describeConditions,
  draftFingerprint,
  draftFromVersion,
  emptyDraft,
  placeProblems,
  problemsFor,
  removeState,
  toDraftBody,
  uniqueKey,
  type Draft,
  type PlacedProblem,
  type Position,
} from "../../../lib/workflowDraft";
import { t } from "../../../i18n";
import { describeSetAttribute } from "../../../lib/workflowActions";
import { changedPolicies, describePolicy } from "../../../lib/workflowApprovals";
import { problemText } from "../../../lib/workflowProblems";
import { useFlashStore } from "../../../stores/flash";
import StateInspector from "./StateInspector.vue";
import TransitionInspector from "./TransitionInspector.vue";
import WorkflowGraph from "./WorkflowGraph.vue";

/**
 * The draft of a workflow: a diagram and tables of its states and transitions, an inspector for the
 * selected one, and the lint. Every edit is saved as the whole graph (PUT …/draft, guarded by the
 * draft's checksum) shortly after it is made, then linted (POST …/draft/validate); problems show on
 * the state or transition they are about. Publishing sends the checksum of the linted draft.
 */
const props = defineProps<{ workflow: WorkflowDefinitionDetail }>();
const router = useRouter();
const flash = useFlashStore();
const wid = computed(() => props.workflow.id);

const draftQ = useWorkflowDraft(wid);
const current = useWorkflowVersion(wid, () => props.workflow.currentVersionNo);
const grants = useWorkflowGrants(wid);
const grantedKeys = computed(() => new Set((grants.data.value?.grants ?? []).filter((g) => g.profiles.length).map((g) => g.transitionKey)));
const attrs = useClassAttributes(() => props.workflow.classId);
const fields = computed(() => (attrs.data.value ?? []).filter((a) => a.isActive));
const labelOf = (key: string) => fields.value.find((f) => f.key === key)?.label ?? key;
const classes = useCiClasses();
const personClassIds = computed(() => new Set((classes.data.value ?? []).filter((c) => c.systemRole === "person").map((c) => c.id)));
const stateField = computed(() => (attrs.data.value ?? []).find((a) => a.id === props.workflow.stateAttributeId));
const stateValuesQ = useLookupListValues(() => stateField.value?.lookupListId);
const stateValues = computed(() => (props.workflow.stateAttributeId ? (stateValuesQ.data.value ?? []) : null));

// ---------- The local draft ----------

const draft = ref<Draft | null>(null);
/** Checksum of the stored draft our local copy is based on: sent with every save. */
const checksum = ref<string | null>(null);
/** What the last successful save sent, to tell unsaved edits apart and to place the lint's problems. */
const savedFingerprint = ref("");
const savedBody = ref<WorkflowDraftBody | null>(null);
const lint = ref<WorkflowValidation | null>(null);
const lintError = ref<unknown>(null);
const saveError = ref<unknown>(null);
const status = ref<"idle" | "saving" | "linting">("idle");
const selected = ref<{ kind: "state" | "transition"; key: string } | null>(null);

function load(v: Parameters<typeof draftFromVersion>[0] & { checksum: string | null }) {
  const d = draftFromVersion(v);
  autoLayout(d);
  draft.value = d;
  checksum.value = v.checksum;
  savedBody.value = toDraftBody(d);
  savedFingerprint.value = draftFingerprint(d);
  saveError.value = null;
  if (selected.value && !exists(selected.value)) selected.value = null;
}

const dirty = computed(() => !!draft.value && draftFingerprint(draft.value) !== savedFingerprint.value);
const localProblems = computed(() => (draft.value ? checkDraft(draft.value) : []));
const conflict = computed(() => saveError.value instanceof ApiError && saveError.value.code === "VERSION_CONFLICT");
/** Problems shown on the diagram and in the inspector: the local checks, else the API's refusal, else the lint. */
const problems = computed<PlacedProblem[]>(() => {
  if (localProblems.value.length) return localProblems.value;
  const e = saveError.value;
  if (e instanceof ApiError && e.code === "VALIDATION_ERROR" && savedAttempt.value) {
    return placeProblems(
      e.details.map((d) => ({ path: d.field, code: d.code ?? "invalid", message: d.message, severity: "error" as const })),
      savedAttempt.value,
    );
  }
  return lint.value && savedBody.value ? placeProblems(lint.value.problems, savedBody.value) : [];
});
const errorCount = computed(() => problems.value.filter((p) => p.severity === "error").length);
const warningCount = computed(() => problems.value.filter((p) => p.severity === "warning").length);

// ---------- Saving and linting ----------

const save = useSaveDraft();
/** The body of the save in flight or last refused, to place a 400's paths. */
const savedAttempt = ref<WorkflowDraftBody | null>(null);
let timer: ReturnType<typeof setTimeout> | undefined;
let again = false;
/** The latest lint; an older answer is dropped. Declared before the seeding watcher below, which lints at once when the draft is already cached. */
let lintSeq = 0;

watch(
  () => draftQ.data.value,
  (v) => {
    // Seed once per stored draft; a refetch does not overwrite local edits (our own saves update the cache too).
    if (v && (!draft.value || v.checksum !== checksum.value) && !dirty.value && status.value === "idle") {
      load(v);
      void runLint();
    }
    if (v === null) draft.value = null;
  },
  { immediate: true },
);


watch(
  () => (draft.value ? draftFingerprint(draft.value) : ""),
  () => {
    if (!dirty.value) return;
    clearTimeout(timer);
    timer = setTimeout(() => void flush(), 700);
  },
);
onBeforeUnmount(() => clearTimeout(timer));

/** Saves the local draft now (when it passes the local checks), then lints it. */
async function flush(): Promise<boolean> {
  clearTimeout(timer);
  const d = draft.value;
  if (!d || !dirty.value || conflict.value) return !dirty.value;
  if (localProblems.value.length) return false;
  if (status.value === "saving") {
    again = true;
    return false;
  }
  status.value = "saving";
  const body = toDraftBody(d, checksum.value);
  const fingerprint = draftFingerprint(d);
  savedAttempt.value = body;
  try {
    const res = await save.mutateAsync({ id: wid.value, body });
    checksum.value = res.checksum;
    savedBody.value = body;
    savedFingerprint.value = fingerprint;
    saveError.value = null;
  } catch (e) {
    saveError.value = e;
    status.value = "idle";
    return false;
  }
  status.value = "idle";
  if (again || dirty.value) {
    again = false;
    if (dirty.value) return flush();
  }
  await runLint();
  return true;
}

async function runLint() {
  const seq = ++lintSeq;
  status.value = status.value === "saving" ? "saving" : "linting";
  try {
    const res = await validateDraft(wid.value);
    if (seq !== lintSeq) return;
    lint.value = res;
    lintError.value = null;
  } catch (e) {
    if (seq === lintSeq) lintError.value = e;
  } finally {
    if (seq === lintSeq && status.value === "linting") status.value = "idle";
  }
}

/** After a 409: drop local edits and load the stored draft (or none, when it was published or discarded). */
async function reloadStored() {
  saveError.value = null;
  savedFingerprint.value = "";
  draft.value = null;
  const res = await draftQ.refetch();
  if (res.data) {
    load(res.data);
    void runLint();
  }
}

/** Leaving with edits not yet saved (another page, or another tab of this one): save them first; ask when that fails. */
async function beforeLeave() {
  if (!dirty.value) return true;
  if (await flush()) return true;
  return window.confirm(t("wfDesign.leave"));
}
onBeforeRouteLeave(beforeLeave);
onBeforeRouteUpdate(beforeLeave);

const saveState = computed(() => {
  if (conflict.value) return { tone: "warn", text: t("wfDesign.save.conflict") };
  if (status.value === "saving") return { tone: "", text: t("common.saving") };
  if (localProblems.value.length) return { tone: "warn", text: t("wfDesign.save.fix") };
  if (saveError.value) return { tone: "warn", text: t("wfDesign.save.failed") };
  if (dirty.value) return { tone: "", text: t("wfDesign.save.dirty") };
  if (status.value === "linting") return { tone: "", text: t("wfDesign.save.checking") };
  return { tone: "ok", text: t("wfDesign.save.ok") };
});

// ---------- Starting and discarding a draft ----------

const starting = ref(false);
const startError = ref<unknown>(null);
/** No draft: start one, from the current version's graph when there is one. */
async function startDraft() {
  starting.value = true;
  startError.value = null;
  try {
    const base = current.data.value ? draftFromVersion(current.data.value) : emptyDraft();
    autoLayout(base);
    const res = await save.mutateAsync({ id: wid.value, body: toDraftBody(base) });
    load(res);
    void runLint();
  } catch (e) {
    startError.value = e;
  } finally {
    starting.value = false;
  }
}

const discard = useDiscardDraft();
const discarding = ref(false);
function confirmDiscard() {
  clearTimeout(timer);
  discard.mutate(
    { id: wid.value },
    {
      onSuccess: () => {
        discarding.value = false;
        draft.value = null;
        savedFingerprint.value = "";
        lint.value = null;
        selected.value = null;
      },
    },
  );
}

// ---------- Editing ----------

function exists(sel: { kind: "state" | "transition"; key: string }) {
  const d = draft.value;
  if (!d) return false;
  return sel.kind === "state" ? d.states.some((s) => s.key === sel.key) : d.transitions.some((t) => t.key === sel.key);
}
const selectedState = computed(() => (selected.value?.kind === "state" ? draft.value?.states.find((s) => s.key === selected.value!.key) : undefined));
const selectedTransition = computed(() =>
  selected.value?.kind === "transition" ? draft.value?.transitions.find((t) => t.key === selected.value!.key) : undefined,
);

function addState() {
  const d = draft.value!;
  const key = uniqueKey("new_state", d.states.map((s) => s.key));
  d.states.push({ key, name: t("wfDesign.newState"), category: d.states.length ? "active" : "open", terminal: false, stateValue: null });
  if (!d.initialState) d.initialState = key;
  // Next to the selected state, else below the others.
  const near = selectedState.value ? d.positions[selectedState.value.key] : undefined;
  if (near) d.positions[key] = { x: near.x + COLUMN_STEP, y: near.y };
  else autoLayout(d);
  selected.value = { kind: "state", key };
}

function addTransition() {
  const d = draft.value!;
  const from = selectedState.value?.key ?? d.states[0].key;
  const to = d.states.find((s) => s.key !== from && !d.transitions.some((t) => t.from === from && t.to === s.key))?.key ?? d.states.find((s) => s.key !== from)!.key;
  const toName = d.states.find((s) => s.key === to)?.name ?? to;
  const key = uniqueKey(`to_${to}`.slice(0, 60), d.transitions.map((t) => t.key));
  d.transitions.push({ key, name: toName, from, to, requiresComment: false, fields: [], conditions: { kind: "group", mode: "all", children: [] }, approval: [], setAttributes: [] });
  selected.value = { kind: "transition", key };
}

const removingState = ref<string | null>(null);
const removingStateTransitions = computed(() =>
  removingState.value ? (draft.value?.transitions.filter((t) => t.from === removingState.value || t.to === removingState.value) ?? []) : [],
);
function confirmRemoveState() {
  if (draft.value && removingState.value) removeState(draft.value, removingState.value);
  removingState.value = null;
  selected.value = null;
}
function removeTransition(key: string) {
  const d = draft.value!;
  d.transitions = d.transitions.filter((t) => t.key !== key);
  for (const t of d.transitions) for (const s of t.approval) s.excludeActorsOf = s.excludeActorsOf.filter((k) => k !== key);
  selected.value = null;
}

function move(key: string, p: Position) {
  if (draft.value) draft.value.positions[key] = p;
}
function arrange() {
  if (draft.value) autoLayout(draft.value, true);
}

function select(target: { kind: "state" | "transition"; key: string }) {
  selected.value = target;
}
function selectProblem(p: PlacedProblem) {
  if (p.target.kind !== "graph") selected.value = { kind: p.target.kind, key: p.target.key };
}
/** A state value by its lookup value's name, as the inspector shows it; the key when the value is gone from the list. */
const stateValueName = (key: string) => stateValues.value?.find((v) => v.key === key)?.name ?? key;
const stateName = (key: string) => draft.value?.states.find((s) => s.key === key)?.name ?? key;
const marker = (kind: "state" | "transition", key: string) => {
  const mine = problemsFor(problems.value, kind, key);
  return mine[0]?.severity;
};

// ---------- Publishing ----------

const publish = usePublishDraft();
const publishing = ref(false);
const changeNote = ref("");
const canPublish = computed(
  () => !!lint.value?.valid && !dirty.value && !saveError.value && status.value === "idle" && localProblems.value.length === 0 && !!checksum.value && lint.value.checksum === checksum.value,
);
const versions = useWorkflowVersions(wid);
/** Instances running on published versions: they keep their version's rules, approvals included, until migrated. */
const runningOnOlder = computed(() =>
  (versions.data.value?.data ?? []).filter((v) => v.status !== "draft").reduce((sum, v) => sum + (v.activeInstanceCount ?? 0), 0),
);
const newlyGated = computed(() => (draft.value ? changedPolicies(draft.value.transitions, current.data.value?.transitions) : []));
const publishWarnings = computed(() => (lint.value && savedBody.value ? placeProblems(lint.value.problems, savedBody.value) : []).filter((p) => p.severity === "warning"));
/** A publish the lint refused after all (the draft or the model changed since the check): its problems, worded like the check's. */
const publishRefusal = computed<PlacedProblem[] | null>(() => {
  const e = publish.error.value;
  if (!(e instanceof ApiError) || e.code !== "VALIDATION_ERROR" || !e.details.length || !savedBody.value) return null;
  const known = lint.value?.problems ?? [];
  return placeProblems(
    e.details.map((d) => ({
      path: d.field,
      code: d.code ?? "invalid",
      message: d.message,
      severity: "error" as const,
      params: known.find((p) => p.path === d.field && p.code === d.code)?.params,
    })),
    savedBody.value,
  );
});

function openPublish() {
  publish.reset();
  changeNote.value = "";
  publishing.value = true;
}
function confirmPublish() {
  const sum = checksum.value;
  if (!sum) return;
  publish.mutate(
    { id: wid.value, expectedDraftChecksum: sum, changeNote: changeNote.value.trim() || null },
    {
      onSuccess: (v) => {
        publishing.value = false;
        draft.value = null;
        lint.value = null;
        savedFingerprint.value = "";
        selected.value = null;
        flash.show(t("wfDesign.published", { n: v.versionNo }));
        void router.push({ query: { tab: "versions" } });
      },
      onError: (e) => {
        // The check panel shows what changed since the last check.
        if (e instanceof ApiError && e.code === "VALIDATION_ERROR") void runLint();
      },
    },
  );
}
</script>

<template>
  <LoadingState v-if="draftQ.isLoading.value" :label="t('wfDesign.loading')" />
  <ErrorAlert v-else-if="draftQ.isError.value" :error="draftQ.error.value" :on-retry="() => draftQ.refetch()" />

  <!-- No draft: everything is published. -->
  <EmptyState v-else-if="!draft" :title="workflow.currentVersionNo ? t('wfDesign.noDraft.title') : t('wfDesign.noDraft.titleFirst')" data-testid="wf-no-draft">
    <template v-if="workflow.currentVersionNo">
      {{ t("wfDesign.noDraft.body", { n: workflow.currentVersionNo }) }}
    </template>
    <template v-else>{{ t("wfDesign.noDraft.bodyFirst") }}</template>
    <ErrorAlert v-if="startError" :error="startError" :title="t('wfDesign.startFailed')" />
    <template #actions>
      <button type="button" class="btn btn-primary" :disabled="starting || (!!workflow.currentVersionNo && !current.data.value)" @click="startDraft">
        {{ starting ? t("wfDesign.creating") : workflow.currentVersionNo ? t("wfDesign.startFrom", { n: workflow.currentVersionNo }) : t("wfDesign.start") }}
      </button>
    </template>
  </EmptyState>

  <template v-else>
    <div class="toolbar wf-toolbar">
      <button type="button" class="btn" @click="addState">{{ t("wfDesign.addState") }}</button>
      <button type="button" class="btn" :disabled="draft.states.length < 2" @click="addTransition">{{ t("wfDesign.addTransition") }}</button>
      <button type="button" class="btn" :disabled="draft.states.length === 0" @click="arrange">{{ t("wfDesign.arrange") }}</button>
      <span :class="['wf-save-state', saveState.tone]" role="status" aria-live="polite" data-testid="wf-save-state">{{ saveState.text }}</span>
      <div class="toolbar-end">
        <button type="button" class="btn btn-quiet-danger" @click="(discard.reset(), (discarding = true))">{{ t("wfDesign.discard") }}</button>
        <button
          type="button"
          class="btn btn-primary"
          :disabled="!canPublish"
          :title="canPublish ? undefined : t('wfDesign.publishHint')"
          @click="openPublish"
        >
          {{ t("wfDesign.publishOpen", { n: workflow.draftVersionNo ?? "" }) }}
        </button>
      </div>
    </div>

    <div v-if="conflict" class="alert alert-warn" role="alert">
      <strong>{{ t("wfDesign.conflict.title") }}</strong>
      <div>{{ t("wfDesign.conflict.body") }}</div>
      <div><button type="button" class="btn btn-sm" @click="reloadStored">{{ t("wfDesign.conflict.reload") }}</button></div>
    </div>
    <ErrorAlert v-else-if="saveError && !(saveError instanceof ApiError && saveError.code === 'VALIDATION_ERROR')" :error="saveError" :title="t('wfDesign.saveFailed')" :on-retry="() => void flush()" />
    <div v-else-if="saveError" class="alert alert-error" role="alert">
      <strong>{{ t("wfDesign.saveFailed") }}.</strong> {{ t("wfDesign.refused") }}
    </div>

    <section class="panel wf-lint" aria-labelledby="wf-lint-title" data-testid="wf-lint">
      <div class="panel-header">
        <h2 id="wf-lint-title">{{ t("wfDesign.check.title") }}</h2>
        <span v-if="errorCount" class="badge danger">{{ t("wfDesign.check.errors", { n: errorCount }) }}</span>
        <span v-if="warningCount" class="badge warn">{{ t("wfDesign.check.warnings", { n: warningCount }) }}</span>
        <span v-if="!errorCount && !warningCount && lint?.valid && !dirty" class="badge ok">{{ t("wfDesign.check.ready") }}</span>
      </div>
      <div class="panel-body">
        <ErrorAlert v-if="lintError" :error="lintError" :title="t('wfDesign.check.failed')" :on-retry="runLint" />
        <p v-else-if="problems.length === 0" class="muted no-margin">
          {{ lint?.valid ? t("wfDesign.check.none") : t("wfDesign.check.checking") }}
        </p>
        <ul v-else class="wf-problems">
          <li v-for="(p, i) in problems" :key="i" :class="p.severity">
            <span :class="['badge', p.severity === 'error' ? 'danger' : 'warn']">{{ p.severity === "error" ? t("wfApproval.error") : t("wfApproval.warning") }}</span>
            <button v-if="p.target.kind !== 'graph'" type="button" class="btn-link" @click="selectProblem(p)">
              {{ t(p.target.kind === "state" ? "wfDesign.problem.state" : "wfDesign.problem.transition", { key: p.target.key }) }}
            </button>
            {{ problemText(p) }}
          </li>
        </ul>
      </div>
    </section>

    <div class="wf-designer-wrap">
      <div class="wf-designer">
        <section class="panel wf-diagram" :aria-label="t('wfDesign.diagram')">
          <EmptyState v-if="draft.states.length === 0" icon="network" :title="t('wfDesign.noStates.title')">
            {{ t("wfDesign.noStates.body") }}
            <template #actions><button type="button" class="btn btn-primary" @click="addState">{{ t("wfDesign.addState") }}</button></template>
          </EmptyState>
          <WorkflowGraph v-else :draft="draft" :selected="selected" :problems="problems" :state-values="stateValues" @select="select" @move="move" />
        </section>
        <div class="wf-side">
          <StateInspector
            v-if="selectedState"
            :key="`s-${selectedState.key}`"
            :draft="draft"
            :state="selectedState"
            :problems="problemsFor(problems, 'state', selectedState.key)"
            :state-values="stateValues"
            :state-field-label="stateField?.label"
            @renamed="(k) => (selected = { kind: 'state', key: k })"
            @remove="removingState = selectedState.key"
          />
          <TransitionInspector
            v-else-if="selectedTransition"
            :key="`t-${selectedTransition.key}`"
            :workflow-id="wid"
            :draft="draft"
            :transition="selectedTransition"
            :problems="problemsFor(problems, 'transition', selectedTransition.key)"
            :fields="fields"
            :state-field-key="stateField?.key"
            :granted-keys="grantedKeys"
            :person-class-ids="personClassIds"
            @renamed="(k) => (selected = { kind: 'transition', key: k })"
            @remove="removeTransition(selectedTransition.key)"
          />
          <section v-else class="panel">
            <div class="panel-body muted">{{ t("wfDesign.selectHint") }}</div>
          </section>
        </div>
      </div>
    </div>

    <div class="grid-2">
      <section class="panel" aria-labelledby="wf-states-title">
        <div class="panel-header"><h2 id="wf-states-title">{{ t("wfDesign.states.title", { n: draft.states.length }) }}</h2></div>
        <div v-if="draft.states.length" class="table-wrap">
          <table class="data">
            <thead>
              <tr>
                <th scope="col">{{ t("wfDesign.col.name") }}</th>
                <th scope="col">{{ t("wfDesign.col.key") }}</th>
                <th scope="col">{{ t("wfDesign.col.category") }}</th>
                <th scope="col">{{ t("wfDesign.col.stateValue") }}</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="s in draft.states" :key="s.key" :class="{ selected: selected?.kind === 'state' && selected.key === s.key }">
                <td class="wrap wf-name-cell">
                  <button type="button" class="btn-link" @click="select({ kind: 'state', key: s.key })">{{ s.name }}</button>
                  <span v-if="draft.initialState === s.key" class="badge info spaced">{{ t("wfDesign.initial") }}</span>
                  <span v-if="s.terminal" class="badge spaced">{{ t("wfDesign.terminal") }}</span>
                  <span v-if="marker('state', s.key)" :class="['badge', 'spaced', marker('state', s.key) === 'error' ? 'danger' : 'warn']">
                    {{ marker("state", s.key) === "error" ? t("wfApproval.error") : t("wfApproval.warning") }}
                  </span>
                </td>
                <td class="mono">{{ s.key }}</td>
                <td>{{ categoryLabel(s.category) }}</td>
                <td :class="{ muted: !s.stateValue }" :title="s.stateValue ?? undefined">{{ s.stateValue ? stateValueName(s.stateValue) : t("wfAdmin.none") }}</td>
              </tr>
            </tbody>
          </table>
        </div>
        <div v-else class="panel-body muted">{{ t("wfDesign.states.empty") }}</div>
      </section>
      <section class="panel" aria-labelledby="wf-transitions-title">
        <div class="panel-header"><h2 id="wf-transitions-title">{{ t("wfDesign.transitions.title", { n: draft.transitions.length }) }}</h2></div>
        <div v-if="draft.transitions.length" class="table-wrap">
          <table class="data">
            <thead>
              <tr>
                <th scope="col">{{ t("wfDesign.col.name") }}</th>
                <th scope="col">{{ t("wfDesign.col.fromTo") }}</th>
                <th scope="col">{{ t("wfDesign.col.needs") }}</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="tn in draft.transitions" :key="tn.key" :class="{ selected: selected?.kind === 'transition' && selected.key === tn.key }">
                <td class="wrap wf-name-cell">
                  <button type="button" class="btn-link" @click="select({ kind: 'transition', key: tn.key })">{{ tn.name }}</button>{{ " " }}
                  <span class="mono muted">{{ tn.key }}</span>
                  <span v-if="marker('transition', tn.key)" :class="['badge', 'spaced', marker('transition', tn.key) === 'error' ? 'danger' : 'warn']">
                    {{ marker("transition", tn.key) === "error" ? t("wfApproval.error") : t("wfApproval.warning") }}
                  </span>
                </td>
                <td class="wrap">{{ stateName(tn.from) }} → {{ stateName(tn.to) }}</td>
                <td :title="describeConditions(tn.conditions, labelOf)">
                  <span class="cell-clip">
                    <span v-if="tn.requiresComment">{{ t("wfDesign.needs.comment") }}{{ " " }}</span>
                    <span v-if="tn.fields.length">{{ t("wfDesign.needs.fields", { n: tn.fields.length }) }}{{ " " }}</span>
                    <span v-if="tn.conditions.children.length">{{ t("wfDesign.needs.if", { conditions: describeConditions(tn.conditions, labelOf) }) }}{{ " " }}</span>
                    <span v-if="tn.approval.length" class="badge info" data-testid="wf-gated">{{ describePolicy(tn.approval) }}</span>
                    <span v-if="tn.setAttributes.length" :title="tn.setAttributes.map((s) => describeSetAttribute(s, labelOf)).join('; ')">
                      {{ t("wfActions.set.count", { n: tn.setAttributes.length }) }}
                    </span>
                    <span v-if="!tn.requiresComment && !tn.fields.length && !tn.conditions.children.length && !tn.approval.length && !tn.setAttributes.length" class="muted">{{ t("wfDesign.needs.nothing") }}</span>
                  </span>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <div v-else class="panel-body muted">{{ t("wfDesign.transitions.empty") }}</div>
      </section>
    </div>

    <ConfirmDialog
      :open="!!removingState"
      :title="t('wfDesign.removeState.title', { name: removingState ? stateName(removingState) : '' })"
      :confirm-label="t('wfDesign.removeState.confirm')"
      @cancel="removingState = null"
      @confirm="confirmRemoveState"
    >
      <p>{{ t("wfDesign.removeState.body") }}</p>
      <p v-if="removingStateTransitions.length">
        {{ t("wfDesign.removeState.goes", { n: removingStateTransitions.length }) }}
        <strong>{{ removingStateTransitions.map((x) => x.name).join(", ") }}</strong>.
      </p>
    </ConfirmDialog>

    <ConfirmDialog
      :open="discarding"
      :title="t('wfDesign.discardTitle')"
      :confirm-label="t('wfDesign.discardConfirm')"
      :busy="discard.isPending.value"
      @cancel="discarding = false"
      @confirm="confirmDiscard"
    >
      <ErrorAlert v-if="discard.isError.value" :error="discard.error.value" :title="t('wfDesign.discardFailed')" />
      <p>
        {{ t("wfDesign.discardBody") }}
        {{ workflow.currentVersionNo ? t("wfDesign.discardStays", { n: workflow.currentVersionNo }) : t("wfDesign.discardNoVersion") }}
      </p>
    </ConfirmDialog>

    <ConfirmDialog
      :open="publishing"
      :title="t('wfDesign.publishTitle', { n: workflow.draftVersionNo ?? '' })"
      :confirm-label="t('wfDesign.publish')"
      tone="primary"
      :busy="publish.isPending.value"
      :busy-label="t('wfDesign.publishing')"
      @cancel="publishing = false"
      @confirm="confirmPublish"
    >
      <div v-if="publishRefusal" class="alert alert-error" role="alert" data-testid="wf-publish-refused">
        <strong>{{ t("wfDesign.publishFailed") }}</strong>
        <div>{{ t("wfDesign.publishRefused", { n: publishRefusal.length }) }}</div>
        <ul class="no-margin">
          <li v-for="(p, i) in publishRefusal" :key="i">{{ problemText(p) }}</li>
        </ul>
      </div>
      <ErrorAlert v-else-if="publish.isError.value" :error="publish.error.value" :title="t('wfDesign.publishFailed')" />
      <p>
        {{ t("wfDesign.publishBody") }}
      </p>
      <div v-if="runningOnOlder > 0 && newlyGated.length" class="alert alert-warn" data-testid="wf-publish-running">
        <strong>{{ t("wfApproval.publish.runningTitle", { n: runningOnOlder }) }}</strong>
        <div>{{ t("wfApproval.publish.runningBody", { n: runningOnOlder, transitions: newlyGated.join(", ") }) }}</div>
      </div>
      <div v-if="publishWarnings.length" class="alert alert-warn">
        <strong>{{ t("wfDesign.check.warnings", { n: publishWarnings.length }) }}</strong>
        <ul class="no-margin">
          <li v-for="(w, i) in publishWarnings" :key="i">{{ problemText(w) }}</li>
        </ul>
      </div>
      <div class="field">
        <label for="wf-change-note">{{ t("wfDesign.changeNote") }}</label>
        <textarea id="wf-change-note" v-model="changeNote" rows="3" maxlength="2000" aria-describedby="wf-change-note-hint" />
        <span id="wf-change-note-hint" class="hint">{{ t("wfDesign.changeNoteHint") }}</span>
      </div>
    </ConfirmDialog>
  </template>
</template>
