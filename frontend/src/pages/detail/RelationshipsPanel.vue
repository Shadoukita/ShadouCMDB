<script setup lang="ts">
import { computed, ref } from "vue";
import { useCiClasses, useDeleteRelationship, useRelationships, type Ci, type Relationship } from "../../api/queries";
import CiLink from "../../components/CiLink.vue";
import ClassBadge from "../../components/ClassBadge.vue";
import ConfirmDialog from "../../components/ConfirmDialog.vue";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import Icon from "../../components/Icon.vue";
import LoadingState from "../../components/LoadingState.vue";
import { t } from "../../i18n";
import { isHostLike } from "../../lib/format";
import { describeEdge } from "../../lib/relationships";
import type { TrailStep } from "../../lib/trail";
import { useSessionStore } from "../../stores/session";
import AddRelationshipForm from "./AddRelationshipForm.vue";

/**
 * The CI's direct relationships: each row reads as a sentence from this CI ("runs on", then the related CI),
 * with the direction as an icon and its name for assistive technology, and the related CI's class icon (audit R5).
 * `embedded`: placed in a layout section, which gives the heading; only the content is drawn.
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
const rows = computed(() =>
  [...(rels.data.value?.data ?? [])]
    .map((r) => ({ r, d: describeEdge(r, props.ci.id), dir: direction(r) }))
    .sort((a, b) => a.d.label.localeCompare(b.d.label) || a.d.other.name.localeCompare(b.d.other.name)),
);

function direction(r: Relationship) {
  if (!r.type.isDirectional) return { icon: "arrow-left-right" as const, label: t("rel.dir.both") };
  return r.sourceCiId === props.ci.id
    ? { icon: "arrow-right" as const, label: t("rel.dir.outgoing") }
    : { icon: "arrow-left" as const, label: t("rel.dir.incoming") };
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
  <section :class="{ panel: !embedded }" :aria-labelledby="embedded ? undefined : 'rel-title'">
    <div v-if="!embedded" class="panel-header">
      <h2 id="rel-title">{{ t("rel.title") }}</h2>
      <span v-if="rels.data.value" class="muted">{{ t("rel.count", { n: rels.data.value.page.total }) }}</span>
    </div>
    <div class="panel-body flush">
      <LoadingState v-if="rels.isLoading.value" :label="t('rel.loading')" />
      <div v-if="rels.isError.value" class="panel-body">
        <ErrorAlert :error="rels.error.value" :on-retry="() => rels.refetch()" />
      </div>
      <EmptyState v-if="rels.data.value && rows.length === 0" :title="t('rel.empty.title')">
        {{ ci.deletedAt ? t("rel.empty.deleted") : mayAdd ? t("rel.empty.canAdd") : t("rel.empty.cannotAdd") }}
      </EmptyState>
      <div v-if="rows.length > 0" class="table-wrap">
        <table class="data rel-table">
          <thead>
            <tr>
              <th scope="col">{{ t("rel.col.edge") }}</th>
              <th scope="col">{{ t("rel.col.related") }}</th>
              <th scope="col">{{ t("rel.col.class") }}</th>
              <th scope="col">{{ t("rel.col.notes") }}</th>
              <th v-if="!ci.deletedAt" scope="col" class="row-actions"><span class="sr-only">{{ t("rel.col.actions") }}</span></th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="{ r, d, dir: way } in rows" :key="r.id">
              <td>
                <span class="rel-edge">
                  <Icon :name="way.icon" :size="14" :label="way.label" class="rel-dir" />
                  <span dir="auto">{{ d.label }}</span>
                </span>
              </td>
              <td>
                <span class="rel-ci">
                  <ClassBadge :icon="classByKey.get(d.other.classKey)?.icon" :color="classByKey.get(d.other.classKey)?.color" />
                  <CiLink :id="d.other.id" :class="{ mono: isHostLike(d.other.name) }" :from="self" :trail="trail">{{ d.other.name }}</CiLink>
                  <span v-if="d.other.deleted" class="badge danger">{{ t("rel.deleted") }}</span>
                </span>
              </td>
              <td dir="auto">{{ d.other.className }}</td>
              <td class="rel-notes" :title="r.notes ?? undefined" dir="auto">{{ r.notes ?? "" }}</td>
              <td v-if="!ci.deletedAt" class="row-actions">
                <button
                  v-if="mayRemove(r)"
                  type="button"
                  class="btn btn-sm btn-ghost btn-quiet-danger"
                  :aria-label="t('rel.remove.aria', { ci: ci.label, label: d.label, other: d.other.name })"
                  @click="removing = r"
                >
                  <Icon name="x" :size="14" />{{ t("rel.remove") }}
                </button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
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
