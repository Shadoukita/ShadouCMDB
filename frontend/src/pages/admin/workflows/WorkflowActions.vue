<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { onBeforeRouteLeave, onBeforeRouteUpdate, RouterLink, useRoute, useRouter } from "vue-router";
import { useAllProfiles } from "../../../api/admin";
import { ApiError } from "../../../api/client";
import { MAX_PAGE, useCiClasses, useClassAttributes } from "../../../api/queries";
import {
  useSaveActions,
  useWorkflowActions,
  useWorkflowDraft,
  useWorkflowVersion,
  type WorkflowActions,
  type WorkflowDefinitionDetail,
} from "../../../api/workflows";
import { useWebhookEndpoints } from "../../../api/webhooks";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import SaveBar from "../../../components/SaveBar.vue";
import { useFlashStore } from "../../../stores/flash";
import { t } from "../../../i18n";
import {
  actionFromApi,
  actionsBody,
  checkActions,
  describeTrigger,
  generalProblems,
  newAction,
  notifiesPeople,
  previewableActions,
  recipientLabel,
  transitionChoices,
  type ActionProblem,
  type DraftAction,
} from "../../../lib/workflowActions";
import type { AttributeChoice } from "../../../lib/workflowApprovals";
import ActionEditor from "./ActionEditor.vue";
import ActionPreview from "./ActionPreview.vue";

/**
 * The workflow's notification actions (SHAA-2725 §2.1, §3.1–3.3): inbox entries, e-mails and webhooks
 * fired by a transition, an approval event or an instance being cancelled or forced. They belong to the
 * workflow, not to a version: the whole set is saved at once with its own Save button, guarded by the
 * workflow's version, and applies to every version without publishing. The answer carries the actions
 * lint (recipients who cannot view the type, a transition the current version lacks, missing texts).
 */
const props = defineProps<{ workflow: WorkflowDefinitionDetail }>();
const route = useRoute();
const router = useRouter();
const wid = computed(() => props.workflow.id);
const actionsQ = useWorkflowActions(wid);
const draft = useWorkflowDraft(wid);
const current = useWorkflowVersion(wid, () => props.workflow.currentVersionNo);
const profilesQ = useAllProfiles();
const cannotListProfiles = computed(() => profilesQ.error.value instanceof ApiError && profilesQ.error.value.status === 403);
const profiles = computed(() => (cannotListProfiles.value ? null : (profilesQ.data.value?.data ?? []).map((p) => ({ id: p.id, name: p.name }))));
const classes = useCiClasses();
const attrs = useClassAttributes(() => props.workflow.classId);
const fields = computed(() => (attrs.data.value ?? []).filter((a) => a.isActive));
/** Reference fields of the type (own and inherited) to the Person type. */
const personFields = computed<AttributeChoice[]>(() => {
  const person = new Set((classes.data.value ?? []).filter((c) => c.systemRole === "person").map((c) => c.id));
  return fields.value.filter((a) => a.dataType === "reference" && !!a.referenceClassId && person.has(a.referenceClassId)).map((a) => ({ id: a.id, key: a.key, label: a.label }));
});

// ---------- Local edits ----------

const list = ref<DraftAction[]>([]);
const serialize = (l: DraftAction[]) => JSON.stringify(actionsBody(l));
const base = ref(serialize([]));
/** The workflow's version the local copy was loaded at: sent back, so a save over someone else's fails. */
const baseVersion = ref(0);
/** Keys of the stored actions by position: the lint's paths index them. */
const storedKeys = ref<string[]>([]);
const dirty = computed(() => serialize(list.value) !== base.value);
const editing = ref<DraftAction | null>(null);
/**
 * The endpoints a webhook action can pick: loaded once an action is a webhook. A `workflows.manage`-only
 * caller gets key, name and status. The API serves at most MAX_PAGE; beyond that the picker still shows
 * the action's own key.
 */
const endpointsQ = useWebhookEndpoints({ limit: MAX_PAGE }, () => list.value.some((a) => a.kind === "webhook"));
const endpoints = computed(() => endpointsQ.data.value?.data ?? null);

function seed(a: WorkflowActions) {
  const keyBefore = editing.value?.key;
  list.value = a.actions.map(actionFromApi);
  labelAttributes();
  base.value = serialize(list.value);
  baseVersion.value = a.version;
  storedKeys.value = a.actions.map((x) => x.key);
  editing.value = list.value.find((x) => x.key === keyBefore) ?? null;
}
/** The API names a Person field by id and key: show its label once the type's fields are loaded. */
function labelAttributes() {
  for (const x of list.value) for (const r of x.recipients) if (r.attribute) r.attribute.label = fields.value.find((f) => f.id === r.attribute!.id)?.label ?? r.attribute.key;
}
watch(fields, labelAttributes);
watch(
  () => actionsQ.data.value,
  (a) => {
    if (a && !dirty.value) seed(a);
  },
  { immediate: true },
);

const transitions = computed(() =>
  transitionChoices(
    [(draft.data.value?.transitions ?? []).map((x) => ({ key: x.key, name: x.name })), (current.data.value?.transitions ?? []).map((x) => ({ key: x.key, name: x.name }))],
    list.value,
  ),
);
const transitionName = (key: string) => transitions.value.find((x) => x.key === key)?.name ?? key;

/** `?transition=` narrows the list to the actions about one transition (the designer links here). */
const filter = computed(() => (typeof route.query.transition === "string" ? route.query.transition : ""));
function setFilter(key: string) {
  void router.replace({ query: { ...route.query, transition: key || undefined } });
}
const shown = computed(() => list.value.map((a, i) => ({ a, i })).filter(({ a }) => !filter.value || a.transition === filter.value));

async function add() {
  const a = newAction(list.value, filter.value || transitions.value.find((x) => !x.orphan)?.key || null);
  list.value.push(a);
  editing.value = a;
  await nextTick();
  document.getElementById(`wf-action-${list.value.length - 1}-name`)?.focus();
}
function remove(a: DraftAction) {
  list.value = list.value.filter((x) => x !== a);
  if (editing.value === a) editing.value = null;
}
async function edit(a: DraftAction, i: number) {
  editing.value = editing.value === a ? null : a;
  if (editing.value) {
    await nextTick();
    document.getElementById(`wf-action-${i}-name`)?.focus();
  }
}

// ---------- Problems ----------

const localProblems = computed(() => checkActions(list.value));
/** Paths of a refused save index the body sent; keep the keys it had to find the action again after edits. */
const refused = ref<{ keys: string[]; problems: ActionProblem[] } | null>(null);
const lint = computed<ActionProblem[]>(() => actionsQ.data.value?.problems ?? []);

/** Problems from a body with `keys` by position, moved to the positions those actions have now. */
function reindex(problems: ActionProblem[], keys: string[]): ActionProblem[] {
  const out: ActionProblem[] = [];
  for (const p of problems) {
    const m = /^actions\[(\d+)\](.*)$/.exec(p.path);
    if (!m) continue;
    const at = list.value.findIndex((a) => a.key === keys[Number(m[1])]);
    if (at >= 0) out.push({ ...p, path: `actions[${at}]${m[2]}` });
  }
  return out;
}
const problems = computed<ActionProblem[]>(() => [
  ...localProblems.value,
  ...(refused.value ? reindex(refused.value.problems, refused.value.keys) : []),
  ...(dirty.value ? [] : reindex(lint.value, storedKeys.value)),
]);
const problemsOf = (i: number) => problems.value.filter((p) => new RegExp(`^actions\\[${i}\\]($|[.[])`).test(p.path));
const otherProblems = computed(() => [...generalProblems(refused.value?.problems ?? []), ...(dirty.value ? [] : generalProblems(lint.value))]);
const markerOf = (i: number) => {
  const mine = problemsOf(i);
  return mine.some((p) => p.severity === "error") ? "error" : mine.length ? "warning" : null;
};

// ---------- Saving ----------

const save = useSaveActions();
const flash = useFlashStore();
const error = ref<unknown>(null);
const conflict = computed(() => error.value instanceof ApiError && error.value.code === "VERSION_CONFLICT");

async function submit() {
  if (!actionsQ.data.value || localProblems.value.length) return;
  error.value = null;
  refused.value = null;
  const keys = list.value.map((a) => a.key);
  try {
    const next = await save.mutateAsync({ id: wid.value, body: { version: baseVersion.value, actions: actionsBody(list.value) } });
    seed(next);
    flash.show(next.problems.length ? t("wfActions.savedWithWarnings", { n: next.problems.length }) : t("wfActions.saved"));
  } catch (e) {
    error.value = e;
    if (e instanceof ApiError && e.code === "VALIDATION_ERROR") {
      refused.value = { keys, problems: e.details.map((d) => ({ path: d.field, code: d.code ?? "invalid", message: d.message, severity: "error" as const })) };
      // Open the first action the API refused, so its fields show why.
      const first = reindex(refused.value.problems, keys)[0];
      const at = first ? Number(/^actions\[(\d+)\]/.exec(first.path)?.[1]) : -1;
      if (at >= 0) editing.value = list.value[at];
    }
  }
}
async function reload() {
  error.value = null;
  refused.value = null;
  const res = await actionsQ.refetch();
  if (res.data) seed(res.data);
}
function reset() {
  if (actionsQ.data.value) seed(actionsQ.data.value);
  error.value = null;
  refused.value = null;
}

const keepChanges = (to: { path: string }) => to.path === route.path || !dirty.value || window.confirm(t("wfActions.leave"));
onBeforeRouteLeave(keepChanges);
onBeforeRouteUpdate((to) => (to.query.tab === route.query.tab ? true : !dirty.value || window.confirm(t("wfActions.leave"))));
function onBeforeUnload(e: BeforeUnloadEvent) {
  if (!dirty.value) return;
  e.preventDefault();
  e.returnValue = "";
}
onMounted(() => window.addEventListener("beforeunload", onBeforeUnload));
onBeforeUnmount(() => window.removeEventListener("beforeunload", onBeforeUnload));

/** Who or where, in a few words, for the table. */
function target(a: DraftAction): string {
  if (!notifiesPeople(a.kind)) {
    const name = endpoints.value?.find((e) => e.key === a.endpoint)?.name;
    return a.endpoint ? (name ? `${name} (${a.endpoint})` : t("wfActions.endpointLabel", { key: a.endpoint })) : "—";
  }
  if (a.recipients.length === 0) return "—";
  const first = recipientLabel(a.recipients[0]);
  return a.recipients.length === 1 ? first : t("wfActions.andMore", { first, n: a.recipients.length - 1 });
}
const previewable = computed(() => previewableActions(actionsQ.data.value?.actions ?? []));
const loading = computed(() => actionsQ.isLoading.value || draft.isLoading.value || current.isLoading.value);
</script>

<template>
  <section class="panel" aria-labelledby="wf-actions-title" data-testid="wf-actions">
    <div class="panel-header">
      <h2 id="wf-actions-title">{{ t("wfActions.title") }}</h2>
      <span v-if="!dirty && lint.length" class="badge warn">{{ t("wfApprovers.warnings", { n: lint.length }) }}</span>
      <RouterLink class="btn btn-sm" :to="{ path: '/admin/workflow-deliveries', query: { workflow: workflow.id } }">{{ t("wfActions.deliveries") }}</RouterLink>
    </div>
    <div class="panel-body stack">
      <p class="muted no-margin">{{ t("wfActions.intro") }}</p>
      <div v-if="conflict" class="alert alert-warn" role="alert">
        <div>{{ t("wfActions.conflict") }}</div>
        <div><button type="button" class="btn btn-sm" @click="reload">{{ t("wfActions.reload") }}</button></div>
      </div>
      <div v-else-if="refused" class="alert alert-error" role="alert">
        <strong>{{ t("wfActions.notSaved") }}</strong> {{ t("wfActions.refused") }}
      </div>
      <ErrorAlert v-else-if="error" :error="error" :title="t('wfActions.notSaved')" />
      <div v-if="cannotListProfiles" class="alert" role="note">{{ t("wfApprovers.noProfileList") }}</div>
      <ul v-if="otherProblems.length" class="wf-problems" :aria-label="t('wfApprovers.lint')">
        <li v-for="(p, i) in otherProblems" :key="i" :class="p.severity">
          <span :class="['badge', p.severity === 'error' ? 'danger' : 'warn']">{{ p.severity === "error" ? t("wfApproval.error") : t("wfApproval.warning") }}</span>
          {{ p.message }}
        </li>
      </ul>
    </div>
    <LoadingState v-if="loading" :label="t('wfActions.loading')" />
    <div v-else-if="actionsQ.isError.value" class="panel-body"><ErrorAlert :error="actionsQ.error.value" :on-retry="() => actionsQ.refetch()" /></div>
    <template v-else-if="actionsQ.data.value">
      <div class="panel-body toolbar">
        <div class="field inline-field">
          <label for="wf-actions-filter">{{ t("wfActions.filter") }}</label>
          <select id="wf-actions-filter" :value="filter" @change="setFilter(($event.target as HTMLSelectElement).value)">
            <option value="">{{ t("wfActions.filterAll") }}</option>
            <option v-for="x in transitions" :key="x.key" :value="x.key">{{ x.name }} ({{ x.key }})</option>
          </select>
        </div>
        <div class="toolbar-end">
          <button type="button" class="btn" data-testid="wf-actions-add" @click="add">{{ t("wfActions.add") }}</button>
        </div>
      </div>
      <EmptyState v-if="list.length === 0" :title="t('wfActions.empty.title')">{{ t("wfActions.empty.body") }}</EmptyState>
      <p v-else-if="shown.length === 0" class="panel-body muted no-margin">{{ t("wfActions.emptyFilter", { transition: transitionName(filter) }) }}</p>
      <div v-else class="table-wrap">
        <table class="data" data-testid="wf-actions-table">
          <caption class="sr-only">{{ t("wfActions.caption") }}</caption>
          <thead>
            <tr>
              <th scope="col">{{ t("wfActions.col.name") }}</th>
              <th scope="col">{{ t("wfActions.col.kind") }}</th>
              <th scope="col">{{ t("wfActions.col.when") }}</th>
              <th scope="col">{{ t("wfActions.col.target") }}</th>
              <th scope="col"><span class="sr-only">{{ t("wfActions.col.actions") }}</span></th>
            </tr>
          </thead>
          <tbody>
            <template v-for="{ a, i } in shown" :key="i">
              <tr :class="{ selected: editing === a }" :data-testid="`wf-action-row-${a.key}`">
                <td class="wrap wf-name-cell">
                  {{ a.name || "—" }} <span class="mono muted">{{ a.key }}</span>
                  <span v-if="!a.enabled" class="badge off spaced">{{ t("wfActions.disabled") }}</span>
                  <span v-if="markerOf(i)" :class="['badge', 'spaced', markerOf(i) === 'error' ? 'danger' : 'warn']">
                    {{ markerOf(i) === "error" ? t("wfApproval.error") : t("wfApproval.warning") }}
                  </span>
                </td>
                <td>{{ t(`wfActions.kind.${a.kind}`) }}</td>
                <td class="wrap">{{ describeTrigger(a, transitionName) }}</td>
                <td class="wrap">{{ target(a) }}</td>
                <td class="row-actions">
                  <button type="button" class="btn btn-sm" :aria-expanded="editing === a" :aria-controls="`wf-action-edit-${i}`" @click="edit(a, i)">
                    {{ editing === a ? t("wfActions.close") : t("wfActions.edit") }}<span class="sr-only"> {{ a.name }}</span>
                  </button>
                  <button type="button" class="btn btn-sm btn-quiet-danger" :aria-label="t('wfActions.removeLabel', { name: a.name || a.key })" @click="remove(a)">
                    {{ t("wfApproval.remove") }}
                  </button>
                </td>
              </tr>
              <tr v-if="editing === a" :id="`wf-action-edit-${i}`" class="wf-action-edit-row">
                <td colspan="5">
                  <section :aria-label="t('wfActions.editing', { name: a.name || a.key })">
                    <ActionEditor
                      :action="a"
                      :index="i"
                      :transitions="transitions"
                      :fields="fields"
                      :profiles="profiles"
                      :person-fields="personFields"
                      :endpoints="endpoints"
                      :endpoints-error="endpointsQ.error.value"
                      :problems="problemsOf(i)"
                      @retry-endpoints="endpointsQ.refetch()"
                    />
                  </section>
                </td>
              </tr>
            </template>
          </tbody>
        </table>
      </div>
    </template>
  </section>
  <ActionPreview
    v-if="!loading && actionsQ.data.value"
    :workflow-id="wid"
    :class-id="workflow.classId"
    :actions="previewable"
    :dirty="dirty"
    :selected-key="editing?.key"
  />
  <SaveBar v-if="!loading && !actionsQ.isError.value && actionsQ.data.value" :label="t('record.save.region')" :dirty="dirty">
    <span v-if="dirty && localProblems.length" class="hint" role="status">{{ t("wfActions.fixFirst") }}</span>
    <button v-if="dirty" type="button" class="btn" :disabled="save.isPending.value" @click="reset">{{ t("record.save.discard") }}</button>
    <button
      type="button"
      class="btn btn-primary"
      :disabled="!dirty || save.isPending.value || localProblems.length > 0"
      :title="localProblems.length ? t('wfActions.fixFirst') : undefined"
      data-testid="wf-actions-save"
      @click="submit"
    >
      {{ save.isPending.value ? t("wfApprovers.saving") : t("wfActions.save") }}
    </button>
  </SaveBar>
</template>
