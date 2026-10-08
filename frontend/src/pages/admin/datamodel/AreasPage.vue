<script setup lang="ts">
import { adminCrumbs } from "../sections";
import { computed, ref } from "vue";
import { RouterLink } from "vue-router";
import { useAreas, usePatch, useRemove, useReorder, type Area } from "../../../api/datamodel";
import { usePurge } from "../../../api/schemaChanges";
import { useCiClasses } from "../../../api/queries";
import Breadcrumbs from "../../../components/Breadcrumbs.vue";
import ClassBadge from "../../../components/ClassBadge.vue";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import RowMenu, { type RowMenuItem } from "../../../components/RowMenu.vue";
import SchemaChangeDialog from "../../../components/SchemaChangeDialog.vue";
import { formatNumber, t } from "../../../i18n";
import { useDocumentTitle } from "../../../lib/composables";
import { useListQuery } from "../../../lib/listQuery";
import { moveItem, useDragReorder } from "../../../lib/reorder";
import { useSchemaChangeFlow } from "../../../lib/schemaChange";
import { useFlashStore } from "../../../stores/flash";
import AreaDialog from "./AreaDialog.vue";
import SchemaChangesPanel from "./SchemaChangesPanel.vue";
import Icon from "../../../components/Icon.vue";

/**
 * Administration › Data model › Areas. An area is a menu tab and a PostgreSQL
 * schema: "Bestand" holds the tables of its types (bestand.netzwerk,
 * bestand.virtuelle_maschinen). Areas are ordered like the tabs (drag or
 * arrows); deleting one archives it (schema and data kept, hidden from the
 * menu), and only a purge, typed to confirm, drops the schema.
 */
useDocumentTitle(t("dm.areas.title"));
const lq = useListQuery({ sort: "sortOrder" });
const showArchived = computed(() => lq.get("archived") === "show");
const areas = useAreas();
const classes = useCiClasses();
const reorder = useReorder("areas");
const patch = usePatch<Area>("areas");
const remove = useRemove("areas");
const purge = usePurge("areas");
const flow = useSchemaChangeFlow();
const flash = useFlashStore();
const failure = ref<unknown>(null);
const pendingOrder = ref<string[] | null>(null);

/** The intro with its two example names set as code, in the translator's word order. */
const INTRO_CODE: Record<string, string> = { "\u0000": "bestand.netzwerk", "\u0001": "bestand.v_netzwerk" };
const intro = computed(() =>
  t("dm.areas.intro", { table: "\u0000", view: "\u0001" })
    .split(/([\u0000\u0001])/)
    .map((part) => ({ text: part, code: Object.hasOwn(INTRO_CODE, part) ? INTRO_CODE[part] : null })),
);

const all = computed(() => {
  const list = areas.data.value ?? [];
  if (!pendingOrder.value) return list;
  const rank = new Map(pendingOrder.value.map((id, i) => [id, i]));
  return [...list].sort((a, b) => (rank.get(a.id) ?? 0) - (rank.get(b.id) ?? 0));
});
const rows = computed(() => all.value.filter((a) => showArchived.value || a.isActive));
const archivedCount = computed(() => all.value.filter((a) => !a.isActive).length);
const typesOf = (a: Area) => (classes.data.value ?? []).filter((c) => c.areaId === a.id);
const nextSortOrder = computed(() => Math.max(0, ...all.value.map((a) => a.sortOrder)) + 10);

function commit(dragId: string, targetId: string) {
  const list = all.value;
  const from = list.findIndex((a) => a.id === dragId);
  const to = list.findIndex((a) => a.id === targetId);
  if (from < 0 || to < 0) return;
  const moved = moveItem(list, from, to);
  pendingOrder.value = moved.map((a) => a.id);
  reorder.mutate(
    moved.map((a) => ({ id: a.id, sortOrder: a.sortOrder })),
    {
      onSuccess: (n) => {
        if (n > 0) flash.show(t("dm.areas.moved", { name: list[from].name }));
      },
      onSettled: () => (pendingOrder.value = null),
    },
  );
}
function step(a: Area, delta: -1 | 1) {
  const target = rows.value[rows.value.indexOf(a) + delta];
  if (target) commit(a.id, target.id);
}
const dnd = useDragReorder(commit, () => !reorder.isPending.value);

async function report(outcome: Awaited<ReturnType<typeof flow.run>>, message: string) {
  if (outcome.status === "applied") flash.show(message);
  if (outcome.status === "refused") failure.value = outcome.error;
}

async function archive(a: Area) {
  failure.value = null;
  report(
    await flow.run({
      title: t("dm.areas.archive.title", { name: a.name }),
      intro: t("dm.areas.archive.intro", { n: a.typeCount, key: a.key }),
      preview: { operation: "deleteArea", id: a.id },
      apply: () => remove.mutateAsync(a.id),
      applyLabel: t("dm.areas.archive.apply"),
      alwaysShow: true,
    }),
    t("dm.areas.archive.done", { name: a.name }),
  );
}

async function restore(a: Area) {
  failure.value = null;
  report(
    await flow.run({
      title: t("dm.areas.restore.title", { name: a.name }),
      preview: { operation: "updateArea", id: a.id, body: { isActive: true } },
      apply: () => patch.mutateAsync({ id: a.id, body: { isActive: true } }),
      applyLabel: t("dm.areas.restore.apply"),
    }),
    t("dm.areas.restore.done", { name: a.name }),
  );
}

async function purgeArea(a: Area) {
  failure.value = null;
  report(
    await flow.run({
      title: t("dm.areas.purge.title", { name: a.name }),
      intro: a.typeCount > 0 ? t("dm.areas.purge.introTypes", { n: a.typeCount }) : t("dm.areas.purge.intro", { key: a.key }),
      preview: { operation: "purgeArea", id: a.id, body: { confirm: a.key } },
      apply: (confirm) => purge.mutateAsync({ id: a.id, confirm }),
      applyLabel: t("dm.areas.purge.apply"),
      danger: true,
      confirmName: a.key,
    }),
    t("dm.areas.purge.done", { name: a.name, key: a.key }),
  );
}

/** Edit first, then Archive or Restore, then the destructive Purge… */
function rowMenu(a: Area): RowMenuItem[] {
  const items: RowMenuItem[] = [{ label: t("common.edit"), action: () => openEdit(a) }];
  if (a.isActive) items.push({ label: t("dm.areas.row.archive"), action: () => archive(a) });
  else {
    items.push({ label: t("dm.areas.row.restore"), action: () => restore(a) });
    items.push({ label: t("dm.areas.row.purge"), action: () => purgeArea(a), danger: true });
  }
  return items;
}

// ---------- Create / edit dialog ----------
const dialogOpen = ref(false);
const editing = ref<Area | null>(null);
function openNew() {
  editing.value = null;
  dialogOpen.value = true;
}
function openEdit(a: Area) {
  editing.value = a;
  dialogOpen.value = true;
}
</script>

<template>
  <div class="list-head">
    <Breadcrumbs :items="adminCrumbs('areas')" />
    <div class="page-header">
      <div class="title">
        <h1>{{ t("dm.areas.title") }}</h1>
        <span v-if="areas.data.value" class="count mono">{{ t("common.total", { n: formatNumber(areas.data.value.length) }) }}</span>
        <span v-if="reorder.isPending.value || (areas.isFetching.value && !areas.isLoading.value)" class="spinner" :aria-label="t('common.saving')" />
      </div>
      <div class="actions">
        <button type="button" class="btn btn-primary" @click="openNew"><Icon name="plus" />{{ t("dm.areas.create") }}</button>
      </div>
    </div>
    <p class="page-intro">
      <template v-for="(part, i) in intro" :key="i">
        <code v-if="part.code">{{ part.code }}</code>
        <template v-else>{{ part.text }}</template>
      </template>
    </p>
    <div class="toolbar">
      <label class="checkbox-row">
        <input type="checkbox" :checked="showArchived" @change="lq.update({ archived: ($event.target as HTMLInputElement).checked ? 'show' : undefined })" />
        {{ t("dm.areas.showArchived") }}<span v-if="archivedCount" class="muted">&nbsp;({{ formatNumber(archivedCount) }})</span>
      </label>
      <p class="toolbar-hint">{{ t("dm.areas.reorderHint") }}</p>
    </div>
  </div>

  <ErrorAlert v-if="failure" :error="failure" />
  <ErrorAlert v-if="reorder.isError.value" :error="reorder.error.value" :title="t('dm.areas.orderFailed')" />

  <section class="panel explorer" :aria-label="t('dm.areas.title')">
    <div v-if="areas.isError.value" class="panel-body">
      <ErrorAlert :error="areas.error.value" :on-retry="() => areas.refetch()" />
    </div>
    <LoadingState v-if="areas.isLoading.value" :label="t('dm.areas.loading')" />
    <EmptyState v-if="areas.data.value && areas.data.value.length === 0" icon="layers" :title="t('dm.areas.empty.title')">
      {{ t("dm.areas.empty.body") }}
      <template #actions>
        <button type="button" class="btn btn-primary" @click="openNew"><Icon name="plus" />{{ t("dm.areas.create") }}</button>
        <RouterLink class="btn" to="/admin/templates">{{ t("dm.areas.empty.template") }}</RouterLink>
      </template>
    </EmptyState>
    <EmptyState v-else-if="areas.data.value && rows.length === 0" icon="layers" :title="t('dm.areas.allArchived.title')">
      {{ t("dm.areas.allArchived.body") }}
      <template #actions>
        <button type="button" class="btn" @click="lq.update({ archived: 'show' })">{{ t("dm.areas.showArchived") }}</button>
      </template>
    </EmptyState>

    <div v-if="rows.length > 0" class="table-wrap">
      <table class="data list-table reorderable">
        <thead>
          <tr>
            <th scope="col" class="drag-col"><span class="sr-only">{{ t("dm.areas.dragToReorder") }}</span></th>
            <th scope="col">{{ t("dm.areas.col.area") }}</th>
            <th scope="col">{{ t("dm.areas.col.schema") }}</th>
            <th scope="col">{{ t("dm.areas.col.classes") }}</th>
            <th scope="col">{{ t("admin.col.status") }}</th>
            <th scope="col">{{ t("dm.areas.col.order") }}</th>
            <th scope="col" class="row-actions"><span class="sr-only">{{ t("inventory.actions") }}</span></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="(a, i) in rows" :key="a.id" v-bind="dnd.row(a.id)" :data-id="a.id" :class="{ disabled: !a.isActive }">
            <td class="drag-handle" aria-hidden="true" :title="t('dm.areas.dragToReorder')"><Icon name="grip-vertical" /></td>
            <td>
              <button type="button" class="btn-link list-name" dir="auto" :title="t('dm.areas.editName', { name: a.name })" @click="openEdit(a)">
                <ClassBadge :icon="a.icon" :color="a.color" :name="a.name" />
              </button>
              <div v-if="a.description" class="muted cell-note">{{ a.description }}</div>
            </td>
            <td class="mono">{{ a.key }}</td>
            <td class="wrap">
              <span v-if="typesOf(a).length === 0" class="muted">{{ t("dm.areas.noClasses") }}</span>
              <template v-for="(c, j) in typesOf(a)" :key="c.id">
                <RouterLink class="list-name" :to="`/admin/classes/${c.id}`" dir="auto" :class="{ muted: !c.isActive }">{{ c.name }}</RouterLink><template v-if="j < typesOf(a).length - 1">, </template>
              </template>
              <div>
                <RouterLink class="cell-note area-new-class" :to="{ path: '/admin/classes/new', query: { areaId: a.id } }"><Icon name="plus" :size="14" />{{ t("dm.areas.classIn", { name: a.name }) }}</RouterLink>
              </div>
            </td>
            <td>
              <span v-if="a.isActive" class="badge ok"><span class="status-dot" aria-hidden="true" />{{ t("common.active") }}</span>
              <span v-else class="badge off"><span class="status-dot" aria-hidden="true" />{{ t("dm.areas.archived") }}</span>
            </td>
            <td class="order-buttons">
              <button type="button" class="btn btn-sm btn-icon" :disabled="reorder.isPending.value || i === 0" :aria-label="t('dm.areas.moveUp', { name: a.name })" @click="step(a, -1)"><Icon name="arrow-up" /></button>
              <button type="button" class="btn btn-sm btn-icon" :disabled="reorder.isPending.value || i === rows.length - 1" :aria-label="t('dm.areas.moveDown', { name: a.name })" @click="step(a, 1)"><Icon name="arrow-down" /></button>
            </td>
            <td class="row-actions">
              <RowMenu :label="t('inventory.rowMenu', { name: a.name })" :items="rowMenu(a)" />
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>

  <SchemaChangesPanel />

  <AreaDialog :open="dialogOpen" :area="editing" :next-sort-order="nextSortOrder" @close="dialogOpen = false" @saved="(m) => flash.show(m)" />
  <SchemaChangeDialog :flow="flow" />
</template>
