<script setup lang="ts">
import { computed, ref } from "vue";
import { RouterLink } from "vue-router";
import { useAreas, usePatch, useRemove, useReorder } from "../../../api/datamodel";
import { useCiClasses, type CiClass } from "../../../api/queries";
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
import { bySortOrder, flattenTree } from "../../../lib/tree";
import { useFlashStore } from "../../../stores/flash";
import Icon from "../../../components/Icon.vue";

/**
 * Administration › Data model › CI classes. The class tree in the order menus and
 * pickers use. Reorder by dragging a row (or with the arrow buttons) among its
 * siblings; a class moves with its subclasses. Archived classes are hidden unless
 * "Show archived" is on; the area filter narrows the list to one area's classes
 * (both kept in the URL).
 */
useDocumentTitle("CI classes");
const lq = useListQuery({ sort: "sortOrder" });
const showArchived = computed(() => lq.get("archived") === "show");
const classes = useCiClasses();
const reorder = useReorder("ci-classes");
const patch = usePatch<CiClass>("ci-classes");
const remove = useRemove("ci-classes");
const areas = useAreas();
const flow = useSchemaChangeFlow();
const flash = useFlashStore();
const notice = ref<string | null>(flash.forCi("classes") ?? null);
const failure = ref<unknown>(null);
const areaFilter = computed(() => lq.get("areaId") ?? "");
const areaById = computed(() => new Map((areas.data.value ?? []).map((a) => [a.id, a])));
/** The order being saved, shown until the refetch arrives. */
const pendingOrder = ref<string[] | null>(null);

const all = computed(() => {
  const list = classes.data.value ?? [];
  if (!pendingOrder.value) return list;
  const rank = new Map(pendingOrder.value.map((id, i) => [id, i]));
  return [...list].sort((a, b) => (rank.get(a.id) ?? 0) - (rank.get(b.id) ?? 0));
});
const byId = computed(() => new Map(all.value.map((c) => [c.id, c])));
const tree = computed(() => flattenTree(all.value));
const rows = computed(() =>
  tree.value.filter((n) => (showArchived.value || n.item.isActive) && (!areaFilter.value || n.item.areaId === areaFilter.value)),
);
const archivedCount = computed(() => all.value.filter((c) => !c.isActive).length);

/** Siblings of a class in the current order (archived ones included, so their place is kept). */
function siblings(c: CiClass): CiClass[] {
  return tree.value.filter((n) => (n.item.parentId ?? null) === (c.parentId ?? null) && byId.value.has(n.item.id)).map((n) => n.item);
}

function commit(dragId: string, targetId: string) {
  const drag = byId.value.get(dragId);
  const target = byId.value.get(targetId);
  if (!drag || !target) return;
  if ((drag.parentId ?? null) !== (target.parentId ?? null)) {
    notice.value = `${drag.name} can only move among classes with the same parent. To move it under another parent, edit the class.`;
    return;
  }
  const sibs = siblings(drag);
  const moved = moveItem(sibs, sibs.indexOf(drag), sibs.indexOf(target));
  const rank = new Map(moved.map((c, i) => [c.id, i]));
  // Depth-first order with the new sibling order; sortOrder is renumbered along it, so flat menus match the tree.
  const order = flattenTree(all.value, (a, b) => (rank.has(a.id) && rank.has(b.id) ? rank.get(a.id)! - rank.get(b.id)! : bySortOrder(a, b)));
  pendingOrder.value = order.map((n) => n.item.id);
  notice.value = null;
  reorder.mutate(
    order.map((n) => ({ id: n.item.id, sortOrder: n.item.sortOrder })),
    {
      onSuccess: (n) => (notice.value = n > 0 ? `Moved ${drag.name}. Menus and pickers use the new order.` : null),
      onSettled: () => (pendingOrder.value = null),
    },
  );
}

/** Keyboard alternative to dragging: swap with the previous/next visible sibling. */
function step(c: CiClass, delta: -1 | 1) {
  const visible = siblings(c).filter((s) => showArchived.value || s.isActive);
  const target = visible[visible.indexOf(c) + delta];
  if (target) commit(c.id, target.id);
}
function canStep(c: CiClass, delta: -1 | 1): boolean {
  const visible = siblings(c).filter((s) => showArchived.value || s.isActive);
  const i = visible.indexOf(c) + delta;
  return i >= 0 && i < visible.length;
}

const dnd = useDragReorder(commit, () => !reorder.isPending.value);
const parentName = (c: CiClass) => (c.parentId ? byId.value.get(c.parentId)?.name : undefined);

async function setActive(c: CiClass, isActive: boolean) {
  notice.value = null;
  failure.value = null;
  const outcome = isActive
    ? await flow.run({
        title: `Restore class “${c.name}”`,
        preview: { operation: "updateType", id: c.id, body: { isActive: true } },
        apply: () => patch.mutateAsync({ id: c.id, body: { isActive: true } }),
        applyLabel: "Restore class",
      })
    : await flow.run({
        title: `Archive class “${c.name}”?`,
        intro: `Its table ${c.tableName}, its CIs and every stored value are kept and stay readable, but no new CIs can be created and the menu hides it. Restore it at any time; only a purge (on the class's page) deletes the data.`,
        preview: { operation: "deleteType", id: c.id },
        apply: () => remove.mutateAsync(c.id),
        applyLabel: "Archive class",
        alwaysShow: true,
      });
  if (outcome.status === "applied")
    notice.value = isActive
      ? `Restored ${c.name}: new CIs of this class can be created again.`
      : `Archived ${c.name}: its CIs are kept, but no new ones can be created.`;
  else if (outcome.status === "refused") failure.value = outcome.error;
}
</script>

<template>
  <Breadcrumbs :items="[{ label: 'Administration', to: '/admin' }, { label: 'Data model' }, { label: 'CI classes' }]" />
  <div class="page-header">
    <div class="title">
      <h1>CI classes</h1>
      <span v-if="classes.data.value" class="muted">{{ classes.data.value.length }} total</span>
      <span v-if="reorder.isPending.value || patch.isPending.value || (classes.isFetching.value && !classes.isLoading.value)" class="spinner" aria-label="Saving" />
    </div>
    <div class="actions">
      <RouterLink class="btn btn-primary" :to="{ path: '/admin/classes/new', query: areaFilter ? { areaId: areaFilter } : {} }">+ New class</RouterLink>
    </div>
  </div>

  <div v-if="notice" class="alert alert-success" role="status">{{ notice }}</div>
  <ErrorAlert v-if="reorder.isError.value" :error="reorder.error.value" title="The new order was not saved completely" />
  <ErrorAlert v-if="failure" :error="failure" title="Not saved" />

  <section class="panel" aria-label="CI classes">
    <div class="toolbar">
      <label class="checkbox-row">
        <input type="checkbox" :checked="showArchived" @change="lq.update({ archived: ($event.target as HTMLInputElement).checked ? 'show' : undefined })" />
        Show archived classes<span v-if="archivedCount" class="muted">&nbsp;({{ archivedCount }})</span>
      </label>
      <label class="inline-control">
        Area
        <select :value="areaFilter" @change="lq.update({ areaId: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">All areas</option>
          <option v-for="a in areas.data.value ?? []" :key="a.id" :value="a.id">{{ a.name }}{{ a.isActive ? "" : " (archived)" }}</option>
        </select>
      </label>
      <span class="muted" style="margin-left: auto">Drag a row, or use the arrows, to change the order of menus and pickers.</span>
    </div>
    <div v-if="classes.isError.value" class="panel-body">
      <ErrorAlert :error="classes.error.value" :on-retry="() => classes.refetch()" />
    </div>
    <LoadingState v-if="classes.isLoading.value" label="Loading classes…" />
    <EmptyState v-if="classes.data.value && classes.data.value.length === 0" title="No CI classes yet">
      A class is a kind of configuration item (server, application, database…) and decides which attributes its CIs
      carry. Start from the IT infrastructure starter, or build your own model class by class.
      <template #actions>
        <RouterLink class="btn btn-primary" to="/admin/templates">Install a starter template</RouterLink>
        <RouterLink class="btn" to="/admin/classes/new">+ New class</RouterLink>
      </template>
    </EmptyState>
    <EmptyState v-else-if="classes.data.value && rows.length === 0 && areaFilter" title="No classes in this area">
      <template #actions>
        <RouterLink class="btn btn-primary" :to="{ path: '/admin/classes/new', query: { areaId: areaFilter } }">+ New class in {{ areaById.get(areaFilter)?.name }}</RouterLink>
        <button type="button" class="btn" @click="lq.update({ areaId: undefined })">All areas</button>
      </template>
    </EmptyState>
    <EmptyState v-else-if="classes.data.value && rows.length === 0" title="Every class is archived">
      <template #actions>
        <button type="button" class="btn" @click="lq.update({ archived: 'show' })">Show archived classes</button>
      </template>
    </EmptyState>

    <div v-if="rows.length > 0" class="table-wrap">
      <table class="data reorderable">
        <thead>
          <tr>
            <th scope="col" class="drag-col"><span class="sr-only">Drag to reorder</span></th>
            <th scope="col">Class</th>
            <th scope="col">Area</th>
            <th scope="col">Table</th>
            <th scope="col">Parent</th>
            <th scope="col">Kind</th>
            <th scope="col">Status</th>
            <th scope="col">Order</th>
            <th scope="col"><span class="sr-only">Actions</span></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="{ item: c, depth } in rows" :key="c.id" v-bind="dnd.row(c.id)" :class="{ disabled: !c.isActive }">
            <td class="drag-handle" aria-hidden="true" title="Drag to reorder"><Icon name="grip-vertical" /></td>
            <td>
              <span :style="{ paddingLeft: `${depth * 18}px` }">
                <RouterLink :to="`/admin/classes/${c.id}`"><ClassBadge :icon="c.icon" :color="c.color" :name="c.name" /></RouterLink>
              </span>
            </td>
            <td>
              <RouterLink v-if="areaById.get(c.areaId)" :to="{ query: { ...$route.query, areaId: c.areaId } }" :title="`Only classes in ${areaById.get(c.areaId)!.name}`">
                {{ areaById.get(c.areaId)!.name }}
              </RouterLink>
            </td>
            <td class="mono">{{ c.tableName }}</td>
            <td>{{ parentName(c) ?? "" }}</td>
            <td>
              <span v-if="c.isAbstract" class="badge warn" title="Groups other classes; holds no CIs itself">Abstract</span>
              <span v-else class="muted">Concrete</span>
            </td>
            <td>
              <span v-if="c.isActive" class="badge ok">Active</span>
              <span v-else class="badge off">Archived</span>
            </td>
            <td class="order-buttons">
              <button type="button" class="btn btn-sm btn-icon" :disabled="reorder.isPending.value || !canStep(c, -1)" :aria-label="`Move ${c.name} up`" @click="step(c, -1)"><Icon name="arrow-up" /></button>
              <button type="button" class="btn btn-sm btn-icon" :disabled="reorder.isPending.value || !canStep(c, 1)" :aria-label="`Move ${c.name} down`" @click="step(c, 1)"><Icon name="arrow-down" /></button>
            </td>
            <td class="row-actions">
              <RouterLink class="btn btn-sm" :to="`/admin/classes/${c.id}`">Edit</RouterLink>
              <button v-if="c.isActive" type="button" class="btn btn-sm" :aria-label="`Archive ${c.name}`" @click="setActive(c, false)">Archive</button>
              <button v-else type="button" class="btn btn-sm" :aria-label="`Restore ${c.name}`" @click="setActive(c, true)">Restore</button>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>
  <SchemaChangeDialog :flow="flow" />
</template>
