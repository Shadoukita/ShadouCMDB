<script setup lang="ts">
import { computed, ref } from "vue";
import { useCreateLookup, usePatch, useReorder } from "../../../api/datamodel";
import DeleteRowButton from "../../../components/DeleteRowButton.vue";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import RecordDialog, { type FieldSpec } from "../../../components/RecordDialog.vue";
import { moveItem, useDragReorder } from "../../../lib/reorder";

/**
 * A short, ordered lookup list (statuses, environments, the values of an
 * admin-defined list): drag or arrow to reorder (the order of pickers), add,
 * edit, archive, restore and delete with a usage check.
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
  resource: "statuses" | "environments" | "lookup-list-values";
  /** "status", "environment", "value" */
  noun: string;
  title: string;
  rows: Row[] | undefined;
  loading: boolean;
  error: unknown;
  refetch: () => void;
  fields: FieldSpec[];
  /** Extra columns after Name and Key. */
  columns?: { key: string; label: string }[];
  /** Merged into every create body (e.g. the list id of a value). */
  createExtra?: Record<string, unknown>;
  emptyHint: string;
}>();
const slots = defineSlots<{ cell(props: { row: Row; column: string }): unknown; empty(): unknown }>();

const create = useCreateLookup(props.resource);
const patch = usePatch(props.resource);
const reorder = useReorder(props.resource);
const notice = ref<string | null>(null);
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
  notice.value = null;
  reorder.mutate(
    moved.map((r) => ({ id: r.id, sortOrder: r.sortOrder })),
    {
      onSuccess: () => (notice.value = `Moved ${dragged?.name ?? props.noun}.`),
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
  notice.value = null;
  patch.mutate(
    { id: r.id, body: { isActive } },
    {
      onSuccess: () =>
        (notice.value = isActive
          ? `Restored ${r.name}: it can be chosen again.`
          : `Archived ${r.name}: records that have it keep it, but it can no longer be chosen.`),
    },
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
    return `Added ${props.noun} ${created.name}.`;
  }
  const saved = (await patch.mutateAsync({ id: editing.value!.id, body })) as { name: string };
  return `Saved ${props.noun} ${saved.name}.`;
}
</script>

<template>
  <section class="panel" :aria-label="title">
    <div class="panel-header">
      <h2>{{ title }}</h2>
      <span v-if="rows" class="muted">{{ rows.length }}</span>
      <span v-if="reorder.isPending.value || patch.isPending.value" class="spinner" aria-label="Saving" />
      <span class="muted">Drag a row, or use the arrows, to set the order of pickers.</span>
      <button type="button" class="btn btn-primary btn-sm" style="margin-left: auto" @click="open(null)">+ Add {{ noun }}</button>
    </div>
    <div v-if="notice || reorder.isError.value || patch.isError.value" class="panel-body">
      <div v-if="notice" class="alert" role="status">{{ notice }}</div>
      <ErrorAlert v-if="reorder.isError.value" :error="reorder.error.value" title="The new order was not saved completely" />
      <ErrorAlert v-if="patch.isError.value" :error="patch.error.value" title="Not saved" />
    </div>
    <div v-if="error" class="panel-body"><ErrorAlert :error="error" :on-retry="refetch" /></div>
    <LoadingState v-else-if="loading" />
    <EmptyState v-else-if="list.length === 0" :title="`No ${noun === 'status' ? 'statuses' : `${noun}s`} yet`">
      {{ emptyHint }}
      <template #actions>
        <button type="button" class="btn btn-primary" @click="open(null)">+ Add {{ noun }}</button>
        <slot name="empty" />
      </template>
    </EmptyState>
    <div v-else class="table-wrap">
      <table class="data reorderable">
        <thead>
          <tr>
            <th scope="col" class="drag-col"><span class="sr-only">Drag to reorder</span></th>
            <th scope="col">Name</th>
            <th scope="col">Key</th>
            <th v-for="c in columns ?? []" :key="c.key" scope="col">{{ c.label }}</th>
            <th scope="col">Description</th>
            <th scope="col">Status</th>
            <th scope="col">Order</th>
            <th scope="col"><span class="sr-only">Actions</span></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="r in list" :key="r.id" v-bind="dnd.row(r.id)" :class="{ disabled: !r.isActive }">
            <td class="drag-handle" aria-hidden="true" title="Drag to reorder">⠿</td>
            <td><button type="button" class="btn-link" @click="open(r)">{{ r.name }}</button></td>
            <td class="mono">{{ r.key }}</td>
            <td v-for="c in columns ?? []" :key="c.key">
              <slot v-if="slots.cell" name="cell" :row="r" :column="c.key" />
              <template v-else>{{ r[c.key] }}</template>
            </td>
            <td class="muted">{{ r.description ?? "" }}</td>
            <td>
              <span v-if="r.isActive" class="badge ok">Active</span>
              <span v-else class="badge off">Archived</span>
            </td>
            <td class="order-buttons">
              <button type="button" class="btn btn-sm" :disabled="reorder.isPending.value || list.indexOf(r) === 0" :aria-label="`Move ${r.name} up`" @click="step(r, -1)">↑</button>
              <button type="button" class="btn btn-sm" :disabled="reorder.isPending.value || list.indexOf(r) === list.length - 1" :aria-label="`Move ${r.name} down`" @click="step(r, 1)">↓</button>
            </td>
            <td class="row-actions">
              <button type="button" class="btn btn-sm" :aria-label="`Edit ${r.name}`" @click="open(r)">Edit</button>
              <button v-if="r.isActive" type="button" class="btn btn-sm" :disabled="patch.isPending.value" :aria-label="`Archive ${r.name}`" @click="setActive(r, false)">Archive</button>
              <button v-else type="button" class="btn btn-sm" :disabled="patch.isPending.value" :aria-label="`Restore ${r.name}`" @click="setActive(r, true)">Restore</button>
              <DeleteRowButton
                :resource="resource"
                :id="r.id"
                :label="`${noun} “${r.name}”`"
                archivable
                :archived="!r.isActive"
                small
                @archive="setActive(r, false)"
                @deleted="notice = `Deleted ${noun} ${r.name}.`"
              />
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>

  <RecordDialog
    :open="dialogOpen"
    :title="editing ? `Edit ${noun} “${editing.name}”` : `New ${noun}`"
    :submit-label="editing ? 'Save' : `Add ${noun}`"
    :fields="fields"
    :record="editing"
    :save="save"
    :id-prefix="resource"
    @close="dialogOpen = false"
    @saved="(m) => (notice = m)"
  />
</template>
