<script setup lang="ts">
import { computed, ref } from "vue";
import { RouterLink } from "vue-router";
import { useLookupLists, useOwnAttributes, usePatch, useReorder, type AttributeDefinition } from "../../../api/datamodel";
import { useCiClasses, useClassAttributes, type CiClass } from "../../../api/queries";
import DeleteRowButton from "../../../components/DeleteRowButton.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import LookupValueName from "../../../components/LookupValueName.vue";
import { DEFAULT_SECTION, groupAttributes } from "../../../lib/attributes";
import { dataTypeLabel } from "../../../lib/dataTypes";
import { formatDate, formatDateTime } from "../../../lib/format";
import { moveItem, useDragReorder } from "../../../lib/reorder";
import AttributeDialog from "./AttributeDialog.vue";

/**
 * The attributes defined on one class, by form section, in form order. Drag a
 * row (or use the arrows) to reorder; dropping it into another section moves it
 * there. Attributes inherited from parent classes are listed read-only below.
 */
const props = defineProps<{ cls: CiClass }>();
const own = useOwnAttributes(() => props.cls.id);
const effective = useClassAttributes(() => props.cls.id);
const classes = useCiClasses();
const lists = useLookupLists();
const reorder = useReorder("attribute-definitions");
const patch = usePatch<AttributeDefinition>("attribute-definitions");
const notice = ref<string | null>(null);

type Placed = { id: string; groupName: string | null };
/** The order and sections being saved, shown until the refetch arrives. */
const pending = ref<Placed[] | null>(null);

const defs = computed<AttributeDefinition[]>(() => {
  const list = own.data.value?.data ?? [];
  if (!pending.value) return list;
  const at = new Map(pending.value.map((p, i) => [p.id, { i, groupName: p.groupName }]));
  return list.map((d) => ({ ...d, sortOrder: (at.get(d.id)?.i ?? 0) * 10, groupName: at.get(d.id)?.groupName ?? null }));
});
const groups = computed(() => groupAttributes(defs.value));
const flat = computed(() => groups.value.flatMap(([, items]) => items));
const sections = computed(() => [
  ...new Set([...groups.value.map(([g]) => g), ...(effective.data.value ?? []).map((d) => d.groupName ?? DEFAULT_SECTION)]),
]);
const inherited = computed(() => (effective.data.value ?? []).filter((d) => d.inherited));
const nextSortOrder = computed(() => Math.max(0, ...defs.value.map((d) => d.sortOrder)) + 10);
const sectionOf = (d: AttributeDefinition) => d.groupName || DEFAULT_SECTION;
/** The groupName to store for a section heading (the default section is "no section"). */
const storedSection = (section: string) => (section === DEFAULT_SECTION ? null : section);

function save(order: AttributeDefinition[], moved: AttributeDefinition, toSection: string) {
  const changedSection = sectionOf(moved) !== toSection;
  pending.value = order.map((d) => ({ id: d.id, groupName: d.id === moved.id ? storedSection(toSection) : d.groupName }));
  notice.value = null;
  reorder.mutate(
    order.map((d) => ({
      id: d.id,
      sortOrder: d.sortOrder,
      extra: d.id === moved.id && changedSection ? { groupName: storedSection(toSection) } : undefined,
    })),
    {
      onSuccess: () => (notice.value = `Moved ${moved.label}${changedSection ? ` to section “${toSection}”` : ""}.`),
      onSettled: () => (pending.value = null),
    },
  );
}

/** Drop on a row: take its position and its section. Drop on a section heading: become the section's first attribute. */
function commit(dragId: string, targetId: string) {
  const list = flat.value;
  const drag = list.find((d) => d.id === dragId);
  if (!drag) return;
  if (targetId.startsWith("section:")) {
    const section = targetId.slice(8);
    const first = list.findIndex((d) => sectionOf(d) === section);
    if (first < 0) return;
    const from = list.indexOf(drag);
    save(moveItem(list, from, from < first ? first - 1 : first), drag, section);
    return;
  }
  const target = list.find((d) => d.id === targetId);
  if (!target) return;
  save(moveItem(list, list.indexOf(drag), list.indexOf(target)), drag, sectionOf(target));
}

/** Arrow buttons: move one place; at a section boundary, first move into the neighbouring section. */
function step(d: AttributeDefinition, delta: -1 | 1) {
  const list = flat.value;
  const neighbour = list[list.indexOf(d) + delta];
  if (!neighbour) return;
  if (sectionOf(neighbour) !== sectionOf(d)) save(list, d, sectionOf(neighbour));
  else commit(d.id, neighbour.id);
}

const dnd = useDragReorder(commit, () => !reorder.isPending.value);

function setActive(d: AttributeDefinition, isActive: boolean) {
  notice.value = null;
  patch.mutate(
    { id: d.id, body: { isActive } },
    {
      onSuccess: () =>
        (notice.value = isActive
          ? `Restored ${d.label}: it shows on forms again.`
          : `Archived ${d.label}: stored values are kept and still shown, but it is no longer on forms.`),
    },
  );
}

// ---------- Add / edit dialog ----------
const dialogOpen = ref(false);
const editing = ref<AttributeDefinition | null>(null);
const dialogSection = ref<string | undefined>(undefined);

function openNew(section?: string) {
  editing.value = null;
  dialogSection.value = section && section !== DEFAULT_SECTION ? section : undefined;
  dialogOpen.value = true;
}
function openEdit(d: AttributeDefinition) {
  editing.value = d;
  dialogOpen.value = true;
}

// ---------- Display helpers ----------
const className = (id: string | null) => (id ? (classes.data.value?.find((c) => c.id === id)?.name ?? "unknown class") : "");
const listName = (id: string | null) => (id ? (lists.data.value?.find((l) => l.id === id)?.name ?? "unknown list") : "");

function typeDetail(d: { dataType: string; enumValues: string[] | null; referenceClassId: string | null; lookupListId: string | null }): string {
  if (d.dataType === "enum") return (d.enumValues ?? []).join(", ");
  if (d.dataType === "reference") return `→ ${className(d.referenceClassId)}`;
  if (d.dataType === "lookup") return listName(d.lookupListId);
  return "";
}

function defaultText(d: AttributeDefinition): string {
  const v = d.defaultValue;
  if (v === null || v === undefined || v === "") return "";
  if (d.dataType === "boolean") return v ? "Yes" : "No";
  if (d.dataType === "date") return formatDate(String(v));
  if (d.dataType === "datetime") return formatDateTime(String(v));
  return String(v);
}
</script>

<template>
  <section class="panel" aria-labelledby="attrs-title">
    <div class="panel-header">
      <h2 id="attrs-title">Attributes of {{ cls.name }}</h2>
      <span v-if="reorder.isPending.value || patch.isPending.value" class="spinner" aria-label="Saving" />
      <span class="muted">Drag a row, or use the arrows, to change the form order. Drop on a section to move it there.</span>
      <button type="button" class="btn btn-primary btn-sm" style="margin-left: auto" @click="openNew()">+ Add attribute</button>
    </div>
    <div v-if="notice || reorder.isError.value || patch.isError.value" class="panel-body">
      <div v-if="notice" class="alert" role="status">{{ notice }}</div>
      <ErrorAlert v-if="reorder.isError.value" :error="reorder.error.value" title="The new order was not saved completely" />
      <ErrorAlert v-if="patch.isError.value" :error="patch.error.value" title="Not saved" />
    </div>
    <LoadingState v-if="own.isLoading.value" label="Loading attributes…" />
    <div v-else-if="own.isError.value" class="panel-body">
      <ErrorAlert :error="own.error.value" :on-retry="() => own.refetch()" />
    </div>
    <div v-else-if="defs.length === 0" class="panel-body">
      <p class="muted" style="margin: 0">
        {{ cls.name }} defines no attributes of its own yet{{ inherited.length ? "; its CIs carry the inherited ones listed below" : "" }}.
        CIs always have the general fields (name, status, environment, owner, location, hostname, IP, serial, notes).
      </p>
    </div>
    <div v-else class="table-wrap">
      <table class="data reorderable attributes">
        <thead>
          <tr>
            <th scope="col" class="drag-col"><span class="sr-only">Drag to reorder</span></th>
            <th scope="col">Label</th>
            <th scope="col">Key</th>
            <th scope="col">Type</th>
            <th scope="col">Required</th>
            <th scope="col">Default</th>
            <th scope="col">Order</th>
            <th scope="col"><span class="sr-only">Actions</span></th>
          </tr>
        </thead>
        <tbody v-for="[section, items] in groups" :key="section">
          <tr class="section-row" v-bind="dnd.row(`section:${section}`)">
            <th colspan="7" scope="colgroup">{{ section }}</th>
            <td class="row-actions">
              <button type="button" class="btn-link" :aria-label="`Add an attribute to ${section}`" @click="openNew(section)">+ Add here</button>
            </td>
          </tr>
          <tr v-for="d in items" :key="d.id" v-bind="dnd.row(d.id)" :class="{ disabled: !d.isActive }">
            <td class="drag-handle" aria-hidden="true" title="Drag to reorder">⠿</td>
            <td>
              <button type="button" class="btn-link" :title="`Edit ${d.label}`" @click="openEdit(d)">{{ d.label }}</button>
              <span v-if="!d.isActive" class="badge off" title="Kept on CIs that have a value; not on forms">archived</span>
              <div v-if="d.helpText" class="muted cell-note">{{ d.helpText }}</div>
            </td>
            <td class="mono">{{ d.key }}</td>
            <td>
              {{ dataTypeLabel(d.dataType) }}
              <div v-if="typeDetail(d)" class="muted cell-note" :title="typeDetail(d)">{{ typeDetail(d) }}</div>
            </td>
            <td><span v-if="d.isRequired" class="badge warn">Required</span></td>
            <td>
              <LookupValueName v-if="d.dataType === 'lookup' && d.defaultValue" :list-id="d.lookupListId" :value-id="String(d.defaultValue)" />
              <template v-else>{{ defaultText(d) }}</template>
            </td>
            <td class="order-buttons">
              <button type="button" class="btn btn-sm" :disabled="reorder.isPending.value || flat.indexOf(d) === 0" :aria-label="`Move ${d.label} up`" @click="step(d, -1)">↑</button>
              <button type="button" class="btn btn-sm" :disabled="reorder.isPending.value || flat.indexOf(d) === flat.length - 1" :aria-label="`Move ${d.label} down`" @click="step(d, 1)">↓</button>
            </td>
            <td class="row-actions">
              <button v-if="d.isActive" type="button" class="btn btn-sm" :disabled="patch.isPending.value" :aria-label="`Archive ${d.label}`" @click="setActive(d, false)">Archive</button>
              <button v-else type="button" class="btn btn-sm" :disabled="patch.isPending.value" :aria-label="`Restore ${d.label}`" @click="setActive(d, true)">Restore</button>
              <DeleteRowButton
                resource="attribute-definitions"
                :id="d.id"
                :label="`attribute “${d.label}”`"
                archivable
                :archived="!d.isActive"
                small
                @archive="setActive(d, false)"
                @deleted="notice = `Deleted attribute ${d.label}.`"
              />
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>

  <section v-if="inherited.length > 0" class="panel" aria-labelledby="inherited-title">
    <div class="panel-header">
      <h2 id="inherited-title">Inherited attributes</h2>
      <span class="muted">Defined on a parent class; edit them there</span>
    </div>
    <div class="table-wrap">
      <table class="data">
        <thead>
          <tr>
            <th scope="col">Label</th>
            <th scope="col">Key</th>
            <th scope="col">Type</th>
            <th scope="col">Section</th>
            <th scope="col">Required</th>
            <th scope="col">Defined on</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="d in inherited" :key="d.id" :class="{ disabled: !d.isActive }">
            <td>{{ d.label }}</td>
            <td class="mono">{{ d.key }}</td>
            <td>{{ dataTypeLabel(d.dataType) }}<span v-if="typeDetail(d)" class="muted"> · {{ typeDetail(d) }}</span></td>
            <td>{{ d.groupName ?? DEFAULT_SECTION }}</td>
            <td><span v-if="d.isRequired" class="badge warn">Required</span></td>
            <td><RouterLink :to="`/admin/classes/${d.definedOn.id}`">{{ d.definedOn.name }}</RouterLink></td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>

  <AttributeDialog
    :open="dialogOpen"
    :cls="cls"
    :def="editing"
    :sections="sections"
    :default-section="dialogSection"
    :next-sort-order="nextSortOrder"
    @close="dialogOpen = false"
    @saved="(m) => (notice = m)"
  />
</template>
