<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { RouterLink } from "vue-router";
import { ApiError } from "../../../api/client";
import {
  useCreateRelType,
  useCreateRule,
  usePatch,
  useRelTypeList,
  useReorder,
  useRules,
  type RelTypeCreateBody,
  type RelTypeUpdateBody,
} from "../../../api/datamodel";
import { useCiClasses, type RelationshipType } from "../../../api/queries";
import Breadcrumbs from "../../../components/Breadcrumbs.vue";
import DeleteRowButton from "../../../components/DeleteRowButton.vue";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import RecordDialog, { type FieldSpec, type SelectOption } from "../../../components/RecordDialog.vue";
import { useDocumentTitle } from "../../../lib/composables";
import { useListQuery } from "../../../lib/listQuery";
import { moveItem, useDragReorder } from "../../../lib/reorder";
import { flattenTree } from "../../../lib/tree";
import Icon from "../../../components/Icon.vue";

/**
 * Administration › Data model › Relationship types. A type names an edge in both
 * directions ("runs on" / "hosts"); its rules say which classes it may connect.
 * The selected type (?type=…, in the URL) shows its rules below the list.
 */
useDocumentTitle("Relationship types");
const lq = useListQuery({ sort: "sortOrder" });
const types = useRelTypeList();
const rules = useRules();
const classes = useCiClasses();
const createType = useCreateRelType();
const patchType = usePatch<RelationshipType>("relationship-types");
const reorder = useReorder("relationship-types");
const createRule = useCreateRule();
const notice = ref<string | null>(null);
const pendingOrder = ref<string[] | null>(null);

const rows = computed(() => {
  const list = types.data.value?.data ?? [];
  if (!pendingOrder.value) return list;
  const rank = new Map(pendingOrder.value.map((id, i) => [id, i]));
  return [...list].sort((a, b) => (rank.get(a.id) ?? 0) - (rank.get(b.id) ?? 0));
});
const selectedId = computed(() => lq.get("type") || rows.value[0]?.id || "");
const selected = computed(() => rows.value.find((t) => t.id === selectedId.value));
const allRules = computed(() => rules.data.value?.data ?? []);
const rulesOf = (typeId: string) => allRules.value.filter((r) => r.relationshipTypeId === typeId);
const classById = computed(() => new Map((classes.data.value ?? []).map((c) => [c.id, c])));
const className = (id: string) => classById.value.get(id)?.name ?? "unknown class";
const selectedRules = computed(() =>
  selected.value
    ? rulesOf(selected.value.id).sort((a, b) => className(a.sourceClassId).localeCompare(className(b.sourceClassId)) || className(a.targetClassId).localeCompare(className(b.targetClassId)))
    : [],
);

function commit(dragId: string, targetId: string) {
  const list = rows.value;
  const dragged = list.find((t) => t.id === dragId);
  const moved = moveItem(list, list.findIndex((t) => t.id === dragId), list.findIndex((t) => t.id === targetId));
  pendingOrder.value = moved.map((t) => t.id);
  notice.value = null;
  reorder.mutate(
    moved.map((t) => ({ id: t.id, sortOrder: t.sortOrder })),
    {
      onSuccess: () => (notice.value = `Moved ${dragged?.name}. Pickers list relationship types in this order.`),
      onSettled: () => (pendingOrder.value = null),
    },
  );
}
function step(t: RelationshipType, delta: -1 | 1) {
  const target = rows.value[rows.value.indexOf(t) + delta];
  if (target) commit(t.id, target.id);
}
const dnd = useDragReorder(commit, () => !reorder.isPending.value);

function setActive(t: RelationshipType, isActive: boolean) {
  notice.value = null;
  patchType.mutate(
    { id: t.id, body: { isActive } },
    {
      onSuccess: () =>
        (notice.value = isActive
          ? `Restored ${t.name}: it can be chosen for new relationships again.`
          : `Archived ${t.name}: existing relationships are kept, but no new ones can use it.`),
    },
  );
}

// ---------- Impact propagation ----------
type ImpactDirection = RelationshipType["impactDirection"];
/**
 * The impact settings in words from the type's own labels ("When the target fails, the source is
 * affected (source runs on target)"). A symmetric type offers only none and both: its ends mean the same.
 */
function impactOptions(values: Record<string, string | boolean>, record: Record<string, unknown> | null): SelectOption[] {
  const fwd = String(values.forwardLabel || "…");
  const rev = String(values.reverseLabel || "…");
  const directional = record ? record.isDirectional !== false : values.isDirectional !== false;
  return [
    { value: "none", label: "Does not propagate impact" },
    ...(directional
      ? [
          { value: "target_to_source", label: `When the target fails, the source is affected (source ${fwd} target)` },
          { value: "source_to_target", label: `When the source fails, the target is affected (target ${rev} source)` },
        ]
      : []),
    { value: "both", label: directional ? "Both ways: either end failing affects the other" : `Both ways: either end failing affects the other (${fwd})` },
  ];
}
const IMPACT_LABEL: Record<ImpactDirection, string> = {
  none: "—",
  target_to_source: "Target → source",
  source_to_target: "Source → target",
  both: "Both ways",
};
const impactTitle = (t: RelationshipType) =>
  impactOptions({ forwardLabel: t.forwardLabel, reverseLabel: t.reverseLabel }, t).find((o) => o.value === t.impactDirection)?.label ?? "";

// ---------- Type dialog ----------
const TYPE_FIELDS: FieldSpec[] = [
  { name: "name", label: "Name", type: "text", required: true, hint: "e.g. Runs on" },
  { name: "key", label: "Key", type: "key", createOnly: true, from: "name" },
  { name: "forwardLabel", label: "Label from the source", type: "text", required: true, hint: "Source → target, e.g. “runs on”" },
  { name: "reverseLabel", label: "Label from the target", type: "text", required: true, hint: "Target → source, e.g. “hosts”" },
  { name: "isDirectional", label: "Direction", type: "checkbox", createOnly: true, text: "Directional (source and target differ in meaning)" },
  {
    name: "impactDirection",
    label: "Impact propagation",
    type: "select",
    wide: true,
    options: impactOptions,
    hint: "Which way a failure travels across relationships of this type, for impact analysis",
  },
  { name: "description", label: "Description", type: "textarea" },
];
const dialogOpen = ref(false);
const editing = ref<RelationshipType | null>(null);
function openType(t: RelationshipType | null) {
  editing.value = t;
  dialogOpen.value = true;
}
async function saveType(body: Record<string, unknown>, isNew: boolean): Promise<string> {
  if (isNew) {
    const last = Math.max(0, ...rows.value.map((t) => t.sortOrder));
    const created = await createType.mutateAsync({ ...(body as RelTypeCreateBody), sortOrder: last + 10 });
    lq.update({ type: created.id });
    return `Created relationship type ${created.name}. Add rules to say which classes it connects.`;
  }
  const saved = await patchType.mutateAsync({ id: editing.value!.id, body: body as RelTypeUpdateBody });
  return `Saved ${saved.name}.`;
}

// ---------- Add rule ----------
const ruleSource = ref("");
const ruleTarget = ref("");
watch(selectedId, () => {
  createRule.reset();
  ruleSource.value = "";
  ruleTarget.value = "";
});
const classOptions = computed(() => flattenTree(classes.data.value ?? []));
async function addRule() {
  if (!selected.value || !ruleSource.value || !ruleTarget.value) return;
  notice.value = null;
  try {
    await createRule.mutateAsync({ relationshipTypeId: selected.value.id, sourceClassId: ruleSource.value, targetClassId: ruleTarget.value });
    notice.value = `Added rule: ${className(ruleSource.value)} ${selected.value.forwardLabel} ${className(ruleTarget.value)}.`;
    ruleSource.value = "";
    ruleTarget.value = "";
  } catch {
    // shown from createRule.error
  }
}
const ruleFieldErrors = computed(() => (createRule.error.value instanceof ApiError ? createRule.error.value.fieldErrors() : {}));

// A rule row has no name of its own; reuse it in the delete dialog.
const ruleLabel = (r: { sourceClassId: string; targetClassId: string }) =>
  `rule “${className(r.sourceClassId)} ${selected.value?.forwardLabel ?? "→"} ${className(r.targetClassId)}”`;
</script>

<template>
  <Breadcrumbs :items="[{ label: 'Administration', to: '/admin' }, { label: 'Data model' }, { label: 'Relationship types' }]" />
  <div class="page-header">
    <div class="title">
      <h1>Relationship types</h1>
      <span v-if="types.data.value" class="muted">{{ types.data.value.page.total }} total</span>
      <span v-if="reorder.isPending.value || patchType.isPending.value" class="spinner" aria-label="Saving" />
    </div>
    <div class="actions">
      <button type="button" class="btn btn-primary" @click="openType(null)">+ New relationship type</button>
    </div>
  </div>
  <div v-if="notice" class="alert alert-success" role="status">{{ notice }}</div>
  <ErrorAlert v-if="reorder.isError.value" :error="reorder.error.value" title="The new order was not saved completely" />
  <ErrorAlert v-if="patchType.isError.value" :error="patchType.error.value" title="Not saved" />

  <section class="panel" aria-label="Relationship types">
    <div v-if="types.isError.value" class="panel-body">
      <ErrorAlert :error="types.error.value" :on-retry="() => types.refetch()" />
    </div>
    <LoadingState v-if="types.isLoading.value" label="Loading relationship types…" />
    <EmptyState v-if="types.data.value && rows.length === 0" title="No relationship types yet">
      Relationship types name how CIs connect (“runs on”, “depends on”, “located in”). Create one, then add rules for the
      classes it may connect, or install the IT infrastructure starter.
      <template #actions>
        <button type="button" class="btn btn-primary" @click="openType(null)">+ New relationship type</button>
        <RouterLink class="btn" to="/admin/templates">Starter templates</RouterLink>
      </template>
    </EmptyState>
    <div v-if="rows.length > 0" class="table-wrap">
      <table class="data reorderable">
        <thead>
          <tr>
            <th scope="col" class="drag-col"><span class="sr-only">Drag to reorder</span></th>
            <th scope="col">Name</th>
            <th scope="col">Key</th>
            <th scope="col">Source → target</th>
            <th scope="col">Target → source</th>
            <th scope="col">Impact</th>
            <th scope="col">Rules</th>
            <th scope="col">Status</th>
            <th scope="col">Order</th>
            <th scope="col"><span class="sr-only">Actions</span></th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="t in rows"
            :key="t.id"
            v-bind="dnd.row(t.id)"
            :class="{ disabled: !t.isActive, selected: t.id === selectedId }"
            :aria-selected="t.id === selectedId"
          >
            <td class="drag-handle" aria-hidden="true" title="Drag to reorder"><Icon name="grip-vertical" /></td>
            <td>
              <RouterLink :to="{ query: { type: t.id } }">{{ t.name }}</RouterLink>
              <span v-if="!t.isDirectional" class="badge" title="Both ends mean the same (e.g. connected to)">symmetric</span>
            </td>
            <td class="mono">{{ t.key }}</td>
            <td>{{ t.forwardLabel }}</td>
            <td>{{ t.reverseLabel }}</td>
            <td :title="impactTitle(t)">
              <span v-if="t.impactDirection === 'none'" class="muted">{{ IMPACT_LABEL.none }}</span>
              <template v-else>{{ IMPACT_LABEL[t.impactDirection] }}</template>
            </td>
            <td>{{ rules.data.value ? rulesOf(t.id).length : "" }}</td>
            <td>
              <span v-if="t.isActive" class="badge ok">Active</span>
              <span v-else class="badge off">Archived</span>
            </td>
            <td class="order-buttons">
              <button type="button" class="btn btn-sm btn-icon" :disabled="reorder.isPending.value || rows.indexOf(t) === 0" :aria-label="`Move ${t.name} up`" @click="step(t, -1)"><Icon name="arrow-up" /></button>
              <button type="button" class="btn btn-sm btn-icon" :disabled="reorder.isPending.value || rows.indexOf(t) === rows.length - 1" :aria-label="`Move ${t.name} down`" @click="step(t, 1)"><Icon name="arrow-down" /></button>
            </td>
            <td class="row-actions">
              <button type="button" class="btn btn-sm" :aria-label="`Edit ${t.name}`" @click="openType(t)">Edit</button>
              <button v-if="t.isActive" type="button" class="btn btn-sm" :disabled="patchType.isPending.value" :aria-label="`Archive ${t.name}`" @click="setActive(t, false)">Archive</button>
              <button v-else type="button" class="btn btn-sm" :disabled="patchType.isPending.value" :aria-label="`Restore ${t.name}`" @click="setActive(t, true)">Restore</button>
              <DeleteRowButton
                resource="relationship-types"
                :id="t.id"
                :label="`relationship type “${t.name}”`"
                archivable
                :archived="!t.isActive"
                small
                @archive="setActive(t, false)"
                @deleted="(notice = `Deleted relationship type ${t.name}.`), lq.update({ type: undefined })"
              />
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>

  <section v-if="selected" class="panel" aria-labelledby="rules-title">
    <div class="panel-header">
      <h2 id="rules-title">Rules for “{{ selected.name }}”</h2>
      <span class="muted">Which classes it may connect. A rule also covers the subclasses of its classes.</span>
    </div>
    <div v-if="rules.isError.value" class="panel-body">
      <ErrorAlert :error="rules.error.value" :on-retry="() => rules.refetch()" />
    </div>
    <LoadingState v-else-if="rules.isLoading.value" label="Loading rules…" />
    <template v-else>
      <p v-if="selectedRules.length === 0" class="panel-body muted" style="margin: 0">
        No rules yet: no two CIs can be related with “{{ selected.name }}” until you add one.
      </p>
      <div v-else class="table-wrap">
        <table class="data">
          <thead>
            <tr>
              <th scope="col">Source class</th>
              <th scope="col">Reads as</th>
              <th scope="col">Target class</th>
              <th scope="col"><span class="sr-only">Actions</span></th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="r in selectedRules" :key="r.id">
              <td><RouterLink :to="`/admin/classes/${r.sourceClassId}`">{{ className(r.sourceClassId) }}</RouterLink></td>
              <td class="muted">
                {{ className(r.sourceClassId) }} <strong>{{ selected.forwardLabel }}</strong> {{ className(r.targetClassId) }} ·
                {{ className(r.targetClassId) }} <strong>{{ selected.reverseLabel }}</strong> {{ className(r.sourceClassId) }}
              </td>
              <td><RouterLink :to="`/admin/classes/${r.targetClassId}`">{{ className(r.targetClassId) }}</RouterLink></td>
              <td class="row-actions">
                <DeleteRowButton resource="relationship-rules" :id="r.id" :label="ruleLabel(r)" small @deleted="notice = `Deleted ${ruleLabel(r)}.`" />
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <form class="toolbar" aria-label="Add a rule" @submit.prevent="addRule">
        <div class="field">
          <label for="rule-source">Source class</label>
          <select id="rule-source" v-model="ruleSource" :aria-invalid="!!ruleFieldErrors.sourceClassId || undefined">
            <option value="">Choose…</option>
            <option v-for="n in classOptions" :key="n.item.id" :value="n.item.id">{{ "  ".repeat(n.depth) }}{{ n.item.name }}</option>
          </select>
        </div>
        <span class="muted rule-verb">{{ selected.forwardLabel }}</span>
        <div class="field">
          <label for="rule-target">Target class</label>
          <select id="rule-target" v-model="ruleTarget" :aria-invalid="!!ruleFieldErrors.targetClassId || undefined">
            <option value="">Choose…</option>
            <option v-for="n in classOptions" :key="n.item.id" :value="n.item.id">{{ "  ".repeat(n.depth) }}{{ n.item.name }}</option>
          </select>
        </div>
        <button type="submit" class="btn btn-primary" :disabled="!ruleSource || !ruleTarget || createRule.isPending.value">
          {{ createRule.isPending.value ? "Adding…" : "Add rule" }}
        </button>
      </form>
      <div v-if="createRule.isError.value" class="panel-body">
        <ErrorAlert :error="createRule.error.value" title="Rule not added" />
      </div>
    </template>
  </section>

  <RecordDialog
    :open="dialogOpen"
    :title="editing ? `Edit relationship type “${editing.name}”` : 'New relationship type'"
    :submit-label="editing ? 'Save' : 'Create relationship type'"
    :fields="TYPE_FIELDS"
    :record="editing"
    :defaults="{ isDirectional: true, impactDirection: 'none' }"
    :save="saveType"
    id-prefix="rt"
    @close="dialogOpen = false"
    @saved="(m) => (notice = m)"
  />
</template>
