<script setup lang="ts">
import { computed, ref } from "vue";
import { useRouter } from "vue-router";
import { useRetireVersion, useWorkflowVersion, useWorkflowVersions, type WorkflowDefinitionDetail, type WorkflowVersionSummary } from "../../../api/workflows";
import ConfirmDialog from "../../../components/ConfirmDialog.vue";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import { formatNumber, t } from "../../../i18n";
import { formatDateTime } from "../../../lib/format";
import { useFlashStore } from "../../../stores/flash";
import { autoLayout, categoryLabel, describeConditions, draftFromVersion } from "../../../lib/workflowDraft";
import WorkflowGraph from "./WorkflowGraph.vue";
import WorkflowMigrate from "./WorkflowMigrate.vue";

/**
 * Every version of a workflow, newest first: the draft, published and retired ones. Published versions can be viewed and
 * retired; the running instances of an older one can be migrated to a newer published version.
 */
const props = defineProps<{ workflow: WorkflowDefinitionDetail }>();
const router = useRouter();
const flash = useFlashStore();
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
const stateList = computed(() =>
  (viewedDraft.value?.states ?? [])
    .map((s) => t(s.terminal ? "wfAdmin.versions.stateTerminal" : "wfAdmin.versions.state", { name: s.name, category: categoryLabel(s.category) }))
    .join(", "),
);

function view(v: WorkflowVersionSummary) {
  if (v.status === "draft") {
    void router.push({ query: { tab: "designer" } });
    return;
  }
  viewing.value = viewing.value === v.versionNo ? null : v.versionNo;
}

// ---------- Migrate ----------

const migratingNo = ref<number | null>(null);
const migrating = computed(() => rows.value.find((r) => r.versionNo === migratingNo.value) ?? null);
const newestPublished = computed(() => Math.max(0, ...rows.value.filter((r) => r.status === "published").map((r) => r.versionNo)));
/** Running instances of a published or retired version can move to a newer published one (a withheld count may hide some). */
const canMigrate = (v: WorkflowVersionSummary) => v.status !== "draft" && v.versionNo < newestPublished.value && v.activeInstanceCount !== 0;
function openMigrate(v: WorkflowVersionSummary) {
  migratingNo.value = migratingNo.value === v.versionNo ? null : v.versionNo;
}

// ---------- Retire ----------

const retire = useRetireVersion();
const retiring = ref<WorkflowVersionSummary | null>(null);
/** Retiring the current version makes the newest other published version current, if there is one. */
const nextCurrent = computed(() => rows.value.find((r) => r.status === "published" && r.versionNo !== retiring.value?.versionNo)?.versionNo ?? null);

function openRetire(v: WorkflowVersionSummary) {
  retire.reset();
  retiring.value = v;
}
function confirmRetire() {
  const v = retiring.value;
  if (!v) return;
  retire.mutate(
    { id: wid.value, no: v.versionNo },
    {
      onSuccess: () => {
        flash.show(t("wfAdmin.versions.retired", { n: v.versionNo }));
        retiring.value = null;
      },
    },
  );
}
</script>

<template>
  <section class="panel" aria-labelledby="wf-versions-title">
    <div class="panel-header">
      <h2 id="wf-versions-title">{{ t("wfAdmin.tab.versions") }}</h2>
      <span v-if="versions.data.value" class="meta">{{ t("wfAdmin.versions.count", { n: rows.length }) }}</span>
      <span v-if="versions.isFetching.value && !versions.isLoading.value" class="spinner" :aria-label="t('common.refreshing')" />
    </div>
    <LoadingState v-if="versions.isLoading.value" :label="t('wfAdmin.versions.loading')" />
    <div v-else-if="versions.isError.value" class="panel-body">
      <ErrorAlert :error="versions.error.value" :on-retry="() => versions.refetch()" />
    </div>
    <EmptyState v-else-if="rows.length === 0" icon="network" :title="t('wfAdmin.versions.empty.title')">{{ t("wfAdmin.versions.empty.body") }}</EmptyState>
    <div v-else class="table-wrap">
      <table class="data">
        <caption class="sr-only">{{ t("wfAdmin.tab.versions") }}</caption>
        <thead>
          <tr>
            <th scope="col" class="num">{{ t("wfAdmin.versions.col.version") }}</th>
            <th scope="col">{{ t("wfAdmin.col.status") }}</th>
            <th scope="col" class="num">{{ t("wfAdmin.versions.col.states") }}</th>
            <th scope="col" class="num">{{ t("wfAdmin.versions.col.transitions") }}</th>
            <th scope="col" class="num">{{ t("wfAdmin.versions.col.running") }}</th>
            <th scope="col">{{ t("wfAdmin.versions.col.published") }}</th>
            <th scope="col">{{ t("wfAdmin.versions.col.note") }}</th>
            <th scope="col"><span class="sr-only">{{ t("wfAdmin.versions.col.actions") }}</span></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="v in rows" :key="v.versionNo" :data-testid="`wf-version-${v.versionNo}`" :class="{ disabled: v.status === 'retired', selected: viewing === v.versionNo }">
            <td class="num mono">{{ v.versionNo }}</td>
            <td>
              <span v-if="v.status === 'draft'" class="badge info"><span class="status-dot" aria-hidden="true" />{{ t("wfAdmin.versions.status.draft") }}</span>
              <span v-else-if="v.status === 'retired'" class="badge off"><span class="status-dot" aria-hidden="true" />{{ t("wfAdmin.versions.status.retired") }}</span>
              <span v-else class="badge"><span class="status-dot" aria-hidden="true" />{{ t("wfAdmin.versions.status.published") }}</span>
              <span v-if="v.isCurrent" class="badge ok spaced">{{ t("wfAdmin.versions.current") }}</span>
            </td>
            <td class="num mono">{{ formatNumber(v.stateCount) }}</td>
            <td class="num mono">{{ formatNumber(v.transitionCount) }}</td>
            <td class="num mono" :title="v.activeInstanceCount === null ? t('wfAdmin.versions.withheld') : undefined">
              {{ v.activeInstanceCount === null ? "–" : formatNumber(v.activeInstanceCount) }}
            </td>
            <td>
              <template v-if="v.publishedAt">{{ t("wfAdmin.facts.by", { when: formatDateTime(v.publishedAt), name: v.publishedByName ?? "" }) }}</template>
              <span v-else class="muted">{{ t("wfAdmin.versions.notPublished") }}</span>
            </td>
            <td class="cell-clip" :title="v.changeNote ?? undefined" dir="auto">{{ v.changeNote ?? "" }}</td>
            <td class="row-actions">
              <button type="button" class="btn btn-sm" :aria-expanded="v.status === 'draft' ? undefined : viewing === v.versionNo" @click="view(v)">
                {{ v.status === "draft" ? t("common.edit") : viewing === v.versionNo ? t("wfAdmin.versions.hide") : t("wfAdmin.versions.view") }}
              </button>
              <button v-if="canMigrate(v)" type="button" class="btn btn-sm" :aria-expanded="migratingNo === v.versionNo" @click="openMigrate(v)">
                {{ t("wfAdmin.migrate.open") }}
              </button>
              <button v-if="v.status === 'published'" type="button" class="btn btn-sm btn-quiet-danger" @click="openRetire(v)">{{ t("wfAdmin.versions.retire") }}</button>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>

  <WorkflowMigrate v-if="migrating" :key="migrating.versionNo" :workflow="workflow" :from="migrating" :versions="rows" @close="migratingNo = null" />

  <section v-if="viewing" class="panel" aria-labelledby="wf-version-view-title">
    <div class="panel-header">
      <h2 id="wf-version-view-title">{{ t("wfAdmin.versionN", { n: viewing }) }}</h2>
      <span class="badge off">{{ t("wfAdmin.versions.readOnly") }}</span>
    </div>
    <LoadingState v-if="viewed.isLoading.value" :label="t('wfAdmin.versions.loadingOne')" />
    <div v-else-if="viewed.isError.value" class="panel-body"><ErrorAlert :error="viewed.error.value" :on-retry="() => viewed.refetch()" /></div>
    <template v-else-if="viewedDraft && viewed.data.value">
      <WorkflowGraph :draft="viewedDraft" readonly />
      <div class="table-wrap">
        <table class="data">
          <thead>
            <tr>
              <th scope="col">{{ t("wfAdmin.grants.transition") }}</th>
              <th scope="col">{{ t("wfAdmin.versions.col.fromTo") }}</th>
              <th scope="col">{{ t("wfAdmin.versions.col.comment") }}</th>
              <th scope="col">{{ t("wfAdmin.versions.col.fields") }}</th>
              <th scope="col">{{ t("wfAdmin.versions.col.conditions") }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="tr in viewedDraft.transitions" :key="tr.key">
              <td><span dir="auto">{{ tr.name }}</span> <span class="mono muted">{{ tr.key }}</span></td>
              <td dir="auto">{{ stateName(tr.from) }} → {{ stateName(tr.to) }}</td>
              <td>{{ tr.requiresComment ? t("common.required") : "" }}</td>
              <td>{{ tr.fields.map((f) => (f.required ? f.attribute : t("wfAdmin.versions.optional", { field: f.attribute }))).join(", ") }}</td>
              <td>{{ describeConditions(tr.conditions) }}</td>
            </tr>
          </tbody>
        </table>
      </div>
      <p class="panel-body muted no-margin">
        {{ t("wfAdmin.versions.states", { list: stateList }) }}
        {{ t("wfAdmin.versions.initial", { name: viewedDraft.initialState ? stateName(viewedDraft.initialState) : t("wfAdmin.none") }) }}
      </p>
    </template>
  </section>

  <ConfirmDialog
    :open="!!retiring"
    :title="t('wfAdmin.versions.retireTitle', { n: retiring?.versionNo ?? '' })"
    :confirm-label="t('wfAdmin.versions.retireConfirm')"
    :busy="retire.isPending.value"
    @cancel="retiring = null"
    @confirm="confirmRetire"
  >
    <ErrorAlert v-if="retire.isError.value" :error="retire.error.value" :title="t('wfAdmin.versions.retireFailed')" />
    <template v-if="retiring">
      <p>{{ t("wfAdmin.versions.retireBody", { n: retiring.versionNo }) }}</p>
      <p>
        {{
          retiring.activeInstanceCount === null
            ? t("wfAdmin.versions.retireRunningUnknown")
            : t("wfAdmin.versions.retireRunning", { n: retiring.activeInstanceCount })
        }}
      </p>
      <p v-if="retiring.isCurrent">
        <strong v-if="nextCurrent">{{ t("wfAdmin.versions.retireNext", { n: nextCurrent }) }}</strong>
        <strong v-else>{{ t("wfAdmin.versions.retireLast") }}</strong>
      </p>
    </template>
  </ConfirmDialog>
</template>
