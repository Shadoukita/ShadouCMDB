<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useCiClasses, useDeleteRelationship, useRelationships, type Ci, type Relationship } from "../../api/queries";
import CiLink from "../../components/CiLink.vue";
import ClassBadge from "../../components/ClassBadge.vue";
import ConfirmDialog from "../../components/ConfirmDialog.vue";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import Icon from "../../components/Icon.vue";
import LoadingState from "../../components/LoadingState.vue";
import RowMenu, { type RowMenuItem } from "../../components/RowMenu.vue";
import { t } from "../../i18n";
import { isHostLike } from "../../lib/format";
import { describeEdge } from "../../lib/relationships";
import type { TrailStep } from "../../lib/trail";
import { useSessionStore } from "../../stores/session";
import AddRelationshipForm from "./AddRelationshipForm.vue";

/**
 * The CI's direct relationships (design §0 step 12d, audit R5), grouped by how the edge reads from this CI
 * ("is located in", "hosts"): relationship types have no category yet (gap G15), so the sentence is the
 * group. Each row is the related CI's class tile, its name as a link with the class and the notes under it,
 * a pill with the direction (an icon named for assistive technology) and the type's key, and an action menu
 * (Remove). All / Outgoing / Incoming and a text filter narrow the list; a relationship that goes both ways
 * counts under either direction. `embedded`: placed in a layout section, which gives the heading; only the
 * content is drawn.
 */
const props = defineProps<{ ci: Ci; self: TrailStep; trail: TrailStep[]; embedded?: boolean }>();
const rels = useRelationships(() => props.ci.id);
const removing = ref<Relationship | null>(null);
const del = useDeleteRelationship();
const session = useSessionStore();
const classes = useCiClasses();
// A relationship belongs to its source CI: adding or removing one needs the edit right on the source's class.
const classByKey = computed(() => new Map((classes.data.value ?? []).map((c) => [c.key, c])));
const mayRemove = (r: Relationship) => session.canOnClass(classByKey.value.get(r.source.classKey)?.id, "edit");
const mayAdd = computed(() => session.canOnAnyClass("edit"));
type Way = "out" | "in" | "both";
const rows = computed(() =>
  [...(rels.data.value?.data ?? [])]
    .map((r) => ({ r, d: describeEdge(r, props.ci.id), dir: direction(r) }))
    .sort((a, b) => a.d.label.localeCompare(b.d.label) || a.d.other.name.localeCompare(b.d.other.name)),
);
const shownWay = ref<"all" | "out" | "in">("all");
const query = ref("");
watch(
  () => props.ci.id,
  () => {
    shownWay.value = "all";
    query.value = "";
  },
);
const count = (way: "out" | "in") => rows.value.filter((x) => x.dir.way === way || x.dir.way === "both").length;
const WAYS = computed(() => [
  { key: "all" as const, label: t("rel.filter.all") },
  { key: "out" as const, label: t("rel.filter.outgoing", { n: count("out") }) },
  { key: "in" as const, label: t("rel.filter.incoming", { n: count("in") }) },
]);
const filtered = computed(() => {
  const q = query.value.trim().toLocaleLowerCase();
  return rows.value.filter(
    (x) =>
      (shownWay.value === "all" || x.dir.way === shownWay.value || x.dir.way === "both") &&
      (!q || [x.d.label, x.d.other.name, x.d.other.className, x.r.type.key, x.r.notes ?? ""].some((v) => v.toLocaleLowerCase().includes(q))),
  );
});
/** The rows by sentence, in the order of the sorted rows. */
const groups = computed(() => {
  const out = new Map<string, (typeof filtered.value)[number][]>();
  for (const x of filtered.value) out.set(x.d.label, [...(out.get(x.d.label) ?? []), x]);
  return [...out].map(([label, items], i) => ({ label, items, id: `rel-group-${props.ci.id}-${i}` }));
});
/** Not every relationship was fetched (more than one page): the filter and the groups cover the loaded ones. */
const partial = computed(() => !!rels.data.value && rels.data.value.data.length < rels.data.value.page.total);
const tileStyle = (classKey: string) => {
  const color = classByKey.value.get(classKey)?.color;
  return color ? { "--tile-c": color } : undefined;
};
const menuItems = (r: Relationship): RowMenuItem[] => (mayRemove(r) ? [{ label: t("rel.remove"), danger: true, action: () => (removing.value = r) }] : []);

function direction(r: Relationship): { way: Way; icon: "arrow-left-right" | "arrow-right" | "arrow-left"; label: string } {
  if (!r.type.isDirectional) return { way: "both", icon: "arrow-left-right", label: t("rel.dir.both") };
  return r.sourceCiId === props.ci.id
    ? { way: "out", icon: "arrow-right", label: t("rel.dir.outgoing") }
    : { way: "in", icon: "arrow-left", label: t("rel.dir.incoming") };
}

function cancelRemove() {
  del.reset();
  removing.value = null;
}

function confirmRemove() {
  if (removing.value) del.mutate(removing.value.id, { onSuccess: () => (removing.value = null) });
}
</script>

<template>
  <section :class="['rel-panel', { panel: !embedded }]" :aria-labelledby="embedded ? undefined : 'rel-title'">
    <div v-if="!embedded" class="panel-header">
      <h2 id="rel-title">{{ t("rel.title") }} <span v-if="rels.data.value" class="count mono">{{ rels.data.value.page.total.toLocaleString() }}</span></h2>
    </div>
    <div class="panel-body flush">
      <LoadingState v-if="rels.isLoading.value" :label="t('rel.loading')" />
      <div v-if="rels.isError.value" class="panel-body">
        <ErrorAlert :error="rels.error.value" :on-retry="() => rels.refetch()" />
      </div>
      <EmptyState v-if="rels.data.value && rows.length === 0" :title="t('rel.empty.title')">
        {{ ci.deletedAt ? t("rel.empty.deleted") : mayAdd ? t("rel.empty.canAdd") : t("rel.empty.cannotAdd") }}
      </EmptyState>
      <template v-if="rows.length > 0">
        <div class="rel-toolbar">
          <div class="segmented" role="radiogroup" :aria-label="t('rel.filter.direction')">
            <label v-for="w in WAYS" :key="w.key"><input v-model="shownWay" type="radio" :name="`rel-way-${ci.id}`" :value="w.key" />{{ w.label }}</label>
          </div>
          <label class="search-field rel-search">
            <Icon name="search" :size="14" />
            <span class="sr-only">{{ t("rel.filter.label") }}</span>
            <input v-model="query" type="search" :placeholder="t('rel.filter.label')" />
          </label>
        </div>
        <p v-if="partial" class="hint rel-partial">{{ t("rel.partial", { n: rels.data.value!.data.length, total: rels.data.value!.page.total }) }}</p>
        <p v-if="filtered.length === 0" class="rel-none muted" role="status">{{ t("rel.filter.none") }}</p>
        <div v-for="g in groups" :key="g.label" class="rel-group">
          <h3 :id="g.id" class="rel-group-head">
            <span dir="auto">{{ g.label }}</span> <span class="count mono">{{ g.items.length }}</span>
          </h3>
          <ul class="rel-list" :aria-labelledby="g.id">
            <li v-for="{ r, d, dir: way } in g.items" :key="r.id" class="rel-row">
              <span class="class-tile" :style="tileStyle(d.other.classKey)" aria-hidden="true">
                <ClassBadge :icon="classByKey.get(d.other.classKey)?.icon" :color="classByKey.get(d.other.classKey)?.color" />
              </span>
              <span class="rel-main">
                <span class="rel-name">
                  <CiLink :id="d.other.id" :class="{ mono: isHostLike(d.other.name) }" :from="self" :trail="trail">{{ d.other.name }}</CiLink>
                  <span v-if="d.other.deleted" class="badge danger">{{ t("rel.deleted") }}</span>
                </span>
                <span class="rel-sub" dir="auto">{{ d.other.className }}<template v-if="r.notes"> · {{ r.notes }}</template></span>
              </span>
              <span class="rel-type" :title="r.type.name">
                <Icon :name="way.icon" :size="12" :label="way.label" class="rel-dir" />
                <span class="mono">{{ r.type.key }}</span>
              </span>
              <span class="rel-actions">
                <RowMenu v-if="!ci.deletedAt && menuItems(r).length > 0" :label="t('rel.actions', { other: d.other.name })" :items="menuItems(r)" />
              </span>
            </li>
          </ul>
        </div>
      </template>
      <AddRelationshipForm v-if="!ci.deletedAt && mayAdd" :ci="ci" />
    </div>
    <ConfirmDialog
      :open="!!removing"
      :title="t('rel.remove.title')"
      :confirm-label="t('rel.remove.confirm')"
      :busy="del.isPending.value"
      @cancel="cancelRemove"
      @confirm="confirmRemove"
    >
      <ErrorAlert v-if="del.isError.value" :error="del.error.value" :title="t('rel.remove.failed')" />
      <template v-if="removing">
        <p class="rel-sentence">
          <strong dir="auto">{{ removing.source.name }}</strong> <em dir="auto">{{ removing.type.forwardLabel }}</em>
          <strong dir="auto">{{ removing.target.name }}</strong>
        </p>
        <p>{{ t("rel.remove.body") }}</p>
      </template>
    </ConfirmDialog>
  </section>
</template>
