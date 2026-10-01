<script setup lang="ts">
import { computed, ref } from "vue";
import { RouterLink } from "vue-router";
import {
  importDownloads,
  useCancelImport,
  useDeleteImport,
  useImportList,
  type ImportJobSummary,
  type ImportListQuery,
} from "../../api/imports";
import { useCiClasses } from "../../api/queries";
import Breadcrumbs from "../../components/Breadcrumbs.vue";
import ConfirmDialog from "../../components/ConfirmDialog.vue";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import PaginationBar from "../../components/PaginationBar.vue";
import { useDocumentTitle } from "../../lib/composables";
import { formatBytes, formatDateTime, formatRelative } from "../../lib/format";
import { RUNNING } from "../../lib/imports";
import { useListQuery } from "../../lib/listQuery";
import { useImportAccess } from "../../lib/useImportAccess";
import { useSessionStore } from "../../stores/session";
import ImportStatusBadge from "./ImportStatusBadge.vue";

/**
 * /imports: where bulk import starts. New import, the class templates, and the user's recent imports
 * (administrators may list everyone's). While import is turned off the notice sits above the list instead of
 * replacing it, so remaining jobs can still be cancelled and deleted (W3). Page and "All users" live in the URL.
 */
useDocumentTitle("Imports");
const session = useSessionStore();
const { permitted, settings, available } = useImportAccess();

const lq = useListQuery({ sort: "-createdAt", limit: 20 });
const { get, limit, offset, update } = lq;
const allUsers = computed(() => session.isAdministrator && get("all") === "true");
const query = computed<ImportListQuery>(() => ({ limit: limit.value, offset: offset.value, all: allUsers.value ? "true" : undefined }));
const list = useImportList(query, permitted);
const total = computed(() => list.data.value?.page.total ?? 0);
const rows = computed(() => list.data.value?.data ?? []);

const classes = useCiClasses();
const className = (key: string | null | undefined) => (key ? (classes.data.value?.find((c) => c.key === key)?.name ?? key) : "");
/** Classes a template can be downloaded for: concrete, active, with create or edit rights (the server checks again). */
const templateClasses = computed(() =>
  (classes.data.value ?? []).filter(
    (c) => !c.isAbstract && c.isActive && (session.canOnClass(c.id, "create") || session.canOnClass(c.id, "edit")),
  ),
);
const templateClass = ref("");
const templateBusy = ref(false);
const templateError = ref<unknown>(null);
async function downloadTemplate() {
  if (!templateClass.value) return;
  templateBusy.value = true;
  templateError.value = null;
  try {
    await importDownloads.template(templateClass.value);
  } catch (e) {
    templateError.value = e;
  } finally {
    templateBusy.value = false;
  }
}

function counts(j: ImportJobSummary): string {
  const c = j.summary?.committed;
  if (c) return `${c.created.toLocaleString()} / ${c.updated.toLocaleString()} / ${c.unchanged.toLocaleString()} / ${(c.failed + c.skipped).toLocaleString()}`;
  return "";
}

// ---------- Cancel and delete ----------
const cancel = useCancelImport();
const del = useDeleteImport();
const cancelling = ref<ImportJobSummary | null>(null);
const deleting = ref<ImportJobSummary | null>(null);

async function confirmCancel() {
  if (!cancelling.value) return;
  try {
    await cancel.mutateAsync(cancelling.value.id);
    cancelling.value = null;
  } catch {
    // shown in the dialog
  }
}
async function confirmDelete() {
  if (!deleting.value) return;
  try {
    await del.mutateAsync(deleting.value.id);
    deleting.value = null;
  } catch {
    // shown in the dialog
  }
}
function openCancel(j: ImportJobSummary) {
  cancel.reset();
  cancelling.value = j;
}
function openDelete(j: ImportJobSummary) {
  del.reset();
  deleting.value = j;
}
</script>

<template>
  <Breadcrumbs :items="[{ label: 'Inventory', to: '/cis' }, { label: 'Imports' }]" />

  <EmptyState v-if="!permitted" title="Permission denied">
    You need the <strong>Bulk import</strong> permission. Ask an administrator for access.
    <template #actions><RouterLink class="btn" to="/cis">Back to inventory</RouterLink></template>
  </EmptyState>

  <template v-else>
    <div class="page-header">
      <div class="title">
        <h1>Imports</h1>
        <span v-if="list.isFetching.value && !list.isLoading.value" class="spinner" aria-label="Refreshing" />
      </div>
      <div v-if="available" class="actions">
        <RouterLink class="btn btn-primary" to="/imports/new">+ New import</RouterLink>
      </div>
    </div>

    <LoadingState v-if="settings.isPending.value" label="Loading import settings…" />
    <ErrorAlert v-else-if="settings.isError.value" :error="settings.error.value" :on-retry="() => settings.refetch()" />
    <div v-else-if="!available" class="alert alert-warn" role="status">
      <template v-if="settings.data.value?.locked">
        <strong>Bulk import is disabled by the server configuration.</strong>
      </template>
      <template v-else>
        <strong>Bulk import is turned off for this instance.</strong>
        An administrator can turn it on under
        <RouterLink v-if="session.isAdministrator" to="/admin/import">Administration › Import</RouterLink>
        <template v-else>Administration › Import</template>.
      </template>
      <template v-if="total > 0"> Your remaining imports are listed below; you can still cancel and delete them.</template>
    </div>

    <section v-if="available" class="panel import-intro">
      <div class="panel-header"><h2>How import works</h2></div>
      <div class="panel-body">
        <ol class="import-how">
          <li><strong>Upload</strong> a CSV or Excel (.xlsx) file, up to {{ formatBytes(settings.data.value!.limits.maxFileBytes) }} and {{ settings.data.value!.limits.maxRows.toLocaleString() }} rows.</li>
          <li><strong>Map</strong> its columns to the attributes and relationships of one CI class.</li>
          <li><strong>Check</strong> it: a dry run lists every row that would fail, before anything is saved.</li>
          <li><strong>Import</strong> it. Existing CIs are matched and updated, new ones created; nothing is deleted.</li>
        </ol>
        <form class="inline-control" @submit.prevent="downloadTemplate">
          <label for="template-class">Template for class</label>
          <select id="template-class" v-model="templateClass" :disabled="templateClasses.length === 0">
            <option value="">{{ templateClasses.length === 0 ? "No class you can import into" : "Choose a class…" }}</option>
            <option v-for="c in templateClasses" :key="c.id" :value="c.key">{{ c.name }}</option>
          </select>
          <button type="submit" class="btn" :disabled="!templateClass || templateBusy">
            {{ templateBusy ? "Preparing…" : "Download template (CSV)" }}
          </button>
        </form>
        <ErrorAlert v-if="templateError" :error="templateError" title="The template could not be downloaded" />
      </div>
    </section>

    <section class="panel" aria-labelledby="recent-imports">
      <div class="panel-header">
        <h2 id="recent-imports">Recent imports</h2>
        <label v-if="session.isAdministrator" class="checkbox-row">
          <input type="checkbox" :checked="allUsers" @change="update({ all: ($event.target as HTMLInputElement).checked ? 'true' : undefined })" />
          All users
        </label>
      </div>
      <div v-if="list.isError.value" class="panel-body">
        <ErrorAlert :error="list.error.value" :on-retry="() => list.refetch()" />
      </div>
      <div v-if="list.isLoading.value" class="table-wrap" aria-busy="true">
        <LoadingState label="Loading imports…" />
      </div>
      <EmptyState v-else-if="list.data.value && total === 0" title="No imports yet">
        <template v-if="available">
          Import CIs from a CSV or Excel file: upload it, map its columns and check it before anything is saved.
        </template>
        <template v-else>There are no imports to show.</template>
        <template v-if="available" #actions><RouterLink class="btn btn-primary" to="/imports/new">+ New import</RouterLink></template>
      </EmptyState>
      <EmptyState v-else-if="list.data.value && rows.length === 0" title="This page is past the end of the results">
        <template #actions><button class="btn" @click="update({})">Go to first page</button></template>
      </EmptyState>

      <template v-if="rows.length > 0">
        <div class="table-wrap">
          <table :class="['data', { loading: list.isPlaceholderData.value }]">
            <caption class="sr-only">Recent imports, newest first</caption>
            <thead>
              <tr>
                <th scope="col">File</th>
                <th scope="col">Class</th>
                <th scope="col">Status</th>
                <th scope="col" class="num" title="Created / updated / unchanged / failed or skipped">Rows <span class="muted">(new / upd. / same / failed)</span></th>
                <th v-if="allUsers" scope="col">Started by</th>
                <th scope="col">Started</th>
                <th scope="col"><span class="sr-only">Actions</span></th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="j in rows" :key="j.id">
                <td class="cell-clip" :title="`${j.fileName} (${formatBytes(j.fileSize)})`">
                  <RouterLink :to="`/imports/${j.id}`">{{ j.fileName }}</RouterLink>
                </td>
                <td>{{ className(j.classKey) }}</td>
                <td><ImportStatusBadge :status="j.status" /></td>
                <td class="num">
                  <template v-if="counts(j)">{{ counts(j) }}</template>
                  <span v-else-if="j.rowCount != null" class="muted">{{ j.rowCount.toLocaleString() }} in file</span>
                </td>
                <td v-if="allUsers">{{ j.createdBy.name }}</td>
                <td :title="formatDateTime(j.createdAt)">{{ formatRelative(j.createdAt) }}</td>
                <td class="row-actions">
                  <button v-if="RUNNING.has(j.status)" type="button" class="btn btn-sm" :aria-label="`Stop import of ${j.fileName}`" @click="openCancel(j)">
                    Stop
                  </button>
                  <button
                    v-else-if="j.status !== 'uploading'"
                    type="button"
                    class="btn btn-sm btn-quiet-danger"
                    :aria-label="`Delete import of ${j.fileName}`"
                    @click="openDelete(j)"
                  >
                    Delete
                  </button>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <PaginationBar :total="total" :limit="limit" :offset="offset" @change="lq.onPage" />
      </template>
    </section>
  </template>

  <ConfirmDialog
    :open="!!cancelling"
    :title="`Stop the import of “${cancelling?.fileName ?? ''}”?`"
    confirm-label="Stop import"
    :busy="cancel.isPending.value"
    @cancel="cancelling = null"
    @confirm="confirmCancel"
  >
    <ErrorAlert v-if="cancel.isError.value" :error="cancel.error.value" title="Not stopped" />
    <p v-if="cancelling?.phase === 'commit'">
      The import stops after the current batch of at most 500 rows. Rows already imported stay imported. You can run
      the same file again later; rows already imported will show as unchanged.
    </p>
    <p v-else>Nothing has been imported yet, so no configuration item changes.</p>
  </ConfirmDialog>

  <ConfirmDialog
    :open="!!deleting"
    :title="`Delete the import of “${deleting?.fileName ?? ''}”?`"
    confirm-label="Delete import"
    :busy="del.isPending.value"
    @cancel="deleting = null"
    @confirm="confirmDelete"
  >
    <ErrorAlert v-if="del.isError.value" :error="del.error.value" title="Not deleted" />
    <p>
      The uploaded file, its mapping and its list of row problems are removed now. Configuration items already
      imported are not changed, and the audit log keeps its entries.
    </p>
  </ConfirmDialog>
</template>
