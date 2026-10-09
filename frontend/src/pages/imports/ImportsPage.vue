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
import PermissionDenied from "../../components/PermissionDenied.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import Icon from "../../components/Icon.vue";
import KeyboardHints from "../../components/KeyboardHints.vue";
import LoadingState from "../../components/LoadingState.vue";
import PaginationBar from "../../components/PaginationBar.vue";
import RowMenu, { type RowMenuItem } from "../../components/RowMenu.vue";
import SkeletonRows from "../../components/SkeletonRows.vue";
import { formatNumber, t } from "../../i18n";
import { useDocumentTitle } from "../../lib/composables";
import { formatBytes, formatDateTime, formatRelative } from "../../lib/format";
import { RUNNING } from "../../lib/imports";
import { useListQuery } from "../../lib/listQuery";
import { onRowKeydown } from "../../lib/rowKeyboard";
import { useImportAccess } from "../../lib/useImportAccess";
import { useSessionStore } from "../../stores/session";
import ImportStatusBadge from "./ImportStatusBadge.vue";

/**
 * /imports: where bulk import starts, as an explorer list (design §0, step 9c). New import, the class templates,
 * and the user's recent imports (administrators may list everyone's). While import is turned off the notice sits
 * above the list instead of replacing it, so remaining jobs can still be stopped and deleted (W3). Page and
 * "All users" live in the URL. Stop and Delete sit in the row menu and ask first.
 */
useDocumentTitle(() => t("imports.title"));
const session = useSessionStore();
const { permitted, settings, available } = useImportAccess();

const lq = useListQuery({ sort: "-createdAt", limit: 20 });
const { get, limit, offset, update } = lq;
const allUsers = computed(() => session.isAdministrator && get("all") === "true");
const query = computed<ImportListQuery>(() => ({ limit: limit.value, offset: offset.value, all: allUsers.value ? "true" : undefined }));
const list = useImportList(query, permitted);
const total = computed(() => list.data.value?.page.total ?? 0);
const rows = computed(() => list.data.value?.data ?? []);
/** The sentences after the "turned off" heading, joined here: template whitespace around them is condensed away. */
const offDetail = computed(() =>
  [settings.data.value?.locked ? "" : t("imports.off.hint"), total.value > 0 ? t("imports.off.remaining") : ""].filter(Boolean).join(" "),
);
const crumbs = computed(() => [{ label: t("inventory.crumb"), to: "/cis" }, { label: t("imports.title") }]);

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
  if (c) return [c.created, c.updated, c.unchanged, c.failed + c.skipped].map((n) => formatNumber(n)).join(" / ");
  return "";
}

// ---------- Stop and delete ----------
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

/** Open, then Stop while the job runs or Delete once it has stopped; an upload in flight has neither. */
const rowMenu = (j: ImportJobSummary): RowMenuItem[] => [
  { label: t("imports.row.open"), to: `/imports/${j.id}` },
  ...(RUNNING.has(j.status)
    ? [{ label: t("imports.stop"), action: () => openCancel(j), danger: true }]
    : j.status !== "uploading"
      ? [{ label: t("imports.delete"), action: () => openDelete(j), danger: true }]
      : []),
];
</script>

<template>
  <PermissionDenied v-if="!permitted" :crumbs="crumbs" :permissions="['cis.import']" :panel-title="t('imports.denied.panelTitle')">
    {{ t("imports.denied.body", { permission: t("permission.cis.import") }) }}
    <template #actions><RouterLink class="btn btn-primary" to="/cis">{{ t("imports.backToInventory") }}</RouterLink></template>
  </PermissionDenied>

  <template v-else>
    <div class="list-head">
      <Breadcrumbs :items="crumbs" />
      <div class="page-header">
        <div class="title">
          <h1>{{ t("imports.title") }}</h1>
          <span v-if="list.data.value" class="count mono">{{ t("common.total", { n: formatNumber(total) }) }}</span>
          <span v-if="list.isFetching.value && !list.isPending.value" class="spinner" :aria-label="t('common.refreshing')" />
        </div>
        <div v-if="available" class="actions">
          <RouterLink class="btn btn-primary" to="/imports/new"><Icon name="plus" />{{ t("imports.new") }}</RouterLink>
        </div>
      </div>
      <p class="page-intro">{{ t("imports.intro") }}</p>
      <form v-if="session.isAdministrator" class="toolbar" @submit.prevent>
        <div class="field">
          <span class="label">{{ t("imports.filter.scope") }}</span>
          <label class="checkbox-row">
            <input type="checkbox" :checked="allUsers" @change="update({ all: ($event.target as HTMLInputElement).checked ? 'true' : undefined })" />
            {{ t("imports.filter.allUsers") }}
          </label>
        </div>
      </form>
    </div>

    <LoadingState v-if="settings.isPending.value" :label="t('imports.settingsLoading')" />
    <ErrorAlert v-else-if="settings.isError.value" :error="settings.error.value" :on-retry="() => settings.refetch()" />
    <div v-else-if="!available" class="alert alert-warn" role="status">
      <strong>{{ t(settings.data.value?.locked ? "imports.off.locked" : "imports.off.title") }}</strong>
      {{ offDetail }}
      <p v-if="!settings.data.value?.locked && session.isAdministrator" class="meta">
        <RouterLink to="/admin/import">{{ t("imports.off.settingsLink") }}</RouterLink>
      </p>
    </div>

    <section v-if="available" class="panel import-intro" aria-labelledby="import-how-title">
      <div class="panel-header"><h2 id="import-how-title">{{ t("imports.how.title") }}</h2></div>
      <div class="panel-body">
        <ol class="import-how">
          <li>
            <strong>{{ t("imports.how.upload") }}</strong>
            {{ t("imports.how.uploadBody", { size: formatBytes(settings.data.value!.limits.maxFileBytes), rows: formatNumber(settings.data.value!.limits.maxRows) }) }}
          </li>
          <li><strong>{{ t("imports.how.map") }}</strong> {{ t("imports.how.mapBody") }}</li>
          <li><strong>{{ t("imports.how.check") }}</strong> {{ t("imports.how.checkBody") }}</li>
          <li><strong>{{ t("imports.how.import") }}</strong> {{ t("imports.how.importBody") }}</li>
        </ol>
        <form class="inline-control" @submit.prevent="downloadTemplate">
          <label for="template-class">{{ t("imports.template.label") }}</label>
          <select id="template-class" v-model="templateClass" :disabled="templateClasses.length === 0">
            <option value="">{{ templateClasses.length === 0 ? t("imports.template.none") : t("imports.template.choose") }}</option>
            <option v-for="c in templateClasses" :key="c.id" :value="c.key" dir="auto">{{ c.name }}</option>
          </select>
          <button type="submit" class="btn" :disabled="!templateClass || templateBusy">
            <Icon name="arrow-down-to-line" />{{ templateBusy ? t("imports.template.preparing") : t("imports.template.download") }}
          </button>
        </form>
        <ErrorAlert v-if="templateError" :error="templateError" :title="t('imports.template.failed')" />
      </div>
    </section>

    <section class="panel explorer" :aria-label="t('imports.recent')">
      <div v-if="list.isError.value" class="panel-body">
        <ErrorAlert :error="list.error.value" :on-retry="() => list.refetch()" />
      </div>
      <SkeletonRows v-else-if="list.isPending.value" :label="t('imports.loading')" />
      <EmptyState v-else-if="total === 0" icon="upload" :title="t('imports.empty.title')">
        {{ available ? t("imports.empty.body") : t("imports.empty.off") }}
        <template v-if="available" #actions>
          <RouterLink class="btn btn-primary" to="/imports/new"><Icon name="plus" />{{ t("imports.new") }}</RouterLink>
        </template>
      </EmptyState>
      <EmptyState v-else-if="rows.length === 0" :title="t('common.pastEnd')">
        <template #actions><button type="button" class="btn" @click="update({})">{{ t("common.firstPage") }}</button></template>
      </EmptyState>

      <template v-if="rows.length > 0 && !list.isError.value">
        <div class="table-wrap table-scroll">
          <table :class="['data', 'list-table', 'imports-table', { loading: list.isPlaceholderData.value }]" aria-describedby="imports-keys">
            <caption class="sr-only">{{ t("imports.caption") }}</caption>
            <thead>
              <tr>
                <th scope="col">{{ t("imports.col.file") }}</th>
                <th scope="col">{{ t("imports.col.class") }}</th>
                <th scope="col">{{ t("imports.col.status") }}</th>
                <th scope="col" class="num" :title="t('imports.col.rowsTitle')">
                  {{ t("imports.col.rows") }} <span class="muted rows-key">{{ t("imports.col.rowsKey") }}</span>
                </th>
                <th v-if="allUsers" scope="col">{{ t("imports.col.startedBy") }}</th>
                <th scope="col">{{ t("imports.col.started") }}</th>
                <th scope="col" class="row-actions"><span class="sr-only">{{ t("inventory.actions") }}</span></th>
              </tr>
            </thead>
            <tbody @keydown="onRowKeydown($event)">
              <tr v-for="j in rows" :key="j.id" :data-id="j.id">
                <td :title="`${j.fileName} (${formatBytes(j.fileSize)})`">
                  <span class="cell-clip"><RouterLink :to="`/imports/${j.id}`" class="list-name" dir="auto">{{ j.fileName }}</RouterLink></span>
                </td>
                <td dir="auto">{{ className(j.classKey) }}</td>
                <td><ImportStatusBadge :status="j.status" /></td>
                <td class="num mono">
                  <template v-if="counts(j)">{{ counts(j) }}</template>
                  <span v-else-if="j.rowCount != null" class="muted">{{ t("imports.inFile", { n: formatNumber(j.rowCount) }) }}</span>
                </td>
                <td v-if="allUsers" dir="auto">{{ j.createdBy.name }}</td>
                <td><time :datetime="j.createdAt" :title="formatDateTime(j.createdAt)">{{ formatRelative(j.createdAt) }}</time></td>
                <td class="row-actions">
                  <RowMenu :label="t('inventory.rowMenu', { name: j.fileName })" :items="rowMenu(j)" />
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <div class="table-footer">
          <PaginationBar numbered :total="total" :limit="limit" :offset="offset" @change="lq.onPage" />
        </div>
        <KeyboardHints id="imports-keys" />
      </template>
    </section>
  </template>

  <ConfirmDialog
    :open="!!cancelling"
    :title="t('imports.stopDialog.title', { file: cancelling?.fileName ?? '' })"
    :confirm-label="t('imports.stop')"
    :busy="cancel.isPending.value"
    @cancel="cancelling = null"
    @confirm="confirmCancel"
  >
    <ErrorAlert v-if="cancel.isError.value" :error="cancel.error.value" :title="t('imports.stopDialog.failed')" />
    <p>{{ cancelling?.phase === "commit" ? t("imports.stopDialog.commit") : t("imports.stopDialog.nothingYet") }}</p>
  </ConfirmDialog>

  <ConfirmDialog
    :open="!!deleting"
    :title="t('imports.deleteDialog.title', { file: deleting?.fileName ?? '' })"
    :confirm-label="t('imports.delete')"
    :busy="del.isPending.value"
    @cancel="deleting = null"
    @confirm="confirmDelete"
  >
    <ErrorAlert v-if="del.isError.value" :error="del.error.value" :title="t('imports.deleteDialog.failed')" />
    <p>{{ t("imports.deleteDialog.body") }}</p>
  </ConfirmDialog>
</template>
