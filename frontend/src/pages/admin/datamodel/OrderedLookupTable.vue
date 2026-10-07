<script setup lang="ts">
import { computed, nextTick, ref } from "vue";
import { useCreateLookup, usePatch, useReorder } from "../../../api/datamodel";
import DeleteRowButton from "../../../components/DeleteRowButton.vue";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import RecordDialog, { type FieldSpec } from "../../../components/RecordDialog.vue";
import RowMenu, { type RowMenuItem } from "../../../components/RowMenu.vue";
import { formatNumber, t } from "../../../i18n";
import { moveItem, useDragReorder } from "../../../lib/reorder";
import { useFlashStore } from "../../../stores/flash";
import Icon from "../../../components/Icon.vue";

/**
 * The values of a lookup list, short and ordered: drag or arrow to reorder
 * (the order of pickers), add, edit, archive, restore and delete with a usage
 * check. Edit, Archive/Restore and Delete sit in each row's RowMenu; confirmations are toasts.
 */
export interface Row {
  id: string;
  key: string;
  name: string;
  description: string | null;
  sortOrder: number;
  isActive: boolean;
  [field: string]: unknown;
}

const props = defineProps<{
  resource: "lookup-list-values";
  title: string;
  rows: Row[] | undefined;
  loading: boolean;
  error: unknown;
  refetch: () => void;
  /** The add/edit dialog. */
  fields: FieldSpec[];
  /** Extra columns after Name and Key. */
  columns?: { key: string; label: string }[];
  /** Merged into every create body (e.g. the list id of a value). */
  createExtra?: Record<string, unknown>;
  /** Initial values of the "add" dialog (e.g. the parent value the table is filtered by). */
  createDefaults?: Record<string, string>;
  /** The rows are a filtered part of a longer list: reordering keeps the sort orders they had among the rest. */
  partial?: boolean;
  emptyHint: string;
}>();
const slots = defineSlots<{ cell(props: { row: Row; column: string }): unknown; empty(): unknown; toolbar(): unknown }>();

const flash = useFlashStore();
const create = useCreateLookup(props.resource);
const patch = usePatch(props.resource);
const reorder = useReorder(props.resource);
const pendingOrder = ref<string[] | null>(null);

const list = computed(() => {
  const rows = props.rows ?? [];
  if (!pendingOrder.value) return rows;
  const rank = new Map(pendingOrder.value.map((id, i) => [id, i]));
  return [...rows].sort((a, b) => (rank.get(a.id) ?? 0) - (rank.get(b.id) ?? 0));
});

function commit(dragId: string, targetId: string) {
  const dragged = list.value.find((r) => r.id === dragId);
  const moved = moveItem(list.value, list.value.findIndex((r) => r.id === dragId), list.value.findIndex((r) => r.id === targetId));
  pendingOrder.value = moved.map((r) => r.id);
  // A filtered part hands its own sort orders out again, if they are distinct; otherwise 10, 20, 30…
  const own = list.value.map((r) => r.sortOrder).sort((a, b) => a - b);
  const keep = props.partial && own.every((n, i) => i === 0 || n > own[i - 1]);
  reorder.mutate(
    moved.map((r, i) => ({ id: r.id, sortOrder: r.sortOrder, ...(keep ? { next: own[i] } : {}) })),
    {
      onSuccess: () => flash.show(dragged ? t("dm.lookup.moved", { name: dragged.name }) : t("dm.lookup.movedValue")),
      onSettled: () => (pendingOrder.value = null),
    },
  );
}
function step(r: Row, delta: -1 | 1) {
  const target = list.value[list.value.indexOf(r) + delta];
  if (target) commit(r.id, target.id);
}
const dnd = useDragReorder(commit, () => !reorder.isPending.value);

function setActive(r: Row, isActive: boolean) {
  patch.mutate(
    { id: r.id, body: { isActive } },
    { onSuccess: () => flash.show(t(isActive ? "dm.lookup.restored" : "dm.lookup.archived", { name: r.name })) },
  );
}

const dialogOpen = ref(false);
const editing = ref<Row | null>(null);
function open(r: Row | null) {
  editing.value = r;
  dialogOpen.value = true;
}
async function save(body: Record<string, unknown>, isNew: boolean): Promise<string> {
  if (isNew) {
    const last = Math.max(0, ...list.value.map((r) => r.sortOrder));
    const created = (await create.mutateAsync({ ...body, ...props.createExtra, sortOrder: last + 10 })) as { name: string };
    return t("dm.lookup.added", { name: created.name });
  }
  const saved = (await patch.mutateAsync({ id: editing.value!.id, body })) as { name: string };
  return t("dm.lookup.saved", { name: saved.name });
}

// One delete dialog for the table, opened from a row's menu.
const deleting = ref<Row | null>(null);
const deleteDialog = ref<InstanceType<typeof DeleteRowButton>>();
async function askDelete(r: Row) {
  deleting.value = r;
  await nextTick();
  deleteDialog.value?.open();
}
const rowMenu = (r: Row): RowMenuItem[] => [
  { label: t("common.edit"), action: () => open(r) },
  r.isActive ? { label: t("dm.lookup.archive"), action: () => setActive(r, false) } : { label: t("dm.lookup.restore"), action: () => setActive(r, true) },
  { label: t("common.delete"), action: () => void askDelete(r), danger: true },
];
</script>

<template>
  <section class="panel lookup-values" :aria-label="title">
    <div class="panel-header">
      <div class="lookup-values-title">
        <h2>{{ title }}</h2>
        <span v-if="rows" class="meta">{{ formatNumber(rows.length) }}</span>
        <span v-if="reorder.isPending.value || patch.isPending.value" class="spinner" :aria-label="t('common.saving')" />
      </div>
      <button type="button" class="btn btn-primary btn-sm" @click="open(null)"><Icon name="plus" />{{ t("dm.lookup.add") }}</button>
    </div>
    <div v-if="reorder.isError.value || patch.isError.value" class="panel-body">
      <ErrorAlert v-if="reorder.isError.value" :error="reorder.error.value" :title="t('dm.lookup.reorderFailed')" />
      <ErrorAlert v-if="patch.isError.value" :error="patch.error.value" :title="t('formError.notSaved')" />
    </div>
    <div class="toolbar">
      <slot v-if="slots.toolbar" name="toolbar" />
      <p class="toolbar-hint">{{ t("dm.lookup.reorderHint") }}</p>
    </div>
    <div v-if="error" class="panel-body"><ErrorAlert :error="error" :on-retry="refetch" /></div>
    <LoadingState v-else-if="loading" :label="t('dm.lookup.loading')" />
    <EmptyState v-else-if="list.length === 0" icon="list" :title="t('dm.lookup.empty.title')">
      {{ emptyHint }}
      <template #actions>
        <button type="button" class="btn btn-primary" @click="open(null)"><Icon name="plus" />{{ t("dm.lookup.add") }}</button>
        <slot name="empty" />
      </template>
    </EmptyState>
    <div v-else class="table-wrap">
      <table class="data reorderable">
        <thead>
          <tr>
            <th scope="col" class="drag-col"><span class="sr-only">{{ t("dm.lookup.col.drag") }}</span></th>
            <th scope="col">{{ t("dm.lookup.col.name") }}</th>
            <th scope="col">{{ t("dm.lookup.col.key") }}</th>
            <th v-for="c in columns ?? []" :key="c.key" scope="col">{{ c.label }}</th>
            <th scope="col">{{ t("dm.lookup.col.description") }}</th>
            <th scope="col">{{ t("admin.col.status") }}</th>
            <th scope="col">{{ t("dm.lookup.col.order") }}</th>
            <th scope="col" class="row-actions"><span class="sr-only">{{ t("inventory.actions") }}</span></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="r in list" :key="r.id" v-bind="dnd.row(r.id)" :class="{ disabled: !r.isActive }">
            <td class="drag-handle" aria-hidden="true" :title="t('dm.lookup.col.drag')"><Icon name="grip-vertical" /></td>
            <td><button type="button" class="btn-link" @click="open(r)">{{ r.name }}</button></td>
            <td class="mono">{{ r.key }}</td>
            <td v-for="c in columns ?? []" :key="c.key">
              <slot v-if="slots.cell" name="cell" :row="r" :column="c.key" />
              <template v-else>{{ r[c.key] }}</template>
            </td>
            <td class="muted fill" :title="r.description ?? undefined">{{ r.description ?? "" }}</td>
            <td>
              <span v-if="r.isActive" class="badge ok">{{ t("common.active") }}</span>
              <span v-else class="badge off">{{ t("dm.lookup.archivedBadge") }}</span>
            </td>
            <td class="order-buttons">
              <button type="button" class="btn btn-sm btn-icon" :disabled="reorder.isPending.value || list.indexOf(r) === 0" :aria-label="t('dm.lookup.moveUp', { name: r.name })" @click="step(r, -1)"><Icon name="arrow-up" /></button>
              <button type="button" class="btn btn-sm btn-icon" :disabled="reorder.isPending.value || list.indexOf(r) === list.length - 1" :aria-label="t('dm.lookup.moveDown', { name: r.name })" @click="step(r, 1)"><Icon name="arrow-down" /></button>
            </td>
            <td class="row-actions">
              <RowMenu :label="t('inventory.rowMenu', { name: r.name })" :items="rowMenu(r)" />
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>

  <DeleteRowButton
    ref="deleteDialog"
    headless
    :resource="resource"
    :id="deleting?.id ?? ''"
    :label="t('dm.lookup.deleteLabel', { name: deleting?.name ?? '' })"
    archivable
    :archived="!deleting?.isActive"
    @archive="deleting && setActive(deleting, false)"
    @deleted="flash.show(t('dm.lookup.deleted', { name: deleting?.name ?? '' }))"
  />

  <RecordDialog
    :open="dialogOpen"
    :title="editing ? t('dm.lookup.dialog.editTitle', { name: editing.name }) : t('dm.lookup.dialog.newTitle')"
    :submit-label="editing ? t('record.save.save') : t('dm.lookup.add')"
    :fields="fields"
    :record="editing"
    :defaults="createDefaults"
    :save="save"
    :id-prefix="resource"
    @close="dialogOpen = false"
    @saved="(m) => flash.show(m)"
  />
</template>
