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
import SchemaChangeDialog from "../../../components/SchemaChangeDialog.vue";
import { useDocumentTitle } from "../../../lib/composables";
import { useListQuery } from "../../../lib/listQuery";
import { moveItem, useDragReorder } from "../../../lib/reorder";
import { useSchemaChangeFlow } from "../../../lib/schemaChange";
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
useDocumentTitle("Areas");
const lq = useListQuery({ sort: "sortOrder" });
const showArchived = computed(() => lq.get("archived") === "show");
const areas = useAreas();
const classes = useCiClasses();
const reorder = useReorder("areas");
const patch = usePatch<Area>("areas");
const remove = useRemove("areas");
const purge = usePurge("areas");
const flow = useSchemaChangeFlow();
const notice = ref<string | null>(null);
const failure = ref<unknown>(null);
const pendingOrder = ref<string[] | null>(null);

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
  notice.value = null;
  reorder.mutate(
    moved.map((a) => ({ id: a.id, sortOrder: a.sortOrder })),
    {
      onSuccess: (n) => (notice.value = n > 0 ? `Moved ${list[from].name}. The menu tabs use the new order.` : null),
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
  if (outcome.status === "applied") notice.value = message;
  if (outcome.status === "refused") failure.value = outcome.error;
}

async function archive(a: Area) {
  notice.value = null;
  failure.value = null;
  report(
    await flow.run({
      title: `Archive area “${a.name}”?`,
      intro: `The tab and its ${a.typeCount} ${a.typeCount === 1 ? "type" : "types"} disappear from the menu. The schema “${a.key}”, its tables and every stored value are kept; restore the area to bring it back.`,
      preview: { operation: "deleteArea", id: a.id },
      apply: () => remove.mutateAsync(a.id),
      applyLabel: "Archive area",
      alwaysShow: true,
    }),
    `Archived ${a.name}: its schema and data are kept.`,
  );
}

async function restore(a: Area) {
  notice.value = null;
  failure.value = null;
  report(
    await flow.run({
      title: `Restore area “${a.name}”`,
      preview: { operation: "updateArea", id: a.id, body: { isActive: true } },
      apply: () => patch.mutateAsync({ id: a.id, body: { isActive: true } }),
      applyLabel: "Restore area",
    }),
    `Restored ${a.name}: it is a menu tab again.`,
  );
}

async function purgeArea(a: Area) {
  notice.value = null;
  failure.value = null;
  report(
    await flow.run({
      title: `Purge area “${a.name}”?`,
      intro:
        a.typeCount > 0
          ? `The area still holds ${a.typeCount} ${a.typeCount === 1 ? "type" : "types"}. Purge those first (Administration › CI classes); the preview below shows the refusal otherwise.`
          : `Drops the PostgreSQL schema “${a.key}”. The change is recorded in the database change history.`,
      preview: { operation: "purgeArea", id: a.id, body: { confirm: a.key } },
      apply: (confirm) => purge.mutateAsync({ id: a.id, confirm }),
      applyLabel: "Purge area",
      danger: true,
      confirmName: a.key,
    }),
    `Purged ${a.name}: schema ${a.key} was dropped.`,
  );
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
  <Breadcrumbs :items="adminCrumbs('areas')" />
  <div class="page-header">
    <div class="title">
      <h1>Areas</h1>
      <span v-if="areas.data.value" class="muted">{{ areas.data.value.length }} total</span>
      <span v-if="reorder.isPending.value || (areas.isFetching.value && !areas.isLoading.value)" class="spinner" aria-label="Saving" />
    </div>
    <div class="actions">
      <button type="button" class="btn btn-primary" @click="openNew">+ New area</button>
    </div>
  </div>

  <p class="page-intro muted">
    An area is a tab of the main menu and a PostgreSQL schema. Each CI class in it gets its own table there, e.g.
    <code>bestand.netzwerk</code>, with one typed column per attribute, and a read-only reporting view
    (<code>bestand.v_netzwerk</code>).
  </p>

  <div v-if="notice" class="alert alert-success" role="status">{{ notice }}</div>
  <ErrorAlert v-if="failure" :error="failure" />
  <ErrorAlert v-if="reorder.isError.value" :error="reorder.error.value" title="The new order was not saved completely" />

  <section class="panel" aria-label="Areas">
    <div class="toolbar">
      <label class="checkbox-row">
        <input type="checkbox" :checked="showArchived" @change="lq.update({ archived: ($event.target as HTMLInputElement).checked ? 'show' : undefined })" />
        Show archived areas<span v-if="archivedCount" class="muted">&nbsp;({{ archivedCount }})</span>
      </label>
      <span class="muted" style="margin-left: auto">Drag a row, or use the arrows, to order the menu tabs.</span>
    </div>
    <div v-if="areas.isError.value" class="panel-body">
      <ErrorAlert :error="areas.error.value" :on-retry="() => areas.refetch()" />
    </div>
    <LoadingState v-if="areas.isLoading.value" label="Loading areas…" />
    <EmptyState v-if="areas.data.value && areas.data.value.length === 0" title="No areas yet">
      Create an area such as “Bestand” to get a menu tab and a database schema for its classes, or install the IT
      infrastructure starter template, which brings the area “Infrastruktur”.
      <template #actions>
        <button type="button" class="btn btn-primary" @click="openNew">+ New area</button>
        <RouterLink class="btn" to="/admin/templates">Install a starter template</RouterLink>
      </template>
    </EmptyState>
    <EmptyState v-else-if="areas.data.value && rows.length === 0" title="Every area is archived">
      <template #actions>
        <button type="button" class="btn" @click="lq.update({ archived: 'show' })">Show archived areas</button>
      </template>
    </EmptyState>

    <div v-if="rows.length > 0" class="table-wrap">
      <table class="data reorderable">
        <thead>
          <tr>
            <th scope="col" class="drag-col"><span class="sr-only">Drag to reorder</span></th>
            <th scope="col">Area</th>
            <th scope="col">Schema</th>
            <th scope="col">Classes</th>
            <th scope="col">Status</th>
            <th scope="col">Order</th>
            <th scope="col"><span class="sr-only">Actions</span></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="(a, i) in rows" :key="a.id" v-bind="dnd.row(a.id)" :class="{ disabled: !a.isActive }">
            <td class="drag-handle" aria-hidden="true" title="Drag to reorder"><Icon name="grip-vertical" /></td>
            <td>
              <button type="button" class="btn-link" :title="`Edit ${a.name}`" @click="openEdit(a)">
                <ClassBadge :icon="a.icon" :color="a.color" :name="a.name" />
              </button>
              <div v-if="a.description" class="muted cell-note">{{ a.description }}</div>
            </td>
            <td class="mono">{{ a.key }}</td>
            <td class="wrap">
              <span v-if="typesOf(a).length === 0" class="muted">none</span>
              <template v-for="(c, j) in typesOf(a)" :key="c.id">
                <RouterLink :to="`/admin/classes/${c.id}`" :class="{ muted: !c.isActive }">{{ c.name }}</RouterLink><template v-if="j < typesOf(a).length - 1">, </template>
              </template>
              <div><RouterLink class="cell-note" :to="{ path: '/admin/classes/new', query: { areaId: a.id } }">+ Class in {{ a.name }}</RouterLink></div>
            </td>
            <td>
              <span v-if="a.isActive" class="badge ok">Active</span>
              <span v-else class="badge off">Archived</span>
            </td>
            <td class="order-buttons">
              <button type="button" class="btn btn-sm btn-icon" :disabled="reorder.isPending.value || i === 0" :aria-label="`Move ${a.name} up`" @click="step(a, -1)"><Icon name="arrow-up" /></button>
              <button type="button" class="btn btn-sm btn-icon" :disabled="reorder.isPending.value || i === rows.length - 1" :aria-label="`Move ${a.name} down`" @click="step(a, 1)"><Icon name="arrow-down" /></button>
            </td>
            <td class="row-actions">
              <button type="button" class="btn btn-sm" @click="openEdit(a)">Edit</button>
              <button v-if="a.isActive" type="button" class="btn btn-sm" :aria-label="`Archive ${a.name}`" @click="archive(a)">Archive</button>
              <template v-else>
                <button type="button" class="btn btn-sm" :aria-label="`Restore ${a.name}`" @click="restore(a)">Restore</button>
                <button type="button" class="btn btn-sm btn-quiet-danger" :aria-label="`Purge ${a.name}`" @click="purgeArea(a)">Purge…</button>
              </template>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>

  <SchemaChangesPanel />

  <AreaDialog :open="dialogOpen" :area="editing" :next-sort-order="nextSortOrder" @close="dialogOpen = false" @saved="(m) => (notice = m)" />
  <SchemaChangeDialog :flow="flow" />
</template>
