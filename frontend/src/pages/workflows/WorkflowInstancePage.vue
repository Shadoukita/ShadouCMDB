<script setup lang="ts">
import { computed, ref } from "vue";
import { RouterLink, useRoute } from "vue-router";
import { ApiError } from "../../api/client";
import { useCi, useCiClasses, useClassAttributes } from "../../api/queries";
import { EVENT_LABELS, STATUS_LABELS, STATUS_TONES, useForceWorkflowState, useWorkflowEvents, useWorkflowInstance, type WorkflowEvent } from "../../api/workflowRuntime";
import Breadcrumbs from "../../components/Breadcrumbs.vue";
import CiLink from "../../components/CiLink.vue";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import FormDialog from "../../components/FormDialog.vue";
import LoadingState from "../../components/LoadingState.vue";
import PaginationBar from "../../components/PaginationBar.vue";
import { useDocumentTitle } from "../../lib/composables";
import { formatDateTime, formatRelative } from "../../lib/format";
import { CATEGORIES } from "../../lib/workflowDraft";
import { useFlashStore } from "../../stores/flash";
import { useSessionStore } from "../../stores/session";
import FormField from "../form/FormField.vue";
import ChangeValue from "../imports/ChangeValue.vue";
import ApprovalRequestHistory from "./ApprovalRequestHistory.vue";
import PendingApprovalBanner from "./PendingApprovalBanner.vue";
import WorkflowActions from "./WorkflowActions.vue";
import WorkflowStateBadge from "./WorkflowStateBadge.vue";

/**
 * One workflow instance (GET /workflow-instances/{id}): where it stands, the transitions the caller may run,
 * the states and transitions of the version it is pinned to, and its history (GET …/events, oldest first).
 * An instance on a CI of a type the caller may not view does not exist for them (404). Holders of
 * workflows.manage may force it into any state of its version, with a reason.
 */
const route = useRoute();
const session = useSessionStore();
const flash = useFlashStore();
const id = computed(() => String(route.params.id ?? ""));
const q = useWorkflowInstance(id);
const d = computed(() => q.data.value);
const inst = computed(() => d.value?.instance);
useDocumentTitle(() => (inst.value ? `${inst.value.definitionName}: ${inst.value.ciLabel}` : "Workflow"));
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
const categoryLabel = (c: string) => CATEGORIES.find((x) => x.value === c)?.label ?? c;
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
const actor = (e: WorkflowEvent) =>
  e.actorType === "system" ? "System" : e.actorType === "import" ? `Import${e.actorName ? ` (${e.actorName})` : ""}` : `${e.actorName ?? "Unknown"}${e.actorType === "api_client" ? " (API token)" : ""}`;

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
    flash.show(`${i.definitionName} on ${i.ciLabel} is now ${stateName(forceState.value)}.`);
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
  <LoadingState v-if="q.isLoading.value" label="Loading workflow…" />
  <template v-else-if="q.isError.value">
    <Breadcrumbs :items="[{ label: 'Workflows', to: '/workflows' }, { label: notFound ? 'Not found' : 'Error' }]" />
    <EmptyState v-if="notFound" title="Workflow instance not found">
      No workflow instance has the id <code>{{ id }}</code> on a configuration item you may view. It may be on a CI type your permission profiles
      do not show, or the link is wrong.
      <template #actions><RouterLink class="btn" to="/workflows">All workflows</RouterLink></template>
    </EmptyState>
    <ErrorAlert v-else :error="q.error.value" :on-retry="() => q.refetch()" />
  </template>
  <template v-else-if="d && inst">
    <Breadcrumbs
      :items="[
        { label: 'Workflows', to: '/workflows' },
        { label: inst.definitionName, to: `/workflows?workflow=${encodeURIComponent(inst.definitionKey)}` },
        { label: inst.ciLabel },
      ]"
    />
    <div class="page-header">
      <div class="title">
        <h1 dir="auto">{{ inst.definitionName }}</h1>
        <span class="muted">on</span>
        <CiLink :id="inst.ciId">{{ inst.ciLabel }}</CiLink>
        <WorkflowStateBadge :state="inst.state" />
        <span :class="['badge', STATUS_TONES[inst.status]]">{{ STATUS_LABELS[inst.status] }}</span>
      </div>
      <div v-if="mayForce" class="actions">
        <button type="button" class="btn" @click="openForce">Force state…</button>
      </div>
    </div>

    <div class="grid-2 wf-grid">
      <section class="panel" aria-labelledby="wfi-props">
        <div class="panel-header"><h2 id="wfi-props">Instance</h2></div>
        <div class="panel-body">
          <dl class="props">
            <dt>Configuration item</dt>
            <dd>
              <CiLink :id="inst.ciId">{{ inst.ciLabel }}</CiLink>&nbsp;<span class="muted mono">{{ inst.ciIdent }}</span>
            </dd>
            <dt>CI type</dt>
            <dd>{{ ciClass?.name ?? inst.classKey }}</dd>
            <dt>Workflow</dt>
            <dd>
              {{ inst.definitionName }} <span class="muted mono">{{ inst.definitionKey }}</span>, version {{ inst.versionNo }}
            </dd>
            <dt>Started</dt>
            <dd>{{ formatDateTime(inst.startedAt) }} by {{ inst.startedByName }}</dd>
            <dt>Last step</dt>
            <dd>{{ formatDateTime(inst.lastTransitionAt) }}</dd>
            <template v-if="inst.endedAt">
              <dt>{{ inst.status === "cancelled" ? "Cancelled" : "Completed" }}</dt>
              <dd>{{ formatDateTime(inst.endedAt) }}</dd>
            </template>
          </dl>
        </div>
      </section>
      <section class="panel" aria-labelledby="wfi-next">
        <div class="panel-header"><h2 id="wfi-next">Next steps</h2></div>
        <div class="panel-body">
          <p v-if="inst.status !== 'active'" class="muted">This workflow has ended; no further steps run.</p>
          <PendingApprovalBanner v-if="inst.status === 'active' && inst.pendingApproval" :instance="{ ...inst, pendingApproval: inst.pendingApproval }" @reload="reload" />
          <WorkflowActions v-if="inst.status === 'active'" :instance="inst" :transitions="d.availableTransitions" :can-cancel="d.canCancel" :class-id="ciClass?.id" :ci="ci.data.value" @reload="reload" />
        </div>
      </section>
    </div>

    <section class="panel" aria-labelledby="wfi-graph">
      <div class="panel-header">
        <h2 id="wfi-graph">States of version {{ inst.versionNo }}</h2>
        <span class="meta">Every transition is listed; the ones you may run are under Next steps.</span>
      </div>
      <div class="table-wrap">
        <table class="data">
          <thead>
            <tr>
              <th scope="col">State</th>
              <th scope="col">Category</th>
              <th scope="col" style="width: 100%">Transitions out</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="s in d.graph.states" :key="s.key" :class="{ 'row-current': s.key === inst.state.key }" :aria-current="s.key === inst.state.key ? 'step' : undefined">
              <td>
                <WorkflowStateBadge :state="s" />
                <span v-if="s.key === d.graph.initialState" class="muted"> initial</span>
                <span v-if="s.terminal" class="muted"> final</span>
                <strong v-if="s.key === inst.state.key"> current</strong>
              </td>
              <td>{{ categoryLabel(s.category) }}</td>
              <td style="white-space: normal">
                <span v-if="outOf(s.key).length === 0" class="muted">None</span>
                <template v-for="(t, i) in outOf(s.key)" :key="t.key"
                  >{{ i > 0 ? ", " : "" }}{{ t.name }} <span class="muted">to {{ stateName(t.to) }}</span></template
                >
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </section>

    <ApprovalRequestHistory :instance-id="inst.id" :state-name="stateName" />

    <section class="panel" aria-labelledby="wfi-history" data-testid="wf-events">
      <div class="panel-header">
        <h2 id="wfi-history">History</h2>
        <span v-if="events.data.value" class="meta">{{ events.data.value.page.total.toLocaleString() }} events, oldest first</span>
      </div>
      <LoadingState v-if="events.isLoading.value" label="Loading history…" />
      <div v-else-if="events.isError.value" class="panel-body">
        <ErrorAlert :error="events.error.value" :on-retry="() => events.refetch()" />
      </div>
      <template v-else-if="evRows.length > 0">
        <div class="table-wrap">
          <table :class="['data', { loading: events.isPlaceholderData.value }]">
            <thead>
              <tr>
                <th scope="col">When</th>
                <th scope="col">Event</th>
                <th scope="col">Step</th>
                <th scope="col">By</th>
                <th scope="col" style="width: 100%">Comment and changes</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="e in evRows" :key="e.id" style="vertical-align: top">
                <td :title="formatDateTime(e.occurredAt)">{{ formatRelative(e.occurredAt) }}</td>
                <td>{{ e.kind === "transition" ? transitionName(e.transitionKey) : EVENT_LABELS[e.kind] }}</td>
                <td>
                  <template v-if="e.fromStateKey">{{ stateName(e.fromStateKey) }} → </template>{{ stateName(e.toStateKey) }}
                  <span v-if="e.kind === 'migrate'" class="muted"> (v{{ e.fromVersionNo }} → v{{ e.toVersionNo }})</span>
                </td>
                <td>{{ actor(e) }}</td>
                <td style="white-space: normal">
                  <div v-if="e.comment" class="wf-comment" dir="auto">{{ e.comment }}</div>
                  <ul v-if="changes(e).length > 0" class="diff">
                    <li v-for="c in changes(e)" :key="c.key">
                      <span :title="c.key">{{ c.label }}</span>: <del v-if="c.hasOld" dir="auto"><ChangeValue :def="c.def" :value="c.old" /></del> →
                      <ins v-if="c.hasNew" dir="auto"><ChangeValue :def="c.def" :value="c.new" /></ins><span v-else class="muted">cleared</span>
                    </li>
                  </ul>
                  <span v-if="!e.comment && changes(e).length === 0" class="muted">–</span>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <PaginationBar
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
      </template>
      <p v-else class="panel-body muted">No events recorded.</p>
    </section>

    <Teleport to="body">
      <FormDialog :open="forcing" title="Force a state" submit-label="Force state" :busy="force.isPending.value" @submit="confirmForce" @cancel="forcing = false">
        <div v-if="forceError?.code === 'VERSION_CONFLICT'" class="alert alert-warn" role="alert">
          <strong>This workflow moved on since you opened it.</strong>
          <div>Nothing was changed. <button type="button" class="btn btn-sm" @click="reload">Reload the workflow</button></div>
        </div>
        <ErrorAlert v-else-if="force.isError.value" :error="force.error.value" title="The state was not forced" />
        <p>
          Moves the instance into a state without a transition: no condition, field or grant is checked. Use it to repair an instance, not
          to run a step. If the workflow drives a state field, the field is set.
        </p>
        <FormField id="wf-force-state" v-slot="p" label="New state" required>
          <select :id="p.id" v-model="forceState">
            <option v-for="s in d.graph.states" :key="s.key" :value="s.key" :disabled="s.key === inst.state.key">
              {{ s.name }}{{ s.key === inst.state.key ? " (current)" : "" }}{{ s.terminal ? " (final)" : "" }}
            </option>
          </select>
        </FormField>
        <FormField id="wf-force-reason" v-slot="p" label="Reason" required :error="forceMissing ? 'Give a reason.' : forceError?.fieldErrors().reason">
          <textarea :id="p.id" v-model="forceReason" rows="3" maxlength="4000" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
        </FormField>
      </FormDialog>
    </Teleport>
  </template>
</template>
