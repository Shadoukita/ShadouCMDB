<script setup lang="ts">
import { adminCrumbs } from "../sections";
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
import Icon from "../../../components/Icon.vue";
import RowMenu, { type RowMenuItem } from "../../../components/RowMenu.vue";
import { formatNumber, t } from "../../../i18n";
import { onRowKeydown } from "../../../lib/rowKeyboard";
import { useFlashStore } from "../../../stores/flash";

/**
 * Administration › Data model › CI classes. The class tree in the order menus and
 * pickers use. Reorder by dragging a row (or with the arrow buttons) among its
 * siblings; a class moves with its subclasses. Archived classes are hidden unless
 * "Show archived" is on; the area filter narrows the list to one area's classes
 * (both kept in the URL).
 */
useDocumentTitle(t("dm.classes.title"));
const lq = useListQuery({ sort: "sortOrder" });
const showArchived = computed(() => lq.get("archived") === "show");
const classes = useCiClasses();
const reorder = useReorder("ci-classes");
const patch = usePatch<CiClass>("ci-classes");
const remove = useRemove("ci-classes");
const areas = useAreas();
const flow = useSchemaChangeFlow();
const flash = useFlashStore();
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
    flash.show(t("dm.classes.sameParent", { name: drag.name }));
    return;
  }
  const sibs = siblings(drag);
  const moved = moveItem(sibs, sibs.indexOf(drag), sibs.indexOf(target));
  const rank = new Map(moved.map((c, i) => [c.id, i]));
  // Depth-first order with the new sibling order; sortOrder is renumbered along it, so flat menus match the tree.
  const order = flattenTree(all.value, (a, b) => (rank.has(a.id) && rank.has(b.id) ? rank.get(a.id)! - rank.get(b.id)! : bySortOrder(a, b)));
  pendingOrder.value = order.map((n) => n.item.id);
  reorder.mutate(
    order.map((n) => ({ id: n.item.id, sortOrder: n.item.sortOrder })),
    {
      onSuccess: (n) => {
        if (n > 0) flash.show(t("dm.classes.moved", { name: drag.name }));
      },
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
  failure.value = null;
  const outcome = isActive
    ? await flow.run({
        title: t("dm.class.restore.title", { name: c.name }),
        preview: { operation: "updateType", id: c.id, body: { isActive: true } },
        apply: () => patch.mutateAsync({ id: c.id, body: { isActive: true } }),
        applyLabel: t("dm.class.restore.apply"),
      })
    : await flow.run({
        title: t("dm.class.archive.title", { name: c.name }),
        intro: t("dm.classes.archive.intro", { table: c.tableName }),
        preview: { operation: "deleteType", id: c.id },
        apply: () => remove.mutateAsync(c.id),
        applyLabel: t("dm.class.archive.apply"),
        alwaysShow: true,
      });
  if (outcome.status === "applied") flash.show(t(isActive ? "dm.class.restored" : "dm.class.archived", { name: c.name }));
  else if (outcome.status === "refused") failure.value = outcome.error;
}

const rowMenu = (c: CiClass): RowMenuItem[] => [
  { label: t("common.edit"), to: `/admin/classes/${c.id}` },
  c.isActive ? { label: t("dm.class.archive"), action: () => void setActive(c, false) } : { label: t("dm.class.restore"), action: () => void setActive(c, true) },
];
const newClassTo = computed(() => ({ path: "/admin/classes/new", query: areaFilter.value ? { areaId: areaFilter.value } : {} }));
</script>

<template>
  <Breadcrumbs :items="adminCrumbs('classes')" />
  <div class="page-header">
    <div class="title">
      <h1>{{ t("dm.classes.title") }}</h1>
      <span v-if="classes.data.value" class="muted count">{{ t("common.total", { n: formatNumber(classes.data.value.length) }) }}</span>
      <span
        v-if="reorder.isPending.value || patch.isPending.value || (classes.isFetching.value && !classes.isLoading.value)"
        class="spinner"
        :aria-label="t('common.saving')"
      />
    </div>
    <div class="actions">
      <RouterLink class="btn btn-primary" :to="newClassTo"><Icon name="plus" />{{ t("dm.classes.create") }}</RouterLink>
    </div>
  </div>
  <p class="page-intro">{{ t("dm.classes.intro") }}</p>

  <ErrorAlert v-if="reorder.isError.value" :error="reorder.error.value" :title="t('dm.classes.reorderFailed')" />
  <ErrorAlert v-if="failure" :error="failure" :title="t('dm.classes.notSaved')" />

  <section class="panel explorer" :aria-label="t('dm.classes.title')">
    <div class="toolbar">
      <label class="checkbox-row">
        <input type="checkbox" :checked="showArchived" @change="lq.update({ archived: ($event.target as HTMLInputElement).checked ? 'show' : undefined })" />
        {{ t("dm.classes.showArchived") }}<span v-if="archivedCount" class="muted">&nbsp;({{ formatNumber(archivedCount) }})</span>
      </label>
      <label class="inline-control">
        {{ t("dm.classes.area") }}
        <select :value="areaFilter" @change="lq.update({ areaId: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">{{ t("dm.classes.allAreas") }}</option>
          <option v-for="a in areas.data.value ?? []" :key="a.id" :value="a.id">{{ a.isActive ? a.name : t("dm.classes.archivedArea", { name: a.name }) }}</option>
        </select>
      </label>
      <p class="toolbar-hint">{{ t("dm.classes.reorderHint") }}</p>
    </div>
    <div v-if="classes.isError.value" class="panel-body">
      <ErrorAlert :error="classes.error.value" :on-retry="() => classes.refetch()" />
    </div>
    <LoadingState v-if="classes.isLoading.value" :label="t('dm.classes.loading')" />
    <EmptyState v-if="classes.data.value && classes.data.value.length === 0" icon="layers" :title="t('dm.classes.empty.title')">
      {{ t("dm.classes.empty.body") }}
      <template #actions>
        <RouterLink class="btn btn-primary" to="/admin/templates">{{ t("dataModel.empty.installTemplate") }}</RouterLink>
        <RouterLink class="btn" to="/admin/classes/new"><Icon name="plus" />{{ t("dm.classes.create") }}</RouterLink>
      </template>
    </EmptyState>
    <EmptyState v-else-if="classes.data.value && rows.length === 0 && areaFilter" icon="layers" :title="t('dm.classes.emptyArea.title')">
      {{ t("dm.classes.emptyArea.body") }}
      <template #actions>
        <RouterLink class="btn btn-primary" :to="newClassTo">
          <Icon name="plus" />{{ t("dm.classes.emptyArea.create", { area: areaById.get(areaFilter)?.name ?? "" }) }}
        </RouterLink>
        <button type="button" class="btn" @click="lq.update({ areaId: undefined })">{{ t("dm.classes.allAreas") }}</button>
      </template>
    </EmptyState>
    <EmptyState v-else-if="classes.data.value && rows.length === 0" icon="layers" :title="t('dm.classes.allArchived.title')">
      {{ t("dm.classes.allArchived.body") }}
      <template #actions>
        <button type="button" class="btn" @click="lq.update({ archived: 'show' })">{{ t("dm.classes.showArchived") }}</button>
      </template>
    </EmptyState>

    <div v-if="rows.length > 0" class="table-wrap">
      <table class="data reorderable">
        <thead>
          <tr>
            <th scope="col" class="drag-col"><span class="sr-only">{{ t("dm.classes.drag") }}</span></th>
            <th scope="col">{{ t("dm.classes.col.class") }}</th>
            <th scope="col">{{ t("dm.classes.col.area") }}</th>
            <th scope="col">{{ t("dm.classes.col.table") }}</th>
            <th scope="col">{{ t("dm.classes.col.parent") }}</th>
            <th scope="col">{{ t("dm.classes.col.kind") }}</th>
            <th scope="col">{{ t("dm.classes.col.status") }}</th>
            <th scope="col">{{ t("dm.classes.col.order") }}</th>
            <th scope="col" class="row-actions"><span class="sr-only">{{ t("inventory.actions") }}</span></th>
          </tr>
        </thead>
        <tbody @keydown="onRowKeydown($event)">
          <tr v-for="{ item: c, depth } in rows" :key="c.id" v-bind="dnd.row(c.id)" :data-id="c.id" :class="{ disabled: !c.isActive }">
            <td class="drag-handle" aria-hidden="true" :title="t('dm.classes.drag')"><Icon name="grip-vertical" /></td>
            <td>
              <span :style="{ paddingLeft: `${depth * 18}px` }">
                <RouterLink :to="`/admin/classes/${c.id}`"><ClassBadge :icon="c.icon" :color="c.color" :name="c.name" /></RouterLink>
              </span>
            </td>
            <td>
              <RouterLink
                v-if="areaById.get(c.areaId)"
                :to="{ query: { ...$route.query, areaId: c.areaId } }"
                :title="t('dm.classes.onlyArea', { area: areaById.get(c.areaId)!.name })"
              >
                {{ areaById.get(c.areaId)!.name }}
              </RouterLink>
            </td>
            <td class="mono">{{ c.tableName }}</td>
            <td>{{ parentName(c) ?? "" }}</td>
            <td>
              <span v-if="c.isAbstract" class="badge warn" :title="t('dm.class.abstractTitle')">{{ t("dm.class.abstract") }}</span>
              <span v-else class="muted">{{ t("dm.classes.concrete") }}</span>
            </td>
            <td>
              <span v-if="c.isActive" class="badge ok">{{ t("common.active") }}</span>
              <span v-else class="badge off">{{ t("dm.class.archivedBadge") }}</span>
            </td>
            <td class="order-buttons">
              <button
                type="button"
                class="btn btn-sm btn-icon"
                :disabled="reorder.isPending.value || !canStep(c, -1)"
                :aria-label="t('dm.classes.moveUp', { name: c.name })"
                @click="step(c, -1)"
              >
                <Icon name="arrow-up" />
              </button>
              <button
                type="button"
                class="btn btn-sm btn-icon"
                :disabled="reorder.isPending.value || !canStep(c, 1)"
                :aria-label="t('dm.classes.moveDown', { name: c.name })"
                @click="step(c, 1)"
              >
                <Icon name="arrow-down" />
              </button>
            </td>
            <td class="row-actions">
              <RowMenu :label="t('inventory.rowMenu', { name: c.name })" :items="rowMenu(c)" />
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>
  <SchemaChangeDialog :flow="flow" />
</template>
