<script setup lang="ts">
import { computed, ref } from "vue";
import { useRouter } from "vue-router";
import { useRetireVersion, useWorkflowVersion, useWorkflowVersions, type WorkflowDefinitionDetail, type WorkflowVersionSummary } from "../../../api/workflows";
import ConfirmDialog from "../../../components/ConfirmDialog.vue";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import { formatDateTime } from "../../../lib/format";
import { autoLayout, CATEGORIES, describeConditions, draftFromVersion } from "../../../lib/workflowDraft";
import WorkflowGraph from "./WorkflowGraph.vue";

/** Every version of a workflow, newest first: the draft, published and retired ones. Published versions can be viewed and retired. */
const props = defineProps<{ workflow: WorkflowDefinitionDetail }>();
const router = useRouter();
const wid = computed(() => props.workflow.id);
const versions = useWorkflowVersions(wid);
const rows = computed(() => versions.data.value?.data ?? []);

const viewing = ref<number | null>(null);
const viewed = useWorkflowVersion(wid, viewing);
const viewedDraft = computed(() => {
  const v = viewed.data.value;
  if (!v) return null;
  const d = draftFromVersion(v);
  autoLayout(d);
  return d;
});
const stateName = (key: string) => viewed.data.value?.states.find((s) => s.key === key)?.name ?? key;
const categoryLabel = (c: string) => CATEGORIES.find((x) => x.value === c)?.label ?? c;

function view(v: WorkflowVersionSummary) {
  if (v.status === "draft") {
    void router.push({ query: { tab: "designer" } });
    return;
  }
  viewing.value = viewing.value === v.versionNo ? null : v.versionNo;
}

// ---------- Retire ----------

const retire = useRetireVersion();
const retiring = ref<WorkflowVersionSummary | null>(null);
const retired = ref<string | null>(null);
/** Retiring the current version makes the newest other published version current, if there is one. */
const nextCurrent = computed(() => rows.value.find((r) => r.status === "published" && r.versionNo !== retiring.value?.versionNo)?.versionNo ?? null);

function openRetire(v: WorkflowVersionSummary) {
  retire.reset();
  retired.value = null;
  retiring.value = v;
}
function confirmRetire() {
  const v = retiring.value;
  if (!v) return;
  retire.mutate(
    { id: wid.value, no: v.versionNo },
    {
      onSuccess: () => {
        retired.value = `Version ${v.versionNo} retired.`;
        retiring.value = null;
      },
    },
  );
}
</script>

<template>
  <div v-if="retired" class="alert" role="status">{{ retired }}</div>
  <section class="panel" aria-labelledby="wf-versions-title">
    <div class="panel-header">
      <h2 id="wf-versions-title">Versions</h2>
      <span v-if="versions.isFetching.value && !versions.isLoading.value" class="spinner" aria-label="Refreshing" />
    </div>
    <LoadingState v-if="versions.isLoading.value" label="Loading versions…" />
    <div v-else-if="versions.isError.value" class="panel-body">
      <ErrorAlert :error="versions.error.value" :on-retry="() => versions.refetch()" />
    </div>
    <EmptyState v-else-if="rows.length === 0" title="No versions">Start a draft in the Designer tab.</EmptyState>
    <div v-else class="table-wrap">
      <table class="data">
        <thead>
          <tr>
            <th scope="col" class="num">Version</th>
            <th scope="col">Status</th>
            <th scope="col" class="num">States</th>
            <th scope="col" class="num">Transitions</th>
            <th scope="col" class="num">Running instances</th>
            <th scope="col">Published</th>
            <th scope="col">Change note</th>
            <th scope="col"><span class="sr-only">Actions</span></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="v in rows" :key="v.versionNo" :class="{ disabled: v.status === 'retired', selected: viewing === v.versionNo }">
            <td class="num">{{ v.versionNo }}</td>
            <td>
              <span v-if="v.status === 'draft'" class="badge info">Draft</span>
              <span v-else-if="v.status === 'retired'" class="badge off">Retired</span>
              <span v-else class="badge">Published</span>
              <span v-if="v.isCurrent" class="badge ok spaced">Current</span>
            </td>
            <td class="num">{{ v.stateCount }}</td>
            <td class="num">{{ v.transitionCount }}</td>
            <td class="num" :title="v.activeInstanceCount === null ? 'Withheld: the workflow covers types you may not view' : undefined">
              {{ v.activeInstanceCount === null ? "–" : v.activeInstanceCount.toLocaleString() }}
            </td>
            <td>
              <template v-if="v.publishedAt">{{ formatDateTime(v.publishedAt) }} by {{ v.publishedByName }}</template>
              <span v-else class="muted">Not published</span>
            </td>
            <td class="cell-clip" :title="v.changeNote ?? undefined">{{ v.changeNote ?? "" }}</td>
            <td class="row-actions">
              <button type="button" class="btn btn-sm" :aria-expanded="v.status === 'draft' ? undefined : viewing === v.versionNo" @click="view(v)">
                {{ v.status === "draft" ? "Edit" : viewing === v.versionNo ? "Hide" : "View" }}
              </button>
              <button v-if="v.status === 'published'" type="button" class="btn btn-sm btn-quiet-danger" @click="openRetire(v)">Retire…</button>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>

  <section v-if="viewing" class="panel" aria-labelledby="wf-version-view-title">
    <div class="panel-header"><h2 id="wf-version-view-title">Version {{ viewing }} (read-only)</h2></div>
    <LoadingState v-if="viewed.isLoading.value" label="Loading the version…" />
    <div v-else-if="viewed.isError.value" class="panel-body"><ErrorAlert :error="viewed.error.value" :on-retry="() => viewed.refetch()" /></div>
    <template v-else-if="viewedDraft && viewed.data.value">
      <WorkflowGraph :draft="viewedDraft" readonly />
      <div class="table-wrap">
        <table class="data">
          <thead>
            <tr>
              <th scope="col">Transition</th>
              <th scope="col">From → to</th>
              <th scope="col">Comment</th>
              <th scope="col">Fields</th>
              <th scope="col">Conditions</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="t in viewedDraft.transitions" :key="t.key">
              <td>{{ t.name }} <span class="mono muted">{{ t.key }}</span></td>
              <td>{{ stateName(t.from) }} → {{ stateName(t.to) }}</td>
              <td>{{ t.requiresComment ? "Required" : "" }}</td>
              <td>{{ t.fields.map((f) => `${f.attribute}${f.required ? "" : " (optional)"}`).join(", ") }}</td>
              <td>{{ describeConditions(t.conditions) }}</td>
            </tr>
          </tbody>
        </table>
      </div>
      <p class="panel-body muted no-margin">
        States: {{ viewedDraft.states.map((s) => `${s.name} (${categoryLabel(s.category)}${s.terminal ? ", terminal" : ""})`).join(", ") }}.
        Initial: {{ viewedDraft.initialState ? stateName(viewedDraft.initialState) : "none" }}.
      </p>
    </template>
  </section>

  <ConfirmDialog
    :open="!!retiring"
    :title="`Retire version ${retiring?.versionNo ?? ''}?`"
    confirm-label="Retire version"
    :busy="retire.isPending.value"
    @cancel="retiring = null"
    @confirm="confirmRetire"
  >
    <ErrorAlert v-if="retire.isError.value" :error="retire.error.value" title="The version was not retired" />
    <template v-if="retiring">
      <p>No new instance will start on version {{ retiring.versionNo }}. Retiring cannot be undone.</p>
      <p>
        <template v-if="retiring.activeInstanceCount === null">Instances running on it keep running on it until they are migrated.</template>
        <template v-else>
          {{ retiring.activeInstanceCount.toLocaleString() }} running {{ retiring.activeInstanceCount === 1 ? "instance stays" : "instances stay" }} on it until
          migrated.
        </template>
      </p>
      <p v-if="retiring.isCurrent">
        <strong v-if="nextCurrent">Version {{ nextCurrent }} becomes the current version.</strong>
        <strong v-else>No published version is left: the workflow starts no new instances until you publish again.</strong>
      </p>
    </template>
  </ConfirmDialog>
</template>
