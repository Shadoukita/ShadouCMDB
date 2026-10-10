<script setup lang="ts">
import { computed, ref } from "vue";
import { RouterLink, useRoute } from "vue-router";
import { ApiError } from "../../api/client";
import { useCi, useCiClasses, useClassAttributes } from "../../api/queries";
import { EVENT_LABELS, useForceWorkflowState, useWorkflowEvents, useWorkflowInstance, type WorkflowEvent } from "../../api/workflowRuntime";
import Breadcrumbs from "../../components/Breadcrumbs.vue";
import CiLink from "../../components/CiLink.vue";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import FormDialog from "../../components/FormDialog.vue";
import Icon from "../../components/Icon.vue";
import LoadingState from "../../components/LoadingState.vue";
import PaginationBar from "../../components/PaginationBar.vue";
import SkeletonRows from "../../components/SkeletonRows.vue";
import { t, tAround } from "../../i18n";
import { useDocumentTitle } from "../../lib/composables";
import { formatDateTime, formatRelative } from "../../lib/format";
import { categoryLabel } from "../../lib/workflowDraft";
import { useFlashStore } from "../../stores/flash";
import { useSessionStore } from "../../stores/session";
import FormField from "../form/FormField.vue";
import ChangeValue from "../imports/ChangeValue.vue";
import WorkflowActions from "./WorkflowActions.vue";
import WorkflowStateBadge from "./WorkflowStateBadge.vue";
import WorkflowStatusBadge from "./WorkflowStatusBadge.vue";

/**
 * One workflow instance (GET /workflow-instances/{id}): where it stands, the transitions the caller may run,
 * the states and transitions of the version it is pinned to, and its history (GET …/events, oldest first).
 * An instance on a CI of a type the caller may not view does not exist for them (404). Holders of
 * workflows.manage may force it into any state of its version, with a reason. The head is the record head band
 * (design §0, 12d): the workflow's name, its state and status pills, the CI as a chip and Force state.
 */
const route = useRoute();
const session = useSessionStore();
const flash = useFlashStore();
const id = computed(() => String(route.params.id ?? ""));
const q = useWorkflowInstance(id);
const d = computed(() => q.data.value);
const inst = computed(() => d.value?.instance);
useDocumentTitle(() => (inst.value ? `${inst.value.definitionName}: ${inst.value.ciLabel}` : t("wfRun.instance.docTitle")));
const notFound = computed(() => {
  const e = q.error.value;
  return e instanceof ApiError && (e.code === "NOT_FOUND" || (e.code === "VALIDATION_ERROR" && e.details.some((x) => x.in === "params")));
});

const classes = useCiClasses();
const ciClass = computed(() => classes.data.value?.find((c) => c.key === inst.value?.classKey));
/** Retired fields too: the history may record changes to a field deactivated since. */
const attrs = useClassAttributes(() => ciClass.value?.id, { includeInactive: true });
/** The CI, for the names of the CIs its reference fields point to. */
const ci = useCi(() => inst.value?.ciId);
const fieldDef = (key: string) => attrs.data.value?.find((a) => a.key === key);

const stateName = (key: string | null | undefined) => (key ? (d.value?.graph.states.find((s) => s.key === key)?.name ?? key) : "");
const transitionName = (key: string | null | undefined) => (key ? (d.value?.graph.transitions.find((t) => t.key === key)?.name ?? key) : "");
const outOf = (state: string) => d.value?.graph.transitions.filter((t) => t.from === state) ?? [];

// History, paged.
const evLimit = ref(50);
const evOffset = ref(0);
const events = useWorkflowEvents(id, evLimit, evOffset);
const evRows = computed(() => events.data.value?.data ?? []);
/** Field changes of a step. Lookup and reference fields store ids: ChangeValue shows the value's name and the CI's label. */
function changes(e: WorkflowEvent) {
  const fc = (e.fieldChanges ?? {}) as Record<string, { old?: unknown; new?: unknown }>;
  const set = (v: unknown) => v !== null && v !== undefined && v !== "";
  return Object.entries(fc).map(([k, v]) => {
    const def = fieldDef(k);
    return { key: k, label: def?.label ?? k, def, old: v?.old, new: v?.new, hasOld: set(v?.old), hasNew: set(v?.new) };
  });
}
function actor(e: WorkflowEvent) {
  if (e.actorType === "system") return t("wfRun.actor.system");
  if (e.actorType === "import") return e.actorName ? t("wfRun.actor.importBy", { name: e.actorName }) : t("wfRun.actor.import");
  const name = e.actorName ?? t("wfRun.actor.unknown");
  return e.actorType === "api_client" ? t("wfRun.actor.apiToken", { name }) : name;
}
const notFoundParts = computed(() => tAround("wfRun.instance.notFound.body", "id"));
const crumbs = computed(() => {
  const root = { label: t("wfRun.list.title"), to: "/workflows" };
  const i = inst.value;
  if (!i) return [root, { label: notFound.value ? t("record.crumb.notFound") : t("common.error") }];
  return [root, { label: i.definitionName, to: `/workflows?workflow=${encodeURIComponent(i.definitionKey)}` }, { label: i.ciLabel }];
});

// Force a state (workflows.manage).
const mayForce = computed(() => session.can("workflows.manage") && inst.value?.status === "active");
const forcing = ref(false);
const forceState = ref("");
const forceReason = ref("");
const forceMissing = ref(false);
const force = useForceWorkflowState();
const forceError = computed(() => (force.error.value instanceof ApiError ? force.error.value : null));
function openForce() {
  forceState.value = d.value?.graph.states.find((s) => s.key !== inst.value?.state.key)?.key ?? "";
  forceReason.value = "";
  forceMissing.value = false;
  force.reset();
  forcing.value = true;
}
async function confirmForce() {
  const i = inst.value;
  if (!i) return;
  forceMissing.value = !forceReason.value.trim();
  if (forceMissing.value) return;
  try {
    await force.mutateAsync({ id: i.id, ciId: i.ciId, expectedVersion: i.version, stateKey: forceState.value, reason: forceReason.value.trim() });
    flash.show(t("wfRun.force.done", { workflow: i.definitionName, ci: i.ciLabel, state: stateName(forceState.value) }));
    forcing.value = false;
  } catch {
    // shown in the dialog
  }
}
async function reload() {
  forcing.value = false;
  await q.refetch();
  await events.refetch();
}
</script>

<template>
  <LoadingState v-if="q.isLoading.value" :label="t('wfRun.instance.loading')" />
  <template v-else-if="q.isError.value">
    <Breadcrumbs :items="crumbs" />
    <EmptyState v-if="notFound" :title="t('wfRun.instance.notFound.title')">
      {{ notFoundParts[0] }}<code>{{ id }}</code>{{ notFoundParts[1] }}
      <template #actions><RouterLink class="btn" to="/workflows">{{ t("wfRun.instance.notFound.all") }}</RouterLink></template>
    </EmptyState>
    <ErrorAlert v-else :error="q.error.value" :on-retry="() => q.refetch()" />
  </template>
  <template v-else-if="d && inst">
    <div class="record-head record-head-plain wf-instance-head">
      <Breadcrumbs :items="crumbs" />
      <div class="page-header record-header">
        <div class="record-heading">
          <span class="class-tile class-tile-lg" aria-hidden="true"><Icon name="circle-check" class="class-icon" /></span>
          <div class="record-title">
            <div class="title">
              <h1 dir="auto">{{ inst.definitionName }}</h1>
            </div>
            <p class="record-meta" data-testid="record-meta">
              <WorkflowStateBadge :state="inst.state" />
              <WorkflowStatusBadge :status="inst.status" />
              <CiLink :id="inst.ciId" class="badge record-class-chip">{{ inst.ciLabel }}</CiLink>
              <span class="badge mono">{{ t("wfRun.instance.versionChip", { n: inst.versionNo }) }}</span>
              <span class="record-meta-line">
                <time :datetime="inst.startedAt" :title="formatDateTime(inst.startedAt)">{{
                  t("wfRun.instance.startedBy", { when: formatRelative(inst.startedAt), name: inst.startedByName })
                }}</time>
              </span>
            </p>
          </div>
        </div>
        <div v-if="mayForce" class="actions">
          <button type="button" class="btn" @click="openForce">{{ t("wfRun.force.open") }}</button>
        </div>
      </div>
    </div>

    <div class="grid-2 wf-grid">
      <section class="panel" aria-labelledby="wfi-props">
        <div class="panel-header"><h2 id="wfi-props">{{ t("wfRun.instance.props") }}</h2></div>
        <div class="panel-body">
          <dl class="props">
            <dt>{{ t("wfRun.col.ci") }}</dt>
            <dd>
              <CiLink :id="inst.ciId">{{ inst.ciLabel }}</CiLink>&nbsp;<span class="muted mono">{{ inst.ciIdent }}</span>
            </dd>
            <dt>{{ t("wfRun.col.class") }}</dt>
            <dd dir="auto">{{ ciClass?.name ?? inst.classKey }}</dd>
            <dt>{{ t("wfRun.col.workflow") }}</dt>
            <dd>
              <span dir="auto">{{ inst.definitionName }}</span> <span class="muted mono">{{ inst.definitionKey }}</span>,
              {{ t("wfRun.instance.version", { n: inst.versionNo }) }}
            </dd>
            <dt>{{ t("wfRun.col.started") }}</dt>
            <dd>{{ t("wfRun.startedTitle", { when: formatDateTime(inst.startedAt), name: inst.startedByName }) }}</dd>
            <dt>{{ t("wfRun.col.lastStep") }}</dt>
            <dd>{{ formatDateTime(inst.lastTransitionAt) }}</dd>
            <template v-if="inst.endedAt">
              <dt>{{ inst.status === "cancelled" ? t("wfRun.instance.cancelledAt") : t("wfRun.instance.completedAt") }}</dt>
              <dd>{{ formatDateTime(inst.endedAt) }}</dd>
            </template>
          </dl>
        </div>
      </section>
      <section class="panel" aria-labelledby="wfi-next">
        <div class="panel-header"><h2 id="wfi-next">{{ t("wfRun.instance.next") }}</h2></div>
        <div class="panel-body">
          <p v-if="inst.status !== 'active'" class="muted">{{ t("wfRun.instance.ended") }}</p>
          <WorkflowActions v-else :instance="inst" :transitions="d.availableTransitions" :can-cancel="d.canCancel" :class-id="ciClass?.id" :ci="ci.data.value" @reload="reload" />
        </div>
      </section>
    </div>

    <section class="panel" aria-labelledby="wfi-graph">
      <div class="panel-header">
        <h2 id="wfi-graph">{{ t("wfRun.graph.title", { n: inst.versionNo }) }}</h2>
        <span class="meta">{{ t("wfRun.graph.meta") }}</span>
      </div>
      <div class="table-wrap">
        <table class="data">
          <thead>
            <tr>
              <th scope="col">{{ t("wfRun.col.state") }}</th>
              <th scope="col">{{ t("wfRun.graph.category") }}</th>
              <th scope="col" class="wf-fill">{{ t("wfRun.graph.out") }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="s in d.graph.states" :key="s.key" :class="{ 'row-current': s.key === inst.state.key }" :aria-current="s.key === inst.state.key ? 'step' : undefined">
              <td class="wf-state-cell">
                <WorkflowStateBadge :state="s" />
                <span v-if="s.key === d.graph.initialState" class="muted">{{ t("wfRun.graph.initial") }}</span>
                <span v-if="s.terminal" class="muted">{{ t("wfRun.graph.final") }}</span>
                <strong v-if="s.key === inst.state.key">{{ t("wfRun.graph.current") }}</strong>
              </td>
              <td>{{ categoryLabel(s.category) }}</td>
              <td class="wf-wrap">
                <span v-if="outOf(s.key).length === 0" class="muted">{{ t("wfRun.graph.none") }}</span>
                <template v-for="(tr, i) in outOf(s.key)" :key="tr.key"
                  >{{ i > 0 ? ", " : "" }}<span dir="auto">{{ tr.name }}</span> <span class="muted">{{ t("wfRun.graph.to", { state: stateName(tr.to) }) }}</span></template
                >
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </section>

    <section class="panel" aria-labelledby="wfi-history" data-testid="wf-events">
      <div class="panel-header">
        <h2 id="wfi-history">{{ t("wfRun.history.title") }}</h2>
        <span v-if="events.data.value" class="meta">{{ t("wfRun.history.count", { n: events.data.value.page.total }) }}</span>
      </div>
      <SkeletonRows v-if="events.isLoading.value" :label="t('wfRun.history.loading')" :rows="4" />
      <div v-else-if="events.isError.value" class="panel-body">
        <ErrorAlert :error="events.error.value" :on-retry="() => events.refetch()" />
      </div>
      <template v-else-if="evRows.length > 0">
        <div class="table-wrap">
          <table :class="['data', 'wf-events', { loading: events.isPlaceholderData.value }]">
            <thead>
              <tr>
                <th scope="col">{{ t("wfRun.history.when") }}</th>
                <th scope="col">{{ t("wfRun.history.event") }}</th>
                <th scope="col">{{ t("wfRun.history.step") }}</th>
                <th scope="col">{{ t("wfRun.history.by") }}</th>
                <th scope="col" class="wf-fill">{{ t("wfRun.history.changes") }}</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="e in evRows" :key="e.id">
                <td>
                  <time :datetime="e.occurredAt" :title="formatDateTime(e.occurredAt)">{{ formatRelative(e.occurredAt) }}</time>
                </td>
                <td dir="auto">{{ e.kind === "transition" ? transitionName(e.transitionKey) : t(EVENT_LABELS[e.kind]) }}</td>
                <td>
                  <template v-if="e.fromStateKey">{{ stateName(e.fromStateKey) }} → </template>{{ stateName(e.toStateKey) }}
                  <span v-if="e.kind === 'migrate'" class="muted mono"> (v{{ e.fromVersionNo }} → v{{ e.toVersionNo }})</span>
                </td>
                <td dir="auto">{{ actor(e) }}</td>
                <td class="wf-wrap">
                  <div v-if="e.comment" class="wf-comment" dir="auto">{{ e.comment }}</div>
                  <ul v-if="changes(e).length > 0" class="diff">
                    <li v-for="c in changes(e)" :key="c.key">
                      <span :title="c.key">{{ c.label }}</span>: <del v-if="c.hasOld" dir="auto"><ChangeValue :def="c.def" :value="c.old" /></del> →
                      <ins v-if="c.hasNew" dir="auto"><ChangeValue :def="c.def" :value="c.new" /></ins><span v-else class="muted">{{ t("wfRun.history.cleared") }}</span>
                    </li>
                  </ul>
                  <span v-if="!e.comment && changes(e).length === 0" class="muted">–</span>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <div class="table-footer">
          <PaginationBar
            numbered
            :total="events.data.value?.page.total ?? 0"
            :limit="evLimit"
            :offset="evOffset"
            @change="
              (p) => {
                evLimit = p.limit;
                evOffset = p.offset;
              }
            "
          />
        </div>
      </template>
      <EmptyState v-else :title="t('wfRun.history.none')" />
    </section>

    <Teleport to="body">
      <FormDialog :open="forcing" :title="t('wfRun.force.title')" :submit-label="t('wfRun.force.submit')" :busy="force.isPending.value" @submit="confirmForce" @cancel="forcing = false">
        <div v-if="forceError?.code === 'VERSION_CONFLICT'" class="alert alert-warn" role="alert">
          <strong>{{ t("wfRun.conflict.title") }}</strong>
          <div>
            {{ t("wfRun.conflict.nothingChanged") }} <button type="button" class="btn btn-sm" @click="reload">{{ t("wfRun.conflict.reload") }}</button>
          </div>
        </div>
        <ErrorAlert v-else-if="force.isError.value" :error="force.error.value" :title="t('wfRun.force.failed')" />
        <p>{{ t("wfRun.force.body") }}</p>
        <FormField id="wf-force-state" v-slot="p" :label="t('wfRun.force.state')" required>
          <select :id="p.id" v-model="forceState">
            <option v-for="s in d.graph.states" :key="s.key" :value="s.key" :disabled="s.key === inst.state.key" dir="auto">
              {{ s.name }}{{ s.key === inst.state.key ? ` (${t("wfRun.graph.current")})` : "" }}{{ s.terminal ? ` (${t("wfRun.graph.final")})` : "" }}
            </option>
          </select>
        </FormField>
        <FormField id="wf-force-reason" v-slot="p" :label="t('wfRun.reason')" required :error="forceMissing ? t('wfRun.reasonMissing') : forceError?.fieldErrors().reason">
          <textarea :id="p.id" v-model="forceReason" rows="3" maxlength="4000" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
        </FormField>
      </FormDialog>
    </Teleport>
  </template>
</template>
