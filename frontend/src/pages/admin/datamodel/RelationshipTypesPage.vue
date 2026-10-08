<script setup lang="ts">
import { adminCrumbs } from "../sections";
import { computed, nextTick, ref, watch } from "vue";
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
import RowMenu, { type RowMenuItem } from "../../../components/RowMenu.vue";
import { formatNumber, t, type MessageKey } from "../../../i18n";
import { useDocumentTitle } from "../../../lib/composables";
import { useListQuery } from "../../../lib/listQuery";
import { moveItem, useDragReorder } from "../../../lib/reorder";
import { flattenTree } from "../../../lib/tree";
import { useFlashStore } from "../../../stores/flash";
import Icon from "../../../components/Icon.vue";

/**
 * Administration › Data model › Relationship types. A type names an edge in both
 * directions ("runs on" / "hosts"); its rules say which classes it may connect.
 * The selected type (?type=…, in the URL) shows its rules below the list.
 */
useDocumentTitle(t("dm.relTypes.title"));
const flash = useFlashStore();
const lq = useListQuery({ sort: "sortOrder" });
const types = useRelTypeList();
const rules = useRules();
const classes = useCiClasses();
const createType = useCreateRelType();
const patchType = usePatch<RelationshipType>("relationship-types");
const reorder = useReorder("relationship-types");
const createRule = useCreateRule();
const pendingOrder = ref<string[] | null>(null);

const rows = computed(() => {
  const list = types.data.value?.data ?? [];
  if (!pendingOrder.value) return list;
  const rank = new Map(pendingOrder.value.map((id, i) => [id, i]));
  return [...list].sort((a, b) => (rank.get(a.id) ?? 0) - (rank.get(b.id) ?? 0));
});
const selectedId = computed(() => lq.get("type") || rows.value[0]?.id || "");
const selected = computed(() => rows.value.find((rt) => rt.id === selectedId.value));
const allRules = computed(() => rules.data.value?.data ?? []);
const rulesOf = (typeId: string) => allRules.value.filter((r) => r.relationshipTypeId === typeId);
const classById = computed(() => new Map((classes.data.value ?? []).map((c) => [c.id, c])));
const className = (id: string) => classById.value.get(id)?.name ?? t("dm.relTypes.unknownClass");
const selectedRules = computed(() =>
  selected.value
    ? rulesOf(selected.value.id).sort((a, b) => className(a.sourceClassId).localeCompare(className(b.sourceClassId)) || className(a.targetClassId).localeCompare(className(b.targetClassId)))
    : [],
);

function commit(dragId: string, targetId: string) {
  const list = rows.value;
  const dragged = list.find((rt) => rt.id === dragId);
  const moved = moveItem(list, list.findIndex((rt) => rt.id === dragId), list.findIndex((rt) => rt.id === targetId));
  pendingOrder.value = moved.map((rt) => rt.id);
  reorder.mutate(
    moved.map((rt) => ({ id: rt.id, sortOrder: rt.sortOrder })),
    {
      onSuccess: () => flash.show(t("dm.relTypes.moved", { name: dragged?.name ?? "" })),
      onSettled: () => (pendingOrder.value = null),
    },
  );
}
function step(rt: RelationshipType, delta: -1 | 1) {
  const target = rows.value[rows.value.indexOf(rt) + delta];
  if (target) commit(rt.id, target.id);
}
const dnd = useDragReorder(commit, () => !reorder.isPending.value);

function setActive(rt: RelationshipType, isActive: boolean) {
  patchType.mutate(
    { id: rt.id, body: { isActive } },
    { onSuccess: () => flash.show(t(isActive ? "dm.relTypes.restored" : "dm.relTypes.archived", { name: rt.name })) },
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
    { value: "none", label: t("dm.relTypes.impact.none") },
    ...(directional
      ? [
          { value: "target_to_source", label: t("dm.relTypes.impact.targetToSource", { fwd }) },
          { value: "source_to_target", label: t("dm.relTypes.impact.sourceToTarget", { rev }) },
        ]
      : []),
    { value: "both", label: directional ? t("dm.relTypes.impact.both") : t("dm.relTypes.impact.bothSymmetric", { fwd }) },
  ];
}
const IMPACT_LABEL: Record<ImpactDirection, MessageKey | null> = {
  none: null,
  target_to_source: "dm.relTypes.impactShort.targetToSource",
  source_to_target: "dm.relTypes.impactShort.sourceToTarget",
  both: "dm.relTypes.impactShort.both",
};
const impactLabel = (d: ImpactDirection) => {
  const key = IMPACT_LABEL[d];
  return key ? t(key) : "";
};
const impactTitle = (rt: RelationshipType) =>
  impactOptions({ forwardLabel: rt.forwardLabel, reverseLabel: rt.reverseLabel }, rt).find((o) => o.value === rt.impactDirection)?.label ?? "";

// ---------- Type dialog ----------
const TYPE_FIELDS: FieldSpec[] = [
  { name: "name", label: t("dm.lookup.col.name"), type: "text", required: true, hint: t("dm.relTypes.field.nameHint") },
  { name: "key", label: t("dm.lookup.col.key"), type: "key", createOnly: true, from: "name" },
  { name: "forwardLabel", label: t("dm.relTypes.field.forward"), type: "text", required: true, hint: t("dm.relTypes.field.forwardHint") },
  { name: "reverseLabel", label: t("dm.relTypes.field.reverse"), type: "text", required: true, hint: t("dm.relTypes.field.reverseHint") },
  { name: "category", label: t("dm.relTypes.field.category"), type: "text", hint: t("dm.relTypes.field.categoryHint") },
  { name: "isDirectional", label: t("dm.relTypes.field.direction"), type: "checkbox", createOnly: true, text: t("dm.relTypes.field.directional") },
  {
    name: "impactDirection",
    label: t("dm.relTypes.field.impact"),
    type: "select",
    wide: true,
    options: impactOptions,
    hint: t("dm.relTypes.field.impactHint"),
  },
  { name: "description", label: t("dm.lookup.col.description"), type: "textarea" },
];
const dialogOpen = ref(false);
const editing = ref<RelationshipType | null>(null);
function openType(rt: RelationshipType | null) {
  editing.value = rt;
  dialogOpen.value = true;
}
async function saveType(body: Record<string, unknown>, isNew: boolean): Promise<string> {
  if (isNew) {
    const last = Math.max(0, ...rows.value.map((rt) => rt.sortOrder));
    const created = await createType.mutateAsync({ ...(body as RelTypeCreateBody), sortOrder: last + 10 });
    lq.update({ type: created.id });
    return t("dm.relTypes.created", { name: created.name });
  }
  const saved = await patchType.mutateAsync({ id: editing.value!.id, body: body as RelTypeUpdateBody });
  return t("dm.relTypes.saved", { name: saved.name });
}

// ---------- Row menus and delete ----------
// One delete dialog per table, opened from a row's menu.
const deletingType = ref<RelationshipType | null>(null);
const typeDeleteDialog = ref<InstanceType<typeof DeleteRowButton>>();
async function askDeleteType(rt: RelationshipType) {
  deletingType.value = rt;
  await nextTick();
  typeDeleteDialog.value?.open();
}
function onTypeDeleted() {
  flash.show(t("dm.relTypes.deleted", { name: deletingType.value?.name ?? "" }));
  lq.update({ type: undefined });
}
const typeMenu = (rt: RelationshipType): RowMenuItem[] => [
  { label: t("common.edit"), action: () => openType(rt) },
  rt.isActive ? { label: t("dm.lookup.archive"), action: () => setActive(rt, false) } : { label: t("dm.lookup.restore"), action: () => setActive(rt, true) },
  { label: t("common.delete"), action: () => void askDeleteType(rt), danger: true },
];

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
  try {
    await createRule.mutateAsync({ relationshipTypeId: selected.value.id, sourceClassId: ruleSource.value, targetClassId: ruleTarget.value });
    flash.show(t("dm.relTypes.rules.added", { source: className(ruleSource.value), verb: selected.value.forwardLabel, target: className(ruleTarget.value) }));
    ruleSource.value = "";
    ruleTarget.value = "";
  } catch {
    // shown from createRule.error
  }
}
const ruleFieldErrors = computed(() => (createRule.error.value instanceof ApiError ? createRule.error.value.fieldErrors() : {}));
const focusRuleForm = () => document.getElementById("rule-source")?.focus();

type Rule = { id: string; sourceClassId: string; targetClassId: string };
/** A rule has no name of its own: "Server runs on Hypervisor". */
const ruleText = (r: Rule) => `${className(r.sourceClassId)} ${selected.value?.forwardLabel ?? "→"} ${className(r.targetClassId)}`;
const deletingRule = ref<Rule | null>(null);
const ruleDeleteDialog = ref<InstanceType<typeof DeleteRowButton>>();
async function askDeleteRule(r: Rule) {
  deletingRule.value = r;
  await nextTick();
  ruleDeleteDialog.value?.open();
}
const ruleDeleteLabel = computed(() => (deletingRule.value ? t("dm.relTypes.rules.deleteLabel", { rule: ruleText(deletingRule.value) }) : ""));
const ruleMenu = (r: Rule): RowMenuItem[] => [{ label: t("common.delete"), action: () => void askDeleteRule(r), danger: true }];
</script>

<template>
  <div class="list-head">
    <Breadcrumbs :items="adminCrumbs('relationships')" />
    <div class="page-header">
      <div class="title">
        <h1>{{ t("dm.relTypes.title") }}</h1>
        <span v-if="types.data.value" class="count mono">{{ t("common.total", { n: formatNumber(types.data.value.page.total) }) }}</span>
        <span v-if="reorder.isPending.value || patchType.isPending.value" class="spinner" :aria-label="t('common.saving')" />
      </div>
      <div class="actions">
        <button type="button" class="btn btn-primary" @click="openType(null)"><Icon name="plus" />{{ t("dm.relTypes.create") }}</button>
      </div>
    </div>
    <p class="page-intro">{{ t("dm.relTypes.intro") }}</p>
  </div>
  <ErrorAlert v-if="reorder.isError.value" :error="reorder.error.value" :title="t('dm.lookup.reorderFailed')" />
  <ErrorAlert v-if="patchType.isError.value" :error="patchType.error.value" :title="t('formError.notSaved')" />

  <section class="panel explorer" :aria-label="t('dm.relTypes.title')">
    <div v-if="types.isError.value" class="panel-body">
      <ErrorAlert :error="types.error.value" :on-retry="() => types.refetch()" />
    </div>
    <LoadingState v-if="types.isLoading.value" :label="t('dm.relTypes.loading')" />
    <EmptyState v-if="types.data.value && rows.length === 0" icon="network" :title="t('dm.relTypes.empty.title')">
      {{ t("dm.relTypes.empty.body") }}
      <template #actions>
        <button type="button" class="btn btn-primary" @click="openType(null)"><Icon name="plus" />{{ t("dm.relTypes.create") }}</button>
        <RouterLink class="btn" to="/admin/templates">{{ t("admin.section.templates") }}</RouterLink>
      </template>
    </EmptyState>
    <div v-if="rows.length > 0" class="table-wrap">
      <table class="data list-table reorderable relationship-types">
        <thead>
          <tr>
            <th scope="col" class="drag-col"><span class="sr-only">{{ t("dm.lookup.col.drag") }}</span></th>
            <th scope="col">{{ t("dm.lookup.col.name") }}</th>
            <th scope="col">{{ t("dm.lookup.col.key") }}</th>
            <th scope="col">{{ t("dm.relTypes.col.forward") }}</th>
            <th scope="col">{{ t("dm.relTypes.col.reverse") }}</th>
            <th scope="col">{{ t("dm.relTypes.col.category") }}</th>
            <th scope="col">{{ t("dm.relTypes.col.impact") }}</th>
            <th scope="col">{{ t("dm.relTypes.col.rules") }}</th>
            <th scope="col">{{ t("admin.col.status") }}</th>
            <th scope="col">{{ t("dm.lookup.col.order") }}</th>
            <th scope="col" class="row-actions"><span class="sr-only">{{ t("inventory.actions") }}</span></th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="rt in rows"
            :key="rt.id"
            v-bind="dnd.row(rt.id)"
            :class="{ disabled: !rt.isActive, selected: rt.id === selectedId }"
            :aria-selected="rt.id === selectedId"
          >
            <td class="drag-handle" aria-hidden="true" :title="t('dm.lookup.col.drag')"><Icon name="grip-vertical" /></td>
            <td>
              <RouterLink class="list-name" :to="{ query: { type: rt.id } }" dir="auto">{{ rt.name }}</RouterLink>
              <span v-if="!rt.isDirectional" class="badge" :title="t('dm.relTypes.symmetricTitle')">{{ t("dm.relTypes.symmetric") }}</span>
            </td>
            <td class="mono">{{ rt.key }}</td>
            <td>{{ rt.forwardLabel }}</td>
            <td>{{ rt.reverseLabel }}</td>
            <td dir="auto">
              <template v-if="rt.category">{{ rt.category }}</template><span v-else class="muted">—</span>
            </td>
            <td :title="impactTitle(rt)">
              <span v-if="rt.impactDirection === 'none'" class="muted">—</span>
              <template v-else>{{ impactLabel(rt.impactDirection) }}</template>
            </td>
            <td>{{ rules.data.value ? formatNumber(rulesOf(rt.id).length) : "" }}</td>
            <td>
              <span v-if="rt.isActive" class="badge ok"><span class="status-dot" aria-hidden="true" />{{ t("common.active") }}</span>
              <span v-else class="badge off"><span class="status-dot" aria-hidden="true" />{{ t("dm.lookup.archivedBadge") }}</span>
            </td>
            <td class="order-buttons">
              <button type="button" class="btn btn-sm btn-icon" :disabled="reorder.isPending.value || rows.indexOf(rt) === 0" :aria-label="t('dm.lookup.moveUp', { name: rt.name })" @click="step(rt, -1)"><Icon name="arrow-up" /></button>
              <button type="button" class="btn btn-sm btn-icon" :disabled="reorder.isPending.value || rows.indexOf(rt) === rows.length - 1" :aria-label="t('dm.lookup.moveDown', { name: rt.name })" @click="step(rt, 1)"><Icon name="arrow-down" /></button>
            </td>
            <td class="row-actions">
              <RowMenu :label="t('inventory.rowMenu', { name: rt.name })" :items="typeMenu(rt)" />
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>

  <section v-if="selected" class="panel" aria-labelledby="rules-title">
    <div class="panel-header">
      <h2 id="rules-title">{{ t("dm.relTypes.rules.title", { name: selected.name }) }}</h2>
      <span class="meta">{{ t("dm.relTypes.rules.hint") }}</span>
    </div>
    <div v-if="rules.isError.value" class="panel-body">
      <ErrorAlert :error="rules.error.value" :on-retry="() => rules.refetch()" />
    </div>
    <LoadingState v-else-if="rules.isLoading.value" :label="t('dm.relTypes.rules.loading')" />
    <template v-else>
      <EmptyState v-if="selectedRules.length === 0" icon="arrow-left-right" :title="t('dm.relTypes.rules.empty.title')">
        {{ t("dm.relTypes.rules.empty.body", { name: selected.name }) }}
        <template #actions>
          <button type="button" class="btn" @click="focusRuleForm"><Icon name="plus" />{{ t("dm.relTypes.rules.form") }}</button>
        </template>
      </EmptyState>
      <div v-else class="table-wrap">
        <table class="data list-table relationship-rules">
          <thead>
            <tr>
              <th scope="col">{{ t("dm.relTypes.rules.col.source") }}</th>
              <th scope="col">{{ t("dm.relTypes.rules.col.readsAs") }}</th>
              <th scope="col">{{ t("dm.relTypes.rules.col.target") }}</th>
              <th scope="col" class="row-actions"><span class="sr-only">{{ t("inventory.actions") }}</span></th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="r in selectedRules" :key="r.id">
              <td><RouterLink class="list-name" :to="`/admin/classes/${r.sourceClassId}`" dir="auto">{{ className(r.sourceClassId) }}</RouterLink></td>
              <td class="muted">
                {{ className(r.sourceClassId) }} <strong>{{ selected.forwardLabel }}</strong> {{ className(r.targetClassId) }} ·
                {{ className(r.targetClassId) }} <strong>{{ selected.reverseLabel }}</strong> {{ className(r.sourceClassId) }}
              </td>
              <td><RouterLink class="list-name" :to="`/admin/classes/${r.targetClassId}`" dir="auto">{{ className(r.targetClassId) }}</RouterLink></td>
              <td class="row-actions">
                <RowMenu :label="t('inventory.rowMenu', { name: ruleText(r) })" :items="ruleMenu(r)" />
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <form class="toolbar" :aria-label="t('dm.relTypes.rules.form')" @submit.prevent="addRule">
        <div class="field">
          <label for="rule-source">{{ t("dm.relTypes.rules.col.source") }}</label>
          <select id="rule-source" v-model="ruleSource" :aria-invalid="!!ruleFieldErrors.sourceClassId || undefined">
            <option value="">{{ t("dm.relTypes.rules.choose") }}</option>
            <option v-for="n in classOptions" :key="n.item.id" :value="n.item.id">{{ "  ".repeat(n.depth) }}{{ n.item.name }}</option>
          </select>
        </div>
        <span class="muted rule-verb">{{ selected.forwardLabel }}</span>
        <div class="field">
          <label for="rule-target">{{ t("dm.relTypes.rules.col.target") }}</label>
          <select id="rule-target" v-model="ruleTarget" :aria-invalid="!!ruleFieldErrors.targetClassId || undefined">
            <option value="">{{ t("dm.relTypes.rules.choose") }}</option>
            <option v-for="n in classOptions" :key="n.item.id" :value="n.item.id">{{ "  ".repeat(n.depth) }}{{ n.item.name }}</option>
          </select>
        </div>
        <button type="submit" class="btn btn-primary" :disabled="!ruleSource || !ruleTarget || createRule.isPending.value">
          {{ createRule.isPending.value ? t("dm.relTypes.rules.adding") : t("dm.relTypes.rules.add") }}
        </button>
      </form>
      <div v-if="createRule.isError.value" class="panel-body">
        <ErrorAlert :error="createRule.error.value" :title="t('dm.relTypes.rules.notAdded')" />
      </div>
    </template>
  </section>

  <DeleteRowButton
    ref="typeDeleteDialog"
    headless
    resource="relationship-types"
    :id="deletingType?.id ?? ''"
    :label="t('dm.relTypes.deleteLabel', { name: deletingType?.name ?? '' })"
    archivable
    :archived="!deletingType?.isActive"
    @archive="deletingType && setActive(deletingType, false)"
    @deleted="onTypeDeleted"
  />
  <DeleteRowButton
    ref="ruleDeleteDialog"
    headless
    resource="relationship-rules"
    :id="deletingRule?.id ?? ''"
    :label="ruleDeleteLabel"
    @deleted="flash.show(t('dm.relTypes.rules.deleted', { label: ruleDeleteLabel }))"
  />

  <RecordDialog
    :open="dialogOpen"
    :title="editing ? t('dm.relTypes.dialog.editTitle', { name: editing.name }) : t('dm.relTypes.dialog.newTitle')"
    :submit-label="editing ? t('record.save.save') : t('dm.relTypes.dialog.create')"
    :fields="TYPE_FIELDS"
    :record="editing"
    :defaults="{ isDirectional: true, impactDirection: 'none' }"
    :save="saveType"
    id-prefix="rt"
    @close="dialogOpen = false"
    @saved="(m) => flash.show(m)"
  />
</template>
