<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { onBeforeRouteLeave, onBeforeRouteUpdate, useRouter } from "vue-router";
import { ApiError } from "../../../api/client";
import { useLookupListValues } from "../../../api/datamodel";
import { useClassAttributes } from "../../../api/queries";
import {
  useDiscardDraft,
  usePublishDraft,
  useSaveDraft,
  useWorkflowDraft,
  useWorkflowGrants,
  useWorkflowVersion,
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
  CATEGORIES,
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

let lintSeq = 0;
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
  return window.confirm("This draft has changes that could not be saved. Leave and lose them?");
}
onBeforeRouteLeave(beforeLeave);
onBeforeRouteUpdate(beforeLeave);

const saveState = computed(() => {
  if (conflict.value) return { tone: "warn", text: "Not saved: the draft changed elsewhere" };
  if (status.value === "saving") return { tone: "", text: "Saving…" };
  if (localProblems.value.length) return { tone: "warn", text: "Not saved: fix the marked problems" };
  if (saveError.value) return { tone: "warn", text: "Not saved" };
  if (dirty.value) return { tone: "", text: "Unsaved changes" };
  if (status.value === "linting") return { tone: "", text: "Saved · checking…" };
  return { tone: "ok", text: "All changes saved" };
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
  d.states.push({ key, name: "New state", category: d.states.length ? "active" : "open", terminal: false, stateValue: null });
  if (!d.initialState) d.initialState = key;
  // Next to the selected state, else below the others.
  const near = selectedState.value ? d.positions[selectedState.value.key] : undefined;
  if (near) d.positions[key] = { x: near.x + 240, y: near.y };
  else autoLayout(d);
  selected.value = { kind: "state", key };
}

function addTransition() {
  const d = draft.value!;
  const from = selectedState.value?.key ?? d.states[0].key;
  const to = d.states.find((s) => s.key !== from && !d.transitions.some((t) => t.from === from && t.to === s.key))?.key ?? d.states.find((s) => s.key !== from)!.key;
  const toName = d.states.find((s) => s.key === to)?.name ?? to;
  const key = uniqueKey(`to_${to}`.slice(0, 60), d.transitions.map((t) => t.key));
  d.transitions.push({ key, name: toName, from, to, requiresComment: false, fields: [], conditions: { kind: "group", mode: "all", children: [] } });
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
const stateName = (key: string) => draft.value?.states.find((s) => s.key === key)?.name ?? key;
const categoryLabel = (c: string) => CATEGORIES.find((x) => x.value === c)?.label ?? c;
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
const publishWarnings = computed(() => (lint.value?.problems ?? []).filter((p) => p.severity === "warning"));

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
        flash.show(wid.value, `Version ${v.versionNo} published. New instances start on it; running ones stay on their version.`);
        void router.push({ query: { tab: "versions" } });
      },
    },
  );
}
</script>

<template>
  <LoadingState v-if="draftQ.isLoading.value" label="Loading the draft…" />
  <ErrorAlert v-else-if="draftQ.isError.value" :error="draftQ.error.value" :on-retry="() => draftQ.refetch()" />

  <!-- No draft: everything is published. -->
  <EmptyState v-else-if="!draft" :title="workflow.currentVersionNo ? 'No draft' : 'No draft yet'" data-testid="wf-no-draft">
    <template v-if="workflow.currentVersionNo">
      Version {{ workflow.currentVersionNo }} is current. To change the workflow, start a draft from it; running instances are not affected
      until you publish.
    </template>
    <template v-else>Start a draft to design this workflow's states and transitions.</template>
    <ErrorAlert v-if="startError" :error="startError" title="The draft was not created" />
    <template #actions>
      <button type="button" class="btn btn-primary" :disabled="starting || (!!workflow.currentVersionNo && !current.data.value)" @click="startDraft">
        {{ starting ? "Creating…" : workflow.currentVersionNo ? `Start a draft from version ${workflow.currentVersionNo}` : "Start a draft" }}
      </button>
    </template>
  </EmptyState>

  <template v-else>
    <div class="toolbar wf-toolbar">
      <button type="button" class="btn" @click="addState">+ State</button>
      <button type="button" class="btn" :disabled="draft.states.length < 2" @click="addTransition">+ Transition</button>
      <button type="button" class="btn" :disabled="draft.states.length === 0" @click="arrange">Arrange</button>
      <span :class="['wf-save-state', saveState.tone]" role="status" aria-live="polite" data-testid="wf-save-state">{{ saveState.text }}</span>
      <div class="toolbar-end">
        <button type="button" class="btn btn-quiet-danger" @click="(discard.reset(), (discarding = true))">Discard draft…</button>
        <button
          type="button"
          class="btn btn-primary"
          :disabled="!canPublish"
          :title="canPublish ? undefined : 'Publishing needs a saved draft the check finds no errors in.'"
          @click="openPublish"
        >
          Publish version {{ workflow.draftVersionNo ?? "" }}…
        </button>
      </div>
    </div>

    <div v-if="conflict" class="alert alert-warn" role="alert">
      <strong>The draft changed elsewhere.</strong>
      <div>Someone saved, published or discarded this draft since you opened it. Your latest changes were not saved.</div>
      <div><button type="button" class="btn btn-sm" @click="reloadStored">Load the stored draft</button></div>
    </div>
    <ErrorAlert v-else-if="saveError && !(saveError instanceof ApiError && saveError.code === 'VALIDATION_ERROR')" :error="saveError" title="The draft was not saved" :on-retry="() => void flush()" />
    <div v-else-if="saveError" class="alert alert-error" role="alert">
      <strong>The draft was not saved.</strong> The API refused it for the problems marked below.
    </div>

    <section class="panel wf-lint" aria-labelledby="wf-lint-title" data-testid="wf-lint">
      <div class="panel-header">
        <h2 id="wf-lint-title">Check</h2>
        <span v-if="errorCount" class="badge danger">{{ errorCount }} {{ errorCount === 1 ? "error" : "errors" }}</span>
        <span v-if="warningCount" class="badge warn">{{ warningCount }} {{ warningCount === 1 ? "warning" : "warnings" }}</span>
        <span v-if="!errorCount && !warningCount && lint?.valid && !dirty" class="badge ok">Ready to publish</span>
      </div>
      <div class="panel-body">
        <ErrorAlert v-if="lintError" :error="lintError" title="The draft could not be checked" :on-retry="runLint" />
        <p v-else-if="problems.length === 0" class="muted no-margin">
          {{ lint?.valid ? "No problems: the draft can be published." : "Checking…" }}
        </p>
        <ul v-else class="wf-problems">
          <li v-for="(p, i) in problems" :key="i" :class="p.severity">
            <span :class="['badge', p.severity === 'error' ? 'danger' : 'warn']">{{ p.severity === "error" ? "Error" : "Warning" }}</span>
            <button v-if="p.target.kind !== 'graph'" type="button" class="btn-link" @click="selectProblem(p)">
              {{ p.target.kind === "state" ? "State" : "Transition" }} {{ p.target.key }}
            </button>
            {{ p.message }}
          </li>
        </ul>
      </div>
    </section>

    <div class="wf-designer">
      <section class="panel wf-diagram" aria-label="Diagram">
        <EmptyState v-if="draft.states.length === 0" title="No states yet">
          Add the first state; it becomes the initial state. Then add the states it leads to and the transitions between them.
          <template #actions><button type="button" class="btn btn-primary" @click="addState">+ State</button></template>
        </EmptyState>
        <WorkflowGraph v-else :draft="draft" :selected="selected" :problems="problems" @select="select" @move="move" />
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
          :draft="draft"
          :transition="selectedTransition"
          :problems="problemsFor(problems, 'transition', selectedTransition.key)"
          :fields="fields"
          :granted-keys="grantedKeys"
          @renamed="(k) => (selected = { kind: 'transition', key: k })"
          @remove="removeTransition(selectedTransition.key)"
        />
        <section v-else class="panel">
          <div class="panel-body muted">Select a state or transition in the diagram or the tables to edit it.</div>
        </section>
      </div>
    </div>

    <div class="grid-2">
      <section class="panel" aria-labelledby="wf-states-title">
        <div class="panel-header"><h2 id="wf-states-title">States ({{ draft.states.length }})</h2></div>
        <div v-if="draft.states.length" class="table-wrap">
          <table class="data">
            <thead>
              <tr>
                <th scope="col">Name</th>
                <th scope="col">Key</th>
                <th scope="col">Category</th>
                <th scope="col">State value</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="s in draft.states" :key="s.key" :class="{ selected: selected?.kind === 'state' && selected.key === s.key }">
                <td>
                  <button type="button" class="btn-link" @click="select({ kind: 'state', key: s.key })">{{ s.name }}</button>
                  <span v-if="draft.initialState === s.key" class="badge info spaced">Initial</span>
                  <span v-if="s.terminal" class="badge spaced">Terminal</span>
                  <span v-if="marker('state', s.key)" :class="['badge', 'spaced', marker('state', s.key) === 'error' ? 'danger' : 'warn']">
                    {{ marker("state", s.key) === "error" ? "Error" : "Warning" }}
                  </span>
                </td>
                <td class="mono">{{ s.key }}</td>
                <td>{{ categoryLabel(s.category) }}</td>
                <td :class="{ muted: !s.stateValue }">{{ s.stateValue ?? "None" }}</td>
              </tr>
            </tbody>
          </table>
        </div>
        <div v-else class="panel-body muted">None.</div>
      </section>
      <section class="panel" aria-labelledby="wf-transitions-title">
        <div class="panel-header"><h2 id="wf-transitions-title">Transitions ({{ draft.transitions.length }})</h2></div>
        <div v-if="draft.transitions.length" class="table-wrap">
          <table class="data">
            <thead>
              <tr>
                <th scope="col">Name</th>
                <th scope="col">From → to</th>
                <th scope="col">Needs</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="t in draft.transitions" :key="t.key" :class="{ selected: selected?.kind === 'transition' && selected.key === t.key }">
                <td>
                  <button type="button" class="btn-link" @click="select({ kind: 'transition', key: t.key })">{{ t.name }}</button>
                  <span class="mono muted"> {{ t.key }}</span>
                  <span v-if="marker('transition', t.key)" :class="['badge', 'spaced', marker('transition', t.key) === 'error' ? 'danger' : 'warn']">
                    {{ marker("transition", t.key) === "error" ? "Error" : "Warning" }}
                  </span>
                </td>
                <td>{{ stateName(t.from) }} → {{ stateName(t.to) }}</td>
                <td class="cell-clip" :title="describeConditions(t.conditions, labelOf)">
                  <span v-if="t.requiresComment">Comment. </span>
                  <span v-if="t.fields.length">{{ t.fields.length }} {{ t.fields.length === 1 ? "field" : "fields" }}. </span>
                  <span v-if="t.conditions.children.length">If {{ describeConditions(t.conditions, labelOf) }}</span>
                  <span v-if="!t.requiresComment && !t.fields.length && !t.conditions.children.length" class="muted">Nothing</span>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <div v-else class="panel-body muted">None. Add at least two states, then a transition between them.</div>
      </section>
    </div>

    <ConfirmDialog
      :open="!!removingState"
      :title="`Delete state ${removingState ? stateName(removingState) : ''}?`"
      confirm-label="Delete state"
      @cancel="removingState = null"
      @confirm="confirmRemoveState"
    >
      <p>The state is removed from the draft. Published versions and running instances are not affected.</p>
      <p v-if="removingStateTransitions.length">
        {{ removingStateTransitions.length === 1 ? "This transition goes" : "These transitions go" }} with it:
        <strong>{{ removingStateTransitions.map((t) => t.name).join(", ") }}</strong>.
      </p>
    </ConfirmDialog>

    <ConfirmDialog
      :open="discarding"
      title="Discard this draft?"
      confirm-label="Discard draft"
      :busy="discard.isPending.value"
      @cancel="discarding = false"
      @confirm="confirmDiscard"
    >
      <ErrorAlert v-if="discard.isError.value" :error="discard.error.value" title="The draft was not discarded" />
      <p>
        Every unpublished change to the states, transitions, fields and conditions is lost.
        {{ workflow.currentVersionNo ? `Version ${workflow.currentVersionNo} stays current.` : "The workflow has no published version." }}
      </p>
    </ConfirmDialog>

    <ConfirmDialog
      :open="publishing"
      :title="`Publish version ${workflow.draftVersionNo ?? ''}?`"
      confirm-label="Publish"
      tone="primary"
      :busy="publish.isPending.value"
      busy-label="Publishing…"
      @cancel="publishing = false"
      @confirm="confirmPublish"
    >
      <ErrorAlert v-if="publish.isError.value" :error="publish.error.value" title="The version was not published" />
      <p>
        The version becomes current: new instances start on it. Instances already running stay on their version. A published version
        never changes again, and the fields it uses cannot be archived or retyped while it exists.
      </p>
      <div v-if="publishWarnings.length" class="alert alert-warn">
        <strong>{{ publishWarnings.length }} {{ publishWarnings.length === 1 ? "warning" : "warnings" }}</strong>
        <ul class="no-margin">
          <li v-for="(w, i) in publishWarnings" :key="i">{{ w.message }}</li>
        </ul>
      </div>
      <div class="field">
        <label for="wf-change-note">Change note</label>
        <textarea id="wf-change-note" v-model="changeNote" rows="3" maxlength="2000" aria-describedby="wf-change-note-hint" />
        <span id="wf-change-note-hint" class="hint">Optional. Recorded with the version and in the audit log.</span>
      </div>
    </ConfirmDialog>
  </template>
</template>
